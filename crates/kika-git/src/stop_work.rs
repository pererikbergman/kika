//! Work done since a rebase, cherry-pick or revert stopped: what skipping
//! the stopped commit would throw away. Read fresh when Skip is pressed,
//! never from the last status load, which cannot tell a conflict the user
//! resolved and staged from a part git merged cleanly itself. At a
//! rebase `edit` or `break` stop git merged nothing, so there any staged
//! change is the user's (rule 4).

use std::io::Read;
use std::path::Path;
use std::time::SystemTime;

use super::git_dir_file::{OpenError, open_regular};
use super::path::RepoPath;
use super::repo_state::{Operation, git_dir_command, git_dir_from};
use super::runner::{GitCommand, GitRunner};
use super::status::{EntryKind, FileEntry, parse_status, status_command, stdout_of};

/// The most of a rebase's `done` file that is read. A line is about a
/// hundred bytes, so this holds thousands of commits; a longer file is an
/// error, so Skip asks, rather than a memory risk.
const DONE_MAX: u64 = 1 << 20;

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

/// The command of the last line in a rebase's `done` file that is not
/// blank: its first word, such as `edit` or `break`. git copies no
/// comment lines to `done`.
pub fn last_done_command(done: &[u8]) -> Option<&[u8]> {
    done.split(|&b| b == b'\n')
        .rev()
        .find_map(|line| line.split(u8::is_ascii_whitespace).find(|w| !w.is_empty()))
}

/// Whether a rebase stopped on an `edit` or a `break`, where git stages
/// nothing, rather than on a conflict or a failed squash, where git
/// stages its own merge. An `edit` stop leaves `amend`; a conflict on an
/// `edit` line does not. A failed squash leaves `amend` too, but its
/// `done` ends in `squash`. A `break` stop leaves no `amend`.
pub fn edit_or_break(amend: bool, done: &[u8]) -> bool {
    match last_done_command(done) {
        Some(b"edit" | b"e") => amend,
        Some(b"break" | b"b") => true,
        _ => false,
    }
}

/// Rule 4, for a rebase `edit` or `break` stop only: a tracked file has
/// staged changes. Porcelain compares the index with HEAD, so a change
/// committed since the stop does not count, and `rebase --skip` keeps
/// commits. An intent-to-add entry is caught by rule 3.
pub fn staged_tracked_change(files: &[FileEntry]) -> bool {
    files.iter().any(|f| {
        matches!(
            f.kind,
            EntryKind::Changed | EntryKind::Renamed | EntryKind::Copied
        ) && f.index != '.'
    })
}

/// Whether the rebase in `git_dir` stopped on an `edit` or a `break`.
/// Only the merge backend has those commands, so no `rebase-merge/` is
/// `false`. Any failure to read `done` is an `Err`.
fn edit_or_break_stop(git_dir: &Path) -> Result<bool, String> {
    let state = git_dir.join("rebase-merge");
    match std::fs::symlink_metadata(&state) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.to_string()),
    }
    let amend = modified(&state.join("amend"))?.is_some();
    let done = read_done(&state.join("done"))?;
    Ok(edit_or_break(amend, &done))
}

/// The rebase's `done` file, opened only as a regular file
/// ([`open_regular`]). Missing, not regular, unreadable or over
/// [`DONE_MAX`] is an `Err`.
fn read_done(path: &Path) -> Result<Vec<u8>, String> {
    let fail = |why: &str| format!("Could not read the rebase's done list: {why}");
    let file = open_regular(path).map_err(|e| match e {
        OpenError::NotRegular => fail("not a regular file"),
        OpenError::Unreadable(e) => fail(&e.to_string()),
    })?;
    let mut done = Vec::new();
    file.take(DONE_MAX + 1)
        .read_to_end(&mut done)
        .map_err(|e| fail(&e.to_string()))?;
    if done.len() as u64 > DONE_MAX {
        return Err(fail("too long"));
    }
    Ok(done)
}

/// Whether skipping `op`'s stopped commit would lose work done since the
/// stop, by rules 1 to 4. Any failure is an `Err`, which the caller treats
/// as work found.
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
    // The git dir is only read when something is staged, so a fresh
    // conflict stop has no new reads and no new ways to fail.
    if op == Operation::Rebase
        && staged_tracked_change(&status.files)
        && edit_or_break_stop(&git_dir)?
    {
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
    use crate::CliRunner;
    use crate::fake::{FakeRunner, TempDir, fail, git, ok, try_git};
    use crate::runner::EnvProfile;

    fn entry(path: &str, worktree: char, kind: EntryKind) -> FileEntry {
        staged(path, '.', worktree, kind)
    }

    fn staged(path: &str, index: char, worktree: char, kind: EntryKind) -> FileEntry {
        FileEntry {
            path: path.into(),
            orig_path: None,
            index,
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
        // 100 ns is the finest step every platform's clock holds: Windows counts
        // in 100 ns ticks, so a 1 ns step would round away to `stop` itself.
        assert!(edited_since(stop, Some(stop + Duration::from_nanos(100))));
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
    fn the_last_done_command_is_the_first_word_of_the_last_line_with_text() {
        assert_eq!(
            last_done_command(b"pick a # x\nedit b # y\n"),
            Some(&b"edit"[..])
        );
        assert_eq!(
            last_done_command(b"pick a # x\nbreak\n\n"),
            Some(&b"break"[..])
        );
        assert_eq!(
            last_done_command(b"pick a # x\r\nedit b\r\n"),
            Some(&b"edit"[..])
        );
        assert_eq!(
            last_done_command(b"pick a\r\nbreak\r\n\r\n"),
            Some(&b"break"[..])
        );
        assert_eq!(last_done_command(b""), None);
        assert_eq!(last_done_command(b"\n"), None);
        assert_eq!(last_done_command(b"\r\n \n"), None);
    }

    #[test]
    fn an_edit_stop_needs_amend_and_a_break_stop_does_not() {
        assert!(edit_or_break(true, b"pick a\nedit b # y\n"));
        assert!(edit_or_break(true, b"e b # y\n"));
        assert!(edit_or_break(false, b"pick a\nbreak\n"));
        assert!(edit_or_break(false, b"b\n"));
        assert!(!edit_or_break(false, b"edit b # y\n"));
        assert!(!edit_or_break(true, b"pick a\nsquash b # y\n"));
        assert!(!edit_or_break(true, b"pick a\nfixup b # y\n"));
        assert!(!edit_or_break(false, b"pick a # x\n"));
        assert!(!edit_or_break(true, b""));
        assert!(!edit_or_break(false, b""));
    }

    #[test]
    fn a_staged_change_to_a_tracked_file_is_found() {
        assert!(staged_tracked_change(&[staged(
            "a",
            'M',
            '.',
            EntryKind::Changed
        )]));
        for kind in [EntryKind::Renamed, EntryKind::Copied] {
            assert!(staged_tracked_change(&[staged("a", 'R', '.', kind)]));
        }
        let not_staged = [
            entry("unstaged", 'M', EntryKind::Changed),
            staged("new", '?', '?', EntryKind::Untracked),
            staged("both", 'U', 'U', EntryKind::Conflicted),
        ];
        assert!(!staged_tracked_change(&not_staged));
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

    /// `main` with `a.txt` and `b.txt`, then two commits, `one` and `two`,
    /// that each change `a.txt`, so a rebase of them has no conflict.
    fn linear(label: &str) -> TempDir {
        let tmp = TempDir::new(label);
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        write(dir, "a.txt", "base\n");
        write(dir, "b.txt", "b\n");
        git(dir, &["add", "."]);
        git(dir, &["commit", "-q", "-m", "base"]);
        write(dir, "a.txt", "one\n");
        git(dir, &["commit", "-q", "-am", "one"]);
        write(dir, "a.txt", "two\n");
        git(dir, &["commit", "-q", "-am", "two"]);
        tmp
    }

    /// Run `rebase -i <upstream>` with its todo edited by the sed
    /// `scripts`, and `editor` as the message editor. `-i.bak` works with
    /// both BSD and GNU sed, and the editor variables a host may set are
    /// removed so they cannot override the config. git exits 0 at an
    /// `edit` or `break` stop, so the rebase must still be in progress.
    fn rebase_todo(dir: &Path, upstream: &str, scripts: &[&str], editor: &str) {
        let quoted: Vec<String> = scripts
            .iter()
            .map(|s| format!("-e {}", sh_quoted(s)))
            .collect();
        let sequence = format!("sequence.editor=sed -i.bak {}", quoted.join(" "));
        let core = format!("core.editor={editor}");
        std::process::Command::new("git")
            .current_dir(dir)
            .envs(crate::runner::isolation_env())
            .env_remove("GIT_SEQUENCE_EDITOR")
            .env_remove("GIT_EDITOR")
            .env_remove("VISUAL")
            .env_remove("EDITOR")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(["-c", "commit.gpgsign=false", "-c", &sequence, "-c", &core])
            .args(["rebase", "-i", upstream])
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(
            dir.join(".git/rebase-merge").is_dir(),
            "the rebase did not stop"
        );
    }

    /// `text` as one shell word: git runs the editor through the shell, so
    /// each sed script is quoted whole, any `'` closed, escaped and reopened.
    fn sh_quoted(text: &str) -> String {
        format!("'{}'", text.replace('\'', r"'\''"))
    }

    #[cfg(unix)]
    #[test]
    fn a_sed_script_reaches_the_shell_as_one_word() {
        for text in ["1{p;s/.*/break/;}", "s/'a b'/$x \\/", "''"] {
            let out = std::process::Command::new("sh")
                .args(["-c", &format!("printf %s {}", sh_quoted(text))])
                .output()
                .unwrap();
            assert_eq!(out.stdout, text.as_bytes(), "{text}");
        }
    }

    /// `linear`, rebased with `one` marked `edit`, stopped clean on it.
    fn edit_stopped(label: &str) -> TempDir {
        let tmp = linear(label);
        rebase_todo(tmp.path(), "HEAD~2", &["1s/^pick/edit/"], "true");
        assert!(tmp.path().join(".git/rebase-merge/amend").exists());
        tmp
    }

    /// `linear`, rebased with a `break` after `one`. The sed prints line 1
    /// and then replaces it, as BSD sed has no one-line append.
    fn break_stopped(label: &str) -> TempDir {
        let tmp = linear(label);
        rebase_todo(tmp.path(), "HEAD~2", &["1{p;s/.*/break/;}"], "true");
        let done = std::fs::read(tmp.path().join(".git/rebase-merge/done")).unwrap();
        assert_eq!(last_done_command(&done), Some(&b"break"[..]));
        tmp
    }

    fn stage_b(tmp: &TempDir) {
        write(tmp.path(), "b.txt", "staged\n");
        git(tmp.path(), &["add", "b.txt"]);
    }

    #[test]
    fn a_fresh_edit_stop_has_no_work() {
        let tmp = edit_stopped("stop-edit-fresh");
        assert_eq!(check(&tmp, Operation::Rebase), Ok(false));
    }

    #[test]
    fn a_staged_change_at_an_edit_stop_is_work() {
        let tmp = edit_stopped("stop-edit-staged");
        stage_b(&tmp);
        assert_eq!(check(&tmp, Operation::Rebase), Ok(true));
    }

    #[test]
    fn a_staged_change_amended_into_the_commit_at_an_edit_stop_is_not_work() {
        let tmp = edit_stopped("stop-edit-amended");
        stage_b(&tmp);
        git(tmp.path(), &["commit", "-q", "--amend", "--no-edit"]);
        assert!(tmp.path().join(".git/rebase-merge/amend").exists());
        assert_eq!(check(&tmp, Operation::Rebase), Ok(false));
    }

    #[test]
    fn a_staged_change_at_a_break_stop_is_work() {
        let tmp = break_stopped("stop-break");
        assert_eq!(check(&tmp, Operation::Rebase), Ok(false));
        stage_b(&tmp);
        assert_eq!(check(&tmp, Operation::Rebase), Ok(true));
    }

    #[test]
    fn a_squash_git_staged_when_its_editor_failed_is_not_work() {
        let tmp = linear("stop-squash");
        rebase_todo(tmp.path(), "HEAD~2", &["2s/^pick/squash/"], "false");
        let git_dir = tmp.path().join(".git/rebase-merge");
        assert!(git_dir.join("amend").exists());
        let status = git(tmp.path(), &["status", "--porcelain=v2", "-z"]);
        assert!(status.starts_with(b"1 M. "), "{status:?}");
        assert_eq!(check(&tmp, Operation::Rebase), Ok(false));
    }

    #[test]
    fn a_staged_change_at_a_conflict_stop_keeps_todays_rules() {
        let tmp = rebase_stopped("stop-conflict-staged");
        stage_b(&tmp);
        assert_eq!(check(&tmp, Operation::Rebase), Ok(false));

        let tmp = cherry_pick_stopped("stop-pick-staged");
        stage_b(&tmp);
        assert_eq!(check(&tmp, Operation::CherryPick), Ok(false));
    }

    #[test]
    fn a_staged_change_at_a_conflict_stop_on_an_edit_line_is_not_work() {
        let tmp = diverged("stop-edit-conflict");
        git(tmp.path(), &["checkout", "-q", "topic"]);
        rebase_todo(tmp.path(), "main", &["1s/^pick/edit/"], "true");
        let state = tmp.path().join(".git/rebase-merge");
        let done = std::fs::read(state.join("done")).unwrap();
        assert_eq!(last_done_command(&done), Some(&b"edit"[..]));
        assert!(!state.join("amend").exists());
        stage_b(&tmp);
        assert_eq!(check(&tmp, Operation::Rebase), Ok(false));
    }

    /// `edit_stopped` with a staged change, and `done` swapped for what
    /// `replace` puts at its path.
    fn edit_stopped_with_done(label: &str, replace: impl FnOnce(&Path)) -> TempDir {
        let tmp = edit_stopped(label);
        stage_b(&tmp);
        let done = tmp.path().join(".git/rebase-merge/done");
        std::fs::remove_file(&done).unwrap();
        replace(&done);
        tmp
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_done_file_fails_closed() {
        let tmp = edit_stopped_with_done("stop-done-link", |done| {
            let elsewhere = done.with_file_name("elsewhere");
            std::fs::write(&elsewhere, "pick a # x\n").unwrap();
            std::os::unix::fs::symlink(&elsewhere, done).unwrap();
        });
        let result = check(&tmp, Operation::Rebase);
        assert!(matches!(result, Err(_) | Ok(true)), "{result:?}");
    }

    #[test]
    fn a_directory_in_place_of_the_done_file_fails_closed() {
        let tmp = edit_stopped_with_done("stop-done-dir", |done| {
            std::fs::create_dir(done).unwrap();
        });
        let result = check(&tmp, Operation::Rebase);
        assert!(matches!(result, Err(_) | Ok(true)), "{result:?}");
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_in_place_of_the_done_file_fails_closed_and_never_blocks() {
        let tmp = edit_stopped_with_done("stop-done-fifo", crate::git_dir_file::mkfifo);
        let result = check(&tmp, Operation::Rebase);
        assert!(matches!(result, Err(_) | Ok(true)), "{result:?}");
    }

    #[test]
    fn a_done_file_over_the_cap_fails_closed() {
        let tmp = edit_stopped_with_done("stop-done-long", |done| {
            let mut text = vec![b'#'; DONE_MAX as usize];
            text.extend_from_slice(b"\npick a # x\n");
            std::fs::write(done, text).unwrap();
        });
        let result = check(&tmp, Operation::Rebase);
        assert!(matches!(result, Err(_) | Ok(true)), "{result:?}");
    }

    #[test]
    fn a_missing_done_file_at_an_edit_stop_with_a_staged_change_fails_closed() {
        let tmp = edit_stopped("stop-edit-no-done");
        stage_b(&tmp);
        std::fs::remove_file(tmp.path().join(".git/rebase-merge/done")).unwrap();
        let result = check(&tmp, Operation::Rebase);
        assert!(matches!(result, Err(_) | Ok(true)), "{result:?}");
    }
}
