//! The Commits panel's key bindings: moving in the log, opening a
//! commit's files and moving in them, checkout, revert and reset with the
//! hard reset's question, switching to the Reflog tab, the log filter's
//! path and author keys, and the Files panel's log of the selected file.
//! `CHUNKS` in `keymap.rs` places each chunk, so their position there is
//! behaviour. Also the guards on the selected commit's actions, which
//! `rewrite` and core's menu share, and the reasons a history rewrite
//! waits for the branch tips, which the rewrites and
//! [`super::with_stacked_branches`] give.

use crossterm::event::{KeyCode, KeyModifiers};

use crate::intent::AppIntent;
use crate::keymap::{
    Binding, CLEAR_RANGE_FIRST, KeyPattern, KeyScope, NONE, OP_RUNNING, Reason,
    in_progress_disabled, list_guard, never, no_commit, no_file_selected,
};
use crate::list_filter::LogFilterField;
use crate::menu::MenuId;
use crate::model::AppState;

/// Why a rewrite waits: the branches are still being read, so whether
/// one sits on its commits is not known yet.
pub(crate) const TIPS_UNKNOWN: &str = "Reading the branches; try again in a moment";

/// Why a rewrite on an older git cannot go ahead: reading the branches
/// failed, so whether one would be left behind is not known.
pub(crate) fn tips_failed(error: &str) -> String {
    let first = error.lines().next().unwrap_or_default();
    format!("Could not read the branches ({first}); press r to retry")
}

/// A new path or author reloads the log, which a range was made over.
fn log_filter_disabled(state: &AppState) -> Option<Reason> {
    state
        .commits
        .range_anchor
        .is_some()
        .then_some(CLEAR_RANGE_FIRST.into())
}

fn files_log_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| no_file_selected(state))
}

/// Why no action can run on the selected commit now: git runs one
/// operation at a time, and the log needs a commit.
pub(crate) fn commit_action_disabled(state: &AppState) -> Option<Reason> {
    if state.op_seq.is_some() {
        Some(OP_RUNNING.into())
    } else {
        no_commit(state)
    }
}

pub(crate) fn checkout_commit_disabled(state: &AppState) -> Option<Reason> {
    commit_action_disabled(state).or_else(|| crate::bisect::head_move_disabled(state))
}

/// Why the selected commit cannot be reverted now. A merge commit needs a
/// mainline chosen, which Kika does not ask for.
pub(crate) fn revert_disabled(state: &AppState) -> Option<Reason> {
    commit_action_disabled(state)
        .or_else(|| in_progress_disabled(state))
        .or_else(|| {
            let row = state.commits.rows.get(state.commits.selected);
            row.is_some_and(|r| r.parents.len() > 1)
                .then_some("Cannot revert a merge commit".into())
        })
}

pub(crate) fn reset_disabled(state: &AppState) -> Option<Reason> {
    commit_action_disabled(state).or_else(|| in_progress_disabled(state))
}

fn commits_open_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| no_commit(state))
}

fn commits_checkout_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| checkout_commit_disabled(state))
}

fn commits_revert_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| revert_disabled(state))
}

fn commits_reset_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| reset_disabled(state))
}

// Chunk 11: commits
pub(crate) static COMMITS_KEYS: &[Binding] = &[
    Binding {
        id: "commits.next",
        // Not any modifiers on the arrow: Alt+Down moves the commit.
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::exact(KeyCode::Down, NONE),
        ],
        scope: KeyScope::Commits,
        intent: AppIntent::CommitNext,
        description: "Select the next commit",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "commits.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::exact(KeyCode::Up, NONE),
        ],
        scope: KeyScope::Commits,
        intent: AppIntent::CommitPrev,
        description: "Select the previous commit",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "commits.open",
        keys: &[KeyPattern::any(KeyCode::Enter)],
        scope: KeyScope::Commits,
        intent: AppIntent::OpenCommitFiles,
        description: "Show the files the selected commit changed, or those changed from the diff mark to it",
        label: "files",
        bar: true,
        disabled: commits_open_disabled,
    },
    Binding {
        id: "commits.checkout",
        keys: &[KeyPattern::exact(KeyCode::Char(' '), NONE)],
        scope: KeyScope::Commits,
        intent: AppIntent::CheckoutCommit,
        description: "Check out the selected commit as a detached HEAD, asking first",
        label: "checkout",
        bar: true,
        disabled: commits_checkout_disabled,
    },
    Binding {
        id: "commits.revert",
        keys: &[KeyPattern::any(KeyCode::Char('R'))],
        scope: KeyScope::Commits,
        intent: AppIntent::RevertCommit,
        description: "Make a commit that undoes the selected one",
        label: "revert",
        bar: true,
        disabled: commits_revert_disabled,
    },
    Binding {
        id: "commits.reset",
        keys: &[KeyPattern::any(KeyCode::Char('X'))],
        scope: KeyScope::Commits,
        intent: AppIntent::OpenMenu(MenuId::Reset),
        description: "Reset the current branch to the selected commit: soft, mixed or hard",
        label: "reset",
        bar: true,
        disabled: commits_reset_disabled,
    },
];

// Chunk 13: commits
#[rustfmt::skip]
pub(crate) static COMMITS_SWITCH_TAB: &[Binding] = &[
    Binding {
        id: "commits.switch_tab",
        keys: &[
            KeyPattern::exact(KeyCode::Char('['), NONE),
            KeyPattern::exact(KeyCode::Char(']'), NONE),
        ],
        scope: KeyScope::Commits,
        intent: AppIntent::SwitchCommitsTab,
        description: "Show HEAD's reflog: everywhere HEAD has been",
        label: "reflog",
        bar: true,
        disabled: never,
    },
];

// Chunk 16: commits
pub(crate) static COMMITS_FILES_NAV: &[Binding] = &[
    Binding {
        id: "commit_files.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::CommitFiles,
        intent: AppIntent::CommitFileNext,
        description: "Select the next file and show its change",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "commit_files.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::CommitFiles,
        intent: AppIntent::CommitFilePrev,
        description: "Select the previous file and show its change",
        label: "",
        bar: false,
        disabled: never,
    },
];

// Chunk 18: commits
#[rustfmt::skip]
pub(crate) static COMMITS_FILES_BACK: &[Binding] = &[
    Binding {
        id: "commit_files.back",
        // Esc is `commit_files.escape`, which clears a search first and stands in
        // for this one in the bar.
        keys: &[
            KeyPattern::exact(KeyCode::Char('h'), NONE),
            KeyPattern::any(KeyCode::Left),
        ],
        scope: KeyScope::CommitFiles,
        intent: AppIntent::CloseCommitFiles,
        description: "Back to the log, at the same commit",
        label: "back",
        bar: false,
        disabled: never,
    },
];

// Chunk 58: commits
pub(crate) static COMMITS_RESET_HARD_CONFIRM: &[Binding] = &[
    // As with deleting a branch, Enter is left unbound: a hard reset
    // cannot be undone.
    Binding {
        id: "reset_hard.yes",
        keys: &[KeyPattern::exact(KeyCode::Char('y'), NONE)],
        scope: KeyScope::ResetHard,
        intent: AppIntent::ConfirmResetHard,
        description: "Yes, reset and throw away every uncommitted change",
        label: "reset",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "reset_hard.no",
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::ResetHard,
        intent: AppIntent::ConfirmNo,
        description: "No, keep the changes",
        label: "keep",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "reset_hard.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::ResetHard,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
];

// Chunk 73: commits
pub(crate) static COMMITS_LOG_FILTER: &[Binding] = &[
    // The log query and the diff mark.
    Binding {
        id: "commits.filter_path",
        keys: &[KeyPattern::any(KeyCode::Char('L'))],
        scope: KeyScope::Commits,
        intent: AppIntent::OpenLogFilter(LogFilterField::Path),
        description: "Show only the commits that changed a typed path, across renames",
        label: "log of path",
        bar: false,
        disabled: log_filter_disabled,
    },
    Binding {
        id: "commits.filter_author",
        keys: &[KeyPattern::any(KeyCode::Char('A'))],
        scope: KeyScope::Commits,
        intent: AppIntent::OpenLogFilter(LogFilterField::Author),
        description: "Show only the commits by a typed author",
        label: "log by author",
        bar: false,
        disabled: log_filter_disabled,
    },
];

// Chunk 77: commits
#[rustfmt::skip]
pub(crate) static COMMITS_FILES_LOG: &[Binding] = &[
    Binding {
        id: "files.log",
        keys: &[KeyPattern::any(KeyCode::Char('L'))],
        scope: KeyScope::Files,
        intent: AppIntent::LogOfFile,
        description: "Show the selected file's history in the Commits panel",
        label: "log of file",
        bar: false,
        disabled: files_log_disabled,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::commits::compare::keys::commits_mark_disabled;
    use crate::git::repo_state::Operation;
    use crate::git::status::Head;
    use crate::model::{Context, Focus};

    #[test]
    fn a_range_keeps_the_new_commits_keys_from_running() {
        let mut state = AppState::new(Context::Workspace);
        state.focus = Focus::Commits;
        state.commits.rows = vec![crate::git::log::CommitRow {
            oid: "a".repeat(40),
            short_oid: "a".into(),
            parents: vec![],
            author: "Ann".into(),
            time: 0,
            subject: "s".into(),
            subject_altered: false,
        }];
        assert_eq!(log_filter_disabled(&state), None);
        assert_eq!(commits_mark_disabled(&state), None);
        state.commits.range_anchor = Some("a".repeat(40));
        assert_eq!(
            log_filter_disabled(&state).as_deref(),
            Some(CLEAR_RANGE_FIRST)
        );
        assert_eq!(
            commits_mark_disabled(&state).as_deref(),
            Some(CLEAR_RANGE_FIRST)
        );
    }

    #[test]
    fn commit_actions_say_why_they_cannot_run() {
        let row = |parents: &[&str]| crate::git::log::CommitRow {
            oid: "abc".into(),
            short_oid: "abc".into(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
            author: "Ann".into(),
            time: 0,
            subject: "s".into(),
            subject_altered: false,
        };
        let mut state = AppState::new(Context::Workspace);
        for disabled in [checkout_commit_disabled, revert_disabled, reset_disabled] {
            assert_eq!(disabled(&state), Some("No commits".into()));
        }
        state.commits.rows = vec![row(&["p"])];
        for disabled in [checkout_commit_disabled, revert_disabled, reset_disabled] {
            assert_eq!(disabled(&state), None);
        }
        state.commits.rows = vec![row(&["p", "q"])];
        assert_eq!(
            revert_disabled(&state),
            Some("Cannot revert a merge commit".into())
        );
        assert_eq!(reset_disabled(&state), None);
        assert_eq!(checkout_commit_disabled(&state), None);

        state.commits.rows = vec![row(&["p"])];
        state.branch = Some(crate::git::status::BranchStatus {
            head: Head::Branch("main".into()),
            upstream: None,
            ahead_behind: None,
            in_progress: vec![Operation::Revert],
            bisect: None,
        });
        let busy = Some("Finish or abort the operation in progress");
        assert_eq!(revert_disabled(&state).as_deref(), busy);
        assert_eq!(reset_disabled(&state).as_deref(), busy);
        // Git refuses a checkout that would lose work; it may go ahead.
        assert_eq!(checkout_commit_disabled(&state), None);

        state.op_seq = Some(1);
        for disabled in [checkout_commit_disabled, revert_disabled, reset_disabled] {
            assert_eq!(disabled(&state), Some(OP_RUNNING.into()));
        }
    }
}
