//! The Blame feature's key bindings: `b` in Files blames the selected file,
//! the blame scope moves the cursor and opens a line's commit, and the
//! commit scope scrolls it. `CHUNKS` in `keymap.rs` places all three chunks,
//! so their position there is behaviour.

use crate::features::blame::BlameIntent;
use crate::git::status::EntryKind;
use crate::intent::{AppIntent, Scroll};
use crate::keymap::{Binding, KeyPattern, KeyScope, NONE, Reason, list_guard, never};
use crate::model::AppState;
use crossterm::event::KeyCode;

/// Why `b` cannot blame the selected file. An untracked or deleted file
/// has no lines git can blame.
pub(crate) fn blame_disabled(state: &AppState) -> Option<Reason> {
    match state.selected.and_then(|i| state.files.get(i)) {
        None => Some("No file selected".into()),
        Some(file) if file.kind == EntryKind::Untracked => {
            Some("Untracked file: nothing to blame".into())
        }
        Some(file) if file.worktree == 'D' || file.index == 'D' => {
            Some("The file is deleted: nothing to blame".into())
        }
        Some(_) => None,
    }
}

fn files_blame_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| blame_disabled(state))
}

// Chunk 28: blame
#[rustfmt::skip]
pub(crate) static BLAME_FILES: &[Binding] = &[
    Binding {
        id: "files.blame",
        keys: &[KeyPattern::exact(KeyCode::Char('b'), NONE)],
        scope: KeyScope::Files,
        intent: AppIntent::Blame(BlameIntent::Open),
        description: "Blame the selected file",
        label: "blame",
        bar: false,
        disabled: files_blame_disabled,
    },
];

// Chunk 35: blame
pub(crate) static BLAME_KEYS: &[Binding] = &[
    Binding {
        id: "blame.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::Blame,
        intent: AppIntent::Blame(BlameIntent::Next),
        description: "Move to the next line",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "blame.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::Blame,
        intent: AppIntent::Blame(BlameIntent::Prev),
        description: "Move to the previous line",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "blame.page_down",
        keys: &[KeyPattern::any(KeyCode::PageDown)],
        scope: KeyScope::Blame,
        intent: AppIntent::Blame(BlameIntent::PageDown),
        description: "Move down a page",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "blame.page_up",
        keys: &[KeyPattern::any(KeyCode::PageUp)],
        scope: KeyScope::Blame,
        intent: AppIntent::Blame(BlameIntent::PageUp),
        description: "Move up a page",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "blame.commit",
        keys: &[KeyPattern::any(KeyCode::Enter)],
        scope: KeyScope::Blame,
        intent: AppIntent::Blame(BlameIntent::OpenCommit),
        description: "Show the commit that last changed this line",
        label: "commit",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "blame.back",
        // Esc is `blame.escape`, which clears a search first and stands in
        // for this one in the bar.
        keys: &[
            KeyPattern::exact(KeyCode::Char('h'), NONE),
            KeyPattern::any(KeyCode::Left),
        ],
        scope: KeyScope::Blame,
        intent: AppIntent::Blame(BlameIntent::Close),
        description: "Back to the file list",
        label: "back",
        bar: false,
        disabled: never,
    },
];

// Chunk 37: blame
pub(crate) static BLAME_COMMIT: &[Binding] = &[
    Binding {
        id: "blame_commit.scroll_down",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::BlameCommit,
        intent: AppIntent::Blame(BlameIntent::ScrollCommit(Scroll::LineDown)),
        description: "Scroll the commit down a line",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "blame_commit.scroll_up",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::BlameCommit,
        intent: AppIntent::Blame(BlameIntent::ScrollCommit(Scroll::LineUp)),
        description: "Scroll the commit up a line",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "blame_commit.page_down",
        keys: &[KeyPattern::any(KeyCode::PageDown)],
        scope: KeyScope::BlameCommit,
        intent: AppIntent::Blame(BlameIntent::ScrollCommit(Scroll::PageDown)),
        description: "Scroll the commit down a page",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "blame_commit.page_up",
        keys: &[KeyPattern::any(KeyCode::PageUp)],
        scope: KeyScope::BlameCommit,
        intent: AppIntent::Blame(BlameIntent::ScrollCommit(Scroll::PageUp)),
        description: "Scroll the commit up a page",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "blame_commit.back",
        // Esc is `blame_commit.escape`, which clears a search first and stands in
        // for this one in the bar.
        keys: &[
            KeyPattern::exact(KeyCode::Char('h'), NONE),
            KeyPattern::any(KeyCode::Left),
        ],
        scope: KeyScope::BlameCommit,
        intent: AppIntent::Blame(BlameIntent::CloseCommit),
        description: "Back to the blame, at the same line",
        label: "back",
        bar: false,
        disabled: never,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::registry;
    use crate::test_support::{from, key, plain};
    use crossterm::event::KeyModifiers;

    #[test]
    fn b_blames_from_files_and_the_blame_scopes_have_their_own_keys() {
        use AppIntent::*;
        let none = KeyModifiers::NONE;
        let b = registry()
            .iter()
            .find(|b| b.scope == KeyScope::Files && b.matches(plain('b')))
            .expect("b is bound in Files");
        assert_eq!(b.intent, Blame(BlameIntent::Open));
        assert_eq!(b.label, "blame");
        assert!(!b.bar);
        assert_eq!(from(KeyScope::Diff, plain('b')), None);

        for scope in [KeyScope::Blame, KeyScope::BlameCommit] {
            assert_eq!(scope.parent(), Some(KeyScope::Workspace));
        }
        assert_eq!(KeyScope::Blame.title(), "Blame");
        assert_eq!(KeyScope::BlameCommit.title(), "Commit");

        let blame = |k| from(KeyScope::Blame, k);
        assert_eq!(blame(plain('j')), Some(Blame(BlameIntent::Next)));
        assert_eq!(
            blame(key(KeyCode::Down, none)),
            Some(Blame(BlameIntent::Next))
        );
        assert_eq!(blame(plain('k')), Some(Blame(BlameIntent::Prev)));
        assert_eq!(
            blame(key(KeyCode::Up, none)),
            Some(Blame(BlameIntent::Prev))
        );
        assert_eq!(
            blame(key(KeyCode::PageDown, none)),
            Some(Blame(BlameIntent::PageDown))
        );
        assert_eq!(
            blame(key(KeyCode::PageUp, none)),
            Some(Blame(BlameIntent::PageUp))
        );
        assert_eq!(
            blame(key(KeyCode::Enter, none)),
            Some(Blame(BlameIntent::OpenCommit))
        );
        // Esc clears a search first, then closes the blame.
        assert_eq!(blame(key(KeyCode::Esc, none)), Some(PaneEscape));
        for k in [plain('h'), key(KeyCode::Left, none)] {
            assert_eq!(blame(k), Some(Blame(BlameIntent::Close)), "{k:?}");
        }
        assert_eq!(blame(plain('q')), Some(Quit));
        // File and hunk keys do not reach the blame.
        for c in [' ', 'a', 'c', 'd', 'o', 'b', 'v', 't', 'e'] {
            assert_eq!(blame(plain(c)), None, "{c:?}");
        }

        let commit = |k| from(KeyScope::BlameCommit, k);
        assert_eq!(
            commit(plain('j')),
            Some(Blame(BlameIntent::ScrollCommit(Scroll::LineDown)))
        );
        assert_eq!(
            commit(plain('k')),
            Some(Blame(BlameIntent::ScrollCommit(Scroll::LineUp)))
        );
        assert_eq!(
            commit(key(KeyCode::PageDown, none)),
            Some(Blame(BlameIntent::ScrollCommit(Scroll::PageDown)))
        );
        assert_eq!(commit(key(KeyCode::Esc, none)), Some(PaneEscape));
        for k in [plain('h'), key(KeyCode::Left, none)] {
            assert_eq!(commit(k), Some(Blame(BlameIntent::CloseCommit)), "{k:?}");
        }
        assert_eq!(commit(key(KeyCode::Enter, none)), None);
    }
}
