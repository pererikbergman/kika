//! Operation: the Status panel (branch, upstream, operations in progress);
//! merging the selected branch or rebasing onto it; and continuing,
//! skipping or aborting the merge, rebase, cherry-pick or revert in
//! progress, with the abort question. Skip first checks, as an effect,
//! for work done since the stop
//! (`git::stop_work::read_stop_work`), and asks the skip question when
//! it finds some or the check fails; otherwise it skips right away. Its
//! git ops run through the core op slot.
//!
//! Its intents are nested (ADR-0002): core wraps [`OperationIntent`] in
//! `AppIntent::Operation`, and its one `update` arm calls [`update`] here.
//!
//! Its `pub(crate)` API:
//! - the intent enum [`OperationIntent`] and the free function [`update`];
//! - [`StopWorkCheck`], the check in flight that `AppState` holds as
//!   `skip_check`;
//! - the handlers in `update.rs`: `branch_op`, `resume`, `skip` and
//!   `ask_abort`, which asks `abort_question`; the skip question is
//!   `skip_question`, or `skip_question_unchecked` when the check failed;
//! - `effects::check_stop_work`, behind `Effect::CheckStopWork`;
//! - `keys::{OPERATION_BRANCHES, OPERATION_STATUS, OPERATION_ABORT_CONFIRM,
//!   OPERATION_SKIP_CONFIRM, merge_disabled}`;
//! - `view::render_status_panel`.
//!
//! Kept cross-feature reads and calls:
//! - `merge_disabled` reads `branches.selected_branch()`;
//! - `branch_op` calls core `branch_for_action` (`model.rs`);
//! - `render_status_panel` reads `branch`, `files.error`, `files.loaded`
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
//! - the shared intents `ContinueOperation`, `SkipOperation` and
//!   `AbortOperation` and their `update()` arms, including the
//!   `bisect::bisect_only` guards: bisect shares them, and only core may
//!   send them on to it;
//! - the core op slot: `run_op_where_it_belongs`, `start_op`,
//!   `op_finished`, `failure_title`/`failure_next` and `confirm_refused`;
//! - in `view.rs`: `bisect_lines`, `FOUND_MARK`, `short_hash`,
//!   `SHORT_HASH`, `fit`, the elide helpers, `width_of`, `PanelLook` and
//!   `panel_block`;
//! - the merge menu (`MenuId::Merge` in `menu.rs`) and the
//!   `op_command`/`run_op` arms in `op.rs`.

pub(crate) mod effects;
mod intent;
pub(crate) mod keys;
mod state;
mod update;
pub(crate) mod view;

pub(crate) use intent::OperationIntent;
pub(crate) use state::StopWorkCheck;
pub(crate) use update::update;

#[cfg(test)]
pub(crate) use update::{abort_question, skip_question, skip_question_unchecked};
