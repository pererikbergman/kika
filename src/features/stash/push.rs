//! Stashing selected files: what a set of Files entries puts aside, and
//! the push that unstages staged deletions before it and restores the
//! old sides of staged renames after it, failing closed when anything
//! sits where git would write.

use std::path::Path;

use super::git::{StashMode, checked};
use crate::keymap::Reason;
use crate::notice::Notice;
use crate::op::{GitOp, op_command};
use kika_git::commit_files::CommitId;
use kika_git::path::RepoPath;
use kika_git::runner::{GitCommand, GitRunner};
use kika_git::status::{EntryKind, FileEntry};

/// The words before the path in [`stash_rename_taken`].
pub(crate) const STASH_RENAME_TAKEN_LEAD: &str = "A file now sits at the renamed file's old path: ";

/// Why a stash of a staged rename is refused: something sits at its old
/// path, `path` here, which the push would leave alone and the restore
/// overwrite. The name is its terminal-safe display form.
pub(crate) fn stash_rename_taken(path: &RepoPath) -> String {
    format!("{STASH_RENAME_TAKEN_LEAD}{}", path.display())
}

/// [`stash_rename_taken`] as a menu reason, its path marked so a narrow
/// row keeps the file name.
pub(crate) fn stash_rename_taken_reason(path: &RepoPath) -> Reason {
    Notice::with_path(STASH_RENAME_TAKEN_LEAD, path.display())
}

/// Why a stash of a rename that only changes case is refused: on a
/// case-insensitive filesystem its old and new paths are one file, so a
/// per-file stash cannot restore the old side.
pub const STASH_RENAME_CASE: &str = "This rename only changes case and cannot be stashed by file";

/// Whether `old` and `new` differ only in ASCII case.
pub(crate) fn case_only(old: &RepoPath, new: &RepoPath) -> bool {
    old != new && old.as_bytes().eq_ignore_ascii_case(new.as_bytes())
}

/// Why a stash of a staged deletion is refused: something sits at its
/// path, which the push's cleanup would overwrite with `HEAD`'s version.
pub const STASH_DELETED_TAKEN: &str = "A file now sits at the deleted file's path";

/// What a selected-file stash takes: the push's mode and pathspec, the
/// old sides of staged renames, restored after it, and the staged
/// deletions, reset to `HEAD` in the index before it. See
/// [`stash_files_for`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashSet {
    pub mode: StashMode,
    pub paths: Vec<RepoPath>,
    pub restore: Vec<RepoPath>,
    pub unstage: Vec<RepoPath>,
}

/// What stashing only `entry` puts aside. A staged rename goes as one
/// change: its new path in the pathspec and its old one restored after
/// the push, since git no longer knows the old path in the index and
/// refuses it as a pathspec. A staged deletion is named and also
/// unstaged before the push, for the same reason. Any other entry names
/// its paths, a copy's source too. An untracked file needs `-u`, since
/// git does not match untracked files by pathspec without it. Conflicts
/// cannot be stashed.
pub(crate) fn stash_file_for(entry: &FileEntry) -> Option<StashSet> {
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
pub(crate) fn stash_files_for(entries: &[&FileEntry]) -> Option<StashSet> {
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
        let one = stash_file_for(entry)?;
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

/// `cmd`, then `--` and each name from its raw bytes, read literally.
pub(super) fn with_literal_paths(cmd: GitCommand, paths: &[RepoPath]) -> GitCommand {
    paths
        .iter()
        .fold(cmd.arg("--"), |cmd, p| cmd.arg(p.as_os_str()))
        .write_env()
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

/// What sits where a restore of `restore` would write, the first name
/// git lists: at an old path or under it, or exactly at one of its leading
/// directories. A file under a leading directory is fine; that directory
/// is real. Output with no name in it still refuses, naming the first old
/// path, so an odd answer fails closed.
fn restore_taken(runner: &dyn GitRunner, restore: &[RepoPath]) -> Result<Option<RepoPath>, String> {
    let at_old = checked(runner, &restore_taken_command(restore))?;
    if !at_old.stdout.is_empty() {
        let name = at_old.stdout.split(|&b| b == 0).find(|n| !n.is_empty());
        return Ok(name
            .map(RepoPath::from_bytes)
            .or_else(|| restore.first().cloned()));
    }
    let mut dirs: Vec<RepoPath> = Vec::new();
    for dir in restore.iter().flat_map(leading_dirs) {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    if dirs.is_empty() {
        return Ok(None);
    }
    let found = checked(runner, &restore_taken_command(&dirs))?;
    Ok((found.stdout.split(|&b| b == 0))
        .find(|name| dirs.iter().any(|d| d.as_bytes() == *name))
        .map(RepoPath::from_bytes))
}

/// Where the worktree at `top` holds something a restore of `restore`
/// would overwrite or unlink: an old path with anything at it, or a
/// leading directory that is not a real directory (a file or a symlink).
/// The filesystem answers, so a name that differs only in case or Unicode
/// form counts where the filesystem folds them, as git's byte-wise checks
/// cannot; the name given back is the one probed, as the filesystem does
/// not say how it stores it. Anything unreadable counts as taken.
fn restore_blocked_on_disk(top: &Path, restore: &[RepoPath]) -> Option<RepoPath> {
    let found = |path: &RepoPath| match std::fs::symlink_metadata(top.join(path.as_os_str())) {
        Ok(meta) => Some(Ok(meta.is_dir())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => Some(Err(e)),
    };
    restore.iter().find_map(|old| {
        let bad_dir = leading_dirs(old)
            .into_iter()
            .find(|dir| matches!(found(dir), Some(Ok(false) | Err(_))));
        bad_dir.or_else(|| found(old).map(|_| old.clone()))
    })
}

/// `HEAD` as a commit id, for a restore that must use the tree the push
/// was made on.
fn head_commit(runner: &dyn GitRunner) -> Result<CommitId, String> {
    let output = checked(runner, &head_command())?;
    CommitId::parse(String::from_utf8_lossy(&output.stdout).trim())
        .ok_or_else(|| "Could not read HEAD".to_string())
}

/// `HEAD`'s commit. A read with no paths, so taking no optional lock and
/// reading pathspecs literally change nothing.
fn head_command() -> GitCommand {
    GitCommand::new("rev-parse")
        .arg("--verify")
        .arg("-q")
        .arg("HEAD^{commit}")
        .standard_env()
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
/// version there. A rename that only changes case is refused before any
/// git command runs, should the set have changed since its guard ran.
pub(crate) fn push_with_fixups(
    runner: &dyn GitRunner,
    op: &GitOp,
    restore: &[RepoPath],
    unstage: &[RepoPath],
) -> Result<(), String> {
    let paths = match op {
        GitOp::StashPush { paths, .. } => paths.as_slice(),
        _ => &[],
    };
    if (restore.iter()).any(|old| paths.iter().any(|new| case_only(old, new))) {
        return Err(STASH_RENAME_CASE.into());
    }
    let head = head_commit(runner)?;
    if !restore.is_empty()
        && let Some(path) = restore_taken(runner, restore)?
    {
        return Err(stash_rename_taken(&path));
    }
    if !unstage.is_empty() && restore_taken(runner, unstage)?.is_some() {
        return Err(STASH_DELETED_TAKEN.into());
    }
    let top = kika_git::repo_root::find_toplevel(runner)
        .ok_or_else(|| "Could not find the worktree's top level".to_string())?;
    if let Some(path) = restore_blocked_on_disk(&top, restore) {
        return Err(stash_rename_taken(&path));
    }
    if restore_blocked_on_disk(&top, unstage).is_some() {
        return Err(STASH_DELETED_TAKEN.into());
    }
    if !unstage.is_empty() {
        checked(runner, &unstage_command(&head, unstage))?;
    }
    if let Err(why) = checked(runner, &op_command(op)) {
        if unstage.is_empty() {
            return Err(why);
        }
        return match checked(runner, &restage_command(unstage)) {
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
    checked(runner, &restore_command(&head, restore))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::stash::git::tests::{WRITE, env_of, ignores_case, literal_env, set_push};
    use crate::op::run_op;
    use crate::test_support::{argv, cid as oid, file_entry as entry, two_commit_repo};
    use kika_git::fake::{FakeRunner, ok};
    use kika_git::runner::GitOutput;

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
            let op = set_push(stash_file_for(entry).unwrap(), Some("m"));
            let cmd = op_command(&op);
            assert_eq!(argv(&cmd), expected, "{op:?}");
            assert_eq!(literal_env(&cmd), env_of(&WRITE));
        }
        assert_eq!(stash_file_for(&conflicted), None);
    }

    #[test]
    fn the_path_commands_take_the_lock_and_the_head_read_does_not() {
        use kika_git::runner::EnvProfile;
        let paths: [RepoPath; 1] = ["-a.rs".into()];
        let head = oid(&"ab".repeat(20));
        for cmd in [
            restore_command(&head, &paths),
            restore_taken_command(&paths),
            unstage_command(&head, &paths),
            restage_command(&paths),
        ] {
            assert_eq!(cmd.env_profile(), Some(EnvProfile::Write), "{cmd:?}");
            assert_eq!(literal_env(&cmd), env_of(&WRITE), "{cmd:?}");
        }
        let read = head_command();
        assert_eq!(
            argv(&read),
            ["rev-parse", "--verify", "-q", "HEAD^{commit}"]
        );
        assert_eq!(read.env_profile(), Some(EnvProfile::Standard));
        assert_eq!(
            literal_env(&read),
            env_of(&[
                ("LC_ALL", "C"),
                ("GIT_OPTIONAL_LOCKS", "0"),
                ("GIT_LITERAL_PATHSPECS", "1")
            ])
        );
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
            stash_files_for(&[&renamed]),
            Some(set(StashMode::All, &["new name.rs"], &["old.rs"]))
        );
        assert_eq!(
            stash_files_for(&[&changed, &renamed, &renamed]),
            Some(set(StashMode::All, &["a.rs", "new name.rs"], &["old.rs"]))
        );
        assert_eq!(
            stash_files_for(&[&changed, &untracked]),
            Some(set(StashMode::IncludeUntracked, &["a.rs", "new.rs"], &[]))
        );
        // Its old path is never in the pathspec, so it can go with `-u`.
        assert_eq!(
            stash_files_for(&[&renamed, &untracked]),
            Some(set(
                StashMode::IncludeUntracked,
                &["new name.rs", "new.rs"],
                &["old.rs"]
            ))
        );
        // A copy's source is still in the index, so it is named.
        assert_eq!(
            stash_files_for(&[&copied]),
            Some(set(StashMode::All, &["a.rs", "copy.rs"], &[]))
        );
        // A rename whose old path is also marked is refused: the restore
        // would overwrite what the push took.
        let at_old = entry("old.rs", '?', '?', EntryKind::Untracked);
        assert_eq!(stash_files_for(&[&renamed, &at_old]), None);
        assert_eq!(stash_files_for(&[&at_old, &renamed]), None);
        // A staged deletion beside an untracked file is refused rather
        // than half done; an unstaged one is still on the index.
        let staged_deleted = entry("gone.rs", 'D', '.', EntryKind::Changed);
        let unstaged_deleted = entry("gone.rs", '.', 'D', EntryKind::Changed);
        assert_eq!(stash_files_for(&[&untracked, &staged_deleted]), None);
        assert_eq!(
            stash_files_for(&[&unstaged_deleted, &untracked]),
            Some(set(
                StashMode::IncludeUntracked,
                &["gone.rs", "new.rs"],
                &[]
            ))
        );
        // A staged deletion is named and unstaged before the push; an
        // unstaged one is still in the index, so it is only named.
        assert_eq!(
            stash_files_for(&[&staged_deleted]),
            Some(unstaging(StashMode::All, &["gone.rs"], &[], &["gone.rs"]))
        );
        assert_eq!(
            stash_files_for(&[&staged_deleted, &changed]),
            Some(unstaging(
                StashMode::All,
                &["gone.rs", "a.rs"],
                &[],
                &["gone.rs"]
            ))
        );
        assert_eq!(
            stash_files_for(&[&unstaged_deleted]),
            Some(set(StashMode::All, &["gone.rs"], &[]))
        );
        // Nothing, or any conflict, stashes nothing.
        let conflicted = entry("both.rs", 'U', 'U', EntryKind::Conflicted);
        assert_eq!(stash_files_for(&[]), None);
        assert_eq!(stash_files_for(&[&changed, &conflicted, &untracked]), None);
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
        let literal = env_of(&WRITE);
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
        assert_eq!(
            run_op(&runner, &op),
            Err(stash_rename_taken(&"old.rs".into()))
        );
        runner.assert_done();
    }

    #[test]
    fn output_with_no_name_in_it_still_refuses_naming_the_old_path() {
        let head = "ab".repeat(20);
        let op = set_push(set(StashMode::All, &["new.rs"], &["old.rs"]), None);
        let runner = FakeRunner::new()
            .expect(
                &["rev-parse", "--verify", "-q", "HEAD^{commit}"],
                ok(head.as_bytes()),
            )
            .expect(
                &["ls-files", "-z", "--cached", "--others", "--", "old.rs"],
                ok(b"\0"),
            );
        assert_eq!(
            run_op(&runner, &op),
            Err(stash_rename_taken(&"old.rs".into()))
        );
        runner.assert_done();
    }

    #[test]
    fn a_case_only_rename_is_refused_before_any_git_command_runs() {
        let op = set_push(set(StashMode::All, &["old.txt"], &["Old.txt"]), None);
        let runner = FakeRunner::new();
        assert_eq!(run_op(&runner, &op), Err(STASH_RENAME_CASE.into()));
        runner.assert_done();
    }

    #[test]
    fn a_case_only_rename_folds_ascii_case_and_differs_in_bytes() {
        let case_only = |old: &str, new: &str| case_only(&old.into(), &new.into());
        assert!(case_only("Old.txt", "old.txt"));
        assert!(case_only("D/old.rs", "d/old.rs"));
        assert!(!case_only("old.txt", "old.txt"), "the same name");
        assert!(!case_only("old.txt", "new.txt"));
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
        let op = set_push(stash_file_for(&gone).unwrap(), None);
        let literal = env_of(&WRITE);
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
    struct EmptyTop(kika_git::fake::TempDir);

    impl EmptyTop {
        fn new() -> Self {
            Self(kika_git::fake::TempDir::new("top"))
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
        let tmp = kika_git::fake::TempDir::new("disk");
        let top = tmp.path();
        let blocked = |old: &str| restore_blocked_on_disk(top, &[old.into()]).is_some();
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
        // It names the probed directory or old path.
        let named = |old: &str| restore_blocked_on_disk(top, &[old.into()]);
        assert_eq!(named("f/g/old.txt"), Some("f".into()));
        assert_eq!(named("a/b/old.txt"), Some("a/b/old.txt".into()));

        let elsewhere = kika_git::fake::TempDir::new("elsewhere");
        std::os::unix::fs::symlink(elsewhere.path(), top.join("s")).unwrap();
        assert!(blocked("s/old.txt"), "a symlinked directory is not real");
        std::os::unix::fs::symlink("missing", top.join("dangling")).unwrap();
        assert!(blocked("dangling"), "a dangling symlink at the old path");
    }

    /// A repo whose last commit adds `old`, then stages `old` renamed to
    /// `new.txt`. Git may leave an emptied directory behind; it is removed.
    fn renamed_away(old: &str) -> kika_git::fake::TempDir {
        use kika_git::fake::git;
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
        use kika_git::fake::git;
        let tmp = renamed_away("Old.txt");
        let dir = tmp.path();
        if !ignores_case(dir) {
            return;
        }
        std::fs::write(dir.join("old.txt"), "MINE\n").unwrap();
        let before = git(dir, &["status", "--porcelain"]);

        let op = set_push(set(StashMode::All, &["new.txt"], &["Old.txt"]), None);
        let runner = kika_git::CliRunner::isolated(dir);
        assert_eq!(
            run_op(&runner, &op),
            Err(stash_rename_taken(&"Old.txt".into()))
        );
        assert_eq!(stash_count(dir), 0);
        assert_eq!(git(dir, &["status", "--porcelain"]), before);
        assert_eq!(
            std::fs::read_to_string(dir.join("old.txt")).unwrap(),
            "MINE\n"
        );
    }

    #[test]
    fn a_file_at_the_old_directory_in_another_case_stops_the_stash_in_real_git() {
        use kika_git::fake::git;
        let tmp = renamed_away("D/old.txt");
        let dir = tmp.path();
        if !ignores_case(dir) {
            return;
        }
        std::fs::write(dir.join("d"), "MINE\n").unwrap();
        let before = git(dir, &["status", "--porcelain"]);

        let op = set_push(set(StashMode::All, &["new.txt"], &["D/old.txt"]), None);
        let runner = kika_git::CliRunner::isolated(dir);
        assert_eq!(run_op(&runner, &op), Err(stash_rename_taken(&"D".into())));
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
        assert_eq!(run_op(&runner, &op), Err(stash_rename_taken(&"a/b".into())));
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
        use kika_git::fake::git;
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
        let runner = kika_git::CliRunner::isolated(dir);
        assert_eq!(run_op(&runner, &op), Err(stash_rename_taken(&"d".into())));
        assert_eq!(stash_count(dir), 0);
        assert_eq!(git(dir, &["status", "--porcelain"]), before);
        assert_eq!(std::fs::read_to_string(dir.join("d")).unwrap(), "mine\n");
    }

    /// A repo with `old.txt` committed and staged as renamed to `new.txt`,
    /// with its content changed.
    fn staged_rename_repo() -> kika_git::fake::TempDir {
        use kika_git::fake::git;
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
        let list = kika_git::fake::git(dir, &["stash", "list"]);
        list.iter().filter(|&&b| b == b'\n').count()
    }

    #[test]
    fn a_staged_rename_is_stashed_whole_and_pops_back_in_real_git() {
        use kika_git::fake::git;
        let tmp = staged_rename_repo();
        let dir = tmp.path();
        let renamed = FileEntry {
            orig_path: Some("old.txt".into()),
            ..entry("new.txt", 'R', '.', EntryKind::Renamed)
        };
        let op = set_push(stash_file_for(&renamed).unwrap(), None);
        let runner = kika_git::CliRunner::isolated(dir);
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
        use kika_git::fake::git;
        let tmp = staged_rename_repo();
        let dir = tmp.path();
        let op = set_push(set(StashMode::All, &["new.txt"], &["old.txt"]), None);
        // It appeared after the set was chosen.
        std::fs::write(dir.join("old.txt"), "mine\n").unwrap();
        let before = git(dir, &["status", "--porcelain"]);

        let runner = kika_git::CliRunner::isolated(dir);
        assert_eq!(
            run_op(&runner, &op),
            Err(stash_rename_taken(&"old.txt".into()))
        );
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
    fn staged_deletion_repo() -> kika_git::fake::TempDir {
        use kika_git::fake::git;
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
        use kika_git::fake::git;
        let tmp = staged_deletion_repo();
        let dir = tmp.path();
        let deleted = entry("d/gone", 'D', '.', EntryKind::Changed);
        let op = set_push(stash_file_for(&deleted).unwrap(), None);
        let runner = kika_git::CliRunner::isolated(dir);
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
        use kika_git::fake::git;
        let tmp = staged_deletion_repo();
        let dir = tmp.path();
        let deleted = entry("d/gone", 'D', '.', EntryKind::Changed);
        let op = set_push(stash_file_for(&deleted).unwrap(), None);
        // It appeared after the set was chosen.
        std::fs::create_dir(dir.join("d")).unwrap();
        std::fs::write(dir.join("d/gone"), "mine\n").unwrap();
        let before = git(dir, &["status", "--porcelain"]);
        assert!(before.windows(10).any(|w| w == b"D  d/gone\n"));

        let runner = kika_git::CliRunner::isolated(dir);
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
        use kika_git::fake::git;
        let tmp = staged_deletion_repo();
        let dir = tmp.path();
        if !ignores_case(dir) {
            return;
        }
        let deleted = entry("d/gone", 'D', '.', EntryKind::Changed);
        let op = set_push(stash_file_for(&deleted).unwrap(), None);
        // Git sees `D/Gone` as untracked; the filesystem folds it onto
        // the deleted path.
        std::fs::create_dir(dir.join("D")).unwrap();
        std::fs::write(dir.join("D/Gone"), "MINE\n").unwrap();
        let before = git(dir, &["status", "--porcelain"]);

        let runner = kika_git::CliRunner::isolated(dir);
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
        let set = stash_files_for(&[&changed, &untracked]).unwrap();
        let cmd = op_command(&set_push(set, Some("m")));
        assert_eq!(
            argv(&cmd),
            ["stash", "push", "-u", "-m", "m", "--", "-a.rs", "*.txt"]
        );
        assert_eq!(literal_env(&cmd), env_of(&WRITE));
    }

    #[cfg(unix)]
    #[test]
    fn a_stashed_files_name_reaches_argv_as_its_raw_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let mut untracked = entry("x", '?', '?', EntryKind::Untracked);
        untracked.path = RepoPath::from_bytes(b"x\xff");
        let cmd = op_command(&set_push(stash_file_for(&untracked).unwrap(), None));
        let args: Vec<&[u8]> = cmd.args().iter().map(|a| a.as_bytes()).collect();
        assert_eq!(args, [&b"stash"[..], b"push", b"-u", b"--", b"x\xff"]);
    }
}
