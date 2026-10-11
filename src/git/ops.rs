//! Mutations of the index and history. Each is a [`GitOp`] so the reducer can
//! name the work, and the refresh planner can say what it invalidates.

use std::path::Path;

use super::branches::BranchName;
use super::commit_files::CommitId;
use super::diff::terminal_safe;
use super::path::RepoPath;
use super::repo_state::Operation;
use super::runner::{GitCommand, GitOutput, GitRunner};
use super::stash::StashRef;
use super::status::{EntryKind, FileEntry};
use super::version::{GitVersion, parse_version, version_command};

/// One repository-changing Git operation. Path payloads are repo file names,
/// in argv order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitOp {
    Stage(Vec<RepoPath>),
    Unstage(Vec<RepoPath>),
    StageAll,
    UnstageAll,
    /// Plain `git commit` with `message` on stdin, so the user's hooks,
    /// signing and cleanup config apply exactly as on the command line.
    /// With `stage_all`, everything is staged first, at submit time.
    ///
    /// When signing is configured or a hook would run, Kika hands the
    /// terminal over and git runs on it, so a terminal pinentry, a key
    /// passphrase or a hook that asks a question works (see
    /// `git::prompts`). Otherwise git runs detached, with the message on
    /// stdin.
    ///
    /// With `amend`, the staged changes are folded into `HEAD` instead; a
    /// `None` message keeps `HEAD`'s, and is only valid with `amend`. With
    /// `no_verify`, git skips the pre-commit and commit-msg hooks, and only
    /// then, since a hook is the user's own check.
    Commit {
        message: Option<String>,
        stage_all: bool,
        amend: bool,
        no_verify: bool,
    },
    /// Apply `patch` to the index only, from stdin; with `reverse`, take it
    /// back out. How a hunk or some lines are staged or unstaged.
    ApplyCached {
        patch: Vec<u8>,
        reverse: bool,
    },
    /// Throw away the worktree changes to these files: each goes back to its
    /// index version, which is `HEAD` when nothing is staged. The index is
    /// not touched.
    DiscardWorktree(Vec<RepoPath>),
    /// Delete these untracked files from disk. Ignored files are never
    /// removed.
    DeleteUntracked(Vec<RepoPath>),
    /// Take `patch` back out of the worktree, from stdin. How a hunk or some
    /// lines are discarded; the index is not touched.
    ApplyWorktree {
        patch: Vec<u8>,
    },
    /// Apply a hunk the user edited to the index, from stdin. Its `@@`
    /// counts are recounted, since the edit invalidates them; git applies
    /// all or nothing, so a broken edit leaves the index as it was.
    ApplyEdited {
        patch: Vec<u8>,
    },
    /// Record the submodule's URL in the parent's config. It stays
    /// uninitialized in the list until it is cloned by an update.
    SubmoduleInit(RepoPath),
    /// Clone the submodule if needed and check out the commit the parent
    /// records, initializing it first.
    SubmoduleUpdate(RepoPath),
    /// Switch to this local branch. Git refuses when it would overwrite
    /// changes in the worktree, so no work is lost.
    Checkout(BranchName),
    /// Merge the branch into the current one, as `strategy` says.
    Merge {
        branch: BranchName,
        strategy: MergeStrategy,
    },
    /// Replay the current branch's own commits on top of `onto`.
    Rebase {
        onto: BranchName,
    },
    /// Go on with the operation git stopped, once its conflicts are
    /// resolved and staged.
    Continue(Operation),
    /// Drop the commit the operation stopped on and go on. Never built for
    /// a merge, which has nothing to skip.
    Skip(Operation),
    /// Stop the operation and put `HEAD`, the index and the worktree back
    /// where it started.
    Abort(Operation),
    /// Check out this commit with no branch: a detached `HEAD`. Git
    /// refuses when it would overwrite changes in the worktree.
    CheckoutCommit(CommitId),
    /// Make a new commit that undoes this one. Never built for a merge
    /// commit, which needs a mainline.
    Revert(CommitId),
    /// Move the current branch, or a detached `HEAD`, to `to`; `mode` says
    /// what happens to the index and the worktree.
    Reset {
        to: CommitId,
        mode: ResetMode,
    },
    /// Make branch `name` at `from` and switch to it, the usual next step.
    CreateBranch {
        name: String,
        from: BranchName,
    },
    RenameBranch {
        from: BranchName,
        to: String,
    },
    /// Delete a local branch. Without `force`, git refuses when its
    /// commits are not merged; the user confirms `force` separately.
    DeleteBranch {
        name: BranchName,
        force: bool,
    },
    /// Resolve this conflicted file in the user's merge tool, on the
    /// terminal. The tool may ask which one to use or wait for Enter.
    MergeTool(RepoPath),
    /// Put changes aside as a new `stash@{0}`, as `mode` says, with the
    /// message given or git's own. With `paths`, only those files go.
    StashPush {
        mode: StashMode,
        message: Option<String>,
        paths: Vec<RepoPath>,
    },
    /// Apply the entry to the worktree and keep it in the list.
    StashApply(StashRef),
    /// Apply the entry and drop it from the list once it applied cleanly.
    StashPop(StashRef),
    /// Drop the entry from the list. Nothing in the worktree changes.
    StashDrop(StashRef),
    /// Give the entry a new message. Git has no rename, so it is stored
    /// again as `stash@{0}` and the old place dropped.
    StashRename {
        stash: StashRef,
        message: String,
    },
}

/// Which changes a stash push puts aside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StashMode {
    /// Every tracked change, staged or not. Untracked files stay.
    All,
    /// Every change, untracked files too (`-u`).
    IncludeUntracked,
    /// Only what is staged (`--staged`). Gated: git added it in 2.35,
    /// newer than the 2.15 Kika needs, and an older git answers it with a
    /// usage dump, so `run_op` checks the version first and refuses with
    /// one plain sentence that names the version found.
    Staged,
}

/// The first git with `stash push --staged`.
const STASH_STAGED_GIT: GitVersion = GitVersion {
    major: 2,
    minor: 35,
    patch: 0,
};

/// Why a stash action is refused when its entry moved since it was chosen.
pub const STASH_LIST_CHANGED: &str = "The stash list changed; refresh and try again";

/// How a merge records the other branch's commits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeStrategy {
    /// Only move the branch forward; refused when a merge commit is needed.
    FastForwardOnly,
    /// Always make a merge commit, even when a fast-forward would do.
    NoFastForward,
    /// Stage the combined changes for one ordinary commit; no merge is
    /// recorded.
    Squash,
}

/// What a reset keeps besides moving the branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetMode {
    /// Keep the index and the worktree: the undone commits' changes stay
    /// staged.
    Soft,
    /// Keep the worktree but reset the index: the changes stay, unstaged.
    Mixed,
    /// Keep nothing: the index and the worktree match the target, and
    /// every uncommitted change is lost.
    Hard,
}

impl ResetMode {
    fn flag(self) -> &'static str {
        match self {
            ResetMode::Soft => "--soft",
            ResetMode::Mixed => "--mixed",
            ResetMode::Hard => "--hard",
        }
    }
}

/// Why staging everything is refused while conflicts exist.
pub const CONFLICTS_REFUSAL: &str = "Resolve conflicts before staging everything";

impl GitOp {
    /// What one key press does to `entry`: stage it while it has unstaged
    /// changes, else unstage it. Conflicts are never touched, since staging
    /// one would mark it resolved.
    pub fn toggle_for(entry: &FileEntry) -> Option<GitOp> {
        if entry.kind == EntryKind::Conflicted {
            return None;
        }
        if has_unstaged(entry) {
            return Some(GitOp::Stage(vec![entry.path.clone()]));
        }
        // A staged rename also deletes its old path, which must be unstaged too.
        let paths = entry
            .orig_path
            .iter()
            .chain([&entry.path])
            .cloned()
            .collect();
        Some(GitOp::Unstage(paths))
    }

    /// What the stage-all key does: stage everything while anything is
    /// unstaged, else unstage everything. Refused while conflicts exist,
    /// because `add -A` would mark them resolved and `reset` would drop their
    /// conflict stages.
    pub fn toggle_all(files: &[FileEntry]) -> Result<Option<GitOp>, String> {
        if files.iter().any(|f| f.kind == EntryKind::Conflicted) {
            return Err(CONFLICTS_REFUSAL.into());
        }
        if files.is_empty() {
            return Ok(None);
        }
        Ok(Some(if files.iter().any(has_unstaged) {
            GitOp::StageAll
        } else {
            GitOp::UnstageAll
        }))
    }

    /// What discarding `entry` from the Files panel does: delete it when
    /// untracked, else restore its worktree from the index. Only the current
    /// path is restored, so a staged rename stays staged. Conflicts are never
    /// touched, and a file with only staged changes has nothing to discard.
    pub fn discard_for(entry: &FileEntry) -> Result<Option<GitOp>, &'static str> {
        match entry.kind {
            EntryKind::Conflicted => Err("Resolve the conflict first"),
            EntryKind::Untracked => Ok(Some(GitOp::DeleteUntracked(vec![entry.path.clone()]))),
            _ if entry.worktree == '.' => Err("No unstaged changes to discard"),
            _ => Ok(Some(GitOp::DiscardWorktree(vec![entry.path.clone()]))),
        }
    }

    /// What stashing only `entry` puts aside: its path, plus the old one of
    /// a staged rename so both sides go together. An untracked file needs
    /// `-u`, since git does not match untracked files by pathspec without
    /// it. Conflicts cannot be stashed.
    pub fn stash_file_for(entry: &FileEntry) -> Option<(StashMode, Vec<RepoPath>)> {
        let mode = match entry.kind {
            EntryKind::Conflicted => return None,
            EntryKind::Untracked => StashMode::IncludeUntracked,
            _ => StashMode::All,
        };
        let paths = entry
            .orig_path
            .iter()
            .chain([&entry.path])
            .cloned()
            .collect();
        Some((mode, paths))
    }
}

/// The command for `op`. Unlike reads, these need `index.lock`, so
/// `GIT_OPTIONAL_LOCKS=0` is not set. File names come from the repo, so they
/// are read literally, never as globs or pathspec magic. A commit adds no
/// env at all: its hooks inherit it, and must see what they would see
/// outside Kika. Git runs hooks with stdin closed, so the message on stdin
/// never reaches them. Merges, rebases, reverts and their continue, skip
/// and abort add none for the same reason.
pub fn op_command(op: &GitOp) -> GitCommand {
    let with_paths = |cmd: GitCommand, paths: &[RepoPath]| {
        paths
            .iter()
            .fold(cmd.arg("--"), |cmd, p| cmd.arg(p.as_os_str()))
    };
    let cmd = match op {
        GitOp::Stage(paths) => with_paths(GitCommand::new("add"), paths),
        GitOp::Unstage(paths) => with_paths(GitCommand::new("reset").arg("-q"), paths),
        GitOp::StageAll => GitCommand::new("add").arg("-A"),
        GitOp::UnstageAll => GitCommand::new("reset").arg("-q"),
        // `--whitespace=nowarn` so the user's `apply.whitespace` can never
        // reject or rewrite the patch.
        GitOp::ApplyCached { patch, reverse } => {
            let cmd = GitCommand::new("apply").arg("--cached");
            let cmd = if *reverse { cmd.arg("--reverse") } else { cmd };
            cmd.arg("--whitespace=nowarn").arg("-").stdin(patch.clone())
        }
        GitOp::DiscardWorktree(paths) => with_paths(GitCommand::new("checkout").arg("-q"), paths),
        // Status lists untracked files one by one, so `-d` is never needed;
        // without `-x`, ignored files are left alone.
        GitOp::DeleteUntracked(paths) => {
            with_paths(GitCommand::new("clean").arg("-f").arg("-q"), paths)
        }
        // Without `--cached`, git applies to the worktree only.
        GitOp::ApplyWorktree { patch } => GitCommand::new("apply")
            .arg("--reverse")
            .arg("--whitespace=nowarn")
            .arg("-")
            .stdin(patch.clone()),
        GitOp::ApplyEdited { patch } => GitCommand::new("apply")
            .arg("--cached")
            .arg("--recount")
            .arg("--whitespace=nowarn")
            .arg("-")
            .stdin(patch.clone()),
        GitOp::SubmoduleInit(path) => with_paths(
            GitCommand::new("submodule").arg("init"),
            std::slice::from_ref(path),
        ),
        // Git has no terminal while Kika holds the screen, so a credential
        // prompt would hang; it fails instead.
        GitOp::SubmoduleUpdate(path) => with_paths(
            GitCommand::new("submodule").arg("update").arg("--init"),
            std::slice::from_ref(path),
        )
        .env("GIT_TERMINAL_PROMPT", "0"),
        // A short name, since a full ref would detach `HEAD`; `--` keeps
        // it from being read as a path. A ref made outside git's porcelain
        // can be named like an option, such as `-f`, which would force the
        // checkout and lose the worktree's changes; `--end-of-options`
        // keeps it a name. Only then: it needs git 2.24, newer than Kika
        // requires, and an older git fails on it without touching anything.
        GitOp::Checkout(name) => {
            let cmd = GitCommand::new("checkout").arg("-q");
            let cmd = if name.0.starts_with(b"-") {
                cmd.arg("--end-of-options")
            } else {
                cmd
            };
            cmd.arg(name.as_os_string()).arg("--")
        }
        // The start point as a full ref, so a tag of the same name, or a
        // name like an option, cannot stand in for it.
        GitOp::CreateBranch { name, from } => GitCommand::new("checkout")
            .arg("-q")
            .arg("-b")
            .arg(name)
            .arg(from.full_ref())
            .arg("--"),
        // The id is hex digits only, so it cannot be read as an option;
        // `--` keeps it from being read as a path.
        GitOp::CheckoutCommit(oid) => GitCommand::new("checkout")
            .arg("-q")
            .arg("--detach")
            .arg(oid.as_str())
            .arg("--"),
        GitOp::Reset { to, mode } => GitCommand::new("reset")
            .arg("-q")
            .arg(mode.flag())
            .arg(to.as_str())
            .arg("--"),
        // Paths only with `-u` or no flag: `--staged` takes the whole
        // index, and no key builds it with paths. Only a push that names
        // files reads pathspecs literally: without paths, `-u` cleans the
        // untracked files through a `:/` pathspec of git's own, which a
        // literal reading turns into a name that matches nothing, and they
        // would stay in the worktree as well as in the stash.
        GitOp::StashPush {
            mode,
            message,
            paths,
        } => {
            let cmd = GitCommand::new("stash").arg("push");
            let cmd = match mode {
                StashMode::All => cmd,
                StashMode::IncludeUntracked => cmd.arg("-u"),
                StashMode::Staged => cmd.arg("--staged"),
            };
            let cmd = match message.as_deref().filter(|m| !m.trim().is_empty()) {
                Some(message) => cmd.arg("-m").arg(message),
                None => cmd,
            };
            debug_assert!(
                *mode != StashMode::Staged || paths.is_empty(),
                "--staged takes no paths"
            );
            if paths.is_empty() || *mode == StashMode::Staged {
                return cmd.env("LC_ALL", "C");
            }
            with_paths(cmd, paths)
        }
        // These take no file names, so nothing is read as a pathspec.
        GitOp::StashApply(stash) => return stash_command("apply", stash),
        GitOp::StashPop(stash) => return stash_command("pop", stash),
        GitOp::StashDrop(stash) => return stash_command("drop", stash),
        // The first of the two steps `run_op` takes; see `rename_stash`.
        GitOp::StashRename { stash, message } => return store_command(stash, message),
        GitOp::RenameBranch { from, to } => GitCommand::new("branch")
            .arg("-m")
            .arg("--")
            .arg(from.as_os_string())
            .arg(to),
        GitOp::DeleteBranch { name, force } => GitCommand::new("branch")
            .arg(if *force { "-D" } else { "-d" })
            .arg("--")
            .arg(name.as_os_string()),
        // These take no paths and may run hooks, which must see the env they
        // would see outside Kika, as a commit's do. The branch is a full
        // ref, so a name like an option, or one a tag shadows, stays this
        // branch.
        GitOp::Merge { branch, strategy } => {
            let flag = match strategy {
                MergeStrategy::FastForwardOnly => "--ff-only",
                MergeStrategy::NoFastForward => "--no-ff",
                MergeStrategy::Squash => "--squash",
            };
            return GitCommand::new("merge").arg(flag).arg(branch.full_ref());
        }
        GitOp::Rebase { onto } => return GitCommand::new("rebase").arg(onto.full_ref()),
        GitOp::Revert(oid) => return GitCommand::new("revert").arg(oid.as_str()),
        GitOp::Continue(operation) => {
            return GitCommand::new(operation.subcommand()).arg("--continue");
        }
        // Git rejects `merge --skip` without touching anything; no key
        // builds it. `cherry-pick --skip` and `revert --skip` need git 2.23,
        // and an older one rejects them the same way.
        GitOp::Skip(operation) => {
            debug_assert!(operation.can_skip(), "{operation:?} cannot be skipped");
            return GitCommand::new(operation.subcommand()).arg("--skip");
        }
        GitOp::Abort(operation) => {
            return GitCommand::new(operation.subcommand()).arg("--abort");
        }
        // The user's tool runs on the terminal and keeps their locale, so
        // no `LC_ALL`; the name is still read literally.
        GitOp::MergeTool(path) => {
            return with_paths(GitCommand::new("mergetool"), std::slice::from_ref(path))
                .env("GIT_LITERAL_PATHSPECS", "1");
        }
        GitOp::Commit {
            message,
            amend,
            no_verify,
            ..
        } => {
            return match message {
                Some(message) => commit_command(*amend, *no_verify, Some(Path::new("-")))
                    .stdin(message.clone().into_bytes()),
                None => commit_command(*amend, *no_verify, None),
            };
        }
    };
    cmd.env("GIT_LITERAL_PATHSPECS", "1").env("LC_ALL", "C")
}

/// `git stash store`: the entry's commit again, as `stash@{0}`, under
/// `message`. The id is hex digits only, so it cannot be read as an option.
fn store_command(stash: &StashRef, message: &str) -> GitCommand {
    GitCommand::new("stash")
        .arg("store")
        .arg("-m")
        .arg(message)
        .arg(stash.oid.as_str())
        .env("LC_ALL", "C")
}

/// `git stash <verb> stash@{N}`.
fn stash_command(verb: &str, stash: &StashRef) -> GitCommand {
    GitCommand::new("stash")
        .arg(verb)
        .arg(stash.arg())
        .env("LC_ALL", "C")
}

/// `git commit` taking its message from `message_file` (`-` is stdin), or
/// keeping `HEAD`'s with none. Shared by the detached run and the one on
/// the terminal, whose stdin is the user's, so the two differ only there.
pub fn commit_command(amend: bool, no_verify: bool, message_file: Option<&Path>) -> GitCommand {
    let cmd = GitCommand::new("commit");
    let cmd = if amend { cmd.arg("--amend") } else { cmd };
    let cmd = if no_verify {
        cmd.arg("--no-verify")
    } else {
        cmd
    };
    match message_file {
        Some(file) => cmd.arg("-F").arg(file),
        None => cmd.arg("--no-edit"),
    }
}

/// Run `op`. Failures carry git's message, made safe to draw since it can
/// quote repo-controlled paths. A commit's, a merge's, a revert's or a
/// stash apply's failure also carries stdout: hooks print to stderr, but
/// git says "nothing to commit" and names each conflict on stdout.
///
/// A stash action first checks its entry is still where it was chosen,
/// and `--staged` first checks git is new enough.
pub fn run_op(runner: &dyn GitRunner, op: &GitOp) -> Result<(), String> {
    run_op_with(runner, op, &|key| std::env::var_os(key).is_some())
}

/// [`run_op`], with `env_set` saying which variables Kika's own
/// environment sets, so tests do not depend on the host's.
fn run_op_with(
    runner: &dyn GitRunner,
    op: &GitOp,
    env_set: &dyn Fn(&str) -> bool,
) -> Result<(), String> {
    if let GitOp::Commit {
        stage_all: true, ..
    } = op
    {
        run_op(runner, &GitOp::StageAll)?;
    }
    match op {
        GitOp::StashApply(stash)
        | GitOp::StashPop(stash)
        | GitOp::StashDrop(stash)
        | GitOp::StashRename { stash, .. } => verify_stash(runner, stash)?,
        GitOp::StashPush {
            mode: StashMode::Staged,
            ..
        } => staged_stash_supported(runner)?,
        _ => {}
    }
    if let GitOp::StashRename { stash, message } = op {
        return rename_stash(runner, stash, message);
    }
    let mut cmd = op_command(op);
    if matches!(op, GitOp::SubmoduleUpdate(_)) && ssh_is_default(runner, env_set) {
        cmd = cmd.env("GIT_SSH_COMMAND", BATCH_SSH);
    }
    let output = runner.run(&cmd).map_err(|e| e.to_string())?;
    if output.success {
        return Ok(());
    }
    let message = match op {
        GitOp::Commit { .. }
        | GitOp::Merge { .. }
        | GitOp::Revert(_)
        | GitOp::StashApply(_)
        | GitOp::StashPop(_) => [&output.stderr, &output.stdout]
            .map(|bytes| terminal_safe(bytes))
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => terminal_safe(&output.stderr),
    };
    if message.is_empty() {
        let subcommand = cmd.args()[0].to_string_lossy();
        return Err(format!("git {subcommand} failed"));
    }
    Err(message)
}

/// Run `cmd`, with git's message, made safe to draw, as the error.
fn run_checked(runner: &dyn GitRunner, cmd: &GitCommand) -> Result<GitOutput, String> {
    let output = runner.run(cmd).map_err(|e| e.to_string())?;
    if output.success {
        return Ok(output);
    }
    let message = terminal_safe(&output.stderr);
    if message.is_empty() {
        let subcommand = cmd.args()[0].to_string_lossy();
        return Err(format!("git {subcommand} failed"));
    }
    Err(message)
}

/// Whether `stash@{N}` still names the commit `stash` was chosen as.
fn stash_is(runner: &dyn GitRunner, stash: &StashRef) -> Result<bool, String> {
    let cmd = GitCommand::new("rev-parse")
        .arg("-q")
        .arg("--verify")
        .arg(stash.arg())
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C");
    let output = runner.run(&cmd).map_err(|e| e.to_string())?;
    // `-q` makes a missing entry a quiet failure, not an error.
    let found = String::from_utf8_lossy(&output.stdout);
    Ok(output.success && found.trim().eq_ignore_ascii_case(stash.oid.as_str()))
}

/// Refuse when `stash` no longer names the entry it was chosen as: the
/// list changed under Kika, and the number now means another stash.
fn verify_stash(runner: &dyn GitRunner, stash: &StashRef) -> Result<(), String> {
    if stash_is(runner, stash)? {
        Ok(())
    } else {
        Err(STASH_LIST_CHANGED.into())
    }
}

/// Refuse `--staged` on a git older than 2.35. A version Kika cannot read
/// is let through, as at startup.
fn staged_stash_supported(runner: &dyn GitRunner) -> Result<(), String> {
    let output = runner.run(&version_command()).map_err(|e| e.to_string())?;
    match parse_version(&output.stdout).filter(|_| output.success) {
        Some(found) if found < STASH_STAGED_GIT => Err(format!(
            "Stashing only staged changes needs Git {}.{} or newer; found {found}",
            STASH_STAGED_GIT.major, STASH_STAGED_GIT.minor
        )),
        _ => Ok(()),
    }
}

/// Store the entry again under `message`, then drop its old place, which
/// the store moved down by one. Storing first means a failure leaves a
/// copy, never a loss.
///
/// Git does not log a ref update that leaves the ref where it was, so
/// storing the entry `refs/stash` already points at (`stash@{0}`) adds
/// nothing, and dropping `stash@{1}` then would drop another stash. That
/// entry is dropped first instead; if the store then fails, the error
/// names its id so it can be stored by hand.
fn rename_stash(runner: &dyn GitRunner, stash: &StashRef, message: &str) -> Result<(), String> {
    let store = store_command(stash, message);
    let at = |index| StashRef {
        index,
        oid: stash.oid.clone(),
    };
    let drop = |index| op_command(&GitOp::StashDrop(at(index)));
    if stash_is(runner, &at(0))? {
        run_checked(runner, &drop(stash.index))?;
        return run_checked(runner, &store).map(|_| ()).map_err(|message| {
            format!(
                "{message}\nThe stash was dropped but not stored again; \
                 git stash store {} brings it back.",
                stash.oid.as_str()
            )
        });
    }
    run_checked(runner, &store)?;
    verify_stash(runner, &at(stash.index + 1))?;
    run_checked(runner, &drop(stash.index + 1)).map(|_| ())
}

/// ssh that fails instead of asking. `GIT_TERMINAL_PROMPT=0` stops git's
/// own prompts, but ssh asks about host keys and passphrases on the
/// terminal itself, and git has none while Kika holds the screen: the
/// update would hang until Kika quits.
const BATCH_SSH: &str = "ssh -o BatchMode=yes";

/// Whether git would run plain `ssh`, so [`BATCH_SSH`] replaces nothing
/// the user chose. Any doubt, such as a failed config read, keeps theirs.
fn ssh_is_default(runner: &dyn GitRunner, env_set: &dyn Fn(&str) -> bool) -> bool {
    if env_set("GIT_SSH_COMMAND") || env_set("GIT_SSH") {
        return false;
    }
    let cmd = GitCommand::new("config")
        .arg("--get")
        .arg("core.sshCommand")
        .env("LC_ALL", "C");
    // Unset is exit 1 with nothing on stderr.
    matches!(runner.run(&cmd), Ok(out) if !out.success && out.stderr.is_empty())
}

/// Untracked, or changed in the worktree (including partly staged files).
fn has_unstaged(entry: &FileEntry) -> bool {
    entry.kind == EntryKind::Untracked || entry.worktree != '.'
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::fake::{FakeRunner, fail, ok};
    use crate::git::runner::GitOutput;

    fn entry(path: &str, index: char, worktree: char, kind: EntryKind) -> FileEntry {
        FileEntry {
            path: path.into(),
            orig_path: None,
            index,
            worktree,
            kind,
        }
    }

    fn stage(paths: &[&str]) -> Option<GitOp> {
        Some(GitOp::Stage(paths.iter().map(|&p| p.into()).collect()))
    }

    fn unstage(paths: &[&str]) -> Option<GitOp> {
        Some(GitOp::Unstage(paths.iter().map(|&p| p.into()).collect()))
    }

    #[test]
    fn files_with_unstaged_changes_are_staged() {
        let unstaged = entry("a.rs", '.', 'M', EntryKind::Changed);
        let untracked = entry("new.rs", '?', '?', EntryKind::Untracked);
        let partly = entry("mm.rs", 'M', 'M', EntryKind::Changed);
        assert_eq!(GitOp::toggle_for(&unstaged), stage(&["a.rs"]));
        assert_eq!(GitOp::toggle_for(&untracked), stage(&["new.rs"]));
        assert_eq!(GitOp::toggle_for(&partly), stage(&["mm.rs"]));
    }

    #[test]
    fn staged_only_files_are_unstaged() {
        let staged = entry("a.rs", 'M', '.', EntryKind::Changed);
        assert_eq!(GitOp::toggle_for(&staged), unstage(&["a.rs"]));
    }

    #[test]
    fn a_staged_rename_unstages_both_paths() {
        let mut renamed = entry("new.rs", 'R', '.', EntryKind::Renamed);
        renamed.orig_path = Some("old.rs".into());
        assert_eq!(GitOp::toggle_for(&renamed), unstage(&["old.rs", "new.rs"]));
    }

    #[test]
    fn conflicted_files_are_left_alone() {
        let conflicted = entry("both.rs", 'U', 'U', EntryKind::Conflicted);
        assert_eq!(GitOp::toggle_for(&conflicted), None);
    }

    #[test]
    fn toggle_all_stages_everything_while_anything_is_unstaged() {
        let staged = entry("a.rs", 'M', '.', EntryKind::Changed);
        for other in [
            entry("b.rs", '.', 'M', EntryKind::Changed),
            entry("c.rs", '?', '?', EntryKind::Untracked),
        ] {
            let files = [staged.clone(), other];
            assert_eq!(GitOp::toggle_all(&files), Ok(Some(GitOp::StageAll)));
        }
    }

    #[test]
    fn toggle_all_unstages_everything_when_all_is_staged() {
        let files = [
            entry("a.rs", 'M', '.', EntryKind::Changed),
            entry("b.rs", 'A', '.', EntryKind::Changed),
        ];
        assert_eq!(GitOp::toggle_all(&files), Ok(Some(GitOp::UnstageAll)));
    }

    #[test]
    fn toggle_all_on_a_clean_tree_does_nothing() {
        assert_eq!(GitOp::toggle_all(&[]), Ok(None));
    }

    #[test]
    fn toggle_all_refuses_while_conflicts_exist() {
        let files = [
            entry("a.rs", '.', 'M', EntryKind::Changed),
            entry("both.rs", 'U', 'U', EntryKind::Conflicted),
        ];
        assert_eq!(
            GitOp::toggle_all(&files),
            Err("Resolve conflicts before staging everything".into())
        );
    }

    #[test]
    fn branch_ops_issue_exact_argv_with_the_separator_and_full_refs() {
        let cases: [(GitOp, &[&str]); 7] = [
            (
                GitOp::Checkout("feature".into()),
                &["checkout", "-q", "feature", "--"],
            ),
            // A ref made outside git's porcelain can look like an option;
            // it must stay a branch name, never become `-f`.
            (
                GitOp::Checkout("-f".into()),
                &["checkout", "-q", "--end-of-options", "-f", "--"],
            ),
            (
                GitOp::Checkout("--force".into()),
                &["checkout", "-q", "--end-of-options", "--force", "--"],
            ),
            (
                GitOp::CreateBranch {
                    name: "new".into(),
                    from: "feature".into(),
                },
                &["checkout", "-q", "-b", "new", "refs/heads/feature", "--"],
            ),
            (
                GitOp::RenameBranch {
                    from: "-old".into(),
                    to: "fresh".into(),
                },
                &["branch", "-m", "--", "-old", "fresh"],
            ),
            (
                GitOp::DeleteBranch {
                    name: "done".into(),
                    force: false,
                },
                &["branch", "-d", "--", "done"],
            ),
            (
                GitOp::DeleteBranch {
                    name: "done".into(),
                    force: true,
                },
                &["branch", "-D", "--", "done"],
            ),
        ];
        for (op, argv) in cases {
            let runner = FakeRunner::new().expect(argv, ok(b""));
            assert_eq!(run_op(&runner, &op), Ok(()), "{op:?}");
            runner.assert_done();
        }
    }

    fn argv(cmd: &GitCommand) -> Vec<String> {
        cmd.args()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn merge_and_rebase_name_the_branch_by_its_full_ref_and_set_no_env() {
        let merge = |strategy| GitOp::Merge {
            branch: "feature".into(),
            strategy,
        };
        let cases: [(GitOp, &[&str]); 5] = [
            (
                merge(MergeStrategy::FastForwardOnly),
                &["merge", "--ff-only", "refs/heads/feature"],
            ),
            (
                merge(MergeStrategy::NoFastForward),
                &["merge", "--no-ff", "refs/heads/feature"],
            ),
            (
                merge(MergeStrategy::Squash),
                &["merge", "--squash", "refs/heads/feature"],
            ),
            // A name like an option stays a ref.
            (
                GitOp::Merge {
                    branch: "-x".into(),
                    strategy: MergeStrategy::NoFastForward,
                },
                &["merge", "--no-ff", "refs/heads/-x"],
            ),
            (
                GitOp::Rebase {
                    onto: "main".into(),
                },
                &["rebase", "refs/heads/main"],
            ),
        ];
        for (op, expected) in cases {
            let cmd = op_command(&op);
            assert_eq!(argv(&cmd), expected, "{op:?}");
            assert!(cmd.envs().is_empty(), "{op:?}: {:?}", cmd.envs());
            assert_eq!(cmd.input(), None, "{op:?}");
        }
    }

    #[test]
    fn continue_skip_and_abort_run_the_operations_own_subcommand_and_set_no_env() {
        use Operation::*;
        let cases: [(GitOp, &[&str]); 8] = [
            (GitOp::Continue(Merge), &["merge", "--continue"]),
            (GitOp::Continue(Rebase), &["rebase", "--continue"]),
            (GitOp::Continue(CherryPick), &["cherry-pick", "--continue"]),
            (GitOp::Skip(Rebase), &["rebase", "--skip"]),
            (GitOp::Skip(CherryPick), &["cherry-pick", "--skip"]),
            (GitOp::Abort(Merge), &["merge", "--abort"]),
            (GitOp::Abort(Rebase), &["rebase", "--abort"]),
            (GitOp::Abort(CherryPick), &["cherry-pick", "--abort"]),
        ];
        for (op, expected) in cases {
            let cmd = op_command(&op);
            assert_eq!(argv(&cmd), expected, "{op:?}");
            assert!(cmd.envs().is_empty(), "{op:?}: {:?}", cmd.envs());
        }
    }

    #[test]
    fn the_merge_tool_reads_the_path_literally_and_keeps_the_users_locale() {
        let cmd = op_command(&GitOp::MergeTool("-x".into()));
        assert_eq!(argv(&cmd), ["mergetool", "--", "-x"]);
        assert_eq!(
            cmd.envs(),
            [("GIT_LITERAL_PATHSPECS".into(), "1".into())],
            "no LC_ALL"
        );
    }

    #[test]
    fn a_failed_merge_shows_the_conflicts_git_printed_on_stdout() {
        let op = GitOp::Merge {
            branch: "feature".into(),
            strategy: MergeStrategy::Squash,
        };
        let output = GitOutput {
            success: false,
            stdout: b"CONFLICT (content): Merge conflict in a.txt\n".to_vec(),
            stderr: b"Squash commit -- not updating HEAD\n".to_vec(),
        };
        let runner = FakeRunner::new().expect(&["merge", "--squash", "refs/heads/feature"], output);
        assert_eq!(
            run_op(&runner, &op),
            Err("Squash commit -- not updating HEAD\n\
                 CONFLICT (content): Merge conflict in a.txt"
                .into())
        );
        runner.assert_done();
    }

    fn oid(text: &str) -> CommitId {
        CommitId::parse(text).unwrap()
    }

    #[test]
    fn checkout_and_reset_name_the_commit_before_the_path_separator() {
        let cases: [(GitOp, &[&str]); 4] = [
            (
                GitOp::CheckoutCommit(oid("abc123")),
                &["checkout", "-q", "--detach", "abc123", "--"],
            ),
            (
                GitOp::Reset {
                    to: oid("abc123"),
                    mode: ResetMode::Soft,
                },
                &["reset", "-q", "--soft", "abc123", "--"],
            ),
            (
                GitOp::Reset {
                    to: oid("abc123"),
                    mode: ResetMode::Mixed,
                },
                &["reset", "-q", "--mixed", "abc123", "--"],
            ),
            (
                GitOp::Reset {
                    to: oid("abc123"),
                    mode: ResetMode::Hard,
                },
                &["reset", "-q", "--hard", "abc123", "--"],
            ),
        ];
        for (op, expected) in cases {
            let cmd = op_command(&op);
            assert_eq!(argv(&cmd), expected, "{op:?}");
            let envs: Vec<_> = cmd
                .envs()
                .iter()
                .map(|(k, v)| (k.to_str().unwrap(), v.to_str().unwrap()))
                .collect();
            assert_eq!(
                envs,
                [("GIT_LITERAL_PATHSPECS", "1"), ("LC_ALL", "C")],
                "{op:?}"
            );
        }
    }

    #[test]
    fn revert_is_plain_git_revert_and_sets_no_env() {
        let cmd = op_command(&GitOp::Revert(oid("abc123")));
        assert_eq!(argv(&cmd), ["revert", "abc123"]);
        assert!(cmd.envs().is_empty(), "{:?}", cmd.envs());
        assert_eq!(cmd.input(), None);
    }

    /// `a.txt` committed as `one`, then as `two`, with an identity and no
    /// signing. Returns the repo and the first commit's id.
    fn two_commit_repo() -> (crate::git::fake::TempDir, CommitId) {
        use crate::git::fake::{TempDir, git};

        let tmp = TempDir::new("history");
        let dir = tmp.path();
        git(dir, &["init", "-q", "--template="]);
        for (key, value) in [
            ("user.name", "Kika Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
        ] {
            git(dir, &["config", key, value]);
        }
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "first"]);
        let first = rev_parse(dir, "HEAD");
        std::fs::write(dir.join("a.txt"), "two\n").unwrap();
        git(dir, &["commit", "-q", "-am", "second"]);
        (tmp, first)
    }

    fn rev_parse(dir: &Path, rev: &str) -> CommitId {
        let out = crate::git::fake::git(dir, &["rev-parse", rev]);
        oid(String::from_utf8(out).unwrap().trim())
    }

    #[test]
    fn checkout_commit_leaves_head_detached_at_it_in_real_git() {
        use crate::git::fake::{git, try_git};

        let (tmp, first) = two_commit_repo();
        let dir = tmp.path();
        let runner = crate::git::CliRunner::isolated(dir);
        run_op(&runner, &GitOp::CheckoutCommit(first.clone())).unwrap();
        assert_eq!(rev_parse(dir, "HEAD"), first);
        assert!(
            !try_git(dir, &["symbolic-ref", "-q", "HEAD"]),
            "HEAD is on a branch"
        );
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one\n");
        assert_eq!(git(dir, &["status", "--porcelain"]), b"");
    }

    #[test]
    fn revert_makes_a_commit_on_head_that_undoes_the_change_in_real_git() {
        use crate::git::fake::git;

        let (tmp, _) = two_commit_repo();
        let dir = tmp.path();
        let second = rev_parse(dir, "HEAD");
        let runner = crate::git::CliRunner::isolated(dir).with_env("GIT_EDITOR", "true");
        run_op(&runner, &GitOp::Revert(second.clone())).unwrap();
        assert_eq!(rev_parse(dir, "HEAD^"), second);
        assert_eq!(git(dir, &["show", "HEAD:a.txt"]), b"one\n");
        assert_eq!(git(dir, &["rev-list", "--count", "HEAD"]), b"3\n");
        assert_eq!(git(dir, &["status", "--porcelain"]), b"");
    }

    #[test]
    fn each_reset_mode_keeps_what_it_says_in_real_git() {
        use crate::git::fake::git;

        let cases = [
            (ResetMode::Soft, "M  a.txt\n"),
            (ResetMode::Mixed, " M a.txt\n"),
            (ResetMode::Hard, ""),
        ];
        for (mode, status) in cases {
            let (tmp, first) = two_commit_repo();
            let dir = tmp.path();
            let runner = crate::git::CliRunner::isolated(dir);
            run_op(
                &runner,
                &GitOp::Reset {
                    to: first.clone(),
                    mode,
                },
            )
            .unwrap();
            assert_eq!(rev_parse(dir, "HEAD"), first, "{mode:?}");
            // The branch moved with it; HEAD is still on it.
            assert!(
                crate::git::fake::try_git(dir, &["symbolic-ref", "-q", "HEAD"]),
                "{mode:?}"
            );
            assert_eq!(
                String::from_utf8(git(dir, &["status", "--porcelain"])).unwrap(),
                status,
                "{mode:?}"
            );
            let worktree = std::fs::read_to_string(dir.join("a.txt")).unwrap();
            let expected = if mode == ResetMode::Hard {
                "one\n"
            } else {
                "two\n"
            };
            assert_eq!(worktree, expected, "{mode:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn merge_and_rebase_put_the_raw_bytes_of_a_name_in_argv() {
        use std::os::unix::ffi::OsStrExt;
        let raw = crate::git::branches::BranchName::from_bytes(b"x\xff");
        let merge = op_command(&GitOp::Merge {
            branch: raw.clone(),
            strategy: MergeStrategy::Squash,
        });
        assert_eq!(merge.args()[2].as_bytes(), b"refs/heads/x\xff");
        let rebase = op_command(&GitOp::Rebase { onto: raw });
        assert_eq!(rebase.args()[1].as_bytes(), b"refs/heads/x\xff");
    }

    #[cfg(unix)]
    #[test]
    fn branch_ops_put_the_raw_bytes_of_a_name_in_argv() {
        use std::os::unix::ffi::OsStrExt;
        let raw = crate::git::branches::BranchName::from_bytes(b"x\xff");
        let create = op_command(&GitOp::CreateBranch {
            name: "new".into(),
            from: raw.clone(),
        });
        assert_eq!(create.args()[4].as_bytes(), b"refs/heads/x\xff");
        let delete = op_command(&GitOp::DeleteBranch {
            name: raw,
            force: true,
        });
        assert_eq!(delete.args()[3].as_bytes(), b"x\xff");
    }

    /// A repo on `main` with `a.txt` committed and then changed in the
    /// worktree, so a forced checkout would lose the change.
    fn dirty_repo() -> crate::git::fake::TempDir {
        let tmp = crate::git::fake::TempDir::new("branch-dash");
        let dir = tmp.path();
        crate::git::fake::git(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        crate::git::fake::git(dir, &["add", "-A"]);
        crate::git::fake::git(dir, &["commit", "-q", "-m", "init"]);
        std::fs::write(dir.join("a.txt"), "work in progress\n").unwrap();
        tmp
    }

    #[test]
    fn checking_out_a_branch_named_like_an_option_keeps_the_worktree_in_real_git() {
        let tmp = dirty_repo();
        let dir = tmp.path();
        let runner = crate::git::CliRunner::isolated(dir);
        // `git branch` refuses such names; plumbing does not.
        crate::git::fake::git(dir, &["update-ref", "refs/heads/-f", "HEAD"]);
        crate::git::fake::git(dir, &["update-ref", "refs/heads/--force", "HEAD"]);
        use crate::git::version::{GitVersion, parse_version};
        let version = parse_version(&crate::git::fake::git(dir, &["--version"]));
        // Older git has no `--end-of-options`: the checkout fails instead.
        let modern = version.is_some_and(|v| {
            v >= GitVersion {
                major: 2,
                minor: 24,
                patch: 0,
            }
        });

        for name in ["-f", "--force"] {
            let result = run_op(&runner, &GitOp::Checkout(name.into()));
            assert_eq!(
                std::fs::read_to_string(dir.join("a.txt")).unwrap(),
                "work in progress\n",
                "{name}: the change survives"
            );
            if modern {
                assert_eq!(result, Ok(()), "{name}");
                let head = crate::git::fake::git(dir, &["symbolic-ref", "HEAD"]);
                assert_eq!(head, format!("refs/heads/{name}\n").as_bytes());
            }
        }
    }

    #[test]
    fn a_new_branch_named_like_an_option_is_refused_by_git_in_real_git() {
        let tmp = dirty_repo();
        let dir = tmp.path();
        let runner = crate::git::CliRunner::isolated(dir);
        for name in ["-f", "--orphan"] {
            let create = GitOp::CreateBranch {
                name: name.into(),
                from: "main".into(),
            };
            assert!(run_op(&runner, &create).is_err(), "{name}");
            let head = crate::git::fake::git(dir, &["symbolic-ref", "HEAD"]);
            assert_eq!(head, b"refs/heads/main\n", "{name}");
            assert_eq!(
                std::fs::read_to_string(dir.join("a.txt")).unwrap(),
                "work in progress\n"
            );
        }
    }

    #[test]
    fn checkout_create_rename_and_delete_change_branches_in_real_git() {
        let tmp = crate::git::fake::TempDir::new("branch-ops");
        let dir = tmp.path();
        crate::git::fake::git(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        crate::git::fake::git(dir, &["add", "-A"]);
        crate::git::fake::git(dir, &["commit", "-q", "-m", "init"]);
        crate::git::fake::git(dir, &["branch", "side"]);
        let runner = crate::git::CliRunner::isolated(dir);
        let head = || {
            String::from_utf8(crate::git::fake::git(
                dir,
                &["symbolic-ref", "--short", "HEAD"],
            ))
            .unwrap()
        };
        let branches = || {
            String::from_utf8(crate::git::fake::git(
                dir,
                &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
            ))
            .unwrap()
        };

        run_op(&runner, &GitOp::Checkout("side".into())).unwrap();
        assert_eq!(head(), "side\n");
        let create = GitOp::CreateBranch {
            name: "made".into(),
            from: "main".into(),
        };
        run_op(&runner, &create).unwrap();
        assert_eq!(head(), "made\n");
        let rename = GitOp::RenameBranch {
            from: "side".into(),
            to: "renamed".into(),
        };
        run_op(&runner, &rename).unwrap();
        let delete = GitOp::DeleteBranch {
            name: "renamed".into(),
            force: false,
        };
        run_op(&runner, &delete).unwrap();
        assert_eq!(branches(), "made\nmain\n");
    }

    fn paths(paths: &[&str]) -> Vec<RepoPath> {
        paths.iter().map(|&p| p.into()).collect()
    }

    #[test]
    fn run_op_issues_exact_argv_with_literal_pathspecs() {
        let cases: [(GitOp, &[&str]); 4] = [
            (
                GitOp::Stage(paths(&["a", "b.rs"])),
                &["add", "--", "a", "b.rs"],
            ),
            (
                GitOp::Unstage(paths(&["old.rs", "new.rs"])),
                &["reset", "-q", "--", "old.rs", "new.rs"],
            ),
            (GitOp::StageAll, &["add", "-A"]),
            (GitOp::UnstageAll, &["reset", "-q"]),
        ];
        for (op, argv) in cases {
            let runner = FakeRunner::new().expect(argv, ok(b""));
            assert_eq!(run_op(&runner, &op), Ok(()), "{op:?}");
            runner.assert_done();
            let envs = runner.envs();
            assert!(
                envs[0].contains(&("GIT_LITERAL_PATHSPECS".into(), "1".into())),
                "{op:?}: {envs:?}"
            );
            assert!(
                envs[0].contains(&("LC_ALL".into(), "C".into())),
                "{op:?}: {envs:?}"
            );
        }
    }

    #[test]
    fn submodule_ops_name_one_path_literally_and_update_never_prompts() {
        let literal = ("GIT_LITERAL_PATHSPECS".into(), "1".into());
        let c = ("LC_ALL".into(), "C".into());
        let no_prompt = ("GIT_TERMINAL_PROMPT".into(), "0".into());
        let cases = [
            (
                GitOp::SubmoduleInit("libs/a b".into()),
                &["submodule", "init", "--", "libs/a b"][..],
                vec![literal.clone(), c.clone()],
            ),
            (
                GitOp::SubmoduleUpdate("libs/a b".into()),
                &["submodule", "update", "--init", "--", "libs/a b"][..],
                vec![no_prompt, literal, c],
            ),
        ];
        for (op, argv, env) in cases {
            let update = matches!(op, GitOp::SubmoduleUpdate(_));
            let mut runner = FakeRunner::new();
            if update {
                runner = runner.expect(SSH_CONFIG, fail(""));
            }
            let runner = runner.expect(argv, ok(b""));
            assert_eq!(run_op_with(&runner, &op, &|_| false), Ok(()), "{op:?}");
            runner.assert_done();
            let envs = runner.envs();
            let mut env = env;
            if update {
                env.push(("GIT_SSH_COMMAND".into(), BATCH_SSH.into()));
            }
            assert_eq!(envs.last(), Some(&env), "{op:?}");
        }
    }

    const SSH_CONFIG: &[&str] = &["config", "--get", "core.sshCommand"];

    #[test]
    fn update_keeps_the_users_own_ssh_command() {
        let op = GitOp::SubmoduleUpdate("sub".into());
        let argv = &["submodule", "update", "--init", "--", "sub"];
        let has_batch = |runner: &FakeRunner| {
            runner
                .envs()
                .last()
                .unwrap()
                .iter()
                .any(|(k, _)| k == "GIT_SSH_COMMAND")
        };
        // Set in the environment: git is not even asked.
        for key in ["GIT_SSH_COMMAND", "GIT_SSH"] {
            let runner = FakeRunner::new().expect(argv, ok(b""));
            assert_eq!(run_op_with(&runner, &op, &|k| k == key), Ok(()));
            runner.assert_done();
            assert!(!has_batch(&runner), "{key}");
        }
        // Set in config, or the config read failed.
        for config in [ok(b"ssh -i key\n"), fail("fatal: bad config")] {
            let runner = FakeRunner::new()
                .expect(SSH_CONFIG, config)
                .expect(argv, ok(b""));
            assert_eq!(run_op_with(&runner, &op, &|_| false), Ok(()));
            runner.assert_done();
            assert!(!has_batch(&runner));
        }
    }

    #[test]
    fn an_ssh_command_in_the_repo_config_is_seen_in_real_git() {
        use crate::git::fake::{TempDir, git};

        let tmp = TempDir::new("batch-ssh");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        let runner = crate::git::CliRunner::isolated(dir);
        assert!(ssh_is_default(&runner, &|_| false));
        git(dir, &["config", "core.sshCommand", "my-ssh"]);
        assert!(!ssh_is_default(&runner, &|_| false));
    }

    #[test]
    fn apply_cached_sends_the_patch_on_stdin() {
        for (reverse, argv) in [
            (
                false,
                &["apply", "--cached", "--whitespace=nowarn", "-"][..],
            ),
            (
                true,
                &["apply", "--cached", "--reverse", "--whitespace=nowarn", "-"][..],
            ),
        ] {
            let runner = FakeRunner::new().expect(argv, ok(b""));
            let op = GitOp::ApplyCached {
                patch: b"the patch\n".to_vec(),
                reverse,
            };
            assert_eq!(run_op(&runner, &op), Ok(()));
            runner.assert_done();
            assert_eq!(runner.inputs(), [Some(b"the patch\n".to_vec())]);
            assert_eq!(
                runner.envs(),
                [vec![
                    ("GIT_LITERAL_PATHSPECS".into(), "1".into()),
                    ("LC_ALL".into(), "C".into())
                ]]
            );
        }
    }

    #[test]
    fn discard_for_picks_the_op_or_says_why_not() {
        let modified = entry("a.rs", '.', 'M', EntryKind::Changed);
        let partly = entry("mm.rs", 'M', 'M', EntryKind::Changed);
        let deleted = entry("gone.rs", '.', 'D', EntryKind::Changed);
        let untracked = entry("new.rs", '?', '?', EntryKind::Untracked);
        let staged = entry("s.rs", 'M', '.', EntryKind::Changed);
        let conflicted = entry("both.rs", 'U', 'U', EntryKind::Conflicted);
        let mut renamed = entry("new_name.rs", 'R', 'M', EntryKind::Renamed);
        renamed.orig_path = Some("old_name.rs".into());
        let discard = |p: &str| Ok(Some(GitOp::DiscardWorktree(vec![p.into()])));
        assert_eq!(GitOp::discard_for(&modified), discard("a.rs"));
        assert_eq!(GitOp::discard_for(&partly), discard("mm.rs"));
        assert_eq!(GitOp::discard_for(&deleted), discard("gone.rs"));
        assert_eq!(GitOp::discard_for(&renamed), discard("new_name.rs"));
        assert_eq!(
            GitOp::discard_for(&untracked),
            Ok(Some(GitOp::DeleteUntracked(vec!["new.rs".into()])))
        );
        assert_eq!(
            GitOp::discard_for(&staged),
            Err("No unstaged changes to discard")
        );
        assert_eq!(
            GitOp::discard_for(&conflicted),
            Err("Resolve the conflict first")
        );
    }

    #[test]
    fn discard_ops_issue_exact_argv_with_literal_pathspecs() {
        let cases: [(GitOp, &[&str]); 2] = [
            (
                GitOp::DiscardWorktree(paths(&["a", "-b.rs"])),
                &["checkout", "-q", "--", "a", "-b.rs"],
            ),
            (
                GitOp::DeleteUntracked(paths(&["*.rs"])),
                &["clean", "-f", "-q", "--", "*.rs"],
            ),
        ];
        for (op, argv) in cases {
            let runner = FakeRunner::new().expect(argv, ok(b""));
            assert_eq!(run_op(&runner, &op), Ok(()), "{op:?}");
            runner.assert_done();
            assert_eq!(runner.inputs(), [None], "{op:?}");
            assert_eq!(
                runner.envs(),
                [vec![
                    ("GIT_LITERAL_PATHSPECS".into(), "1".into()),
                    ("LC_ALL".into(), "C".into())
                ]],
                "{op:?}"
            );
        }
    }

    #[test]
    fn apply_worktree_sends_the_reverse_patch_on_stdin_without_cached() {
        let runner =
            FakeRunner::new().expect(&["apply", "--reverse", "--whitespace=nowarn", "-"], ok(b""));
        let op = GitOp::ApplyWorktree {
            patch: b"the patch\n".to_vec(),
        };
        assert_eq!(run_op(&runner, &op), Ok(()));
        runner.assert_done();
        assert_eq!(runner.inputs(), [Some(b"the patch\n".to_vec())]);
        assert_eq!(
            runner.envs(),
            [vec![
                ("GIT_LITERAL_PATHSPECS".into(), "1".into()),
                ("LC_ALL".into(), "C".into())
            ]]
        );
    }

    #[test]
    fn apply_edited_recounts_and_sends_the_patch_on_stdin() {
        let runner = FakeRunner::new().expect(
            &["apply", "--cached", "--recount", "--whitespace=nowarn", "-"],
            ok(b""),
        );
        let op = GitOp::ApplyEdited {
            patch: b"the patch\n".to_vec(),
        };
        assert_eq!(run_op(&runner, &op), Ok(()));
        runner.assert_done();
        assert_eq!(runner.inputs(), [Some(b"the patch\n".to_vec())]);
        assert_eq!(
            runner.envs(),
            [vec![
                ("GIT_LITERAL_PATHSPECS".into(), "1".into()),
                ("LC_ALL".into(), "C".into())
            ]]
        );
    }

    /// `e.txt` committed as `one`, then `two` and `three` added after it.
    fn edit_repo() -> crate::git::fake::TempDir {
        use crate::git::fake::{TempDir, git};

        let tmp = TempDir::new("edited");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        std::fs::write(dir.join("e.txt"), "one\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "init"]);
        std::fs::write(dir.join("e.txt"), "one\ntwo\nthree\n").unwrap();
        tmp
    }

    #[test]
    fn an_edited_hunk_with_stale_counts_stages_exactly_what_is_left_in_real_git() {
        use crate::git::fake::git;

        let tmp = edit_repo();
        let dir = tmp.path();
        let runner = crate::git::CliRunner::isolated(dir);
        // `+three` was deleted in the editor; the counts still say 3.
        let patch = b"diff --git a/e.txt b/e.txt\n--- a/e.txt\n+++ b/e.txt\n\
            @@ -1 +1,3 @@\n one\n+two\n"
            .to_vec();
        run_op(&runner, &GitOp::ApplyEdited { patch }).unwrap();
        assert_eq!(git(dir, &["show", ":e.txt"]), b"one\ntwo\n");
        assert_eq!(
            std::fs::read_to_string(dir.join("e.txt")).unwrap(),
            "one\ntwo\nthree\n"
        );
    }

    #[test]
    fn a_garbage_edit_fails_and_leaves_the_index_alone_in_real_git() {
        use crate::git::fake::git;

        let tmp = edit_repo();
        let dir = tmp.path();
        let runner = crate::git::CliRunner::isolated(dir);
        let header = "diff --git a/e.txt b/e.txt\n--- a/e.txt\n+++ b/e.txt\n";
        for body in ["garbage\n", "@@ -1 +1,2 @@\n nope\n+two\n"] {
            let patch = format!("{header}{body}").into_bytes();
            let result = run_op(&runner, &GitOp::ApplyEdited { patch });
            assert!(result.is_err(), "{body:?}: {result:?}");
            assert_eq!(git(dir, &["diff", "--cached"]), b"", "{body:?}");
        }
    }

    /// `d.txt` committed as `head`, `staged` in the index, `work` in the
    /// worktree.
    fn staged_and_changed_repo() -> crate::git::fake::TempDir {
        use crate::git::fake::{TempDir, git};

        let tmp = TempDir::new("discard");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        std::fs::write(dir.join("d.txt"), "head\n").unwrap();
        std::fs::write(dir.join("other.txt"), "head\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "init"]);
        std::fs::write(dir.join("d.txt"), "staged\n").unwrap();
        git(dir, &["add", "--", "d.txt"]);
        std::fs::write(dir.join("d.txt"), "work\n").unwrap();
        std::fs::write(dir.join("other.txt"), "other work\n").unwrap();
        tmp
    }

    #[test]
    fn discard_worktree_restores_the_index_version_and_keeps_staged_content_in_real_git() {
        use crate::git::fake::git;

        let tmp = staged_and_changed_repo();
        let dir = tmp.path();
        let runner = crate::git::CliRunner::isolated(dir);
        run_op(&runner, &GitOp::DiscardWorktree(paths(&["d.txt"]))).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("d.txt")).unwrap(),
            "staged\n"
        );
        assert_eq!(git(dir, &["show", ":d.txt"]), b"staged\n");
        // Only the named file goes back.
        assert_eq!(
            std::fs::read_to_string(dir.join("other.txt")).unwrap(),
            "other work\n"
        );

        // A file deleted in the worktree comes back too.
        std::fs::remove_file(dir.join("other.txt")).unwrap();
        run_op(&runner, &GitOp::DiscardWorktree(paths(&["other.txt"]))).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("other.txt")).unwrap(),
            "head\n"
        );
    }

    #[test]
    fn apply_worktree_removes_only_the_selected_lines_and_leaves_the_index_in_real_git() {
        use crate::git::diff::{DiffRequest, DiffSide, load_patch};
        use crate::git::fake::{TempDir, git};
        use crate::git::patch::{Direction, LineId, PatchSelection, build};

        let tmp = TempDir::new("discard-lines");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        let lines: String = (1..=20).map(|i| format!("line {i}\n")).collect();
        std::fs::write(dir.join("l.txt"), &lines).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "init"]);
        let changed = lines
            .replace("line 1\n", "line one\n")
            .replace("line 10\n", "ten a\nten b\n");
        std::fs::write(dir.join("l.txt"), &changed).unwrap();
        let runner = crate::git::CliRunner::isolated(dir);
        let request = DiffRequest {
            path: "l.txt".into(),
            orig_path: None,
            side: DiffSide::Worktree,
            options: Default::default(),
            commit: None,
        };
        let patch = load_patch(&runner, &request).unwrap();
        // `ten a` is the 10th line of the new side.
        let selection = PatchSelection::Lines([LineId::added(10)].into());
        let reverse = build(&patch, &selection, Direction::Unstage).unwrap();
        run_op(&runner, &GitOp::ApplyWorktree { patch: reverse }).unwrap();

        let expected = lines
            .replace("line 1\n", "line one\n")
            .replace("line 10\n", "ten b\n");
        assert_eq!(
            std::fs::read_to_string(dir.join("l.txt")).unwrap(),
            expected
        );
        assert_eq!(git(dir, &["show", ":l.txt"]), lines.as_bytes());
    }

    #[test]
    #[cfg_attr(windows, ignore = "Windows forbids * and : in file names")]
    fn delete_untracked_on_a_glob_name_deletes_only_that_file_in_real_git() {
        use crate::git::fake::{TempDir, git};

        let tmp = TempDir::new("clean");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        std::fs::write(dir.join("tracked.rs"), "keep\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "init"]);
        for name in ["*.rs", "a.rs", ":(glob)b.rs", "b.rs"] {
            std::fs::write(dir.join(name), "new\n").unwrap();
        }
        let runner = crate::git::CliRunner::isolated(dir);
        run_op(&runner, &GitOp::DeleteUntracked(paths(&["*.rs"]))).unwrap();
        run_op(&runner, &GitOp::DeleteUntracked(paths(&[":(glob)b.rs"]))).unwrap();
        assert!(!dir.join("*.rs").exists());
        assert!(!dir.join(":(glob)b.rs").exists());
        for name in ["a.rs", "b.rs", "tracked.rs"] {
            assert!(dir.join(name).exists(), "{name}");
        }
    }

    /// Stage hunk 1 of a two-hunk file through the real patch engine and
    /// `git apply`, then unstage it again from the staged diff.
    fn stage_and_unstage_one_hunk(name: &str) {
        use crate::git::diff::{DiffRequest, DiffSide, load_patch};
        use crate::git::fake::{TempDir, git};
        use crate::git::patch::{Direction, PatchSelection, build};

        let tmp = TempDir::new("apply");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        let lines: Vec<String> = (1..=20).map(|i| format!("line {i}\n")).collect();
        std::fs::write(dir.join(name), lines.concat()).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "init"]);
        let mut changed = lines.clone();
        changed[0] = "line 1a\nline 1b\n".into();
        changed[19] = "line twenty\n".into();
        std::fs::write(dir.join(name), changed.concat()).unwrap();
        let runner = crate::git::CliRunner::isolated(dir);
        let request = |side| DiffRequest {
            path: name.into(),
            orig_path: None,
            side,
            options: Default::default(),
            commit: None,
        };
        let cached = || String::from_utf8(git(dir, &["diff", "--cached"])).unwrap();
        let worktree = || String::from_utf8(git(dir, &["diff"])).unwrap();

        let patch = load_patch(&runner, &request(DiffSide::Worktree)).unwrap();
        let hunk = build(&patch, &PatchSelection::Hunk(1), Direction::Stage).unwrap();
        let stage = GitOp::ApplyCached {
            patch: hunk,
            reverse: false,
        };
        run_op(&runner, &stage).unwrap();
        let (index, work) = (cached(), worktree());
        assert!(
            index.contains("+line twenty") && !index.contains("+line 1a"),
            "{index}"
        );
        assert!(
            work.contains("+line 1a") && !work.contains("line twenty"),
            "{work}"
        );

        // Stage the rest too, then take only hunk 0 back out of the index.
        git(dir, &["add", "--", name]);
        let patch = load_patch(&runner, &request(DiffSide::Index)).unwrap();
        assert_eq!(patch.hunks.len(), 2);
        let hunk = build(&patch, &PatchSelection::Hunk(0), Direction::Unstage).unwrap();
        let unstage = GitOp::ApplyCached {
            patch: hunk,
            reverse: true,
        };
        run_op(&runner, &unstage).unwrap();
        let (index, work) = (cached(), worktree());
        assert!(
            index.contains("+line twenty") && !index.contains("+line 1a"),
            "{index}"
        );
        assert!(
            work.contains("+line 1a") && !work.contains("line twenty"),
            "{work}"
        );
    }

    #[test]
    fn a_built_hunk_stages_and_unstages_only_itself_in_real_git() {
        stage_and_unstage_one_hunk("a.txt");
    }

    /// git quotes this name in the diff header; the patch keeps it quoted.
    #[cfg(unix)]
    #[test]
    fn a_hunk_of_a_file_whose_name_needs_quoting_stages_in_real_git() {
        stage_and_unstage_one_hunk("tab\there \"q\".txt");
    }

    /// Stage one added line of a later hunk, then unstage one line from the
    /// staged view, each time through the real engine and `git apply`.
    #[test]
    fn single_lines_stage_and_unstage_alone_in_real_git() {
        use crate::git::diff::{DiffRequest, DiffSide, load_patch};
        use crate::git::fake::{TempDir, git};
        use crate::git::patch::{Direction, FilePatch, LineId, PatchSelection, build};

        /// The id of the one changed line whose text is `text`.
        fn id_of(patch: &FilePatch, text: &str) -> LineId {
            let wanted = format!("{text}\n");
            patch
                .hunks
                .iter()
                .flat_map(|h| h.lines.iter().zip(h.line_ids()))
                .find_map(|(line, id)| id.filter(|_| line.text == wanted.as_bytes()))
                .unwrap()
        }

        let tmp = TempDir::new("lines");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        let lines: Vec<String> = (1..=20).map(|i| format!("line {i}\n")).collect();
        std::fs::write(dir.join("l.txt"), lines.concat()).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "init"]);
        let mut changed = lines.clone();
        changed[0] = "line 1a\nline 1b\n".into();
        changed[14] = "fifteen a\nfifteen b\n".into();
        std::fs::write(dir.join("l.txt"), changed.concat()).unwrap();
        let runner = crate::git::CliRunner::isolated(dir);
        let request = |side| DiffRequest {
            path: "l.txt".into(),
            orig_path: None,
            side,
            options: Default::default(),
            commit: None,
        };
        let apply = |patch: &FilePatch, text: &str, direction| {
            let selection = PatchSelection::Lines([id_of(patch, text)].into());
            let op = GitOp::ApplyCached {
                patch: build(patch, &selection, direction).unwrap(),
                reverse: direction == Direction::Unstage,
            };
            run_op(&runner, &op).unwrap();
        };
        let index = || String::from_utf8(git(dir, &["show", ":l.txt"])).unwrap();
        let with = |at: usize, text: &str| {
            let mut out = lines.clone();
            out[at] = text.into();
            out
        };

        let patch = load_patch(&runner, &request(DiffSide::Worktree)).unwrap();
        apply(&patch, "fifteen a", Direction::Stage);
        assert_eq!(index(), with(14, "line 15\nfifteen a\n").concat());
        let cached = String::from_utf8(git(dir, &["diff", "--cached"])).unwrap();
        let added: Vec<&str> = cached.lines().filter(|l| l.starts_with('+')).collect();
        assert_eq!(added, ["+++ b/l.txt", "+fifteen a"], "{cached}");
        assert!(!cached.lines().any(|l| l.starts_with("-l")), "{cached}");

        // Everything staged; take only "fifteen b" back out, past hunk 0.
        git(dir, &["add", "--", "l.txt"]);
        let patch = load_patch(&runner, &request(DiffSide::Index)).unwrap();
        assert_eq!(patch.hunks.len(), 2);
        apply(&patch, "fifteen b", Direction::Unstage);
        let mut expected = with(14, "fifteen a\n");
        expected[0] = "line 1a\nline 1b\n".into();
        assert_eq!(index(), expected.concat());
        let work = String::from_utf8(git(dir, &["diff"])).unwrap();
        let changes: Vec<&str> = work
            .lines()
            .filter(|l| l.starts_with(['+', '-']) && !l.starts_with("+++") && !l.starts_with("---"))
            .collect();
        assert_eq!(changes, ["+fifteen b"], "{work}");
    }

    /// Regression: a reverse patch must carry the index-side start of its
    /// hunk. The file holds identical blocks, and the first staged hunk adds
    /// exactly one block's worth of lines, so a start shifted as if for a
    /// forward apply lands on an identical copy and unstages that instead.
    #[test]
    fn unstaging_one_of_two_staged_hunks_next_to_repeated_blocks_takes_out_only_it() {
        use crate::git::diff::{DiffRequest, DiffSide, load_patch};
        use crate::git::fake::{TempDir, git};
        use crate::git::patch::{Direction, PatchSelection, build};

        let tmp = TempDir::new("reverse");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        let block = |middle: &str| format!("a\nb\nc\n{middle}\nd\ne\nf\n");
        let filler: String = (1..=8).map(|i| format!("filler {i}\n")).collect();
        let head = format!(
            "top\n{filler}{}{}{}{filler}",
            block("Y"),
            block("X"),
            block("Y")
        );
        std::fs::write(dir.join("r.txt"), &head).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "init"]);
        let inserted: String = (1..=7).map(|i| format!("new {i}\n")).collect();
        let changed = format!(
            "top\n{inserted}{filler}{}{}{}{filler}",
            block("Y"),
            block("Y"),
            block("Y")
        );
        std::fs::write(dir.join("r.txt"), &changed).unwrap();
        git(dir, &["add", "--", "r.txt"]);

        let runner = crate::git::CliRunner::isolated(dir);
        let request = DiffRequest {
            path: "r.txt".into(),
            orig_path: None,
            side: DiffSide::Index,
            options: Default::default(),
            commit: None,
        };
        let patch = load_patch(&runner, &request).unwrap();
        assert_eq!(patch.hunks.len(), 2);
        let hunk = build(&patch, &PatchSelection::Hunk(1), Direction::Unstage).unwrap();
        let unstage = GitOp::ApplyCached {
            patch: hunk,
            reverse: true,
        };
        run_op(&runner, &unstage).unwrap();

        let index = git(dir, &["show", ":r.txt"]);
        let only_first = format!(
            "top\n{inserted}{filler}{}{}{}{filler}",
            block("Y"),
            block("X"),
            block("Y")
        );
        assert_eq!(String::from_utf8(index).unwrap(), only_first);
        let work = String::from_utf8(git(dir, &["diff"])).unwrap();
        assert!(
            work.contains("-X\n+Y\n") && !work.contains("new 1"),
            "{work}"
        );
        let cached = String::from_utf8(git(dir, &["diff", "--cached"])).unwrap();
        assert!(
            cached.contains("+new 7") && !cached.contains("+Y") && !cached.contains("-Y"),
            "{cached}"
        );
    }

    #[test]
    fn a_failed_op_returns_terminal_safe_stderr() {
        let runner = FakeRunner::new().expect(
            &["add", "--", "a"],
            fail("fatal: Unable to create '\x1b[2J.git/index.lock': File exists.\r\nhint: retry\n"),
        );
        assert_eq!(
            run_op(&runner, &GitOp::Stage(paths(&["a"]))),
            Err(
                "fatal: Unable to create '\u{FFFD}[2J.git/index.lock': File exists.\nhint: retry"
                    .into()
            )
        );
    }

    #[test]
    fn a_quiet_failure_still_has_a_message() {
        let runner = FakeRunner::new().expect(&["reset", "-q"], fail(" \n"));
        assert_eq!(
            run_op(&runner, &GitOp::UnstageAll),
            Err("git reset failed".into())
        );
    }

    fn commit(message: &str, stage_all: bool) -> GitOp {
        GitOp::Commit {
            message: Some(message.into()),
            stage_all,
            amend: false,
            no_verify: false,
        }
    }

    #[test]
    fn amend_and_skip_hooks_add_their_flags_before_the_message_source() {
        let op = |message: Option<&str>, amend, no_verify| GitOp::Commit {
            message: message.map(Into::into),
            stage_all: false,
            amend,
            no_verify,
        };
        /// The op, its argv and its stdin.
        type Case = (GitOp, &'static [&'static str], Option<&'static [u8]>);
        let cases: [Case; 5] = [
            (
                op(Some("m\n"), false, false),
                &["commit", "-F", "-"],
                Some(b"m\n"),
            ),
            (
                op(Some("m\n"), true, false),
                &["commit", "--amend", "-F", "-"],
                Some(b"m\n"),
            ),
            (
                op(None, true, false),
                &["commit", "--amend", "--no-edit"],
                None,
            ),
            (
                op(Some("m\n"), false, true),
                &["commit", "--no-verify", "-F", "-"],
                Some(b"m\n"),
            ),
            (
                op(None, true, true),
                &["commit", "--amend", "--no-verify", "--no-edit"],
                None,
            ),
        ];
        for (op, argv, input) in cases {
            let runner = FakeRunner::new().expect(argv, ok(b""));
            assert_eq!(run_op(&runner, &op), Ok(()), "{op:?}");
            runner.assert_done();
            assert_eq!(runner.envs(), [vec![]], "{op:?}");
            assert_eq!(runner.inputs(), [input.map(<[u8]>::to_vec)], "{op:?}");
        }
    }

    /// One commit of `a.txt` as `one`, with an identity, no signing and only
    /// this repo's hooks, then `a.txt` changed to `two` and staged.
    fn committed_and_staged_repo() -> crate::git::fake::TempDir {
        use crate::git::fake::{TempDir, git};

        let tmp = TempDir::new("amend");
        let dir = tmp.path();
        git(dir, &["init", "-q", "--template="]);
        for (key, value) in [
            ("user.name", "Kika Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
            ("core.hooksPath", ".git/hooks"),
        ] {
            git(dir, &["config", key, value]);
        }
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "first\n\nbody"]);
        std::fs::write(dir.join("a.txt"), "two\n").unwrap();
        git(dir, &["add", "-A"]);
        tmp
    }

    #[test]
    fn amend_folds_the_staged_change_into_head_and_keeps_or_replaces_the_message_in_real_git() {
        use crate::git::fake::git;

        let tmp = committed_and_staged_repo();
        let dir = tmp.path();
        let runner = crate::git::CliRunner::isolated(dir);
        let amend = |message: Option<&str>| GitOp::Commit {
            message: message.map(Into::into),
            stage_all: false,
            amend: true,
            no_verify: false,
        };

        run_op(&runner, &amend(None)).unwrap();
        assert_eq!(git(dir, &["rev-list", "--count", "HEAD"]), b"1\n");
        assert_eq!(git(dir, &["show", "HEAD:a.txt"]), b"two\n");
        assert_eq!(
            git(dir, &["log", "-1", "--format=%B"]),
            b"first\n\nbody\n\n"
        );
        assert_eq!(git(dir, &["diff", "--cached"]), b"");

        run_op(&runner, &amend(Some("new\n"))).unwrap();
        assert_eq!(git(dir, &["rev-list", "--count", "HEAD"]), b"1\n");
        assert_eq!(git(dir, &["log", "-1", "--format=%B"]), b"new\n\n");
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_pre_commit_hook_blocks_a_commit_unless_hooks_are_skipped_in_real_git() {
        use crate::git::fake::git;
        use std::os::unix::fs::PermissionsExt;

        let tmp = committed_and_staged_repo();
        let dir = tmp.path();
        let hooks = dir.join(".git").join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("pre-commit");
        std::fs::write(&hook, "#!/bin/sh\necho 'lint failed'\nexit 1\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        let runner = crate::git::CliRunner::isolated(dir);
        let commit = |no_verify| GitOp::Commit {
            message: Some("second\n".into()),
            stage_all: false,
            amend: false,
            no_verify,
        };

        let blocked = run_op(&runner, &commit(false));
        assert!(
            blocked.as_ref().is_err_and(|e| e.contains("lint failed")),
            "{blocked:?}"
        );
        assert_eq!(git(dir, &["rev-list", "--count", "HEAD"]), b"1\n");

        run_op(&runner, &commit(true)).unwrap();
        assert_eq!(git(dir, &["rev-list", "--count", "HEAD"]), b"2\n");
        assert_eq!(git(dir, &["log", "-1", "--format=%B"]), b"second\n\n");
    }

    #[test]
    fn commit_is_plain_git_commit_with_the_message_on_stdin_and_no_env() {
        let runner = FakeRunner::new().expect(&["commit", "-F", "-"], ok(b""));
        assert_eq!(run_op(&runner, &commit("subj\n\nbody\n", false)), Ok(()));
        runner.assert_done();
        assert_eq!(runner.envs(), [vec![]]);
        assert_eq!(runner.inputs(), [Some(b"subj\n\nbody\n".to_vec())]);
    }

    #[test]
    fn commit_with_stage_all_runs_add_first() {
        let runner = FakeRunner::new()
            .expect(&["add", "-A"], ok(b""))
            .expect(&["commit", "-F", "-"], ok(b""));
        assert_eq!(run_op(&runner, &commit("subj\n", true)), Ok(()));
        runner.assert_done();
        assert_eq!(runner.inputs(), [None, Some(b"subj\n".to_vec())]);
    }

    #[test]
    fn a_failed_stage_all_returns_its_error_and_does_not_commit() {
        let runner = FakeRunner::new().expect(&["add", "-A"], fail("fatal: index.lock\n"));
        assert_eq!(
            run_op(&runner, &commit("subj\n", true)),
            Err("fatal: index.lock".into())
        );
        // The fake panics on any call not expected, so no commit was attempted.
        runner.assert_done();
    }

    #[test]
    fn a_failed_commit_shows_stderr_then_stdout_made_safe() {
        let output = GitOutput {
            success: false,
            stdout: b"nothing added to commit\n".to_vec(),
            stderr: b"lint \x1b[31mfailed\n".to_vec(),
        };
        let runner = FakeRunner::new().expect(&["commit", "-F", "-"], output);
        assert_eq!(
            run_op(&runner, &commit("subj\n", false)),
            Err("lint \u{FFFD}[31mfailed\nnothing added to commit".into())
        );

        let stdout_only = GitOutput {
            success: false,
            stdout: b"nothing to commit, working tree clean\n".to_vec(),
            stderr: Vec::new(),
        };
        let runner = FakeRunner::new().expect(&["commit", "-F", "-"], stdout_only);
        assert_eq!(
            run_op(&runner, &commit("subj\n", false)),
            Err("nothing to commit, working tree clean".into())
        );

        let runner = FakeRunner::new().expect(&["commit", "-F", "-"], fail(""));
        assert_eq!(
            run_op(&runner, &commit("subj\n", false)),
            Err("git commit failed".into())
        );
    }

    #[test]
    #[cfg_attr(windows, ignore = "Windows forbids * in file names")]
    fn a_file_named_like_a_glob_stages_and_unstages_only_itself_in_real_git() {
        use crate::git::fake::{TempDir, git};
        use crate::git::status::read_status;

        let tmp = TempDir::new("ops");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        for name in ["a.rs", "*.rs"] {
            std::fs::write(dir.join(name), "old\n").unwrap();
        }
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "init"]);
        for name in ["a.rs", "*.rs"] {
            std::fs::write(dir.join(name), "new\n").unwrap();
        }
        let runner = crate::git::CliRunner::new(dir);
        let letters = || {
            let mut files: Vec<_> = read_status(&runner)
                .unwrap()
                .files
                .into_iter()
                .map(|f| {
                    let xy = f.status_letters();
                    (f.path.display().to_string(), xy)
                })
                .collect();
            files.sort();
            files
        };
        let glob = paths(&["*.rs"]);

        run_op(&runner, &GitOp::Stage(glob.clone())).unwrap();
        assert_eq!(
            letters(),
            [("*.rs".into(), "M ".into()), ("a.rs".into(), " M".into())]
        );

        run_op(&runner, &GitOp::Unstage(glob)).unwrap();
        assert_eq!(
            letters(),
            [("*.rs".into(), " M".into()), ("a.rs".into(), " M".into())]
        );
    }

    /// Regression: a name that is not UTF-8 used to be decoded lossily into
    /// argv, so toggling it staged its look-alike instead.
    #[cfg(unix)]
    #[test]
    fn a_name_that_is_not_utf8_stages_and_unstages_itself_not_its_look_alike() {
        use crate::git::fake::{LOOK_ALIKE, RAW_NAME, entry_named, git, look_alike_repo};

        let tmp = look_alike_repo();
        let dir = tmp.path();
        std::fs::write(dir.join(LOOK_ALIKE), "look-alike new\n").unwrap();
        let runner = crate::git::CliRunner::new(dir);
        let letters = |raw: &[u8]| entry_named(&runner, raw).map(|e| e.status_letters());
        assert_eq!(letters(RAW_NAME).as_deref(), Some(" D"));

        let raw = entry_named(&runner, RAW_NAME).unwrap();
        run_op(&runner, &GitOp::toggle_for(&raw).unwrap()).unwrap();
        assert_eq!(letters(RAW_NAME).as_deref(), Some("D "));
        assert_eq!(letters(LOOK_ALIKE.as_bytes()).as_deref(), Some(" M"));

        git(dir, &["add", "--", LOOK_ALIKE]);
        let raw = entry_named(&runner, RAW_NAME).unwrap();
        run_op(&runner, &GitOp::toggle_for(&raw).unwrap()).unwrap();
        assert_eq!(letters(RAW_NAME).as_deref(), Some(" D"));
        assert_eq!(letters(LOOK_ALIKE.as_bytes()).as_deref(), Some("M "));
    }

    // --- Stash ---

    use crate::git::stash::StashRef;

    const STASH_OID: &str = "5a5a5a";

    fn at(index: usize) -> StashRef {
        StashRef {
            index,
            oid: oid(STASH_OID),
        }
    }

    fn push(mode: StashMode, message: Option<&str>, paths: &[&str]) -> GitOp {
        GitOp::StashPush {
            mode,
            message: message.map(Into::into),
            paths: paths.iter().map(|&p| p.into()).collect(),
        }
    }

    fn literal_env(cmd: &GitCommand) -> Vec<(&str, &str)> {
        cmd.envs()
            .iter()
            .map(|(k, v)| (k.to_str().unwrap(), v.to_str().unwrap()))
            .collect()
    }

    #[test]
    fn each_stash_push_mode_with_and_without_a_message() {
        let cases: [(GitOp, &[&str]); 7] = [
            (push(StashMode::All, None, &[]), &["stash", "push"]),
            (
                push(StashMode::All, Some("wip"), &[]),
                &["stash", "push", "-m", "wip"],
            ),
            (
                push(StashMode::IncludeUntracked, None, &[]),
                &["stash", "push", "-u"],
            ),
            (
                push(StashMode::IncludeUntracked, Some("-x"), &[]),
                &["stash", "push", "-u", "-m", "-x"],
            ),
            (
                push(StashMode::Staged, None, &[]),
                &["stash", "push", "--staged"],
            ),
            (
                push(StashMode::Staged, Some("idx"), &[]),
                &["stash", "push", "--staged", "-m", "idx"],
            ),
            // An empty message is git's own.
            (push(StashMode::All, Some("  "), &[]), &["stash", "push"]),
        ];
        for (op, expected) in cases {
            let cmd = op_command(&op);
            assert_eq!(argv(&cmd), expected, "{op:?}");
            assert_eq!(literal_env(&cmd), [("LC_ALL", "C")], "{op:?}");
        }
    }

    #[test]
    fn stashing_one_file_names_it_after_the_separator_and_untracked_adds_u() {
        let changed = entry("-a.rs", '.', 'M', EntryKind::Changed);
        let untracked = entry("new.rs", '?', '?', EntryKind::Untracked);
        let mut renamed = entry("new name.rs", 'R', '.', EntryKind::Renamed);
        renamed.orig_path = Some("old.rs".into());
        let conflicted = entry("both.rs", 'U', 'U', EntryKind::Conflicted);

        let cases: [(&FileEntry, &[&str]); 3] = [
            (&changed, &["stash", "push", "-m", "m", "--", "-a.rs"]),
            (
                &untracked,
                &["stash", "push", "-u", "-m", "m", "--", "new.rs"],
            ),
            (
                &renamed,
                &["stash", "push", "-m", "m", "--", "old.rs", "new name.rs"],
            ),
        ];
        for (entry, expected) in cases {
            let (mode, paths) = GitOp::stash_file_for(entry).unwrap();
            let op = GitOp::StashPush {
                mode,
                message: Some("m".into()),
                paths,
            };
            let cmd = op_command(&op);
            assert_eq!(argv(&cmd), expected, "{op:?}");
            assert_eq!(
                literal_env(&cmd),
                [("GIT_LITERAL_PATHSPECS", "1"), ("LC_ALL", "C")]
            );
        }
        assert_eq!(GitOp::stash_file_for(&conflicted), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_stashed_files_name_reaches_argv_as_its_raw_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let mut untracked = entry("x", '?', '?', EntryKind::Untracked);
        untracked.path = RepoPath::from_bytes(b"x\xff");
        let (mode, paths) = GitOp::stash_file_for(&untracked).unwrap();
        let cmd = op_command(&GitOp::StashPush {
            mode,
            message: None,
            paths,
        });
        let args: Vec<&[u8]> = cmd.args().iter().map(|a| a.as_bytes()).collect();
        assert_eq!(args, [&b"stash"[..], b"push", b"-u", b"--", b"x\xff"]);
    }

    #[test]
    fn apply_pop_and_drop_name_the_entry_by_its_place() {
        let cases: [(GitOp, &[&str]); 3] = [
            (GitOp::StashApply(at(3)), &["stash", "apply", "stash@{3}"]),
            (GitOp::StashPop(at(0)), &["stash", "pop", "stash@{0}"]),
            (GitOp::StashDrop(at(12)), &["stash", "drop", "stash@{12}"]),
        ];
        for (op, expected) in cases {
            let cmd = op_command(&op);
            assert_eq!(argv(&cmd), expected, "{op:?}");
            assert_eq!(literal_env(&cmd), [("LC_ALL", "C")], "{op:?}");
        }
    }

    fn verify(index: usize, found: &str) -> (Vec<String>, GitOutput) {
        let argv = ["rev-parse", "-q", "--verify", &format!("stash@{{{index}}}")]
            .map(String::from)
            .to_vec();
        let output = if found.is_empty() {
            fail("")
        } else {
            ok(format!("{found}\n").as_bytes())
        };
        (argv, output)
    }

    fn expect_verify(runner: FakeRunner, index: usize, found: &str) -> FakeRunner {
        let (argv, output) = verify(index, found);
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        runner.expect(&argv, output)
    }

    #[test]
    fn each_stash_action_checks_its_entry_first_then_runs() {
        for (op, expected) in [
            (GitOp::StashApply(at(2)), ["stash", "apply", "stash@{2}"]),
            (GitOp::StashPop(at(2)), ["stash", "pop", "stash@{2}"]),
            (GitOp::StashDrop(at(2)), ["stash", "drop", "stash@{2}"]),
        ] {
            let runner = expect_verify(FakeRunner::new(), 2, STASH_OID).expect(&expected, ok(b""));
            assert_eq!(run_op(&runner, &op), Ok(()), "{op:?}");
            runner.assert_done();
        }
    }

    #[test]
    fn rename_stores_the_entry_then_drops_its_old_place_one_further_down() {
        let op = GitOp::StashRename {
            stash: at(2),
            message: "better words".into(),
        };
        assert_eq!(
            argv(&op_command(&op)),
            ["stash", "store", "-m", "better words", STASH_OID]
        );
        let runner = expect_verify(FakeRunner::new(), 2, STASH_OID);
        let runner = expect_verify(runner, 0, "0ther");
        let runner = runner.expect(
            &["stash", "store", "-m", "better words", STASH_OID],
            ok(b""),
        );
        let runner =
            expect_verify(runner, 3, STASH_OID).expect(&["stash", "drop", "stash@{3}"], ok(b""));
        assert_eq!(run_op(&runner, &op), Ok(()));
        runner.assert_done();
    }

    #[test]
    fn renaming_the_top_entry_drops_it_first_since_storing_it_again_adds_nothing() {
        let op = GitOp::StashRename {
            stash: at(0),
            message: "m".into(),
        };
        let store = ["stash", "store", "-m", "m", STASH_OID];
        let runner = expect_verify(FakeRunner::new(), 0, STASH_OID);
        let runner = expect_verify(runner, 0, STASH_OID)
            .expect(&["stash", "drop", "stash@{0}"], ok(b""))
            .expect(&store, ok(b""));
        assert_eq!(run_op(&runner, &op), Ok(()));
        runner.assert_done();

        // A failed store says how to bring the entry back.
        let runner = expect_verify(FakeRunner::new(), 0, STASH_OID);
        let runner = expect_verify(runner, 0, STASH_OID)
            .expect(&["stash", "drop", "stash@{0}"], ok(b""))
            .expect(&store, fail("fatal: no space"));
        let error = run_op(&runner, &op).unwrap_err();
        assert!(error.starts_with("fatal: no space\n"), "{error}");
        assert!(
            error.contains(&format!("git stash store {STASH_OID}")),
            "{error}"
        );
    }

    #[test]
    fn a_rename_whose_old_place_moved_after_the_store_drops_nothing() {
        let op = GitOp::StashRename {
            stash: at(1),
            message: "m".into(),
        };
        let runner = expect_verify(FakeRunner::new(), 1, STASH_OID);
        let runner = expect_verify(runner, 0, "0ther")
            .expect(&["stash", "store", "-m", "m", STASH_OID], ok(b""));
        let runner = expect_verify(runner, 2, "f00");
        assert_eq!(run_op(&runner, &op), Err(STASH_LIST_CHANGED.into()));
        runner.assert_done();
    }

    #[test]
    fn a_stash_that_moved_is_refused_without_running_anything() {
        let ops = [
            GitOp::StashApply(at(1)),
            GitOp::StashPop(at(1)),
            GitOp::StashDrop(at(1)),
            GitOp::StashRename {
                stash: at(1),
                message: "m".into(),
            },
        ];
        for op in ops {
            for found in ["f00", ""] {
                let runner = expect_verify(FakeRunner::new(), 1, found);
                assert_eq!(
                    run_op(&runner, &op),
                    Err("The stash list changed; refresh and try again".into()),
                    "{op:?}"
                );
                runner.assert_done();
            }
        }
    }

    #[test]
    fn stashing_only_staged_changes_needs_git_2_35() {
        let op = push(StashMode::Staged, None, &[]);
        let runner = FakeRunner::new().expect(&["--version"], ok(b"git version 2.34.1\n"));
        assert_eq!(
            run_op(&runner, &op),
            Err("Stashing only staged changes needs Git 2.35 or newer; found 2.34.1".into())
        );
        runner.assert_done();

        for version in [&b"git version 2.35.0\n"[..], b"git version odd\n"] {
            let runner = FakeRunner::new()
                .expect(&["--version"], ok(version))
                .expect(&["stash", "push", "--staged"], ok(b""));
            assert_eq!(run_op(&runner, &op), Ok(()));
            runner.assert_done();
        }

        // The other modes ask no version.
        let runner = FakeRunner::new().expect(&["stash", "push", "-u"], ok(b""));
        let op = push(StashMode::IncludeUntracked, None, &[]);
        assert_eq!(run_op(&runner, &op), Ok(()));
        runner.assert_done();
    }

    #[test]
    fn a_failed_apply_shows_the_conflicts_git_printed_on_stdout() {
        let output = GitOutput {
            success: false,
            stdout: b"CONFLICT (content): Merge conflict in a.txt\n".to_vec(),
            stderr: b"error: could not apply\n".to_vec(),
        };
        for op in [GitOp::StashApply(at(0)), GitOp::StashPop(at(0))] {
            let verb = if matches!(op, GitOp::StashPop(_)) {
                "pop"
            } else {
                "apply"
            };
            let runner = expect_verify(FakeRunner::new(), 0, STASH_OID)
                .expect(&["stash", verb, "stash@{0}"], output.clone());
            assert_eq!(
                run_op(&runner, &op),
                Err("error: could not apply\nCONFLICT (content): Merge conflict in a.txt".into())
            );
        }
    }

    // --- Stash, in real git ---

    /// `a.txt` and `b.txt` committed, with an identity and no signing.
    fn stash_repo() -> crate::git::fake::TempDir {
        use crate::git::fake::{TempDir, git};

        let tmp = TempDir::new("stash");
        let dir = tmp.path();
        git(dir, &["init", "-q", "--template="]);
        for (key, value) in [
            ("user.name", "Kika Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
        ] {
            git(dir, &["config", key, value]);
        }
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        std::fs::write(dir.join("b.txt"), "b\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "first"]);
        tmp
    }

    fn porcelain(dir: &Path) -> String {
        String::from_utf8(crate::git::fake::git(dir, &["status", "--porcelain"])).unwrap()
    }

    fn read(dir: &Path, name: &str) -> String {
        std::fs::read_to_string(dir.join(name)).unwrap()
    }

    fn real_entries(dir: &Path) -> Vec<crate::git::stash::StashEntry> {
        crate::git::stash::list_stashes(&crate::git::CliRunner::isolated(dir)).unwrap()
    }

    fn real_ref(dir: &Path, index: usize) -> StashRef {
        real_entries(dir)[index].stash_ref()
    }

    #[test]
    fn stash_all_leaves_a_clean_worktree_but_the_untracked_file() {
        let tmp = stash_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "changed\n").unwrap();
        std::fs::write(dir.join("new.txt"), "new\n").unwrap();
        let runner = crate::git::CliRunner::isolated(dir);
        run_op(&runner, &push(StashMode::All, Some("wip"), &[])).unwrap();
        assert_eq!(porcelain(dir), "?? new.txt\n");
        assert_eq!(read(dir, "a.txt"), "a\n");
        let entries = real_entries(dir);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].subject.ends_with(": wip"), "{entries:?}");
        assert_eq!(entries[0].untracked, None);
    }

    #[test]
    fn stash_including_untracked_takes_the_untracked_file_into_a_third_parent() {
        let tmp = stash_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "changed\n").unwrap();
        std::fs::write(dir.join("new.txt"), "new\n").unwrap();
        let runner = crate::git::CliRunner::isolated(dir);
        run_op(&runner, &push(StashMode::IncludeUntracked, None, &[])).unwrap();
        assert_eq!(porcelain(dir), "");
        assert!(!dir.join("new.txt").exists());
        assert!(real_entries(dir)[0].untracked.is_some());
    }

    /// `--staged` came in git 2.35; an older one skips this test.
    fn has_staged_stash() -> bool {
        let out = crate::git::fake::git(Path::new("."), &["--version"]);
        parse_version(&out).is_some_and(|v| v >= STASH_STAGED_GIT)
    }

    #[test]
    fn stash_staged_takes_only_the_staged_change() {
        if !has_staged_stash() {
            eprintln!("skipped: git older than 2.35 has no stash --staged");
            return;
        }
        let tmp = stash_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "staged\n").unwrap();
        crate::git::fake::git(dir, &["add", "a.txt"]);
        std::fs::write(dir.join("b.txt"), "unstaged\n").unwrap();
        let runner = crate::git::CliRunner::isolated(dir);
        run_op(&runner, &push(StashMode::Staged, None, &[])).unwrap();
        assert_eq!(porcelain(dir), " M b.txt\n");
        assert_eq!(read(dir, "a.txt"), "a\n");
        assert_eq!(read(dir, "b.txt"), "unstaged\n");
    }

    #[test]
    fn stash_this_file_takes_only_it_and_leaves_the_other_byte_for_byte() {
        let tmp = stash_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "changed a\n").unwrap();
        let other = b"changed b\r\nno newline \xff";
        std::fs::write(dir.join("b.txt"), other).unwrap();
        let runner = crate::git::CliRunner::isolated(dir);
        let a = entry("a.txt", '.', 'M', EntryKind::Changed);
        let (mode, paths) = GitOp::stash_file_for(&a).unwrap();
        run_op(
            &runner,
            &GitOp::StashPush {
                mode,
                message: None,
                paths,
            },
        )
        .unwrap();
        assert_eq!(read(dir, "a.txt"), "a\n");
        assert_eq!(std::fs::read(dir.join("b.txt")).unwrap(), other);
        assert_eq!(porcelain(dir), " M b.txt\n");

        // An untracked file goes too, with `-u`.
        std::fs::write(dir.join("new.txt"), "new\n").unwrap();
        let new = entry("new.txt", '?', '?', EntryKind::Untracked);
        let (mode, paths) = GitOp::stash_file_for(&new).unwrap();
        run_op(
            &runner,
            &GitOp::StashPush {
                mode,
                message: None,
                paths,
            },
        )
        .unwrap();
        assert!(!dir.join("new.txt").exists());
        assert_eq!(porcelain(dir), " M b.txt\n");
        assert_eq!(real_entries(dir).len(), 2);
    }

    #[test]
    fn apply_keeps_the_entry_and_pop_and_drop_remove_it() {
        let tmp = stash_repo();
        let dir = tmp.path();
        let runner = crate::git::CliRunner::isolated(dir);
        std::fs::write(dir.join("a.txt"), "changed\n").unwrap();
        run_op(&runner, &push(StashMode::All, None, &[])).unwrap();

        run_op(&runner, &GitOp::StashApply(real_ref(dir, 0))).unwrap();
        assert_eq!(read(dir, "a.txt"), "changed\n");
        assert_eq!(real_entries(dir).len(), 1);

        crate::git::fake::git(dir, &["checkout", "-q", "--", "a.txt"]);
        run_op(&runner, &GitOp::StashPop(real_ref(dir, 0))).unwrap();
        assert_eq!(read(dir, "a.txt"), "changed\n");
        assert_eq!(real_entries(dir).len(), 0);

        run_op(&runner, &push(StashMode::All, None, &[])).unwrap();
        run_op(&runner, &GitOp::StashDrop(real_ref(dir, 0))).unwrap();
        assert_eq!(real_entries(dir).len(), 0);
        assert_eq!(read(dir, "a.txt"), "a\n");
    }

    #[test]
    fn rename_puts_the_new_subject_on_top_with_the_same_tree_and_count() {
        let tmp = stash_repo();
        let dir = tmp.path();
        let runner = crate::git::CliRunner::isolated(dir);
        for (name, text) in [("a.txt", "one\n"), ("b.txt", "two\n"), ("a.txt", "three\n")] {
            std::fs::write(dir.join(name), text).unwrap();
            run_op(&runner, &push(StashMode::All, Some(text.trim()), &[])).unwrap();
        }
        let tree =
            |rev: &str| crate::git::fake::git(dir, &["rev-parse", &format!("{rev}^{{tree}}")]);

        // The middle one, then the top one, which takes the other path.
        for (index, subject) in [(1, "renamed two"), (0, "renamed again")] {
            let before = real_entries(dir);
            let moved = before[index].clone();
            let old_tree = tree(moved.oid.as_str());
            run_op(
                &runner,
                &GitOp::StashRename {
                    stash: moved.stash_ref(),
                    message: subject.into(),
                },
            )
            .unwrap();
            let after = real_entries(dir);
            assert_eq!(after.len(), 3, "{after:?}");
            assert_eq!(after[0].subject, subject);
            assert_eq!(after[0].oid, moved.oid);
            assert_eq!(tree("stash@{0}"), old_tree);
            // Every other entry is still there, in its order.
            let others = |entries: &[crate::git::stash::StashEntry]| {
                entries
                    .iter()
                    .filter(|e| e.oid != moved.oid)
                    .map(|e| e.oid.clone())
                    .collect::<Vec<_>>()
            };
            assert_eq!(others(&after), others(&before));
        }
    }
}
