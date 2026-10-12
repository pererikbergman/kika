//! The I/O behind the Custom commands feature's effects: `Runtime::spawn`
//! calls these, and keeps the abort handle in its own slot.

use std::ffi::OsString;
use std::path::PathBuf;

use tokio::sync::mpsc::UnboundedSender;
use tokio::task::AbortHandle;

use super::captured::run_captured;
use super::command::{CommandFailure, OutputMode};
use crate::git::diff::terminal_safe;
use crate::intent::AppIntent;

/// Run a captured command in `workdir` and send `CommandFinished` back.
/// The returned handle cancels it.
pub(crate) fn run(
    workdir: PathBuf,
    tx: UnboundedSender<AppIntent>,
    title: String,
    argv: Vec<OsString>,
    output: OutputMode,
) -> AbortHandle {
    let task = tokio::spawn(run_captured(argv, workdir));
    let abort = task.abort_handle();
    tokio::spawn(async move {
        // A panic or a cancel still reports back, or
        // `command_running` would stay set.
        let result = task.await.unwrap_or_else(|e| {
            Err(if e.is_cancelled() {
                CommandFailure::Cancelled
            } else {
                CommandFailure::Launch(terminal_safe(
                    format!("The command crashed: {e}").as_bytes(),
                ))
            })
        });
        let result = match output {
            OutputMode::Popup => result,
            OutputMode::Terminal | OutputMode::None => result.map(|_| String::new()),
        };
        let _ = tx.send(AppIntent::CommandFinished {
            title,
            output,
            result,
        });
    });
    abort
}

/// Stop the running command, if there is one.
pub(crate) fn cancel(task: Option<AbortHandle>) {
    // Dropping the aborted future kills the program.
    if let Some(task) = task {
        task.abort();
    }
}
