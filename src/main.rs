mod app;
mod cli;
mod intent;
mod model;
mod view;

use clap::Parser;

use crate::cli::Cli;
use crate::model::AppState;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    // Install before ratatui::init so the terminal is restored ahead of the report.
    color_eyre::install()?;
    let cli = Cli::parse();

    let terminal = ratatui::init();
    let result = app::run(terminal, AppState::new(cli.context())).await;
    ratatui::restore();
    result
}
