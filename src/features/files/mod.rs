//! Files: the Files panel's list navigation, its renderer, and the effect
//! that loads the working tree's status. Chunks 10 (`files.next`,
//! `files.prev`) and 38 (`files.menu`) are its key bindings, and
//! `Effect::LoadStatus` is its effect.
//!
//! Its `pub(crate)` API:
//! - `move_selection` (`update.rs`), which core dispatch calls for
//!   `SelectNext` and `SelectPrev`;
//! - `keys::{FILES_NAV, FILES_MENU}`;
//! - `view::render_files_panel` and `effects::load`.
//!
//! Kept cross-feature reads: none. The handler and the renderer read the
//! core files fields (`files`, `selected`, `files_filter`, `files_anchor`,
//! `files_loaded`, `status_error`) and call the core queries
//! `files_visible` and `files_range`, and core `select`.
//!
//! What stays in core, and why (a feature never calls a sibling's
//! behaviour, so anything another feature calls, or that calls one, stays):
//! - `select` and `refresh_status`: other features call them;
//! - `status_loaded`: it calls `announce_bisect` (commits/bisect) and
//!   `reload_rebase_todo` (commits/rewrite);
//! - `reselect`: it calls `files_open` (stash) and `review_files_shown`
//!   (review);
//! - `files_visible` and `files_selected_hidden`: core, the stash feature
//!   and its tests call them;
//! - `status_unchanged`, `files_range`, `files_range_paths`,
//!   `RANGE_FILES_CHANGED` and `stream_diff`, the files fields of
//!   `AppState`, `refresh::Selection`, `src/git/status.rs`,
//!   `Effect::LoadStatus` itself, the `StatusLoaded`/`SelectNext`/
//!   `SelectPrev` intents and their `update()` arms;
//! - the stage, discard, ignore and go-to-file keys and handlers, which
//!   later slices move.

pub(crate) mod effects;
pub(crate) mod keys;
mod update;
pub(crate) mod view;
