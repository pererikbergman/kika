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
            subcommand: safe(cmd.subcommand().to_string_lossy().as_bytes()),
            message: safe(&output.stderr),
        }
    }
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
    fn an_io_error_is_sanitised_and_redacted() {
        let io = io::Error::other("cannot reach https://u:tok@h/r\x07");

        let error = GitError::io(&io);

        assert!(matches!(error, GitError::Io(_)));
        let text = error.to_string();
        assert!(!text.contains("tok") && !text.contains('\x07'), "{text}");
    }
}
