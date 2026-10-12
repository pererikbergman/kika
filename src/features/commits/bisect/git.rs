//! `git bisect start`, which runs before the first mark of a bisect.

use crate::git::runner::{GitCommand, GitRunner};
use crate::op::stderr_and_stdout;

/// `git bisect start` with no arguments: it records where `HEAD` is and
/// moves nothing, so the first mark that follows starts the bisect.
pub(crate) fn run_bisect_start(runner: &dyn GitRunner) -> Result<(), String> {
    let output = runner
        .run(&GitCommand::new("bisect").arg("start"))
        .map_err(|e| e.to_string())?;
    if output.success {
        return Ok(());
    }
    let message = stderr_and_stdout(&output);
    Err(if message.is_empty() {
        "git bisect start failed".into()
    } else {
        message
    })
}

#[cfg(test)]
mod tests {
    use crate::git::bisect::BisectMark;
    use crate::git::commit_files::CommitId;
    use crate::git::fake::{FakeRunner, fail, ok};
    use crate::git::runner::GitOutput;
    use crate::op::{GitOp, op_command, run_op};
    use crate::test_support::{argv, cid as oid, rev_parse};

    fn mark(mark: BisectMark, start: bool) -> GitOp {
        GitOp::BisectMark {
            mark,
            commit: oid("abc123"),
            start,
        }
    }

    #[test]
    fn bisect_ops_pass_only_fixed_words_and_the_commits_hex() {
        let cases: [(GitOp, &[&str]); 4] = [
            (mark(BisectMark::Bad, false), &["bisect", "bad", "abc123"]),
            (mark(BisectMark::Good, true), &["bisect", "good", "abc123"]),
            (mark(BisectMark::Skip, false), &["bisect", "skip", "abc123"]),
            (GitOp::BisectReset, &["bisect", "reset"]),
        ];
        for (op, expected) in cases {
            let cmd = op_command(&op);
            assert_eq!(argv(&cmd), expected, "{op:?}");
            assert!(cmd.envs().is_empty(), "{op:?}: {:?}", cmd.envs());
            assert_eq!(cmd.input(), None);
        }
    }

    #[test]
    fn a_first_mark_runs_bisect_start_before_it() {
        let runner = FakeRunner::new()
            .expect(&["bisect", "start"], ok(b""))
            .expect(&["bisect", "bad", "abc123"], ok(b""));
        run_op(&runner, &mark(BisectMark::Bad, true)).unwrap();
        runner.assert_done();

        let runner = FakeRunner::new().expect(&["bisect", "good", "abc123"], ok(b""));
        run_op(&runner, &mark(BisectMark::Good, false)).unwrap();
        runner.assert_done();
    }

    #[test]
    fn a_failed_start_stops_before_the_mark_with_gits_words() {
        let runner = FakeRunner::new().expect(&["bisect", "start"], fail("fatal: Bad HEAD\n"));
        assert_eq!(
            run_op(&runner, &mark(BisectMark::Bad, true)),
            Err("fatal: Bad HEAD".into())
        );
        runner.assert_done();
    }

    #[test]
    fn a_failed_mark_carries_what_git_said_on_stdout() {
        let output = GitOutput {
            stdout: b"There are only 'skip'ped commits left to test.\nThe first bad commit could be any of:\n\x1b[31mabc\n".to_vec(),
            ..fail("")
        };
        let runner = FakeRunner::new().expect(&["bisect", "skip", "abc123"], output);
        assert_eq!(
            run_op(&runner, &mark(BisectMark::Skip, false)),
            Err("There are only 'skip'ped commits left to test.\n\
                 The first bad commit could be any of:\n\u{FFFD}[31mabc"
                .into())
        );
    }

    /// Eight commits on `main`, `commit 1` to `commit 8`; from commit 5 on
    /// the tree has a `bug` file. Returns the repo and the commits, oldest
    /// first.
    fn buggy_history() -> (crate::git::fake::TempDir, Vec<CommitId>) {
        use crate::git::fake::{TempDir, git};

        let tmp = TempDir::new("bisect");
        let dir = tmp.path();
        git(dir, &["init", "-q", "--template=", "-b", "main"]);
        for (key, value) in [
            ("user.name", "Kika Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
        ] {
            git(dir, &["config", key, value]);
        }
        let mut commits = Vec::new();
        for i in 1..=8 {
            std::fs::write(dir.join("n.txt"), format!("{i}\n")).unwrap();
            if i == 5 {
                std::fs::write(dir.join("bug"), "bug\n").unwrap();
            }
            git(dir, &["add", "-A"]);
            git(dir, &["commit", "-q", "-m", &format!("commit {i}")]);
            commits.push(rev_parse(dir, "HEAD"));
        }
        (tmp, commits)
    }

    #[test]
    fn marks_find_the_first_bad_commit_and_reset_returns_to_the_branch_in_real_git() {
        use crate::git::bisect::read_bisect;
        use crate::git::fake::{git, try_git};

        let (tmp, commits) = buggy_history();
        let dir = tmp.path();
        let git_dir = dir.join(".git");
        let runner = crate::git::CliRunner::isolated(dir);
        let run = |mark: BisectMark, commit: &CommitId, start: bool| {
            let op = GitOp::BisectMark {
                mark,
                commit: commit.clone(),
                start,
            };
            run_op(&runner, &op).unwrap();
        };

        run(BisectMark::Bad, &commits[7], true);
        assert_eq!(read_bisect(&git_dir).unwrap().bad, 1);
        run(BisectMark::Good, &commits[0], false);
        let mut steps = 0;
        let found = loop {
            let state = read_bisect(&git_dir).unwrap();
            if let Some(found) = state.first_bad {
                break found;
            }
            assert!(
                !try_git(dir, &["symbolic-ref", "-q", "HEAD"]),
                "HEAD is on a branch"
            );
            let head = rev_parse(dir, "HEAD");
            assert!(
                commits[1..7].contains(&head),
                "HEAD {head:?} is outside the range"
            );
            let mark = if dir.join("bug").exists() {
                BisectMark::Bad
            } else {
                BisectMark::Good
            };
            run(mark, &head, false);
            steps += 1;
            assert!(
                steps < 8,
                "the bisect never ended; {}:\n{}",
                String::from_utf8_lossy(&git(dir, &["--version"])),
                String::from_utf8_lossy(&std::fs::read(git_dir.join("BISECT_LOG")).unwrap())
            );
        };
        assert_eq!(found.commit, commits[4]);
        assert_eq!(found.subject, "commit 5");

        run_op(&runner, &GitOp::BisectReset).unwrap();
        assert_eq!(git(dir, &["symbolic-ref", "HEAD"]), b"refs/heads/main\n");
        assert_eq!(rev_parse(dir, "HEAD"), commits[7]);
        assert_eq!(read_bisect(&git_dir), None);
    }
}
