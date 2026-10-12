use clap::{Parser, Subcommand};

use crate::model::Context;

#[derive(Debug, Parser)]
#[command(name = "kika", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Open a GitHub pull request for review.
    Review {
        /// The pull request number.
        number: u64,
    },
}

impl Cli {
    /// The context Kika starts in.
    pub fn context(&self) -> Context {
        match self.command {
            Some(Command::Review { number }) => Context::Review {
                number: Some(number),
            },
            None => Context::Workspace,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_with_a_number_starts_on_that_pull_request() {
        let cli = Cli::try_parse_from(["kika", "review", "42"]).unwrap();
        assert_eq!(cli.context(), Context::Review { number: Some(42) });
        let cli = Cli::try_parse_from(["kika"]).unwrap();
        assert_eq!(cli.context(), Context::Workspace);
        assert!(Cli::try_parse_from(["kika", "review", "x"]).is_err());
    }
}
