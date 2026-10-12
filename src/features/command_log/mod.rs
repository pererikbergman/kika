//! The Command log: the session's record of every git command Kika ran, and
//! the popup that shows it. The feature owns the log's state (`log.rs`), its
//! `update` handlers (`update.rs`), its key bindings (`keys.rs`) and its
//! render function (`view.rs`).
//!
//! Its `pub(crate)` API: the types [`CommandLog`] and [`CommandLogView`]
//! (its `LogEntry` rows stay inside the feature); the handlers
//! `command_logged`, `open_command_log`, `move_command_log` and
//! `command_log_page` on `AppState`; the chunks
//! `keys::{COMMAND_LOG_WORKSPACE, COMMAND_LOG_KEYS}`; and `view::render`.
//!
//! Kept cross-feature reads: none. It reads only core state (`popups`,
//! `size`, `command_log`).
//!
//! The log's branches in the shared filter, escape and focus code
//! (`key_focus`, `open_filter`, the filter apply match and `list_escape` in
//! `model.rs`) stay in core, as does `CommandLogged` reaching `update` from
//! the runner listener in `app.rs`.

pub(crate) mod keys;
mod log;
mod update;
pub(crate) mod view;

pub(crate) use log::{CommandLog, CommandLogView};
