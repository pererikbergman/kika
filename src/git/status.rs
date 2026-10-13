use super::bisect::{BisectState, read_bisect};
use super::path::RepoPath;
use super::repo_state::{Operation, git_dir_command, git_dir_from, in_progress};
use super::runner::{GitCommand, GitRunner};
use super::safe_text::sanitize;

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

/// Where `HEAD` points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Head {
    Branch(String),
    /// Not on a branch; holds the full commit id.
    Detached(String),
    /// On a branch that has no commits yet; holds its name.
    Unborn(String),
}

/// The branch side of `git status`: where `HEAD` is, how it relates to its
/// upstream, and what operation is half done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchStatus {
    pub head: Head,
    /// `None` when no upstream is configured.
    pub upstream: Option<String>,
    /// Commits ahead of and behind the upstream. `None` with an upstream
    /// means git could not find it: the upstream branch is gone.
    pub ahead_behind: Option<(u32, u32)>,
    pub in_progress: Vec<Operation>,
    /// The bisect's marks and outcome, read only while one is in progress.
    /// A linked worktree keeps its own, as git does.
    pub bisect: Option<BisectState>,
}

/// One status load: the branch and the changed files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoStatus {
    pub branch: BranchStatus,
    pub files: Vec<FileEntry>,
}

/// `git status` in the machine-readable form we parse. No optional locks
/// stops a background refresh from taking `index.lock` and racing the user.
/// It takes no pathspec, so literal pathspecs change nothing (a real-git
/// test pins that).
pub fn status_command() -> GitCommand {
    GitCommand::new("status")
        .arg("--porcelain=v2")
        .arg("--branch")
        .arg("-z")
        .arg("--untracked-files=all")
        .standard_env()
}

/// Run `git status`, then find the git dir and read what is in progress
/// there. Failures carry git's own message.
pub fn read_status(runner: &dyn GitRunner) -> Result<RepoStatus, String> {
    let mut status = parse_status(&stdout_of(runner, &status_command(), "git status failed")?)?;
    let git_dir = stdout_of(runner, &git_dir_command(), "git rev-parse failed")?;
    let git_dir = git_dir_from(&git_dir);
    status.branch.in_progress = in_progress(&git_dir);
    if status.branch.in_progress.contains(&Operation::Bisect) {
        status.branch.bisect = read_bisect(&git_dir);
    }
    Ok(status)
}

/// The stdout of `cmd`, or git's stderr (else `fallback`) when it fails.
fn stdout_of(runner: &dyn GitRunner, cmd: &GitCommand, fallback: &str) -> Result<Vec<u8>, String> {
    runner
        .run_checked(cmd)
        .map(|output| output.stdout)
        .map_err(|e| e.message_or(fallback))
}

/// Parse `git status --porcelain=v2 --branch -z`. Paths are taken as
/// everything after the fixed fields, so spaces inside them survive, and are
/// kept as raw bytes so a name that is not UTF-8 still reaches git unchanged.
/// `in_progress` is left empty: status does not report it. Conflicted
/// files come first, since they block the operation in progress; each
/// group keeps git's order.
pub fn parse_status(bytes: &[u8]) -> Result<RepoStatus, String> {
    let mut entries = Vec::new();
    let mut headers = BranchHeaders::default();
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
            Some(b'#') => {
                headers.read(record)?;
                continue;
            }
            Some(b'!') => continue,
            _ => return Err(format!("unknown status record: {}", sanitize(record))),
        };
        entries.push(entry?);
    }
    // Stable, so git's order holds within each group.
    entries.sort_by_key(|e| e.kind != EntryKind::Conflicted);
    Ok(RepoStatus {
        branch: headers.finish(),
        files: entries,
    })
}

/// The `# branch.*` records seen so far. Unknown headers are skipped, so a
/// newer git that adds some still parses.
#[derive(Default)]
struct BranchHeaders {
    oid: Option<String>,
    head: Option<String>,
    upstream: Option<String>,
    ahead_behind: Option<(u32, u32)>,
}

impl BranchHeaders {
    fn read(&mut self, record: &[u8]) -> Result<(), String> {
        let Some(header) = record.strip_prefix(b"# ") else {
            return Ok(());
        };
        let (key, value) = match header.iter().position(|&b| b == b' ') {
            Some(i) => (&header[..i], &header[i + 1..]),
            None => (header, &b""[..]),
        };
        match key {
            b"branch.oid" => self.oid = Some(sanitize(value)),
            b"branch.head" => self.head = Some(sanitize(value)),
            b"branch.upstream" => self.upstream = Some(sanitize(value)),
            b"branch.ab" => {
                let counts = ahead_behind(value)
                    .ok_or_else(|| format!("malformed status record: {}", sanitize(record)))?;
                self.ahead_behind = Some(counts);
            }
            _ => {}
        }
        Ok(())
    }

    /// Without headers (status run without `--branch`) the head reads as
    /// detached at an unknown commit, the least specific claim.
    fn finish(self) -> BranchStatus {
        let oid = self.oid.unwrap_or_default();
        let head = match self.head {
            Some(head) if head == "(detached)" => Head::Detached(oid),
            Some(name) if oid == "(initial)" => Head::Unborn(name),
            Some(name) => Head::Branch(name),
            None => Head::Detached(oid),
        };
        BranchStatus {
            head,
            upstream: self.upstream,
            ahead_behind: self.ahead_behind,
            in_progress: Vec::new(),
            bisect: None,
        }
    }
}

/// `+A -B` as `(A, B)`.
fn ahead_behind(value: &[u8]) -> Option<(u32, u32)> {
    let text = std::str::from_utf8(value).ok()?;
    let (ahead, behind) = text.split_once(' ')?;
    let ahead = ahead.strip_prefix('+')?.parse().ok()?;
    let behind = behind.strip_prefix('-')?.parse().ok()?;
    Some((ahead, behind))
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
    use crate::git::fake::{FakeRunner, TempDir, fail, ok};
    use crate::git::runner::EnvProfile;
    use crate::test_support::{STANDARD_ENV, env_list};

    const STATUS_ARGV: &[&str] = &[
        "status",
        "--porcelain=v2",
        "--branch",
        "-z",
        "--untracked-files=all",
    ];

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
        assert_eq!(cmd.env_profile(), Some(EnvProfile::Standard));
        assert_eq!(cmd.process_env(), env_list(&STANDARD_ENV));
    }

    /// Literal pathspecs must change nothing for a status, which takes no
    /// pathspec but walks the tree: the same files, with or without them,
    /// even one literally named `*`.
    #[cfg(unix)]
    #[test]
    fn the_standard_profile_changes_nothing_for_a_real_status() {
        use crate::git::CliRunner;
        use crate::git::fake::git;

        let tmp = TempDir::new("status-profile");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/new.txt"), "new\n").unwrap();
        std::fs::write(dir.join("staged.txt"), "staged\n").unwrap();
        std::fs::write(dir.join("*"), "star\n").unwrap();
        git(dir, &["add", "--", "staged.txt"]);
        let runner = CliRunner::isolated(dir).with_env("GIT_LITERAL_PATHSPECS", "0");
        let user_env = GitCommand::new("status")
            .arg("--porcelain=v2")
            .arg("--branch")
            .arg("-z")
            .arg("--untracked-files=all")
            .user_env();

        let standard = runner.run(&status_command()).unwrap();
        let plain = runner.run(&user_env).unwrap();

        assert!(
            standard.success,
            "{}",
            String::from_utf8_lossy(&standard.stderr)
        );
        assert_eq!(standard.stdout, plain.stdout);
        let files = parse_status(&standard.stdout).unwrap().files;
        let names: Vec<_> = files.iter().map(|f| (f.index, f.path.display())).collect();
        assert_eq!(
            names,
            [('A', "staged.txt"), ('?', "*"), ('?', "sub/new.txt"),]
        );
    }

    const GIT_DIR_ARGV: &[&str] = &["rev-parse", "--absolute-git-dir"];

    #[test]
    fn read_status_runs_status_then_rev_parse_and_parses_output() {
        let tmp = TempDir::new("read-status");
        std::fs::write(tmp.path().join("MERGE_HEAD"), "x\n").unwrap();
        let mut git_dir = tmp.path().as_os_str().as_encoded_bytes().to_vec();
        git_dir.push(b'\n');
        let runner = FakeRunner::new()
            .expect(
                STATUS_ARGV,
                ok(b"# branch.oid 1234567\0# branch.head main\0? new_file\0"),
            )
            .expect(GIT_DIR_ARGV, ok(&git_dir));
        let status = read_status(&runner).unwrap();
        runner.assert_done();
        assert_eq!(
            status.files,
            [entry('?', '?', EntryKind::Untracked, "new_file", None)]
        );
        assert_eq!(status.branch.head, Head::Branch("main".into()));
        assert_eq!(status.branch.in_progress, [Operation::Merge]);
        assert_eq!(runner.process_envs(), vec![env_list(&STANDARD_ENV); 2]);
    }

    #[test]
    fn read_status_reads_the_bisect_only_while_one_is_in_progress() {
        let tmp = TempDir::new("read-status-bisect");
        let mut git_dir = tmp.path().as_os_str().as_encoded_bytes().to_vec();
        git_dir.push(b'\n');
        let load = || {
            let runner = FakeRunner::new()
                .expect(
                    STATUS_ARGV,
                    ok(b"# branch.oid 1234567\0# branch.head (detached)\0"),
                )
                .expect(GIT_DIR_ARGV, ok(&git_dir));
            read_status(&runner).unwrap().branch
        };
        let branch = load();
        assert_eq!(branch.in_progress, []);
        assert_eq!(branch.bisect, None);

        std::fs::write(tmp.path().join("BISECT_LOG"), "git bisect bad abc\n").unwrap();
        std::fs::write(tmp.path().join("BISECT_START"), "main\n").unwrap();
        let branch = load();
        assert_eq!(branch.in_progress, [Operation::Bisect]);
        let bisect = branch.bisect.unwrap();
        assert_eq!((bisect.bad, bisect.start.as_str()), (1, "main"));
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
    fn a_failing_rev_parse_fails_the_load_with_its_stderr() {
        let runner = FakeRunner::new()
            .expect(STATUS_ARGV, ok(b"# branch.head main\0"))
            .expect(GIT_DIR_ARGV, fail("fatal: rev-parse broke\n"));
        assert_eq!(read_status(&runner), Err("fatal: rev-parse broke".into()));
        runner.assert_done();

        let runner = FakeRunner::new()
            .expect(STATUS_ARGV, ok(b""))
            .expect(GIT_DIR_ARGV, fail(""));
        assert_eq!(read_status(&runner), Err("git rev-parse failed".into()));
    }

    #[test]
    fn parses_renames_and_staged_plus_modified() {
        let entries = parse_status(include_bytes!("fixtures/renames.bin"))
            .unwrap()
            .files;
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
        let entries = parse_status(include_bytes!("fixtures/conflicts.bin"))
            .unwrap()
            .files;
        assert_eq!(
            entries,
            [
                entry('A', 'A', EntryKind::Conflicted, "added.txt", None),
                entry('U', 'U', EntryKind::Conflicted, "both.txt", None),
            ]
        );
    }

    #[test]
    fn conflicts_come_first_in_their_own_order() {
        let mut bytes = b"1 .M N... 100644 100644 100644 0000000000000000000000000000000000000000 0000000000000000000000000000000000000000 a.txt\0".to_vec();
        bytes.extend_from_slice(include_bytes!("fixtures/conflicts.bin"));
        bytes.extend_from_slice(b"? z.txt\0");
        let entries = parse_status(&bytes).unwrap().files;
        assert_eq!(
            entries,
            [
                entry('A', 'A', EntryKind::Conflicted, "added.txt", None),
                entry('U', 'U', EntryKind::Conflicted, "both.txt", None),
                entry('.', 'M', EntryKind::Changed, "a.txt", None),
                entry('?', '?', EntryKind::Untracked, "z.txt", None),
            ]
        );
    }

    #[test]
    fn lists_every_untracked_file_individually() {
        let entries = parse_status(include_bytes!("fixtures/untracked.bin"))
            .unwrap()
            .files;
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
        let entries = parse_status(include_bytes!("fixtures/unicode.bin"))
            .unwrap()
            .files;
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
        let entries = parse_status(bytes).unwrap().files;
        assert_eq!(
            letters_and_paths(&entries),
            [("??".into(), "kept".into(), None)]
        );
    }

    fn branch_of(bytes: &[u8]) -> BranchStatus {
        parse_status(bytes).unwrap().branch
    }

    #[test]
    fn reads_the_branch_its_upstream_and_ahead_behind() {
        let branch = branch_of(
            b"# branch.oid 1234567890abcdef\0# branch.head main\0\
              # branch.upstream origin/main\0# branch.ab +2 -1\0? kept\0",
        );
        assert_eq!(branch.head, Head::Branch("main".into()));
        assert_eq!(branch.upstream.as_deref(), Some("origin/main"));
        assert_eq!(branch.ahead_behind, Some((2, 1)));
        assert!(branch.in_progress.is_empty());
    }

    #[test]
    fn a_branch_without_upstream_has_none() {
        let branch = branch_of(b"# branch.oid 1234567\0# branch.head topic\0");
        assert_eq!(branch.head, Head::Branch("topic".into()));
        assert_eq!(branch.upstream, None);
        assert_eq!(branch.ahead_behind, None);
    }

    #[test]
    fn a_detached_head_keeps_its_oid() {
        let branch = branch_of(b"# branch.oid abcdef1234567890\0# branch.head (detached)\0");
        assert_eq!(branch.head, Head::Detached("abcdef1234567890".into()));
    }

    #[test]
    fn an_initial_oid_is_an_unborn_branch() {
        let branch = branch_of(b"# branch.oid (initial)\0# branch.head main\0");
        assert_eq!(branch.head, Head::Unborn("main".into()));
    }

    #[test]
    fn an_upstream_without_counts_is_gone() {
        let branch =
            branch_of(b"# branch.oid 1234567\0# branch.head main\0# branch.upstream origin/old\0");
        assert_eq!(branch.upstream.as_deref(), Some("origin/old"));
        assert_eq!(branch.ahead_behind, None);
    }

    #[test]
    fn branch_names_are_made_terminal_safe() {
        let branch = branch_of(
            b"# branch.oid 1234567\0# branch.head evil\x1b[2J\0# branch.upstream o/\x07x\0",
        );
        assert_eq!(branch.head, Head::Branch("evil\u{FFFD}[2J".into()));
        assert_eq!(branch.upstream.as_deref(), Some("o/\u{FFFD}x"));
    }

    #[test]
    fn branch_names_show_bidi_escapes() {
        let branch = branch_of(
            "# branch.oid 1234567\0# branch.head evil\u{202E}gnp\0# branch.upstream o/a\u{200B}b\0"
                .as_bytes(),
        );
        assert_eq!(branch.head, Head::Branch("evil<U+202E>gnp".into()));
        assert_eq!(branch.upstream.as_deref(), Some("o/a<U+200B>b"));
    }

    #[test]
    fn malformed_ahead_behind_is_an_error() {
        assert!(parse_status(b"# branch.ab +x -1\0").is_err());
        assert!(parse_status(b"# branch.ab +1\0").is_err());
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
        let entries = parse_status(bytes).unwrap().files;
        assert_eq!(entries[0].path.as_os_str().as_bytes(), b"new\xfe");
        let orig = entries[0].orig_path.as_ref().unwrap();
        assert_eq!(orig.as_os_str().as_bytes(), b"old\xff");
        assert_eq!(entries[1].path.as_os_str().as_bytes(), b"x\xff");
        assert_eq!(entries[1].path.display(), "x\u{FFFD}");
    }
}
