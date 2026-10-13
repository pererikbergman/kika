//! Files: the Files panel's list navigation, its renderer, the effect
//! that loads the working tree's status, and staging from the panel: Space
//! stages or unstages the selected file or the range, `a` stages or
//! unstages everything, and `d` on a range asks before discarding it.
//! Chunks 10 (`files.next`, `files.prev`), 20 (`files.stage`,
//! `files.stage_all`), 22 (`files.discard`, `files.open`), 38
//! (`files.menu`) and 44 (`discard_confirm.*`) are its key bindings, and it
//! runs `Effect::LoadStatus`, whose variant stays in core.
//!
//! Its `pub(crate)` API:
//! - `move_selection`, `toggle_stage`, `toggle_stage_all` and
//!   `ask_discard_range` (`update.rs`), which core dispatch and core
//!   `ask_discard` call;
//! - `keys::{FILES_NAV, FILES_STAGE, FILES_DISCARD_OPEN, FILES_MENU,
//!   FILES_DISCARD_CONFIRM}` and the guards `stage_all_disabled` and
//!   `files_stage_disabled`, which the actions menu binds too;
//! - `git::{toggle_for, toggle_all, toggle_many, CONFLICTS_REFUSAL}`, the
//!   ops the stage keys run; their argv stays in core `op_command`;
//! - `view::render_files_panel` and `effects::load`.
//!
//! Kept cross-feature reads: none. The handlers and the renderer read the
//! core files fields (`files`, `selected`, `files_filter`, `files_anchor`,
//! `files_loaded`, `status_error`) and call the core queries
//! `files_visible` and `files_range`, and core `select` and `start_op`.
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
//! - discard itself: `ask_discard`, `discard_confirm`, the discard
//!   question's popup and answer, `GitOp::discard_for` and
//!   `GitOp::discard_many`, and the guards `discard_disabled` and
//!   `open_disabled`, because the Diff pane discards a hunk or lines
//!   through the same question;
//! - the ignore and go-to-file keys and handlers, which a later slice
//!   moves.

pub(crate) mod effects;
pub(crate) mod git;
pub(crate) mod keys;
mod update;
pub(crate) mod view;
