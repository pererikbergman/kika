//! The diff's key bindings, with their guards. Chunk 5: `+`/`=` and `-`
//! change the context size and `w` hides or shows whitespace-only changes,
//! from Files, Diff and Lines. Chunks 29, 32 and 34: Enter focuses the
//! hunks, and the Diff and Lines keys move, stage, flip the side, discard,
//! open and edit. `CHUNKS` in `keymap.rs` places each chunk, so its
//! position there is behaviour.

use crate::git::diff::{DiffSide, MAX_CONTEXT, MIN_CONTEXT};
use crate::git::patch::{FileKind, PatchError};
use crate::git::status::EntryKind;
use crate::hunk_view::NO_CHANGES;
use crate::intent::AppIntent;
use crate::keymap::{
    Binding, KeyPattern, KeyScope, NONE, OP_RUNNING, Reason, blame_open, discard_disabled,
    display_only, in_commit, list_guard, never, no_file_selected, open_disabled, whitespace_hidden,
};
use crate::model::{AppState, Focus};
use crossterm::event::{KeyCode, KeyModifiers};

/// The diff settings reach the conflict view the same way, and the diff
/// is just as hidden.
fn diff_hidden(state: &AppState) -> Option<Reason> {
    blame_open(state).or_else(|| {
        matches!(state.focus, Focus::Conflict(_)).then_some("Close the conflict view first".into())
    })
}

fn most_context(state: &AppState) -> Option<Reason> {
    diff_hidden(state).or_else(|| {
        (state.diff_options.context >= MAX_CONTEXT).then_some("Already at the most context".into())
    })
}

fn least_context(state: &AppState) -> Option<Reason> {
    diff_hidden(state).or_else(|| {
        (state.diff_options.context <= MIN_CONTEXT).then_some("Already at the least context".into())
    })
}

// Chunk 5: diff
pub(crate) static DIFF_WORKSPACE: &[Binding] = &[
    // Diff settings, in Workspace so they work from Files, Diff and Lines.
    // Out of the bar, which is full at 80 columns; the help menu lists them.
    Binding {
        id: "workspace.more_context",
        keys: &[
            KeyPattern::any(KeyCode::Char('+')),
            KeyPattern::exact(KeyCode::Char('='), NONE),
        ],
        scope: KeyScope::Workspace,
        intent: AppIntent::MoreContext,
        description: "Show more context lines in the diff",
        label: "more context",
        bar: false,
        disabled: most_context,
    },
    Binding {
        id: "workspace.less_context",
        keys: &[KeyPattern::exact(KeyCode::Char('-'), NONE)],
        scope: KeyScope::Workspace,
        intent: AppIntent::LessContext,
        description: "Show fewer context lines in the diff",
        label: "less context",
        bar: false,
        disabled: least_context,
    },
    Binding {
        id: "workspace.whitespace",
        keys: &[KeyPattern::exact(KeyCode::Char('w'), NONE)],
        scope: KeyScope::Workspace,
        intent: AppIntent::ToggleWhitespace,
        description: "Hide or show whitespace-only changes",
        label: "whitespace",
        bar: false,
        disabled: diff_hidden,
    },
];

/// Why the selected hunk cannot be staged or unstaged right now.
fn hunk_disabled(state: &AppState) -> Option<Reason> {
    if state.op_is_running() {
        return Some(OP_RUNNING.into());
    }
    let Some(view) = state.hunk_view() else {
        return Some(NO_CHANGES.into());
    };
    if let Some(reason) = display_only(state).or_else(|| whitespace_hidden(state)) {
        return Some(reason);
    }
    if view.loading() {
        return Some("Loading…".into());
    }
    // A commit's file is not the selected working-tree one.
    let entry = state
        .selected
        .and_then(|i| state.files.get(i))
        .filter(|_| view.commit.is_none());
    match entry.map(|e| e.kind) {
        Some(EntryKind::Conflicted) => return Some("Resolve the conflict first".into()),
        Some(EntryKind::Renamed | EntryKind::Copied) => {
            return Some(PatchError::Renamed.message().into());
        }
        _ => {}
    }
    match &view.patch {
        Some(patch) if patch.kind == FileKind::Binary => Some(PatchError::Binary.message().into()),
        Some(patch) if !patch.hunks.is_empty() => None,
        _ => Some(NO_CHANGES.into()),
    }
}

/// An untracked file has only one side to show.
fn side_disabled(state: &AppState) -> Option<Reason> {
    if let Some(reason) = in_commit(state) {
        return Some(reason);
    }
    match state.hunk_view() {
        Some(view) if view.side == DiffSide::Untracked => {
            Some("Untracked file: nothing is staged".into())
        }
        _ => None,
    }
}

fn files_hunks_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| no_file_selected(state))
}

// Chunk 29: diff
#[rustfmt::skip]
pub(crate) static DIFF_FILES: &[Binding] = &[
    Binding {
        id: "files.hunks",
        keys: &[KeyPattern::any(KeyCode::Enter)],
        scope: KeyScope::Files,
        intent: AppIntent::FocusDiff,
        description: "Work on the hunks of the selected file, or resolve its conflicts",
        label: "hunks",
        // Out of the bar, which is full at 80 columns; the help menu lists it.
        bar: false,
        disabled: files_hunks_disabled,
    },
];

/// Why `e` cannot edit the selected hunk right now. What kind of change
/// can be edited is the model's call once it runs.
fn edit_hunk_disabled(state: &AppState) -> Option<Reason> {
    if let Some(reason) = in_commit(state) {
        return Some(reason);
    }
    if state.op_is_running() {
        return Some(OP_RUNNING.into());
    }
    let Some(view) = state.hunk_view() else {
        return Some(NO_CHANGES.into());
    };
    if let Some(reason) = display_only(state).or_else(|| whitespace_hidden(state)) {
        return Some(reason);
    }
    if view.lines.is_some() {
        return Some("Press Esc to edit the whole hunk".into());
    }
    if view.loading() {
        return Some("Loading…".into());
    }
    match &view.patch {
        Some(patch) if !patch.hunks.is_empty() => None,
        _ => Some(NO_CHANGES.into()),
    }
}

// Chunk 32: diff
pub(crate) static DIFF_KEYS: &[Binding] = &[
    Binding {
        id: "diff.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::Diff,
        intent: AppIntent::NextHunk,
        description: "Select the next hunk",
        label: "",
        bar: false,
        // The tool's rows show no hunk, so moving the selection is unseen.
        disabled: display_only,
    },
    Binding {
        id: "diff.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::Diff,
        intent: AppIntent::PrevHunk,
        description: "Select the previous hunk",
        label: "",
        bar: false,
        disabled: display_only,
    },
    Binding {
        id: "diff.stage",
        keys: &[KeyPattern::exact(KeyCode::Char(' '), NONE)],
        scope: KeyScope::Diff,
        intent: AppIntent::ToggleHunkStage,
        description: "Stage the selected hunk, or unstage it in the staged view; in a commit's change, pick it for the custom patch or drop it",
        label: "hunk",
        bar: true,
        disabled: hunk_disabled,
    },
    Binding {
        id: "diff.lines",
        keys: &[KeyPattern::exact(KeyCode::Char('v'), NONE)],
        scope: KeyScope::Diff,
        intent: AppIntent::EnterLineMode,
        description: "Pick changed lines inside the selected hunk",
        label: "lines",
        bar: true,
        disabled: hunk_disabled,
    },
    Binding {
        id: "diff.side",
        keys: &[KeyPattern::exact(KeyCode::Char('t'), NONE)],
        scope: KeyScope::Diff,
        intent: AppIntent::ToggleDiffSide,
        description: "Switch between the unstaged and the staged changes",
        label: "staged/unstaged",
        bar: true,
        disabled: side_disabled,
    },
    Binding {
        id: "diff.discard",
        keys: &[KeyPattern::exact(KeyCode::Char('d'), NONE)],
        scope: KeyScope::Diff,
        intent: AppIntent::Discard,
        description: "Discard the selected hunk from the working tree",
        label: "discard",
        bar: true,
        disabled: discard_disabled,
    },
    Binding {
        id: "diff.open",
        keys: &[KeyPattern::exact(KeyCode::Char('o'), NONE)],
        scope: KeyScope::Diff,
        intent: AppIntent::OpenInEditor,
        description: "Open the file in your editor at the selected hunk",
        label: "open in editor",
        // Out of the bar, which is full at 80 columns; the help menu lists it.
        bar: false,
        disabled: open_disabled,
    },
    Binding {
        id: "diff.edit_hunk",
        keys: &[KeyPattern::exact(KeyCode::Char('e'), NONE)],
        scope: KeyScope::Diff,
        intent: AppIntent::EditHunk,
        description: "Edit the selected hunk in your editor, then stage it",
        label: "edit hunk",
        // Out of the bar, which is full at 80 columns; the help menu lists it.
        bar: false,
        disabled: edit_hunk_disabled,
    },
];

// Chunk 34: diff
pub(crate) static DIFF_LINES: &[Binding] = &[
    Binding {
        id: "diff.back",
        // Esc is `diff.escape`, which clears a search first and stands in
        // for this one in the bar.
        keys: &[
            KeyPattern::exact(KeyCode::Char('h'), NONE),
            KeyPattern::any(KeyCode::Left),
        ],
        scope: KeyScope::Diff,
        intent: AppIntent::FocusLeft,
        description: "Back to the side panel last focused",
        label: "back",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "lines.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::exact(KeyCode::Down, NONE),
        ],
        scope: KeyScope::Lines,
        intent: AppIntent::NextLine { extend: false },
        description: "Move to the next changed line",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "lines.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::exact(KeyCode::Up, NONE),
        ],
        scope: KeyScope::Lines,
        intent: AppIntent::PrevLine { extend: false },
        description: "Move to the previous changed line",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "lines.extend_down",
        keys: &[
            KeyPattern::any(KeyCode::Char('J')),
            KeyPattern::exact(KeyCode::Down, KeyModifiers::SHIFT),
        ],
        scope: KeyScope::Lines,
        intent: AppIntent::NextLine { extend: true },
        description: "Grow the selection down a changed line",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "lines.extend_up",
        keys: &[
            KeyPattern::any(KeyCode::Char('K')),
            KeyPattern::exact(KeyCode::Up, KeyModifiers::SHIFT),
        ],
        scope: KeyScope::Lines,
        intent: AppIntent::PrevLine { extend: true },
        description: "Grow the selection up a changed line",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "lines.stage",
        keys: &[KeyPattern::exact(KeyCode::Char(' '), NONE)],
        scope: KeyScope::Lines,
        intent: AppIntent::ToggleLinesStage,
        description: "Stage the selected lines, or unstage them in the staged view; in a commit's change, pick them for the custom patch or drop them",
        label: "lines",
        bar: true,
        disabled: hunk_disabled,
    },
    Binding {
        id: "lines.back",
        keys: &[
            KeyPattern::any(KeyCode::Esc),
            KeyPattern::exact(KeyCode::Char('v'), NONE),
        ],
        scope: KeyScope::Lines,
        intent: AppIntent::ExitLineMode,
        description: "Back to whole hunks",
        label: "hunks",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "lines.discard",
        keys: &[KeyPattern::exact(KeyCode::Char('d'), NONE)],
        scope: KeyScope::Lines,
        intent: AppIntent::Discard,
        description: "Discard the selected lines from the working tree",
        label: "discard",
        bar: true,
        disabled: discard_disabled,
    },
    Binding {
        id: "lines.open",
        keys: &[KeyPattern::exact(KeyCode::Char('o'), NONE)],
        scope: KeyScope::Lines,
        intent: AppIntent::OpenInEditor,
        description: "Open the file in your editor at the selected line",
        label: "open in editor",
        // Out of the bar, which is full at 80 columns; the help menu lists it.
        bar: false,
        disabled: open_disabled,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent::Scroll;
    use crate::keymap::{DISPLAY_ONLY, Keymap, Resolution, WHITESPACE_HIDDEN, registry, resolve};
    use crate::model::Context;
    use crate::test_support::{from, key, plain};

    #[test]
    fn diff_option_keys_work_from_files_diff_and_lines() {
        use AppIntent::*;
        for scope in [KeyScope::Files, KeyScope::Diff, KeyScope::Lines] {
            assert_eq!(from(scope, plain('+')), Some(MoreContext), "{scope:?}");
            assert_eq!(
                from(scope, key(KeyCode::Char('+'), KeyModifiers::SHIFT)),
                Some(MoreContext),
                "{scope:?}"
            );
            assert_eq!(from(scope, plain('=')), Some(MoreContext), "{scope:?}");
            assert_eq!(from(scope, plain('-')), Some(LessContext), "{scope:?}");
            assert_eq!(from(scope, plain('w')), Some(ToggleWhitespace), "{scope:?}");
        }
        for k in [plain('+'), plain('='), plain('-'), plain('w')] {
            let binding = registry().iter().find(|b| b.matches(k)).expect("bound");
            assert_eq!(binding.scope, KeyScope::Workspace, "{k:?}");
            assert!(!binding.bar, "{k:?} stays out of the bar");
        }
    }

    #[test]
    fn context_keys_are_disabled_at_the_limits() {
        let reason = |state: &AppState, c| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::Files,
            state,
            plain(c),
        ) {
            Resolution::Disabled(reason) => Some(reason),
            Resolution::Run(_) => None,
            Resolution::Unbound | Resolution::Command(_) => panic!("{c} is unbound"),
        };
        let mut state = AppState::new(Context::Workspace);
        assert_eq!(reason(&state, '+'), None);
        assert_eq!(reason(&state, '-'), None);
        state.diff_options.context = MAX_CONTEXT;
        assert_eq!(
            reason(&state, '+'),
            Some("Already at the most context".into())
        );
        assert_eq!(
            reason(&state, '='),
            Some("Already at the most context".into())
        );
        assert_eq!(reason(&state, '-'), None);
        state.diff_options.context = MIN_CONTEXT;
        assert_eq!(
            reason(&state, '-'),
            Some("Already at the least context".into())
        );
        assert_eq!(reason(&state, '+'), None);
    }

    #[test]
    fn the_diff_pane_has_its_own_keys_and_esc_goes_back() {
        use AppIntent::*;
        let none = KeyModifiers::NONE;
        assert_eq!(
            from(KeyScope::Files, key(KeyCode::Enter, none)),
            Some(FocusDiff)
        );
        let diff = |k| from(KeyScope::Diff, k);
        assert_eq!(diff(plain('j')), Some(NextHunk));
        assert_eq!(diff(key(KeyCode::Down, none)), Some(NextHunk));
        assert_eq!(diff(plain('k')), Some(PrevHunk));
        assert_eq!(diff(key(KeyCode::Up, none)), Some(PrevHunk));
        assert_eq!(diff(plain('t')), Some(ToggleDiffSide));
        for k in [plain('h'), key(KeyCode::Left, none)] {
            assert_eq!(diff(k), Some(FocusLeft), "{k:?}");
        }
        // Esc clears a search first, and goes back with none.
        assert_eq!(diff(key(KeyCode::Esc, none)), Some(DiffEscape));
        // Workspace and global keys still reach through.
        assert_eq!(diff(plain('r')), Some(RefreshStatus));
        assert_eq!(diff(plain('J')), Some(ScrollDiff(Scroll::LineDown)));
        assert_eq!(diff(plain('q')), Some(Quit));
        // File keys do not.
        assert_eq!(diff(plain('a')), None);
        assert_eq!(diff(plain('c')), None);
    }

    #[test]
    fn line_mode_has_its_own_keys_and_falls_through_to_the_diff() {
        use AppIntent::*;
        let none = KeyModifiers::NONE;
        let shift = KeyModifiers::SHIFT;
        assert_eq!(KeyScope::Lines.parent(), Some(KeyScope::Diff));
        assert_eq!(KeyScope::Lines.title(), "Lines");
        let lines = |k| from(KeyScope::Lines, k);
        let next = NextLine { extend: false };
        let prev = PrevLine { extend: false };
        assert_eq!(lines(plain('j')), Some(next.clone()));
        assert_eq!(lines(key(KeyCode::Down, none)), Some(next));
        assert_eq!(lines(plain('k')), Some(prev.clone()));
        assert_eq!(lines(key(KeyCode::Up, none)), Some(prev));
        for mods in [none, shift] {
            assert_eq!(
                lines(key(KeyCode::Char('J'), mods)),
                Some(NextLine { extend: true })
            );
            assert_eq!(
                lines(key(KeyCode::Char('K'), mods)),
                Some(PrevLine { extend: true })
            );
        }
        assert_eq!(
            lines(key(KeyCode::Down, shift)),
            Some(NextLine { extend: true })
        );
        assert_eq!(
            lines(key(KeyCode::Up, shift)),
            Some(PrevLine { extend: true })
        );
        assert_eq!(lines(key(KeyCode::Esc, none)), Some(ExitLineMode));
        assert_eq!(lines(plain('v')), Some(ExitLineMode));
        // Other Diff keys, and workspace and global ones, reach through.
        assert_eq!(lines(plain('t')), Some(ToggleDiffSide));
        assert_eq!(lines(plain('h')), Some(FocusLeft));
        assert_eq!(lines(plain('r')), Some(RefreshStatus));
        assert_eq!(lines(plain('q')), Some(Quit));

        // Space and `v` share the hunk key's refusals; with no hunks here,
        // that is the same reason.
        let state = AppState::new(Context::Workspace);
        for (scope, k) in [(KeyScope::Lines, plain(' ')), (KeyScope::Diff, plain('v'))] {
            assert!(matches!(
                resolve(registry(), &Keymap::default(), scope, &state, k),
                Resolution::Disabled(ref reason) if reason == NO_CHANGES
            ));
        }
        let bound = |scope, k| {
            registry()
                .iter()
                .find(|b| b.scope == scope && b.matches(k))
                .map(|b| b.intent.clone())
        };
        assert_eq!(bound(KeyScope::Lines, plain(' ')), Some(ToggleLinesStage));
        assert_eq!(bound(KeyScope::Diff, plain('v')), Some(EnterLineMode));
    }

    #[test]
    fn hidden_whitespace_disables_the_patch_keys_in_the_diff_only() {
        use crate::git::patch::parse;
        use crate::hunk_view::{HunkView, LineCursor};

        let mut state = AppState::new(Context::Workspace);
        state.files = vec![crate::git::FileEntry {
            path: "a.rs".into(),
            orig_path: None,
            index: '.',
            worktree: 'M',
            kind: EntryKind::Changed,
        }];
        state.selected = Some(0);
        state.diff_options.ignore_whitespace = true;
        let reason = |state: &AppState, scope, c| match resolve(
            registry(),
            &Keymap::default(),
            scope,
            state,
            plain(c),
        ) {
            Resolution::Disabled(reason) => Some(reason),
            Resolution::Run(_) => None,
            Resolution::Unbound | Resolution::Command(_) => panic!("{c} is unbound in {scope:?}"),
        };
        // Whole-file keys are unaffected.
        assert_eq!(reason(&state, KeyScope::Files, ' '), None);
        assert_eq!(reason(&state, KeyScope::Files, 'd'), None);

        let mut view = HunkView::new("a.rs".into(), DiffSide::Worktree);
        view.patch =
            Some(parse(b"diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-a\n+b\n").unwrap());
        state.focus_hunk_view(view.clone());
        for c in [' ', 'v', 'd', 'e'] {
            assert_eq!(
                reason(&state, KeyScope::Diff, c),
                Some(WHITESPACE_HIDDEN.into()),
                "{c:?}"
            );
        }
        view.lines = Some(LineCursor {
            anchor: 0,
            cursor: 0,
        });
        state.focus_hunk_view(view);
        for c in [' ', 'd'] {
            assert_eq!(
                reason(&state, KeyScope::Lines, c),
                Some(WHITESPACE_HIDDEN.into()),
                "{c:?}"
            );
        }
        state.diff_options.ignore_whitespace = false;
        assert_eq!(reason(&state, KeyScope::Lines, ' '), None);
    }

    #[test]
    fn a_diff_tool_view_disables_the_patch_keys_in_the_diff_only() {
        use crate::features::diff::renderers::{ExternalRenderer, RendererKind};
        use crate::git::patch::parse;
        use crate::hunk_view::{HunkView, LineCursor};

        let mut state = AppState::new(Context::Workspace);
        state.files = vec![crate::git::FileEntry {
            path: "a.rs".into(),
            orig_path: None,
            index: '.',
            worktree: 'M',
            kind: EntryKind::Changed,
        }];
        state.selected = Some(0);
        state.renderers = vec![ExternalRenderer {
            label: "difft".into(),
            argv: vec!["difft".into()],
            kind: RendererKind::Tool,
        }];
        state.renderer = Some(0);
        let reason = |state: &AppState, scope, c| match resolve(
            registry(),
            &Keymap::default(),
            scope,
            state,
            plain(c),
        ) {
            Resolution::Disabled(reason) => Some(reason),
            Resolution::Run(_) => None,
            Resolution::Unbound | Resolution::Command(_) => panic!("{c} is unbound in {scope:?}"),
        };
        // Whole-file keys are unaffected.
        assert_eq!(reason(&state, KeyScope::Files, ' '), None);
        assert_eq!(reason(&state, KeyScope::Files, 'd'), None);

        let mut view = HunkView::new("a.rs".into(), DiffSide::Worktree);
        view.patch =
            Some(parse(b"diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-a\n+b\n").unwrap());
        state.focus_hunk_view(view.clone());
        for c in [' ', 'v', 'd', 'e'] {
            assert_eq!(
                reason(&state, KeyScope::Diff, c),
                Some(DISPLAY_ONLY.into()),
                "{c:?}"
            );
        }
        view.lines = Some(LineCursor {
            anchor: 0,
            cursor: 0,
        });
        state.focus_hunk_view(view);
        for c in [' ', 'd'] {
            assert_eq!(
                reason(&state, KeyScope::Lines, c),
                Some(DISPLAY_ONLY.into()),
                "{c:?}"
            );
        }
        state.renderer = None;
        assert_eq!(reason(&state, KeyScope::Lines, ' '), None);
    }

    #[test]
    fn a_diff_tool_view_disables_the_hunk_keys_with_its_notice() {
        use crate::features::diff::renderers::{ExternalRenderer, RendererKind};
        use crate::hunk_view::HunkView;

        let mut state = AppState::new(Context::Workspace);
        state.focus_hunk_view(HunkView::new("a.rs".into(), DiffSide::Worktree));
        state.renderers = vec![ExternalRenderer {
            label: "difft".into(),
            argv: vec!["difft".into()],
            kind: RendererKind::Tool,
        }];
        let keys = [
            (plain('j'), AppIntent::NextHunk),
            (key(KeyCode::Down, KeyModifiers::NONE), AppIntent::NextHunk),
            (plain('k'), AppIntent::PrevHunk),
            (key(KeyCode::Up, KeyModifiers::NONE), AppIntent::PrevHunk),
        ];
        let resolved = |state: &AppState, key| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::Diff,
            state,
            key,
        ) {
            Resolution::Run(binding) => Ok(binding.intent.clone()),
            Resolution::Disabled(reason) => Err(reason),
            other => panic!("{key:?} resolved to {other:?}"),
        };

        state.renderer = Some(0);
        for (key, _) in &keys {
            assert_eq!(resolved(&state, *key), Err(DISPLAY_ONLY.into()), "{key:?}");
        }
        // A pager's rows map to the patch, and the built-in view is the patch.
        state.renderers[0].kind = RendererKind::Pager;
        for (key, intent) in &keys {
            assert_eq!(resolved(&state, *key), Ok(intent.clone()), "{key:?}");
        }
        state.renderer = None;
        for (key, intent) in &keys {
            assert_eq!(resolved(&state, *key), Ok(intent.clone()), "{key:?}");
        }
    }

    #[test]
    fn o_and_e_say_why_they_cannot_run() {
        use crate::git::diff::DiffSide;
        use crate::git::patch::parse;
        use crate::hunk_view::{HunkView, LineCursor};

        let reason = |state: &AppState, scope, c| match resolve(
            registry(),
            &Keymap::default(),
            scope,
            state,
            plain(c),
        ) {
            Resolution::Disabled(reason) => Some(reason),
            Resolution::Run(_) => None,
            Resolution::Unbound | Resolution::Command(_) => panic!("{c} is unbound in {scope:?}"),
        };
        let mut state = AppState::new(Context::Workspace);
        assert_eq!(
            reason(&state, KeyScope::Files, 'o'),
            Some("No file selected".into())
        );
        state.hold_op(1);
        assert_eq!(
            reason(&state, KeyScope::Files, 'o'),
            Some(OP_RUNNING.into())
        );
        assert_eq!(reason(&state, KeyScope::Diff, 'e'), Some(OP_RUNNING.into()));
        state.free_op();

        let mut view = HunkView::new("a.rs".into(), DiffSide::Worktree);
        view.seq = Some(3);
        state.focus_hunk_view(view.clone());
        assert_eq!(reason(&state, KeyScope::Diff, 'e'), Some("Loading…".into()));
        view.seq = None;
        state.focus_hunk_view(view.clone());
        assert_eq!(reason(&state, KeyScope::Diff, 'e'), Some(NO_CHANGES.into()));
        view.patch =
            Some(parse(b"diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-a\n+b\n").unwrap());
        state.focus_hunk_view(view.clone());
        assert_eq!(reason(&state, KeyScope::Diff, 'e'), None);
        view.lines = Some(LineCursor {
            anchor: 0,
            cursor: 0,
        });
        state.focus_hunk_view(view);
        assert_eq!(
            reason(&state, KeyScope::Lines, 'e'),
            Some("Press Esc to edit the whole hunk".into())
        );
    }
}
