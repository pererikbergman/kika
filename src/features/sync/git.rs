//! Background `git fetch`, so ahead/behind stays current. Opt-in only: it
//! reaches the network, so it runs only once the user has turned it on. It
//! must never stop to ask for a password: nobody is there to type it, and
//! the prompt would draw over the screen.

use kika_git::runner::{GitCommand, GitRunner};

/// Fetch every remote, quietly, failing instead of prompting for
/// credentials or an SSH passphrase. `FETCH_HEAD` is kept as it is by
/// `--no-write-fetch-head`, passed when `no_write_fetch_head` (git 2.29
/// and later; older git has no such flag and would fail every fetch).
/// Only the locale: a fetch may gc and run the `pre-auto-gc` hook, which
/// keeps the user's other vars.
pub fn fetch_command(no_write_fetch_head: bool) -> GitCommand {
    let cmd = GitCommand::new("fetch").arg("--all").arg("--quiet");
    let cmd = if no_write_fetch_head {
        cmd.arg("--no-write-fetch-head")
    } else {
        cmd
    };
    cmd.locale_env()
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("SSH_ASKPASS", "false")
}

/// Run [`fetch_command`]. A failure carries git's message, safe to draw.
pub fn run_fetch(runner: &dyn GitRunner, no_write_fetch_head: bool) -> Result<(), String> {
    runner
        .run_checked(&fetch_command(no_write_fetch_head))
        .map(drop)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;
    use kika_git::fake::{FakeRunner, fail, ok};
    use kika_git::runner::EnvProfile;

    const ARGV: &[&str] = &["fetch", "--all", "--quiet", "--no-write-fetch-head"];
    /// Git before 2.29 has no `--no-write-fetch-head`.
    const OLD_GIT_ARGV: &[&str] = &["fetch", "--all", "--quiet"];

    /// Each argv with the flag that gives it.
    const CASES: [(&[&str], bool); 2] = [(ARGV, true), (OLD_GIT_ARGV, false)];

    #[test]
    fn fetch_never_prompts_and_runs_closed_to_input() {
        for (argv, no_write_fetch_head) in CASES {
            let fake = FakeRunner::new().expect(argv, ok(b""));

            assert_eq!(run_fetch(&fake, no_write_fetch_head), Ok(()));

            fake.assert_done();
            let env: Vec<(OsString, OsString)> = vec![
                ("LC_ALL".into(), "C".into()),
                ("GIT_TERMINAL_PROMPT".into(), "0".into()),
                ("SSH_ASKPASS_REQUIRE".into(), "force".into()),
                ("SSH_ASKPASS".into(), "false".into()),
            ];
            assert_eq!(
                fetch_command(no_write_fetch_head).env_profile(),
                Some(EnvProfile::Locale)
            );
            assert_eq!(fake.process_envs(), [env]);
            assert_eq!(fake.inputs(), [None]);
        }
    }

    #[test]
    fn a_failure_returns_gits_message_made_safe() {
        for (argv, no_write_fetch_head) in CASES {
            let fake = FakeRunner::new().expect(argv, fail("fatal: \x1b[31mno route\n"));
            assert_eq!(
                run_fetch(&fake, no_write_fetch_head),
                Err("fatal: \u{FFFD}[31mno route".into())
            );

            let fake = FakeRunner::new().expect(argv, fail(""));
            assert_eq!(
                run_fetch(&fake, no_write_fetch_head),
                Err("git fetch failed".into())
            );
        }
    }
}
