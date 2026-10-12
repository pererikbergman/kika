//! The operation's key bindings: `m` on a branch opens the merge menu,
//! the Status panel's `c`, `s` and `a` continue, skip or abort the
//! operation in progress, and the abort question's own keys answer it.
//! `CHUNKS` in `keymap.rs` places the chunks, so their position there is
//! behaviour.

use crate::bisect;
use crate::intent::AppIntent;
use crate::keymap::{
    Binding, KeyPattern, KeyScope, NONE, Reason, branch_action_disabled, never, resumable_operation,
};
use crate::menu::MenuId;
use crate::model::AppState;
use crossterm::event::{KeyCode, KeyModifiers};

/// Why the selected branch cannot be merged or rebased onto now: not into
/// itself, and not while another operation waits to be finished.
pub(crate) fn merge_disabled(state: &AppState) -> Option<Reason> {
    branch_action_disabled(state).or_else(|| {
        if state.branches.selected_branch().is_some_and(|b| b.head) {
            Some("Already on this branch".into())
        } else if resumable_operation(state).is_some() {
            Some("Finish or abort the operation in progress".into())
        } else {
            crate::bisect::head_move_disabled(state)
        }
    })
}

// Chunk 46: operation
#[rustfmt::skip]
pub(crate) static OPERATION_BRANCHES: &[Binding] = &[
    Binding {
        id: "branches.merge",
        keys: &[KeyPattern::exact(KeyCode::Char('m'), NONE)],
        scope: KeyScope::Branches,
        intent: AppIntent::OpenMenu(MenuId::Merge),
        description: "Merge the selected branch into the current one, or rebase onto it",
        label: "merge",
        bar: true,
        disabled: merge_disabled,
    },
];

// Chunk 52: operation
pub(crate) static OPERATION_STATUS: &[Binding] = &[
    Binding {
        id: "status.continue",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), NONE)],
        scope: KeyScope::Status,
        intent: AppIntent::ContinueOperation,
        description: "Continue the merge, rebase, cherry-pick or revert once conflicts are resolved",
        label: "continue",
        bar: true,
        disabled: bisect::continue_disabled,
    },
    Binding {
        id: "status.skip",
        keys: &[KeyPattern::exact(KeyCode::Char('s'), NONE)],
        scope: KeyScope::Status,
        intent: AppIntent::SkipOperation,
        description: "Skip the commit the rebase, cherry-pick or revert stopped on, or the one a bisect is testing",
        label: "skip",
        bar: true,
        disabled: bisect::skip_disabled,
    },
    Binding {
        id: "status.abort",
        keys: &[KeyPattern::exact(KeyCode::Char('a'), NONE)],
        scope: KeyScope::Status,
        intent: AppIntent::AbortOperation,
        description: "Abort the operation in progress, or reset the bisect, asking first",
        label: "abort",
        bar: true,
        disabled: bisect::abort_disabled,
    },
];

// Chunk 61: operation
pub(crate) static OPERATION_ABORT_CONFIRM: &[Binding] = &[
    // As with discarding, Enter is left unbound: aborting loses resolved
    // conflicts.
    Binding {
        id: "abort_confirm.yes",
        keys: &[KeyPattern::exact(KeyCode::Char('y'), NONE)],
        scope: KeyScope::AbortConfirm,
        intent: AppIntent::ConfirmYes,
        description: "Yes, abort the operation",
        label: "abort",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "abort_confirm.no",
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::AbortConfirm,
        intent: AppIntent::ConfirmNo,
        description: "No, keep the operation going",
        label: "keep",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "abort_confirm.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::AbortConfirm,
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
    use crate::git::repo_state::Operation;
    use crate::git::status::Head;
    use crate::keymap::{Keymap, Resolution, registry, resolve};
    use crate::model::Context;
    use crate::test_support::{key, plain};

    #[test]
    fn the_abort_question_has_no_enter_and_n_or_esc_keeps_the_operation() {
        let state = AppState::new(Context::Workspace);
        let resolve_in = |key| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::AbortConfirm,
            &state,
            key,
        ) {
            Resolution::Run(binding) => Some(binding.intent.clone()),
            Resolution::Unbound => None,
            other => panic!("{key:?}: {other:?}"),
        };
        let none = KeyModifiers::NONE;
        assert_eq!(resolve_in(key(KeyCode::Enter, none)), None);
        assert_eq!(resolve_in(plain('q')), None);
        assert_eq!(resolve_in(plain('y')), Some(AppIntent::ConfirmYes));
        assert_eq!(resolve_in(plain('n')), Some(AppIntent::ConfirmNo));
        assert_eq!(
            resolve_in(key(KeyCode::Esc, none)),
            Some(AppIntent::ConfirmNo)
        );
        assert_eq!(
            resolve_in(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(AppIntent::Quit)
        );
        assert_eq!(KeyScope::AbortConfirm.parent(), None);
        assert_eq!(KeyScope::AbortConfirm.title(), "Abort");

        let enter = key(KeyCode::Enter, none);
        for b in registry()
            .iter()
            .filter(|b| b.scope == KeyScope::AbortConfirm)
        {
            assert!(!b.keys.iter().any(|k| k.matches(enter)), "{}", b.id);
        }
    }

    #[test]
    fn the_status_panel_controls_the_operation_and_falls_back_to_the_workspace() {
        use AppIntent::*;
        assert_eq!(KeyScope::Status.parent(), Some(KeyScope::Workspace));
        assert_eq!(KeyScope::Status.title(), "Status");
        // A rebase is in progress, so all three keys are enabled.
        let mut state = AppState::new(Context::Workspace);
        state.branch = Some(crate::git::status::BranchStatus {
            head: Head::Branch("main".into()),
            upstream: None,
            ahead_behind: None,
            in_progress: vec![Operation::Rebase],
            bisect: None,
        });
        let status = |c| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::Status,
            &state,
            plain(c),
        ) {
            Resolution::Run(binding) => Some(binding.intent.clone()),
            _ => None,
        };
        assert_eq!(status('c'), Some(ContinueOperation));
        assert_eq!(status('s'), Some(SkipOperation));
        assert_eq!(status('a'), Some(AbortOperation));
        assert_eq!(status('r'), Some(RefreshStatus));
        let merge = registry().iter().find(|b| b.id == "branches.merge");
        assert_eq!(merge.map(|b| &b.intent), Some(&OpenMenu(MenuId::Merge)));
    }
}
