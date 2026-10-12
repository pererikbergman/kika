//! The diff options' key bindings: `+`/`=` and `-` change the context
//! size and `w` hides or shows whitespace-only changes, from Files, Diff
//! and Lines, with their guards. `CHUNKS` in `keymap.rs` places the chunk,
//! so its position there is behaviour.

use crate::git::diff::{MAX_CONTEXT, MIN_CONTEXT};
use crate::intent::AppIntent;
use crate::keymap::{Binding, KeyPattern, KeyScope, NONE, Reason, blame_open};
use crate::model::{AppState, Focus};
use crossterm::event::KeyCode;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{Keymap, Resolution, registry, resolve};
    use crate::model::Context;
    use crate::test_support::{from, key, plain};
    use crossterm::event::KeyModifiers;

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
}
