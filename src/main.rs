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
mod layout;
mod menu;
mod model;
mod popup;
mod popup_view;
mod recent;
mod recent_list;
mod refresh;
mod submodule_list;
mod view;
mod watch;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use crossterm::event::{DisableFocusChange, EnableFocusChange};
use crossterm::execute;

use crate::cli::Cli;
use crate::git::retry::RetryRunner;
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
    let cli_runner = CliRunner::new(workdir);
    // Another process holding a lock for a moment is not worth an error.
    let runner = Arc::new(RetryRunner::new(Arc::new(cli_runner)));
    let state = AppState::new(cli.context()).with_location(repo, launch);
    let recent_store = recent::default_path(|key| std::env::var_os(key));
    let runtime = tokio::runtime::Runtime::new()?;

    let terminal = ratatui::init();
    // Focus-in is a cue that the repository may have changed elsewhere.
    // A terminal that cannot report focus still has the timers.
    let _ = execute!(std::io::stdout(), EnableFocusChange);
    let result = runtime.block_on(app::run(terminal, state, runner, recent_store));
    let _ = execute!(std::io::stdout(), DisableFocusChange);
    ratatui::restore();
    // Quitting interrupts git, but a process that ignores it must not keep
    // Kika from exiting: dropping the runtime would wait for it forever.
    runtime.shutdown_timeout(Duration::from_secs(1));
    result.map(|()| ExitCode::SUCCESS)
}
