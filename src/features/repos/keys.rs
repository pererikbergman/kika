//! The recent list's key bindings: `O` in the workspace opens it, and the
//! list's own scope moves, opens, inits and closes. `CHUNKS` in `keymap.rs`
//! places both chunks, so their position there is behaviour.

use crate::intent::AppIntent;
use crate::keymap::{Binding, KeyPattern, KeyScope, NONE, Reason, never};
use crate::model::AppState;
use crate::popup::Popup;
use crossterm::event::{KeyCode, KeyModifiers};

/// Why `i` cannot make the launch directory a repository: it is offered
/// only by the recent list shown outside one.
pub(crate) fn init_repo_disabled(state: &AppState) -> Option<Reason> {
    let offered = matches!(state.popups.last(), Some(Popup::Recent(list)) if list.offer_init);
    if state.repo.is_some() || !offered {
        Some("Already in a repository".into())
    } else if state.is_busy() {
        Some("Loading…".into())
    } else {
        None
    }
}

// Chunk 26: repos
#[rustfmt::skip]
pub(crate) static REPOS_WORKSPACE: &[Binding] = &[
    Binding {
        id: "workspace.recent",
        keys: &[KeyPattern::any(KeyCode::Char('O'))],
        // From the Diff pane too, so switching away can keep it focused.
        scope: KeyScope::Workspace,
        intent: AppIntent::OpenRecent,
        description: "Show recent repositories, to switch to one",
        label: "recent",
        bar: false,
        disabled: never,
    },
];

// Chunk 70: repos
pub(crate) static REPOS_RECENT: &[Binding] = &[
    Binding {
        id: "recent.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::Recent,
        intent: AppIntent::RecentNext,
        description: "Select the next row",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "recent.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::Recent,
        intent: AppIntent::RecentPrev,
        description: "Select the previous row",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "recent.open",
        keys: &[KeyPattern::any(KeyCode::Enter)],
        scope: KeyScope::Recent,
        intent: AppIntent::RecentSelect,
        description: "Open the selected repository",
        label: "open",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "recent.init",
        keys: &[KeyPattern::exact(KeyCode::Char('i'), NONE)],
        scope: KeyScope::Recent,
        intent: AppIntent::InitRepo,
        description: "Make the directory Kika started in a repository",
        label: "init",
        bar: true,
        disabled: init_repo_disabled,
    },
    Binding {
        id: "recent.close",
        keys: &[
            KeyPattern::any(KeyCode::Esc),
            KeyPattern::exact(KeyCode::Char('q'), NONE),
        ],
        scope: KeyScope::Recent,
        intent: AppIntent::PopPopup,
        description: "Close the list",
        label: "close",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "recent.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::Recent,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
];

#[cfg(test)]
mod tests {
    use crate::intent::AppIntent;
    use crate::keymap::{KeyScope, Keymap, Resolution, registry, resolve};
    use crate::model::{AppState, Context};
    use crate::test_support::{from, key, plain};
    use crossterm::event::{KeyCode, KeyModifiers};

    #[test]
    fn shift_o_opens_the_recent_list_from_the_workspace_panels() {
        let o = key(KeyCode::Char('O'), KeyModifiers::SHIFT);
        for scope in [
            KeyScope::Files,
            KeyScope::Diff,
            KeyScope::Lines,
            KeyScope::Blame,
        ] {
            assert_eq!(from(scope, o), Some(AppIntent::OpenRecent), "{scope:?}");
        }
    }

    #[test]
    fn the_recent_list_has_its_own_keys() {
        use AppIntent::*;
        let recent = |k| from(KeyScope::Recent, k);
        assert_eq!(recent(plain('j')), Some(RecentNext));
        assert_eq!(
            recent(key(KeyCode::Down, KeyModifiers::NONE)),
            Some(RecentNext)
        );
        assert_eq!(recent(plain('k')), Some(RecentPrev));
        assert_eq!(
            recent(key(KeyCode::Up, KeyModifiers::NONE)),
            Some(RecentPrev)
        );
        assert_eq!(
            recent(key(KeyCode::Enter, KeyModifiers::NONE)),
            Some(RecentSelect)
        );
        assert_eq!(
            recent(key(KeyCode::Esc, KeyModifiers::NONE)),
            Some(PopPopup)
        );
        assert_eq!(recent(plain('q')), Some(PopPopup));
        assert_eq!(
            recent(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Quit)
        );
        // `i` only where the list offers it.
        let state = AppState::new(Context::Workspace);
        assert!(matches!(
            resolve(
                registry(),
                &Keymap::default(),
                KeyScope::Recent,
                &state,
                plain('i')
            ),
            Resolution::Disabled(ref reason) if reason == "Already in a repository"
        ));
    }
}
