//! Custom commands: user-defined commands from the global config. The
//! feature owns parsing a command table and expanding its placeholders to
//! argv (`command.rs`), the command waiting for its prompts (`pending.rs`),
//! asking the prompts and running the command in the terminal or captured
//! (`update.rs`), the captured runner (`captured.rs`), the cancel key
//! (`keys.rs`) and the I/O behind `Effect::RunCommand` and
//! `Effect::CancelCommand` (`effects.rs`).
//!
//! Security: programs come only from the global config file, and they run
//! as argv, never as a shell string.
//!
//! Its `pub(crate)` API: the types [`CommandFailure`], [`CommandTable`],
//! [`CustomCommand`], [`OutputMode`] and [`PendingCommand`]; the handlers
//! `start_command`, `command_answered`, `command_finished` and
//! `cancel_command` on `AppState`; the chunk `keys::CUSTOM_COMMANDS_WORKSPACE`;
//! and `effects::{run, cancel}`.
//!
//! Kept cross-feature reads and calls, left as they are because this slice
//! only moves code:
//! - `command_target` reads `selected` and `files` (files), the `HunkView`
//!   in `Focus::Diff` (diff) and the `BlameView` in `Focus::Blame` (blame);
//! - `command_finished` calls `refresh_status` (files).
//!
//! What stays in core: the command engine in `keymap.rs` (custom-command
//! resolution, `available_commands`, the clash checks and `REPO_LOCKED`);
//! loading `[[commands]]` from the global file only, in `config.rs`;
//! `HandOff::RunCommand` in `handoff.rs`; the `AppState` fields
//! `pending_command` and `command_running`; and the command branches in
//! shared core code (`Resolution::Command`, the `PromptKind::Command`
//! cleanup in `PopPopup`, `prompt_title`, `switch_repo`, the help height and
//! the running-command bar hint).

mod captured;
mod command;
pub(crate) mod effects;
pub(crate) mod keys;
mod pending;
mod update;

pub(crate) use command::{CommandFailure, CommandTable, CustomCommand, OutputMode};
pub(crate) use pending::PendingCommand;

#[cfg(all(test, unix))]
pub(crate) use captured::run_captured;
#[cfg(test)]
pub(crate) use command::{Panel, Part, Target, test_table};
