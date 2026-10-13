//! The Branches feature's key bindings: moving through the Local tab,
//! checking out the selected branch, creating, renaming and deleting one,
//! setting or unsetting its upstream, answering the delete question, and
//! switching tabs. `CHUNKS` in `keymap.rs` places every chunk, so their
//! position there is behaviour.

use crate::intent::AppIntent;
use crate::keymap::{
    Binding, KeyPattern, KeyScope, NONE, Reason, branch_action_disabled, branch_selected_disabled,
    list_guard, never, op_running,
};
use crate::model::AppState;
use crate::popup::{DeleteBranchConfirm, Popup};
use crossterm::event::{KeyCode, KeyModifiers};

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

/// Over a range, `d` asks about the range, which skips the current
/// branch itself; only the one-at-a-time rule is left to check.
pub(crate) fn delete_branch_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, true).or_else(|| {
        if state.branches.range_anchor.is_some() {
            return op_running(state);
        }
        branch_selected_disabled(state).or_else(|| {
            let selected = state.branches.selected_branch();
            selected
                .is_some_and(|b| b.head)
                .then_some("Cannot delete the current branch".into())
        })
    })
}

/// Why the selected local branch's upstream cannot be removed: it needs
/// one.
pub(crate) fn unset_upstream_disabled(state: &AppState) -> Option<Reason> {
    branch_action_disabled(state).or_else(|| {
        let selected = state.branches.selected_branch();
        selected
            .is_some_and(|b| b.upstream.is_none())
            .then_some("No upstream to unset".into())
    })
}

/// The delete question on top, if one is.
fn delete_question(state: &AppState) -> Option<&DeleteBranchConfirm> {
    match state.popups.last() {
        Some(Popup::DeleteBranch(confirm)) => Some(confirm),
        _ => None,
    }
}

/// `y` deletes only a merged branch; an unmerged one needs `D`.
fn plain_delete_disabled(state: &AppState) -> Option<Reason> {
    delete_question(state)
        .is_some_and(DeleteBranchConfirm::force)
        .then_some("Not fully merged: press D to force delete".into())
}

/// `D` answers only the stronger question, about an unmerged branch.
fn force_delete_disabled(state: &AppState) -> Option<Reason> {
    delete_question(state)
        .is_some_and(|c| !c.force())
        .then_some("Fully merged: press y to delete".into())
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

// Chunk 45 (new, rename, delete): branches
pub(crate) static BRANCHES_CHANGE: &[Binding] = &[
    Binding {
        id: "branches.new",
        keys: &[KeyPattern::exact(KeyCode::Char('n'), NONE)],
        scope: KeyScope::Branches,
        intent: AppIntent::NewBranch,
        description: "Create a branch at the selected one and check it out",
        label: "new",
        bar: true,
        disabled: new_branch_disabled,
    },
    // Not `r`, which reloads everywhere in the workspace.
    Binding {
        id: "branches.rename",
        keys: &[KeyPattern::any(KeyCode::Char('R'))],
        scope: KeyScope::Branches,
        intent: AppIntent::RenameBranch,
        description: "Rename the selected branch",
        label: "rename",
        bar: true,
        disabled: branch_action_disabled,
    },
    Binding {
        id: "branches.delete",
        keys: &[KeyPattern::exact(KeyCode::Char('d'), NONE)],
        scope: KeyScope::Branches,
        intent: AppIntent::DeleteBranch,
        description: "Delete the selected branch, asking first",
        label: "delete",
        bar: true,
        disabled: delete_branch_disabled,
    },
];

// Chunk 48 (upstream): branches
pub(crate) static BRANCHES_UPSTREAM: &[Binding] = &[
    Binding {
        id: "branches.set_upstream",
        keys: &[KeyPattern::exact(KeyCode::Char('u'), NONE)],
        scope: KeyScope::Branches,
        intent: AppIntent::SetUpstream,
        description: "Set the remote branch the selected branch tracks",
        label: "upstream",
        bar: true,
        disabled: branch_action_disabled,
    },
    Binding {
        id: "branches.unset_upstream",
        keys: &[KeyPattern::any(KeyCode::Char('U'))],
        scope: KeyScope::Branches,
        intent: AppIntent::UnsetUpstream,
        description: "Stop the selected branch tracking its upstream",
        label: "unset upstream",
        bar: false,
        disabled: unset_upstream_disabled,
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

// Chunk 54: branches
pub(crate) static BRANCHES_DELETE_CONFIRM: &[Binding] = &[
    // As with discarding, Enter is left unbound: a delete cannot be undone.
    Binding {
        id: "delete_branch.yes",
        keys: &[KeyPattern::exact(KeyCode::Char('y'), NONE)],
        scope: KeyScope::DeleteBranch,
        intent: AppIntent::ConfirmDeleteBranch { force: false },
        description: "Yes, delete the merged branch",
        label: "delete",
        bar: true,
        disabled: plain_delete_disabled,
    },
    Binding {
        id: "delete_branch.force",
        keys: &[KeyPattern::any(KeyCode::Char('D'))],
        scope: KeyScope::DeleteBranch,
        intent: AppIntent::ConfirmDeleteBranch { force: true },
        description: "Yes, delete the branch and its unmerged commits",
        label: "force delete",
        bar: true,
        disabled: force_delete_disabled,
    },
    Binding {
        id: "delete_branch.no",
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::DeleteBranch,
        intent: AppIntent::ConfirmNo,
        description: "No, keep the branch",
        label: "keep",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "delete_branch.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::DeleteBranch,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{Keymap, Resolution, registry, resolve};
    use crate::model::Context;
    use crate::test_support::{key, plain};

    #[test]
    fn the_delete_question_lets_only_the_matching_key_answer() {
        use crate::popup::DeleteBranchConfirm;
        let mut state = AppState::new(Context::Workspace);
        let ask = |force| {
            Popup::DeleteBranch(DeleteBranchConfirm {
                name: "t".into(),
                op: crate::op::GitOp::DeleteBranch {
                    name: "t".into(),
                    force,
                },
            })
        };
        let resolve_in = |state: &AppState, key| {
            resolve(
                registry(),
                &Keymap::default(),
                KeyScope::DeleteBranch,
                state,
                key,
            )
        };
        let shift_d = key(KeyCode::Char('D'), KeyModifiers::SHIFT);
        state.popups = vec![ask(false)];
        assert!(matches!(resolve_in(&state, plain('y')), Resolution::Run(_)));
        assert!(matches!(
            resolve_in(&state, shift_d),
            Resolution::Disabled(_)
        ));
        state.popups = vec![ask(true)];
        assert!(matches!(
            resolve_in(&state, plain('y')),
            Resolution::Disabled(_)
        ));
        assert!(matches!(resolve_in(&state, shift_d), Resolution::Run(_)));
        assert!(matches!(
            resolve_in(&state, plain('q')),
            Resolution::Unbound
        ));
        assert_eq!(KeyScope::DeleteBranch.parent(), None);
        assert_eq!(KeyScope::Branches.parent(), Some(KeyScope::Workspace));
    }
}
