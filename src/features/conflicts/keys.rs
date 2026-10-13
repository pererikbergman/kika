//! The conflict view's key bindings: `M` in Files hands a conflicted file
//! to the merge tool, and the view's own scope moves between blocks, picks
//! a side, opens the merge tool and goes back. `CHUNKS` in `keymap.rs`
//! places both chunks, so their position there is behaviour.

use super::state::Choice;
use crate::features::conflicts::ConflictsIntent;
use crate::git::status::EntryKind;
use crate::intent::AppIntent;
use crate::keymap::{Binding, KeyPattern, KeyScope, NONE, OP_RUNNING, Reason, list_guard, never};
use crate::model::{AppState, Focus};
use crossterm::event::KeyCode;

/// Why the conflict view cannot pick a side now: one write at a time, and
/// only in a file whose blocks were read.
pub(crate) fn pick_disabled(state: &AppState) -> Option<Reason> {
    if state.op_is_running() {
        return Some(OP_RUNNING.into());
    }
    match &state.focus {
        Focus::Conflict(view) if view.loaded.is_some() => None,
        _ => Some("Nothing to pick".into()),
    }
}

/// Why the merge tool cannot start now: in the conflict view it is for
/// that file; from Files, only for a conflicted one.
pub(crate) fn mergetool_disabled(state: &AppState) -> Option<Reason> {
    if state.op_is_running() {
        return Some(OP_RUNNING.into());
    }
    if matches!(state.focus, Focus::Conflict(_)) {
        return None;
    }
    match state.files.selected_row() {
        Some(file) if file.kind == EntryKind::Conflicted => None,
        _ => Some("Not a conflicted file".into()),
    }
}

fn files_mergetool_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| mergetool_disabled(state))
}

// Chunk 30: conflicts
#[rustfmt::skip]
pub(crate) static CONFLICTS_FILES: &[Binding] = &[
    Binding {
        id: "files.mergetool",
        keys: &[KeyPattern::any(KeyCode::Char('M'))],
        scope: KeyScope::Files,
        intent: AppIntent::Conflicts(ConflictsIntent::OpenMergeTool),
        description: "Resolve the selected conflicted file in your merge tool",
        label: "merge tool",
        // Out of the bar, which is full at 80 columns; the help menu lists it.
        bar: false,
        disabled: files_mergetool_disabled,
    },
];

// Chunk 36: conflicts
pub(crate) static CONFLICTS_KEYS: &[Binding] = &[
    Binding {
        id: "conflict.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::Conflict,
        intent: AppIntent::Conflicts(ConflictsIntent::Next),
        description: "Move to the next conflict",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "conflict.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::Conflict,
        intent: AppIntent::Conflicts(ConflictsIntent::Prev),
        description: "Move to the previous conflict",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "conflict.ours",
        keys: &[KeyPattern::exact(KeyCode::Char('o'), NONE)],
        scope: KeyScope::Conflict,
        intent: AppIntent::Conflicts(ConflictsIntent::Pick(Choice::Ours)),
        description: "Keep our side of this conflict and write the file",
        label: "ours",
        bar: true,
        disabled: pick_disabled,
    },
    Binding {
        id: "conflict.theirs",
        keys: &[KeyPattern::exact(KeyCode::Char('t'), NONE)],
        scope: KeyScope::Conflict,
        intent: AppIntent::Conflicts(ConflictsIntent::Pick(Choice::Theirs)),
        description: "Keep their side of this conflict and write the file",
        label: "theirs",
        bar: true,
        disabled: pick_disabled,
    },
    Binding {
        id: "conflict.both",
        keys: &[KeyPattern::exact(KeyCode::Char('b'), NONE)],
        scope: KeyScope::Conflict,
        intent: AppIntent::Conflicts(ConflictsIntent::Pick(Choice::Both)),
        description: "Keep both sides of this conflict, ours first, and write the file",
        label: "both",
        bar: true,
        disabled: pick_disabled,
    },
    Binding {
        id: "conflict.mergetool",
        keys: &[KeyPattern::any(KeyCode::Char('M'))],
        scope: KeyScope::Conflict,
        intent: AppIntent::Conflicts(ConflictsIntent::OpenMergeTool),
        description: "Resolve this file in your merge tool instead",
        label: "merge tool",
        bar: true,
        disabled: mergetool_disabled,
    },
    Binding {
        id: "conflict.back",
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::Conflict,
        intent: AppIntent::FocusLeft,
        description: "Back to the file list; picks already written stay",
        label: "back",
        bar: true,
        disabled: never,
    },
];
