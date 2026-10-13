//! The Submodules feature's key bindings and their guards: Backspace in
//! the workspace leaves a submodule, `S` on the files opens the list, and
//! the list's own scope moves, enters, runs init and update, and closes.
//! `CHUNKS` in `keymap.rs` places the three chunks, so their position there
//! is behaviour.

use crate::features::submodules::SubmodulesIntent;
use crate::git::submodule::SubmoduleState;
use crate::intent::AppIntent;
use crate::keymap::{Binding, KeyPattern, KeyScope, NONE, OP_RUNNING, Reason, never};
use crate::model::AppState;
use crossterm::event::{KeyCode, KeyModifiers};

fn no_submodule(state: &AppState) -> Option<Reason> {
    state
        .selected_submodule()
        .is_none()
        .then_some("No submodule selected".into())
}

/// Why `i` or `u` cannot run on the selected submodule right now.
fn submodule_op_disabled(state: &AppState) -> Option<Reason> {
    if state.op_is_running() {
        return Some(OP_RUNNING.into());
    }
    no_submodule(state)
}

/// Why the selected submodule cannot be entered: it must be surely checked
/// out, and an operation must not be running in the repository being left.
pub(crate) fn enter_submodule_disabled(state: &AppState) -> Option<Reason> {
    if state.op_is_running() {
        return Some("Wait for the operation to finish".into());
    }
    match state.selected_submodule() {
        None => Some("No submodule selected".into()),
        Some(sub) if sub.state == SubmoduleState::Uninitialized => {
            Some("Update the submodule first".into())
        }
        // Git marks a conflict before it looks for a checkout, so the
        // directory may hold none, and git there would find the parent.
        Some(sub) if sub.state == SubmoduleState::Conflicted => {
            Some("Resolve the submodule conflict first".into())
        }
        Some(_) => None,
    }
}

/// Why there is no parent to go back to right now.
pub(crate) fn leave_submodule_disabled(state: &AppState) -> Option<Reason> {
    if state.repo_stack.is_empty() {
        Some("Not in a submodule".into())
    } else if state.op_is_running() {
        Some("Wait for the operation to finish".into())
    } else {
        None
    }
}

// Chunk 4: submodules
#[rustfmt::skip]
pub(crate) static SUBMODULES_WORKSPACE: &[Binding] = &[
    Binding {
        id: "workspace.parent",
        keys: &[KeyPattern::any(KeyCode::Backspace)],
        scope: KeyScope::Workspace,
        intent: AppIntent::Submodules(SubmodulesIntent::Leave),
        description: "Back to the parent repository",
        label: "parent",
        bar: true,
        disabled: leave_submodule_disabled,
    },
];

// Chunk 23: submodules
#[rustfmt::skip]
pub(crate) static SUBMODULES_FILES: &[Binding] = &[
    Binding {
        id: "files.submodules",
        keys: &[KeyPattern::any(KeyCode::Char('S'))],
        scope: KeyScope::Files,
        intent: AppIntent::Submodules(SubmodulesIntent::Open),
        description: "Show submodules",
        label: "submodules",
        bar: false,
        disabled: never,
    },
];

// Chunk 66: submodules
pub(crate) static SUBMODULES_KEYS: &[Binding] = &[
    Binding {
        id: "submodules.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::Submodules,
        intent: AppIntent::Submodules(SubmodulesIntent::Next),
        description: "Select the next submodule",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "submodules.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::Submodules,
        intent: AppIntent::Submodules(SubmodulesIntent::Prev),
        description: "Select the previous submodule",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "submodules.enter",
        keys: &[KeyPattern::any(KeyCode::Enter)],
        scope: KeyScope::Submodules,
        intent: AppIntent::Submodules(SubmodulesIntent::Enter),
        description: "Work in the selected submodule",
        label: "enter",
        bar: true,
        disabled: enter_submodule_disabled,
    },
    Binding {
        id: "submodules.update",
        keys: &[KeyPattern::exact(KeyCode::Char('u'), NONE)],
        scope: KeyScope::Submodules,
        intent: AppIntent::Submodules(SubmodulesIntent::Update),
        description: "Clone or check out the selected submodule at the recorded commit",
        label: "update",
        bar: true,
        disabled: submodule_op_disabled,
    },
    Binding {
        id: "submodules.init",
        keys: &[KeyPattern::exact(KeyCode::Char('i'), NONE)],
        scope: KeyScope::Submodules,
        intent: AppIntent::Submodules(SubmodulesIntent::Init),
        description: "Record the selected submodule's URL without cloning it",
        label: "init",
        bar: true,
        disabled: submodule_op_disabled,
    },
    Binding {
        id: "submodules.close",
        keys: &[
            KeyPattern::any(KeyCode::Esc),
            KeyPattern::exact(KeyCode::Char('q'), NONE),
        ],
        scope: KeyScope::Submodules,
        intent: AppIntent::PopPopup,
        description: "Close the list",
        label: "close",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "submodules.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::Submodules,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
];

#[cfg(test)]
mod tests {
    use crate::features::submodules::SubmodulesIntent;
    use crate::intent::AppIntent;
    use crate::keymap::{Binding, KeyPattern, KeyScope, Keymap, Resolution, registry, resolve};
    use crate::model::{AppState, Context};
    use crate::test_support::{from, key, plain};
    use crossterm::event::{KeyCode, KeyModifiers};

    #[test]
    fn the_submodule_list_has_its_own_keys_and_never_reaches_global_ones() {
        use crate::features::submodules::SubmoduleList;
        use crate::git::submodule::{Submodule, SubmoduleState};
        use crate::popup::Popup;
        use AppIntent::*;

        let none = KeyModifiers::NONE;
        let shift = KeyModifiers::SHIFT;
        assert_eq!(
            from(KeyScope::Files, key(KeyCode::Char('S'), shift)),
            Some(Submodules(SubmodulesIntent::Open))
        );
        assert_eq!(KeyScope::Submodules.parent(), None);
        assert_eq!(KeyScope::Submodules.title(), "Submodules");

        let mut state = AppState::new(Context::Workspace);
        let mut list = SubmoduleList::loading(0);
        crate::test_support::load_list(
            &mut list.list,
            Ok(vec![Submodule {
                path: "sub".into(),
                oid: "abc".into(),
                state: SubmoduleState::UpToDate,
            }]),
        );
        state.popups = vec![Popup::Submodules(list)];
        assert_eq!(state.key_focus(), KeyScope::Submodules);
        let run = |k| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::Submodules,
            &state,
            k,
        ) {
            Resolution::Run(binding) => Some(binding.intent.clone()),
            Resolution::Disabled(reason) => panic!("{k:?} disabled: {reason}"),
            Resolution::Unbound | Resolution::Command(_) => None,
        };
        assert_eq!(run(plain('j')), Some(Submodules(SubmodulesIntent::Next)));
        assert_eq!(
            run(key(KeyCode::Down, none)),
            Some(Submodules(SubmodulesIntent::Next))
        );
        assert_eq!(run(plain('k')), Some(Submodules(SubmodulesIntent::Prev)));
        assert_eq!(
            run(key(KeyCode::Up, none)),
            Some(Submodules(SubmodulesIntent::Prev))
        );
        assert_eq!(
            run(key(KeyCode::Enter, none)),
            Some(Submodules(SubmodulesIntent::Enter))
        );
        assert_eq!(run(plain('i')), Some(Submodules(SubmodulesIntent::Init)));
        assert_eq!(run(plain('u')), Some(Submodules(SubmodulesIntent::Update)));
        assert_eq!(run(key(KeyCode::Esc, none)), Some(PopPopup));
        assert_eq!(run(plain('q')), Some(PopPopup));
        assert_eq!(
            run(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Quit)
        );
        for k in [
            plain('?'),
            plain('r'),
            plain(' '),
            key(KeyCode::Backspace, none),
        ] {
            assert_eq!(run(k), None, "{k:?}");
        }
    }

    #[test]
    fn backspace_leaves_a_submodule_from_every_workspace_panel_once_inside_one() {
        let backspace = key(KeyCode::Backspace, KeyModifiers::NONE);
        let mut state = AppState::new(Context::Workspace);
        for scope in [KeyScope::Files, KeyScope::Diff, KeyScope::Lines] {
            assert!(
                matches!(
                    resolve(registry(), &Keymap::default(), scope, &state, backspace),
                    Resolution::Disabled(ref reason) if reason == "Not in a submodule"
                ),
                "{scope:?}"
            );
        }
        state.repo_stack.push(crate::model::ParentRepo {
            path: "sub".into(),
            files: vec![],
            files_loaded: true,
            branch: None,
            selected: None,
        });
        for scope in [KeyScope::Files, KeyScope::Diff, KeyScope::Lines] {
            assert!(
                matches!(
                    resolve(registry(), &Keymap::default(), scope, &state, backspace),
                    Resolution::Run(Binding {
                        intent: AppIntent::Submodules(SubmodulesIntent::Leave),
                        ..
                    })
                ),
                "{scope:?}"
            );
        }
        state.hold_op(1);
        assert!(matches!(
            resolve(
                registry(),
                &Keymap::default(),
                KeyScope::Files,
                &state,
                backspace
            ),
            Resolution::Disabled(ref reason) if reason == "Wait for the operation to finish"
        ));
        assert_eq!(
            KeyPattern::any(KeyCode::Backspace).label(),
            "bksp",
            "the same on every platform"
        );
    }
}
