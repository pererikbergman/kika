//! Applying or popping a stash. A stash whose index holds a rename that
//! only changes case comes back with its index too (`--index`): without
//! it, a case-insensitive filesystem folds the old name onto the new one
//! and the index keeps both. That restore runs only where git cannot stop
//! partway; anywhere else it is refused and the stash kept.

use std::collections::HashMap;

use super::git::{StashRef, checked, stash_index_command};
use super::push::case_only;
use crate::op::{GitOp, op_command};
use kika_git::error::GitError;
use kika_git::path::RepoPath;
use kika_git::runner::{GitCommand, GitRunner};
use kika_git::status::{parse_status, status_command};

/// Why a stash with a case-only rename is refused off its own commit:
/// there git's merge could stop with the index half restored.
pub const STASH_CASE_BASE: &str =
    "This stash renames a file by case only; apply it on the commit it was made on";

/// Why a stash with a case-only rename is refused on a changed tree: a
/// local change or untracked file could stop the restore partway.
pub const STASH_CASE_DIRTY: &str =
    "This stash renames a file by case only; apply it with no other changes in the worktree";

/// Why the stash's index could not be checked: its diff did not parse.
pub const STASH_UNREADABLE: &str = "git diff-tree gave output Kika could not read";

/// What the stash's index changed against its base, file by file, with no
/// rename pairing, so a case rename whose content also changed is still a
/// deletion and an addition. `^2` is the index commit `--index` restores.
/// The id is hex that `verify_stash` has just checked.
pub(crate) fn case_rename_command(stash: &StashRef) -> GitCommand {
    let oid = stash.oid.as_str();
    GitCommand::new("diff-tree")
        .arg("-r")
        .arg("-z")
        .arg("--no-renames")
        .arg("--name-status")
        .arg(format!("{oid}^1"))
        .arg(format!("{oid}^2"))
        .arg("--")
        .standard_env()
}

/// Whether `stdout` of [`case_rename_command`] deletes a path and adds one
/// that differs from it only in ASCII case, the fold `case_only` uses.
pub(crate) fn holds_case_rename(stdout: &[u8]) -> Result<bool, String> {
    let body = stdout.strip_suffix(b"\0").unwrap_or(stdout);
    if body.is_empty() {
        return Ok(false);
    }
    let mut fields = body.split(|&b| b == 0);
    let mut deleted: HashMap<Vec<u8>, Vec<RepoPath>> = HashMap::new();
    let mut added = Vec::new();
    while let Some(status) = fields.next() {
        let path = fields.next().filter(|p| !p.is_empty());
        let (false, Some(path)) = (status.is_empty(), path) else {
            return Err(STASH_UNREADABLE.into());
        };
        let path = RepoPath::from_bytes(path);
        match status {
            b"D" => deleted
                .entry(path.as_bytes().to_ascii_lowercase())
                .or_default()
                .push(path),
            b"A" => added.push(path),
            _ => {}
        }
    }
    Ok(added.iter().any(|new| {
        deleted
            .get(&new.as_bytes().to_ascii_lowercase())
            .is_some_and(|olds| olds.iter().any(|old| case_only(old, new)))
    }))
}

/// Apply or pop `op`'s stash; `run_op` has checked the entry is still
/// where it was chosen. A stash with no case-only rename runs as it
/// always has. One with such a rename runs with `--index`, after reads
/// that make sure git cannot stop partway, and never falls back to a
/// plain pop.
pub(crate) fn apply_stash(runner: &dyn GitRunner, op: &GitOp) -> Result<(), String> {
    let (verb, stash) = match op {
        GitOp::StashApply(stash) => ("apply", stash),
        GitOp::StashPop(stash) => ("pop", stash),
        _ => unreachable!("apply_stash takes a stash apply or pop"),
    };
    let diff = checked(runner, &case_rename_command(stash))?;
    let cmd = if holds_case_rename(&diff.stdout)? {
        index_restore_safe(runner, stash)?;
        stash_index_command(verb, stash)
    } else {
        op_command(op)
    };
    let output = runner.run(&cmd).map_err(|e| GitError::io(&e).to_string())?;
    if output.success {
        return Ok(());
    }
    Err(GitError::failed_with_stdout(&cmd, &output).to_string())
}

/// Refuse an `--index` restore git could stop partway: off the stash's
/// own commit its merge can conflict, and on a changed tree a local
/// change or untracked file can block it after the index was written.
/// Both checks only read.
fn index_restore_safe(runner: &dyn GitRunner, stash: &StashRef) -> Result<(), String> {
    let bases = GitCommand::new("rev-parse")
        .arg("HEAD")
        .arg(format!("{}^1", stash.oid.as_str()))
        .arg("--")
        .standard_env();
    let bases = checked(runner, &bases)?;
    let mut lines = bases.stdout.split(|&b| b == b'\n');
    let (Some(head), Some(base)) = (lines.next(), lines.next()) else {
        return Err(STASH_CASE_BASE.into());
    };
    if head.is_empty() || !head.eq_ignore_ascii_case(base) {
        return Err(STASH_CASE_BASE.into());
    }
    let status = checked(runner, &status_command())?;
    if !parse_status(&status.stdout)?.files.is_empty() {
        return Err(STASH_CASE_DIRTY.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::features::stash::git::tests::ignores_case;
    use crate::op::run_op;
    use kika_git::commit_files::CommitId;
    use kika_git::fake::{FakeRunner, TempDir, fail, git, ok};
    use kika_git::runner::GitOutput;

    const OID: &str = "5a5a5a";
    const HEAD: &str = "b0b0b0";

    fn at(index: usize) -> StashRef {
        StashRef {
            index,
            oid: CommitId::parse(OID).unwrap(),
        }
    }

    // --- holds_case_rename ---

    fn holds(stdout: &[u8]) -> Result<bool, String> {
        holds_case_rename(stdout)
    }

    #[test]
    fn a_deletion_and_an_addition_differing_only_in_case_are_a_case_rename() {
        assert_eq!(holds(b"D\0Old.txt\0A\0old.txt\0"), Ok(true));
        assert_eq!(holds(b"A\0old.txt\0D\0Old.txt\0"), Ok(true));
        assert_eq!(
            holds(b"M\0x.rs\0A\0new.rs\0D\0Old.txt\0M\0y.rs\0A\0old.txt\0"),
            Ok(true)
        );
        // A directory renamed by case shows as its files.
        assert_eq!(holds(b"D\0Dir/a\0A\0dir/a\0"), Ok(true));
        // Names that are not UTF-8 are compared as bytes.
        assert_eq!(holds(b"D\0X\xff.txt\0A\0x\xff.txt\0"), Ok(true));
    }

    #[test]
    fn anything_else_is_not_a_case_rename() {
        for out in [
            &b"D\0a.txt\0A\0b.txt\0"[..],
            b"M\0Old.txt\0",
            b"A\0old.txt\0",
            b"D\0Old.txt\0",
            b"D\0Old.txt\0M\0old.txt\0",
            b"T\0Old.txt\0A\0old.txt\0",
            b"",
        ] {
            assert_eq!(holds(out), Ok(false), "{out:?}");
        }
    }

    #[test]
    fn a_status_with_no_path_is_unreadable() {
        for out in [&b"D\0"[..], b"D\0Old.txt\0A", b"\0x\0", b"D\0\0"] {
            assert_eq!(holds(out), Err(STASH_UNREADABLE.into()), "{out:?}");
        }
    }

    // --- apply_stash, faked ---

    fn verify(runner: FakeRunner, index: usize) -> FakeRunner {
        let arg = format!("stash@{{{index}}}");
        runner.expect(
            &["rev-parse", "-q", "--verify", &arg],
            ok(format!("{OID}\n").as_bytes()),
        )
    }

    const DIFF_TREE: [&str; 8] = [
        "diff-tree",
        "-r",
        "-z",
        "--no-renames",
        "--name-status",
        "5a5a5a^1",
        "5a5a5a^2",
        "--",
    ];
    const BASES: [&str; 4] = ["rev-parse", "HEAD", "5a5a5a^1", "--"];
    const STATUS: [&str; 5] = [
        "status",
        "--porcelain=v2",
        "--branch",
        "-z",
        "--untracked-files=all",
    ];
    const CASE: &[u8] = b"M\0b.txt\0D\0Old.txt\0A\0old.txt\0";

    fn bases(head: &str) -> GitOutput {
        ok(format!("{head}\n{OID}\n--\n").as_bytes())
    }

    fn ops() -> [(GitOp, &'static str); 2] {
        [
            (GitOp::StashPop(at(0)), "pop"),
            (GitOp::StashApply(at(0)), "apply"),
        ]
    }

    fn read_env() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        kika_git::fake::env_list(&[
            ("LC_ALL", "C"),
            ("GIT_OPTIONAL_LOCKS", "0"),
            ("GIT_LITERAL_PATHSPECS", "1"),
        ])
    }

    fn write_env() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        kika_git::fake::env_list(&[("LC_ALL", "C"), ("GIT_LITERAL_PATHSPECS", "1")])
    }

    #[test]
    fn a_stash_with_no_case_rename_runs_as_it_always_has() {
        for (op, verb) in ops() {
            let runner = verify(FakeRunner::new(), 0)
                .expect(&DIFF_TREE, ok(b"D\0old.txt\0A\0new.txt\0M\0b.txt\0"))
                .expect(&["stash", verb, "stash@{0}"], ok(b""));
            assert_eq!(run_op(&runner, &op), Ok(()), "{verb}");
            runner.assert_done();
            assert_eq!(
                runner.process_envs(),
                [read_env(), read_env(), write_env()],
                "{verb}"
            );
        }
    }

    #[test]
    fn a_case_rename_on_its_own_commit_and_a_clean_tree_restores_the_index() {
        for (op, verb) in ops() {
            let runner = verify(FakeRunner::new(), 0)
                .expect(&DIFF_TREE, ok(CASE))
                .expect(&BASES, bases(OID))
                .expect(&STATUS, ok(b"# branch.oid 5a5a5a\0# branch.head main\0"))
                .expect(&["stash", verb, "--index", "stash@{0}"], ok(b""));
            assert_eq!(run_op(&runner, &op), Ok(()), "{verb}");
            runner.assert_done();
            assert_eq!(
                runner.process_envs(),
                [read_env(), read_env(), read_env(), read_env(), write_env()],
                "{verb}"
            );
        }
    }

    #[test]
    fn a_case_rename_off_its_own_commit_is_refused_before_any_stash_command() {
        for (op, verb) in ops() {
            let runner = verify(FakeRunner::new(), 0)
                .expect(&DIFF_TREE, ok(CASE))
                .expect(&BASES, bases(HEAD));
            assert_eq!(run_op(&runner, &op), Err(STASH_CASE_BASE.into()), "{verb}");
            runner.assert_done();
        }
    }

    #[test]
    fn a_case_rename_on_a_changed_tree_is_refused_before_any_stash_command() {
        for status in [
            &b"? u.txt\0"[..],
            b"1 .M N... 100644 100644 100644 aa aa b.txt\0",
        ] {
            for (op, verb) in ops() {
                let runner = verify(FakeRunner::new(), 0)
                    .expect(&DIFF_TREE, ok(CASE))
                    .expect(&BASES, bases(OID))
                    .expect(&STATUS, ok(status));
                assert_eq!(run_op(&runner, &op), Err(STASH_CASE_DIRTY.into()), "{verb}");
                runner.assert_done();
            }
        }
    }

    #[test]
    fn a_failed_read_stops_the_op_with_its_error_before_any_stash_command() {
        let bad = || fail("fatal: bad \x1b[2J\n");
        let said = "fatal: bad \u{FFFD}[2J";
        for (op, verb) in ops() {
            // The diff of the stash's index.
            let runner = verify(FakeRunner::new(), 0).expect(&DIFF_TREE, bad());
            assert_eq!(run_op(&runner, &op), Err(said.into()), "{verb}");
            runner.assert_done();
            let runner = verify(FakeRunner::new(), 0).expect(&DIFF_TREE, ok(b"D\0"));
            assert_eq!(run_op(&runner, &op), Err(STASH_UNREADABLE.into()));
            runner.assert_done();
            // The commits.
            let runner = verify(FakeRunner::new(), 0)
                .expect(&DIFF_TREE, ok(CASE))
                .expect(&BASES, bad());
            assert_eq!(run_op(&runner, &op), Err(said.into()), "{verb}");
            runner.assert_done();
            // The status.
            let runner = verify(FakeRunner::new(), 0)
                .expect(&DIFF_TREE, ok(CASE))
                .expect(&BASES, bases(OID))
                .expect(&STATUS, bad());
            assert_eq!(run_op(&runner, &op), Err(said.into()), "{verb}");
            runner.assert_done();
        }
    }

    #[test]
    fn a_failed_index_restore_is_shown_safely_and_never_retried_without_it() {
        for (op, verb) in ops() {
            let output = GitOutput {
                success: false,
                stdout: "Index was not unstashed \u{202E}\x1b[31m\n".into(),
                stderr: "error: conflicts in index \x1b[2J\u{202E}\n".into(),
            };
            let runner = verify(FakeRunner::new(), 0)
                .expect(&DIFF_TREE, ok(CASE))
                .expect(&BASES, bases(OID))
                .expect(&STATUS, ok(b""))
                .expect(&["stash", verb, "--index", "stash@{0}"], output);
            let error = run_op(&runner, &op).unwrap_err();
            assert!(error.contains("conflicts in index"), "{error:?}");
            assert!(error.contains("Index was not unstashed"), "{error:?}");
            assert!(!error.contains('\x1b'), "{error:?}");
            assert!(!error.contains('\u{202E}'), "{error:?}");
            // A second stash command would find nothing expected and panic.
            runner.assert_done();
        }
    }

    // --- In real git ---

    fn repo(files: &[(&str, &str)]) -> TempDir {
        let tmp = TempDir::new("stash-case");
        let dir = tmp.path();
        git(dir, &["init", "-q", "--template="]);
        for (key, value) in [
            ("user.name", "Kika Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
        ] {
            git(dir, &["config", key, value]);
        }
        for (name, text) in files {
            std::fs::write(dir.join(name), text).unwrap();
        }
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "first"]);
        tmp
    }

    fn text(dir: &Path, args: &[&str]) -> String {
        String::from_utf8(git(dir, args)).unwrap()
    }

    fn porcelain(dir: &Path) -> String {
        text(dir, &["status", "--porcelain"])
    }

    fn ls_files(dir: &Path) -> String {
        text(dir, &["ls-files"])
    }

    fn stash_list(dir: &Path) -> String {
        text(dir, &["stash", "list"])
    }

    fn top(dir: &Path) -> StashRef {
        let entries =
            crate::features::stash::git::list_stashes(&kika_git::CliRunner::isolated(dir)).unwrap();
        entries[0].stash_ref()
    }

    /// `Old.txt` and `b.txt` committed; `Old.txt` renamed to `old.txt`,
    /// `b.txt` changed and `u.txt` new; all of it stashed with `-u`.
    fn stashed_with_others() -> TempDir {
        let tmp = repo(&[("Old.txt", "old\n"), ("b.txt", "b\n")]);
        let dir = tmp.path();
        git(dir, &["mv", "--", "Old.txt", "old.txt"]);
        std::fs::write(dir.join("b.txt"), "changed\n").unwrap();
        std::fs::write(dir.join("u.txt"), "new\n").unwrap();
        git(dir, &["stash", "push", "-q", "-u"]);
        assert_eq!(porcelain(dir), "");
        tmp
    }

    fn check_restored(dir: &Path) {
        assert_eq!(
            porcelain(dir),
            " M b.txt\nR  Old.txt -> old.txt\n?? u.txt\n"
        );
        assert_eq!(ls_files(dir), "b.txt\nold.txt\n");
        if ignores_case(dir) {
            assert!(
                !ls_files(dir).lines().any(|l| l == "Old.txt"),
                "the old name stayed in the index"
            );
        } else {
            eprintln!("skipped the folded-name check: this filesystem is case-sensitive");
        }
    }

    #[test]
    fn popping_a_case_rename_brings_the_rename_back_in_real_git() {
        let tmp = stashed_with_others();
        let dir = tmp.path();
        let runner = kika_git::CliRunner::isolated(dir);
        assert_eq!(run_op(&runner, &GitOp::StashPop(top(dir))), Ok(()));
        check_restored(dir);
        assert_eq!(stash_list(dir), "");
    }

    #[test]
    fn applying_a_case_rename_brings_the_rename_back_and_keeps_the_stash_in_real_git() {
        let tmp = stashed_with_others();
        let dir = tmp.path();
        let runner = kika_git::CliRunner::isolated(dir);
        assert_eq!(run_op(&runner, &GitOp::StashApply(top(dir))), Ok(()));
        check_restored(dir);
        assert_eq!(stash_list(dir).lines().count(), 1);
    }

    /// `Old.txt` committed alone, renamed to `old.txt` and stashed.
    fn stashed_alone() -> TempDir {
        let tmp = repo(&[("Old.txt", "old\n")]);
        let dir = tmp.path();
        git(dir, &["mv", "--", "Old.txt", "old.txt"]);
        git(dir, &["stash", "push", "-q"]);
        tmp
    }

    /// Pop `dir`'s top stash and check nothing changed.
    fn refused(dir: &Path) -> String {
        let before = (porcelain(dir), ls_files(dir), stash_list(dir));
        let runner = kika_git::CliRunner::isolated(dir);
        let error = run_op(&runner, &GitOp::StashPop(top(dir))).unwrap_err();
        assert_eq!((porcelain(dir), ls_files(dir), stash_list(dir)), before);
        assert_eq!(stash_list(dir).lines().count(), 1);
        error
    }

    #[test]
    fn a_case_rename_on_a_changed_tree_is_refused_with_nothing_changed_in_real_git() {
        let tmp = stashed_alone();
        let dir = tmp.path();
        std::fs::write(dir.join("Old.txt"), "mine\n").unwrap();
        git(dir, &["add", "--", "Old.txt"]);
        assert_eq!(refused(dir), STASH_CASE_DIRTY);
    }

    #[test]
    fn a_case_rename_on_another_commit_is_refused_with_nothing_changed_in_real_git() {
        let tmp = stashed_alone();
        let dir = tmp.path();
        std::fs::write(dir.join("Old.txt"), "later\n").unwrap();
        git(dir, &["commit", "-q", "-am", "later"]);
        assert_eq!(refused(dir), STASH_CASE_BASE);
    }

    #[test]
    fn a_locked_index_fails_the_restore_and_keeps_the_stash_in_real_git() {
        let tmp = stashed_alone();
        let dir = tmp.path();
        std::fs::write(dir.join(".git/index.lock"), "").unwrap();
        let error = refused(dir);
        assert!(error.contains("index.lock"), "{error}");
        assert_eq!(ls_files(dir), "Old.txt\n");
    }

    #[test]
    fn a_plain_rename_pops_as_it_always_has_in_real_git() {
        let tmp = repo(&[("old.txt", "old\n")]);
        let dir = tmp.path();
        git(dir, &["mv", "--", "old.txt", "new.txt"]);
        git(dir, &["stash", "push", "-q"]);
        let runner = kika_git::CliRunner::isolated(dir);
        assert_eq!(run_op(&runner, &GitOp::StashPop(top(dir))), Ok(()));
        // No `--index`: the old side comes back unstaged.
        assert_eq!(porcelain(dir), "A  new.txt\n D old.txt\n");
        assert_eq!(stash_list(dir), "");
    }
}
