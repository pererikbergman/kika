//! The Branches feature's key bindings: moving through the Local tab,
//! checking out the selected branch, and switching tabs. `CHUNKS` in
//! `keymap.rs` places both chunks, so their position there is behaviour.
//! Creating, renaming, deleting and the upstream keys stay in core for now.

use crate::intent::AppIntent;
use crate::keymap::{Binding, KeyPattern, KeyScope, NONE, Reason, branch_action_disabled, never};
use crate::model::AppState;
use crossterm::event::KeyCode;

/// Why no branch can be created from the selected one now: creating it
/// checks it out, which moves `HEAD`.
pub(crate) fn new_branch_disabled(state: &AppState) -> Option<Reason> {
    branch_action_disabled(state).or_else(|| crate::bisect::head_move_disabled(state))
}

pub(crate) fn checkout_branch_disabled(state: &AppState) -> Option<Reason> {
    branch_action_disabled(state).or_else(|| {
        let selected = state.branches.selected_branch();
        selected
            .is_some_and(|b| b.head)
            .then_some("Already on this branch".into())
            .or_else(|| crate::bisect::head_move_disabled(state))
    })
}

// Chunk 45 (next, prev, checkout): branches
pub(crate) static BRANCHES_KEYS: &[Binding] = &[
    Binding {
        id: "branches.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::Branches,
        intent: AppIntent::BranchNext,
        description: "Select the next branch",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "branches.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::Branches,
        intent: AppIntent::BranchPrev,
        description: "Select the previous branch",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "branches.checkout",
        keys: &[KeyPattern::exact(KeyCode::Char(' '), NONE)],
        scope: KeyScope::Branches,
        intent: AppIntent::CheckoutBranch,
        description: "Check out the selected branch",
        label: "checkout",
        bar: true,
        disabled: checkout_branch_disabled,
    },
];

// Chunk 48 (tabs): branches
pub(crate) static BRANCHES_TABS: &[Binding] = &[
    Binding {
        id: "branches.switch_tab",
        keys: &[KeyPattern::exact(KeyCode::Char(']'), NONE)],
        scope: KeyScope::Branches,
        intent: AppIntent::SwitchBranchesTab,
        description: "Show the remotes and their branches",
        label: "remotes",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "branches.prev_tab",
        keys: &[KeyPattern::exact(KeyCode::Char('['), NONE)],
        scope: KeyScope::Branches,
        intent: AppIntent::PrevBranchesTab,
        description: "Show the tags",
        label: "tags",
        bar: false,
        disabled: never,
    },
];
