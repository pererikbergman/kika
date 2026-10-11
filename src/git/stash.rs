//! The stash list and the files of one entry, so the Stash panel can show
//! work put aside the way the Commits panel shows a commit.

use super::commit_files::{CommitFile, CommitId, CommitRange, commit_files};
use super::diff::{sanitize, terminal_safe};
use super::runner::{GitCommand, GitRunner};

/// One entry of `git stash list`. A stash is a commit whose first parent
/// is `HEAD` when it was made and whose third, if any, holds the untracked
/// files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashEntry {
    /// Its place in the list: `stash@{index}`.
    pub index: usize,
    pub oid: CommitId,
    pub base: CommitId,
    pub untracked: Option<CommitId>,
    /// The reflog subject, safe to draw on one row.
    pub subject: String,
}

impl StashEntry {
    pub fn stash_ref(&self) -> StashRef {
        StashRef {
            index: self.index,
            oid: self.oid.clone(),
        }
    }
}

/// An entry as an action names it: its place in the list, and the commit
/// that was there when the action was chosen, so a list that changed since
/// is caught before anything runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashRef {
    pub index: usize,
    pub oid: CommitId,
}

impl StashRef {
    /// `stash@{N}`, the only way a stash reaches argv. Built from a list
    /// index, never from typed text.
    pub fn arg(&self) -> String {
        format!("stash@{{{}}}", self.index)
    }
}

/// One file of a stash entry and the range its diff is read from: the
/// stash against its base for tracked files, the untracked commit alone
/// for untracked ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashFile {
    pub range: CommitRange,
    pub file: CommitFile,
}

/// `git stash list`, one NUL-terminated record per entry: the id, the
/// parents and the subject, split by 0x1f. A read-only query: no lock, and
/// git's text in English.
pub fn list_command() -> GitCommand {
    GitCommand::new("stash")
        .arg("list")
        .arg("-z")
        .arg("--format=%H%x1f%P%x1f%gs")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
}

/// The stash list, newest first. No stashes is an empty list.
pub fn list_stashes(runner: &dyn GitRunner) -> Result<Vec<StashEntry>, String> {
    let output = runner
        .run(&list_command())
        .map_err(|e| terminal_safe(e.to_string().as_bytes()))?;
    if !output.success {
        let message = terminal_safe(&output.stderr);
        return Err(if message.is_empty() {
            "git stash list failed".into()
        } else {
            message
        });
    }
    parse_list(&output.stdout)
}

/// Any record Kika cannot read refuses the whole list, since every action
/// names an entry by its place in it.
fn parse_list(stdout: &[u8]) -> Result<Vec<StashEntry>, String> {
    let body = stdout.strip_suffix(b"\0").unwrap_or(stdout);
    if body.is_empty() {
        return Ok(Vec::new());
    }
    body.split(|&b| b == 0)
        .enumerate()
        .map(|(index, record)| parse_record(index, record))
        .collect()
}

const UNREADABLE: &str = "git stash list gave an entry Kika could not read";

fn parse_record(index: usize, record: &[u8]) -> Result<StashEntry, String> {
    // Only the first two separators split: the subject may hold anything.
    let mut fields = record.splitn(3, |&b| b == 0x1f);
    let (Some(oid), Some(parents), Some(subject)) = (fields.next(), fields.next(), fields.next())
    else {
        return Err(UNREADABLE.into());
    };
    let id = |bytes: &[u8]| {
        std::str::from_utf8(bytes)
            .ok()
            .and_then(CommitId::parse)
            .ok_or_else(|| UNREADABLE.to_string())
    };
    let parents = parents
        .split(|&b| b == b' ')
        .filter(|p| !p.is_empty())
        .map(id)
        .collect::<Result<Vec<_>, _>>()?;
    let mut parents = parents.into_iter();
    let base = parents.next().ok_or_else(|| UNREADABLE.to_string())?;
    let untracked = parents.nth(1);
    Ok(StashEntry {
        index,
        oid: id(oid)?,
        base,
        untracked,
        subject: sanitize(subject),
    })
}

/// The files `entry` holds: its tracked changes against its base, then
/// its untracked files, which show as added.
pub fn stash_files(runner: &dyn GitRunner, entry: &StashEntry) -> Result<Vec<StashFile>, String> {
    let tracked = CommitRange {
        oid: entry.oid.clone(),
        parent: Some(entry.base.clone()),
    };
    let untracked = entry.untracked.as_ref().map(|oid| CommitRange {
        oid: oid.clone(),
        parent: None,
    });
    let mut files = Vec::new();
    for range in std::iter::once(tracked).chain(untracked) {
        let listed = commit_files(runner, &range)?;
        files.extend(listed.into_iter().map(|file| StashFile {
            range: range.clone(),
            file,
        }));
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;
    use crate::git::fake::{FakeRunner, fail, ok};

    const LIST: [&str; 4] = ["stash", "list", "-z", "--format=%H%x1f%P%x1f%gs"];

    fn oid(text: &str) -> CommitId {
        CommitId::parse(text).unwrap()
    }

    fn listed(stdout: &[u8]) -> Result<Vec<StashEntry>, String> {
        let fake = FakeRunner::new().expect(&LIST, ok(stdout));
        let result = list_stashes(&fake);
        fake.assert_done();
        let env: Vec<(OsString, OsString)> = vec![
            ("GIT_OPTIONAL_LOCKS".into(), "0".into()),
            ("LC_ALL".into(), "C".into()),
        ];
        assert_eq!(fake.envs(), [env]);
        result
    }

    #[test]
    fn no_stashes_is_an_empty_list() {
        assert_eq!(listed(b""), Ok(vec![]));
    }

    #[test]
    fn one_entry_is_read_with_its_base_and_index() {
        assert_eq!(
            listed(b"aa\x1fbb cc\x1fOn main: wip\0"),
            Ok(vec![StashEntry {
                index: 0,
                oid: oid("aa"),
                base: oid("bb"),
                untracked: None,
                subject: "On main: wip".into(),
            }])
        );
    }

    #[test]
    fn three_entries_take_their_place_as_the_index_and_a_third_parent_is_untracked() {
        let out = b"a1\x1fb1 c1\x1fOn main: one\0\
                    a2\x1fb2 c2 d2\x1fWIP on main: 1234 two\0\
                    a3\x1fb3 c3\x1fthree\0";
        let entries = listed(out).unwrap();
        let summary: Vec<_> = entries
            .iter()
            .map(|e| {
                (
                    e.index,
                    e.oid.as_str(),
                    e.untracked.as_ref().map(CommitId::as_str),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [(0, "a1", None), (1, "a2", Some("d2")), (2, "a3", None)]
        );
        assert_eq!(entries[1].subject, "WIP on main: 1234 two");
        assert_eq!(entries[2].base, oid("b3"));
    }

    #[test]
    fn a_subject_with_the_separator_and_escapes_is_kept_and_made_safe() {
        let entries = listed(b"aa\x1fbb cc\x1fOn main: a\x1fb \x1b[2J\0").unwrap();
        let subject = &entries[0].subject;
        assert!(subject.starts_with("On main: a"), "{subject:?}");
        assert!(subject.ends_with("b \u{FFFD}[2J"), "{subject:?}");
        assert!(!subject.contains(char::is_control), "{subject:?}");
    }

    #[test]
    fn an_id_that_is_not_hex_refuses_the_whole_list() {
        for out in [
            &b"aa\x1fbb\x1fok\0--x\x1fbb\x1fbad\0"[..],
            b"aa\x1f-p cc\x1fbad\0",
            b"aa\x1f\x1fno base\0",
            b"aa only\0",
        ] {
            assert_eq!(listed(out), Err(UNREADABLE.into()), "{out:?}");
        }
    }

    #[test]
    fn a_failed_list_says_why_safely() {
        let fake = FakeRunner::new()
            .expect(&LIST, fail("fatal: \x1b[2J bad\n"))
            .expect(&LIST, fail(""));
        assert_eq!(list_stashes(&fake), Err("fatal: \u{FFFD}[2J bad".into()));
        assert_eq!(list_stashes(&fake), Err("git stash list failed".into()));
    }

    const FILES: [&str; 7] = [
        "diff-tree",
        "-r",
        "-z",
        "--no-commit-id",
        "--name-status",
        "-M",
        "--root",
    ];

    #[test]
    fn an_entrys_files_are_its_tracked_changes_then_its_untracked_files() {
        let entry = StashEntry {
            index: 1,
            oid: oid("aa"),
            base: oid("bb"),
            untracked: Some(oid("dd")),
            subject: String::new(),
        };
        let fake = FakeRunner::new()
            .expect(
                &[&FILES[..], &["bb", "aa", "--"]].concat(),
                ok(b"M\0a.rs\0"),
            )
            .expect(&[&FILES[..], &["dd", "--"]].concat(), ok(b"A\0new.rs\0"));
        let files = stash_files(&fake, &entry).unwrap();
        fake.assert_done();
        let tracked = CommitRange {
            oid: oid("aa"),
            parent: Some(oid("bb")),
        };
        let untracked = CommitRange {
            oid: oid("dd"),
            parent: None,
        };
        let summary: Vec<_> = files
            .iter()
            .map(|f| (&f.range, f.file.status, f.file.path.display()))
            .collect();
        assert_eq!(
            summary,
            [(&tracked, 'M', "a.rs"), (&untracked, 'A', "new.rs")]
        );
    }

    #[test]
    fn a_stash_ref_is_named_by_its_place_only() {
        let at = StashRef {
            index: 12,
            oid: oid("aa"),
        };
        assert_eq!(at.arg(), "stash@{12}");
    }

    #[test]
    fn a_real_stash_lists_and_shows_its_tracked_and_untracked_files() {
        use crate::git::CliRunner;
        use crate::git::fake::{TempDir, git};

        let tmp = TempDir::new("stash-list");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "first"]);
        let runner = CliRunner::isolated(dir);
        assert_eq!(list_stashes(&runner), Ok(vec![]));

        std::fs::write(dir.join("a.txt"), "two\n").unwrap();
        git(dir, &["stash", "push", "-q", "-m", "tracked only"]);
        std::fs::write(dir.join("a.txt"), "three\n").unwrap();
        std::fs::write(dir.join("new.txt"), "new\n").unwrap();
        git(dir, &["stash", "push", "-q", "-u", "-m", "with new"]);

        let entries = list_stashes(&runner).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries[0].subject.ends_with("with new"), "{entries:?}");
        assert!(entries[1].subject.ends_with("tracked only"), "{entries:?}");
        assert!(entries[0].untracked.is_some());
        assert_eq!(entries[1].untracked, None);
        let head = String::from_utf8(git(dir, &["rev-parse", "HEAD"])).unwrap();
        assert_eq!(entries[0].base.as_str(), head.trim());

        let files = stash_files(&runner, &entries[0]).unwrap();
        let names: Vec<_> = files
            .iter()
            .map(|f| (f.file.status, f.file.path.display()))
            .collect();
        assert_eq!(names, [('M', "a.txt"), ('A', "new.txt")]);
    }
}
