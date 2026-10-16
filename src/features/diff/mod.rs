//! The Diff pane's features. This module owns the streamed preview
//! (`stream`: one `git diff` at a time, read only as far as the pane asks),
//! taking its chunks (`diff_loaded`), chunk 5 (`keys::DIFF_WORKSPACE`, the
//! context and whitespace keys with their guards) and the effects
//! `LoadDiff`, `DiffDemand`, `CancelDiff` and `DiffLoadingDelay`.
//!
//! It also owns hunk and line staging: chunks 29, 32 and 34
//! (`keys::DIFF_FILES`, `keys::DIFF_KEYS`, `keys::DIFF_LINES`) with their
//! guards `hunk_disabled`, `side_disabled` and `files_hunks_disabled`; the
//! staging handlers in `update.rs`; and `effects::load_patch`, the I/O
//! behind `LoadPatch`. The ops it starts (`ApplyCached`, `Stage`,
//! `Unstage`) stay core `GitOp`s, built and run by `op.rs`.
//!
//! It also owns the editor hand-off: `open_in_editor`, `edit_hunk`,
//! `editor_closed` and `hunk_edited` in `update.rs`, which return
//! `Effect::HandOff` or start the core `GitOp::ApplyEdited`; the guard
//! `edit_hunk_disabled` (`keys.rs`, chunk 32); and `edited_hunk_error`
//! with `EDITED_HUNK_INVALID`, the popup for an edit git rejects. And it
//! draws the streamed pane and the focused pane's title (`view.rs`).
//!
//! Its intents are nested (ADR-0002): core wraps [`DiffIntent`] in
//! `AppIntent::Diff`, and its one `update` arm calls [`update`] here,
//! which hands `DiffIntent::Renderers` on to the child's `update`.
//! `Discard`, `OpenInEditor` and `ScrollDiff` are shared core intents:
//! other features bind them, and core's arms handle them.
//!
//! Its `pub(crate)` API: the intent enum [`DiffIntent`] and the free
//! function [`update`]; `DiffStreamer`; the handlers `diff_loaded`,
//! `patch_loaded`, `with_lines`, `next_hunk`, `prev_hunk`,
//! `enter_line_mode`, `toggle_selection_stage`, `toggle_diff_side`,
//! `open_in_editor`, `edit_hunk`, `editor_closed`, `hunk_edited` and
//! `edited_hunk_error`, and the const `EDITED_HUNK_INVALID`
//! (`update.rs`); the chunks `keys::DIFF_WORKSPACE`, `keys::DIFF_FILES`,
//! `keys::DIFF_KEYS` and `keys::DIFF_LINES`; `effects::loading_delay`
//! and `effects::load_patch`; and `view::options_note`,
//! `view::render_diff_pane`, `view::hunk_title` and
//! `view::hunk_title_parts`.
//!
//! Kept cross-feature reads: `hunk_disabled` reads the Files panel's
//! selected row (`files.selected_row()`), to refuse a conflicted or renamed
//! file, and `open_in_editor` reads it to find the file to open (the Files
//! panel's `files.open` key dispatches the core `OpenInEditor` intent).
//! The rest reads core state (`generation`, `diff`, `focus`,
//! `diff_options`, the op slot, `keymap`).
//!
//! What stays in core, and why:
//! - `set_diff_options` reads `commits.open`, `stash.open`, the Files
//!   panel's selected row (`files.selected_row()`) and `diff_options`, and
//!   calls stash's `stash_file_diff` and core `select`/`commit_file_diff`.
//!   A feature does not call a sibling's behaviour, so core keeps it,
//!   `pub(crate)` so [`update`] can call it;
//! - the pane's one way in and out, `open_diff` and `close_diff` (with
//!   `DiffPane::open`/`clear` and `DiffStart`, in `diff_pane.rs`),
//!   because `commits`, `stash` and `review` call them; and `remap`,
//!   because `custom_patch` calls it;
//! - `focus_diff`, because it calls `focus_commit_diff` (custom_patch),
//!   `files_open` (stash) and `open_conflict` (conflicts);
//! - `toggle_hunk_stage` and `toggle_lines_stage`, because a commit's
//!   change goes to custom_patch's `patch_toggle_lines`. They call the
//!   moved `toggle_selection_stage` for the rest, and are `pub(crate)` so
//!   [`update`] can call them;
//! - `load_patch` and the `display_only` method, which custom_patch and the
//!   renderers call;
//! - in `keymap.rs`: `DISPLAY_ONLY`, `WHITESPACE_HIDDEN`, `NOT_IN_COMMIT`
//!   and the guards `display_only`, `whitespace_hidden`, `in_commit`,
//!   `no_file_selected`, `open_disabled` and `discard_disabled`, because
//!   core guards, custom_patch or the renderers use them too; and
//!   `blame_open`/`BLAME_OPEN`;
//! - discard (`ask_discard`, `discard_confirm`) and the `ApplyWorktree`
//!   tests in `op.rs`, which exercise the discard op;
//! - `DIFF_LOADING_DELAY` in `effect.rs`, which this feature imports;
//! - `failure_title` in `op_slot.rs`, which names every failed op and which
//!   `edited_hunk_error` calls;
//! - the pane's drawing in `diff_pane_view.rs`: `render_hunk_view` and
//!   `HunkPane`, which core calls with custom_patch's picks as an
//!   overlay; `diff_position`, because review draws it too;
//!   `styled_diff_line`, because blame does; and `diff_pane_body`,
//!   `found_note`, `marks_of`, `marked_spans`, `clipped_marked`, `search_match`
//!   and `current_match`, which review, blame and search use;
//! - the hunk cursor, `DiffPane::hunks`, read through core
//!   `hunk_view()`/`hunk_view_mut()` and set by `focus_hunk_view()`
//!   (`diff_pane.rs`), the only way into `Focus::Diff`;
//! - `handoff.rs`, the shared runner behind `Effect::HandOff`, which other
//!   features hand off through too, and `diff_pane.rs`/`hunk_view.rs`,
//!   which many features import.
//!
//! The child `renderers` draws the pane with an external pager or tool. Its
//! intents nest in `DiffIntent::Renderers`.

pub(crate) mod effects;
mod intent;
pub(crate) mod keys;
pub(crate) mod renderers;
pub(crate) mod stream;
mod update;
pub(crate) mod view;

pub(crate) use intent::DiffIntent;
pub(crate) use update::update;

pub(crate) use stream::DiffStreamer;
