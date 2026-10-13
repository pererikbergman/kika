//! The error a checked git run returns. Its constructors sanitise, so a
//! caller can draw the message as it is and never leaks a URL's token.

use std::fmt;
use std::io;

use super::diff::terminal_safe;
use super::remote::redact_userinfo;
use super::runner::{GitCommand, GitOutput};

/// Why a checked git run did not succeed. Every message is already safe to
/// draw and has the user part of any URL removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitError {
    /// git could not be started or its output could not be read.
    Io(String),
    /// git ran and exited with a failure.
    Failed { subcommand: String, message: String },
}

impl GitError {
    pub fn io(error: &io::Error) -> Self {
        Self::Io(safe(error.to_string().as_bytes()))
    }

    /// From git's stderr, the same way an op's failure is reported.
    pub fn failed(cmd: &GitCommand, output: &GitOutput) -> Self {
        Self::Failed {
            subcommand: subcommand(cmd),
            message: safe(&output.stderr),
        }
    }

    /// From both of git's streams, stderr first, for commands that report
    /// their outcome on stdout, such as a merge naming each conflict.
    pub fn failed_with_stdout(cmd: &GitCommand, output: &GitOutput) -> Self {
        let message = [&output.stderr, &output.stdout]
            .map(|bytes| safe(bytes))
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        Self::Failed {
            subcommand: subcommand(cmd),
            message,
        }
    }

    /// The message, or `fallback` where git said nothing, for callers whose
    /// fallback names more than the subcommand, such as `git worktree add`.
    pub fn message_or(&self, fallback: &str) -> String {
        match self {
            Self::Failed { message, .. } if message.is_empty() => fallback.to_string(),
            _ => self.to_string(),
        }
    }
}

fn subcommand(cmd: &GitCommand) -> String {
    safe(cmd.subcommand().to_string_lossy().as_bytes())
}

/// Sanitised first, so redaction sees the text as it will be drawn.
fn safe(bytes: &[u8]) -> String {
    redact_userinfo(&terminal_safe(bytes))
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message) => f.write_str(message),
            // git said nothing, so name what failed.
            Self::Failed {
                subcommand,
                message,
            } if message.is_empty() => write!(f, "git {subcommand} failed"),
            Self::Failed { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for GitError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::fake::fail;

    #[test]
    fn a_failure_shows_git_s_message_sanitised_and_without_credentials() {
        let cmd = GitCommand::new("fetch");
        let output = fail("fatal: https://u:tok@h/r \x1b[2J refused\n");

        let error = GitError::failed(&cmd, &output);
        let text = error.to_string();

        assert!(matches!(error, GitError::Failed { .. }));
        assert!(!text.contains("tok"), "{text}");
        assert!(!text.contains("u:"), "{text}");
        assert!(!text.contains('\x1b'), "{text}");
        assert!(text.contains("https://h/r"), "{text}");
        assert!(text.contains("refused"), "{text}");
    }

    #[test]
    fn a_silent_failure_names_the_subcommand_past_config_pairs() {
        let cmd = GitCommand::new("-c").arg("core.editor=true").arg("rebase");

        let error = GitError::failed(&cmd, &fail(""));

        assert_eq!(error.to_string(), "git rebase failed");
    }

    #[test]
    fn the_subcommand_is_sanitised_too() {
        let error = GitError::failed(&GitCommand::new("st\x1b[2Jatus"), &fail(""));

        assert!(!error.to_string().contains('\x1b'), "{error}");
    }

    #[test]
    fn a_failure_with_stdout_joins_both_streams_sanitised_and_redacted() {
        let cmd = GitCommand::new("merge");
        let output = GitOutput {
            success: false,
            stdout: b"CONFLICT in a.txt\nfrom https://u:tok@h/r\x07\n".to_vec(),
            stderr: b"fatal: \x1b[2J stopped\n".to_vec(),
        };

        let error = GitError::failed_with_stdout(&cmd, &output);

        assert!(matches!(error, GitError::Failed { .. }));
        assert_eq!(
            error.to_string(),
            "fatal: \u{FFFD}[2J stopped\nCONFLICT in a.txt\nfrom https://h/r\u{FFFD}"
        );
    }

    #[test]
    fn a_failure_with_stdout_drops_an_empty_stream_and_falls_back_when_both_are() {
        let cmd = GitCommand::new("-c").arg("k=v").arg("revert");
        let only_stdout = GitOutput {
            success: false,
            stdout: b"nothing to commit\n".to_vec(),
            stderr: b" \n".to_vec(),
        };

        let error = GitError::failed_with_stdout(&cmd, &only_stdout);
        let silent = GitError::failed_with_stdout(&cmd, &fail(""));

        assert_eq!(error.to_string(), "nothing to commit");
        assert_eq!(silent.to_string(), "git revert failed");
    }

    #[test]
    fn message_or_replaces_only_an_empty_failure() {
        let cmd = GitCommand::new("worktree").arg("add");
        let fallback = "git worktree add failed";

        let silent = GitError::failed(&cmd, &fail(""));
        let loud = GitError::failed(&cmd, &fail("fatal: exists\n"));
        let io = GitError::Io(String::new());

        assert_eq!(silent.message_or(fallback), fallback);
        assert_eq!(loud.message_or(fallback), "fatal: exists");
        assert_eq!(io.message_or(fallback), "");
    }

    #[test]
    fn an_io_error_is_sanitised_and_redacted() {
        let io = io::Error::other("cannot reach https://u:tok@h/r\x07");

        let error = GitError::io(&io);

        assert!(matches!(error, GitError::Io(_)));
        let text = error.to_string();
        assert!(!text.contains("tok") && !text.contains('\x07'), "{text}");
    }
}
