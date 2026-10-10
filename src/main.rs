mod app;
mod cli;
mod commit_editor;
mod diff_pane;
mod diff_stream;
#[cfg(test)]
mod e2e;
mod effect;
mod git;
mod intent;
mod keymap;
mod model;
mod refresh;
mod view;

use std::sync::Arc;

use clap::Parser;

use crate::cli::Cli;
use crate::git::CliRunner;
use crate::model::AppState;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    // Install before ratatui::init so the terminal is restored ahead of the report.
    color_eyre::install()?;
    let cli = Cli::parse();
    let runner = Arc::new(CliRunner::new(std::env::current_dir()?));

    let terminal = ratatui::init();
    let result = app::run(terminal, AppState::new(cli.context()), runner).await;
    ratatui::restore();
    result
}
