//! The argv of `git commit`: where its message comes from, and the flags
//! an amend and skipping hooks add. `op_command` builds a detached commit
//! with it and the terminal hand-off builds an attached one, so the two
//! differ only in stdin.

use std::path::Path;

use crate::git::commit_files::CommitId;
use crate::git::runner::GitCommand;

/// Where `git commit` takes its message from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageSource<'a> {
    /// This file, `-` being stdin.
    File(&'a Path),
    /// `HEAD`'s own, when amending.
    Keep,
    /// `fixup! ` and the raw subject of this commit, written by git. The
    /// id is hex digits only, so it cannot be read as an option.
    Fixup(&'a CommitId),
}

/// `git commit` taking its message from `source`. Shared by the detached
/// run and the one on the terminal, whose stdin is the user's, so the two
/// differ only there. Its hooks and editor see the user's own env.
pub fn commit_command(amend: bool, no_verify: bool, source: MessageSource) -> GitCommand {
    let cmd = GitCommand::new("commit").user_env();
    let cmd = if amend { cmd.arg("--amend") } else { cmd };
    let cmd = if no_verify {
        cmd.arg("--no-verify")
    } else {
        cmd
    };
    match source {
        MessageSource::File(file) => cmd.arg("-F").arg(file),
        MessageSource::Keep => cmd.arg("--no-edit"),
        MessageSource::Fixup(target) => cmd.arg(format!("--fixup={}", target.as_str())),
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use crate::git::commit_files::CommitId;
    use crate::git::fake::{FakeRunner, fail, ok};
    use crate::git::runner::GitOutput;
    use crate::op::{GitOp, op_command, run_op};

    fn commit(message: &str, stage_all: bool) -> GitOp {
        GitOp::Commit {
            message: Some(message.into()),
            stage_all,
            amend: false,
            no_verify: false,
            fixup: None,
        }
    }

    #[test]
    fn amend_and_skip_hooks_add_their_flags_before_the_message_source() {
        let op = |message: Option<&str>, amend, no_verify| GitOp::Commit {
            message: message.map(Into::into),
            stage_all: false,
            amend,
            no_verify,
            fixup: None,
        };
        /// The op, its argv and its stdin.
        type Case = (GitOp, &'static [&'static str], Option<&'static [u8]>);
        let cases: [Case; 5] = [
            (
                op(Some("m\n"), false, false),
                &["commit", "-F", "-"],
                Some(b"m\n"),
            ),
            (
                op(Some("m\n"), true, false),
                &["commit", "--amend", "-F", "-"],
                Some(b"m\n"),
            ),
            (
                op(None, true, false),
                &["commit", "--amend", "--no-edit"],
                None,
            ),
            (
                op(Some("m\n"), false, true),
                &["commit", "--no-verify", "-F", "-"],
                Some(b"m\n"),
            ),
            (
                op(None, true, true),
                &["commit", "--amend", "--no-verify", "--no-edit"],
                None,
            ),
        ];
        for (op, argv, input) in cases {
            let runner = FakeRunner::new().expect(argv, ok(b""));
            assert_eq!(run_op(&runner, &op), Ok(()), "{op:?}");
            runner.assert_done();
            assert_eq!(runner.envs(), [vec![]], "{op:?}");
            assert_eq!(runner.inputs(), [input.map(<[u8]>::to_vec)], "{op:?}");
        }
    }

    /// One commit of `a.txt` as `one`, with an identity, no signing and only
    /// this repo's hooks, then `a.txt` changed to `two` and staged.
    fn committed_and_staged_repo() -> crate::git::fake::TempDir {
        use crate::git::fake::{TempDir, git};

        let tmp = TempDir::new("amend");
        let dir = tmp.path();
        git(dir, &["init", "-q", "--template="]);
        for (key, value) in [
            ("user.name", "Kika Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
            ("core.hooksPath", ".git/hooks"),
        ] {
            git(dir, &["config", key, value]);
        }
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "first\n\nbody"]);
        std::fs::write(dir.join("a.txt"), "two\n").unwrap();
        git(dir, &["add", "-A"]);
        tmp
    }

    #[test]
    fn amend_folds_the_staged_change_into_head_and_keeps_or_replaces_the_message_in_real_git() {
        use crate::git::fake::git;

        let tmp = committed_and_staged_repo();
        let dir = tmp.path();
        let runner = crate::git::CliRunner::isolated(dir);
        let amend = |message: Option<&str>| GitOp::Commit {
            message: message.map(Into::into),
            stage_all: false,
            amend: true,
            no_verify: false,
            fixup: None,
        };

        run_op(&runner, &amend(None)).unwrap();
        assert_eq!(git(dir, &["rev-list", "--count", "HEAD"]), b"1\n");
        assert_eq!(git(dir, &["show", "HEAD:a.txt"]), b"two\n");
        assert_eq!(
            git(dir, &["log", "-1", "--format=%B"]),
            b"first\n\nbody\n\n"
        );
        assert_eq!(git(dir, &["diff", "--cached"]), b"");

        run_op(&runner, &amend(Some("new\n"))).unwrap();
        assert_eq!(git(dir, &["rev-list", "--count", "HEAD"]), b"1\n");
        assert_eq!(git(dir, &["log", "-1", "--format=%B"]), b"new\n\n");
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_pre_commit_hook_blocks_a_commit_unless_hooks_are_skipped_in_real_git() {
        use crate::git::fake::git;
        use std::os::unix::fs::PermissionsExt;

        let tmp = committed_and_staged_repo();
        let dir = tmp.path();
        let hooks = dir.join(".git").join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("pre-commit");
        std::fs::write(&hook, "#!/bin/sh\necho 'lint failed'\nexit 1\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        let runner = crate::git::CliRunner::isolated(dir);
        let commit = |no_verify| GitOp::Commit {
            message: Some("second\n".into()),
            stage_all: false,
            amend: false,
            no_verify,
            fixup: None,
        };

        let blocked = run_op(&runner, &commit(false));
        assert!(
            blocked.as_ref().is_err_and(|e| e.contains("lint failed")),
            "{blocked:?}"
        );
        assert_eq!(git(dir, &["rev-list", "--count", "HEAD"]), b"1\n");

        run_op(&runner, &commit(true)).unwrap();
        assert_eq!(git(dir, &["rev-list", "--count", "HEAD"]), b"2\n");
        assert_eq!(git(dir, &["log", "-1", "--format=%B"]), b"second\n\n");
    }

    fn fixup_of(target: &str, no_verify: bool) -> GitOp {
        GitOp::Commit {
            message: None,
            stage_all: false,
            amend: false,
            no_verify,
            fixup: Some(CommitId::parse(target).unwrap()),
        }
    }

    #[test]
    fn a_fixup_commit_names_its_target_by_full_id_with_no_stdin() {
        let oid = "0123456789abcdef0123456789abcdef01234567";
        let fixup = format!("--fixup={oid}");
        for (no_verify, argv) in [
            (false, vec!["commit", fixup.as_str()]),
            (true, vec!["commit", "--no-verify", fixup.as_str()]),
        ] {
            let op = fixup_of(oid, no_verify);
            let cmd = op_command(&op);
            assert_eq!(
                cmd.args(),
                argv.iter().map(OsString::from).collect::<Vec<_>>()
            );
            assert_eq!(cmd.input(), None);
            assert_eq!(cmd.envs(), []);
            let runner = FakeRunner::new().expect(&argv, ok(b""));
            assert_eq!(run_op(&runner, &op), Ok(()));
            runner.assert_done();
            assert_eq!(runner.inputs(), [None]);
        }
    }

    #[test]
    fn a_fixup_commit_takes_its_target_s_raw_subject_in_real_git() {
        use crate::git::fake::git;

        let tmp = committed_and_staged_repo();
        let dir = tmp.path();
        // Bytes the log would make terminal-safe; git must copy them as is.
        let subject = "caf\u{e9} \x1b[1mbold\x1b[0m\ttab";
        git(dir, &["commit", "-q", "-m", subject]);
        let target = git(dir, &["rev-parse", "HEAD"]);
        let target = String::from_utf8(target).unwrap();
        std::fs::write(dir.join("b.txt"), "b\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "B"]);
        std::fs::write(dir.join("a.txt"), "fixed\n").unwrap();
        git(dir, &["add", "-A"]);
        let runner = crate::git::CliRunner::isolated(dir);

        run_op(&runner, &fixup_of(target.trim(), false)).unwrap();

        let made = git(dir, &["log", "-1", "--format=%s"]);
        assert_eq!(made, format!("fixup! {subject}\n").into_bytes());
        assert_eq!(git(dir, &["rev-list", "--count", "HEAD"]), b"4\n");
        assert_eq!(git(dir, &["diff", "--cached"]), b"");
    }

    #[test]
    fn every_message_source_runs_in_the_user_s_env() {
        use super::{MessageSource, commit_command};
        use crate::git::runner::EnvProfile;
        use std::path::Path;

        let target = CommitId::parse(&"a".repeat(40)).unwrap();
        for source in [
            MessageSource::File(Path::new("-")),
            MessageSource::Keep,
            MessageSource::Fixup(&target),
        ] {
            for (amend, no_verify) in [(false, false), (true, true)] {
                let cmd = commit_command(amend, no_verify, source);
                assert_eq!(cmd.env_profile(), EnvProfile::UserEnv, "{source:?}");
                assert_eq!(cmd.process_env(), [], "{source:?}");
            }
        }
        let runner = FakeRunner::new().expect(&["commit", "--amend", "--no-edit"], ok(b""));
        let keep = GitOp::Commit {
            message: None,
            stage_all: false,
            amend: true,
            no_verify: false,
            fixup: None,
        };
        assert_eq!(run_op(&runner, &keep), Ok(()));
        assert_eq!(runner.process_envs(), [vec![]]);
    }

    #[test]
    fn commit_is_plain_git_commit_with_the_message_on_stdin_and_no_env() {
        let runner = FakeRunner::new().expect(&["commit", "-F", "-"], ok(b""));
        assert_eq!(run_op(&runner, &commit("subj\n\nbody\n", false)), Ok(()));
        runner.assert_done();
        assert_eq!(runner.envs(), [vec![]]);
        assert_eq!(runner.inputs(), [Some(b"subj\n\nbody\n".to_vec())]);
    }

    #[test]
    fn commit_with_stage_all_runs_add_first() {
        let runner = FakeRunner::new()
            .expect(&["add", "-A"], ok(b""))
            .expect(&["commit", "-F", "-"], ok(b""));
        assert_eq!(run_op(&runner, &commit("subj\n", true)), Ok(()));
        runner.assert_done();
        assert_eq!(runner.inputs(), [None, Some(b"subj\n".to_vec())]);
    }

    #[test]
    fn a_failed_stage_all_returns_its_error_and_does_not_commit() {
        let runner = FakeRunner::new().expect(&["add", "-A"], fail("fatal: index.lock\n"));
        assert_eq!(
            run_op(&runner, &commit("subj\n", true)),
            Err("fatal: index.lock".into())
        );
        // The fake panics on any call not expected, so no commit was attempted.
        runner.assert_done();
    }

    #[test]
    fn a_failed_commit_shows_stderr_then_stdout_made_safe() {
        let output = GitOutput {
            success: false,
            stdout: b"nothing added to commit\n".to_vec(),
            stderr: b"lint \x1b[31mfailed\n".to_vec(),
        };
        let runner = FakeRunner::new().expect(&["commit", "-F", "-"], output);
        assert_eq!(
            run_op(&runner, &commit("subj\n", false)),
            Err("lint \u{FFFD}[31mfailed\nnothing added to commit".into())
        );

        let stdout_only = GitOutput {
            success: false,
            stdout: b"nothing to commit, working tree clean\n".to_vec(),
            stderr: Vec::new(),
        };
        let runner = FakeRunner::new().expect(&["commit", "-F", "-"], stdout_only);
        assert_eq!(
            run_op(&runner, &commit("subj\n", false)),
            Err("nothing to commit, working tree clean".into())
        );

        let runner = FakeRunner::new().expect(&["commit", "-F", "-"], fail(""));
        assert_eq!(
            run_op(&runner, &commit("subj\n", false)),
            Err("git commit failed".into())
        );
    }
}
