//! Operation: the Status panel (branch, upstream, operations in progress);
//! merging the selected branch or rebasing onto it; and continuing,
//! skipping or aborting the merge, rebase, cherry-pick or revert in
//! progress, with the abort question. The feature has no state and no
//! `Effect` of its own: its git ops run through the core op slot.
//!
//! Its `pub(crate)` API:
//! - the handlers in `update.rs`: `branch_op`, `resume` and `ask_abort`;
//! - `keys::{OPERATION_BRANCHES, OPERATION_STATUS, OPERATION_ABORT_CONFIRM,
//!   merge_disabled}`;
//! - `view::render_status_panel`.
//!
//! Kept cross-feature reads and calls:
//! - `merge_disabled` reads `branches.selected_branch()`;
//! - `branch_op` calls core `branch_for_action` (`model.rs`);
//! - `render_status_panel` reads `branch`, `status_error`, `files_loaded`
//!   and `repo_stack`, and calls core `bisect_lines`;
//! - chunk 52 uses the bisect-aware guards in `src/bisect.rs`
//!   (`continue_disabled`, `skip_disabled`, `abort_disabled`), and
//!   `merge_disabled` uses `bisect::head_move_disabled`.
//!
//! What stays in core:
//! - the shared guards `resumable_operation`, `resume_disabled`,
//!   `skip_disabled`, `in_progress_disabled` and `branch_action_disabled`
//!   (`keymap.rs`), which bisect and other features call too, and
//!   `OP_RUNNING`, `never` and `REPO_LOCKED`;
//! - the bisect-aware status guards in `src/bisect.rs`;
//! - the `update()` dispatch arms, including the `bisect::bisect_only`
//!   guards and the `ConfirmAbort` arm;
//! - the core op slot: `run_op_where_it_belongs`, `start_op`,
//!   `op_finished`, `failure_title`/`failure_next`, `confirm_refused`,
//!   `confirm_question` and `ConfirmKind`;
//! - in `view.rs`: `bisect_lines`, `FOUND_MARK`, `short_hash`,
//!   `SHORT_HASH`, `fit`, the elide helpers, `width_of`, `PanelLook` and
//!   `panel_block`;
//! - the merge menu (`MenuId::Merge` in `menu.rs`) and the
//!   `op_command`/`run_op` arms in `op.rs`.

pub(crate) mod keys;
mod update;
pub(crate) mod view;
