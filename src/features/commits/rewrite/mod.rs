//! History rewrites from the Commits panel. This part owns the stopped
//! rebase's todo list (read, edited in place, written back) and the
//! move, edit, break and pick rewrites: the second half of chunk 12
//! (`commits.move_up`, `commits.move_down`, `commits.edit`,
//! `commits.break`, `commits.pick`), the guards every rewrite key shares,
//! `Effect::LoadRebaseTodo`/`WriteRebaseTodo` and the
//! `GitOp::InteractiveRebase` argv. #304 (squash, fixup, drop, reword,
//! autosquash, fixup base) and #305 (commits left behind) add to this
//! folder.
//!
//! Its `pub(crate)` API:
//! - the todo types `RebaseTodo`, `Rewrite` and `TodoFile` (`todo.rs`);
//! - the shared guards here: `rewrite_disabled`, `log_plan`, `todo_edit`,
//!   `NEEDS_BREAK`, `TODO_SAVING` and `FILTER_HIDES_NEIGHBOUR`, which core
//!   `keymap.rs` still uses for the squash, fixup, drop, reword and
//!   autosquash guards;
//! - `keys::REWRITE_TODO_COMMITS` and `keys::stop_disabled`;
//! - the handlers in `update.rs`: `rewrite`, `rebase_todo_written`,
//!   `reload_rebase_todo` and `rebase_todo_loaded`;
//! - `effects::{load_todo, write_todo}` and `git::rebase_command`.
//!
//! Kept cross-feature reads: `commits.todo`, `todo_selected`, `todo_seq`,
//! `todo_write_seq`, `rows`, `selected`, `query`, `filter` and `tips`
//! (the commits panel, still in core); `op_seq`, `branch` and
//! `generation` (core).
//!
//! What stays in core, and why:
//! - `CommitsPanel.todo`/`set_todo` and the todo rows in
//!   `commits_view.rs`: the commits panel shows and navigates them;
//! - `git/rebase_plan.rs`, `git/comment_prefix.rs` and
//!   `sequence_editor.rs`: plumbing other code reads too;
//! - the `AppIntent` and `Effect` variants and `GitOp::InteractiveRebase`,
//!   which the custom patch's rewrite also builds through `op_command`;
//! - the first half of chunk 12 and its guards, until #304 moves them.

pub(crate) mod effects;
pub(crate) mod git;
pub(crate) mod keys;
mod todo;
mod update;

pub(crate) use todo::{RebaseTodo, Rewrite, TodoFile};

use super::{Crossing, with_stacked_branches};
use crate::git::rebase_plan::{self, Direction, RebasePlan, TodoEntry};
use crate::git::version;
use crate::keymap::{
    CLEAR_LOG_FILTER, OP_RUNNING, Reason, commit_action_disabled, in_progress_disabled, list_guard,
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
    if state.op_seq.is_some() {
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
