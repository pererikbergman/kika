//! The Commits panel's key bindings: switching to the Reflog tab, the
//! log filter's path and author keys, and the Files panel's log of the
//! selected file. `CHUNKS` in `keymap.rs` places each chunk, so their
//! position there is behaviour. Also the reasons a history rewrite waits
//! for the branch tips, which the rewrites and [`super::with_stacked_branches`]
//! give.

use crossterm::event::KeyCode;

use crate::intent::AppIntent;
use crate::keymap::{
    Binding, CLEAR_RANGE_FIRST, KeyPattern, KeyScope, NONE, Reason, list_guard, never,
    no_file_selected,
};
use crate::list_filter::LogFilterField;
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
}
