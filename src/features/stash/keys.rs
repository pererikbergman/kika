//! The stash's key bindings and guards: the Stash panel's rows, its
//! files, the Files panel's `s` that opens the Stash menu, and the drop
//! question; plus the guards of the Stash menu's items, which `menu.rs`
//! imports. `CHUNKS` in `keymap.rs` places the chunks, so their position
//! there is behaviour.

use crate::git::FileEntry;
use crate::git::path::RepoPath;
use crate::git::status::EntryKind;
use crate::intent::AppIntent;
use crate::keymap::{Binding, KeyPattern, KeyScope, NONE, OP_RUNNING, Reason, list_guard, never};
use crate::menu::MenuId;
use crate::model::AppState;
use crate::op::{GitOp, leading_dirs};
use crossterm::event::{KeyCode, KeyModifiers};

/// Nothing in the Stash panel can be opened or acted on before it lists
/// an entry.
fn no_stash(state: &AppState) -> Option<Reason> {
    state
        .stash
        .entries
        .is_empty()
        .then_some("No stashes".into())
}

/// Why the selected stash cannot be applied, popped, dropped or renamed
/// now: the filter hides it, a range is selected, git runs one operation
/// at a time, or the list has no entry. The model asks this again when the
/// key lands, so a hidden or ranged row is refused there too.
pub(crate) fn stash_action_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| stash_busy_or_empty(state))
}

/// Why `d` cannot drop the selected stash, or the range: as
/// [`stash_action_disabled`], but a range is what it acts on.
pub(crate) fn drop_stash_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, true).or_else(|| stash_busy_or_empty(state))
}

/// Why the selected stash's files cannot be opened: the filter hides it,
/// a range is selected, or the list has no entry.
pub(crate) fn stash_files_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| no_stash(state))
}

/// Git runs one operation at a time, and the list needs an entry.
fn stash_busy_or_empty(state: &AppState) -> Option<Reason> {
    if state.op_seq.is_some() {
        Some(OP_RUNNING.into())
    } else {
        no_stash(state)
    }
}

/// The Stash menu has nothing to offer on a clean tree.
fn stash_menu_disabled(state: &AppState) -> Option<Reason> {
    state
        .files
        .is_empty()
        .then_some("No changes to stash".into())
}

/// Why nothing can be stashed now: one operation at a time, and git
/// cannot stash a conflicted index.
fn stash_blocked(state: &AppState) -> Option<Reason> {
    if state.op_seq.is_some() {
        Some(OP_RUNNING.into())
    } else if state.files.iter().any(|f| f.kind == EntryKind::Conflicted) {
        Some("Resolve conflicts first".into())
    } else {
        None
    }
}

/// Untracked files stay behind, so they alone are nothing to stash.
pub(crate) fn stash_all_disabled(state: &AppState) -> Option<Reason> {
    stash_blocked(state).or_else(|| {
        (!state.files.iter().any(|f| f.kind != EntryKind::Untracked))
            .then_some("No changes to stash".into())
    })
}

pub(crate) fn stash_untracked_disabled(state: &AppState) -> Option<Reason> {
    stash_blocked(state).or_else(|| {
        state
            .files
            .is_empty()
            .then_some("No changes to stash".into())
    })
}

pub(crate) fn stash_staged_disabled(state: &AppState) -> Option<Reason> {
    stash_blocked(state).or_else(|| {
        (!state.files.iter().any(|f| !matches!(f.index, '.' | '?')))
            .then_some("Nothing staged".into())
    })
}

/// Why a set with a staged deletion and an untracked file is refused;
/// see `GitOp::stash_files_for`.
pub(crate) const STASH_APART: &str = "Stash deleted files apart from untracked";

/// Why a staged rename is refused when a file sits at its old path again.
pub(crate) use crate::op::STASH_RENAME_TAKEN;

/// Why a staged deletion is refused when a file sits at its path again.
pub(crate) use crate::op::STASH_DELETED_TAKEN;

/// Stashing selected files acts on the marked set: the Files range when
/// one is set, else the row under the cursor. A filter that hides the
/// cursor's row refuses it, as other row actions are refused. A staged
/// rename is refused when any listed entry sits at its old path or at a
/// directory that path needs: the restore after the push would overwrite
/// or unlink that file. A staged deletion is refused the same way when any
/// other entry sits at its path or a directory it needs: git checks
/// `HEAD`'s version out there after the push. Names are compared ignoring
/// ASCII case, since a case-insensitive filesystem folds them; on another
/// one this only refuses more.
pub(crate) fn stash_file_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, true)
        .or_else(|| stash_blocked(state))
        .or_else(|| {
            let (entries, _) = state.files_marked();
            let sets: Vec<_> = (entries.iter())
                .filter_map(|e| GitOp::stash_file_for(e).map(|set| (*e, set)))
                .collect();
            // Whether an entry other than `own` sits at `path` or a
            // directory it needs.
            let taken = |own: Option<&FileEntry>, path: &RepoPath| {
                let mut at = leading_dirs(path);
                at.push(path.clone());
                at.iter().any(|taken| {
                    (state.files.iter())
                        .filter(|f| !own.is_some_and(|own| std::ptr::eq(*f, own)))
                        .any(|f| f.path.as_bytes().eq_ignore_ascii_case(taken.as_bytes()))
                })
            };
            let renamed_taken = (sets.iter())
                .flat_map(|(_, set)| &set.restore)
                .any(|old| taken(None, old));
            let deleted_taken = (sets.iter())
                .flat_map(|(own, set)| set.unstage.iter().map(move |gone| (*own, gone)))
                .any(|(own, gone)| taken(Some(own), gone));
            if entries.is_empty() {
                Some("No file selected".into())
            } else if renamed_taken {
                Some(STASH_RENAME_TAKEN.into())
            } else if deleted_taken {
                Some(STASH_DELETED_TAKEN.into())
            } else if GitOp::stash_files_for(&entries).is_none() {
                Some(STASH_APART.into())
            } else {
                None
            }
        })
}

// Chunk 19: stash
pub(crate) static STASH_KEYS: &[Binding] = &[
    Binding {
        id: "stash.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::Stash,
        intent: AppIntent::StashNext,
        description: "Select the next stash",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "stash.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::Stash,
        intent: AppIntent::StashPrev,
        description: "Select the previous stash",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "stash.files",
        keys: &[KeyPattern::any(KeyCode::Enter)],
        scope: KeyScope::Stash,
        intent: AppIntent::OpenStashFiles,
        description: "Show the files the selected stash holds",
        label: "files",
        bar: true,
        disabled: stash_files_disabled,
    },
    Binding {
        id: "stash.apply",
        keys: &[KeyPattern::exact(KeyCode::Char(' '), NONE)],
        scope: KeyScope::Stash,
        intent: AppIntent::ApplyStash,
        description: "Apply the selected stash to the worktree and keep it",
        label: "apply",
        bar: true,
        disabled: stash_action_disabled,
    },
    Binding {
        id: "stash.pop",
        keys: &[KeyPattern::exact(KeyCode::Char('g'), NONE)],
        scope: KeyScope::Stash,
        intent: AppIntent::PopStash,
        description: "Apply the selected stash and drop it; a conflict keeps it",
        label: "pop",
        bar: true,
        disabled: stash_action_disabled,
    },
    Binding {
        id: "stash.drop",
        keys: &[KeyPattern::exact(KeyCode::Char('d'), NONE)],
        scope: KeyScope::Stash,
        intent: AppIntent::DropStash,
        description: "Drop the selected stash, or the range, asking first",
        label: "drop",
        bar: true,
        disabled: drop_stash_disabled,
    },
    Binding {
        id: "stash.rename",
        keys: &[KeyPattern::any(KeyCode::Char('R'))],
        scope: KeyScope::Stash,
        intent: AppIntent::RenameStash,
        description: "Give the selected stash a new message; it moves to stash@{0}",
        label: "rename",
        bar: true,
        disabled: stash_action_disabled,
    },
    Binding {
        id: "stash.filter",
        keys: &[KeyPattern::any(KeyCode::Char('/'))],
        scope: KeyScope::Stash,
        intent: AppIntent::OpenFilter,
        description: "Filter the stashes by typed text",
        label: "filter",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "stash.range",
        keys: &[KeyPattern::any(KeyCode::Char('V'))],
        scope: KeyScope::Stash,
        intent: AppIntent::ToggleRange,
        description: "Select a range from here, for drop; again to clear it",
        label: "range",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "stash.escape",
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::Stash,
        intent: AppIntent::ListEscape,
        description: "Clear the selection, else the filter, else quit Kika",
        label: "clear",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "stash_files.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::StashFiles,
        intent: AppIntent::StashFileNext,
        description: "Select the next file and show its change",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "stash_files.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::StashFiles,
        intent: AppIntent::StashFilePrev,
        description: "Select the previous file and show its change",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "stash_files.close",
        // Esc is `stash_files.escape`, which clears a search first and
        // stands in for this one in the bar.
        keys: &[KeyPattern::any(KeyCode::Backspace)],
        scope: KeyScope::StashFiles,
        intent: AppIntent::CloseStashFiles,
        description: "Back to the stash list, at the same stash",
        label: "back",
        bar: false,
        disabled: never,
    },
];

// Chunk 39: stash
#[rustfmt::skip]
pub(crate) static STASH_FROM_FILES: &[Binding] = &[
    Binding {
        id: "files.stash",
        keys: &[KeyPattern::exact(KeyCode::Char('s'), NONE)],
        scope: KeyScope::Files,
        intent: AppIntent::OpenMenu(MenuId::Stash),
        description: "Stash changes: all, with untracked files, staged only, or this file",
        label: "stash",
        // Out of the bar, which is full at 80 columns; the help menu lists it.
        bar: false,
        disabled: stash_menu_disabled,
    },
];

// Chunk 60: stash
pub(crate) static STASH_DROP_CONFIRM: &[Binding] = &[
    // As with a hard reset, Enter is left unbound: a dropped stash is gone
    // from the list.
    Binding {
        id: "drop_stash.yes",
        keys: &[KeyPattern::exact(KeyCode::Char('y'), NONE)],
        scope: KeyScope::DropStash,
        intent: AppIntent::ConfirmDropStash,
        description: "Yes, drop the stash",
        label: "drop",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "drop_stash.no",
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::DropStash,
        intent: AppIntent::ConfirmNo,
        description: "No, keep the stash",
        label: "keep",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "drop_stash.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::DropStash,
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
    use crate::intent::Scroll;
    use crate::keymap::{
        CLEAR_RANGE_FIRST, FILTER_HIDES_ROW, Keymap, REPO_LOCKED, Resolution, Trust, registry,
        resolve,
    };
    use crate::model::Context;
    use crate::test_support::{binding, build, from, key, plain};

    #[test]
    fn the_stash_panel_opens_applies_pops_drops_and_renames_the_selected_stash() {
        use AppIntent::*;
        assert_eq!(KeyScope::Stash.parent(), Some(KeyScope::Workspace));
        assert_eq!(KeyScope::Stash.title(), "Stash");
        let stash = |k| from(KeyScope::Stash, k);
        let none = KeyModifiers::NONE;
        assert_eq!(stash(key(KeyCode::Enter, none)), Some(OpenStashFiles));
        assert_eq!(stash(plain(' ')), Some(ApplyStash));
        assert_eq!(stash(plain('g')), Some(PopStash));
        assert_eq!(stash(plain('d')), Some(DropStash));
        assert_eq!(
            stash(key(KeyCode::Char('R'), KeyModifiers::SHIFT)),
            Some(RenameStash)
        );
        assert_eq!(stash(plain('j')), Some(StashNext));
        assert_eq!(stash(key(KeyCode::Down, none)), Some(StashNext));
        assert_eq!(stash(plain('k')), Some(StashPrev));
        assert_eq!(stash(key(KeyCode::Up, none)), Some(StashPrev));
        assert_eq!(stash(plain('q')), Some(Quit));

        assert_eq!(KeyScope::StashFiles.parent(), Some(KeyScope::Workspace));
        assert_eq!(KeyScope::StashFiles.title(), "Stash files");
        let files = |k| from(KeyScope::StashFiles, k);
        assert_eq!(files(key(KeyCode::Esc, none)), Some(PaneEscape));
        assert_eq!(files(key(KeyCode::Backspace, none)), Some(CloseStashFiles));
        assert_eq!(files(plain('j')), Some(StashFileNext));
        assert_eq!(files(key(KeyCode::Up, none)), Some(StashFilePrev));
        assert_eq!(files(plain('J')), Some(ScrollDiff(Scroll::LineDown)));

        assert_eq!(
            from(KeyScope::Files, plain('s')),
            Some(OpenMenu(MenuId::Stash))
        );
    }

    #[test]
    fn stash_actions_say_why_they_cannot_run() {
        let mut state = AppState::new(Context::Workspace);
        assert_eq!(stash_action_disabled(&state), Some("No stashes".into()));
        assert_eq!(no_stash(&state), Some("No stashes".into()));
        assert_eq!(
            stash_menu_disabled(&state),
            Some("No changes to stash".into())
        );
        state.stash.entries = vec![crate::features::stash::StashEntry {
            index: 0,
            oid: crate::git::commit_files::CommitId::parse("a0").unwrap(),
            base: crate::git::commit_files::CommitId::parse("b0").unwrap(),
            untracked: None,
            subject: String::new(),
        }];
        assert_eq!(stash_action_disabled(&state), None);
        state.op_seq = Some(1);
        assert_eq!(stash_action_disabled(&state), Some(OP_RUNNING.into()));
        assert_eq!(no_stash(&state), None);
    }

    /// The Stash panel focused, listing three stashes, the cursor on the
    /// first.
    fn stash_panel() -> AppState {
        let mut state = AppState::new(Context::Workspace);
        state.focus = crate::model::Focus::Stash;
        state.stash.begin(1);
        let entries = ["a0", "a1", "a2"]
            .iter()
            .enumerate()
            .map(|(index, oid)| crate::features::stash::StashEntry {
                index,
                oid: crate::git::commit_files::CommitId::parse(oid).unwrap(),
                base: crate::git::commit_files::CommitId::parse("b0").unwrap(),
                untracked: None,
                subject: format!("On main: {oid}"),
            })
            .collect();
        state.stash.loaded(1, Ok(entries));
        assert_eq!(state.panel_focus(), KeyScope::Stash);
        state
    }

    #[test]
    fn a_stash_range_leaves_only_d_free_of_the_row_actions() {
        let mut state = stash_panel();
        let reason = |state: &AppState, id: &str| (binding(id).disabled)(state);
        let actions = ["stash.files", "stash.apply", "stash.pop", "stash.rename"];
        for id in actions.iter().chain([&"stash.drop"]) {
            assert_eq!(reason(&state, id), None, "{id}");
        }
        state.stash.toggle_range();
        for id in actions {
            assert_eq!(
                reason(&state, id).as_deref(),
                Some(CLEAR_RANGE_FIRST),
                "{id}"
            );
        }
        assert_eq!(reason(&state, "stash.drop"), None, "d works on a range");
        for id in ["stash.next", "stash.prev", "stash.filter", "stash.range"] {
            assert_eq!(reason(&state, id), None, "{id}");
        }
        state.op_seq = Some(1);
        assert_eq!(reason(&state, "stash.drop").as_deref(), Some(OP_RUNNING));
    }

    #[test]
    fn a_stash_filter_that_hides_the_cursor_refuses_every_stash_action() {
        let mut state = stash_panel();
        state.stash.filter = "zzz".into();
        assert!(state.stash.selected_hidden());
        for id in [
            "stash.files",
            "stash.apply",
            "stash.pop",
            "stash.drop",
            "stash.rename",
        ] {
            assert_eq!(
                (binding(id).disabled)(&state).as_deref(),
                Some(FILTER_HIDES_ROW),
                "{id}"
            );
        }
        // The model asks the same, so a key that slipped through is refused.
        assert_eq!(
            stash_action_disabled(&state).as_deref(),
            Some(FILTER_HIDES_ROW)
        );
    }

    #[test]
    fn the_stash_filter_and_range_are_remappable_but_its_escape_is_locked() {
        // Esc with nothing to clear quits, as tags.escape does.
        assert!(REPO_LOCKED.contains(&"stash.escape"));
        assert!(build(&[("stash.escape", &["Z"])], Trust::Repo).is_err());
        for id in ["stash.filter", "stash.range"] {
            assert!(!REPO_LOCKED.contains(&id), "{id}");
            assert!(build(&[(id, &["Z"])], Trust::Repo).is_ok(), "{id}");
        }
        let none = KeyModifiers::NONE;
        let stash = |k| from(KeyScope::Stash, k);
        assert_eq!(stash(plain('/')), Some(AppIntent::OpenFilter));
        assert_eq!(
            stash(key(KeyCode::Char('V'), KeyModifiers::SHIFT)),
            Some(AppIntent::ToggleRange)
        );
        assert_eq!(stash(key(KeyCode::Esc, none)), Some(AppIntent::ListEscape));
    }

    #[test]
    fn the_drop_stash_question_has_no_enter_and_n_or_esc_keeps_it() {
        let state = AppState::new(Context::Workspace);
        let resolve_in = |key| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::DropStash,
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
        assert_eq!(resolve_in(plain('y')), Some(AppIntent::ConfirmDropStash));
        assert_eq!(resolve_in(plain('n')), Some(AppIntent::ConfirmNo));
        assert_eq!(
            resolve_in(key(KeyCode::Esc, none)),
            Some(AppIntent::ConfirmNo)
        );
        assert_eq!(
            resolve_in(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(AppIntent::Quit)
        );
        assert_eq!(KeyScope::DropStash.parent(), None);
        assert_eq!(KeyScope::DropStash.title(), "Drop stash");
    }

    #[test]
    fn the_stash_actions_and_answers_are_locked_but_moving_around_is_not() {
        for id in [
            "files.stash",
            "stash.files",
            "stash.apply",
            "stash.pop",
            "stash.drop",
            "stash.rename",
            "drop_stash.yes",
            "drop_stash.no",
            "drop_stash.quit",
        ] {
            assert!(REPO_LOCKED.contains(&id), "{id}");
            assert!(build(&[(id, &["F"])], Trust::Repo).is_err(), "{id}");
        }
        for id in [
            "stash.next",
            "stash.prev",
            "stash_files.next",
            "stash_files.prev",
            "stash_files.close",
        ] {
            assert!(!REPO_LOCKED.contains(&id), "{id}");
            assert!(build(&[(id, &["F"])], Trust::Repo).is_ok(), "{id}");
        }
    }
}
