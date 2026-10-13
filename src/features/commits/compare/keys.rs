//! Compare's key bindings: `M` marks the selected log commit or local
//! branch, and Enter in Branches compares the mark with the selected
//! branch. `CHUNKS` in `keymap.rs` places both chunks, so their positions
//! there are behaviour.

use crate::diff_mark::DiffMark;
use crate::features::commits::CommitsIntent;
use crate::features::commits::compare::CompareIntent;
use crate::intent::AppIntent;
use crate::keymap::{
    Binding, KeyPattern, KeyScope, Reason, commits_range_disabled, list_guard, no_branch_selected,
    no_commit_selected,
};
use crate::model::AppState;
use crossterm::event::KeyCode;

/// A mark is a log commit, not a row of the todo list above the log.
pub(crate) fn commits_mark_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false)
        .or_else(|| commits_range_disabled(state))
        .or_else(|| no_commit_selected(state))
}

/// A comparison needs a mark, and another branch or commit than it.
pub(crate) fn branches_diff_disabled(state: &AppState) -> Option<Reason> {
    no_branch_selected(state).or_else(|| match &state.diff_mark {
        None => Some("Mark a commit or branch first (M)".into()),
        Some(DiffMark::Branch(name))
            if state
                .branches
                .selected_branch()
                .is_some_and(|b| b.name == *name) =>
        {
            Some("Pick another branch or commit to diff against".into())
        }
        Some(_) => None,
    })
}

// Chunk 74: compare
#[rustfmt::skip]
pub(crate) static COMPARE_COMMITS: &[Binding] = &[
    Binding {
        id: "commits.mark",
        keys: &[KeyPattern::any(KeyCode::Char('M'))],
        scope: KeyScope::Commits,
        intent: AppIntent::Commits(CommitsIntent::Compare(CompareIntent::ToggleMark)),
        description: "Mark the selected commit to diff against (enter on another); again to clear",
        label: "mark for diff",
        bar: false,
        disabled: commits_mark_disabled,
    },
];

// Chunk 76: compare
pub(crate) static COMPARE_BRANCHES: &[Binding] = &[
    Binding {
        id: "branches.mark",
        keys: &[KeyPattern::any(KeyCode::Char('M'))],
        scope: KeyScope::Branches,
        intent: AppIntent::Commits(CommitsIntent::Compare(CompareIntent::ToggleMark)),
        description: "Mark the selected branch to diff against (enter on another); again to clear",
        label: "mark for diff",
        bar: false,
        disabled: no_branch_selected,
    },
    Binding {
        id: "branches.diff",
        keys: &[KeyPattern::any(KeyCode::Enter)],
        scope: KeyScope::Branches,
        intent: AppIntent::Commits(CommitsIntent::Compare(CompareIntent::Open)),
        description: "Show what changed from the diff mark to the selected branch",
        label: "diff with mark",
        bar: false,
        disabled: branches_diff_disabled,
    },
];
