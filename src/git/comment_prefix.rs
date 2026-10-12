//! The comment prefix git uses in a rebase todo list, read through
//! `git config` so includes, `-c` and every scope apply as git applies
//! them. Lines led by it are skipped by git, so Kika must skip them too
//! and nothing else.

use super::diff::terminal_safe;
use super::runner::{GitCommand, GitRunner};

/// The prefix git uses when nothing else is set, and for `auto`: git
/// picks a free character only for commit messages, never for the todo.
pub const DEFAULT: &[u8] = b"#";

/// Every `core.commentChar` and `core.commentString` entry, in the order
/// git reads them, `-c` last. No file option, so every scope is read.
pub fn comment_config_command() -> GitCommand {
    GitCommand::new("config")
        .arg("-z")
        .arg("--get-regexp")
        .arg(r"^core\.comment(char|string)$")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
}

/// The effective prefix from [`comment_config_command`]'s output, as a
/// git that reads `core.commentString` (`string_too`, git 2.45) or not
/// would take it. Entries are `key\nvalue\0`; the last one git reads
/// wins. A value git refuses is an error even when a later entry would
/// win, since git stops on it.
pub fn parse_comment_config(stdout: &[u8], string_too: bool) -> Result<Vec<u8>, String> {
    let mut prefix = DEFAULT.to_vec();
    for entry in stdout.split(|&b| b == 0).filter(|entry| !entry.is_empty()) {
        let (key, value) = match entry.iter().position(|&b| b == b'\n') {
            Some(at) => (&entry[..at], Some(&entry[at + 1..])),
            None => (entry, None),
        };
        let key_name = terminal_safe(key);
        match key {
            b"core.commentchar" => {}
            b"core.commentstring" if string_too => {}
            // An older git does not know the key and skips it.
            b"core.commentstring" => continue,
            _ => return Err(format!("unexpected config key {key_name}")),
        }
        let Some(value) = value else {
            return Err(format!("{key_name} has no value"));
        };
        prefix = if value.eq_ignore_ascii_case(b"auto") {
            DEFAULT.to_vec()
        } else if value.is_empty() {
            return Err(format!("{key_name} is empty"));
        } else if !string_too && value.len() != 1 {
            return Err(format!("{key_name} should be one character"));
        } else if value.contains(&b'\n') {
            return Err(format!("{key_name} has a newline"));
        } else {
            value.to_vec()
        };
    }
    Ok(prefix)
}

/// The comment prefix git will use for the todo list. Blocking; run it
/// from a task.
pub fn read_comment_prefix(runner: &dyn GitRunner, string_too: bool) -> Result<Vec<u8>, String> {
    let output = runner
        .run(&comment_config_command())
        .map_err(|e| terminal_safe(e.to_string().as_bytes()))?;
    if output.success {
        return parse_comment_config(&output.stdout, string_too);
    }
    // Nothing set is a failure that prints nothing.
    if output.stdout.is_empty() && output.stderr.is_empty() {
        return Ok(DEFAULT.to_vec());
    }
    let message = terminal_safe(&output.stderr);
    Err(if message.is_empty() {
        "git config failed".into()
    } else {
        message
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::CliRunner;
    use crate::git::fake::{FakeRunner, TempDir, fail, git, ok};

    fn parse(stdout: &[u8], string_too: bool) -> Result<Vec<u8>, String> {
        parse_comment_config(stdout, string_too)
    }

    #[test]
    fn nothing_set_is_hash() {
        assert_eq!(parse(b"", false), Ok(b"#".to_vec()));
        assert_eq!(parse(b"", true), Ok(b"#".to_vec()));
    }

    #[test]
    fn a_comment_char_is_used_and_auto_is_hash() {
        for string_too in [false, true] {
            assert_eq!(
                parse(b"core.commentchar\n;\0", string_too),
                Ok(b";".to_vec())
            );
            assert_eq!(
                parse(b"core.commentchar\nauto\0", string_too),
                Ok(b"#".to_vec())
            );
            assert_eq!(
                parse(b"core.commentchar\nAUTO\0", string_too),
                Ok(b"#".to_vec())
            );
            assert_eq!(
                parse(b"core.commentchar\n;\0core.commentchar\nauto\0", string_too),
                Ok(b"#".to_vec())
            );
        }
    }

    #[test]
    fn the_last_entry_wins() {
        assert_eq!(
            parse(b"core.commentchar\n;\0core.commentchar\n%\0", false),
            Ok(b"%".to_vec())
        );
        assert_eq!(
            parse(b"core.commentchar\n;\0core.commentstring\n//\0", true),
            Ok(b"//".to_vec())
        );
        assert_eq!(
            parse(b"core.commentstring\n//\0core.commentchar\n;\0", true),
            Ok(b";".to_vec())
        );
    }

    #[test]
    fn a_comment_string_counts_only_for_a_git_that_reads_it() {
        assert_eq!(parse(b"core.commentstring\n//\0", true), Ok(b"//".to_vec()));
        assert_eq!(parse(b"core.commentstring\n//\0", false), Ok(b"#".to_vec()));
        assert_eq!(
            parse(b"core.commentchar\n;\0core.commentstring\n//\0", false),
            Ok(b";".to_vec())
        );
        assert_eq!(parse(b"core.commentchar\n;;\0", true), Ok(b";;".to_vec()));
    }

    #[test]
    fn values_git_refuses_are_errors() {
        assert!(parse(b"core.commentchar\n;;\0", false).is_err());
        assert!(parse("core.commentchar\n\u{e9}\0".as_bytes(), false).is_err());
        for string_too in [false, true] {
            for bad in [
                &b"core.commentchar\n\0"[..],
                b"core.commentchar\0",
                b"core.commentchar\n;\n;\0",
                b"core.commentchar\n\n\0",
                // Git stops on a bad entry, whatever comes after it.
                b"core.commentchar\n\0core.commentchar\n;\0",
                b"core.other\n;\0",
            ] {
                assert!(parse(bad, string_too).is_err(), "{bad:?} {string_too}");
            }
        }
        for bad in [&b"core.commentstring\n\0"[..], b"core.commentstring\0"] {
            assert!(parse(bad, true).is_err(), "{bad:?}");
            assert_eq!(parse(bad, false), Ok(b"#".to_vec()), "{bad:?}");
        }
        let why = parse(b"core.commentchar\x1b[2J\0", true).unwrap_err();
        assert!(!why.contains('\x1b'), "{why}");
    }

    const ARGV: [&str; 4] = [
        "config",
        "-z",
        "--get-regexp",
        r"^core\.comment(char|string)$",
    ];

    #[test]
    fn the_read_runs_one_config_command_without_a_lock() {
        let runner = FakeRunner::new()
            .expect(&ARGV, fail(""))
            .expect(&ARGV, ok(b"core.commentchar\n;\0"))
            .expect(&ARGV, ok(b"core.commentstring\n//\0"));
        assert_eq!(read_comment_prefix(&runner, true), Ok(b"#".to_vec()));
        assert_eq!(read_comment_prefix(&runner, true), Ok(b";".to_vec()));
        assert_eq!(read_comment_prefix(&runner, false), Ok(b"#".to_vec()));
        runner.assert_done();
        for env in runner.envs() {
            assert!(
                env.contains(&("GIT_OPTIONAL_LOCKS".into(), "0".into())),
                "{env:?}"
            );
            assert!(env.contains(&("LC_ALL".into(), "C".into())), "{env:?}");
        }
    }

    #[test]
    fn any_other_failure_is_an_error() {
        let runner = FakeRunner::new()
            .expect(&ARGV, fail("fatal: bad config line 3 \x1b[2J\n"))
            .expect(&ARGV, ok(b"core.commentchar\0"));
        assert_eq!(
            read_comment_prefix(&runner, true),
            Err("fatal: bad config line 3 \u{FFFD}[2J".into())
        );
        assert!(read_comment_prefix(&runner, true).is_err());
        runner.assert_done();
    }

    #[test]
    fn real_git_gives_the_prefix_it_uses() {
        let tmp = TempDir::new("comment-prefix");
        git(tmp.path(), &["init", "-q"]);
        let runner = CliRunner::isolated(tmp.path());
        assert_eq!(read_comment_prefix(&runner, true), Ok(b"#".to_vec()));
        git(tmp.path(), &["config", "core.commentChar", ";"]);
        assert_eq!(read_comment_prefix(&runner, false), Ok(b";".to_vec()));
        git(tmp.path(), &["config", "core.commentChar", "auto"]);
        assert_eq!(read_comment_prefix(&runner, false), Ok(b"#".to_vec()));
        git(tmp.path(), &["config", "core.commentChar", ""]);
        assert!(read_comment_prefix(&runner, false).is_err());
    }

    /// Needs git 2.45 for `core.commentString`; an older git still stores
    /// the key, so only the reading side depends on the version.
    #[test]
    fn real_git_comment_string_wins_when_set_last() {
        let tmp = TempDir::new("comment-string");
        git(tmp.path(), &["init", "-q"]);
        git(tmp.path(), &["config", "core.commentChar", ";"]);
        git(tmp.path(), &["config", "core.commentString", "//"]);
        let runner = CliRunner::isolated(tmp.path());
        assert_eq!(read_comment_prefix(&runner, true), Ok(b"//".to_vec()));
        assert_eq!(read_comment_prefix(&runner, false), Ok(b";".to_vec()));
        let with_c = CliRunner::isolated(tmp.path())
            .with_env("GIT_CONFIG_PARAMETERS", "'core.commentChar=%'");
        assert_eq!(read_comment_prefix(&with_c, true), Ok(b"%".to_vec()));
    }
}
