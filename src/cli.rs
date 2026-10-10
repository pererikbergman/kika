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
    /// Open a pull/merge request for review.
    Review {
        /// The pull/merge request number.
        number: u64,
    },
}

impl Cli {
    /// The context Kika starts in.
    pub fn context(&self) -> Context {
        match self.command {
            Some(Command::Review { number }) => Context::Review { number },
            None => Context::Workspace,
        }
    }
}
