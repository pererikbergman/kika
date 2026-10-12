//! Every menu Kika shows, declared once like the keymap. An item's result
//! is an intent, and it can be disabled with a reason, as a key can.

use crate::bisect;
use crate::clipboard::CopyWhat;
use crate::git::bisect::BisectMark;
use crate::git::ops::{MergeStrategy, ResetMode};
use crate::git::repo_state::Operation;
use crate::git::status::EntryKind;
use crate::intent::AppIntent;
use crate::keymap::{
    OP_RUNNING, amend_disabled, commit_disabled, merge_disabled, never, no_commit_selected,
    reset_disabled, stage_all_disabled, stage_disabled,
};
use crate::model::AppState;
use crate::popup::{ConfirmKind, PromptKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuId {
    Actions,
    Stage,
    /// Merge the selected branch into the current one, or rebase onto it.
    Merge,
    /// Reset the current branch to the selected commit.
    Reset,
    /// Put changes aside in a new stash.
    Stash,
    /// Mark the selected commit for a bisect, or end the bisect.
    Bisect,
    /// Copy the selected commit's hash or message to the clipboard.
    Copy,
}

impl MenuId {
    pub fn title(self) -> &'static str {
        match self {
            MenuId::Actions => "Actions",
            MenuId::Stage => "Stage",
            MenuId::Merge => "Merge / rebase",
            MenuId::Reset => "Reset to commit",
            MenuId::Stash => "Stash",
            MenuId::Bisect => "Bisect",
            MenuId::Copy => "Copy",
        }
    }
}

/// One row of a menu. An item whose intent is `OpenMenu` opens a submenu.
#[derive(Debug)]
pub struct MenuItem {
    pub label: &'static str,
    pub intent: AppIntent,
    /// Why the item cannot run right now, if it cannot. Pure.
    pub disabled: fn(&AppState) -> Option<&'static str>,
}

fn no_files(state: &AppState) -> Option<&'static str> {
    state.files.is_empty().then_some("No files")
}

static ACTIONS: &[MenuItem] = &[
    MenuItem {
        label: "Stage…",
        intent: AppIntent::OpenMenu(MenuId::Stage),
        disabled: never,
    },
    MenuItem {
        label: "Commit…",
        intent: AppIntent::OpenCommit,
        disabled: commit_disabled,
    },
    MenuItem {
        label: "Amend last commit…",
        intent: AppIntent::OpenAmend,
        disabled: amend_disabled,
    },
    MenuItem {
        label: "Go to file…",
        intent: AppIntent::OpenPrompt(PromptKind::GoToFile),
        disabled: no_files,
    },
    MenuItem {
        label: "Refresh",
        intent: AppIntent::RefreshStatus,
        disabled: never,
    },
    MenuItem {
        label: "Submodules…",
        intent: AppIntent::OpenSubmodules,
        disabled: never,
    },
    MenuItem {
        label: "Worktrees…",
        intent: AppIntent::OpenWorktrees,
        disabled: crate::worktree_list::worktrees_disabled,
    },
    MenuItem {
        label: "Command log…",
        intent: AppIntent::OpenCommandLog,
        disabled: never,
    },
    MenuItem {
        label: "Recent repositories…",
        intent: AppIntent::OpenRecent,
        disabled: never,
    },
    MenuItem {
        label: "Quit…",
        intent: AppIntent::Ask(ConfirmKind::Quit),
        disabled: never,
    },
];

static STAGE: &[MenuItem] = &[
    MenuItem {
        label: "Selected file",
        intent: AppIntent::ToggleStage,
        disabled: stage_disabled,
    },
    MenuItem {
        label: "Everything",
        intent: AppIntent::ToggleStageAll,
        disabled: stage_all_disabled,
    },
];

static MERGE: &[MenuItem] = &[
    MenuItem {
        label: "Fast-forward only",
        intent: AppIntent::Merge(MergeStrategy::FastForwardOnly),
        disabled: merge_disabled,
    },
    MenuItem {
        label: "Merge commit",
        intent: AppIntent::Merge(MergeStrategy::NoFastForward),
        disabled: merge_disabled,
    },
    MenuItem {
        label: "Squash",
        intent: AppIntent::Merge(MergeStrategy::Squash),
        disabled: merge_disabled,
    },
    MenuItem {
        label: "Rebase current branch onto it",
        intent: AppIntent::RebaseOnto,
        disabled: merge_disabled,
    },
];

static RESET: &[MenuItem] = &[
    MenuItem {
        label: "Soft: keep index and worktree",
        intent: AppIntent::ResetTo(ResetMode::Soft),
        disabled: reset_disabled,
    },
    MenuItem {
        label: "Mixed: keep worktree, reset index",
        intent: AppIntent::ResetTo(ResetMode::Mixed),
        disabled: reset_disabled,
    },
    MenuItem {
        label: "Hard: discard all changes…",
        intent: AppIntent::ResetTo(ResetMode::Hard),
        disabled: reset_disabled,
    },
];

/// Why nothing can be stashed now: one operation at a time, and git
/// cannot stash a conflicted index.
fn stash_blocked(state: &AppState) -> Option<&'static str> {
    if state.op_seq.is_some() {
        Some(OP_RUNNING)
    } else if state.files.iter().any(|f| f.kind == EntryKind::Conflicted) {
        Some("Resolve conflicts first")
    } else {
        None
    }
}

/// Untracked files stay behind, so they alone are nothing to stash.
pub(crate) fn stash_all_disabled(state: &AppState) -> Option<&'static str> {
    stash_blocked(state).or_else(|| {
        (!state.files.iter().any(|f| f.kind != EntryKind::Untracked))
            .then_some("No changes to stash")
    })
}

pub(crate) fn stash_untracked_disabled(state: &AppState) -> Option<&'static str> {
    stash_blocked(state).or_else(|| state.files.is_empty().then_some("No changes to stash"))
}

pub(crate) fn stash_staged_disabled(state: &AppState) -> Option<&'static str> {
    stash_blocked(state).or_else(|| {
        (!state.files.iter().any(|f| !matches!(f.index, '.' | '?'))).then_some("Nothing staged")
    })
}

pub(crate) fn stash_file_disabled(state: &AppState) -> Option<&'static str> {
    stash_blocked(state).or_else(|| {
        state
            .selected
            .and_then(|i| state.files.get(i))
            .is_none()
            .then_some("No file selected")
    })
}

static STASH: &[MenuItem] = &[
    MenuItem {
        label: "Stash all changes…",
        intent: AppIntent::StashAll,
        disabled: stash_all_disabled,
    },
    MenuItem {
        label: "Stash all, including untracked…",
        intent: AppIntent::StashIncludingUntracked,
        disabled: stash_untracked_disabled,
    },
    MenuItem {
        label: "Stash staged changes only…",
        intent: AppIntent::StashStaged,
        disabled: stash_staged_disabled,
    },
    MenuItem {
        label: "Stash this file…",
        intent: AppIntent::StashFile,
        disabled: stash_file_disabled,
    },
];

static BISECT: &[MenuItem] = &[
    MenuItem {
        label: "Mark selected bad (starts a bisect)",
        intent: AppIntent::BisectSelected(BisectMark::Bad),
        disabled: bisect::selected_bad_disabled,
    },
    MenuItem {
        label: "Mark selected good (starts a bisect)",
        intent: AppIntent::BisectSelected(BisectMark::Good),
        disabled: bisect::selected_good_disabled,
    },
    MenuItem {
        label: "Skip selected",
        intent: AppIntent::BisectSelected(BisectMark::Skip),
        disabled: bisect::selected_skip_disabled,
    },
    MenuItem {
        label: "Reset bisect…",
        intent: AppIntent::Ask(ConfirmKind::Abort(Operation::Bisect)),
        disabled: bisect::reset_disabled,
    },
];

static COPY: &[MenuItem] = &[
    MenuItem {
        label: "Full hash",
        intent: AppIntent::CopyCommitText(CopyWhat::Hash),
        disabled: no_commit_selected,
    },
    MenuItem {
        label: "Short hash",
        intent: AppIntent::CopyCommitText(CopyWhat::ShortHash),
        disabled: no_commit_selected,
    },
    MenuItem {
        label: "Subject",
        intent: AppIntent::CopyCommitText(CopyWhat::Subject),
        disabled: no_commit_selected,
    },
    MenuItem {
        label: "Full message",
        intent: AppIntent::CopyCommitText(CopyWhat::Message),
        disabled: no_commit_selected,
    },
];

pub fn items(id: MenuId) -> &'static [MenuItem] {
    match id {
        MenuId::Actions => ACTIONS,
        MenuId::Stage => STAGE,
        MenuId::Merge => MERGE,
        MenuId::Reset => RESET,
        MenuId::Bisect => BISECT,
        MenuId::Copy => COPY,
        MenuId::Stash => STASH,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::FileEntry;
    use crate::model::Context;

    fn file(path: &str, index: char, worktree: char, kind: EntryKind) -> FileEntry {
        FileEntry {
            path: path.into(),
            orig_path: None,
            index,
            worktree,
            kind,
        }
    }

    fn reasons(files: Vec<FileEntry>, selected: Option<usize>) -> Vec<Option<&'static str>> {
        let mut state = AppState::new(Context::Workspace);
        state.files = files;
        state.selected = selected;
        items(MenuId::Stash)
            .iter()
            .map(|item| (item.disabled)(&state))
            .collect()
    }

    #[test]
    fn each_stash_item_says_when_it_has_nothing_to_take() {
        let none = "No changes to stash";
        assert_eq!(
            reasons(vec![], None),
            [
                Some(none),
                Some(none),
                Some("Nothing staged"),
                Some("No file selected")
            ]
        );
        // Only an untracked file: only "including untracked" and "this file".
        let untracked = file("new", '?', '?', EntryKind::Untracked);
        assert_eq!(
            reasons(vec![untracked], Some(0)),
            [Some(none), None, Some("Nothing staged"), None]
        );
        let unstaged = file("a", '.', 'M', EntryKind::Changed);
        assert_eq!(
            reasons(vec![unstaged.clone()], Some(0)),
            [None, None, Some("Nothing staged"), None]
        );
        let staged = file("b", 'M', '.', EntryKind::Changed);
        assert_eq!(
            reasons(vec![unstaged, staged], None),
            [None, None, None, Some("No file selected")]
        );
    }

    #[test]
    fn every_stash_item_waits_for_conflicts_and_running_ops() {
        let files = vec![
            file("a", 'M', 'M', EntryKind::Changed),
            file("both", 'U', 'U', EntryKind::Conflicted),
        ];
        let conflicts = Some("Resolve conflicts first");
        assert_eq!(reasons(files.clone(), Some(0)), [conflicts; 4]);

        let mut state = AppState::new(Context::Workspace);
        state.files = files;
        state.op_seq = Some(1);
        for item in items(MenuId::Stash) {
            assert_eq!((item.disabled)(&state), Some(OP_RUNNING), "{}", item.label);
        }
    }
}
