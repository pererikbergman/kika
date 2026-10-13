//! The Worktrees popup's key bindings: `W` in the workspace opens it, its own
//! scope switches to, adds, removes and filters worktrees, and the remove
//! question answers with `y` or `n`. `CHUNKS` in `keymap.rs` places all three
//! chunks, so their position there is behaviour.

use super::list::{
    add_worktree_disabled, open_worktree_disabled, remove_worktree_disabled, worktrees_disabled,
};
use crate::confirm_question::Answer;
use crate::intent::AppIntent;
use crate::keymap::{Binding, KeyPattern, KeyScope, NONE, never};
use crossterm::event::{KeyCode, KeyModifiers};

// Chunk 24: worktrees
#[rustfmt::skip]
pub(crate) static WORKTREES_WORKSPACE: &[Binding] = &[
    Binding {
        id: "workspace.worktrees",
        keys: &[KeyPattern::any(KeyCode::Char('W'))],
        scope: KeyScope::Workspace,
        intent: AppIntent::OpenWorktrees,
        description: "Show worktrees, to switch to, add or remove one",
        label: "worktrees",
        bar: false,
        disabled: worktrees_disabled,
    },
];

// Chunk 67: worktrees
pub(crate) static WORKTREES_KEYS: &[Binding] = &[
    Binding {
        id: "worktrees.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::Worktrees,
        intent: AppIntent::WorktreeNext,
        description: "Select the next worktree",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "worktrees.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::Worktrees,
        intent: AppIntent::WorktreePrev,
        description: "Select the previous worktree",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "worktrees.open",
        keys: &[KeyPattern::any(KeyCode::Enter)],
        scope: KeyScope::Worktrees,
        intent: AppIntent::OpenWorktree,
        description: "Switch to the selected worktree",
        label: "switch",
        bar: true,
        disabled: open_worktree_disabled,
    },
    Binding {
        id: "worktrees.add",
        keys: &[KeyPattern::exact(KeyCode::Char('a'), NONE)],
        scope: KeyScope::Worktrees,
        intent: AppIntent::AddWorktree,
        description: "Add a worktree for the local branch selected in the Branches panel",
        label: "add",
        bar: true,
        disabled: add_worktree_disabled,
    },
    Binding {
        id: "worktrees.remove",
        keys: &[KeyPattern::exact(KeyCode::Char('d'), NONE)],
        scope: KeyScope::Worktrees,
        intent: AppIntent::RemoveWorktree,
        description: "Remove the selected linked worktree",
        label: "remove",
        bar: true,
        disabled: remove_worktree_disabled,
    },
    Binding {
        id: "worktrees.filter",
        keys: &[KeyPattern::any(KeyCode::Char('/'))],
        scope: KeyScope::Worktrees,
        intent: AppIntent::OpenFilter,
        description: "Filter the worktrees by branch or path",
        label: "filter",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "worktrees.escape",
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::Worktrees,
        intent: AppIntent::ListEscape,
        description: "Clear the filter, else close the list",
        label: "clear",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "worktrees.close",
        keys: &[KeyPattern::exact(KeyCode::Char('q'), NONE)],
        scope: KeyScope::Worktrees,
        intent: AppIntent::PopPopup,
        description: "Close the list",
        label: "close",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "worktrees.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::Worktrees,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
];

// Chunk 69: worktrees
pub(crate) static WORKTREES_REMOVE_CONFIRM: &[Binding] = &[
    // Enter is left unbound: the worktree's directory goes with it.
    Binding {
        id: "remove_worktree.yes",
        keys: Answer::YOnly.keys(),
        scope: KeyScope::RemoveWorktree,
        intent: AppIntent::ConfirmRemoveWorktree,
        description: "Yes, remove the worktree and its directory",
        label: "remove",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "remove_worktree.no",
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::RemoveWorktree,
        intent: AppIntent::ConfirmNo,
        description: "No, keep the worktree",
        label: "keep",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "remove_worktree.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::RemoveWorktree,
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
    use crate::keymap::{
        FILTER_HIDES_ROW, Keymap, REPO_LOCKED, Resolution, Trust, registry, resolve,
    };
    use crate::model::{AppState, Context};
    use crate::test_support::{build, from, key, load_list, plain};

    #[test]
    fn the_worktree_list_has_its_own_keys_and_never_reaches_global_ones() {
        use crate::features::worktrees::WorktreeList;
        use crate::features::worktrees::git::parse_list;
        use crate::popup::Popup;
        use AppIntent::*;

        let none = KeyModifiers::NONE;
        let shift = KeyModifiers::SHIFT;
        let mut workspace = AppState::new(Context::Workspace);
        workspace.repo = Some("/r/a".into());
        for scope in [KeyScope::Files, KeyScope::Branches, KeyScope::Diff] {
            let found = resolve(
                registry(),
                &Keymap::default(),
                scope,
                &workspace,
                key(KeyCode::Char('W'), shift),
            );
            assert!(
                matches!(found, Resolution::Run(b) if b.intent == OpenWorktrees),
                "{scope:?}"
            );
        }
        assert_eq!(KeyScope::Worktrees.parent(), None);
        assert_eq!(KeyScope::Worktrees.title(), "Worktrees");
        assert_eq!(KeyScope::RemoveWorktree.parent(), None);
        assert_eq!(KeyScope::RemoveWorktree.title(), "Remove worktree");

        let mut state = AppState::new(Context::Workspace);
        state.repo = Some("/r/a".into());
        let mut list = WorktreeList::loading(0);
        load_list(
            &mut list.list,
            Ok(parse_list(
                b"worktree /r/a\0bare\0\0worktree /r/b\0detached\0\0",
                0,
            )),
        );
        list.list.move_by(1);
        state.popups = vec![Popup::Worktrees(list)];
        assert_eq!(state.key_focus(), KeyScope::Worktrees);
        let run = |k| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::Worktrees,
            &state,
            k,
        ) {
            Resolution::Run(binding) => Some(binding.intent.clone()),
            Resolution::Disabled(reason) => panic!("{k:?} disabled: {reason}"),
            Resolution::Unbound | Resolution::Command(_) => None,
        };
        assert_eq!(run(plain('j')), Some(WorktreeNext));
        assert_eq!(run(key(KeyCode::Down, none)), Some(WorktreeNext));
        assert_eq!(run(plain('k')), Some(WorktreePrev));
        assert_eq!(run(key(KeyCode::Up, none)), Some(WorktreePrev));
        assert_eq!(run(key(KeyCode::Enter, none)), Some(OpenWorktree));
        assert_eq!(run(plain('d')), Some(RemoveWorktree));
        assert_eq!(run(key(KeyCode::Esc, none)), Some(ListEscape));
        assert_eq!(run(plain('q')), Some(PopPopup));
        assert_eq!(run(plain('/')), Some(OpenFilter));
        assert_eq!(
            run(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Quit)
        );
        for k in [plain('?'), plain('r'), plain('W')] {
            assert_eq!(run(k), None, "{k:?}");
        }
    }

    #[test]
    fn a_filter_hiding_every_worktree_disables_switch_and_remove_but_not_add() {
        use crate::features::worktrees::WorktreeList;
        use crate::features::worktrees::git::parse_list;
        use crate::popup::Popup;
        let mut state = AppState::new(Context::Workspace);
        state.repo = Some("/r/a".into());
        let mut list = WorktreeList::loading(0);
        load_list(
            &mut list.list,
            Ok(parse_list(
                b"worktree /r/a\0branch refs/heads/main\0\0\
              worktree /r/b\0branch refs/heads/topic\0\0",
                0,
            )),
        );
        list.list.move_by(1);
        list.list.filter = "zzz".into();
        state.popups = vec![Popup::Worktrees(list)];
        let found = |k| {
            resolve(
                registry(),
                &Keymap::default(),
                KeyScope::Worktrees,
                &state,
                k,
            )
        };
        for k in [key(KeyCode::Enter, KeyModifiers::NONE), plain('d')] {
            assert!(
                matches!(found(k), Resolution::Disabled(ref why) if why == FILTER_HIDES_ROW),
                "{k:?}"
            );
        }
        // Adding is for the Branches panel's branch, not a listed row.
        assert!(
            !matches!(found(plain('a')), Resolution::Disabled(ref why) if why == FILTER_HIDES_ROW)
        );
        assert_eq!(
            crate::features::worktrees::list::add_worktree_disabled(&state),
            Some("Select a local branch in the Branches panel".into())
        );
    }

    #[test]
    fn the_worktrees_escape_is_locked_and_its_filter_is_not() {
        assert!(REPO_LOCKED.contains(&"worktrees.escape"));
        assert!(build(&[("worktrees.escape", &["Z"])], Trust::Repo).is_err());
        assert!(!REPO_LOCKED.contains(&"worktrees.filter"));
        assert!(build(&[("worktrees.filter", &["Z"])], Trust::Repo).is_ok());
    }

    #[test]
    fn the_remove_worktree_question_has_no_enter_and_answers_with_y_or_n() {
        let enter = key(KeyCode::Enter, KeyModifiers::NONE);
        for b in registry()
            .iter()
            .filter(|b| b.scope == KeyScope::RemoveWorktree)
        {
            assert!(!b.matches(enter), "{} answers on Enter", b.id);
        }
        assert_eq!(
            from(KeyScope::RemoveWorktree, plain('y')),
            Some(AppIntent::ConfirmRemoveWorktree)
        );
        for k in [plain('n'), key(KeyCode::Esc, KeyModifiers::NONE)] {
            assert_eq!(
                from(KeyScope::RemoveWorktree, k),
                Some(AppIntent::ConfirmNo)
            );
        }
        for id in [
            "worktrees.open",
            "worktrees.add",
            "worktrees.remove",
            "worktrees.quit",
            "remove_worktree.yes",
            "remove_worktree.no",
            "remove_worktree.quit",
        ] {
            assert!(REPO_LOCKED.contains(&id), "{id}");
        }
    }
}
