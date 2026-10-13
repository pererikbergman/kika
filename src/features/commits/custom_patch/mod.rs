//! The custom patch: changes picked from one commit, whole files or
//! single changed lines, then taken out of it, moved to another commit or
//! the index, or split off as a commit of their own. It owns chunks 7
//! (`workspace.patch_menu`), 17 (`commit_files.patch`) and 63
//! (`patch_confirm.*`), the picks, the rewrite menu's choices and their
//! questions, and the rewrite that runs in the terminal.
//!
//! Its `pub(crate)` API:
//! - the state types `CustomPatch`, `FilePick`, `PatchAction` and
//!   `PatchChoice`, and `NO_PATCH`;
//! - `keys::` predicates and constants (`patch_file_disabled`,
//!   `patch_rewrite_disabled`, `patch_plan`, `PATCH_EMPTY`,
//!   `TARGET_IS_SOURCE`, ...) and the three chunks;
//! - `view::picked_rows`, which core `view.rs` passes to the Diff pane
//!   as its mark overlay;
//! - `git::{Rewrite, run, ...}`, which the hand-off runs;
//! - the handlers in `update.rs` that `update()` calls.
//!
//! Kept cross-feature reads: `commits.open`, `commits.rows`, `selected`,
//! `todo_selected`, `query` and `filter` (commits); `files`, the op slot and
//! `branch` (core).
//!
//! What stays in core, and why:
//! - `hunk_view`: the Files diff and review use it as well as a
//!   commit's diff;
//! - `commit_source_disabled`, `list_guard` and `in_progress_disabled`,
//!   which other keys use too;
//! - the patch menu (`menu.rs`), `ConfirmKind`/`PromptKind`, the
//!   `AppIntent` variants, `GitOp::CustomPatch` with its
//!   `op_command`/`refusal` arms, and the `handoff.rs` run arm, all
//!   shared dispatch.
//!
//! The pick marks are drawn by the parent's `view.rs`.

pub(crate) mod git;
pub(crate) mod keys;
mod state;
mod update;
pub(crate) mod view;

pub(crate) use state::{CustomPatch, FilePick, PatchAction, PatchChoice};
pub(crate) use update::NO_PATCH;
