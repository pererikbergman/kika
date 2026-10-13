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
//! Its intents are nested (ADR-0002): core wraps [`CustomCommandsIntent`] in
//! `AppIntent::CustomCommands`, and its one `update` arm calls [`update`] here.
//!
//! Its `pub(crate)` API: the types [`CommandFailure`], [`CommandTable`],
//! [`CustomCommand`], [`OutputMode`] and [`PendingCommand`]; the intent enum
//! [`CustomCommandsIntent`] and the free function [`update`]; the handlers
//! `start_command`, `command_answered`, `command_finished` and
//! `cancel_command` on `AppState`; the chunk `keys::CUSTOM_COMMANDS_WORKSPACE`;
//! and `effects::{run, cancel}`.
//!
//! Kept cross-feature reads and calls, left as they are because this slice
//! only moves code:
//! - `command_target` reads `selected` and `files` (files), the Diff
//!   pane's hunk cursor through core `hunk_view()` (diff) and the
//!   `BlameView` in `Focus::Blame` (blame);
//! - `command_finished` calls core `refresh_status` (`model.rs`).
//!
//! What stays in core: the command engine in `keymap.rs` (custom-command
//! resolution, `available_commands`, the clash checks and `REPO_LOCKED`);
//! loading `[[commands]]` from the global file only, in `config.rs`;
//! `HandOff::RunCommand` in `handoff.rs`; the `AppState` fields
//! `pending_command` and `command_running`; and the command branches in
//! shared core code (`Resolution::Command`, `prompt_title`, `switch_repo`,
//! the help height and the running-command bar hint). Closing the command's
//! prompt is its own `custom_commands_popup_closed`.

mod captured;
mod command;
pub(crate) mod effects;
mod intent;
pub(crate) mod keys;
mod pending;
mod update;

pub(crate) use intent::CustomCommandsIntent;
pub(crate) use update::update;

pub(crate) use command::{CommandFailure, CommandTable, CustomCommand, OutputMode};
pub(crate) use pending::PendingCommand;

#[cfg(all(test, unix))]
pub(crate) use captured::run_captured;
#[cfg(test)]
pub(crate) use command::{Panel, Part, Target, test_table};
