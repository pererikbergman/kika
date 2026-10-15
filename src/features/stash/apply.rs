//! Applying or popping a stash. A stash whose index holds a rename that
//! only changes case comes back with its index too (`--index`): without
//! it, a case-insensitive filesystem folds the old name onto the new one
//! and the index keeps both. That restore runs only on the stash's own
//! commit, on a clean worktree with nothing at the paths of its untracked
//! files; anywhere else it is refused and the stash kept. Something that
//! changes the worktree between those reads and the restore is not caught.

use std::collections::HashMap;

use super::git::{StashRef, checked, stash_index_command};
use super::push::{STASH_NO_TOP, case_only, restore_blocked_on_disk};
use crate::op::{GitOp, op_command};
use kika_git::error::GitError;
use kika_git::path::RepoPath;
use kika_git::runner::{GitCommand, GitRunner};
use kika_git::status::{parse_status, status_command};

/// Why a stash with a case-only rename is refused off its own commit:
/// there git's merge could stop with the index half restored.
pub const STASH_CASE_BASE: &str = "This stash renames a file by case only; \
     apply it on the commit it was made on. The stash was kept.";

/// Why a stash with a case-only rename is refused on a changed tree: a
/// local change, an untracked file, or an ignored file where the stash
/// keeps an untracked one could stop the restore partway.
pub const STASH_CASE_DIRTY: &str = "This stash renames a file by case only; \
     apply it with no other changes in the worktree, untracked files included. \
     Commit or stash them first. The stash was kept.";

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
/// always has. One with such a rename runs with `--index`, only after
/// [`index_restore_safe`]'s reads pass, and never falls back to a plain
/// pop.
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
/// own commit its merge can conflict, and after the index is written a
/// local change, an untracked file, or anything at the path of one of the
/// stash's untracked files (an ignored file, say) can block the rest.
/// Every check only reads; none can see a change made after it ran.
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
    let untracked = untracked_paths(runner, stash)?;
    if untracked.is_empty() {
        return Ok(());
    }
    let top = kika_git::repo_root::find_toplevel(runner).ok_or(STASH_NO_TOP)?;
    if restore_blocked_on_disk(&top, &untracked).is_some() {
        return Err(STASH_CASE_DIRTY.into());
    }
    Ok(())
}

/// The paths of the untracked files `stash` keeps in its third parent,
/// none when it has none. The clean status above ignores ignored files,
/// and git will not write an untracked file over one.
fn untracked_paths(runner: &dyn GitRunner, stash: &StashRef) -> Result<Vec<RepoPath>, String> {
    let third = format!("{}^3", stash.oid.as_str());
    let probe = GitCommand::new("rev-parse")
        .arg("-q")
        .arg("--verify")
        .arg(&third)
        .standard_env();
    let output = runner
        .run(&probe)
        .map_err(|e| GitError::io(&e).to_string())?;
    // `-q` makes a missing parent a silent failure; any other says why.
    if !output.success && output.stdout.is_empty() && output.stderr.is_empty() {
        return Ok(Vec::new());
    }
    if !output.success {
        return Err(GitError::failed(&probe, &output).to_string());
    }
    let files = GitCommand::new("ls-tree")
        .arg("-r")
        .arg("-z")
        .arg("--full-tree")
        .arg("--name-only")
        .arg(&third)
        .arg("--")
        .standard_env();
    let files = checked(runner, &files)?;
    Ok((files.stdout.split(|&b| b == 0))
        .filter(|name| !name.is_empty())
        .map(RepoPath::from_bytes)
        .collect())
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
    const UNTRACKED: [&str; 4] = ["rev-parse", "-q", "--verify", "5a5a5a^3"];
    const UNTRACKED_FILES: [&str; 7] = [
        "ls-tree",
        "-r",
        "-z",
        "--full-tree",
        "--name-only",
        "5a5a5a^3",
        "--",
    ];
    const TOP: [&str; 2] = ["rev-parse", "--show-toplevel"];

    /// What `rev-parse -q --verify` gives for a stash with no untracked
    /// commit: a failure that says nothing.
    fn no_untracked() -> GitOutput {
        GitOutput {
            success: false,
            stdout: vec![],
            stderr: vec![],
        }
    }

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
                .expect(&UNTRACKED, no_untracked())
                .expect(&["stash", verb, "--index", "stash@{0}"], ok(b""));
            assert_eq!(run_op(&runner, &op), Ok(()), "{verb}");
            runner.assert_done();
            assert_eq!(
                runner.process_envs(),
                [
                    read_env(),
                    read_env(),
                    read_env(),
                    read_env(),
                    read_env(),
                    write_env()
                ],
                "{verb}"
            );
        }
    }

    /// A worktree whose top level holds `names`, as files.
    fn top_with(names: &[&str]) -> TempDir {
        let tmp = TempDir::new("stash-case-top");
        for name in names {
            let path = tmp.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "x\n").unwrap();
        }
        tmp
    }

    fn untracked_at(runner: FakeRunner, top: &Path) -> FakeRunner {
        let top = format!("{}\n", top.display());
        runner
            .expect(&UNTRACKED, ok(format!("{}\n", "c3".repeat(20)).as_bytes()))
            .expect(&UNTRACKED_FILES, ok(b"d/v.txt\0u.txt\0"))
            .expect(&TOP, ok(top.as_bytes()))
    }

    #[test]
    fn untracked_files_with_nothing_at_their_paths_are_restored_with_the_index() {
        let top = top_with(&["other.txt"]);
        for (op, verb) in ops() {
            let runner = verify(FakeRunner::new(), 0)
                .expect(&DIFF_TREE, ok(CASE))
                .expect(&BASES, bases(OID))
                .expect(&STATUS, ok(b""));
            let runner = untracked_at(runner, top.path())
                .expect(&["stash", verb, "--index", "stash@{0}"], ok(b""));
            assert_eq!(run_op(&runner, &op), Ok(()), "{verb}");
            runner.assert_done();
        }
    }

    #[test]
    fn an_ignored_file_at_an_untracked_path_is_refused_before_any_stash_command() {
        // The file itself, or a file where a directory of it must go.
        for names in [&["u.txt"][..], &["d/v.txt"], &["d"]] {
            let top = top_with(names);
            for (op, verb) in ops() {
                let runner = verify(FakeRunner::new(), 0)
                    .expect(&DIFF_TREE, ok(CASE))
                    .expect(&BASES, bases(OID))
                    .expect(&STATUS, ok(b""));
                let runner = untracked_at(runner, top.path());
                assert_eq!(
                    run_op(&runner, &op),
                    Err(STASH_CASE_DIRTY.into()),
                    "{names:?} {verb}"
                );
                runner.assert_done();
            }
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
            // The untracked commit: a failure that says why is not "none".
            let runner = verify(FakeRunner::new(), 0)
                .expect(&DIFF_TREE, ok(CASE))
                .expect(&BASES, bases(OID))
                .expect(&STATUS, ok(b""))
                .expect(&UNTRACKED, bad());
            assert_eq!(run_op(&runner, &op), Err(said.into()), "{verb}");
            runner.assert_done();
            // Its files.
            let runner = verify(FakeRunner::new(), 0)
                .expect(&DIFF_TREE, ok(CASE))
                .expect(&BASES, bases(OID))
                .expect(&STATUS, ok(b""))
                .expect(&UNTRACKED, ok(b"c3c3\n"))
                .expect(&UNTRACKED_FILES, bad());
            assert_eq!(run_op(&runner, &op), Err(said.into()), "{verb}");
            runner.assert_done();
            // The top level.
            let runner = verify(FakeRunner::new(), 0)
                .expect(&DIFF_TREE, ok(CASE))
                .expect(&BASES, bases(OID))
                .expect(&STATUS, ok(b""))
                .expect(&UNTRACKED, ok(b"c3c3\n"))
                .expect(&UNTRACKED_FILES, ok(b"u.txt\0"))
                .expect(&TOP, bad());
            assert_eq!(run_op(&runner, &op), Err(STASH_NO_TOP.into()), "{verb}");
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
                .expect(&UNTRACKED, no_untracked())
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
    fn an_ignored_file_at_a_stashed_untracked_path_is_refused_with_nothing_changed_in_real_git() {
        let tmp = stashed_with_others();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join(".git/info")).unwrap();
        std::fs::write(dir.join(".git/info/exclude"), "u.txt\n").unwrap();
        std::fs::write(dir.join("u.txt"), "mine\n").unwrap();
        assert_eq!(porcelain(dir), "");
        assert_eq!(refused(dir), STASH_CASE_DIRTY);
        assert_eq!(
            std::fs::read_to_string(dir.join("u.txt")).unwrap(),
            "mine\n"
        );
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
