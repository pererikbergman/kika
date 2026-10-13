//! A commit's full message: the last one's for amending it, any one's
//! for copying it.

use super::commit_files::CommitId;
use super::runner::{GitCommand, GitRunner};

/// `HEAD`'s full message, as the commit popup shows it to be edited.
pub fn read_head_message(runner: &dyn GitRunner) -> Result<String, String> {
    read_message(runner, "HEAD")
}

/// The full message of commit `oid`, which is hex, so git cannot read it
/// as an option.
pub fn read_commit_message(runner: &dyn GitRunner, oid: &CommitId) -> Result<String, String> {
    read_message(runner, oid.as_str())
}

/// The full message of `rev`, `HEAD` or a commit id. Decoded lossily,
/// without trailing newlines, and with every control character but `\n`
/// dropped, because the text is drawn as it is.
fn read_message(runner: &dyn GitRunner, rev: &str) -> Result<String, String> {
    let cmd = GitCommand::new("log")
        .arg("-1")
        .arg("--format=%B")
        .arg(rev)
        .arg("--")
        .standard_env();
    let output = runner
        .run_checked(&cmd)
        .map_err(|e| e.message_or("git log failed"))?;
    let message: String = String::from_utf8_lossy(&output.stdout)
        .chars()
        .filter(|&c| c == '\n' || !c.is_control())
        .collect();
    Ok(message.trim_end_matches('\n').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::fake::{FakeRunner, fail, ok};

    const ARGV: &[&str] = &["log", "-1", "--format=%B", "HEAD", "--"];

    #[test]
    fn the_message_comes_back_without_trailing_newlines() {
        let runner = FakeRunner::new().expect(ARGV, ok(b"subj\n\nbody\nmore\n\n"));
        assert_eq!(read_head_message(&runner), Ok("subj\n\nbody\nmore".into()));
        runner.assert_done();
        assert_eq!(runner.inputs(), [None]);
        // No path after `--`, so the whole standard profile fits.
        assert_eq!(
            runner.process_envs(),
            [crate::test_support::env_list(
                &crate::test_support::STANDARD_ENV
            )]
        );
    }

    #[test]
    fn control_characters_are_dropped_but_newlines_kept_and_bad_utf8_decoded_lossily() {
        let runner = FakeRunner::new().expect(ARGV, ok(b"s\x1b[2Jubj\r\n\n\tb\xffody\n"));
        assert_eq!(
            read_head_message(&runner),
            Ok("s[2Jubj\n\nb\u{FFFD}ody".into())
        );
    }

    #[test]
    fn a_failure_returns_gits_message_made_safe() {
        let runner = FakeRunner::new().expect(ARGV, fail("fatal: bad \x1b[31mHEAD\n"));
        assert_eq!(
            read_head_message(&runner),
            Err("fatal: bad \u{FFFD}[31mHEAD".into())
        );
        let runner = FakeRunner::new().expect(ARGV, fail(""));
        assert_eq!(read_head_message(&runner), Err("git log failed".into()));
    }

    #[test]
    fn a_commit_message_is_read_by_its_id() {
        let oid = CommitId::parse("0123abcd").unwrap();
        let runner = FakeRunner::new().expect(
            &["log", "-1", "--format=%B", "0123abcd", "--"],
            ok(b"subj\n\nbody\n"),
        );
        assert_eq!(
            read_commit_message(&runner, &oid),
            Ok("subj\n\nbody".into())
        );
        runner.assert_done();
    }

    #[test]
    fn the_message_of_a_real_commit_is_read() {
        use crate::git::fake::{TempDir, git};

        let tmp = TempDir::new("head");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "subj\n\nbody"]);
        let runner = crate::git::CliRunner::isolated(dir);
        assert_eq!(read_head_message(&runner), Ok("subj\n\nbody".into()));
    }
}
