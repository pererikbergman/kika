mod ansi;
mod app;
mod blame_view;
mod captured_command;
mod cli;
mod commit_editor;
mod config;
#[cfg(test)]
mod config_docs;
mod custom_command;
mod diff_pane;
mod diff_stream;
#[cfg(test)]
mod e2e;
mod effect;
mod external_diff;
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
mod rendered_view;
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
use crate::config::ConfigPaths;
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
    let global_config = config::global_path_from_env();
    // Read once here; the loop reloads the keys on focus-in.
    let config = config::load(&ConfigPaths::new(global_config.clone(), repo.as_deref()));
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
    let result = runtime.block_on(app::run(
        terminal,
        state,
        runner,
        recent_store,
        global_config,
        config,
    ));
    let _ = execute!(std::io::stdout(), DisableFocusChange);
    ratatui::restore();
    // Quitting interrupts git, but a process that ignores it must not keep
    // Kika from exiting: dropping the runtime would wait for it forever.
    runtime.shutdown_timeout(Duration::from_secs(1));
    result.map(|()| ExitCode::SUCCESS)
}
