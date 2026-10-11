//! Background `git fetch`, so ahead/behind stays current without the user
//! asking. It must never stop to ask for a password: nobody is there to type
//! it, and the prompt would draw over the screen.

use super::diff::terminal_safe;
use super::runner::{GitCommand, GitRunner};

/// Fetch every remote, quietly, failing instead of prompting for
/// credentials or an SSH passphrase.
pub fn fetch_command() -> GitCommand {
    GitCommand::new("fetch")
        .arg("--all")
        .arg("--quiet")
        .arg("--no-write-fetch-head")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("SSH_ASKPASS", "false")
        .env("LC_ALL", "C")
}

/// Run [`fetch_command`]. A failure carries git's message, safe to draw.
pub fn run_fetch(runner: &dyn GitRunner) -> Result<(), String> {
    let output = runner
        .run(&fetch_command())
        .map_err(|e| terminal_safe(e.to_string().as_bytes()))?;
    if output.success {
        return Ok(());
    }
    let message = terminal_safe(&output.stderr);
    Err(if message.is_empty() {
        "git fetch failed".into()
    } else {
        message
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;
    use crate::git::fake::{FakeRunner, fail, ok};

    const ARGV: &[&str] = &["fetch", "--all", "--quiet", "--no-write-fetch-head"];

    #[test]
    fn fetch_never_prompts_and_runs_closed_to_input() {
        let fake = FakeRunner::new().expect(ARGV, ok(b""));

        assert_eq!(run_fetch(&fake), Ok(()));

        fake.assert_done();
        let env: Vec<(OsString, OsString)> = vec![
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
            ("SSH_ASKPASS_REQUIRE".into(), "force".into()),
            ("SSH_ASKPASS".into(), "false".into()),
            ("LC_ALL".into(), "C".into()),
        ];
        assert_eq!(fake.envs(), [env]);
        assert_eq!(fake.inputs(), [None]);
    }

    #[test]
    fn a_failure_returns_gits_message_made_safe() {
        let fake = FakeRunner::new().expect(ARGV, fail("fatal: \x1b[31mno route\n"));
        assert_eq!(run_fetch(&fake), Err("fatal: \u{FFFD}[31mno route".into()));

        let fake = FakeRunner::new().expect(ARGV, fail(""));
        assert_eq!(run_fetch(&fake), Err("git fetch failed".into()));
    }
}
