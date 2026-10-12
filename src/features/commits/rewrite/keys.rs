//! The move, edit, break and pick keys, on a log row or a todo row, and
//! the guards only they use. `CHUNKS` in `keymap.rs` places the chunk
//! right after the core `REWRITE_COMMITS`, so its position there is
//! behaviour.

use crossterm::event::{KeyCode, KeyModifiers};

use super::{Rewrite, rewrite_disabled};
use crate::git::rebase_plan::{Direction, Edit};
use crate::intent::AppIntent;
use crate::keymap::{Binding, KeyPattern, KeyScope, NONE, Reason};
use crate::model::AppState;

pub(crate) fn stop_disabled(state: &AppState) -> Option<Reason> {
    rewrite_disabled(state, Rewrite::Edit(Edit::Stop))
}

fn todo_pick_disabled(state: &AppState) -> Option<Reason> {
    rewrite_disabled(state, Rewrite::Edit(Edit::Pick))
}

fn break_disabled(state: &AppState) -> Option<Reason> {
    rewrite_disabled(state, Rewrite::Break)
}

fn move_up_disabled(state: &AppState) -> Option<Reason> {
    rewrite_disabled(state, Rewrite::Move(Direction::Up))
}

fn move_down_disabled(state: &AppState) -> Option<Reason> {
    rewrite_disabled(state, Rewrite::Move(Direction::Down))
}

// Chunk 12: rewrite, second half
pub(crate) static REWRITE_TODO_COMMITS: &[Binding] = &[
    // `J` and `K` scroll the diff from the Workspace.
    Binding {
        id: "commits.move_up",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), KeyModifiers::ALT),
            KeyPattern::exact(KeyCode::Up, KeyModifiers::ALT),
        ],
        scope: KeyScope::Commits,
        intent: AppIntent::MoveCommit(Direction::Up),
        description: "Move the selected commit or todo row up, above the newer one",
        label: "move up",
        bar: true,
        disabled: move_up_disabled,
    },
    Binding {
        id: "commits.move_down",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), KeyModifiers::ALT),
            KeyPattern::exact(KeyCode::Down, KeyModifiers::ALT),
        ],
        scope: KeyScope::Commits,
        intent: AppIntent::MoveCommit(Direction::Down),
        description: "Move the selected commit or todo row down, below the older one",
        label: "move down",
        bar: true,
        disabled: move_down_disabled,
    },
    Binding {
        id: "commits.edit",
        keys: &[KeyPattern::any(KeyCode::Char('E'))],
        scope: KeyScope::Commits,
        intent: AppIntent::RewriteCommit(Edit::Stop),
        description: "Stop the rebase at the selected commit, to amend it, then continue from Status",
        label: "edit",
        bar: true,
        disabled: stop_disabled,
    },
    Binding {
        id: "commits.break",
        keys: &[KeyPattern::exact(KeyCode::Char('b'), NONE)],
        scope: KeyScope::Commits,
        intent: AppIntent::InsertBreak,
        description: "Stop the rebase right after the selected commit, or toggle the stop after a todo row",
        label: "break",
        bar: true,
        disabled: break_disabled,
    },
    // `p` is pull; a pick is git's "use commit".
    Binding {
        id: "commits.pick",
        keys: &[KeyPattern::exact(KeyCode::Char('u'), NONE)],
        scope: KeyScope::Commits,
        intent: AppIntent::RewriteCommit(Edit::Pick),
        description: "Pick the selected todo row as it is, undoing its other action",
        label: "pick",
        bar: true,
        disabled: todo_pick_disabled,
    },
];
