//! Files: the Files panel's list navigation, its renderer, the effect
//! that loads the working tree's status, staging from the panel (Space
//! stages or unstages the selected file or the range, `a` stages or
//! unstages everything, and `d` on a range asks before discarding it), the
//! `.gitignore` flow (the `i` key, its prompt, the pure `gitignore` lines
//! and the write) and go-to-file. Chunks 10 (`files.next`, `files.prev`),
//! 20 (`files.stage`, `files.stage_all`), 22 (`files.discard`,
//! `files.open`), 31 (`files.ignore`), 38 (`files.menu`) and 44
//! (`discard_confirm.*`) are its key bindings, and it runs
//! `Effect::LoadStatus` and `Effect::AppendIgnore`, whose variants stay in
//! core.
//!
//! Its intents are nested (ADR-0002): core wraps [`FilesIntent`] in
//! `AppIntent::Files`, and its one `update` arm calls [`update`] here.
//! `Discard` and `OpenInEditor`, which its keys also bind, are shared core
//! intents.
//!
//! Its `pub(crate)` API:
//! - the intent enum [`FilesIntent`] and the free function [`update`];
//! - the handlers (`update.rs`): `move_selection`, `toggle_stage` and
//!   `toggle_stage_all`, `ask_discard_range` (from core `ask_discard`),
//!   `go_to_file`, and `open_ignore`, `ignore_answered`, `ignore_written`
//!   and `files_popup_closed` for the `.gitignore` flow;
//! - `IgnoreDraft` (the `ignore_draft` field) and `IgnoreAdded` (the
//!   `FilesIntent::IgnoreWritten` intent);
//! - `keys::{FILES_NAV, FILES_STAGE, FILES_DISCARD_OPEN, FILES_IGNORE,
//!   FILES_MENU, FILES_DISCARD_CONFIRM}` and the guards
//!   `stage_all_disabled` and `files_stage_disabled`, which the actions
//!   menu binds too;
//! - `git::{toggle_for, toggle_all, toggle_many, CONFLICTS_REFUSAL}`, the
//!   ops the stage keys run; their argv stays in core `op_command`;
//! - `view::render_files_panel`, `effects::load` and `effects::ignore`, and
//!   the mouse's queries `files_list_shown` and `files_step_to` (`view.rs`).
//!
//! Its rows are a `FilteredList<FileEntry>` (`list.rs` makes `FileEntry`
//! a row), which owns their cursor, filter, range and load in flight.
//!
//! Kept cross-feature reads: none. The handlers and the renderer read the
//! core `files` list, `context` and `ignore_draft`, and call core
//! `select`, `refresh_status` and `start_op`. The `.gitignore` write goes only
//! through core `worktree_file::update`.
//!
//! What stays in core, and why (a feature never calls a sibling's
//! behaviour, so anything another feature calls, or that calls one, stays):
//! - `select` and `refresh_status`: other features call them;
//! - `status_loaded`: it calls `announce_bisect` (commits/bisect) and
//!   `reload_rebase_todo` (commits/rewrite);
//! - `reselect`: it calls `files_open` (stash) and `review_files_shown`
//!   (review);
//! - `status_unchanged`, `files_range_paths`, `RANGE_FILES_CHANGED` and
//!   `select_from` (the commit and stash closes call it), the `files`
//!   field of `AppState`, `refresh::Selection`,
//!   `kika_git::status`, the `Effect::LoadStatus` variant, and the
//!   `StatusLoaded` intent and its `update()` arm;
//! - the `ignore_draft` field, and the prompt kinds and menu item that
//!   open the `.gitignore` and go-to-file prompts;
//! - discard itself: `ask_discard`, `discard_confirm`, the discard
//!   question's popup and answer, `GitOp::discard_for` and
//!   `GitOp::discard_many`, and the guards `discard_disabled` and
//!   `open_disabled`, because the Diff pane discards a hunk or lines
//!   through the same question.

pub(crate) mod effects;
pub(crate) mod git;
mod gitignore;
mod intent;
pub(crate) mod keys;
mod list;
mod update;
pub(crate) mod view;

pub(crate) use intent::FilesIntent;
pub(crate) use update::update;

pub(crate) use gitignore::{IgnoreAdded, IgnoreDraft};
