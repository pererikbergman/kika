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
mod recent;
mod recent_list;
mod refresh;
mod submodule_list;
mod view;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;

use crate::cli::Cli;
use crate::git::{CliRunner, repo_root, version};
use crate::model::AppState;

fn main() -> color_eyre::Result<ExitCode> {
    // Install before ratatui::init so the terminal is restored ahead of the report.
    color_eyre::install()?;
    let cli = Cli::parse();
    let launch = std::env::current_dir()?;
    let probe = CliRunner::new(&launch);
    // Before the terminal is taken, so the refusal is plain text on exit.
    if let Err(message) = version::check(&probe) {
        eprintln!("{message}");
        return Ok(ExitCode::FAILURE);
    }
    // Once, before anything runs: status paths are root-relative, so every
    // git command runs at the top level whichever subdirectory Kika started in.
    let repo = repo_root::find_toplevel(&probe);
    let workdir = repo.clone().unwrap_or_else(|| launch.clone());
    let runner = Arc::new(CliRunner::new(workdir));
    let state = AppState::new(cli.context()).with_location(repo, launch);
    let recent_store = recent::default_path(|key| std::env::var_os(key));
    let runtime = tokio::runtime::Runtime::new()?;

    let terminal = ratatui::init();
    let result = runtime.block_on(app::run(terminal, state, runner, recent_store));
    ratatui::restore();
    // Quitting interrupts git, but a process that ignores it must not keep
    // Kika from exiting: dropping the runtime would wait for it forever.
    runtime.shutdown_timeout(Duration::from_secs(1));
    result.map(|()| ExitCode::SUCCESS)
}
