//! Work done since a rebase, cherry-pick or revert stopped: what skipping
//! the stopped commit would throw away. Read fresh when Skip is pressed,
//! never from the last status load, which cannot tell a conflict the user
//! resolved and staged from a part git merged cleanly itself.

use std::path::Path;
use std::time::SystemTime;

use super::path::RepoPath;
use super::repo_state::{Operation, git_dir_command, git_dir_from};
use super::runner::{GitCommand, GitRunner};
use super::status::{EntryKind, FileEntry, parse_status, status_command, stdout_of};

/// List the index's resolve-undo records: one per conflict resolved in
/// the index (`git add` or `git rm`) since the stop. A read with no
/// pathspec, so literal pathspecs change nothing.
pub fn resolve_undo_command() -> GitCommand {
    GitCommand::new("ls-files")
        .arg("--resolve-undo")
        .arg("-z")
        .standard_env()
}

/// The file in the git dir git writes when `op` stops on a commit, after
/// the worktree files. `None` for operations that have no commit to skip.
pub fn stop_marker(op: Operation) -> Option<&'static str> {
    match op {
        Operation::Rebase => Some("REBASE_HEAD"),
        Operation::CherryPick => Some("CHERRY_PICK_HEAD"),
        Operation::Revert => Some("REVERT_HEAD"),
        Operation::Merge | Operation::Bisect => None,
    }
}

/// Rule 1: a conflict was resolved in the index. The list is empty at
/// every fresh stop, and `rebase --skip` clears it.
pub fn resolved_in_index(resolve_undo: &[u8]) -> bool {
    !resolve_undo.is_empty()
}

/// Rule 2: a conflicted file was written after the stop marker, or is
/// gone. git keeps no snapshot of the stop, so the mtime is the only
/// trace of an edit that is not staged yet.
pub fn edited_since(marker: SystemTime, file: Option<SystemTime>) -> bool {
    file.is_none_or(|modified| modified > marker)
}

/// Rule 3, for a rebase only: a tracked file has unstaged changes. A
/// rebase starts clean and `rebase --skip` resets hard, so any is work
/// since the stop. A cherry-pick or revert keeps unstaged edits to other
/// files across a skip, so this rule does not apply to them.
pub fn unstaged_tracked_change(op: Operation, files: &[FileEntry]) -> bool {
    op == Operation::Rebase
        && files.iter().any(|f| {
            !matches!(f.kind, EntryKind::Untracked | EntryKind::Conflicted) && f.worktree != '.'
        })
}

/// Whether skipping `op`'s stopped commit would lose work done since the
/// stop. Any failure is an `Err`, which the caller treats as work found.
pub fn read_stop_work(runner: &dyn GitRunner, op: Operation) -> Result<bool, String> {
    let marker = stop_marker(op).ok_or_else(|| format!("A {} has no stop", op.subcommand()))?;
    let git_dir = git_dir_from(&stdout_of(
        runner,
        &git_dir_command(),
        "git rev-parse failed",
    )?);
    let status = parse_status(&stdout_of(runner, &status_command(), "git status failed")?)?;
    let resolve_undo = stdout_of(runner, &resolve_undo_command(), "git ls-files failed")?;
    if resolved_in_index(&resolve_undo) || unstaged_tracked_change(op, &status.files) {
        return Ok(true);
    }
    let conflicted: Vec<&RepoPath> = status
        .files
        .iter()
        .filter(|f| f.kind == EntryKind::Conflicted)
        .map(|f| &f.path)
        .collect();
    if conflicted.is_empty() {
        return Ok(false);
    }
    let stopped = modified(&git_dir.join(marker))?
        .ok_or_else(|| format!("The {} stop marker {marker} is missing", op.subcommand()))?;
    let workdir = runner.workdir();
    for path in conflicted {
        if edited_since(stopped, modified(&workdir.join(path.as_os_str()))?) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `path`'s mtime, without following a symlink; `None` when it is gone.
fn modified(path: &Path) -> Result<Option<SystemTime>, String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => meta.modified().map(Some).map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use super::*;
    use crate::git::CliRunner;
    use crate::git::fake::{FakeRunner, TempDir, fail, git, ok, try_git};
    use crate::git::runner::EnvProfile;

    fn entry(path: &str, worktree: char, kind: EntryKind) -> FileEntry {
        FileEntry {
            path: path.into(),
            orig_path: None,
            index: '.',
            worktree,
            kind,
        }
    }

    #[test]
    fn the_resolve_undo_read_takes_no_pathspec_in_the_standard_profile() {
        let cmd = resolve_undo_command();
        assert_eq!(cmd.args(), ["ls-files", "--resolve-undo", "-z"]);
        assert_eq!(cmd.env_profile(), Some(EnvProfile::Standard));
    }

    #[test]
    fn each_operation_that_can_skip_has_a_stop_marker() {
        assert_eq!(stop_marker(Operation::Rebase), Some("REBASE_HEAD"));
        assert_eq!(stop_marker(Operation::CherryPick), Some("CHERRY_PICK_HEAD"));
        assert_eq!(stop_marker(Operation::Revert), Some("REVERT_HEAD"));
        assert_eq!(stop_marker(Operation::Merge), None);
        assert_eq!(stop_marker(Operation::Bisect), None);
    }

    #[test]
    fn any_resolve_undo_record_is_work() {
        assert!(!resolved_in_index(b""));
        assert!(resolved_in_index(b"100644 abc 1\ta.txt\0"));
    }

    #[test]
    fn a_conflicted_file_written_after_the_stop_or_gone_is_work() {
        let stop = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        assert!(!edited_since(stop, Some(stop - Duration::from_secs(1))));
        assert!(!edited_since(stop, Some(stop)));
        assert!(edited_since(stop, Some(stop + Duration::from_nanos(1))));
        assert!(edited_since(stop, None));
    }

    #[test]
    fn only_a_rebase_counts_unstaged_changes_to_tracked_files() {
        let changed = [entry("a", 'M', EntryKind::Changed)];
        assert!(unstaged_tracked_change(Operation::Rebase, &changed));
        assert!(!unstaged_tracked_change(Operation::CherryPick, &changed));
        assert!(!unstaged_tracked_change(Operation::Revert, &changed));
        for kind in [EntryKind::Renamed, EntryKind::Copied] {
            assert!(unstaged_tracked_change(
                Operation::Rebase,
                &[entry("a", 'D', kind)]
            ));
        }
        let not_work = [
            entry("staged", '.', EntryKind::Changed),
            entry("new", '?', EntryKind::Untracked),
            entry("both", 'U', EntryKind::Conflicted),
        ];
        assert!(!unstaged_tracked_change(Operation::Rebase, &not_work));
    }

    #[test]
    fn a_merge_or_a_failed_read_is_an_error() {
        let fake = FakeRunner::new();
        assert!(read_stop_work(&fake, Operation::Merge).is_err());
        fake.assert_done();

        let fake = FakeRunner::new()
            .expect(&["rev-parse", "--absolute-git-dir"], ok(b"/r/.git\n"))
            .expect(
                &[
                    "status",
                    "--porcelain=v2",
                    "--branch",
                    "-z",
                    "--untracked-files=all",
                ],
                fail("fatal: bad\x1b]0;t\x07"),
            );
        let err = read_stop_work(&fake, Operation::Rebase).unwrap_err();
        assert!(err.contains("fatal: bad"), "{err}");
        assert!(!err.contains('\x1b'), "{err}");
        fake.assert_done();
    }

    // --- Against real git ---

    fn write(dir: &Path, name: &str, text: &str) {
        std::fs::write(dir.join(name), text).unwrap();
    }

    /// `main` with `a.txt` and `b.txt`, and `topic` with two commits that
    /// each change `a.txt` against `main`'s change to it, so each stops.
    fn diverged(label: &str) -> TempDir {
        let tmp = TempDir::new(label);
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        write(dir, "a.txt", "base\n");
        write(dir, "b.txt", "b\n");
        git(dir, &["add", "."]);
        git(dir, &["commit", "-q", "-m", "base"]);
        git(dir, &["checkout", "-q", "-b", "topic"]);
        write(dir, "a.txt", "topic one\n");
        git(dir, &["commit", "-q", "-am", "one"]);
        write(dir, "a.txt", "topic two\n");
        git(dir, &["commit", "-q", "-am", "two"]);
        git(dir, &["checkout", "-q", "main"]);
        write(dir, "a.txt", "main\n");
        git(dir, &["commit", "-q", "-am", "main"]);
        tmp
    }

    /// `diverged`, with `topic` rebased onto `main` and stopped on `one`.
    fn rebase_stopped(label: &str) -> TempDir {
        let tmp = diverged(label);
        let dir = tmp.path();
        git(dir, &["checkout", "-q", "topic"]);
        assert!(!try_git(dir, &["rebase", "main"]));
        tmp
    }

    /// `diverged`, with `topic`'s first commit cherry-picked onto `main`.
    fn cherry_pick_stopped(label: &str) -> TempDir {
        let tmp = diverged(label);
        assert!(!try_git(tmp.path(), &["cherry-pick", "topic~1"]));
        tmp
    }

    fn check(tmp: &TempDir, op: Operation) -> Result<bool, String> {
        read_stop_work(&CliRunner::isolated(tmp.path()), op)
    }

    /// Set `name`'s mtime well after the stop marker's, so the edit reads
    /// as later on any file system's clock.
    fn touch_after_stop(tmp: &TempDir, name: &str, marker: &str) {
        let dir = tmp.path();
        let stopped = std::fs::metadata(dir.join(".git").join(marker))
            .unwrap()
            .modified()
            .unwrap();
        let file = std::fs::File::options()
            .write(true)
            .open(dir.join(name))
            .unwrap();
        file.set_modified(stopped + Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn a_fresh_rebase_stop_has_no_work() {
        let tmp = rebase_stopped("stop-fresh");
        assert_eq!(check(&tmp, Operation::Rebase), Ok(false));
    }

    #[test]
    fn a_conflict_resolved_in_the_index_is_work() {
        let tmp = rebase_stopped("stop-added");
        write(tmp.path(), "a.txt", "resolved\n");
        git(tmp.path(), &["add", "a.txt"]);
        assert_eq!(check(&tmp, Operation::Rebase), Ok(true));
    }

    #[test]
    fn an_edited_conflicted_file_is_work() {
        let tmp = rebase_stopped("stop-edited");
        write(tmp.path(), "a.txt", "half resolved\n");
        touch_after_stop(&tmp, "a.txt", "REBASE_HEAD");
        assert_eq!(check(&tmp, Operation::Rebase), Ok(true));
    }

    #[test]
    fn a_missing_conflicted_file_is_work() {
        let tmp = rebase_stopped("stop-gone");
        std::fs::remove_file(tmp.path().join("a.txt")).unwrap();
        assert_eq!(check(&tmp, Operation::Rebase), Ok(true));
    }

    #[test]
    fn an_unrelated_unstaged_edit_is_work_in_a_rebase_but_not_a_cherry_pick() {
        let tmp = rebase_stopped("stop-rebase-other");
        write(tmp.path(), "b.txt", "edited\n");
        assert_eq!(check(&tmp, Operation::Rebase), Ok(true));

        let tmp = cherry_pick_stopped("stop-pick-other");
        assert_eq!(check(&tmp, Operation::CherryPick), Ok(false));
        write(tmp.path(), "b.txt", "edited\n");
        assert_eq!(check(&tmp, Operation::CherryPick), Ok(false));
    }

    #[test]
    fn the_next_stop_after_a_skip_has_no_work() {
        let tmp = rebase_stopped("stop-next");
        write(tmp.path(), "a.txt", "resolved\n");
        git(tmp.path(), &["add", "a.txt"]);
        assert!(!try_git(tmp.path(), &["rebase", "--skip"]));
        assert!(tmp.path().join(".git/rebase-merge").exists());
        assert_eq!(check(&tmp, Operation::Rebase), Ok(false));
    }

    #[test]
    fn a_missing_stop_marker_with_conflicts_fails_closed() {
        let tmp = rebase_stopped("stop-no-marker");
        std::fs::remove_file(tmp.path().join(".git/REBASE_HEAD")).unwrap();
        let result = check(&tmp, Operation::Rebase);
        assert!(matches!(result, Err(_) | Ok(true)), "{result:?}");
    }
}
