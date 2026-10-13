mod app;
mod background;
mod bisect;
mod cli;
mod clipboard;
mod config;
#[cfg(test)]
mod config_docs;
mod diff_mark;
mod diff_pane;
mod diff_pane_view;
mod diff_search;
#[cfg(test)]
mod e2e;
mod effect;
mod features;
mod filtered_list;
mod forge;
mod git;
mod handoff;
mod hunk_view;
mod intent;
mod keymap;
mod layout;
mod list_filter;
mod list_input;
mod menu;
mod model;
mod op;
mod op_slot;
mod pick_list;
mod popup;
mod popup_view;
mod program;
mod prompts;
mod range_delete;
mod refresh;
mod reload;
mod remote_run;
mod rows;
mod sequence_editor;
#[cfg(test)]
mod test_support;
mod view;
mod watch;
mod worktree_file;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use crossterm::event::{DisableFocusChange, EnableFocusChange};
use crossterm::execute;

use crate::cli::Cli;
use crate::config::ConfigPaths;
use crate::git::recording::{CommandSink, RecordingRunner};
use crate::git::retry::RetryRunner;
use crate::git::{CliRunner, repo_root, version};
use crate::model::AppState;

fn main() -> color_eyre::Result<ExitCode> {
    // Hidden modes, each keyed on its own env var and run before anything
    // else. Kika sets each only on the one git command that needs it, and
    // strips both from every other (`CliRunner`), so one never reaches the
    // other's child.
    // Run by git or ssh as their askpass program, with the prompt as the
    // only argument, which `Cli::parse` would reject.
    #[cfg(unix)]
    if let Some(socket) = std::env::var_os(git::askpass::SOCKET_ENV) {
        let prompt = std::env::args_os().nth(1).unwrap_or_default();
        let kind = std::env::var_os("SSH_ASKPASS_PROMPT");
        return Ok(git::askpass::client(
            std::path::Path::new(&socket),
            &prompt,
            kind.as_deref(),
            &mut std::io::stdout().lock(),
        ));
    }
    // Run by git as the sequence editor of Kika's own interactive rebase.
    if let Some(code) = sequence_editor::dispatch() {
        return Ok(code);
    }
    // Install before ratatui::init so the terminal is restored ahead of the report.
    color_eyre::install()?;
    let cli = Cli::parse();
    let launch = std::env::current_dir()?;
    let probe = CliRunner::new(&launch);
    // Before the terminal is taken, so the refusal is plain text on exit.
    let git_version = match version::check(&probe) {
        Ok(found) => found,
        Err(message) => {
            eprintln!("{message}");
            return Ok(ExitCode::FAILURE);
        }
    };
    // Once, before anything runs: status paths are root-relative, so every
    // git command runs at the top level whichever subdirectory Kika started in.
    let repo = repo_root::find_toplevel(&probe);
    let workdir = repo.clone().unwrap_or_else(|| launch.clone());
    let global_config = config::global_path_from_env();
    // Read once here; the loop reloads the keys on focus-in.
    let config = config::load(&ConfigPaths::new(global_config.clone(), repo.as_deref()));
    // Once, for the terminal Kika runs in; it cannot change mid-session.
    let env = |name| std::env::var(name).ok();
    let emoji_zwj = config::emoji_zwj(
        &config.emoji_zwj_terminals,
        env("TERM_PROGRAM").as_deref(),
        env("TERM").as_deref(),
    );
    let cli_runner = CliRunner::new(workdir);
    // Every process is logged, each retry too; the loop connects the sink.
    let sink = CommandSink::new();
    let recording = RecordingRunner::new(Arc::new(cli_runner), sink.clone());
    // Another process holding a lock for a moment is not worth an error.
    let runner = Arc::new(RetryRunner::new(Arc::new(recording)));
    let mut state = AppState::new(cli.context())
        .with_location(repo, launch)
        .with_git_version(git_version)
        .with_forge_hosts(config.forge_hosts.clone())
        .with_forge_links(config.forge_gitlab_hosts.clone(), config.browser.clone());
    state.emoji_zwj = emoji_zwj;
    let recent_store = features::repos::recent::default_path(|key| std::env::var_os(key));
    let runtime = tokio::runtime::Runtime::new()?;

    let terminal = ratatui::init();
    // Focus-in is a cue that the repository may have changed elsewhere.
    // A terminal that cannot report focus still has the timers.
    let _ = execute!(std::io::stdout(), EnableFocusChange);
    let result = runtime.block_on(app::run(
        terminal,
        state,
        runner,
        sink,
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
