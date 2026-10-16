//! The Command log: the session's record of every git command Kika ran, and
//! the popup that shows it. The feature owns the log's state (`log.rs`), its
//! intent enum (`intent.rs`), its `update` (`update.rs`), its key bindings
//! (`keys.rs`) and its render function (`view.rs`).
//!
//! It is the pilot for nested intents (ADR-0002): core wraps
//! [`CommandLogIntent`] in `AppIntent::CommandLog` and its one `update` arm
//! calls [`update`] here.
//!
//! Its `pub(crate)` API: the types [`CommandLog`], [`CommandLogView`] and
//! [`CommandLogIntent`] (its `LogEntry` rows stay inside the feature); the
//! free function [`update`]; the handlers `command_logged`,
//! `open_command_log`, `move_command_log` and `command_log_page` on
//! `AppState`; the chunks
//! `keys::{COMMAND_LOG_WORKSPACE, COMMAND_LOG_KEYS}`; and
//! `view::{render, log_box}`, the second the layout the mouse finds rows
//! in.
//!
//! Kept cross-feature reads: none. It reads only core state (`popups`,
//! `size`, `command_log`).
//!
//! The log's branches in the shared filter, escape and focus code
//! (`key_focus` in `model.rs`; `open_filter`, the filter apply match and
//! `list_escape` in `list_input.rs`) stay in core, as does the runner
//! listener in `app.rs` that sends `CommandLogIntent::Logged` for every git
//! command.

mod intent;
pub(crate) mod keys;
mod log;
mod update;
pub(crate) mod view;

pub(crate) use intent::CommandLogIntent;
pub(crate) use log::{CommandLog, CommandLogView};
pub(crate) use update::update;
