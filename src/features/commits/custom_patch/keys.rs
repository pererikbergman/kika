//! The custom patch's key bindings and the guards they and its menu
//! use: Ctrl+P opens the patch menu (chunk 7), Space in a commit's files
//! picks a whole file (chunk 17), and y/n answer the rewrite question
//! (chunk 63). `CHUNKS` in `keymap.rs` places the chunks, so their
//! position there is behaviour.

use crossterm::event::{KeyCode, KeyModifiers};

use super::{PatchAction, PatchChoice};
use crate::confirm_question::Answer;
use crate::features::commits::with_stacked_branches;
use crate::git::commit_files::{CommitId, CommitRange};
use crate::git::patch::PatchError;
use crate::git::rebase_plan::{self, RebasePlan};
use crate::git::status::EntryKind;
use crate::intent::AppIntent;
use crate::keymap::{
    Binding, CLEAR_LOG_FILTER, KeyPattern, KeyScope, NONE, OP_RUNNING, Reason,
    commit_source_disabled, in_progress_disabled, list_guard, never,
};
use crate::model::AppState;

/// Why picks from `range` cannot join the custom patch: it holds another
/// commit's.
pub(crate) fn other_commit_disabled(state: &AppState, range: &CommitRange) -> Option<Reason> {
    match &state.custom_patch {
        Some(patch) if !patch.is_from(range) => Some(patch.other_commit_notice().into()),
        _ => None,
    }
}

/// Why the selected commit file cannot be picked whole.
pub(crate) fn patch_file_disabled(state: &AppState) -> Option<Reason> {
    let Some(open) = &state.commits.open else {
        return Some("No commit open".into());
    };
    if let Some(reason) = list_guard(state, false)
        .or_else(|| commit_source_disabled(state, open))
        .or_else(|| other_commit_disabled(state, &open.range))
    {
        return Some(reason);
    }
    match open.selected_file().map(|f| f.status) {
        None => Some("No file selected".into()),
        Some('R') => Some(PatchError::Renamed.message().into()),
        Some('C') => Some(PatchError::Copied.message().into()),
        Some(_) => None,
    }
}

/// Why the custom patch's rewrite `choice` cannot run now, the first of:
/// nothing picked, an operation running or in progress, a tracked change
/// (it would mix into the rewrite), a narrowed log (the plan is made from
/// its rows), a source not loaded or not rewritable, and the plan itself,
/// which for a move needs the cursor on another log commit.
pub(crate) fn patch_rewrite_disabled(state: &AppState, choice: PatchChoice) -> Option<Reason> {
    patch_state_disabled(state).or_else(|| match patch_action(state, choice) {
        Ok(action) => patch_plan(state, &action).err(),
        Err(reason) => Some(reason),
    })
}

/// [`patch_rewrite_disabled`] for `action` as it was asked about, its
/// target fixed then.
pub(crate) fn patch_action_disabled(state: &AppState, action: &PatchAction) -> Option<Reason> {
    patch_state_disabled(state).or_else(|| patch_plan(state, action).err())
}

/// Why the patch is empty.
pub(crate) const PATCH_EMPTY: &str = "The custom patch is empty";

fn patch_state_disabled(state: &AppState) -> Option<Reason> {
    let Some(patch) = state.custom_patch.as_ref().filter(|p| !p.is_empty()) else {
        return Some(PATCH_EMPTY.into());
    };
    if state.op_is_running() {
        return Some(OP_RUNNING.into());
    }
    if let Some(reason) = in_progress_disabled(state) {
        return Some(reason);
    }
    if state.files.iter().any(|f| f.kind != EntryKind::Untracked) {
        return Some("Commit or stash your changes first".into());
    }
    if state.commits.query.is_active() || !state.commits.filter.is_empty() {
        return Some(CLEAR_LOG_FILTER.into());
    }
    let rows = &state.commits.rows;
    let source = patch.source.oid.as_str();
    match rows.iter().find(|row| row.oid == source) {
        None => Some("The patch's commit is not loaded; move down to load more".into()),
        Some(row) if row.parents.is_empty() => Some("Cannot rewrite the first commit".into()),
        Some(row) if row.parents.len() > 1 => Some("Cannot rewrite a merge commit".into()),
        Some(_) => None,
    }
}

/// The action `choice` names now: a move goes to the commit under the
/// cursor; a new commit's message is asked for later.
pub(crate) fn patch_action(state: &AppState, choice: PatchChoice) -> Result<PatchAction, Reason> {
    Ok(match choice {
        PatchChoice::Remove => PatchAction::Remove,
        PatchChoice::ToIndex => PatchAction::ToIndex,
        PatchChoice::NewCommit => PatchAction::NewCommit(String::new()),
        PatchChoice::MoveToSelected => {
            if state.commits.todo_selected.is_some() {
                return Err("Select a commit in the log first".into());
            }
            let row = state.commits.rows.get(state.commits.selected);
            let target = row.and_then(|row| CommitId::parse(&row.oid));
            PatchAction::MoveTo(target.ok_or("No commits")?)
        }
    })
}

/// Why a move cannot go to the commit the patch comes from.
pub(crate) const TARGET_IS_SOURCE: &str =
    "The patch comes from this commit; select the commit to move it to";

/// The rebase plan for `action`, as it will run: a stop at the source,
/// and at a move's target, every other commit picked. A move to an older
/// commit stops at the target only: when git picks the source later, the
/// change is there already and drops out of it. The index gets the patch
/// checked at the last commit too, while the rebase can still be aborted.
pub(crate) fn patch_plan(state: &AppState, action: &PatchAction) -> Result<RebasePlan, Reason> {
    let patch = state.custom_patch.as_ref().ok_or(PATCH_EMPTY)?;
    let rows = state.commits.rows.as_slice();
    let row_of = |id: &CommitId| rows.iter().position(|row| row.oid == id.as_str());
    let source = row_of(&patch.source.oid).ok_or("The patch's commit is not loaded")?;
    let stops = match action {
        PatchAction::Remove | PatchAction::NewCommit(_) => vec![source],
        PatchAction::ToIndex => vec![source, 0],
        PatchAction::MoveTo(target) => {
            let target = row_of(target).ok_or("The target commit is not loaded")?;
            if target == source {
                return Err(TARGET_IS_SOURCE.into());
            }
            if target > source {
                vec![target]
            } else {
                vec![source, target]
            }
        }
    };
    let plan = rebase_plan::plan_stops(rows, &stops)?;
    with_stacked_branches(state, plan, &[])
}

// Chunk 7: custom_patch
#[rustfmt::skip]
pub(crate) static CUSTOM_PATCH_WORKSPACE: &[Binding] = &[
    // Out of the bar, which is full at 80 columns; the help menu lists it.
    Binding {
        id: "workspace.patch_menu",
        keys: &[KeyPattern::exact(KeyCode::Char('p'), KeyModifiers::CONTROL)],
        scope: KeyScope::Workspace,
        intent: AppIntent::OpenPatchMenu,
        description: "Show what can be done with the custom patch: remove, move or split it off",
        label: "patch",
        bar: false,
        disabled: never,
    },
];

// Chunk 17: custom_patch
#[rustfmt::skip]
pub(crate) static CUSTOM_PATCH_COMMIT_FILES: &[Binding] = &[
    Binding {
        id: "commit_files.patch",
        keys: &[KeyPattern::exact(KeyCode::Char(' '), NONE)],
        scope: KeyScope::CommitFiles,
        intent: AppIntent::PatchToggleFile,
        description: "Pick the selected file's whole change for the custom patch, or drop it",
        label: "patch",
        bar: true,
        disabled: patch_file_disabled,
    },
];

// Chunk 63: custom_patch
pub(crate) static CUSTOM_PATCH_CONFIRM: &[Binding] = &[
    // Enter is left unbound here too: a custom patch rewrites commits the
    // user may not be looking at.
    Binding {
        id: "patch_confirm.yes",
        keys: Answer::YOnly.keys(),
        scope: KeyScope::PatchConfirm,
        intent: AppIntent::ConfirmYes,
        description: "Yes, rewrite the history with the custom patch",
        label: "rewrite",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "patch_confirm.no",
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::PatchConfirm,
        intent: AppIntent::ConfirmNo,
        description: "No, leave the history as it is",
        label: "keep",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "patch_confirm.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::PatchConfirm,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
];
