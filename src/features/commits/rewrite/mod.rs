//! History rewrites from the Commits panel. This part owns the stopped
//! rebase's todo list (read, edited in place, written back) and the
//! rewrites themselves: squash, fixup, drop and reword, on one row or over
//! a range (`range.rs`), creating a `fixup!` commit, autosquash and
//! finding the fixup base (`fixup.rs`, `fixup_base.rs`), and move, edit,
//! break and pick. It also owns the check before a detached `HEAD`
//! moves: checkout and reset first list the commits nothing else reaches
//! (`left_behind.rs`), then ask before leaving them to the reflog
//! (`leave_commits.rs`). With them go all of chunk 12, the
//! commits-left-behind, autosquash and drop-range questions (chunks 59,
//! 62 and 64), the guards and plans those keys and handlers share,
//! `Effect::LoadRebaseTodo`/`WriteRebaseTodo`/`FindFixupBase`/
//! `CheckLeftBehind` and the `GitOp::InteractiveRebase` argv.
//!
//! Its `pub(crate)` API:
//! - the todo types `RebaseTodo`, `Rewrite` and `TodoFile` (`todo.rs`),
//!   and `FixupBase`, which `intent.rs` carries;
//! - `LeftBehindCheck`, the check in flight that `AppState` holds;
//! - the shared guards and plans here: `rewrite_disabled`, `log_plan`,
//!   `todo_edit`, `autosquash_plan`, `autosquash_disabled`,
//!   `create_fixup_disabled`, `find_base_disabled`,
//!   `range_rewrite_disabled`, `range_log_plan`, and the reasons
//!   `NEEDS_BREAK`, `TODO_SAVING`, `FILTER_HIDES_NEIGHBOUR` and
//!   `STAGE_THE_FIX`;
//! - `keys::{REWRITE_COMMITS, REWRITE_TODO_COMMITS,
//!   REWRITE_LEAVE_COMMITS_CONFIRM, REWRITE_AUTOSQUASH_CONFIRM,
//!   REWRITE_DROP_RANGE_CONFIRM}`,
//!   `keys::stop_disabled` and the `keys::commits_*_disabled` row guards;
//! - the handlers in `update.rs` (`rewrite`, `rebase_todo_written`,
//!   `reload_rebase_todo`, `rebase_todo_loaded`), `range.rs`
//!   (`rewrite_range`, `confirm_drop_range`), `fixup.rs`
//!   (`create_fixup`, `ask_autosquash`, `autosquash`, `find_fixup_base`,
//!   `fixup_base_found`) and `leave_commits.rs` (`move_head`,
//!   `left_behind_checked`, `confirm_leave_commits`);
//! - `effects::{load_todo, write_todo, find_base, check_left_behind}`,
//!   `git::rebase_command` and `fixup_base::candidates_notice`.
//!
//! Kept cross-feature reads: `commits.todo`, `todo_selected`, `todo_seq`,
//! `todo_write_seq`, `rows`, `selected`, `query`, `filter`, `tips`,
//! `range_anchor`, `range()`, `fixup_base_seq`, `pending` and
//! `short_oid` (the parent `commits` panel); `files` (the Files
//! panel, core); the op slot, `branch` (and its `head`), `popups`,
//! `left_behind_check`, `generation` and `notice` (core).
//!
//! What stays elsewhere, and why:
//! - `CommitsPanel.todo`/`set_todo`, `fixup_base_seq`, `range_anchor` and
//!   the todo rows in the parent's `view.rs`: the commits panel shows and
//!   navigates them;
//! - in core: `git/rebase_plan.rs`, `git/comment_prefix.rs` and
//!   `sequence_editor.rs`: plumbing other code reads too;
//! - the `AppIntent` and `Effect` variants, `ConfirmKind::{Autosquash,
//!   DropRange}` with its question, `LeaveCommitsConfirm` (the leave
//!   question's builder, shown as a `Popup::Question`), the popup key
//!   scopes (`KeyScope::LeaveCommits` among them) and
//!   `GitOp::InteractiveRebase`, which the custom patch's rewrite also
//!   builds through `op_command`;
//! - `confirm_refused` (`model/confirm_guard.rs`), the guard every move
//!   question re-checks, and `head_still` and `detached_head`
//!   (`confirm_question.rs`, core), which the reset-hard question also
//!   uses; the reset-hard and detached-checkout questions themselves are
//!   built by the parent's `reset_hard_question` and `checkout_question`;
//! - `commits_range_disabled`, `CLEAR_RANGE_FIRST` and `CLEAR_LOG_FILTER`,
//!   which other features' guards use too.

pub(crate) mod effects;
mod fixup;
pub(crate) mod fixup_base;
pub(crate) mod git;
pub(crate) mod keys;
mod leave_commits;
mod left_behind;
mod range;
mod todo;
mod update;

pub(crate) use fixup_base::FixupBase;
pub(crate) use leave_commits::LeftBehindCheck;
pub(crate) use todo::{RebaseTodo, Rewrite, TodoFile};

use super::keys::commit_action_disabled;
use super::{Crossing, with_stacked_branches};
use crate::git::rebase_plan::{self, Direction, Edit, RebasePlan, TodoEntry};
use crate::git::version;
use crate::keymap::{
    CLEAR_LOG_FILTER, OP_RUNNING, Reason, commits_range_disabled, in_progress_disabled, list_guard,
};
use crate::model::AppState;

/// Why the selected commit or todo row cannot be rewritten now. On a log
/// row: one operation at a time, none in progress, and a history the
/// planner can rewrite. On a todo row: one write at a time, and an edit
/// the list allows.
pub(crate) fn rewrite_disabled(state: &AppState, rewrite: Rewrite) -> Option<Reason> {
    // Each acts on one row: not over a range, nor on a row the filter hides.
    if let Some(reason) = list_guard(state, false) {
        return Some(reason);
    }
    if state.commits.todo_selected.is_some() {
        return todo_disabled(state, rewrite);
    }
    // A narrowed log skips commits: the row below need not be the parent,
    // nor the one a move swaps with, and a plan made from its rows would
    // leave commits out.
    if state.commits.query.is_active() {
        return Some(CLEAR_LOG_FILTER.into());
    }
    // The neighbour a move swaps with may be one the filter hides.
    if matches!(rewrite, Rewrite::Move(_)) && !state.commits.filter.is_empty() {
        return Some(FILTER_HIDES_NEIGHBOUR.into());
    }
    commit_action_disabled(state)
        .or_else(|| in_progress_disabled(state))
        .or_else(|| log_plan(state, rewrite).err())
}

/// Why a log commit does not move while the log is filtered: it swaps
/// with the commit next to it in the history, which may be hidden.
pub(crate) const FILTER_HIDES_NEIGHBOUR: &str =
    "Clear the filter to move commits: the one next to it may be hidden";

/// Why `break` cannot be used: the todo command is newer than Kika's
/// minimum git.
pub(crate) const NEEDS_BREAK: &str = "Needs Git 2.20 or newer";

/// Why a todo edit waits for the last one to be written.
pub(crate) const TODO_SAVING: &str = "Saving the todo list";

/// The plan for `rewrite` on the selected log row, as it will run. With
/// git 2.38 or newer branches stacked on the rewritten commits move along
/// (`--update-refs`), so only a move that would change what one holds is
/// refused; with an older git any stacked branch would be left behind, so
/// the rewrite is refused.
pub(crate) fn log_plan(state: &AppState, rewrite: Rewrite) -> Result<RebasePlan, Reason> {
    if rewrite == Rewrite::Break && !state.git_has(version::BREAK) {
        return Err(NEEDS_BREAK.into());
    }
    let commits = &state.commits;
    let (rows, selected) = (commits.rows.as_slice(), commits.selected);
    let plan = match rewrite {
        Rewrite::Edit(edit) => rebase_plan::plan(rows, selected, edit),
        Rewrite::Move(direction) => rebase_plan::plan_move(rows, selected, direction),
        Rewrite::Break => rebase_plan::plan_break(rows, selected),
    }?;
    // A branch on the older of the two would end up holding the other.
    let crossings: &[Crossing] = match rewrite {
        Rewrite::Move(direction) => &[Crossing {
            moved: selected,
            from: match direction {
                Direction::Up => selected,
                Direction::Down => selected + 1,
            },
            len: 1,
        }],
        _ => &[],
    };
    with_stacked_branches(state, plan, crossings)
}

/// The plan that folds every `fixup!` commit in the log into its target,
/// as it will run. Each fixup moves down past the commits between it and
/// its target, so with `--update-refs` a branch on the fixup or on one of
/// those is refused.
pub(crate) fn autosquash_plan(state: &AppState) -> Result<RebasePlan, Reason> {
    let rows = state.commits.rows.as_slice();
    let pairs = rebase_plan::fixup_targets(rows)?;
    let plan = rebase_plan::plan_autosquash(rows)?;
    let crossings: Vec<Crossing> = pairs
        .into_iter()
        .map(|(fixup, target)| Crossing {
            moved: fixup,
            from: fixup,
            len: target - fixup,
        })
        .collect();
    with_stacked_branches(state, plan, &crossings)
}

/// Why the fixups cannot be folded now. Like any rewrite of the log: one
/// operation at a time, none in progress, and the whole log in view, as
/// the plan is made from its rows.
pub(crate) fn autosquash_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false)
        .or_else(|| commits_range_disabled(state))
        .or_else(|| {
            state
                .commits
                .query
                .is_active()
                .then_some(CLEAR_LOG_FILTER.into())
        })
        .or_else(|| (!state.commits.filter.is_empty()).then_some(FILTER_HIDES_NEIGHBOUR.into()))
        .or_else(|| commit_action_disabled(state))
        .or_else(|| in_progress_disabled(state))
        .or_else(|| autosquash_plan(state).err())
}

/// Why the staged change cannot become a `fixup!` of the selected commit
/// now. The target is named by its id, so a narrowed log is fine.
pub(crate) fn create_fixup_disabled(state: &AppState) -> Option<Reason> {
    fixup_target_disabled(state)
        .or_else(|| in_progress_disabled(state))
        .or_else(|| nothing_staged(state))
}

/// Why the commit the staged lines come from cannot be looked for now.
/// It only reads, so an operation in progress does not stop it.
pub(crate) fn find_base_disabled(state: &AppState) -> Option<Reason> {
    fixup_target_disabled(state).or_else(|| nothing_staged(state))
}

/// One log commit selected, shown, and free to act on.
fn fixup_target_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false)
        .or_else(|| commits_range_disabled(state))
        .or_else(|| commit_action_disabled(state))
}

fn nothing_staged(state: &AppState) -> Option<Reason> {
    let staged = state.files.iter().any(|f| !matches!(f.index, '.' | '?'));
    (!staged).then_some(STAGE_THE_FIX.into())
}

/// Why a fixup cannot be made or looked for: nothing is staged.
pub(crate) const STAGE_THE_FIX: &str = "Stage the fix first";

/// Why `edit` cannot run over the selected log range now: the range is
/// in the log, the log is not narrowed (the plan is made from its rows),
/// one operation at a time with none in progress, and a plan for the
/// whole range.
pub(crate) fn range_rewrite_disabled(state: &AppState, edit: Edit) -> Option<Reason> {
    commits_range_disabled(state)
        .or_else(|| {
            state
                .commits
                .query
                .is_active()
                .then_some(CLEAR_LOG_FILTER.into())
        })
        .or_else(|| commit_action_disabled(state))
        .or_else(|| in_progress_disabled(state))
        .or_else(|| range_log_plan(state, edit).err())
}

/// The plan for `edit` over the selected log range, as it will run,
/// checked against stacked branches like a single-row rewrite.
pub(crate) fn range_log_plan(state: &AppState, edit: Edit) -> Result<RebasePlan, Reason> {
    let rows = state.commits.rows.as_slice();
    let plan = rebase_plan::plan_range(rows, &state.commits.range(), edit)?;
    with_stacked_branches(state, plan, &[])
}

/// The todo list after `rewrite` on the selected todo row, and where the
/// cursor goes, or why it cannot be made.
pub(crate) fn todo_edit(
    state: &AppState,
    rewrite: Rewrite,
) -> Result<(Vec<TodoEntry>, usize), Reason> {
    let commits = &state.commits;
    let (Some(todo), Some(i)) = (&commits.todo, commits.todo_selected) else {
        return Err("No todo row selected".into());
    };
    todo.edit(i, rewrite).map_err(Reason::from)
}

fn todo_disabled(state: &AppState, rewrite: Rewrite) -> Option<Reason> {
    if state.op_is_running() {
        return Some(OP_RUNNING.into());
    }
    // The list shown is what was written only once it is read back.
    if state.commits.todo_write_seq.is_some() || state.commits.todo_seq.is_some() {
        return Some(TODO_SAVING.into());
    }
    if rewrite == Rewrite::Break && !state.git_has(version::BREAK) {
        return Some(NEEDS_BREAK.into());
    }
    todo_edit(state, rewrite).err()
}
