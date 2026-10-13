//! Conflicts: the conflict view, which opens a conflicted file in the Diff
//! pane's place. The feature owns parsing the file into blocks and the
//! view's state (`state.rs`), picking a side of each block and writing the
//! file after each pick, staging it at the end and the merge-tool hand-off
//! (`update.rs`), the `conflict-marker-size` read (`git.rs`), its key
//! bindings and guards (`keys.rs`), its render function (`view.rs`) and the
//! I/O behind `LoadConflict` and `WriteResolved` (`effects.rs`).
//!
//! Its `pub(crate)` API:
//! - the types [`Choice`] and [`ConflictView`];
//! - the handlers in `update.rs`: `open_conflict`, `conflict_loaded`,
//!   `with_conflict`, `pick_conflict`, `resolved_written`, `stage_resolved`
//!   and `open_merge_tool`;
//! - the chunks `keys::{CONFLICTS_FILES, CONFLICTS_KEYS}`;
//! - `view::render`;
//! - `effects::{load, write}`.
//!
//! Kept cross-feature reads, of files through core for now:
//! - `open_conflict`, `open_merge_tool` and `mergetool_disabled` read
//!   `selected` and `files`;
//! - `resolved_written` calls `refresh_status` and sets `status_selection`;
//! - writes and staging go through the core op slot (`take_op`,
//!   `release_op`, `start_op`, `run_op_where_it_belongs`).
//!
//! What stays in core: the view's branches in shared code (`ScrollDiff`,
//! the `Panel::Diff` mapping, `key_focus`, the `Focus::Conflict(_) => None`
//! matches, the Files open path, the stage-finished branch in `op_finished`
//! and the "Close the conflict view first" diff-settings guard), and
//! `worktree_file::{read, replace}`, which hold the fail-closed checks: the
//! size cap on reads and the expected-bytes check before a write.

pub(crate) mod effects;
mod git;
pub(crate) mod keys;
mod state;
mod update;
pub(crate) mod view;

pub(crate) use state::{Choice, ConflictView};
