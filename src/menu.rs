//! Every menu Kika shows, declared once like the keymap. An item's result
//! is an intent, and it can be disabled with a reason, as a key can.

use crate::git::ops::{MergeStrategy, ResetMode};
use crate::intent::AppIntent;
use crate::keymap::{
    amend_disabled, commit_disabled, merge_disabled, never, reset_disabled, stage_all_disabled,
    stage_disabled,
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
}

impl MenuId {
    pub fn title(self) -> &'static str {
        match self {
            MenuId::Actions => "Actions",
            MenuId::Stage => "Stage",
            MenuId::Merge => "Merge / rebase",
            MenuId::Reset => "Reset to commit",
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

pub fn items(id: MenuId) -> &'static [MenuItem] {
    match id {
        MenuId::Actions => ACTIONS,
        MenuId::Stage => STAGE,
        MenuId::Merge => MERGE,
        MenuId::Reset => RESET,
    }
}
