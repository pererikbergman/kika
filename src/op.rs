//! Mutations of the index and history. Each is a [`GitOp`] so the reducer can
//! name the work, and the refresh planner can say what it invalidates.

use std::ffi::OsString;
use std::path::Path;

use crate::features::branches::{remotes, tags};
use crate::features::commits::bisect::git::run_bisect_start;
use crate::features::commits::custom_patch::{CustomPatch, PatchAction};
use crate::git::bisect::BisectMark;
use crate::git::branches::BranchName;
use crate::git::commit_files::CommitId;
use crate::git::diff::terminal_safe;
use crate::git::path::RepoPath;
use crate::git::rebase_plan::{RebasePlan, encode};
use crate::git::remote::redact_userinfo;
use crate::git::remotes::{RemoteBranch, RemoteName};
use crate::git::repo_state::Operation;
use crate::git::runner::{GitCommand, GitOutput, GitRunner, run_checked};
use crate::git::stash::StashRef;
use crate::git::status::{EntryKind, FileEntry};
use crate::git::tags::{TagName, validate_tag_name};
use crate::git::undo::{Direction, Expected, Step};
use crate::git::version::{GitVersion, parse_version, version_command};
use crate::sequence_editor::PLAN_ENV;

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
    /// with parents first (see `crate::git::cherry_pick::ordered`); never with
    /// a merge commit, which needs a mainline.
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
    /// `git::undo`). Git writes `direction`'s marker into the reflog, which
    /// is how the next plan knows this step happened. It runs only while
    /// `HEAD` is still where `expect` says, as when the user was asked.
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

/// Why a rename is refused when another entry, next to it on top of the
/// list, holds the same commit: git adds no entry for the commit
/// `refs/stash` already names, so the rename could only lose one copy.
pub const STASH_COPY: &str =
    "Another stash holds the same commit as this one; drop the copy first, then rename";

/// Why a stash of a staged rename is refused: something sits at its old
/// path, which the push would leave alone and the restore overwrite.
pub const STASH_RENAME_TAKEN: &str = "A file now sits at the renamed file's old path";

/// Why a stash of a staged deletion is refused: something sits at its
/// path, which the push's cleanup would overwrite with `HEAD`'s version.
pub const STASH_DELETED_TAKEN: &str = "A file now sits at the deleted file's path";

/// What a selected-file stash takes: the push's mode and pathspec, the
/// old sides of staged renames, restored after it, and the staged
/// deletions, reset to `HEAD` in the index before it. See
/// [`GitOp::stash_files_for`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashSet {
    pub mode: StashMode,
    pub paths: Vec<RepoPath>,
    pub restore: Vec<RepoPath>,
    pub unstage: Vec<RepoPath>,
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

    /// What one key press does to a range of entries: stage them all while
    /// any has unstaged changes, else unstage them all, with each staged
    /// rename's old path. Conflicts are skipped, as one at a time.
    pub fn toggle_many(entries: &[&FileEntry]) -> Option<GitOp> {
        let entries: Vec<&FileEntry> = entries
            .iter()
            .copied()
            .filter(|e| e.kind != EntryKind::Conflicted)
            .collect();
        if entries.is_empty() {
            return None;
        }
        if entries.iter().any(|e| has_unstaged(e)) {
            return Some(GitOp::Stage(
                entries.iter().map(|e| e.path.clone()).collect(),
            ));
        }
        let paths = entries
            .iter()
            .flat_map(|e| e.orig_path.iter().chain([&e.path]))
            .cloned()
            .collect();
        Some(GitOp::Unstage(paths))
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

    /// What stashing only `entry` puts aside. A staged rename goes as one
    /// change: its new path in the pathspec and its old one restored after
    /// the push, since git no longer knows the old path in the index and
    /// refuses it as a pathspec. A staged deletion is named and also
    /// unstaged before the push, for the same reason. Any other entry names
    /// its paths, a copy's source too. An untracked file needs `-u`, since
    /// git does not match untracked files by pathspec without it. Conflicts
    /// cannot be stashed.
    pub fn stash_file_for(entry: &FileEntry) -> Option<StashSet> {
        let mode = match entry.kind {
            EntryKind::Conflicted => return None,
            EntryKind::Untracked => StashMode::IncludeUntracked,
            _ => StashMode::All,
        };
        let path = entry.path.clone();
        match (&entry.orig_path, entry.index) {
            (Some(old), 'R') => Some(StashSet {
                mode,
                paths: vec![path],
                restore: vec![old.clone()],
                unstage: vec![],
            }),
            (None, 'D') => Some(StashSet {
                mode,
                paths: vec![path.clone()],
                restore: vec![],
                unstage: vec![path],
            }),
            (orig, _) => Some(StashSet {
                mode,
                paths: orig.iter().cloned().chain([path]).collect(),
                restore: vec![],
                unstage: vec![],
            }),
        }
    }

    /// What stashing the marked `entries` puts aside: every path each one
    /// takes, once, in the order first seen, with `-u` if any of them is
    /// untracked. Nothing marked, or any conflict among them, stashes
    /// nothing, so a push never runs on a set the user did not mean. Nor
    /// does a rename whose old path is also marked: the restore would
    /// overwrite what the push took. Nor does a staged deletion beside an
    /// untracked file: with `-u`, git saves the stash and then fails to
    /// clean up a pathspec that is in neither the index nor the worktree,
    /// leaving the files in place as well as in the stash. A rename's old
    /// path is never in the pathspec, so a rename can go with `-u`.
    pub fn stash_files_for(entries: &[&FileEntry]) -> Option<StashSet> {
        if entries.is_empty() {
            return None;
        }
        let mut set = StashSet {
            mode: StashMode::All,
            paths: Vec::new(),
            restore: Vec::new(),
            unstage: Vec::new(),
        };
        let add = |list: &mut Vec<RepoPath>, more: Vec<RepoPath>| {
            for path in more {
                if !list.contains(&path) {
                    list.push(path);
                }
            }
        };
        for entry in entries {
            let one = Self::stash_file_for(entry)?;
            if one.mode == StashMode::IncludeUntracked {
                set.mode = StashMode::IncludeUntracked;
            }
            add(&mut set.paths, one.paths);
            add(&mut set.restore, one.restore);
            add(&mut set.unstage, one.unstage);
        }
        if set.restore.iter().any(|old| set.paths.contains(old)) {
            return None;
        }
        let deleted = entries.iter().any(|e| e.index == 'D');
        if deleted && set.mode == StashMode::IncludeUntracked {
            return None;
        }
        Some(set)
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
        // The start point as a full ref, so a tag of the same name, or a
        // name like an option, cannot stand in for it.
        GitOp::CreateBranch { name, from } => GitCommand::new("checkout")
            .arg("-q")
            .arg("-b")
            .arg(name)
            .arg(from.full_ref())
            .arg("--"),
        // `--keep` makes git itself refuse to move when a local change, or
        // an untracked file, is in the way. The marker in
        // `GIT_REFLOG_ACTION` is all `checkout` logs, and `reset` logs it as
        // `<marker>: updating HEAD`, as far back as git 2.15.
        GitOp::Travel {
            direction, step, ..
        } => match step {
            Step::MoveTo(oid) => GitCommand::new("reset")
                .arg("-q")
                .arg("--keep")
                .arg(oid.as_str())
                .arg("--"),
            Step::CheckoutBranch(name) => checkout_command(name),
            Step::CheckoutDetached(oid) => detached_checkout(oid),
        }
        .env("GIT_REFLOG_ACTION", direction.marker()),
        // The id is hex digits only, so it cannot be read as an option;
        // `--` keeps it from being read as a path.
        GitOp::CheckoutCommit(oid) => detached_checkout(oid),
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
        // A rename's old path is in `restore`, not here; a staged deletion
        // is here and in `unstage`: see `GitOp::StashPush`.
        GitOp::StashPush {
            mode,
            message,
            paths,
            ..
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
        GitOp::DeleteBranches(names) => names.iter().fold(
            GitCommand::new("branch").arg("-d").arg("--"),
            |cmd, name| cmd.arg(name.as_os_string()),
        ),
        GitOp::CheckoutTracking { name, from } => remotes::git::track_command(name, from),
        GitOp::SetUpstream { branch, upstream } => {
            let mut flag = OsString::from("--set-upstream-to=");
            flag.push(upstream.full_ref());
            GitCommand::new("branch")
                .arg(flag)
                .arg("--")
                .arg(branch.as_os_string())
        }
        GitOp::UnsetUpstream(branch) => GitCommand::new("branch")
            .arg("--unset-upstream")
            .arg("--")
            .arg(branch.as_os_string()),
        GitOp::AddRemote { name, url } => remotes::git::add_command(name, url),
        GitOp::RenameRemote { from, to } => remotes::git::rename_command(from, to),
        GitOp::RemoveRemote(name) => remotes::git::remove_command(name),
        // The first of the steps `run_op` takes, one per remote, tag or stash;
        // `refusal` keeps an empty list from running.
        GitOp::RemoveRemotes(_) | GitOp::DeleteTags(_) | GitOp::StashDrops(_) => {
            return op_steps(op)
                .first()
                .map_or_else(|| GitCommand::new("remote"), op_command);
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
            return GitCommand::new("merge").arg(flag).arg(branch.full_ref());
        }
        GitOp::Rebase { onto } => return GitCommand::new("rebase").arg(onto.full_ref()),
        GitOp::Revert(oid) => return GitCommand::new("revert").arg(oid.as_str()),
        // Ids are hex digits only, so none is read as an option.
        GitOp::CherryPick(picks) => {
            return picks
                .iter()
                .fold(GitCommand::new("cherry-pick"), |cmd, pick| {
                    cmd.arg(pick.as_str())
                });
        }
        // The plan goes to the sequence editor, which `handoff` names; the
        // base is hex digits only. The keys are newer config that an older
        // git ignores; they keep the todo list to what the sequence editor
        // accepts. `--update-refs` (git 2.38) or the key turning it off
        // always wins over the user's `rebase.updateRefs`. Comments in the
        // todo use `#`, which the sequence editor reads them by, whatever
        // the user's `core.commentChar` or `core.commentString`; the
        // command line's `-c` wins over every config file. A stopped
        // rebase's todo is read with the user's own prefix, as git will.
        GitOp::InteractiveRebase(plan) => {
            let mut cmd = GitCommand::new("-c");
            if !plan.update_refs {
                cmd = cmd.arg("rebase.updateRefs=false").arg("-c");
            }
            cmd = cmd
                .arg("rebase.rebaseMerges=false")
                .arg("-c")
                .arg("core.commentChar=#")
                .arg("rebase")
                .arg("-i");
            if plan.update_refs {
                cmd = cmd.arg("--update-refs");
            }
            return cmd
                .arg("--autostash")
                .arg("--no-autosquash")
                .arg("--keep-empty")
                .arg(plan.base.as_str())
                .env(PLAN_ENV, encode(&plan.steps));
        }
        // Its first step; the rest run from
        // `crate::features::commits::custom_patch::git::run`.
        GitOp::CustomPatch { plan, .. } => {
            return op_command(&GitOp::InteractiveRebase(plan.clone()));
        }
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
        // A bisect checks commits out, so hooks run and see the user's env,
        // as for a merge. Only fixed words and hex digits reach argv;
        // nothing read from the bisect's own files does. Every subcommand
        // here predates git 2.15.
        GitOp::BisectMark { mark, commit, .. } => {
            return GitCommand::new("bisect")
                .arg(mark.word())
                .arg(commit.as_str());
        }
        GitOp::BisectReset => return GitCommand::new("bisect").arg("reset"),
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
    cmd.env("GIT_LITERAL_PATHSPECS", "1").env("LC_ALL", "C")
}

/// `cmd`, then `--` and each name from its raw bytes, read literally.
fn with_literal_paths(cmd: GitCommand, paths: &[RepoPath]) -> GitCommand {
    paths
        .iter()
        .fold(cmd.arg("--"), |cmd, p| cmd.arg(p.as_os_str()))
        .env("GIT_LITERAL_PATHSPECS", "1")
        .env("LC_ALL", "C")
}

/// Put the old sides of stashed renames back as they are at `head`, the
/// commit the push was made on. The id is hex digits only, so it cannot be
/// read as an option.
fn restore_command(head: &CommitId, restore: &[RepoPath]) -> GitCommand {
    let cmd = GitCommand::new("checkout").arg("-q").arg(head.as_str());
    with_literal_paths(cmd, restore)
}

/// Anything git knows at the old paths, in the index or the worktree,
/// ignored files too: the restore would overwrite it.
fn restore_taken_command(restore: &[RepoPath]) -> GitCommand {
    let cmd = GitCommand::new("ls-files")
        .arg("-z")
        .arg("--cached")
        .arg("--others");
    with_literal_paths(cmd, restore)
}

/// The directories `path` sits in, outermost first: `a` and `a/b` for
/// `a/b/c`. A restore must make each of them, so a file or symlink at any
/// of them would be unlinked.
pub(crate) fn leading_dirs(path: &RepoPath) -> Vec<RepoPath> {
    let bytes = path.as_bytes();
    (bytes.iter().enumerate())
        .filter(|&(i, &b)| b == b'/' && i > 0)
        .map(|(i, _)| RepoPath::from_bytes(&bytes[..i]))
        .collect()
}

/// Whether anything sits where a restore of `restore` would write: at an
/// old path or under it, or exactly at one of its leading directories. A
/// file under a leading directory is fine; that directory is real.
fn restore_taken(runner: &dyn GitRunner, restore: &[RepoPath]) -> Result<bool, String> {
    let at_old = run_checked(runner, &restore_taken_command(restore))?;
    if !at_old.stdout.is_empty() {
        return Ok(true);
    }
    let mut dirs: Vec<RepoPath> = Vec::new();
    for dir in restore.iter().flat_map(leading_dirs) {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    if dirs.is_empty() {
        return Ok(false);
    }
    let found = run_checked(runner, &restore_taken_command(&dirs))?;
    Ok((found.stdout.split(|&b| b == 0)).any(|name| dirs.iter().any(|d| d.as_bytes() == name)))
}

/// Whether the worktree at `top` holds anything a restore of `restore`
/// would overwrite or unlink: anything at an old path, or a leading
/// directory that is not a real directory (a file or a symlink). The
/// filesystem answers, so a name that differs only in case or Unicode form
/// counts where the filesystem folds them, as git's byte-wise checks
/// cannot. Anything unreadable counts as taken.
fn restore_blocked_on_disk(top: &Path, restore: &[RepoPath]) -> bool {
    let found = |path: &RepoPath| match std::fs::symlink_metadata(top.join(path.as_os_str())) {
        Ok(meta) => Some(Ok(meta.is_dir())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => Some(Err(e)),
    };
    restore.iter().any(|old| {
        let bad_dir = leading_dirs(old)
            .iter()
            .any(|dir| matches!(found(dir), Some(Ok(false) | Err(_))));
        bad_dir || found(old).is_some()
    })
}

/// `HEAD` as a commit id, for a restore that must use the tree the push
/// was made on.
fn head_commit(runner: &dyn GitRunner) -> Result<CommitId, String> {
    let cmd = GitCommand::new("rev-parse")
        .arg("--verify")
        .arg("-q")
        .arg("HEAD^{commit}")
        .env("LC_ALL", "C");
    let output = run_checked(runner, &cmd)?;
    CommitId::parse(String::from_utf8_lossy(&output.stdout).trim())
        .ok_or_else(|| "Could not read HEAD".to_string())
}

/// Put staged deletions back in the index as they are at `head`, so git
/// knows their paths as a pathspec again; the worktree keeps them deleted.
/// The id is hex digits only, so it cannot be read as an option.
fn unstage_command(head: &CommitId, unstage: &[RepoPath]) -> GitCommand {
    let cmd = GitCommand::new("reset").arg("-q").arg(head.as_str());
    with_literal_paths(cmd, unstage)
}

/// Stage the deletions again after a push that failed, as they were.
fn restage_command(unstage: &[RepoPath]) -> GitCommand {
    let cmd = GitCommand::new("rm").arg("--cached").arg("-q");
    with_literal_paths(cmd, unstage)
}

/// Stash a set with staged renames or deletions in it. Renames: the push
/// without their old paths, then those checked out from `HEAD`. Deletions:
/// reset to `HEAD` in the index first, so the push can name them, and
/// staged again if the push fails. Either way the worktree ends clean for
/// every path. Fails closed: nothing changes when an old or deleted path,
/// or a directory it needs, is taken, since git would write `HEAD`'s
/// version there.
fn push_with_fixups(
    runner: &dyn GitRunner,
    op: &GitOp,
    restore: &[RepoPath],
    unstage: &[RepoPath],
) -> Result<(), String> {
    let head = head_commit(runner)?;
    if !restore.is_empty() && restore_taken(runner, restore)? {
        return Err(STASH_RENAME_TAKEN.into());
    }
    if !unstage.is_empty() && restore_taken(runner, unstage)? {
        return Err(STASH_DELETED_TAKEN.into());
    }
    let top = crate::git::repo_root::find_toplevel(runner)
        .ok_or_else(|| "Could not find the worktree's top level".to_string())?;
    if restore_blocked_on_disk(&top, restore) {
        return Err(STASH_RENAME_TAKEN.into());
    }
    if restore_blocked_on_disk(&top, unstage) {
        return Err(STASH_DELETED_TAKEN.into());
    }
    if !unstage.is_empty() {
        run_checked(runner, &unstage_command(&head, unstage))?;
    }
    if let Err(why) = run_checked(runner, &op_command(op)) {
        if unstage.is_empty() {
            return Err(why);
        }
        return match run_checked(runner, &restage_command(unstage)) {
            Ok(_) => Err(why),
            Err(also) => {
                let names = listed(unstage);
                Err(format!(
                    "{why}\n{also}\n{names} is now an unstaged deletion; \
                     stage it again with git rm --cached -- {names}."
                ))
            }
        };
    }
    if restore.is_empty() {
        return Ok(());
    }
    run_checked(runner, &restore_command(&head, restore))
        .map(|_| ())
        .map_err(|why| {
            format!(
                "{why}\nThe stash was saved, but {} is still staged as deleted; \
                 check it out from HEAD to restore it.",
                listed(restore)
            )
        })
}

/// `paths` for a message, comma-separated.
fn listed(paths: &[RepoPath]) -> String {
    let names: Vec<&str> = paths.iter().map(RepoPath::display).collect();
    names.join(", ")
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

/// Switch to the local branch `name`. A short name, since a full ref
/// would detach `HEAD`; `--` keeps it from being read as a path. A ref
/// made outside git's porcelain can be named like an option, such as
/// `-f`, which would force the checkout and lose the worktree's changes;
/// `--end-of-options` keeps it a name. Only then: it needs git 2.24, newer
/// than Kika requires, and an older git fails on it without touching
/// anything.
fn checkout_command(name: &BranchName) -> GitCommand {
    let cmd = GitCommand::new("checkout").arg("-q");
    let cmd = if name.0.starts_with(b"-") {
        cmd.arg("--end-of-options")
    } else {
        cmd
    };
    cmd.arg(name.as_os_string()).arg("--")
}

/// Check out `oid` with no branch. The id is hex digits only, so it cannot
/// be read as an option; `--` keeps it from being read as a path.
fn detached_checkout(oid: &CommitId) -> GitCommand {
    GitCommand::new("checkout")
        .arg("-q")
        .arg("--detach")
        .arg(oid.as_str())
        .arg("--")
}

/// Where `git commit` takes its message from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageSource<'a> {
    /// This file, `-` being stdin.
    File(&'a Path),
    /// `HEAD`'s own, when amending.
    Keep,
    /// `fixup! ` and the raw subject of this commit, written by git. The
    /// id is hex digits only, so it cannot be read as an option.
    Fixup(&'a CommitId),
}

/// `git commit` taking its message from `source`. Shared by the detached
/// run and the one on the terminal, whose stdin is the user's, so the two
/// differ only there.
pub fn commit_command(amend: bool, no_verify: bool, source: MessageSource) -> GitCommand {
    let cmd = GitCommand::new("commit");
    let cmd = if amend { cmd.arg("--amend") } else { cmd };
    let cmd = if no_verify {
        cmd.arg("--no-verify")
    } else {
        cmd
    };
    match source {
        MessageSource::File(file) => cmd.arg("-F").arg(file),
        MessageSource::Keep => cmd.arg("--no-edit"),
        MessageSource::Fixup(target) => cmd.arg(format!("--fixup={}", target.as_str())),
    }
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

/// Run `op`. Failures carry git's message, made safe to draw since it can
/// quote repo-controlled paths. A commit's, a merge's, a revert's or a
/// stash apply's failure also carries stdout: hooks print to stderr, but
/// git says "nothing to commit" and names each conflict on stdout. A
/// bisect's does too: git says there why a mark failed, such as "merge base
/// is bad", or which commits are left when only skipped ones remain.
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
        | GitOp::StashRename { stash, .. } => verify_stash(runner, stash)?,
        // Fail closed: one entry that moved means the list changed, so
        // nothing is dropped. Each step checks its own entry again.
        GitOp::StashDrops(stashes) => {
            for stash in stashes {
                verify_stash(runner, stash)?;
            }
        }
        GitOp::StashPush {
            mode: StashMode::Staged,
            ..
        } => staged_stash_supported(runner)?,
        GitOp::StashPush {
            restore, unstage, ..
        } if !restore.is_empty() || !unstage.is_empty() => {
            return push_with_fixups(runner, op, restore, unstage);
        }
        _ => {}
    }
    if let GitOp::StashRename { stash, message } = op {
        return rename_stash(runner, stash, message);
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
        let now = crate::git::reflog::read_head(runner);
        if now.oid.as_ref() != Some(&expect.oid) || now.head_ref != expect.head_ref {
            return Err(HEAD_MOVED.into());
        }
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
        | GitOp::StashPop(_)
        | GitOp::CherryPick(_)
        | GitOp::BisectMark { .. }
        | GitOp::BisectReset => stderr_and_stdout(&output),
        // Git quotes a remote's URL in these errors, and it can carry a
        // token.
        GitOp::CheckoutTracking { .. }
        | GitOp::SetUpstream { .. }
        | GitOp::UnsetUpstream(_)
        | GitOp::AddRemote { .. }
        | GitOp::RenameRemote { .. }
        | GitOp::RemoveRemote(_)
        | GitOp::RemoveRemotes(_) => redact_userinfo(&terminal_safe(&output.stderr)),
        _ => terminal_safe(&output.stderr),
    };
    if message.is_empty() {
        let subcommand = cmd.subcommand().to_string_lossy();
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
/// Git adds no entry when the commit stored is the one `refs/stash`
/// already names, so every path makes sure the store will add one:
/// - `stash@{0}` is dropped first, then stored. If `stash@{1}` is the same
///   commit, the drop would leave it on top and the store add nothing, so
///   that is refused.
/// - A deeper entry whose commit is also at `stash@{0}` is refused for the
///   same reason.
///
/// After a store, `stash@{0}` must be the commit, or nothing more is
/// dropped and the error says so.
fn rename_stash(runner: &dyn GitRunner, stash: &StashRef, message: &str) -> Result<(), String> {
    let store = store_command(stash, message);
    let at = |index| StashRef {
        index,
        oid: stash.oid.clone(),
    };
    let drop = |index| op_command(&GitOp::StashDrop(at(index)));
    let copy_beside = if stash.index == 0 { 1 } else { 0 };
    if stash_is(runner, &at(copy_beside))? {
        return Err(STASH_COPY.into());
    }
    // `run_op` has checked that `stash@{index}` is still this commit.
    if stash.index == 0 {
        run_checked(runner, &drop(0))?;
        let lost = |why: String| {
            format!(
                "{why}\nThe stash was dropped but not stored again; \
                 git stash store {} brings it back.",
                stash.oid.as_str()
            )
        };
        run_checked(runner, &store).map_err(lost)?;
        return verify_stash(runner, &at(0)).map_err(lost);
    }
    run_checked(runner, &store)?;
    verify_stash(runner, &at(0))?;
    verify_stash(runner, &at(stash.index + 1))?;
    run_checked(runner, &drop(stash.index + 1)).map(|_| ())
}

/// Both of git's streams, made safe to draw, for commands that report
/// their outcome on stdout.
pub(crate) fn stderr_and_stdout(output: &GitOutput) -> String {
    [&output.stderr, &output.stdout]
        .map(|bytes| terminal_safe(bytes))
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
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
            if picks.is_empty() || !picks.iter().all(crate::git::cherry_pick::is_full_id) =>
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
    use crate::git::fake::{FakeRunner, config, fail, ok};
    use crate::git::runner::GitOutput;
    use crate::test_support::{argv, cloned_repo, rev_parse};

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
    fn an_interactive_rebase_passes_only_the_plan_in_its_env() {
        let plan = crate::git::rebase_plan::sample();
        let cmd = op_command(&GitOp::InteractiveRebase(plan.clone()));
        assert_eq!(
            argv(&cmd),
            [
                "-c",
                "rebase.updateRefs=false",
                "-c",
                "rebase.rebaseMerges=false",
                "-c",
                "core.commentChar=#",
                "rebase",
                "-i",
                "--autostash",
                "--no-autosquash",
                "--keep-empty",
                plan.base.as_str(),
            ]
        );
        let envs: Vec<_> = cmd
            .envs()
            .iter()
            .map(|(k, v)| (k.to_str().unwrap(), v.to_str().unwrap()))
            .collect();
        let encoded = format!("drop {}\npick {}\n", "b".repeat(40), "a".repeat(40));
        assert_eq!(envs, [("KIKA_REBASE_PLAN", encoded.as_str())]);
        assert_eq!(cmd.input(), None);
        assert_eq!(cmd.subcommand(), "rebase");
    }

    #[test]
    fn with_update_refs_the_rebase_passes_the_flag_and_not_the_key() {
        let mut plan = crate::git::rebase_plan::sample();
        plan.update_refs = true;
        let cmd = op_command(&GitOp::InteractiveRebase(plan.clone()));
        assert_eq!(
            argv(&cmd),
            [
                "-c",
                "rebase.rebaseMerges=false",
                "-c",
                "core.commentChar=#",
                "rebase",
                "-i",
                "--update-refs",
                "--autostash",
                "--no-autosquash",
                "--keep-empty",
                plan.base.as_str(),
            ]
        );
        assert_eq!(cmd.subcommand(), "rebase");
    }

    #[test]
    fn cherry_pick_names_each_commit_in_order_and_sets_no_env() {
        let (one, two) = ("1".repeat(40), "2".repeat(40));
        let cmd = op_command(&GitOp::CherryPick(vec![oid(&one), oid(&two)]));
        assert_eq!(argv(&cmd), ["cherry-pick", one.as_str(), two.as_str()]);
        assert!(cmd.envs().is_empty(), "{:?}", cmd.envs());
        assert_eq!(cmd.input(), None);
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

    fn origin_main() -> RemoteBranch {
        RemoteBranch {
            remote: "origin".into(),
            short: b"main".to_vec(),
            refname: b"refs/remotes/origin/main".to_vec(),
        }
    }

    #[test]
    fn remote_and_upstream_ops_build_exact_argv_and_never_fetch() {
        let cases: [(GitOp, &[&str]); 7] = [
            (
                GitOp::CheckoutTracking {
                    name: "main".into(),
                    from: origin_main(),
                },
                &[
                    "checkout",
                    "-q",
                    "--track",
                    "-b",
                    "main",
                    "refs/remotes/origin/main",
                    "--",
                ],
            ),
            (
                GitOp::SetUpstream {
                    branch: "topic".into(),
                    upstream: origin_main(),
                },
                &[
                    "branch",
                    "--set-upstream-to=refs/remotes/origin/main",
                    "--",
                    "topic",
                ],
            ),
            (
                GitOp::UnsetUpstream("topic".into()),
                &["branch", "--unset-upstream", "--", "topic"],
            ),
            (
                GitOp::AddRemote {
                    name: "fork".into(),
                    url: "https://h/fork.git".into(),
                },
                &["remote", "add", "--", "fork", "https://h/fork.git"],
            ),
            // No `--`: git 2.15's `remote rename` and `remote remove` parse
            // no options and would take it for a name.
            (
                GitOp::RenameRemote {
                    from: "fork".into(),
                    to: "mine".into(),
                },
                &["remote", "rename", "fork", "mine"],
            ),
            (
                GitOp::RemoveRemote("fork".into()),
                &["remote", "remove", "fork"],
            ),
            (
                GitOp::RemoveRemote("my.fork/x".into()),
                &["remote", "remove", "my.fork/x"],
            ),
        ];
        for (op, expected) in cases {
            let cmd = op_command(&op);
            let got = argv(&cmd);
            assert_eq!(got, expected, "{op:?}");
            for banned in ["-f", "--fetch", "fetch", "--end-of-options"] {
                assert!(!got.iter().any(|a| a == banned), "{op:?}: {banned}");
            }
            let runner = FakeRunner::new().expect(expected, ok(b""));
            assert_eq!(run_op(&runner, &op), Ok(()), "{op:?}");
            runner.assert_done();
        }
    }

    #[test]
    fn a_tracking_checkout_then_set_and_unset_upstream_in_real_git() {
        let (tmp, _bare) = cloned_repo();
        let dir = tmp.path();
        let runner = crate::git::CliRunner::isolated(dir);
        let feature = RemoteBranch {
            remote: "origin".into(),
            short: b"feature".to_vec(),
            refname: b"refs/remotes/origin/feature".to_vec(),
        };

        let checkout = GitOp::CheckoutTracking {
            name: "feature".into(),
            from: feature.clone(),
        };
        run_op(&runner, &checkout).unwrap();
        let head = crate::git::fake::git(dir, &["symbolic-ref", "--short", "HEAD"]);
        assert_eq!(head, b"feature\n");
        assert_eq!(
            config(dir, "branch.feature.merge").as_deref(),
            Some("refs/heads/feature")
        );
        assert_eq!(
            config(dir, "branch.feature.remote").as_deref(),
            Some("origin")
        );

        assert_eq!(config(dir, "branch.main.merge"), None);
        let set = GitOp::SetUpstream {
            branch: "main".into(),
            upstream: feature,
        };
        run_op(&runner, &set).unwrap();
        assert_eq!(
            config(dir, "branch.main.merge").as_deref(),
            Some("refs/heads/feature")
        );
        run_op(&runner, &GitOp::UnsetUpstream("main".into())).unwrap();
        assert_eq!(config(dir, "branch.main.merge"), None);
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
            fixup: None,
        }
    }

    #[test]
    fn amend_and_skip_hooks_add_their_flags_before_the_message_source() {
        let op = |message: Option<&str>, amend, no_verify| GitOp::Commit {
            message: message.map(Into::into),
            stage_all: false,
            amend,
            no_verify,
            fixup: None,
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
            fixup: None,
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
            fixup: None,
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

    fn fixup_of(target: &str, no_verify: bool) -> GitOp {
        GitOp::Commit {
            message: None,
            stage_all: false,
            amend: false,
            no_verify,
            fixup: Some(CommitId::parse(target).unwrap()),
        }
    }

    #[test]
    fn a_fixup_commit_names_its_target_by_full_id_with_no_stdin() {
        let oid = "0123456789abcdef0123456789abcdef01234567";
        let fixup = format!("--fixup={oid}");
        for (no_verify, argv) in [
            (false, vec!["commit", fixup.as_str()]),
            (true, vec!["commit", "--no-verify", fixup.as_str()]),
        ] {
            let op = fixup_of(oid, no_verify);
            let cmd = op_command(&op);
            assert_eq!(
                cmd.args(),
                argv.iter().map(OsString::from).collect::<Vec<_>>()
            );
            assert_eq!(cmd.input(), None);
            assert_eq!(cmd.envs(), []);
            let runner = FakeRunner::new().expect(&argv, ok(b""));
            assert_eq!(run_op(&runner, &op), Ok(()));
            runner.assert_done();
            assert_eq!(runner.inputs(), [None]);
        }
    }

    #[test]
    fn a_fixup_commit_takes_its_target_s_raw_subject_in_real_git() {
        use crate::git::fake::git;

        let tmp = committed_and_staged_repo();
        let dir = tmp.path();
        // Bytes the log would make terminal-safe; git must copy them as is.
        let subject = "caf\u{e9} \x1b[1mbold\x1b[0m\ttab";
        git(dir, &["commit", "-q", "-m", subject]);
        let target = git(dir, &["rev-parse", "HEAD"]);
        let target = String::from_utf8(target).unwrap();
        std::fs::write(dir.join("b.txt"), "b\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "B"]);
        std::fs::write(dir.join("a.txt"), "fixed\n").unwrap();
        git(dir, &["add", "-A"]);
        let runner = crate::git::CliRunner::isolated(dir);

        run_op(&runner, &fixup_of(target.trim(), false)).unwrap();

        let made = git(dir, &["log", "-1", "--format=%s"]);
        assert_eq!(made, format!("fixup! {subject}\n").into_bytes());
        assert_eq!(git(dir, &["rev-list", "--count", "HEAD"]), b"4\n");
        assert_eq!(git(dir, &["diff", "--cached"]), b"");
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
        assert_eq!(runner.envs().len(), 2);
        for env in runner.envs() {
            let literal = (OsString::from("GIT_LITERAL_PATHSPECS"), OsString::from("1"));
            assert!(env.contains(&literal), "{env:?}");
        }
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

    #[test]
    fn a_range_toggles_and_discards_as_one_op() {
        let unstaged = entry("a.rs", '.', 'M', EntryKind::Changed);
        let staged = entry("s.rs", 'M', '.', EntryKind::Changed);
        let new = entry("n.txt", '?', '?', EntryKind::Untracked);
        let conflict = entry("c.rs", 'U', 'U', EntryKind::Conflicted);
        assert_eq!(
            GitOp::toggle_many(&[&unstaged, &conflict, &staged]),
            stage(&["a.rs", "s.rs"])
        );
        assert_eq!(
            GitOp::toggle_many(&[&staged, &conflict]),
            unstage(&["s.rs"])
        );
        assert_eq!(GitOp::toggle_many(&[&conflict]), None);
        assert_eq!(
            GitOp::discard_many(&[&unstaged, &staged, &new, &conflict]),
            Some(GitOp::DiscardFiles {
                restore: paths(&["a.rs"]),
                delete: paths(&["n.txt"]),
            })
        );
        assert_eq!(
            GitOp::discard_many(&[&unstaged]),
            Some(GitOp::DiscardWorktree(paths(&["a.rs"])))
        );
        assert_eq!(
            GitOp::discard_many(&[&new]),
            Some(GitOp::DeleteUntracked(paths(&["n.txt"])))
        );
        assert_eq!(GitOp::discard_many(&[&staged, &conflict]), None);
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

    /// A travel op expecting a detached `HEAD` at `abc`; only its argv
    /// is looked at.
    fn travel(direction: Direction, step: Step) -> GitOp {
        GitOp::Travel {
            direction,
            step,
            expect: Expected {
                head_ref: None,
                oid: oid("abc"),
            },
        }
    }

    /// A travel op expecting `HEAD` where it is now in `runner`'s repo.
    fn travel_here(runner: &dyn GitRunner, direction: Direction, step: Step) -> GitOp {
        let now = crate::git::reflog::read_head(runner);
        GitOp::Travel {
            direction,
            step,
            expect: Expected {
                head_ref: now.head_ref,
                oid: now.oid.unwrap(),
            },
        }
    }

    fn planned(plan: crate::git::undo::Plan) -> GitOp {
        GitOp::Travel {
            direction: plan.direction,
            step: plan.step,
            expect: plan.expect,
        }
    }

    #[test]
    fn travel_names_only_ids_and_loaded_branches_and_marks_the_reflog() {
        let cases: [(GitOp, &[&str], &str); 4] = [
            (
                travel(Direction::Undo, Step::MoveTo(oid("abc123"))),
                &["reset", "-q", "--keep", "abc123", "--"],
                "kika undo",
            ),
            (
                travel(Direction::Redo, Step::MoveTo(oid("abc123"))),
                &["reset", "-q", "--keep", "abc123", "--"],
                "kika redo",
            ),
            (
                travel(Direction::Undo, Step::CheckoutBranch("main".into())),
                &["checkout", "-q", "main", "--"],
                "kika undo",
            ),
            (
                travel(Direction::Redo, Step::CheckoutDetached(oid("abc123"))),
                &["checkout", "-q", "--detach", "abc123", "--"],
                "kika redo",
            ),
        ];
        for (op, expected, marker) in cases {
            let cmd = op_command(&op);
            assert_eq!(argv(&cmd), expected, "{op:?}");
            let envs: Vec<_> = cmd
                .envs()
                .iter()
                .map(|(k, v)| (k.to_str().unwrap(), v.to_str().unwrap()))
                .collect();
            assert_eq!(
                envs,
                [
                    ("GIT_REFLOG_ACTION", marker),
                    ("GIT_LITERAL_PATHSPECS", "1"),
                    ("LC_ALL", "C"),
                ],
                "{op:?}"
            );
        }
        let checkout = op_command(&GitOp::Checkout("main".into()));
        assert_eq!(argv(&checkout), ["checkout", "-q", "main", "--"]);
    }

    #[test]
    fn an_undo_leaves_a_marker_the_next_plan_reads_back_in_real_git() {
        use crate::git::reflog::{Action, read_reflog};
        use crate::git::undo::{Context, plan};

        let (tmp, first) = two_commit_repo();
        let dir = tmp.path();
        let second = rev_parse(dir, "HEAD");
        let runner = crate::git::CliRunner::isolated(dir);
        let ctx = Context {
            branch: Some("main"),
            branches: &[],
            dirty: false,
        };

        let undo = plan(Direction::Undo, &read_reflog(&runner).unwrap(), &ctx).unwrap();
        assert_eq!(undo.step, Step::MoveTo(first.clone()));
        run_op(&runner, &planned(undo)).unwrap();

        assert_eq!(rev_parse(dir, "HEAD"), first);
        let reflog = read_reflog(&runner).unwrap();
        assert_eq!(reflog.entries[0].action, Action::Undo);
        assert_eq!(reflog.entries[0].message, b"kika undo: updating HEAD");
        let redo = plan(Direction::Redo, &reflog, &ctx).unwrap();
        assert_eq!(redo.step, Step::MoveTo(second));
    }

    #[test]
    fn reset_keep_refuses_to_clobber_a_local_change_or_an_untracked_file_in_real_git() {
        use crate::git::fake::git;

        let (tmp, first) = two_commit_repo();
        let dir = tmp.path();
        let runner = crate::git::CliRunner::isolated(dir);
        std::fs::write(dir.join("a.txt"), "mine\n").unwrap();
        let undo = travel_here(&runner, Direction::Undo, Step::MoveTo(first.clone()));
        let refused = run_op(&runner, &undo).unwrap_err();
        assert_ne!(refused, HEAD_MOVED);
        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).unwrap(),
            "mine\n"
        );

        // The commit after `first` adds `b.txt`; an untracked one is in
        // the way of going back to it.
        git(dir, &["checkout", "-q", "--", "a.txt"]);
        std::fs::write(dir.join("b.txt"), "theirs\n").unwrap();
        git(dir, &["add", "b.txt"]);
        git(dir, &["commit", "-q", "-m", "third"]);
        let third = rev_parse(dir, "HEAD");
        let undo = travel_here(&runner, Direction::Undo, Step::MoveTo(first));
        run_op(&runner, &undo).unwrap();
        std::fs::write(dir.join("b.txt"), "untracked\n").unwrap();
        let redo = travel_here(&runner, Direction::Redo, Step::MoveTo(third));
        let refused = run_op(&runner, &redo).unwrap_err();
        assert_ne!(refused, HEAD_MOVED);
        assert_eq!(
            std::fs::read_to_string(dir.join("b.txt")).unwrap(),
            "untracked\n"
        );
    }

    #[test]
    fn a_travel_whose_head_moved_since_the_question_changes_nothing_in_real_git() {
        use crate::git::fake::git;

        let (tmp, first) = two_commit_repo();
        let dir = tmp.path();
        let runner = crate::git::CliRunner::isolated(dir);
        let undo = travel_here(&runner, Direction::Undo, Step::MoveTo(first.clone()));

        // Another commit lands while the question is open.
        std::fs::write(dir.join("a.txt"), "three\n").unwrap();
        git(dir, &["commit", "-q", "-am", "third"]);
        let third = rev_parse(dir, "HEAD");
        assert_eq!(run_op(&runner, &undo), Err(HEAD_MOVED.into()));
        assert_eq!(rev_parse(dir, "HEAD"), third);

        // Same commit, another branch: still not what was asked about.
        let undo = travel_here(&runner, Direction::Undo, Step::MoveTo(first));
        git(dir, &["checkout", "-q", "-b", "other"]);
        assert_eq!(run_op(&runner, &undo), Err(HEAD_MOVED.into()));
        assert_eq!(rev_parse(dir, "refs/heads/other"), third);
    }

    #[test]
    fn undoing_a_rebase_started_from_another_branch_restores_the_rebased_branch_in_real_git() {
        use crate::git::fake::git;
        use crate::git::reflog::read_reflog;
        use crate::git::undo::{Context, plan};

        let (tmp, base) = two_commit_repo();
        let dir = tmp.path();
        let main = rev_parse(dir, "HEAD");
        git(dir, &["checkout", "-q", "-b", "topic", base.as_str()]);
        std::fs::write(dir.join("t.txt"), "topic\n").unwrap();
        git(dir, &["add", "t.txt"]);
        git(dir, &["commit", "-q", "-m", "topic"]);
        let old_topic = rev_parse(dir, "HEAD");
        git(dir, &["checkout", "-q", "-b", "other", main.as_str()]);
        std::fs::write(dir.join("o.txt"), "other\n").unwrap();
        git(dir, &["add", "o.txt"]);
        git(dir, &["commit", "-q", "-m", "other"]);
        let other = rev_parse(dir, "HEAD");
        // Run from `other`: HEAD's reflog never held topic's old tip.
        git(dir, &["rebase", "-q", main.as_str(), "topic"]);
        let new_topic = rev_parse(dir, "HEAD");

        let runner = crate::git::CliRunner::isolated(dir);
        let reflog = read_reflog(&runner).unwrap();
        let branches = crate::git::branches::list_branches(&runner).unwrap();
        let ctx = Context {
            branch: Some("topic"),
            branches: &branches,
            dirty: false,
        };
        let undo = plan(Direction::Undo, &reflog, &ctx).unwrap();
        assert_eq!(undo.step, Step::MoveTo(old_topic.clone()));
        assert_ne!(undo.step, Step::MoveTo(other.clone()));
        run_op(&runner, &planned(undo)).unwrap();
        assert_eq!(rev_parse(dir, "refs/heads/topic"), old_topic);
        assert_eq!(rev_parse(dir, "refs/heads/other"), other);

        // Without the branch's own reflog there is no safe target.
        let redo = plan(Direction::Redo, &read_reflog(&runner).unwrap(), &ctx).unwrap();
        run_op(&runner, &planned(redo)).unwrap();
        assert_eq!(rev_parse(dir, "HEAD"), new_topic);
        std::fs::remove_file(dir.join(".git/logs/refs/heads/topic")).unwrap();
        assert_eq!(
            plan(Direction::Undo, &read_reflog(&runner).unwrap(), &ctx),
            Err(
                "Cannot undo rebase: the branch's own reflog does not say where it was before"
                    .into()
            )
        );
    }

    #[test]
    fn a_head_pointing_outside_refs_heads_plans_no_undo_in_real_git() {
        use crate::git::fake::git;
        use crate::git::reflog::read_reflog;
        use crate::git::undo::{Context, plan};

        let (tmp, _) = two_commit_repo();
        let dir = tmp.path();
        let head = rev_parse(dir, "HEAD");
        git(
            dir,
            &["update-ref", "refs/remotes/origin/main", head.as_str()],
        );
        git(dir, &["symbolic-ref", "HEAD", "refs/remotes/origin/main"]);
        std::fs::write(dir.join("a.txt"), "three\n").unwrap();
        git(dir, &["commit", "-q", "-am", "on a remote ref"]);
        let moved = rev_parse(dir, "refs/remotes/origin/main");

        let runner = crate::git::CliRunner::isolated(dir);
        let reflog = read_reflog(&runner).unwrap();
        let ctx = Context {
            branch: None,
            branches: &[],
            dirty: false,
        };
        assert_eq!(
            plan(Direction::Undo, &reflog, &ctx),
            Err("HEAD is on refs/remotes/origin/main, not a branch; nothing is undone".into())
        );
        assert_eq!(rev_parse(dir, "refs/remotes/origin/main"), moved);
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
            restore: vec![],
            unstage: vec![],
        }
    }

    fn set(mode: StashMode, paths: &[&str], restore: &[&str]) -> StashSet {
        unstaging(mode, paths, restore, &[])
    }

    /// A set with staged deletions, `unstage`, in it.
    fn unstaging(mode: StashMode, paths: &[&str], restore: &[&str], unstage: &[&str]) -> StashSet {
        let list = |ps: &[&str]| ps.iter().map(|&p| RepoPath::from(p)).collect();
        StashSet {
            mode,
            paths: list(paths),
            restore: list(restore),
            unstage: list(unstage),
        }
    }

    fn set_push(set: StashSet, message: Option<&str>) -> GitOp {
        GitOp::StashPush {
            mode: set.mode,
            message: message.map(Into::into),
            paths: set.paths,
            restore: set.restore,
            unstage: set.unstage,
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
            // The old side is restored after the push, never named to it.
            (&renamed, &["stash", "push", "-m", "m", "--", "new name.rs"]),
        ];
        for (entry, expected) in cases {
            let op = set_push(GitOp::stash_file_for(entry).unwrap(), Some("m"));
            let cmd = op_command(&op);
            assert_eq!(argv(&cmd), expected, "{op:?}");
            assert_eq!(
                literal_env(&cmd),
                [("GIT_LITERAL_PATHSPECS", "1"), ("LC_ALL", "C")]
            );
        }
        assert_eq!(GitOp::stash_file_for(&conflicted), None);
    }

    #[test]
    fn stashing_a_set_takes_every_path_once_and_u_if_any_is_untracked() {
        let changed = entry("a.rs", '.', 'M', EntryKind::Changed);
        let untracked = entry("new.rs", '?', '?', EntryKind::Untracked);
        let mut renamed = entry("new name.rs", 'R', '.', EntryKind::Renamed);
        renamed.orig_path = Some("old.rs".into());
        let mut copied = entry("copy.rs", 'C', '.', EntryKind::Renamed);
        copied.orig_path = Some("a.rs".into());

        // A staged rename goes as one change: the new side in the
        // pathspec, the old side restored after the push.
        assert_eq!(
            GitOp::stash_files_for(&[&renamed]),
            Some(set(StashMode::All, &["new name.rs"], &["old.rs"]))
        );
        assert_eq!(
            GitOp::stash_files_for(&[&changed, &renamed, &renamed]),
            Some(set(StashMode::All, &["a.rs", "new name.rs"], &["old.rs"]))
        );
        assert_eq!(
            GitOp::stash_files_for(&[&changed, &untracked]),
            Some(set(StashMode::IncludeUntracked, &["a.rs", "new.rs"], &[]))
        );
        // Its old path is never in the pathspec, so it can go with `-u`.
        assert_eq!(
            GitOp::stash_files_for(&[&renamed, &untracked]),
            Some(set(
                StashMode::IncludeUntracked,
                &["new name.rs", "new.rs"],
                &["old.rs"]
            ))
        );
        // A copy's source is still in the index, so it is named.
        assert_eq!(
            GitOp::stash_files_for(&[&copied]),
            Some(set(StashMode::All, &["a.rs", "copy.rs"], &[]))
        );
        // A rename whose old path is also marked is refused: the restore
        // would overwrite what the push took.
        let at_old = entry("old.rs", '?', '?', EntryKind::Untracked);
        assert_eq!(GitOp::stash_files_for(&[&renamed, &at_old]), None);
        assert_eq!(GitOp::stash_files_for(&[&at_old, &renamed]), None);
        // A staged deletion beside an untracked file is refused rather
        // than half done; an unstaged one is still on the index.
        let staged_deleted = entry("gone.rs", 'D', '.', EntryKind::Changed);
        let unstaged_deleted = entry("gone.rs", '.', 'D', EntryKind::Changed);
        assert_eq!(GitOp::stash_files_for(&[&untracked, &staged_deleted]), None);
        assert_eq!(
            GitOp::stash_files_for(&[&unstaged_deleted, &untracked]),
            Some(set(
                StashMode::IncludeUntracked,
                &["gone.rs", "new.rs"],
                &[]
            ))
        );
        // A staged deletion is named and unstaged before the push; an
        // unstaged one is still in the index, so it is only named.
        assert_eq!(
            GitOp::stash_files_for(&[&staged_deleted]),
            Some(unstaging(StashMode::All, &["gone.rs"], &[], &["gone.rs"]))
        );
        assert_eq!(
            GitOp::stash_files_for(&[&staged_deleted, &changed]),
            Some(unstaging(
                StashMode::All,
                &["gone.rs", "a.rs"],
                &[],
                &["gone.rs"]
            ))
        );
        assert_eq!(
            GitOp::stash_files_for(&[&unstaged_deleted]),
            Some(set(StashMode::All, &["gone.rs"], &[]))
        );
        // Nothing, or any conflict, stashes nothing.
        let conflicted = entry("both.rs", 'U', 'U', EntryKind::Conflicted);
        assert_eq!(GitOp::stash_files_for(&[]), None);
        assert_eq!(
            GitOp::stash_files_for(&[&changed, &conflicted, &untracked]),
            None
        );
    }

    #[test]
    fn a_rename_is_pushed_without_its_old_path_then_restored_from_head() {
        let head = "ab".repeat(20);
        let empty_top = EmptyTop::new();
        let op = set_push(
            set(
                StashMode::IncludeUntracked,
                &["new.rs", "u.rs"],
                &["-old.rs"],
            ),
            Some("m"),
        );
        let push = op_command(&op);
        assert_eq!(
            argv(&push),
            ["stash", "push", "-u", "-m", "m", "--", "new.rs", "u.rs"]
        );
        let literal = [("GIT_LITERAL_PATHSPECS", "1"), ("LC_ALL", "C")];
        assert_eq!(literal_env(&push), literal);
        let restore = restore_command(&oid(&head), &["-old.rs".into()]);
        assert_eq!(argv(&restore), ["checkout", "-q", &head, "--", "-old.rs"]);
        assert_eq!(literal_env(&restore), literal);

        let runner = FakeRunner::new()
            .expect(
                &["rev-parse", "--verify", "-q", "HEAD^{commit}"],
                ok(format!("{head}\n").as_bytes()),
            )
            .expect(
                &["ls-files", "-z", "--cached", "--others", "--", "-old.rs"],
                ok(b""),
            )
            .expect(&["rev-parse", "--show-toplevel"], empty_top.stdout())
            .expect(
                &["stash", "push", "-u", "-m", "m", "--", "new.rs", "u.rs"],
                ok(b""),
            )
            .expect(&["checkout", "-q", &head, "--", "-old.rs"], ok(b""));
        assert_eq!(run_op(&runner, &op), Ok(()));
        runner.assert_done();
    }

    #[test]
    fn a_rename_whose_old_path_is_taken_runs_no_push() {
        let head = "ab".repeat(20);
        let op = set_push(set(StashMode::All, &["new.rs"], &["old.rs"]), None);
        let runner = FakeRunner::new()
            .expect(
                &["rev-parse", "--verify", "-q", "HEAD^{commit}"],
                ok(head.as_bytes()),
            )
            .expect(
                &["ls-files", "-z", "--cached", "--others", "--", "old.rs"],
                ok(b"old.rs\0"),
            );
        assert_eq!(run_op(&runner, &op), Err(STASH_RENAME_TAKEN.into()));
        runner.assert_done();
    }

    #[test]
    fn a_failed_restore_says_the_stash_was_saved_and_how_to_restore() {
        let head = "ab".repeat(20);
        let empty_top = EmptyTop::new();
        let op = set_push(set(StashMode::All, &["new.rs"], &["old.rs"]), None);
        let runner = FakeRunner::new()
            .expect(
                &["rev-parse", "--verify", "-q", "HEAD^{commit}"],
                ok(head.as_bytes()),
            )
            .expect(
                &["ls-files", "-z", "--cached", "--others", "--", "old.rs"],
                ok(b""),
            )
            .expect(&["rev-parse", "--show-toplevel"], empty_top.stdout())
            .expect(&["stash", "push", "--", "new.rs"], ok(b""))
            .expect(
                &["checkout", "-q", &head, "--", "old.rs"],
                GitOutput {
                    success: false,
                    stdout: vec![],
                    stderr: b"fatal: index.lock exists\n".to_vec(),
                },
            );
        assert_eq!(
            run_op(&runner, &op),
            Err(
                "fatal: index.lock exists\nThe stash was saved, but old.rs is still \
                 staged as deleted; check it out from HEAD to restore it."
                    .into()
            )
        );
        runner.assert_done();
    }

    #[test]
    fn a_staged_deletion_is_reset_to_head_then_named_to_the_push() {
        let head = "ab".repeat(20);
        let empty_top = EmptyTop::new();
        let gone = entry("-gone.rs", 'D', '.', EntryKind::Changed);
        let op = set_push(GitOp::stash_file_for(&gone).unwrap(), None);
        let literal = [("GIT_LITERAL_PATHSPECS", "1"), ("LC_ALL", "C")];
        let reset = unstage_command(&oid(&head), &["-gone.rs".into()]);
        assert_eq!(argv(&reset), ["reset", "-q", &head, "--", "-gone.rs"]);
        assert_eq!(literal_env(&reset), literal);
        assert_eq!(literal_env(&op_command(&op)), literal);
        let guard = restore_taken_command(&["-gone.rs".into()]);
        assert_eq!(literal_env(&guard), literal);

        let runner = FakeRunner::new()
            .expect(
                &["rev-parse", "--verify", "-q", "HEAD^{commit}"],
                ok(format!("{head}\n").as_bytes()),
            )
            .expect(
                &["ls-files", "-z", "--cached", "--others", "--", "-gone.rs"],
                ok(b""),
            )
            .expect(&["rev-parse", "--show-toplevel"], empty_top.stdout())
            .expect(&["reset", "-q", &head, "--", "-gone.rs"], ok(b""))
            .expect(&["stash", "push", "--", "-gone.rs"], ok(b""));
        assert_eq!(run_op(&runner, &op), Ok(()));
        runner.assert_done();
    }

    #[test]
    fn a_rename_and_a_deletion_together_unstage_before_and_restore_after() {
        let head = "ab".repeat(20);
        let empty_top = EmptyTop::new();
        let op = set_push(
            unstaging(
                StashMode::All,
                &["new.rs", "gone.rs"],
                &["old.rs"],
                &["gone.rs"],
            ),
            None,
        );
        let runner = FakeRunner::new()
            .expect(
                &["rev-parse", "--verify", "-q", "HEAD^{commit}"],
                ok(head.as_bytes()),
            )
            .expect(
                &["ls-files", "-z", "--cached", "--others", "--", "old.rs"],
                ok(b""),
            )
            .expect(
                &["ls-files", "-z", "--cached", "--others", "--", "gone.rs"],
                ok(b""),
            )
            .expect(&["rev-parse", "--show-toplevel"], empty_top.stdout())
            .expect(&["reset", "-q", &head, "--", "gone.rs"], ok(b""))
            .expect(&["stash", "push", "--", "new.rs", "gone.rs"], ok(b""))
            .expect(&["checkout", "-q", &head, "--", "old.rs"], ok(b""));
        assert_eq!(run_op(&runner, &op), Ok(()));
        runner.assert_done();
    }

    #[test]
    fn a_deletion_whose_path_is_taken_runs_no_reset_or_push() {
        let head = "ab".repeat(20);
        let op = set_push(
            unstaging(StashMode::All, &["gone.rs"], &[], &["gone.rs"]),
            None,
        );
        let runner = FakeRunner::new()
            .expect(
                &["rev-parse", "--verify", "-q", "HEAD^{commit}"],
                ok(head.as_bytes()),
            )
            .expect(
                &["ls-files", "-z", "--cached", "--others", "--", "gone.rs"],
                ok(b"gone.rs\0"),
            );
        assert_eq!(run_op(&runner, &op), Err(STASH_DELETED_TAKEN.into()));
        runner.assert_done();
    }

    #[test]
    fn a_failed_push_stages_the_deletion_again() {
        let head = "ab".repeat(20);
        let failed = |why: &[u8]| GitOutput {
            success: false,
            stdout: vec![],
            stderr: why.to_vec(),
        };
        let op = set_push(
            unstaging(StashMode::All, &["gone.rs"], &[], &["gone.rs"]),
            None,
        );
        let runner = |restaged: GitOutput, top: &EmptyTop| {
            FakeRunner::new()
                .expect(
                    &["rev-parse", "--verify", "-q", "HEAD^{commit}"],
                    ok(head.as_bytes()),
                )
                .expect(
                    &["ls-files", "-z", "--cached", "--others", "--", "gone.rs"],
                    ok(b""),
                )
                .expect(&["rev-parse", "--show-toplevel"], top.stdout())
                .expect(&["reset", "-q", &head, "--", "gone.rs"], ok(b""))
                .expect(
                    &["stash", "push", "--", "gone.rs"],
                    failed(b"error: push failed\n"),
                )
                .expect(&["rm", "--cached", "-q", "--", "gone.rs"], restaged)
        };
        let top = EmptyTop::new();
        let rolled_back = runner(ok(b""), &top);
        assert_eq!(run_op(&rolled_back, &op), Err("error: push failed".into()));
        rolled_back.assert_done();

        let stuck = runner(failed(b"fatal: index.lock exists\n"), &top);
        assert_eq!(
            run_op(&stuck, &op),
            Err(
                "error: push failed\nfatal: index.lock exists\ngone.rs is now an \
                 unstaged deletion; stage it again with git rm --cached -- gone.rs."
                    .into()
            )
        );
        stuck.assert_done();
    }

    /// An empty directory standing in for the worktree's top level, so the
    /// filesystem check finds nothing there.
    struct EmptyTop(crate::git::fake::TempDir);

    impl EmptyTop {
        fn new() -> Self {
            Self(crate::git::fake::TempDir::new("top"))
        }

        fn stdout(&self) -> GitOutput {
            let mut line = self.0.path().as_os_str().as_encoded_bytes().to_vec();
            line.push(b'\n');
            ok(&line)
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_disk_check_refuses_anything_at_the_old_path_or_a_non_directory_above_it() {
        let tmp = crate::git::fake::TempDir::new("disk");
        let top = tmp.path();
        let blocked = |old: &str| restore_blocked_on_disk(top, &[old.into()]);
        assert!(!blocked("old.txt"), "nothing there");
        assert!(!blocked("a/b/old.txt"), "no directories either");

        std::fs::create_dir_all(top.join("a/b")).unwrap();
        std::fs::write(top.join("a/b/other.txt"), "x").unwrap();
        assert!(!blocked("a/b/old.txt"), "real directories are fine");
        std::fs::write(top.join("a/b/old.txt"), "mine").unwrap();
        assert!(blocked("a/b/old.txt"), "a file at the old path");

        std::fs::write(top.join("f"), "mine").unwrap();
        assert!(blocked("f/old.txt"), "a file where a directory goes");
        assert!(blocked("f/g/old.txt"), "and below it");

        let elsewhere = crate::git::fake::TempDir::new("elsewhere");
        std::os::unix::fs::symlink(elsewhere.path(), top.join("s")).unwrap();
        assert!(blocked("s/old.txt"), "a symlinked directory is not real");
        std::os::unix::fs::symlink("missing", top.join("dangling")).unwrap();
        assert!(blocked("dangling"), "a dangling symlink at the old path");
    }

    /// Whether git took the repo at `dir` to be on a case-insensitive
    /// filesystem; the tests that need one skip otherwise.
    fn ignores_case(dir: &Path) -> bool {
        crate::git::fake::git(dir, &["config", "--get", "core.ignorecase"]) == b"true\n"
    }

    /// A repo whose last commit adds `old`, then stages `old` renamed to
    /// `new.txt`. Git may leave an emptied directory behind; it is removed.
    fn renamed_away(old: &str) -> crate::git::fake::TempDir {
        use crate::git::fake::git;
        let (tmp, _) = two_commit_repo();
        let dir = tmp.path();
        if let Some(parent) = Path::new(old).parent() {
            std::fs::create_dir_all(dir.join(parent)).unwrap();
        }
        std::fs::write(dir.join(old), "orig\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "old"]);
        git(dir, &["mv", "--", old, "new.txt"]);
        if let Some(parent) = Path::new(old)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
        {
            let _ = std::fs::remove_dir(dir.join(parent));
        }
        tmp
    }

    #[test]
    fn a_file_at_the_old_path_in_another_case_stops_the_stash_in_real_git() {
        use crate::git::fake::git;
        let tmp = renamed_away("Old.txt");
        let dir = tmp.path();
        if !ignores_case(dir) {
            return;
        }
        std::fs::write(dir.join("old.txt"), "MINE\n").unwrap();
        let before = git(dir, &["status", "--porcelain"]);

        let op = set_push(set(StashMode::All, &["new.txt"], &["Old.txt"]), None);
        let runner = crate::git::CliRunner::isolated(dir);
        assert_eq!(run_op(&runner, &op), Err(STASH_RENAME_TAKEN.into()));
        assert_eq!(stash_count(dir), 0);
        assert_eq!(git(dir, &["status", "--porcelain"]), before);
        assert_eq!(
            std::fs::read_to_string(dir.join("old.txt")).unwrap(),
            "MINE\n"
        );
    }

    #[test]
    fn a_file_at_the_old_directory_in_another_case_stops_the_stash_in_real_git() {
        use crate::git::fake::git;
        let tmp = renamed_away("D/old.txt");
        let dir = tmp.path();
        if !ignores_case(dir) {
            return;
        }
        std::fs::write(dir.join("d"), "MINE\n").unwrap();
        let before = git(dir, &["status", "--porcelain"]);

        let op = set_push(set(StashMode::All, &["new.txt"], &["D/old.txt"]), None);
        let runner = crate::git::CliRunner::isolated(dir);
        assert_eq!(run_op(&runner, &op), Err(STASH_RENAME_TAKEN.into()));
        assert_eq!(stash_count(dir), 0);
        assert_eq!(git(dir, &["status", "--porcelain"]), before);
        assert_eq!(std::fs::read_to_string(dir.join("d")).unwrap(), "MINE\n");
    }

    #[test]
    fn leading_dirs_are_every_directory_a_path_sits_in() {
        let dirs = |p: &str| leading_dirs(&p.into());
        assert_eq!(dirs("old.rs"), Vec::<RepoPath>::new());
        assert_eq!(dirs("a/b/c"), [RepoPath::from("a"), "a/b".into()]);
    }

    #[test]
    fn a_file_at_a_directory_the_restore_needs_runs_no_push() {
        let head = "ab".repeat(20);
        let empty_top = EmptyTop::new();
        let op = set_push(set(StashMode::All, &["new.rs"], &["a/b/old.rs"]), None);
        let at_old = ["ls-files", "-z", "--cached", "--others", "--", "a/b/old.rs"];
        let at_dirs = ["ls-files", "-z", "--cached", "--others", "--", "a", "a/b"];
        let verify = ["rev-parse", "--verify", "-q", "HEAD^{commit}"];
        // `a/b` is a file: refused.
        let runner = FakeRunner::new()
            .expect(&verify, ok(head.as_bytes()))
            .expect(&at_old, ok(b""))
            .expect(&at_dirs, ok(b"a/other.rs\0a/b\0"));
        assert_eq!(run_op(&runner, &op), Err(STASH_RENAME_TAKEN.into()));
        runner.assert_done();
        // Only files inside real directories: the push and restore run.
        let runner = FakeRunner::new()
            .expect(&verify, ok(head.as_bytes()))
            .expect(&at_old, ok(b""))
            .expect(&at_dirs, ok(b"a/other.rs\0a/bc\0"))
            .expect(&["rev-parse", "--show-toplevel"], empty_top.stdout())
            .expect(&["stash", "push", "--", "new.rs"], ok(b""))
            .expect(&["checkout", "-q", &head, "--", "a/b/old.rs"], ok(b""));
        assert_eq!(run_op(&runner, &op), Ok(()));
        runner.assert_done();
    }

    #[test]
    fn a_file_where_the_old_paths_directory_was_stops_the_stash_in_real_git() {
        use crate::git::fake::git;
        let (tmp, _) = two_commit_repo();
        let dir = tmp.path();
        std::fs::create_dir(dir.join("d")).unwrap();
        std::fs::write(dir.join("d/old.txt"), "old\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "old"]);
        git(dir, &["mv", "--", "d/old.txt", "new.txt"]);
        // Git may leave the emptied directory behind.
        let _ = std::fs::remove_dir(dir.join("d"));
        std::fs::write(dir.join("d"), "mine\n").unwrap();
        let before = git(dir, &["status", "--porcelain"]);
        assert_eq!(before, b"R  d/old.txt -> new.txt\n?? d\n");

        let op = set_push(set(StashMode::All, &["new.txt"], &["d/old.txt"]), None);
        let runner = crate::git::CliRunner::isolated(dir);
        assert_eq!(run_op(&runner, &op), Err(STASH_RENAME_TAKEN.into()));
        assert_eq!(stash_count(dir), 0);
        assert_eq!(git(dir, &["status", "--porcelain"]), before);
        assert_eq!(std::fs::read_to_string(dir.join("d")).unwrap(), "mine\n");
    }

    /// A repo with `old.txt` committed and staged as renamed to `new.txt`,
    /// with its content changed.
    fn staged_rename_repo() -> crate::git::fake::TempDir {
        use crate::git::fake::git;
        let (tmp, _) = two_commit_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("old.txt"), "old\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "old"]);
        git(dir, &["mv", "--", "old.txt", "new.txt"]);
        assert_eq!(
            git(dir, &["status", "--porcelain"]),
            b"R  old.txt -> new.txt\n"
        );
        tmp
    }

    fn stash_count(dir: &Path) -> usize {
        let list = crate::git::fake::git(dir, &["stash", "list"]);
        list.iter().filter(|&&b| b == b'\n').count()
    }

    #[test]
    fn a_staged_rename_is_stashed_whole_and_pops_back_in_real_git() {
        use crate::git::fake::git;
        let tmp = staged_rename_repo();
        let dir = tmp.path();
        let renamed = FileEntry {
            orig_path: Some("old.txt".into()),
            ..entry("new.txt", 'R', '.', EntryKind::Renamed)
        };
        let op = set_push(GitOp::stash_file_for(&renamed).unwrap(), None);
        let runner = crate::git::CliRunner::isolated(dir);
        run_op(&runner, &op).unwrap();

        assert_eq!(git(dir, &["status", "--porcelain"]), b"");
        assert_eq!(stash_count(dir), 1);
        assert_eq!(
            git(dir, &["stash", "show", "--name-status", "stash@{0}"]),
            b"R100\told.txt\tnew.txt\n"
        );

        git(dir, &["stash", "pop", "-q"]);
        assert_eq!(
            std::fs::read_to_string(dir.join("new.txt")).unwrap(),
            "old\n"
        );
        assert!(!dir.join("old.txt").exists());
        git(dir, &["add", "-A"]);
        assert_eq!(
            git(dir, &["diff", "HEAD", "-M", "--name-status"]),
            b"R100\told.txt\tnew.txt\n"
        );
    }

    #[test]
    fn a_file_back_at_the_old_path_stops_the_stash_before_git_pushes_in_real_git() {
        use crate::git::fake::git;
        let tmp = staged_rename_repo();
        let dir = tmp.path();
        let op = set_push(set(StashMode::All, &["new.txt"], &["old.txt"]), None);
        // It appeared after the set was chosen.
        std::fs::write(dir.join("old.txt"), "mine\n").unwrap();
        let before = git(dir, &["status", "--porcelain"]);

        let runner = crate::git::CliRunner::isolated(dir);
        assert_eq!(run_op(&runner, &op), Err(STASH_RENAME_TAKEN.into()));
        assert_eq!(stash_count(dir), 0);
        assert_eq!(git(dir, &["status", "--porcelain"]), before);
        assert_eq!(
            std::fs::read_to_string(dir.join("old.txt")).unwrap(),
            "mine\n"
        );
    }

    /// A repo with `d/gone`, the only file in `d`, staged as deleted, and
    /// two changes beside it that no stash here selects: ` M other` and
    /// `A  added`.
    fn staged_deletion_repo() -> crate::git::fake::TempDir {
        use crate::git::fake::git;
        let (tmp, _) = two_commit_repo();
        let dir = tmp.path();
        std::fs::create_dir(dir.join("d")).unwrap();
        std::fs::write(dir.join("d/gone"), "gone\n").unwrap();
        std::fs::write(dir.join("other"), "other\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "gone"]);
        git(dir, &["rm", "-q", "--", "d/gone"]);
        let _ = std::fs::remove_dir(dir.join("d"));
        std::fs::write(dir.join("other"), "other changed\n").unwrap();
        std::fs::write(dir.join("added"), "added\n").unwrap();
        git(dir, &["add", "--", "added"]);
        assert_eq!(
            git(dir, &["status", "--porcelain"]),
            b"A  added\nD  d/gone\n M other\n"
        );
        tmp
    }

    #[test]
    fn a_lone_staged_deletion_is_stashed_and_pops_back_in_real_git() {
        use crate::git::fake::git;
        let tmp = staged_deletion_repo();
        let dir = tmp.path();
        let deleted = entry("d/gone", 'D', '.', EntryKind::Changed);
        let op = set_push(GitOp::stash_file_for(&deleted).unwrap(), None);
        let runner = crate::git::CliRunner::isolated(dir);
        run_op(&runner, &op).unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.join("d/gone")).unwrap(),
            "gone\n"
        );
        assert_eq!(
            git(dir, &["status", "--porcelain"]),
            b"A  added\n M other\n"
        );
        let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap();
        assert_eq!(read("other"), "other changed\n");
        assert_eq!(read("added"), "added\n");
        assert_eq!(stash_count(dir), 1);
        // The stash's trees start from the whole index, so the staged
        // `added` shows too; only the named path is cleaned.
        let shown = git(dir, &["stash", "show", "--name-status", "stash@{0}"]);
        assert_eq!(shown, b"A\tadded\nD\td/gone\n");

        git(dir, &["stash", "pop", "-q"]);
        assert!(!dir.join("d/gone").exists());
        assert_eq!(
            git(dir, &["status", "--porcelain"]),
            b"A  added\n D d/gone\n M other\n"
        );
        assert_eq!(stash_count(dir), 0);
    }

    #[test]
    fn a_file_back_at_the_deleted_path_stops_the_stash_before_git_changes_in_real_git() {
        use crate::git::fake::git;
        let tmp = staged_deletion_repo();
        let dir = tmp.path();
        let deleted = entry("d/gone", 'D', '.', EntryKind::Changed);
        let op = set_push(GitOp::stash_file_for(&deleted).unwrap(), None);
        // It appeared after the set was chosen.
        std::fs::create_dir(dir.join("d")).unwrap();
        std::fs::write(dir.join("d/gone"), "mine\n").unwrap();
        let before = git(dir, &["status", "--porcelain"]);
        assert!(before.windows(10).any(|w| w == b"D  d/gone\n"));

        let runner = crate::git::CliRunner::isolated(dir);
        assert_eq!(run_op(&runner, &op), Err(STASH_DELETED_TAKEN.into()));
        assert_eq!(stash_count(dir), 0);
        assert_eq!(git(dir, &["status", "--porcelain"]), before);
        assert_eq!(
            std::fs::read_to_string(dir.join("d/gone")).unwrap(),
            "mine\n"
        );
    }

    #[test]
    fn a_file_at_the_deleted_path_in_another_case_stops_the_stash_in_real_git() {
        use crate::git::fake::git;
        let tmp = staged_deletion_repo();
        let dir = tmp.path();
        if !ignores_case(dir) {
            return;
        }
        let deleted = entry("d/gone", 'D', '.', EntryKind::Changed);
        let op = set_push(GitOp::stash_file_for(&deleted).unwrap(), None);
        // Git sees `D/Gone` as untracked; the filesystem folds it onto
        // the deleted path.
        std::fs::create_dir(dir.join("D")).unwrap();
        std::fs::write(dir.join("D/Gone"), "MINE\n").unwrap();
        let before = git(dir, &["status", "--porcelain"]);

        let runner = crate::git::CliRunner::isolated(dir);
        assert_eq!(run_op(&runner, &op), Err(STASH_DELETED_TAKEN.into()));
        assert_eq!(stash_count(dir), 0);
        assert_eq!(git(dir, &["status", "--porcelain"]), before);
        assert_eq!(
            std::fs::read_to_string(dir.join("D/Gone")).unwrap(),
            "MINE\n"
        );
    }

    #[test]
    fn a_stash_of_several_files_names_them_all_after_the_separator() {
        let changed = entry("-a.rs", '.', 'M', EntryKind::Changed);
        let untracked = entry("*.txt", '?', '?', EntryKind::Untracked);
        let set = GitOp::stash_files_for(&[&changed, &untracked]).unwrap();
        let cmd = op_command(&set_push(set, Some("m")));
        assert_eq!(
            argv(&cmd),
            ["stash", "push", "-u", "-m", "m", "--", "-a.rs", "*.txt"]
        );
        assert_eq!(
            literal_env(&cmd),
            [("GIT_LITERAL_PATHSPECS", "1"), ("LC_ALL", "C")]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_stashed_files_name_reaches_argv_as_its_raw_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let mut untracked = entry("x", '?', '?', EntryKind::Untracked);
        untracked.path = RepoPath::from_bytes(b"x\xff");
        let cmd = op_command(&set_push(GitOp::stash_file_for(&untracked).unwrap(), None));
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
        let runner = expect_verify(runner, 0, "0ther").expect(
            &["stash", "store", "-m", "better words", STASH_OID],
            ok(b""),
        );
        let runner = expect_verify(runner, 0, STASH_OID);
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
        let drop = ["stash", "drop", "stash@{0}"];
        // With another commit below it, and with nothing below it.
        for below in ["0ther", ""] {
            let runner = expect_verify(FakeRunner::new(), 0, STASH_OID);
            let runner = expect_verify(runner, 1, below)
                .expect(&drop, ok(b""))
                .expect(&store, ok(b""));
            let runner = expect_verify(runner, 0, STASH_OID);
            assert_eq!(run_op(&runner, &op), Ok(()), "{below:?}");
            runner.assert_done();
        }

        // A failed store, or one that added nothing, says how to bring the
        // entry back.
        let runner = expect_verify(FakeRunner::new(), 0, STASH_OID);
        let runner = expect_verify(runner, 1, "0ther")
            .expect(&drop, ok(b""))
            .expect(&store, fail("fatal: no space"));
        let error = run_op(&runner, &op).unwrap_err();
        assert!(error.starts_with("fatal: no space\n"), "{error}");
        assert!(
            error.contains(&format!("git stash store {STASH_OID}")),
            "{error}"
        );
        runner.assert_done();
        let runner = expect_verify(FakeRunner::new(), 0, STASH_OID);
        let runner = expect_verify(runner, 1, "0ther")
            .expect(&drop, ok(b""))
            .expect(&store, ok(b""));
        let runner = expect_verify(runner, 0, "0ther");
        let error = run_op(&runner, &op).unwrap_err();
        assert!(
            error.contains(&format!("git stash store {STASH_OID}")),
            "{error}"
        );
        runner.assert_done();
    }

    #[test]
    fn renaming_an_entry_with_a_copy_beside_it_on_top_is_refused_without_dropping() {
        // The top entry with the same commit just below it: [X, X, ...].
        let op = GitOp::StashRename {
            stash: at(0),
            message: "m".into(),
        };
        let runner = expect_verify(FakeRunner::new(), 0, STASH_OID);
        let runner = expect_verify(runner, 1, STASH_OID);
        assert_eq!(run_op(&runner, &op), Err(STASH_COPY.into()));
        runner.assert_done();

        // A deeper entry with the same commit on top: [X, ..., X].
        for index in [1, 2] {
            let op = GitOp::StashRename {
                stash: at(index),
                message: "m".into(),
            };
            let runner = expect_verify(FakeRunner::new(), index, STASH_OID);
            let runner = expect_verify(runner, 0, STASH_OID);
            assert_eq!(run_op(&runner, &op), Err(STASH_COPY.into()), "{index}");
            runner.assert_done();
        }
    }

    #[test]
    fn a_rename_whose_store_or_old_place_moved_drops_nothing() {
        let op = GitOp::StashRename {
            stash: at(1),
            message: "m".into(),
        };
        let store = ["stash", "store", "-m", "m", STASH_OID];
        // The old place moved under the store.
        let runner = expect_verify(FakeRunner::new(), 1, STASH_OID);
        let runner = expect_verify(runner, 0, "0ther").expect(&store, ok(b""));
        let runner = expect_verify(runner, 0, STASH_OID);
        let runner = expect_verify(runner, 2, "f00");
        assert_eq!(run_op(&runner, &op), Err(STASH_LIST_CHANGED.into()));
        runner.assert_done();
        // The store added nothing on top.
        let runner = expect_verify(FakeRunner::new(), 1, STASH_OID);
        let runner = expect_verify(runner, 0, "0ther").expect(&store, ok(b""));
        let runner = expect_verify(runner, 0, "0ther");
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

    fn at_oid(index: usize, oid_hex: &str) -> StashRef {
        StashRef {
            index,
            oid: oid(oid_hex),
        }
    }

    #[test]
    fn a_range_of_stashes_drops_the_deepest_first_after_checking_every_one() {
        let (a, b, c) = ("a1".repeat(20), "b2".repeat(20), "c3".repeat(20));
        // Chosen in list order; dropped from the bottom up.
        let op = GitOp::StashDrops(vec![at_oid(1, &a), at_oid(3, &c), at_oid(2, &b)]);
        let steps = op_steps(&op);
        let argvs: Vec<_> = steps.iter().map(|s| argv(&op_command(s))).collect();
        assert_eq!(
            argvs,
            [
                ["stash", "drop", "stash@{3}"],
                ["stash", "drop", "stash@{2}"],
                ["stash", "drop", "stash@{1}"],
            ]
        );
        assert_eq!(argv(&op_command(&op)), ["stash", "drop", "stash@{3}"]);

        // Every entry is checked first; then each step checks its own
        // again just before git drops it.
        let mut runner = FakeRunner::new();
        for (index, found) in [(1, &a), (3, &c), (2, &b)] {
            runner = expect_verify(runner, index, found);
        }
        for (index, found) in [(3, &c), (2, &b), (1, &a)] {
            let drop = format!("stash@{{{index}}}");
            runner = expect_verify(runner, index, found).expect(&["stash", "drop", &drop], ok(b""));
        }
        assert_eq!(run_op(&runner, &op), Ok(()));
        runner.assert_done();
    }

    #[test]
    fn a_range_of_stashes_drops_nothing_when_any_entry_moved() {
        let (a, b, c) = ("a1".repeat(20), "b2".repeat(20), "c3".repeat(20));
        let op = GitOp::StashDrops(vec![at_oid(1, &a), at_oid(2, &b), at_oid(3, &c)]);
        // The last one checked moved, or is gone: no drop runs at all, and
        // a call to `stash drop` would find nothing expected and panic.
        for found in ["f00", ""] {
            let runner = expect_verify(FakeRunner::new(), 1, &a);
            let runner = expect_verify(runner, 2, &b);
            let runner = expect_verify(runner, 3, found);
            assert_eq!(run_op(&runner, &op), Err(STASH_LIST_CHANGED.into()));
            runner.assert_done();
        }
        // The first checked moved: nothing after it runs.
        let runner = expect_verify(FakeRunner::new(), 1, "f00");
        assert_eq!(run_op(&runner, &op), Err(STASH_LIST_CHANGED.into()));
        runner.assert_done();
    }

    #[test]
    fn an_empty_range_of_stashes_is_refused_without_running_git() {
        let op = GitOp::StashDrops(vec![]);
        assert_eq!(refusal(&op), Some("No stash to drop"));
        let runner = FakeRunner::new();
        assert_eq!(run_op(&runner, &op), Err("No stash to drop".into()));
        runner.assert_done();
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
        let op = set_push(GitOp::stash_file_for(&a).unwrap(), None);
        run_op(&runner, &op).unwrap();
        assert_eq!(read(dir, "a.txt"), "a\n");
        assert_eq!(std::fs::read(dir.join("b.txt")).unwrap(), other);
        assert_eq!(porcelain(dir), " M b.txt\n");

        // An untracked file goes too, with `-u`.
        std::fs::write(dir.join("new.txt"), "new\n").unwrap();
        let new = entry("new.txt", '?', '?', EntryKind::Untracked);
        let op = set_push(GitOp::stash_file_for(&new).unwrap(), None);
        run_op(&runner, &op).unwrap();
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
    fn a_range_of_stashes_drops_exactly_those_entries_in_real_git() {
        let tmp = stash_repo();
        let dir = tmp.path();
        let runner = crate::git::CliRunner::isolated(dir);
        for n in 0..4 {
            std::fs::write(dir.join("a.txt"), format!("change {n}\n")).unwrap();
            run_op(
                &runner,
                &push(StashMode::All, Some(&format!("wip {n}")), &[]),
            )
            .unwrap();
        }
        // Newest first: wip 3, wip 2, wip 1, wip 0. Drop the middle two.
        let op = GitOp::StashDrops(vec![real_ref(dir, 1), real_ref(dir, 2)]);
        run_op(&runner, &op).unwrap();
        let left: Vec<String> = real_entries(dir).into_iter().map(|e| e.subject).collect();
        assert_eq!(left.len(), 2, "{left:?}");
        assert!(left[0].ends_with(": wip 3"), "{left:?}");
        assert!(left[1].ends_with(": wip 0"), "{left:?}");
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

    /// A repository whose stash list holds `layout` from the top: each
    /// letter a commit, a repeated letter a copy of the same commit, each
    /// entry's subject `<letter><place>`.
    fn stash_layout(layout: &str) -> crate::git::fake::TempDir {
        use crate::git::fake::git;

        let tmp = stash_repo();
        let dir = tmp.path();
        let runner = crate::git::CliRunner::isolated(dir);
        let mut made = std::collections::HashMap::new();
        for letter in layout.chars() {
            if made.contains_key(&letter) {
                continue;
            }
            std::fs::write(dir.join("a.txt"), format!("{letter}\n")).unwrap();
            run_op(&runner, &push(StashMode::All, None, &[])).unwrap();
            let oid = real_entries(dir)[0].oid.clone();
            run_op(&runner, &GitOp::StashDrop(real_ref(dir, 0))).unwrap();
            made.insert(letter, oid);
        }
        // Stored bottom first, so `layout` reads from the top. Git adds no
        // entry for the commit already on top, so a filler goes between
        // twins and is dropped after: a drop keeps the twins side by side.
        std::fs::write(dir.join("a.txt"), "filler\n").unwrap();
        run_op(&runner, &push(StashMode::All, None, &[])).unwrap();
        let filler = real_entries(dir)[0].oid.clone();
        run_op(&runner, &GitOp::StashDrop(real_ref(dir, 0))).unwrap();
        let mut last = None;
        for (place, letter) in layout.chars().rev().enumerate() {
            if last == Some(letter) {
                git(dir, &["stash", "store", "-m", "_", filler.as_str()]);
            }
            let subject = format!("{letter}{}", layout.len() - 1 - place);
            git(
                dir,
                &["stash", "store", "-m", &subject, made[&letter].as_str()],
            );
            last = Some(letter);
        }
        while let Some(at) = real_entries(dir).iter().position(|e| e.subject == "_") {
            git(dir, &["stash", "drop", "-q", &format!("stash@{{{at}}}")]);
        }
        let listed: String = real_entries(dir)
            .iter()
            .map(|e| e.subject.chars().next().unwrap())
            .collect();
        assert_eq!(listed, layout, "the layout was not built as asked");
        tmp
    }

    fn subjects(dir: &Path) -> Vec<String> {
        real_entries(dir).into_iter().map(|e| e.subject).collect()
    }

    #[test]
    fn renaming_any_entry_with_a_copy_on_top_keeps_every_entry_in_real_git() {
        // Each layout, each place: refused with nothing changed, or renamed
        // with every other entry kept in order.
        let cases: [(&str, usize, bool); 9] = [
            ("XX", 0, false),
            ("XX", 1, false),
            ("XYX", 0, true),
            ("XYX", 1, true),
            ("XYX", 2, false),
            ("XXX", 0, false),
            ("XXX", 1, false),
            ("XXX", 2, false),
            ("XXY", 2, true),
        ];
        for (layout, index, renames) in cases {
            let tmp = stash_layout(layout);
            let dir = tmp.path();
            let runner = crate::git::CliRunner::isolated(dir);
            let before = real_entries(dir);
            let result = run_op(
                &runner,
                &GitOp::StashRename {
                    stash: before[index].stash_ref(),
                    message: "renamed".into(),
                },
            );
            let after = real_entries(dir);
            let case = format!("{layout} @{index}: {result:?}");
            if renames {
                assert_eq!(result, Ok(()), "{case}");
                assert_eq!(after[0].subject, "renamed", "{case}");
                assert_eq!(after[0].oid, before[index].oid, "{case}");
                let mut expected: Vec<String> = before.iter().map(|e| e.subject.clone()).collect();
                expected.remove(index);
                expected.insert(0, "renamed".into());
                assert_eq!(subjects(dir), expected, "{case}");
            } else {
                assert_eq!(result, Err(STASH_COPY.into()), "{case}");
                let unchanged: Vec<_> = before.iter().map(|e| e.subject.clone()).collect();
                assert_eq!(subjects(dir), unchanged, "{case}");
            }
        }
    }
}
