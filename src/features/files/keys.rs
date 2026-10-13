//! The Files panel's navigation, stage, discard, open, ignore and menu
//! key bindings, the discard question's keys, and the guards they use.
//! `CHUNKS` in `keymap.rs` places every chunk, so their position there is
//! behaviour.

use crate::confirm_question::Answer;
use crate::features::files::FilesIntent;
use crate::git::status::EntryKind;
use crate::intent::AppIntent;
use crate::keymap::{
    Binding, KeyPattern, KeyScope, NONE, OP_RUNNING, Reason, discard_disabled, list_guard, never,
    open_disabled,
};
use crate::menu::MenuId;
use crate::model::{AppState, Focus};
use crossterm::event::{KeyCode, KeyModifiers};

// Chunk 10: files
pub(crate) static FILES_NAV: &[Binding] = &[
    Binding {
        id: "files.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::Files,
        intent: AppIntent::Files(FilesIntent::Next),
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
        intent: AppIntent::Files(FilesIntent::Prev),
        description: "Select the previous file",
        label: "",
        bar: false,
        disabled: never,
    },
];

// Chunk 20: files
pub(crate) static FILES_STAGE: &[Binding] = &[
    Binding {
        id: "files.stage",
        keys: &[KeyPattern::exact(KeyCode::Char(' '), NONE)],
        scope: KeyScope::Files,
        intent: AppIntent::Files(FilesIntent::ToggleStage),
        description: "Stage the selected file, or unstage it if fully staged",
        label: "stage",
        bar: true,
        disabled: files_stage_disabled,
    },
    Binding {
        id: "files.stage_all",
        keys: &[KeyPattern::exact(KeyCode::Char('a'), NONE)],
        scope: KeyScope::Files,
        intent: AppIntent::Files(FilesIntent::ToggleStageAll),
        description: "Stage everything, or unstage everything if all is staged",
        label: "all",
        bar: true,
        disabled: stage_all_disabled,
    },
];

// Chunk 22: files
pub(crate) static FILES_DISCARD_OPEN: &[Binding] = &[
    Binding {
        id: "files.discard",
        keys: &[KeyPattern::exact(KeyCode::Char('d'), NONE)],
        scope: KeyScope::Files,
        intent: AppIntent::Discard,
        description: "Discard the selected file's unstaged changes, or delete it if untracked",
        label: "discard",
        bar: true,
        disabled: files_discard_disabled,
    },
    Binding {
        id: "files.open",
        keys: &[KeyPattern::exact(KeyCode::Char('o'), NONE)],
        scope: KeyScope::Files,
        intent: AppIntent::OpenInEditor,
        description: "Open the selected file in your editor",
        label: "open in editor",
        // Out of the bar, which is full at 80 columns; the help menu lists it.
        bar: false,
        disabled: files_open_disabled,
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
        intent: AppIntent::Files(FilesIntent::OpenIgnore),
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
    if state.files.rows.is_empty() {
        return Some("No files".into());
    }
    match state.files.selected_row() {
        Some(file) if file.kind == EntryKind::Untracked => None,
        _ => Some("Only untracked files can be ignored".into()),
    }
}

fn files_ignore_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| ignore_disabled(state))
}

// Chunk 44: files
pub(crate) static FILES_DISCARD_CONFIRM: &[Binding] = &[
    // Only `y` throws work away. Enter is left unbound on purpose: it
    // answers yes elsewhere, and a discard cannot be undone.
    Binding {
        id: "discard_confirm.yes",
        keys: Answer::YOnly.keys(),
        scope: KeyScope::DiscardConfirm,
        intent: AppIntent::ConfirmYes,
        description: "Yes, throw it away",
        label: "discard",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "discard_confirm.no",
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::DiscardConfirm,
        intent: AppIntent::ConfirmNo,
        description: "No, keep it",
        label: "keep",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "discard_confirm.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::DiscardConfirm,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
];

fn stage_disabled(state: &AppState) -> Option<Reason> {
    if state.op_is_running() {
        return Some(OP_RUNNING.into());
    }
    match state.files.selected_row() {
        None => Some("No file selected".into()),
        Some(file) if file.kind == EntryKind::Conflicted => {
            Some("Resolve the conflict before staging this file".into())
        }
        Some(_) => None,
    }
}

pub(crate) fn stage_all_disabled(state: &AppState) -> Option<Reason> {
    if state.op_is_running() {
        Some(OP_RUNNING.into())
    } else if state.files.rows.is_empty() {
        Some("No changes to stage".into())
    } else if state
        .files
        .rows
        .iter()
        .any(|f| f.kind == EntryKind::Conflicted)
    {
        Some(super::git::CONFLICTS_REFUSAL.into())
    } else {
        None
    }
}

/// Space stages a range too, so long as one of its files can be staged.
pub(crate) fn files_stage_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, true).or_else(|| {
        // As the action: the range counts only where it shows.
        if state.files.range_anchor.is_none() || state.focus != Focus::Files {
            return stage_disabled(state);
        }
        let range = state.files.range();
        if state.op_is_running() {
            Some(OP_RUNNING.into())
        } else if range
            .iter()
            .all(|&i| state.files.rows[i].kind == EntryKind::Conflicted)
        {
            Some("Resolve the conflicts before staging these files".into())
        } else {
            None
        }
    })
}

fn files_discard_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, true).or_else(|| discard_disabled(state))
}

fn files_open_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| open_disabled(state))
}

#[cfg(test)]
mod tests {
    use super::files_stage_disabled;
    use crate::intent::AppIntent;
    use crate::layout::SidePanel;
    use crate::test_support::*;

    #[test]
    fn stage_is_judged_on_the_one_file_outside_the_files_panel() {
        let (mut state, _) = with_files(vec![conflicted("c.rs"), changed("a.rs", '.', 'M')], 24);
        press_char(&mut state, 'V');
        assert_eq!(
            files_stage_disabled(&state),
            Some("Resolve the conflicts before staging these files".into())
        );
        state.update(AppIntent::FocusPanel(crate::layout::Panel::Side(
            SidePanel::Branches,
        )));
        assert!(
            state.files.range_anchor.is_some(),
            "the range stays for later"
        );
        assert_eq!(
            files_stage_disabled(&state),
            Some("Resolve the conflict before staging this file".into())
        );
    }
}
