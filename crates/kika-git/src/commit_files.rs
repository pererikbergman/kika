//! The files one commit changed, against its first parent, so the Commits
//! panel can show a commit file by file.

use super::log::CommitRow;
use super::path::RepoPath;
use super::runner::{GitCommand, GitRunner};
use super::safe_text::terminal_safe;

/// An object id as hex digits only. It is the only way a commit id reaches
/// argv, so nothing git could read as an option gets through, without the
/// `--end-of-options` that older git lacks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitId(String);

impl CommitId {
    pub fn parse(text: &str) -> Option<Self> {
        (!text.is_empty() && text.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| Self(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A commit and the first parent it is shown against; `None` for a root
/// commit. A merge is shown against its first parent, the side the log
/// follows first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRange {
    pub oid: CommitId,
    pub parent: Option<CommitId>,
}

impl CommitRange {
    /// The range of a log row. Any id that is not hex refuses the whole
    /// range, rather than showing the commit against the wrong tree.
    pub fn of(row: &CommitRow) -> Result<Self, String> {
        let id = |text: &str| {
            CommitId::parse(text)
                .ok_or_else(|| format!("not a commit id: {}", terminal_safe(text.as_bytes())))
        };
        Ok(Self {
            oid: id(&row.oid)?,
            parent: row.parents.first().map(|p| id(p)).transpose()?,
        })
    }

    /// The tree-ish arguments: `<parent> <oid>`, or `<oid>` alone for a
    /// root commit, which `--root` diffs against the empty tree.
    pub(crate) fn args(&self) -> impl Iterator<Item = &str> {
        self.parent
            .iter()
            .chain(std::iter::once(&self.oid))
            .map(CommitId::as_str)
    }
}

/// One changed file: git's status letter (`A`, `M`, `D`, `T`, `R`, `C`, …)
/// and its name; a rename or copy also has the name it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitFile {
    pub status: char,
    pub path: RepoPath,
    pub orig_path: Option<RepoPath>,
}

/// `git diff-tree` listing the files `range` changed, NUL-separated so any
/// name survives. A read-only query: no lock, and git's text in English.
pub fn files_command(range: &CommitRange) -> GitCommand {
    let cmd = GitCommand::new("diff-tree")
        .arg("-r")
        .arg("-z")
        .arg("--no-commit-id")
        .arg("--name-status")
        .arg("-M")
        .arg("--root");
    range
        .args()
        .fold(cmd, |cmd, arg| cmd.arg(arg))
        .arg("--")
        .standard_env()
}

/// The files `range` changed, in git's order. Errors are made safe to
/// draw, since git's message can quote repo-controlled names.
pub fn commit_files(
    runner: &dyn GitRunner,
    range: &CommitRange,
) -> Result<Vec<CommitFile>, String> {
    let output = runner
        .run_checked(&files_command(range))
        .map_err(|e| e.message_or("git diff-tree failed"))?;
    parse(&output.stdout)
}

/// `git diff-tree` counting the lines `range` added and removed in each
/// file, renames found, NUL-separated so any name survives. `range` has a
/// parent: a pull request is compared with its merge base.
pub fn numstat_command(range: &CommitRange) -> GitCommand {
    let cmd = GitCommand::new("diff-tree")
        .arg("-r")
        .arg("-z")
        .arg("--numstat")
        .arg("-M")
        .arg("--no-commit-id");
    range
        .args()
        .fold(cmd, |cmd, arg| cmd.arg(arg))
        .arg("--")
        .standard_env()
}

/// Lines added and removed per file, by its (new) name; `None` for a
/// binary file, which has no lines.
pub type LineCounts = Option<(u32, u32)>;

/// Each file `range` changed with its counts, in git's order.
pub fn numstat(
    runner: &dyn GitRunner,
    range: &CommitRange,
) -> Result<Vec<(RepoPath, LineCounts)>, String> {
    let output = runner
        .run_checked(&numstat_command(range))
        .map_err(|e| e.message_or("git diff-tree failed"))?;
    parse_numstat(&output.stdout)
}

/// `-z --numstat` records: `added TAB removed TAB name NUL`, or for a
/// rename or copy an empty name and then the old and new names, each
/// ending in NUL. A binary file counts `-` both ways.
fn parse_numstat(stdout: &[u8]) -> Result<Vec<(RepoPath, LineCounts)>, String> {
    let bad = || "git diff-tree gave a record Kika could not read".to_string();
    let mut fields = stdout.split(|&b| b == 0);
    let mut stats = Vec::new();
    while let Some(record) = fields.next().filter(|r| !r.is_empty()) {
        let mut parts = record.splitn(3, |&b| b == b'\t');
        let (Some(added), Some(removed), Some(name)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(bad());
        };
        let counts = match (added, removed) {
            (b"-", b"-") => None,
            _ => Some((
                count(added).ok_or_else(bad)?,
                count(removed).ok_or_else(bad)?,
            )),
        };
        let path = if name.is_empty() {
            // The old name, then the new one, which the row is known by.
            fields.next().filter(|n| !n.is_empty()).ok_or_else(bad)?;
            fields.next().filter(|n| !n.is_empty()).ok_or_else(bad)?
        } else {
            name
        };
        stats.push((RepoPath::from_bytes(path), counts));
    }
    Ok(stats)
}

/// A count of lines in ASCII digits, held at `u32::MAX`.
fn count(digits: &[u8]) -> Option<u32> {
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(
        std::str::from_utf8(digits)
            .ok()?
            .parse::<u64>()
            .map_or(u32::MAX, |n| u32::try_from(n).unwrap_or(u32::MAX)),
    )
}

/// `-z --name-status` records: a status field, then one name, or two for
/// a rename or copy (the old one first). Only the status's letter is kept,
/// not its score (`R100` → `R`).
fn parse(stdout: &[u8]) -> Result<Vec<CommitFile>, String> {
    let mut fields = stdout.split(|&b| b == 0);
    let mut files = Vec::new();
    while let Some(status) = fields.next().filter(|s| !s.is_empty()) {
        // A letter, as git gives; anything else is never drawn as it is.
        let letter = match status[0] {
            b if b.is_ascii_alphabetic() => char::from(b),
            _ => '?',
        };
        let mut name = || {
            fields
                .next()
                .filter(|name| !name.is_empty())
                .map(RepoPath::from_bytes)
                .ok_or_else(|| "git diff-tree gave a record Kika could not read".to_string())
        };
        let (path, orig_path) = if matches!(letter, 'R' | 'C') {
            let orig = name()?;
            (name()?, Some(orig))
        } else {
            (name()?, None)
        };
        files.push(CommitFile {
            status: letter,
            path,
            orig_path,
        });
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use crate::fake::{STANDARD_ENV, env_list};
    use crate::runner::EnvProfile;

    use super::*;
    use crate::fake::{FakeRunner, fail, ok};

    const OID: &str = "a1b2c3";
    const PARENT: &str = "d4e5f6";

    fn range(parent: Option<&str>) -> CommitRange {
        CommitRange {
            oid: CommitId::parse(OID).unwrap(),
            parent: parent.map(|p| CommitId::parse(p).unwrap()),
        }
    }

    fn row(oid: &str, parents: &[&str]) -> CommitRow {
        CommitRow {
            oid: oid.into(),
            short_oid: oid.into(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
            author: String::new(),
            time: 0,
            subject: String::new(),
            subject_altered: false,
        }
    }

    const FLAGS: [&str; 7] = [
        "diff-tree",
        "-r",
        "-z",
        "--no-commit-id",
        "--name-status",
        "-M",
        "--root",
    ];

    fn argv(tail: &[&'static str]) -> Vec<&'static str> {
        [&FLAGS[..], tail].concat()
    }

    #[test]
    fn a_commit_id_is_non_empty_hex_only() {
        assert_eq!(CommitId::parse("0aF9").unwrap().as_str(), "0aF9");
        for bad in ["", "--output=x", "HEAD", "abc def", "abc\u{1b}", "-1"] {
            assert_eq!(CommitId::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_range_takes_the_first_parent_and_none_for_a_root() {
        let merge = CommitRange::of(&row(OID, &[PARENT, "fff"])).unwrap();
        assert_eq!(merge, range(Some(PARENT)));
        assert_eq!(CommitRange::of(&row(OID, &[])).unwrap(), range(None));
    }

    #[test]
    fn a_range_with_any_id_that_is_not_hex_fails_safely() {
        for (oid, parents) in [("--x", &[][..]), (OID, &["-p"][..]), ("", &[][..])] {
            assert!(CommitRange::of(&row(oid, parents)).is_err(), "{oid:?}");
        }
        assert_eq!(
            CommitRange::of(&row("a\u{1b}[2J", &[])),
            Err("not a commit id: a\u{FFFD}[2J".into())
        );
    }

    #[test]
    fn with_a_parent_both_trees_are_given_and_without_only_the_commit() {
        let fake = FakeRunner::new()
            .expect(&argv(&[PARENT, OID, "--"]), ok(b""))
            .expect(&argv(&[OID, "--"]), ok(b""));
        assert_eq!(commit_files(&fake, &range(Some(PARENT))), Ok(vec![]));
        assert_eq!(commit_files(&fake, &range(None)), Ok(vec![]));
        fake.assert_done();
        assert_eq!(fake.process_envs(), vec![env_list(&STANDARD_ENV); 2]);
        assert_eq!(
            files_command(&range(None)).env_profile(),
            Some(EnvProfile::Standard)
        );
    }

    fn file(status: char, path: &str, orig: Option<&str>) -> CommitFile {
        CommitFile {
            status,
            path: path.into(),
            orig_path: orig.map(Into::into),
        }
    }

    #[test]
    fn every_status_is_read_with_renames_and_copies_carrying_two_names() {
        let out = b"A\0new.rs\0M\0dir/mod.rs\0D\0gone\0T\0link\0\
                    R100\0old name\0new name\0C075\0src\0copy\0";
        let fake = FakeRunner::new().expect(&argv(&[OID, "--"]), ok(out));
        assert_eq!(
            commit_files(&fake, &range(None)).unwrap(),
            [
                file('A', "new.rs", None),
                file('M', "dir/mod.rs", None),
                file('D', "gone", None),
                file('T', "link", None),
                file('R', "new name", Some("old name")),
                file('C', "copy", Some("src")),
            ]
        );
    }

    #[test]
    fn a_name_with_a_newline_is_one_name_and_never_drawn_as_a_line_break() {
        let fake = FakeRunner::new().expect(&argv(&[OID, "--"]), ok(b"M\0two\nlines\0A\0b\0"));
        let files = commit_files(&fake, &range(None)).unwrap();
        assert_eq!(files[0].path, RepoPath::from_bytes(b"two\nlines"));
        assert_eq!(files[1].path, RepoPath::from("b"));
        assert!(!files[0].path.display().contains('\n'));
    }

    /// Only unix names can hold bytes that are not UTF-8; elsewhere a
    /// `RepoPath` is decoded lossily by design.
    #[cfg(unix)]
    #[test]
    fn a_name_that_is_not_utf8_keeps_its_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let fake = FakeRunner::new().expect(&argv(&[OID, "--"]), ok(b"A\0x\xff\0"));
        let files = commit_files(&fake, &range(None)).unwrap();
        assert_eq!(files[0].path.as_os_str().as_bytes(), b"x\xff");
        assert_ne!(files[0].path, RepoPath::from("x\u{FFFD}"));
    }

    #[test]
    fn a_cut_short_record_is_an_error_not_a_guess() {
        let fake = FakeRunner::new().expect(&argv(&[OID, "--"]), ok(b"R100\0only-one\0"));
        assert!(commit_files(&fake, &range(None)).is_err());
    }

    #[test]
    fn a_failure_says_why_safely() {
        let fake = FakeRunner::new()
            .expect(&argv(&[OID, "--"]), fail("fatal: bad \x1b[2J object\n"))
            .expect(&argv(&[OID, "--"]), fail(""));
        assert_eq!(
            commit_files(&fake, &range(None)),
            Err("fatal: bad \u{FFFD}[2J object".into())
        );
        assert_eq!(
            commit_files(&fake, &range(None)),
            Err("git diff-tree failed".into())
        );
    }

    const NUMSTAT: [&str; 6] = ["diff-tree", "-r", "-z", "--numstat", "-M", "--no-commit-id"];

    #[test]
    fn numstat_compares_two_fixed_trees_with_no_room_for_an_option() {
        let fake =
            FakeRunner::new().expect(&[&NUMSTAT[..], &[PARENT, OID, "--"]].concat(), ok(b""));
        assert_eq!(numstat(&fake, &range(Some(PARENT))), Ok(vec![]));
        fake.assert_done();
        assert_eq!(fake.process_envs(), [env_list(&STANDARD_ENV)]);
        assert_eq!(
            numstat_command(&range(Some(PARENT))).env_profile(),
            Some(EnvProfile::Standard)
        );
    }

    #[test]
    fn numstat_reads_counts_renames_and_binary_files() {
        let out = b"3\t1\tsrc/a.rs\0-\t-\tlogo.png\0\
                    0\t0\t\0old name\0new name\0\
                    12\t0\tdir/two\nlines\0";
        let fake =
            FakeRunner::new().expect(&[&NUMSTAT[..], &[PARENT, OID, "--"]].concat(), ok(out));
        assert_eq!(
            numstat(&fake, &range(Some(PARENT))).unwrap(),
            [
                (RepoPath::from("src/a.rs"), Some((3, 1))),
                (RepoPath::from("logo.png"), None),
                (RepoPath::from("new name"), Some((0, 0))),
                (RepoPath::from_bytes(b"dir/two\nlines"), Some((12, 0))),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn numstat_keeps_a_name_that_is_not_utf8() {
        use std::os::unix::ffi::OsStrExt;
        let fake = FakeRunner::new().expect(
            &[&NUMSTAT[..], &[PARENT, OID, "--"]].concat(),
            ok(b"1\t2\tx\xff\0"),
        );
        let stats = numstat(&fake, &range(Some(PARENT))).unwrap();
        assert_eq!(stats[0].0.as_os_str().as_bytes(), b"x\xff");
        assert_eq!(stats[0].1, Some((1, 2)));
    }

    #[test]
    fn a_numstat_record_cut_short_or_garbled_is_an_error() {
        for out in [&b"3\t1\t\0only-one\0"[..], b"x\ty\tpath\0", b"3\t1\0"] {
            let fake =
                FakeRunner::new().expect(&[&NUMSTAT[..], &[PARENT, OID, "--"]].concat(), ok(out));
            assert!(numstat(&fake, &range(Some(PARENT))).is_err(), "{out:?}");
        }
    }

    #[test]
    fn a_real_history_lists_adds_edits_renames_deletes_roots_and_merges() {
        use crate::CliRunner;
        use crate::fake::{TempDir, git};

        let tmp = TempDir::new("commit-files");
        let dir = tmp.path();
        let rev = |name: &str| {
            String::from_utf8(git(dir, &["rev-parse", name]))
                .unwrap()
                .trim()
                .to_string()
        };
        let commit = |message: &str| {
            git(dir, &["add", "-A"]);
            git(dir, &["commit", "-q", "-m", message]);
        };
        git(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("keep.txt"), "keep\n").unwrap();
        std::fs::write(dir.join("gone.txt"), "gone\n").unwrap();
        std::fs::write(dir.join("move.txt"), "a\nb\nc\nd\ne\n").unwrap();
        commit("root");
        let root = rev("HEAD");
        std::fs::write(dir.join("keep.txt"), "kept\n").unwrap();
        std::fs::remove_file(dir.join("gone.txt")).unwrap();
        std::fs::rename(dir.join("move.txt"), dir.join("moved.txt")).unwrap();
        std::fs::write(dir.join("added.txt"), "new\n").unwrap();
        commit("changes");
        let changes = rev("HEAD");
        git(dir, &["checkout", "-q", "-b", "side"]);
        std::fs::write(dir.join("side.txt"), "side\n").unwrap();
        commit("on side");
        git(dir, &["checkout", "-q", "main"]);
        std::fs::write(dir.join("keep.txt"), "main\n").unwrap();
        commit("on main");
        git(dir, &["merge", "-q", "--no-ff", "-m", "merge", "side"]);
        let runner = CliRunner::isolated(dir);
        let range_of = |oid: &str, parent: Option<&str>| CommitRange {
            oid: CommitId::parse(oid).unwrap(),
            parent: parent.map(|p| CommitId::parse(p).unwrap()),
        };

        let listed = commit_files(&runner, &range_of(&root, None)).unwrap();
        let names: Vec<_> = listed
            .iter()
            .map(|f| (f.status, f.path.display()))
            .collect();
        assert_eq!(
            names,
            [('A', "gone.txt"), ('A', "keep.txt"), ('A', "move.txt")]
        );

        let listed = commit_files(&runner, &range_of(&changes, Some(&root))).unwrap();
        assert_eq!(
            listed,
            [
                file('A', "added.txt", None),
                file('D', "gone.txt", None),
                file('M', "keep.txt", None),
                file('R', "moved.txt", Some("move.txt")),
            ]
        );

        // Against the first parent, a merge brings in only the side's work.
        let merge = rev("HEAD");
        let listed = commit_files(&runner, &range_of(&merge, Some(&rev("HEAD^1")))).unwrap();
        assert_eq!(listed, [file('A', "side.txt", None)]);
    }
}
