//! Mutations of the index and history. Each is a [`GitOp`] so the reducer can
//! name the work, and the refresh planner can say what it invalidates.

use std::path::Path;

use crate::features::branches::{self, remotes, tags};
use crate::features::commit::git::{MessageSource, commit_command};
use crate::features::commits::bisect::git::run_bisect_start;
use crate::features::commits::custom_patch::{CustomPatch, PatchAction};
use crate::features::commits::git::detached_checkout;
use crate::features::commits::reflog::{self, Direction, Expected, Step};
use crate::features::commits::rewrite;
use crate::features::stash::{self, StashMode, StashRef};
use kika_git::bisect::BisectMark;
use kika_git::branches::BranchName;
use kika_git::commit_files::CommitId;
use kika_git::error::GitError;
use kika_git::path::RepoPath;
use kika_git::rebase_plan::RebasePlan;
use kika_git::remotes::{RemoteBranch, RemoteName};
use kika_git::repo_state::Operation;
use kika_git::runner::{GitCommand, GitRunner};
use kika_git::status::{EntryKind, FileEntry};
use kika_git::tags::{TagName, validate_tag_name};

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
    /// `crate::prompts`). Otherwise git runs detached, with the message on
    /// stdin.
    ///
    /// With `amend`, the staged changes are folded into `HEAD` instead; a
    /// `None` message keeps `HEAD`'s. With `fixup`, the commit is a
    /// `fixup!` of that commit and git writes its message from the raw
    /// subject, so a `None` message is valid there too. With `no_verify`,
    /// git skips the pre-commit and commit-msg hooks, and only then, since
    /// a hook is the user's own check.
    Commit {
        message: Option<String>,
        stage_all: bool,
        amend: bool,
        no_verify: bool,
        fixup: Option<CommitId>,
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
    /// Discard a range of files at once: restore `restore` as
    /// [`GitOp::DiscardWorktree`] does, then delete the untracked `delete`
    /// as [`GitOp::DeleteUntracked`] does, stopping at the first failure.
    DiscardFiles {
        restore: Vec<RepoPath>,
        delete: Vec<RepoPath>,
    },
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
    /// Rewrite the commits above the plan's base with git's interactive
    /// rebase, Kika acting as the sequence editor (see
    /// `crate::sequence_editor`). Uncommitted changes are stashed and put
    /// back.
    InteractiveRebase(RebasePlan),
    /// Rewrite history with the custom patch `picks`, as `action` says:
    /// the interactive rebase of `plan`, stopping at each commit the patch
    /// changes (see `crate::features::commits::custom_patch::git`).
    /// Refused unless `HEAD` is still `orig_head`; any failure puts
    /// `HEAD`, the index and the worktree back as they were.
    CustomPatch {
        plan: RebasePlan,
        action: PatchAction,
        picks: CustomPatch,
        orig_head: CommitId,
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
    /// Pick these commits onto `HEAD`, one by one in this order. Built
    /// with parents first (see `crate::features::commits::copy::git::ordered`);
    /// never with a merge commit, which needs a mainline.
    CherryPick(Vec<CommitId>),
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
    /// Make branch `name` at the commit `at` and switch to it: how a
    /// detached `HEAD` gets a branch.
    CreateBranchAt {
        name: String,
        at: CommitId,
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
    /// Delete these local branches in one `git branch -d`. Never forced:
    /// git refuses any whose commits are not merged and deletes the rest.
    DeleteBranches(Vec<BranchName>),
    /// Resolve this conflicted file in the user's merge tool, on the
    /// terminal. The tool may ask which one to use or wait for Enter.
    MergeTool(RepoPath),
    /// Put changes aside as a new `stash@{0}`, as `mode` says, with the
    /// message given or git's own. With `paths`, only those files go.
    /// `restore` holds the old sides of staged renames among them: git
    /// no longer knows those paths in the index, so they are left out of
    /// the pathspec and checked out from `HEAD` once the push is done.
    /// `unstage` holds staged deletions among `paths`: git refuses them as
    /// a pathspec for the same reason, and a deletion has no other path to
    /// carry it, so each is reset to `HEAD` in the index first, which
    /// leaves it deleted in the worktree for the push to take.
    StashPush {
        mode: StashMode,
        message: Option<String>,
        paths: Vec<RepoPath>,
        restore: Vec<RepoPath>,
        unstage: Vec<RepoPath>,
    },
    /// Apply the entry to the worktree and keep it in the list.
    StashApply(StashRef),
    /// Apply the entry and drop it from the list once it applied cleanly.
    StashPop(StashRef),
    /// Drop the entry from the list. Nothing in the worktree changes.
    StashDrop(StashRef),
    /// Drop these entries, deepest first, so no drop renumbers one still
    /// to go. Every entry is checked before any is dropped.
    StashDrops(Vec<StashRef>),
    /// Give the entry a new message. Git has no rename, so it is stored
    /// again as `stash@{0}` and the old place dropped.
    StashRename {
        stash: StashRef,
        message: String,
    },
    /// Make branch `name` at the remote branch `from`, tracking it, and
    /// switch to it. Git refuses when `name` exists.
    CheckoutTracking {
        name: String,
        from: RemoteBranch,
    },
    /// Make `branch` track the remote branch `upstream`.
    SetUpstream {
        branch: BranchName,
        upstream: RemoteBranch,
    },
    /// Stop `branch` tracking anything.
    UnsetUpstream(BranchName),
    /// Add a remote. Never fetched here: its branches appear after the
    /// user's own fetch.
    AddRemote {
        name: String,
        url: String,
    },
    /// Rename a remote, its remote-tracking branches and the upstreams
    /// that name it.
    RenameRemote {
        from: RemoteName,
        to: String,
    },
    /// Remove a remote, its remote-tracking branches and the upstreams
    /// that name it.
    RemoveRemote(RemoteName),
    /// Remove these remotes one at a time, as [`GitOp::RemoveRemote`]
    /// does, stopping at the first failure.
    RemoveRemotes(Vec<RemoteName>),
    /// Undo or redo the last action, as the undo planner chose (see
    /// `commits::reflog::undo`). Git writes `direction`'s marker into the
    /// reflog, which is how the next plan knows this step happened. It
    /// runs only while `HEAD` is still where `expect` says, as when the
    /// user was asked.
    Travel {
        direction: Direction,
        step: Step,
        expect: Expected,
    },
    /// Tag `target` as `name`: lightweight without a message, annotated
    /// with one. Never forced, so git refuses a name that exists.
    CreateTag {
        name: String,
        target: CommitId,
        message: Option<String>,
    },
    /// Delete tag `name`, but only while its ref still holds `oid`, the
    /// object it held when listed. Local only.
    DeleteTag {
        name: TagName,
        oid: String,
    },
    /// Delete these tags, each with the object it held when listed, one at
    /// a time as [`GitOp::DeleteTag`] does, stopping at the first failure.
    /// Local only.
    DeleteTags(Vec<(TagName, String)>),
    /// Mark `commit` good, bad or skipped in the bisect; with `start`, a
    /// plain `bisect start` runs first. Once there is a good and a bad
    /// mark, git checks out the next commit to test.
    BisectMark {
        mark: BisectMark,
        commit: CommitId,
        start: bool,
    },
    /// End the bisect and check out where it started.
    BisectReset,
}

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

/// Why an undo or redo stopped before running: `HEAD` is no longer where
/// the question said, so the step would move something else.
pub const HEAD_MOVED: &str = "HEAD moved since the question was asked; nothing was changed";

impl GitOp {
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

    /// What discarding a range of entries does: each as
    /// [`GitOp::discard_for`] would, skipping those it refuses, in one op.
    /// `None` when nothing is left to discard.
    pub fn discard_many(entries: &[&FileEntry]) -> Option<GitOp> {
        let (mut restore, mut delete) = (Vec::new(), Vec::new());
        for entry in entries {
            match GitOp::discard_for(entry) {
                Ok(Some(GitOp::DiscardWorktree(paths))) => restore.extend(paths),
                Ok(Some(GitOp::DeleteUntracked(paths))) => delete.extend(paths),
                _ => {}
            }
        }
        match (restore.is_empty(), delete.is_empty()) {
            (true, true) => None,
            (false, true) => Some(GitOp::DiscardWorktree(restore)),
            (true, false) => Some(GitOp::DeleteUntracked(delete)),
            (false, false) => Some(GitOp::DiscardFiles { restore, delete }),
        }
    }
}

/// The command for `op`. Unlike reads, these need `index.lock`, so most
/// run with [`EnvProfile::Write`](kika_git::runner::EnvProfile::Write):
/// no `GIT_OPTIONAL_LOCKS=0`, and file names from the repo read literally,
/// never as globs or pathspec magic. A commit runs in the user's env: its
/// hooks inherit it, and must see what they would see outside Kika. Git
/// runs hooks with stdin closed, so the message on stdin never reaches
/// them. Merges, rebases, reverts, their continue, skip and abort, and
/// bisects run in the user's env for the same reason.
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
        GitOp::DeleteUntracked(paths) | GitOp::DiscardFiles { delete: paths, .. } => {
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
        GitOp::Checkout(name) => checkout_command(name),
        GitOp::CreateBranch { name, from } => branches::git::create_command(name, from),
        GitOp::CreateBranchAt { name, at } => branches::git::create_at_command(name, at),
        GitOp::Travel {
            direction, step, ..
        } => reflog::git::travel_command(*direction, step),
        // The id is hex digits only, so it cannot be read as an option;
        // `--` keeps it from being read as a path.
        GitOp::CheckoutCommit(oid) => detached_checkout(oid),
        GitOp::Reset { to, mode } => GitCommand::new("reset")
            .arg("-q")
            .arg(mode.flag())
            .arg(to.as_str())
            .arg("--"),
        GitOp::StashPush {
            mode,
            message,
            paths,
            ..
        } => return stash::git::push_command(*mode, message.as_deref(), paths),
        // These take no file names, so nothing is read as a pathspec.
        GitOp::StashApply(stash) => return stash::git::stash_command("apply", stash),
        GitOp::StashPop(stash) => return stash::git::stash_command("pop", stash),
        GitOp::StashDrop(stash) => return stash::git::stash_command("drop", stash),
        // The first of the two steps `run_op` takes; see `rename_stash`.
        GitOp::StashRename { stash, message } => return stash::git::store_command(stash, message),
        GitOp::RenameBranch { from, to } => branches::git::rename_command(from, to),
        GitOp::DeleteBranch { name, force } => branches::git::delete_command(name, *force),
        GitOp::DeleteBranches(names) => names.iter().fold(
            GitCommand::new("branch").arg("-d").arg("--"),
            |cmd, name| cmd.arg(name.as_os_string()),
        ),
        GitOp::CheckoutTracking { name, from } => remotes::git::track_command(name, from),
        GitOp::SetUpstream { branch, upstream } => {
            branches::git::set_upstream_command(branch, upstream)
        }
        GitOp::UnsetUpstream(branch) => branches::git::unset_upstream_command(branch),
        GitOp::AddRemote { name, url } => remotes::git::add_command(name, url),
        GitOp::RenameRemote { from, to } => remotes::git::rename_command(from, to),
        GitOp::RemoveRemote(name) => remotes::git::remove_command(name),
        // The first of the steps `run_op` takes, one per remote, tag or stash;
        // `refusal` keeps an empty list from running.
        GitOp::RemoveRemotes(_) | GitOp::DeleteTags(_) | GitOp::StashDrops(_) => {
            return op_steps(op)
                .first()
                .map_or_else(|| GitCommand::new("remote").user_env(), op_command);
        }
        GitOp::CreateTag {
            name,
            target,
            message,
        } => tags::git::create_command(name, target, message.as_deref()),
        GitOp::DeleteTag { name, oid } => tags::git::delete_command(name, oid),
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
            return GitCommand::new("merge")
                .arg(flag)
                .arg(branch.full_ref())
                .user_env();
        }
        GitOp::Rebase { onto } => {
            return GitCommand::new("rebase").arg(onto.full_ref()).user_env();
        }
        GitOp::Revert(oid) => return GitCommand::new("revert").arg(oid.as_str()).user_env(),
        // Ids are hex digits only, so none is read as an option.
        GitOp::CherryPick(picks) => {
            return picks
                .iter()
                .fold(GitCommand::new("cherry-pick"), |cmd, pick| {
                    cmd.arg(pick.as_str())
                })
                .user_env();
        }
        GitOp::InteractiveRebase(plan) => return rewrite::git::rebase_command(plan),
        // Its first step; the rest run from
        // `crate::features::commits::custom_patch::git::run`.
        GitOp::CustomPatch { plan, .. } => {
            return op_command(&GitOp::InteractiveRebase(plan.clone()));
        }
        GitOp::Continue(operation) => {
            return GitCommand::new(operation.subcommand())
                .arg("--continue")
                .user_env();
        }
        // Git rejects `merge --skip` without touching anything; no key
        // builds it. `cherry-pick --skip` and `revert --skip` need git 2.23,
        // and an older one rejects them the same way.
        GitOp::Skip(operation) => {
            debug_assert!(operation.can_skip(), "{operation:?} cannot be skipped");
            return GitCommand::new(operation.subcommand())
                .arg("--skip")
                .user_env();
        }
        GitOp::Abort(operation) => {
            return GitCommand::new(operation.subcommand())
                .arg("--abort")
                .user_env();
        }
        // A bisect checks commits out, so hooks run and see the user's env,
        // as for a merge. Only fixed words and hex digits reach argv;
        // nothing read from the bisect's own files does. Every subcommand
        // here predates git 2.15.
        GitOp::BisectMark { mark, commit, .. } => {
            return GitCommand::new("bisect")
                .arg(mark.word())
                .arg(commit.as_str())
                .user_env();
        }
        GitOp::BisectReset => return GitCommand::new("bisect").arg("reset").user_env(),
        // The user's tool runs on the terminal and keeps their locale, so
        // no `LC_ALL`; the name is still read literally.
        GitOp::MergeTool(path) => {
            return with_paths(GitCommand::new("mergetool"), std::slice::from_ref(path))
                .user_env()
                .env("GIT_LITERAL_PATHSPECS", "1");
        }
        GitOp::Commit {
            message,
            amend,
            no_verify,
            fixup,
            ..
        } => {
            let source = match (message, fixup) {
                (Some(_), _) => MessageSource::File(Path::new("-")),
                (None, Some(target)) => MessageSource::Fixup(target),
                (None, None) => MessageSource::Keep,
            };
            let cmd = commit_command(*amend, *no_verify, source);
            return match message {
                Some(message) => cmd.stdin(message.clone().into_bytes()),
                None => cmd,
            };
        }
    };
    cmd.write_env()
}

/// Switch to the local branch `name`. A short name, since a full ref
/// would detach `HEAD`; `--` keeps it from being read as a path. A ref
/// made outside git's porcelain can be named like an option, such as
/// `-f`, which would force the checkout and lose the worktree's changes;
/// `--end-of-options` keeps it a name. Only then: it needs git 2.24, newer
/// than Kika requires, and an older git fails on it without touching
/// anything.
pub(crate) fn checkout_command(name: &BranchName) -> GitCommand {
    let cmd = GitCommand::new("checkout").arg("-q");
    let cmd = if name.0.starts_with(b"-") {
        cmd.arg("--end-of-options")
    } else {
        cmd
    };
    cmd.arg(name.as_os_string()).arg("--")
}

/// The single ops a many-step op runs, in order; any other op is its own
/// one step.
pub fn op_steps(op: &GitOp) -> Vec<GitOp> {
    match op {
        GitOp::RemoveRemotes(names) => names.iter().cloned().map(GitOp::RemoveRemote).collect(),
        GitOp::DeleteTags(tags) => tags
            .iter()
            .map(|(name, oid)| GitOp::DeleteTag {
                name: name.clone(),
                oid: oid.clone(),
            })
            .collect(),
        // Deepest first: dropping `stash@{N}` renumbers only those below it.
        GitOp::StashDrops(stashes) => {
            let mut stashes = stashes.clone();
            stashes.sort_by_key(|s| std::cmp::Reverse(s.index));
            stashes.into_iter().map(GitOp::StashDrop).collect()
        }
        other => vec![other.clone()],
    }
}

/// Run `op`. Failures carry git's message as a [`GitError`] gives it: made
/// safe to draw, since it can quote repo-controlled paths, and with any
/// URL's credentials removed. Some ops' failures also carry stdout; see
/// [`reports_on_stdout`].
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
    // The restore runs first; the command below is the delete.
    if let GitOp::DiscardFiles { restore, .. } = op {
        run_op(runner, &GitOp::DiscardWorktree(restore.clone()))?;
    }
    match op {
        GitOp::StashApply(stash)
        | GitOp::StashPop(stash)
        | GitOp::StashDrop(stash)
        | GitOp::StashRename { stash, .. } => stash::git::verify_stash(runner, stash)?,
        // Fail closed: one entry that moved means the list changed, so
        // nothing is dropped. Each step checks its own entry again.
        GitOp::StashDrops(stashes) => {
            for stash in stashes {
                stash::git::verify_stash(runner, stash)?;
            }
        }
        GitOp::StashPush {
            mode: StashMode::Staged,
            ..
        } => stash::git::staged_stash_supported(runner)?,
        GitOp::StashPush {
            restore, unstage, ..
        } if !restore.is_empty() || !unstage.is_empty() => {
            return stash::push::push_with_fixups(runner, op, restore, unstage);
        }
        _ => {}
    }
    if let GitOp::StashRename { stash, message } = op {
        return stash::git::rename_stash(runner, stash, message);
    }
    if let GitOp::BisectMark { start: true, .. } = op {
        run_bisect_start(runner)?;
    }
    if let Some(reason) = refusal(op) {
        return Err(reason.into());
    }
    if let GitOp::RemoveRemotes(_) | GitOp::DeleteTags(_) | GitOp::StashDrops(_) = op {
        return op_steps(op)
            .iter()
            .try_for_each(|step| run_op_with(runner, step, env_set));
    }
    if let GitOp::Travel { expect, .. } = op {
        let now = reflog::git::read_head(runner);
        if now.oid.as_ref() != Some(&expect.oid) || now.head_ref != expect.head_ref {
            return Err(HEAD_MOVED.into());
        }
    }
    let mut cmd = op_command(op);
    if matches!(op, GitOp::SubmoduleUpdate(_)) && ssh_is_default(runner, env_set) {
        cmd = cmd.env("GIT_SSH_COMMAND", BATCH_SSH);
    }
    let output = runner.run(&cmd).map_err(|e| GitError::io(&e).to_string())?;
    if output.success {
        return Ok(());
    }
    let error = if reports_on_stdout(op) {
        GitError::failed_with_stdout(&cmd, &output)
    } else {
        GitError::failed(&cmd, &output)
    };
    Err(error.to_string())
}

/// Whether git reports `op`'s failure on stdout as well as stderr: hooks
/// print to stderr, but git says "nothing to commit" and names each
/// conflict on stdout, and a bisect says there why a mark failed.
fn reports_on_stdout(op: &GitOp) -> bool {
    matches!(
        op,
        GitOp::Commit { .. }
            | GitOp::Merge { .. }
            | GitOp::Revert(_)
            | GitOp::StashApply(_)
            | GitOp::StashPop(_)
            | GitOp::CherryPick(_)
            | GitOp::BisectMark { .. }
            | GitOp::BisectReset
    )
}

/// Why `op` must not run at all. The reducer checks a new tag's name
/// before it builds the op; this checks it again where git would run, so
/// no other path can pass a name git would read as an option.
pub fn refusal(op: &GitOp) -> Option<&'static str> {
    match op {
        GitOp::CreateTag { name, .. } => validate_tag_name(name).err(),
        GitOp::DeleteBranches(names) if names.is_empty() => Some("No branch to delete"),
        GitOp::DeleteTags(tags) if tags.is_empty() => Some("No tag to delete"),
        GitOp::StashDrops(stashes) if stashes.is_empty() => Some("No stash to drop"),
        GitOp::RemoveRemotes(names) if names.is_empty() => Some("No remote to remove"),
        // `remote remove` takes no `--` in older git; see `RemoveRemote`.
        GitOp::RemoveRemotes(names) if names.iter().any(|n| n.0.starts_with(b"-")) => {
            Some("Change a remote named like an option with git directly")
        }
        // Only a full id is surely a commit, never a ref of that name.
        GitOp::CherryPick(picks)
            if picks.is_empty()
                || !picks
                    .iter()
                    .all(crate::features::commits::copy::git::is_full_id) =>
        {
            Some("Only full commit ids can be cherry-picked")
        }
        // Only its first step is one command; run alone it would leave
        // the rebase stopped.
        GitOp::CustomPatch { .. } => Some("A custom patch only runs on the terminal"),
        _ => None,
    }
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
        .standard_env();
    // Unset is exit 1 with nothing on stderr.
    matches!(runner.run(&cmd), Ok(out) if !out.success && out.stderr.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        STANDARD_ENV, argv, dirty_repo, env_list as env, file_entry as entry, paths,
    };
    use kika_git::fake::{FakeRunner, fail, ok};
    use kika_git::runner::{EnvProfile, GitOutput};

    /// The env of an op that takes the index lock.
    const WRITE: [(&str, &str); 2] = [("LC_ALL", "C"), ("GIT_LITERAL_PATHSPECS", "1")];

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
            assert_eq!(cmd.env_profile(), Some(EnvProfile::UserEnv), "{op:?}");
            assert!(cmd.process_env().is_empty(), "{op:?}: {:?}", cmd.envs());
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
            assert_eq!(cmd.env_profile(), Some(EnvProfile::UserEnv), "{op:?}");
            assert!(cmd.process_env().is_empty(), "{op:?}: {:?}", cmd.envs());
        }
    }

    #[test]
    fn the_merge_tool_reads_the_path_literally_and_keeps_the_users_locale() {
        let cmd = op_command(&GitOp::MergeTool("-x".into()));
        assert_eq!(argv(&cmd), ["mergetool", "--", "-x"]);
        assert_eq!(cmd.env_profile(), Some(EnvProfile::UserEnv));
        assert_eq!(
            cmd.process_env(),
            env(&[("GIT_LITERAL_PATHSPECS", "1")]),
            "no LC_ALL"
        );
    }

    /// Which variant `op` is, numbered so the table below must name every
    /// one: a new variant fails to compile here until it is listed.
    fn variant(op: &GitOp) -> usize {
        match op {
            GitOp::Stage(_) => 0,
            GitOp::Unstage(_) => 1,
            GitOp::StageAll => 2,
            GitOp::UnstageAll => 3,
            GitOp::Commit { .. } => 4,
            GitOp::ApplyCached { .. } => 5,
            GitOp::DiscardWorktree(_) => 6,
            GitOp::DeleteUntracked(_) => 7,
            GitOp::DiscardFiles { .. } => 8,
            GitOp::ApplyWorktree { .. } => 9,
            GitOp::ApplyEdited { .. } => 10,
            GitOp::SubmoduleInit(_) => 11,
            GitOp::SubmoduleUpdate(_) => 12,
            GitOp::Checkout(_) => 13,
            GitOp::Merge { .. } => 14,
            GitOp::Rebase { .. } => 15,
            GitOp::InteractiveRebase(_) => 16,
            GitOp::CustomPatch { .. } => 17,
            GitOp::Continue(_) => 18,
            GitOp::Skip(_) => 19,
            GitOp::Abort(_) => 20,
            GitOp::CheckoutCommit(_) => 21,
            GitOp::Revert(_) => 22,
            GitOp::CherryPick(_) => 23,
            GitOp::Reset { .. } => 24,
            GitOp::CreateBranch { .. } => 25,
            GitOp::RenameBranch { .. } => 26,
            GitOp::DeleteBranch { .. } => 27,
            GitOp::DeleteBranches(_) => 28,
            GitOp::MergeTool(_) => 29,
            GitOp::StashPush { .. } => 30,
            GitOp::StashApply(_) => 31,
            GitOp::StashPop(_) => 32,
            GitOp::StashDrop(_) => 33,
            GitOp::StashDrops(_) => 34,
            GitOp::StashRename { .. } => 35,
            GitOp::CheckoutTracking { .. } => 36,
            GitOp::SetUpstream { .. } => 37,
            GitOp::UnsetUpstream(_) => 38,
            GitOp::AddRemote { .. } => 39,
            GitOp::RenameRemote { .. } => 40,
            GitOp::RemoveRemote(_) => 41,
            GitOp::RemoveRemotes(_) => 42,
            GitOp::Travel { .. } => 43,
            GitOp::CreateTag { .. } => 44,
            GitOp::DeleteTag { .. } => 45,
            GitOp::DeleteTags(_) => 46,
            GitOp::BisectMark { .. } => 47,
            GitOp::BisectReset => 48,
            GitOp::CreateBranchAt { .. } => 49,
        }
    }

    /// Vars as `(key, value)` pairs.
    type Env<'a> = &'a [(&'a str, &'a str)];

    #[test]
    fn every_op_declares_its_env() {
        use kika_git::commit_files::CommitRange;
        use kika_git::rebase_plan::PLAN_ENV;
        use kika_git::rebase_plan::{encode, sample};
        use kika_git::remotes::RemoteBranch;
        use std::collections::BTreeSet;

        let id = |digit: &str| CommitId::parse(&digit.repeat(40)).unwrap();
        let stash = StashRef {
            index: 1,
            oid: id("5"),
        };
        let upstream = RemoteBranch {
            remote: "origin".into(),
            short: b"main".to_vec(),
            refname: b"refs/remotes/origin/main".to_vec(),
        };
        let push = |mode, paths: &[&str]| GitOp::StashPush {
            mode,
            message: None,
            paths: paths.iter().map(|&p| p.into()).collect(),
            restore: vec![],
            unstage: vec![],
        };
        let travel = |step| GitOp::Travel {
            direction: Direction::Undo,
            step,
            expect: Expected {
                head_ref: None,
                oid: id("d"),
            },
        };
        let plan = sample();
        let plan_env = encode(&plan.steps);
        let commit = GitOp::Commit {
            message: Some("m".into()),
            stage_all: false,
            amend: false,
            no_verify: false,
            fixup: None,
        };
        let marker = [("GIT_REFLOG_ACTION", "kika undo")];
        let undo_env = [WRITE.as_slice(), marker.as_slice()].concat();
        let update_env = [WRITE.as_slice(), &[("GIT_TERMINAL_PROMPT", "0")]].concat();
        let rebase_env = [(PLAN_ENV, plan_env.as_str())];
        use EnvProfile::{Locale, UserEnv, Write};
        let w: &[(&str, &str)] = &WRITE;
        let none: &[(&str, &str)] = &[];
        let cases: Vec<(GitOp, EnvProfile, Env)> = vec![
            (GitOp::Stage(paths(&["a"])), Write, w),
            (GitOp::Unstage(paths(&["a"])), Write, w),
            (GitOp::StageAll, Write, w),
            (GitOp::UnstageAll, Write, w),
            (commit, UserEnv, none),
            (
                GitOp::ApplyCached {
                    patch: vec![],
                    reverse: true,
                },
                Write,
                w,
            ),
            (GitOp::DiscardWorktree(paths(&["a"])), Write, w),
            (GitOp::DeleteUntracked(paths(&["a"])), Write, w),
            (
                GitOp::DiscardFiles {
                    restore: paths(&["a"]),
                    delete: paths(&["b"]),
                },
                Write,
                w,
            ),
            (GitOp::ApplyWorktree { patch: vec![] }, Write, w),
            (GitOp::ApplyEdited { patch: vec![] }, Write, w),
            (GitOp::SubmoduleInit("s".into()), Write, w),
            (GitOp::SubmoduleUpdate("s".into()), Write, &update_env),
            (GitOp::Checkout("main".into()), Write, w),
            (
                GitOp::Merge {
                    branch: "f".into(),
                    strategy: MergeStrategy::NoFastForward,
                },
                UserEnv,
                none,
            ),
            (GitOp::Rebase { onto: "f".into() }, UserEnv, none),
            (GitOp::InteractiveRebase(plan.clone()), UserEnv, &rebase_env),
            (
                GitOp::CustomPatch {
                    plan: plan.clone(),
                    action: PatchAction::Remove,
                    picks: CustomPatch::new(
                        CommitRange {
                            oid: id("b"),
                            parent: Some(id("c")),
                        },
                        "bbbbbbb".into(),
                    ),
                    orig_head: id("a"),
                },
                UserEnv,
                &rebase_env,
            ),
            (GitOp::Continue(Operation::Rebase), UserEnv, none),
            (GitOp::Skip(Operation::CherryPick), UserEnv, none),
            (GitOp::Abort(Operation::Merge), UserEnv, none),
            (GitOp::CheckoutCommit(id("a")), Write, w),
            (GitOp::Revert(id("a")), UserEnv, none),
            (GitOp::CherryPick(vec![id("a")]), UserEnv, none),
            (
                GitOp::Reset {
                    to: id("a"),
                    mode: ResetMode::Mixed,
                },
                Write,
                w,
            ),
            (
                GitOp::CreateBranch {
                    name: "n".into(),
                    from: "main".into(),
                },
                Write,
                w,
            ),
            (
                GitOp::RenameBranch {
                    from: "a".into(),
                    to: "b".into(),
                },
                Write,
                w,
            ),
            (
                GitOp::DeleteBranch {
                    name: "a".into(),
                    force: true,
                },
                Write,
                w,
            ),
            (GitOp::DeleteBranches(vec!["a".into()]), Write, w),
            (
                GitOp::MergeTool("a".into()),
                UserEnv,
                &[("GIT_LITERAL_PATHSPECS", "1")],
            ),
            (push(StashMode::All, &[]), Write, w),
            (push(StashMode::Staged, &[]), Write, w),
            (push(StashMode::IncludeUntracked, &["a"]), Write, w),
            // Its `:/` clean must not be read literally.
            (
                push(StashMode::IncludeUntracked, &[]),
                Locale,
                &[("LC_ALL", "C")],
            ),
            (GitOp::StashApply(stash.clone()), Write, w),
            (GitOp::StashPop(stash.clone()), Write, w),
            (GitOp::StashDrop(stash.clone()), Write, w),
            (GitOp::StashDrops(vec![stash.clone()]), Write, w),
            (
                GitOp::StashRename {
                    stash: stash.clone(),
                    message: "m".into(),
                },
                Write,
                w,
            ),
            (
                GitOp::CheckoutTracking {
                    name: "main".into(),
                    from: upstream.clone(),
                },
                Write,
                w,
            ),
            (
                GitOp::SetUpstream {
                    branch: "main".into(),
                    upstream,
                },
                Write,
                w,
            ),
            (GitOp::UnsetUpstream("main".into()), Write, w),
            (
                GitOp::AddRemote {
                    name: "o".into(),
                    url: "https://h/r".into(),
                },
                Write,
                w,
            ),
            (
                GitOp::RenameRemote {
                    from: "a".into(),
                    to: "b".into(),
                },
                Write,
                w,
            ),
            (GitOp::RemoveRemote("a".into()), Write, w),
            (GitOp::RemoveRemotes(vec!["a".into()]), Write, w),
            (travel(Step::MoveTo(id("a"))), Write, &undo_env),
            (
                travel(Step::CheckoutBranch("main".into())),
                Write,
                &undo_env,
            ),
            (travel(Step::CheckoutDetached(id("a"))), Write, &undo_env),
            (
                GitOp::CreateTag {
                    name: "v1".into(),
                    target: id("a"),
                    message: Some("m".into()),
                },
                Write,
                w,
            ),
            (
                GitOp::DeleteTag {
                    name: "v1".into(),
                    oid: OID.into(),
                },
                Write,
                w,
            ),
            (GitOp::DeleteTags(vec![("v1".into(), OID.into())]), Write, w),
            (
                GitOp::BisectMark {
                    mark: BisectMark::Good,
                    commit: id("a"),
                    start: true,
                },
                UserEnv,
                none,
            ),
            (GitOp::BisectReset, UserEnv, none),
            (
                GitOp::CreateBranchAt {
                    name: "n".into(),
                    at: id("a"),
                },
                Write,
                w,
            ),
        ];

        let listed: BTreeSet<usize> = cases.iter().map(|(op, ..)| variant(op)).collect();
        assert_eq!(listed, (0..=49).collect(), "a variant is missing");
        for (op, profile, expected) in &cases {
            let cmd = op_command(op);
            assert_eq!(cmd.env_profile(), Some(*profile), "{op:?}");
            assert_eq!(cmd.process_env(), env(expected), "{op:?}");
        }
    }

    /// A URL in git's message loses its credentials, whatever the op.
    #[test]
    fn a_failed_submodule_update_hides_the_token_git_quoted() {
        let op = GitOp::SubmoduleUpdate("sub".into());
        let runner = FakeRunner::new().expect(SSH_CONFIG, fail("")).expect(
            &["submodule", "update", "--init", "--", "sub"],
            fail("fatal: clone of 'https://u:tok@h/x' failed\n"),
        );

        let error = run_op_with(&runner, &op, &|_| false).unwrap_err();

        assert!(!error.contains("tok"), "{error}");
        assert!(error.contains("https://h/x"), "{error}");
    }

    #[test]
    fn a_failed_merge_hides_the_token_on_stdout_and_still_names_the_conflict() {
        let op = GitOp::Merge {
            branch: "feature".into(),
            strategy: MergeStrategy::NoFastForward,
        };
        let output = GitOutput {
            success: false,
            stdout: b"From https://u:tok@h/x\nCONFLICT (content): Merge conflict in a.txt\n"
                .to_vec(),
            stderr: b"Automatic merge failed\n".to_vec(),
        };
        let runner = FakeRunner::new().expect(&["merge", "--no-ff", "refs/heads/feature"], output);

        let error = run_op(&runner, &op).unwrap_err();

        assert!(!error.contains("tok"), "{error}");
        assert!(
            error.contains("CONFLICT (content): Merge conflict in a.txt"),
            "{error}"
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

    #[cfg(unix)]
    #[test]
    fn merge_and_rebase_put_the_raw_bytes_of_a_name_in_argv() {
        use std::os::unix::ffi::OsStrExt;
        let raw = kika_git::branches::BranchName::from_bytes(b"x\xff");
        let merge = op_command(&GitOp::Merge {
            branch: raw.clone(),
            strategy: MergeStrategy::Squash,
        });
        assert_eq!(merge.args()[2].as_bytes(), b"refs/heads/x\xff");
        let rebase = op_command(&GitOp::Rebase { onto: raw });
        assert_eq!(rebase.args()[1].as_bytes(), b"refs/heads/x\xff");
    }

    #[test]
    fn checking_out_a_branch_named_like_an_option_keeps_the_worktree_in_real_git() {
        let tmp = dirty_repo();
        let dir = tmp.path();
        let runner = kika_git::CliRunner::isolated(dir);
        // `git branch` refuses such names; plumbing does not.
        kika_git::fake::git(dir, &["update-ref", "refs/heads/-f", "HEAD"]);
        kika_git::fake::git(dir, &["update-ref", "refs/heads/--force", "HEAD"]);
        use kika_git::version::{GitVersion, parse_version};
        let version = parse_version(&kika_git::fake::git(dir, &["--version"]));
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
                let head = kika_git::fake::git(dir, &["symbolic-ref", "HEAD"]);
                assert_eq!(head, format!("refs/heads/{name}\n").as_bytes());
            }
        }
    }

    const OID: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn a_range_of_branches_is_deleted_by_one_unforced_branch_d() {
        let op = GitOp::DeleteBranches(vec!["a".into(), "-D".into(), "b\x1b".into()]);
        let expected = ["branch", "-d", "--", "a", "-D", "b\x1b"];
        assert_eq!(argv(&op_command(&op)), expected);
        let runner = FakeRunner::new().expect(&expected, ok(b""));
        assert_eq!(run_op(&runner, &op), Ok(()));
        runner.assert_done();
    }

    #[test]
    fn a_range_of_tags_or_remotes_runs_one_command_each_and_stops_at_the_first_failure() {
        let tags = GitOp::DeleteTags(vec![
            ("v1".into(), OID.into()),
            ("v2".into(), OID.into()),
            ("v3".into(), OID.into()),
        ]);
        let runner = FakeRunner::new()
            .expect(
                &["update-ref", "--no-deref", "-d", "refs/tags/v1", OID],
                ok(b""),
            )
            .expect(
                &["update-ref", "--no-deref", "-d", "refs/tags/v2", OID],
                fail("fatal: cannot lock ref"),
            );
        assert_eq!(run_op(&runner, &tags), Err("fatal: cannot lock ref".into()));
        runner.assert_done();

        let remotes = GitOp::RemoveRemotes(vec!["a".into(), "b".into(), "c".into()]);
        assert_eq!(argv(&op_command(&remotes)), ["remote", "remove", "a"]);
        // A call past the failure would find nothing expected and panic.
        let runner = FakeRunner::new()
            .expect(&["remote", "remove", "a"], ok(b""))
            .expect(&["remote", "remove", "b"], fail("error: no such remote"));
        assert_eq!(
            run_op(&runner, &remotes),
            Err("error: no such remote".into())
        );
        runner.assert_done();

        let runner = FakeRunner::new()
            .expect(&["remote", "remove", "a"], ok(b""))
            .expect(&["remote", "remove", "b"], ok(b""))
            .expect(&["remote", "remove", "c"], ok(b""));
        assert_eq!(run_op(&runner, &remotes), Ok(()));
        runner.assert_done();
    }

    #[test]
    fn an_empty_or_option_like_range_runs_no_git() {
        for op in [
            GitOp::DeleteBranches(vec![]),
            GitOp::DeleteTags(vec![]),
            GitOp::RemoveRemotes(vec![]),
            GitOp::RemoveRemotes(vec!["ok".into(), "-f".into()]),
        ] {
            let runner = FakeRunner::new();
            assert!(run_op(&runner, &op).is_err(), "{op:?}");
            runner.assert_done();
        }
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
            assert_eq!(runner.process_envs(), [env(&WRITE)], "{op:?}");
        }
    }

    #[test]
    fn submodule_ops_name_one_path_literally_and_update_never_prompts() {
        let cases = [
            (
                GitOp::SubmoduleInit("libs/a b".into()),
                &["submodule", "init", "--", "libs/a b"][..],
                env(&WRITE),
            ),
            (
                GitOp::SubmoduleUpdate("libs/a b".into()),
                &["submodule", "update", "--init", "--", "libs/a b"][..],
                env(&[
                    ("LC_ALL", "C"),
                    ("GIT_LITERAL_PATHSPECS", "1"),
                    ("GIT_TERMINAL_PROMPT", "0"),
                ]),
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
            let envs = runner.process_envs();
            let mut env = env;
            if update {
                env.push(("GIT_SSH_COMMAND".into(), BATCH_SSH.into()));
                // The config read takes no lock and names no path.
                assert_eq!(envs[0], self::env(&STANDARD_ENV), "{op:?}");
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
        use kika_git::fake::{TempDir, git};

        let tmp = TempDir::new("batch-ssh");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        let runner = kika_git::CliRunner::isolated(dir);
        assert!(ssh_is_default(&runner, &|_| false));
        git(dir, &["config", "core.sshCommand", "my-ssh"]);
        assert!(!ssh_is_default(&runner, &|_| false));
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
            assert_eq!(runner.process_envs(), [env(&WRITE)], "{op:?}");
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
        assert_eq!(runner.process_envs(), [env(&WRITE)]);
    }

    /// `d.txt` committed as `head`, `staged` in the index, `work` in the
    /// worktree.
    fn staged_and_changed_repo() -> kika_git::fake::TempDir {
        use kika_git::fake::{TempDir, git};

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
        use kika_git::fake::git;

        let tmp = staged_and_changed_repo();
        let dir = tmp.path();
        let runner = kika_git::CliRunner::isolated(dir);
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
        use kika_git::diff::{DiffRequest, DiffSide, load_patch};
        use kika_git::fake::{TempDir, git};
        use kika_git::patch::{Direction, LineId, PatchSelection, build};

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
        let runner = kika_git::CliRunner::isolated(dir);
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
        use kika_git::fake::{TempDir, git};

        let tmp = TempDir::new("clean");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        std::fs::write(dir.join("tracked.rs"), "keep\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "init"]);
        for name in ["*.rs", "a.rs", ":(glob)b.rs", "b.rs"] {
            std::fs::write(dir.join(name), "new\n").unwrap();
        }
        let runner = kika_git::CliRunner::isolated(dir);
        run_op(&runner, &GitOp::DeleteUntracked(paths(&["*.rs"]))).unwrap();
        run_op(&runner, &GitOp::DeleteUntracked(paths(&[":(glob)b.rs"]))).unwrap();
        assert!(!dir.join("*.rs").exists());
        assert!(!dir.join(":(glob)b.rs").exists());
        for name in ["a.rs", "b.rs", "tracked.rs"] {
            assert!(dir.join(name).exists(), "{name}");
        }
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

    #[test]
    fn discarding_files_restores_then_deletes_with_literal_paths() {
        let runner = FakeRunner::new()
            .expect(&["checkout", "-q", "--", "a.rs", "*.rs"], ok(b""))
            .expect(&["clean", "-f", "-q", "--", "n.txt"], ok(b""));
        let op = GitOp::DiscardFiles {
            restore: paths(&["a.rs", "*.rs"]),
            delete: paths(&["n.txt"]),
        };
        assert_eq!(run_op(&runner, &op), Ok(()));
        runner.assert_done();
        assert_eq!(runner.process_envs(), [env(&WRITE), env(&WRITE)]);
    }

    #[test]
    fn a_failed_restore_deletes_nothing() {
        let runner = FakeRunner::new().expect(&["checkout", "-q", "--", "a.rs"], fail("nope\n"));
        let op = GitOp::DiscardFiles {
            restore: paths(&["a.rs"]),
            delete: paths(&["n.txt"]),
        };
        assert_eq!(run_op(&runner, &op), Err("nope".into()));
        runner.assert_done();
    }

    /// Every message and URL Kika passes in argv today, with a secret in
    /// the parts that must never show.
    #[test]
    fn no_command_kika_builds_shows_a_secret() {
        use crate::op::{GitOp, op_command};
        use kika_git::command_line::display_line;
        use kika_git::commit_files::CommitId;

        let oid = "0123456789abcdef0123456789abcdef01234567";
        let ops = [
            GitOp::Commit {
                message: Some("subj\n\nSECRET body".into()),
                stage_all: true,
                amend: false,
                no_verify: false,
                fixup: None,
            },
            GitOp::CreateTag {
                name: "v1".into(),
                target: CommitId::parse(oid).unwrap(),
                message: Some("subj\r\nSECRET body".into()),
            },
            GitOp::AddRemote {
                name: "o".into(),
                url: "https://u:SECRET pw@h/r".into(),
            },
            GitOp::ApplyCached {
                patch: b"SECRET patch".to_vec(),
                reverse: false,
            },
        ];
        for op in &ops {
            let line = display_line(&op_command(op));
            assert!(!line.contains("SECRET"), "{op:?}: {line}");
        }
    }

    /// `run` and `spawn` both build their process in `CliRunner::command`,
    /// so every command kind runs in the runner's workdir with its argv
    /// untouched.
    #[test]
    fn every_command_kind_runs_in_the_workdir() {
        use crate::op::{GitOp, op_command};
        use kika_git::diff::{DiffRequest, DiffSide, diff_command, patch_diff_command};
        use kika_git::path::RepoPath;
        use kika_git::repo_state::git_dir_command;
        use kika_git::runner::CliRunner;
        use kika_git::status::status_command;
        use std::path::Path;

        let top = Path::new("/r/top");
        let runner = CliRunner::new(top);
        let request = |side| DiffRequest {
            path: RepoPath::from("a.txt"),
            orig_path: None,
            side,
            options: Default::default(),
            commit: None,
        };
        let tracked = request(DiffSide::Worktree);
        let staged = request(DiffSide::Index);
        let untracked = request(DiffSide::Untracked);
        let paths = vec![RepoPath::from("a.txt")];
        let ops = [
            GitOp::Stage(paths.clone()),
            GitOp::Unstage(paths),
            GitOp::StageAll,
            GitOp::UnstageAll,
            GitOp::ApplyCached {
                patch: b"patch".to_vec(),
                reverse: false,
            },
            GitOp::ApplyCached {
                patch: b"patch".to_vec(),
                reverse: true,
            },
            GitOp::Commit {
                message: Some("msg".into()),
                stage_all: false,
                amend: false,
                no_verify: false,
                fixup: None,
            },
            GitOp::ApplyEdited {
                patch: b"patch".to_vec(),
            },
            GitOp::SubmoduleInit(RepoPath::from("sub")),
            GitOp::SubmoduleUpdate(RepoPath::from("sub")),
        ];
        let commands = [
            status_command(),
            diff_command(&tracked),
            diff_command(&staged),
            diff_command(&untracked),
            patch_diff_command(&tracked),
            patch_diff_command(&staged),
            patch_diff_command(&untracked),
            git_dir_command(),
            kika_git::submodule::status_command(),
        ]
        .into_iter()
        .chain(ops.iter().map(op_command));

        for cmd in commands {
            let process = runner.test_process(&cmd).unwrap();
            assert_eq!(process.get_current_dir(), Some(top), "{cmd:?}");
            let argv: Vec<_> = process.get_args().collect();
            assert_eq!(argv, cmd.args(), "{cmd:?}");
        }
    }
}
