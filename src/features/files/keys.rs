//! The Files panel's navigation and menu key bindings. `CHUNKS` in
//! `keymap.rs` places every chunk, so their position there is behaviour.

use crate::intent::AppIntent;
use crate::keymap::{Binding, KeyPattern, KeyScope, NONE, never};
use crate::menu::MenuId;
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
