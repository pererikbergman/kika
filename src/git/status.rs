use super::diff::sanitize;
use super::path::RepoPath;
use super::runner::{GitCommand, GitRunner};

/// What kind of change a status line describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Changed,
    Renamed,
    Copied,
    Conflicted,
    Untracked,
}

/// One file from `git status`. `index` and `worktree` are porcelain's X and Y
/// letters, kept raw so the view decides how to show them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub path: RepoPath,
    pub orig_path: Option<RepoPath>,
    pub index: char,
    pub worktree: char,
    pub kind: EntryKind,
}

impl FileEntry {
    /// The `XY` column as `git status -s` shows it: porcelain's `.` becomes a space.
    pub fn status_letters(&self) -> String {
        [self.index, self.worktree]
            .iter()
            .map(|&c| if c == '.' { ' ' } else { c })
            .collect()
    }
}

/// `git status` in the machine-readable form we parse. `GIT_OPTIONAL_LOCKS=0`
/// stops a background refresh from taking `index.lock` and racing the user.
pub fn status_command() -> GitCommand {
    GitCommand::new("status")
        .arg("--porcelain=v2")
        .arg("-z")
        .arg("--untracked-files=all")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
}

/// Run `git status` and parse it. Failures carry git's own message.
pub fn read_status(runner: &dyn GitRunner) -> Result<Vec<FileEntry>, String> {
    let output = runner.run(&status_command()).map_err(|e| e.to_string())?;
    if !output.success {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            "git status failed".to_string()
        } else {
            stderr
        });
    }
    parse_status(&output.stdout)
}

/// Parse `git status --porcelain=v2 -z`. Paths are taken as everything after
/// the fixed fields, so spaces inside them survive, and are kept as raw bytes
/// so a name that is not UTF-8 still reaches git unchanged.
pub fn parse_status(bytes: &[u8]) -> Result<Vec<FileEntry>, String> {
    let mut entries = Vec::new();
    let mut records = bytes.split(|&b| b == 0).filter(|r| !r.is_empty());

    while let Some(record) = records.next() {
        let malformed = || format!("malformed status record: {}", sanitize(record));
        let entry = match record.first() {
            Some(b'1') => {
                let (xy, path) = fields(record, 8).ok_or_else(malformed)?;
                changed(xy, path, None, EntryKind::Changed)
            }
            Some(b'2') => {
                let (xy, path) = fields(record, 9).ok_or_else(malformed)?;
                let orig = records
                    .next()
                    .ok_or_else(|| format!("rename without original path: {}", sanitize(record)))?;
                let score = record.split(|&b| b == b' ').nth(8);
                let kind = if score.is_some_and(|s| s.starts_with(b"C")) {
                    EntryKind::Copied
                } else {
                    EntryKind::Renamed
                };
                changed(xy, path, Some(RepoPath::from_bytes(orig)), kind)
            }
            Some(b'u') => {
                let (xy, path) = fields(record, 10).ok_or_else(malformed)?;
                changed(xy, path, None, EntryKind::Conflicted)
            }
            Some(b'?') => {
                let path = record.strip_prefix(b"? ").ok_or_else(malformed)?;
                changed(b"??", path, None, EntryKind::Untracked)
            }
            Some(b'#' | b'!') => continue,
            _ => return Err(format!("unknown status record: {}", sanitize(record))),
        };
        entries.push(entry?);
    }
    Ok(entries)
}

/// Split a record into its `XY` field and the path that follows `fixed` fields.
fn fields(record: &[u8], fixed: usize) -> Option<(&[u8], &[u8])> {
    let mut parts = record.splitn(fixed + 1, |&b| b == b' ');
    let xy = parts.nth(1);
    let path = parts.nth(fixed - 2);
    xy.zip(path)
}

fn changed(
    xy: &[u8],
    path: &[u8],
    orig_path: Option<RepoPath>,
    kind: EntryKind,
) -> Result<FileEntry, String> {
    let &[index, worktree] = xy else {
        return Err(format!("malformed status letters: {}", sanitize(xy)));
    };
    Ok(FileEntry {
        path: RepoPath::from_bytes(path),
        orig_path,
        index: char::from(index),
        worktree: char::from(worktree),
        kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::fake::{FakeRunner, fail, ok};

    const STATUS_ARGV: &[&str] = &["status", "--porcelain=v2", "-z", "--untracked-files=all"];

    fn entry(x: char, y: char, kind: EntryKind, path: &str, orig: Option<&str>) -> FileEntry {
        FileEntry {
            path: path.into(),
            orig_path: orig.map(Into::into),
            index: x,
            worktree: y,
            kind,
        }
    }

    fn letters_and_paths(entries: &[FileEntry]) -> Vec<(String, String, Option<String>)> {
        entries
            .iter()
            .map(|e| {
                let orig = e.orig_path.as_ref().map(|o| o.display().to_string());
                (e.status_letters(), e.path.display().to_string(), orig)
            })
            .collect()
    }

    #[test]
    fn status_command_is_argv_only_with_lock_free_c_locale_env() {
        let cmd = status_command();
        let args: Vec<_> = cmd.args().iter().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(args, STATUS_ARGV);
        let envs: Vec<_> = cmd
            .envs()
            .iter()
            .map(|(k, v)| (k.to_str().unwrap(), v.to_str().unwrap()))
            .collect();
        assert_eq!(envs, [("GIT_OPTIONAL_LOCKS", "0"), ("LC_ALL", "C")]);
    }

    #[test]
    fn read_status_runs_status_once_and_parses_output() {
        let runner = FakeRunner::new().expect(STATUS_ARGV, ok(b"? new_file\0"));
        let entries = read_status(&runner).unwrap();
        runner.assert_done();
        assert_eq!(
            entries,
            [entry('?', '?', EntryKind::Untracked, "new_file", None)]
        );
        let envs = runner.envs();
        assert_eq!(envs.len(), 1);
        assert!(
            envs[0].contains(&("GIT_OPTIONAL_LOCKS".into(), "0".into())),
            "{envs:?}"
        );
    }

    #[test]
    fn read_status_returns_stderr_on_failure() {
        let runner = FakeRunner::new().expect(
            STATUS_ARGV,
            fail("fatal: not a git repository (or any of the parent directories): .git\n"),
        );
        assert_eq!(
            read_status(&runner),
            Err("fatal: not a git repository (or any of the parent directories): .git".into())
        );
        runner.assert_done();
    }

    #[test]
    fn read_status_failure_without_stderr_still_has_a_message() {
        let runner = FakeRunner::new().expect(STATUS_ARGV, fail("  \n"));
        assert_eq!(read_status(&runner), Err("git status failed".into()));
    }

    #[test]
    fn parses_renames_and_staged_plus_modified() {
        let entries = parse_status(include_bytes!("fixtures/renames.bin")).unwrap();
        assert_eq!(
            entries,
            [
                entry('.', 'M', EntryKind::Changed, "keep.txt", None),
                entry('M', 'M', EntryKind::Changed, "mm.txt", None),
                entry('R', '.', EntryKind::Renamed, "new.txt", Some("old.txt")),
            ]
        );
        let letters: Vec<_> = entries.iter().map(FileEntry::status_letters).collect();
        assert_eq!(letters, [" M", "MM", "R "]);
    }

    #[test]
    fn parses_conflicts() {
        let entries = parse_status(include_bytes!("fixtures/conflicts.bin")).unwrap();
        assert_eq!(
            entries,
            [
                entry('A', 'A', EntryKind::Conflicted, "added.txt", None),
                entry('U', 'U', EntryKind::Conflicted, "both.txt", None),
            ]
        );
    }

    #[test]
    fn lists_every_untracked_file_individually() {
        let entries = parse_status(include_bytes!("fixtures/untracked.bin")).unwrap();
        assert_eq!(
            letters_and_paths(&entries),
            [
                ("??".into(), "dir/a.txt".into(), None),
                ("??".into(), "dir/sub/b.txt".into(), None),
                ("??".into(), "new_file".into(), None),
            ]
        );
    }

    #[test]
    fn keeps_unicode_and_space_containing_paths_intact() {
        let entries = parse_status(include_bytes!("fixtures/unicode.bin")).unwrap();
        assert_eq!(
            letters_and_paths(&entries),
            [
                ("M ".into(), "café.txt".into(), None),
                (
                    "R ".into(),
                    "renamed with spaces.txt".into(),
                    Some("file with spaces.txt".into())
                ),
                ("??".into(), "日本語 name.md".into(), None),
            ]
        );
    }

    #[test]
    fn skips_headers_and_ignored_entries() {
        let bytes = b"# branch.oid abc\0! ignored.log\0? kept\0";
        let entries = parse_status(bytes).unwrap();
        assert_eq!(
            letters_and_paths(&entries),
            [("??".into(), "kept".into(), None)]
        );
    }

    #[test]
    fn rename_without_original_path_is_an_error() {
        let bytes = b"2 R. N... 100644 100644 100644 aaa aaa R100 new.txt\0";
        assert!(parse_status(bytes).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn keeps_the_raw_bytes_of_names_that_are_not_utf8() {
        use std::os::unix::ffi::OsStrExt;
        let bytes = b"2 R. N... 100644 100644 100644 aaa aaa R100 new\xfe\0old\xff\0? x\xff\0";
        let entries = parse_status(bytes).unwrap();
        assert_eq!(entries[0].path.as_os_str().as_bytes(), b"new\xfe");
        let orig = entries[0].orig_path.as_ref().unwrap();
        assert_eq!(orig.as_os_str().as_bytes(), b"old\xff");
        assert_eq!(entries[1].path.as_os_str().as_bytes(), b"x\xff");
        assert_eq!(entries[1].path.display(), "x\u{FFFD}");
    }
}
