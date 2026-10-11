mod app;
mod blame_view;
mod cli;
mod commit_editor;
mod diff_pane;
mod diff_stream;
#[cfg(test)]
mod e2e;
mod effect;
mod git;
mod handoff;
mod hunk_view;
mod intent;
mod keymap;
mod menu;
mod model;
mod popup;
mod popup_view;
mod refresh;
mod view;

use std::sync::Arc;
use std::time::Duration;

use clap::Parser;

use crate::cli::Cli;
use crate::git::{CliRunner, repo_root};
use crate::model::AppState;

fn main() -> color_eyre::Result<()> {
    // Install before ratatui::init so the terminal is restored ahead of the report.
    color_eyre::install()?;
    let cli = Cli::parse();
    // Once, before anything runs: status paths are root-relative, so every
    // git command runs at the top level whichever subdirectory Kika started in.
    let launch = std::env::current_dir()?;
    let workdir = repo_root::resolve_workdir(&CliRunner::new(&launch), launch);
    let runner = Arc::new(CliRunner::new(workdir));
    // Editors run where git does, so relative names mean the same to both.
    let workdir = runner.workdir().to_path_buf();
    let runtime = tokio::runtime::Runtime::new()?;

    let terminal = ratatui::init();
    let result = runtime.block_on(app::run(
        terminal,
        AppState::new(cli.context()),
        runner,
        workdir,
    ));
    ratatui::restore();
    // Quitting interrupts git, but a process that ignores it must not keep
    // Kika from exiting: dropping the runtime would wait for it forever.
    runtime.shutdown_timeout(Duration::from_secs(1));
    result
}
