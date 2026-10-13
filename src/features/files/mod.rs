//! Files: the Files panel's list navigation, its renderer, the effect
//! that loads the working tree's status, the `.gitignore` flow (the `i`
//! key, its prompt, the pure `gitignore` lines and the write) and
//! go-to-file. Chunks 10 (`files.next`, `files.prev`), 31
//! (`files.ignore`) and 38 (`files.menu`) are its key bindings, and it
//! runs `Effect::LoadStatus` and `Effect::AppendIgnore`, whose variants
//! stay in core.
//!
//! Its `pub(crate)` API:
//! - the handlers (`update.rs`) that core dispatch calls: `move_selection`
//!   for `SelectNext` and `SelectPrev`, `go_to_file`, and `open_ignore`,
//!   `ignore_answered` and `ignore_written` for the `.gitignore` flow;
//! - `IgnoreDraft` (the `ignore_draft` field) and `IgnoreAdded` (the
//!   `IgnoreWritten` intent);
//! - `keys::{FILES_NAV, FILES_IGNORE, FILES_MENU}`;
//! - `view::render_files_panel`, `effects::load` and `effects::ignore`.
//!
//! Kept cross-feature reads: none. The handlers and the renderer read the
//! core files fields (`files`, `selected`, `files_filter`, `files_anchor`,
//! `files_loaded`, `status_error`), `context` and `ignore_draft`, and call
//! the core queries `files_visible` and `files_range`, and core `select`
//! and `refresh_status`. The `.gitignore` write goes only through core
//! `worktree_file::update`.
//!
//! What stays in core, and why (a feature never calls a sibling's
//! behaviour, so anything another feature calls, or that calls one, stays):
//! - `select` and `refresh_status`: other features call them;
//! - `status_loaded`: it calls `announce_bisect` (commits/bisect) and
//!   `reload_rebase_todo` (commits/rewrite);
//! - `reselect`: it calls `files_open` (stash) and `review_files_shown`
//!   (review);
//! - `files_visible` and `files_selected_hidden`: core (`model.rs`,
//!   `keymap.rs`) and a stash test call them;
//! - `status_unchanged`, `files_range`, `files_range_paths`,
//!   `RANGE_FILES_CHANGED` and `stream_diff`, the files fields of
//!   `AppState`, `refresh::Selection`, `src/git/status.rs`,
//!   the `Effect::LoadStatus` variant, the `StatusLoaded`/`SelectNext`/
//!   `SelectPrev` intents and their `update()` arms;
//! - the `ignore_draft` field, the `OpenIgnore`, `IgnorePatternAnswered`,
//!   `IgnoreWritten` and `GoToFile` intents, and the prompt kinds and menu
//!   item that open them;
//! - the stage and discard keys and handlers, which a later slice moves.

pub(crate) mod effects;
mod gitignore;
pub(crate) mod keys;
mod update;
pub(crate) mod view;

pub(crate) use gitignore::{IgnoreAdded, IgnoreDraft};
