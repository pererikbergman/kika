//! The stash list and the files of one entry, so the Stash panel can show
//! work put aside the way the Commits panel shows a commit.
//! Also the argv of a stash push, apply, pop, drop and rename, and the
//! checks `run_op` makes before them.

use super::push::with_literal_paths;
use crate::git::commit_files::{CommitFile, CommitId, CommitRange, commit_files};
use crate::git::diff::{sanitize, terminal_safe};
use crate::git::path::RepoPath;
use crate::git::runner::{GitCommand, GitRunner, run_checked};
use crate::git::version::{GitVersion, parse_version, version_command};
use crate::op::{GitOp, op_command};

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
pub(crate) const STASH_STAGED_GIT: GitVersion = GitVersion {
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

/// `git stash push` for [`GitOp::StashPush`]: its mode, message and
/// paths.
pub(crate) fn push_command(
    mode: StashMode,
    message: Option<&str>,
    paths: &[RepoPath],
) -> GitCommand {
    // Paths only with `-u` or no flag: `--staged` takes the whole
    // index, and no key builds it with paths. Only a push that names
    // files reads pathspecs literally: without paths, `-u` cleans the
    // untracked files through a `:/` pathspec of git's own, which a
    // literal reading turns into a name that matches nothing, and they
    // would stay in the worktree as well as in the stash.
    // A rename's old path is in `restore`, not here; a staged deletion
    // is here and in `unstage`: see `GitOp::StashPush`.
    let cmd = GitCommand::new("stash").arg("push");
    let cmd = match mode {
        StashMode::All => cmd,
        StashMode::IncludeUntracked => cmd.arg("-u"),
        StashMode::Staged => cmd.arg("--staged"),
    };
    let cmd = match message.filter(|m| !m.trim().is_empty()) {
        Some(message) => cmd.arg("-m").arg(message),
        None => cmd,
    };
    debug_assert!(
        mode != StashMode::Staged || paths.is_empty(),
        "--staged takes no paths"
    );
    if paths.is_empty() || mode == StashMode::Staged {
        return cmd.env("LC_ALL", "C");
    }
    with_literal_paths(cmd, paths)
}

/// `git stash store`: the entry's commit again, as `stash@{0}`, under
/// `message`. The id is hex digits only, so it cannot be read as an option.
pub(crate) fn store_command(stash: &StashRef, message: &str) -> GitCommand {
    GitCommand::new("stash")
        .arg("store")
        .arg("-m")
        .arg(message)
        .arg(stash.oid.as_str())
        .env("LC_ALL", "C")
}

/// `git stash <verb> stash@{N}`.
pub(crate) fn stash_command(verb: &str, stash: &StashRef) -> GitCommand {
    GitCommand::new("stash")
        .arg(verb)
        .arg(stash.arg())
        .env("LC_ALL", "C")
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
pub(crate) fn verify_stash(runner: &dyn GitRunner, stash: &StashRef) -> Result<(), String> {
    if stash_is(runner, stash)? {
        Ok(())
    } else {
        Err(STASH_LIST_CHANGED.into())
    }
}

/// Refuse `--staged` on a git older than 2.35. A version Kika cannot read
/// is let through, as at startup.
pub(crate) fn staged_stash_supported(runner: &dyn GitRunner) -> Result<(), String> {
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
pub(crate) fn rename_stash(
    runner: &dyn GitRunner,
    stash: &StashRef,
    message: &str,
) -> Result<(), String> {
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

#[cfg(test)]
pub(crate) mod tests {
    use std::ffi::OsString;

    use super::*;
    use crate::features::stash::push::StashSet;
    use crate::git::fake::{FakeRunner, fail, ok};
    use crate::git::runner::GitOutput;
    use crate::op::{op_steps, refusal, run_op};
    use crate::test_support::argv;

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

    const STASH_OID: &str = "5a5a5a";

    fn at(index: usize) -> StashRef {
        StashRef {
            index,
            oid: oid(STASH_OID),
        }
    }

    pub(crate) fn push(mode: StashMode, message: Option<&str>, paths: &[&str]) -> GitOp {
        GitOp::StashPush {
            mode,
            message: message.map(Into::into),
            paths: paths.iter().map(|&p| p.into()).collect(),
            restore: vec![],
            unstage: vec![],
        }
    }

    pub(crate) fn set_push(set: StashSet, message: Option<&str>) -> GitOp {
        GitOp::StashPush {
            mode: set.mode,
            message: message.map(Into::into),
            paths: set.paths,
            restore: set.restore,
            unstage: set.unstage,
        }
    }

    pub(crate) fn literal_env(cmd: &GitCommand) -> Vec<(&str, &str)> {
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
}
