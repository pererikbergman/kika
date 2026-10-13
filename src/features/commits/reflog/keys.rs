//! The reflog's key bindings: Ctrl+Z and Ctrl+R from every workspace
//! panel, the Reflog tab's rows and filter, and the undo or redo
//! question. `CHUNKS` in `keymap.rs` places the chunks, so their
//! position there is behaviour.

use crate::confirm_question::Answer;
use crate::features::commits::CommitsIntent;
use crate::features::commits::reflog::ReflogIntent;
use crate::intent::AppIntent;
use crate::keymap::{Binding, KeyPattern, KeyScope, NONE, OP_RUNNING, Reason, never};
use crate::list_filter::LogFilterField;
use crate::model::AppState;
use crossterm::event::{KeyCode, KeyModifiers};

/// Why undo and redo cannot start now: git one operation at a time, a
/// repository to act on, and no operation half done, a bisect included,
/// whose own state a moved `HEAD` would confuse.
pub(crate) fn undo_disabled(state: &AppState) -> Option<Reason> {
    if state.op_is_running() {
        Some(OP_RUNNING.into())
    } else if state.repo.is_none() {
        Some("Not in a repository".into())
    } else if state
        .branch
        .as_ref()
        .is_some_and(|b| !b.in_progress.is_empty())
    {
        Some("Finish or abort the operation in progress".into())
    } else {
        None
    }
}

/// The reflog is not a log git can narrow.
fn reflog_filter_disabled(_: &AppState) -> Option<Reason> {
    Some("Filters work on the log tab".into())
}

// Chunk 9: reflog
pub(crate) static REFLOG_WORKSPACE: &[Binding] = &[
    // Raw mode turns off the terminal's own meaning of Ctrl+Z (suspend)
    // and Ctrl+R (reprint), so both reach Kika as keys on every platform.
    // Redo is not Ctrl+Y: the custom-command example in the docs, and so
    // perhaps the user's own config, binds it in the blame view, where a
    // locked redo would make that config fail to load.
    Binding {
        id: "workspace.undo",
        keys: &[KeyPattern::exact(KeyCode::Char('z'), KeyModifiers::CONTROL)],
        scope: KeyScope::Workspace,
        intent: AppIntent::Commits(CommitsIntent::Reflog(ReflogIntent::Undo)),
        description: "Undo the last commit, checkout, reset or rebase, saying first what moves",
        label: "undo",
        bar: false,
        disabled: undo_disabled,
    },
    Binding {
        id: "workspace.redo",
        keys: &[KeyPattern::exact(KeyCode::Char('r'), KeyModifiers::CONTROL)],
        scope: KeyScope::Workspace,
        intent: AppIntent::Commits(CommitsIntent::Reflog(ReflogIntent::Redo)),
        description: "Redo what the last undo reversed, saying first what moves",
        label: "redo",
        bar: false,
        disabled: undo_disabled,
    },
];

// Chunk 15: reflog
pub(crate) static REFLOG_KEYS: &[Binding] = &[
    Binding {
        id: "reflog.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::Reflog,
        intent: AppIntent::Commits(CommitsIntent::Reflog(ReflogIntent::Next)),
        description: "Select the next reflog entry",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "reflog.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::Reflog,
        intent: AppIntent::Commits(CommitsIntent::Reflog(ReflogIntent::Prev)),
        description: "Select the previous reflog entry",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "reflog.switch_tab",
        keys: &[
            KeyPattern::exact(KeyCode::Char('['), NONE),
            KeyPattern::exact(KeyCode::Char(']'), NONE),
        ],
        scope: KeyScope::Reflog,
        intent: AppIntent::Commits(CommitsIntent::SwitchTab),
        description: "Show the log",
        label: "log",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "reflog.filter",
        keys: &[KeyPattern::any(KeyCode::Char('/'))],
        scope: KeyScope::Reflog,
        intent: AppIntent::OpenFilter,
        description: "Filter the reflog by typed text",
        label: "filter",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "reflog.escape",
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::Reflog,
        intent: AppIntent::ListEscape,
        description: "Clear the filter, else quit Kika",
        label: "clear",
        bar: false,
        disabled: never,
    },
];

// Chunk 65: reflog
pub(crate) static REFLOG_TRAVEL_CONFIRM: &[Binding] = &[
    // Enter is left unbound here too: an undo can move a branch away from
    // commits, so only a deliberate y answers.
    Binding {
        id: "travel.yes",
        keys: Answer::YOnly.keys(),
        scope: KeyScope::Travel,
        intent: AppIntent::Commits(CommitsIntent::Reflog(ReflogIntent::ConfirmTravel)),
        description: "Yes, do what the question says",
        label: "yes",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "travel.no",
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::Travel,
        intent: AppIntent::ConfirmNo,
        description: "No, leave everything where it is",
        label: "no",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "travel.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::Travel,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
];

// Chunk 75: reflog
pub(crate) static REFLOG_LOG_FILTER: &[Binding] = &[
    Binding {
        id: "reflog.filter_path",
        keys: &[KeyPattern::any(KeyCode::Char('L'))],
        scope: KeyScope::Reflog,
        intent: AppIntent::Commits(CommitsIntent::OpenLogFilter(LogFilterField::Path)),
        description: "Show only the commits that changed a typed path, across renames",
        label: "log of path",
        bar: false,
        disabled: reflog_filter_disabled,
    },
    Binding {
        id: "reflog.filter_author",
        keys: &[KeyPattern::any(KeyCode::Char('A'))],
        scope: KeyScope::Reflog,
        intent: AppIntent::Commits(CommitsIntent::OpenLogFilter(LogFilterField::Author)),
        description: "Show only the commits by a typed author",
        label: "log by author",
        bar: false,
        disabled: reflog_filter_disabled,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::repo_state::Operation;
    use crate::git::status::Head;
    use crate::keymap::{Keymap, REPO_LOCKED, Resolution, Trust, registry, resolve};
    use crate::model::Context;
    use crate::test_support::{build, from, key, plain};

    #[test]
    fn the_travel_question_has_no_enter_and_n_or_esc_leave_everything() {
        let state = AppState::new(Context::Workspace);
        let resolve_in = |key| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::Travel,
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
        assert_eq!(
            resolve_in(plain('y')),
            Some(AppIntent::Commits(CommitsIntent::Reflog(
                ReflogIntent::ConfirmTravel
            )))
        );
        assert_eq!(resolve_in(plain('n')), Some(AppIntent::ConfirmNo));
        assert_eq!(
            resolve_in(key(KeyCode::Esc, none)),
            Some(AppIntent::ConfirmNo)
        );
        assert_eq!(
            resolve_in(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(AppIntent::Quit)
        );
        assert_eq!(KeyScope::Travel.parent(), None);
        assert_eq!(KeyScope::Travel.title(), "Undo or redo");
    }

    #[test]
    fn brackets_switch_the_commits_panel_between_log_and_reflog() {
        use AppIntent::*;
        for c in ['[', ']'] {
            assert_eq!(
                from(KeyScope::Commits, plain(c)),
                Some(Commits(CommitsIntent::SwitchTab))
            );
            assert_eq!(
                from(KeyScope::Reflog, plain(c)),
                Some(Commits(CommitsIntent::SwitchTab))
            );
        }
        let none = KeyModifiers::NONE;
        assert_eq!(
            from(KeyScope::Reflog, plain('j')),
            Some(Commits(CommitsIntent::Reflog(ReflogIntent::Next)))
        );
        assert_eq!(
            from(KeyScope::Reflog, key(KeyCode::Down, none)),
            Some(Commits(CommitsIntent::Reflog(ReflogIntent::Next)))
        );
        assert_eq!(
            from(KeyScope::Reflog, plain('k')),
            Some(Commits(CommitsIntent::Reflog(ReflogIntent::Prev)))
        );
        assert_eq!(
            from(KeyScope::Reflog, key(KeyCode::Up, none)),
            Some(Commits(CommitsIntent::Reflog(ReflogIntent::Prev)))
        );
        // The log's actions do not reach the reflog's rows.
        for c in [' ', 's', 'd', 'e'] {
            assert_eq!(from(KeyScope::Reflog, plain(c)), None, "{c:?}");
        }
        assert_eq!(KeyScope::Reflog.parent(), Some(KeyScope::Workspace));
        assert_eq!(KeyScope::Reflog.title(), "Reflog");
    }

    #[test]
    fn undo_waits_for_a_repository_an_op_and_any_operation_in_progress() {
        let mut state = AppState::new(Context::Workspace);
        assert_eq!(undo_disabled(&state), Some("Not in a repository".into()));
        state.repo = Some("/repo".into());
        assert_eq!(undo_disabled(&state), None);
        state.branch = Some(crate::git::status::BranchStatus {
            head: Head::Branch("main".into()),
            upstream: None,
            ahead_behind: None,
            in_progress: vec![Operation::Bisect],
            bisect: None,
        });
        assert_eq!(
            undo_disabled(&state),
            Some("Finish or abort the operation in progress".into())
        );
        state.hold_op(1);
        assert_eq!(undo_disabled(&state), Some(OP_RUNNING.into()));
    }

    #[test]
    fn the_reflog_filter_is_remappable_but_its_escape_is_locked() {
        // Esc with no filter to clear quits, as tags.escape does.
        assert!(REPO_LOCKED.contains(&"reflog.escape"));
        assert!(build(&[("reflog.escape", &["Z"])], Trust::Repo).is_err());
        assert!(!REPO_LOCKED.contains(&"reflog.filter"));
        assert!(build(&[("reflog.filter", &["Z"])], Trust::Repo).is_ok());
        assert_eq!(
            from(KeyScope::Reflog, plain('/')),
            Some(AppIntent::OpenFilter)
        );
        assert_eq!(
            from(KeyScope::Reflog, key(KeyCode::Esc, KeyModifiers::NONE)),
            Some(AppIntent::ListEscape)
        );
        // No range: nothing on a reflog row would act on one.
        assert!(!registry().iter().any(|b| b.id == "reflog.range"));
        assert_ne!(
            from(KeyScope::Reflog, plain('V')),
            Some(AppIntent::ToggleRange)
        );
    }
}
