//! The Files panel's navigation and menu key bindings. `CHUNKS` in
//! `keymap.rs` places every chunk, so their position there is behaviour.

use crate::git::status::EntryKind;
use crate::intent::AppIntent;
use crate::keymap::{Binding, KeyPattern, KeyScope, NONE, Reason, list_guard, never};
use crate::menu::MenuId;
use crate::model::AppState;
use crossterm::event::KeyCode;

// Chunk 10: files
pub(crate) static FILES_NAV: &[Binding] = &[
    Binding {
        id: "files.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::Files,
        intent: AppIntent::SelectNext,
        description: "Select the next file",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "files.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::Files,
        intent: AppIntent::SelectPrev,
        description: "Select the previous file",
        label: "",
        bar: false,
        disabled: never,
    },
];

// Chunk 38: files
#[rustfmt::skip]
pub(crate) static FILES_MENU: &[Binding] = &[
    Binding {
        id: "files.menu",
        keys: &[KeyPattern::exact(KeyCode::Char('m'), NONE)],
        scope: KeyScope::Files,
        intent: AppIntent::OpenMenu(MenuId::Actions),
        description: "Open the actions menu",
        label: "menu",
        bar: true,
        disabled: never,
    },
];

// Chunk 31: files
#[rustfmt::skip]
pub(crate) static FILES_IGNORE: &[Binding] = &[
    Binding {
        id: "files.ignore",
        keys: &[KeyPattern::exact(KeyCode::Char('i'), NONE)],
        scope: KeyScope::Files,
        intent: AppIntent::OpenIgnore,
        description: "Add to .gitignore",
        label: "ignore",
        // Out of the bar, which is full at 80 columns; the help menu lists it.
        bar: false,
        disabled: files_ignore_disabled,
    },
];

/// Why `i` cannot add the selected file to `.gitignore`. A tracked file
/// stays tracked whatever `.gitignore` says, and Kika never untracks one.
pub(super) fn ignore_disabled(state: &AppState) -> Option<Reason> {
    if state.files.is_empty() {
        return Some("No files".into());
    }
    match state.selected.and_then(|i| state.files.get(i)) {
        Some(file) if file.kind == EntryKind::Untracked => None,
        _ => Some("Only untracked files can be ignored".into()),
    }
}

fn files_ignore_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| ignore_disabled(state))
}
