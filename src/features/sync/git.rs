//! Background `git fetch`, so ahead/behind stays current. Opt-in only: it
//! reaches the network, so it runs only once the user has turned it on. It
//! must never stop to ask for a password: nobody is there to type it, and
//! the prompt would draw over the screen.

use crate::git::runner::{GitCommand, GitRunner};

/// Fetch every remote, quietly, failing instead of prompting for
/// credentials or an SSH passphrase. Only the locale: a fetch may gc and
/// run the `pre-auto-gc` hook, which keeps the user's other vars.
pub fn fetch_command() -> GitCommand {
    GitCommand::new("fetch")
        .arg("--all")
        .arg("--quiet")
        .arg("--no-write-fetch-head")
        .locale_env()
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("SSH_ASKPASS", "false")
}

/// Run [`fetch_command`]. A failure carries git's message, safe to draw.
pub fn run_fetch(runner: &dyn GitRunner) -> Result<(), String> {
    runner
        .run_checked(&fetch_command())
        .map(drop)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;
    use crate::git::fake::{FakeRunner, fail, ok};
    use crate::git::runner::EnvProfile;

    const ARGV: &[&str] = &["fetch", "--all", "--quiet", "--no-write-fetch-head"];

    #[test]
    fn fetch_never_prompts_and_runs_closed_to_input() {
        let fake = FakeRunner::new().expect(ARGV, ok(b""));

        assert_eq!(run_fetch(&fake), Ok(()));

        fake.assert_done();
        let env: Vec<(OsString, OsString)> = vec![
            ("LC_ALL".into(), "C".into()),
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
            ("SSH_ASKPASS_REQUIRE".into(), "force".into()),
            ("SSH_ASKPASS".into(), "false".into()),
        ];
        assert_eq!(fetch_command().env_profile(), Some(EnvProfile::Locale));
        assert_eq!(fake.process_envs(), [env]);
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
