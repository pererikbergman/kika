//! Every menu Kika shows, declared once like the keymap. An item's result
//! is an intent, and it can be disabled with a reason, as a key can.

use crate::bisect;
use crate::clipboard::CopyWhat;
use crate::features::command_log::CommandLogIntent;
use crate::features::commit::CommitIntent;
use crate::features::commit::keys::{amend_disabled, commit_disabled};
use crate::features::commits::CommitsIntent;
use crate::features::commits::bisect::BisectIntent;
use crate::features::commits::copy::CopyIntent;
use crate::features::commits::custom_patch::CustomPatchIntent;
use crate::features::commits::custom_patch::PatchChoice;
use crate::features::commits::custom_patch::keys::patch_rewrite_disabled;
use crate::features::commits::keys::reset_disabled;
use crate::features::files::FilesIntent;
use crate::features::files::keys::{files_stage_disabled, stage_all_disabled};
use crate::features::operation::OperationIntent;
use crate::features::operation::keys::merge_disabled;
use crate::features::pull_requests::PullRequestsIntent;
use crate::features::pull_requests::{
    commit_url_disabled, copy_url_disabled, create_disabled, open_url_disabled,
};
use crate::features::repos::ReposIntent;
use crate::features::stash::StashIntent;
use crate::features::stash::keys::{
    stash_all_disabled, stash_file_disabled, stash_staged_disabled, stash_untracked_disabled,
};
use crate::features::submodules::SubmodulesIntent;
use crate::features::worktrees::WorktreesIntent;
use crate::git::bisect::BisectMark;
use crate::intent::AppIntent;
use crate::keymap::{Reason, never, no_commit_selected};
use crate::model::AppState;
use crate::op::{MergeStrategy, ResetMode};
use crate::popup::PromptKind;

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
    /// Open, copy or create the selected branch's pull request.
    PullRequest,
    /// Rewrite history with the custom patch, or drop it.
    CustomPatch,
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
            MenuId::PullRequest => "Pull request",
            MenuId::CustomPatch => "Custom patch",
        }
    }
}

/// One row of a menu. An item whose intent is `OpenMenu` opens a submenu.
#[derive(Debug)]
pub struct MenuItem {
    pub label: &'static str,
    pub intent: AppIntent,
    /// Why the item cannot run right now, if it cannot. Pure.
    pub disabled: fn(&AppState) -> Option<Reason>,
}

fn no_files(state: &AppState) -> Option<Reason> {
    state.files.rows.is_empty().then_some("No files".into())
}

static ACTIONS: &[MenuItem] = &[
    MenuItem {
        label: "Stage…",
        intent: AppIntent::OpenMenu(MenuId::Stage),
        disabled: never,
    },
    MenuItem {
        label: "Commit…",
        intent: AppIntent::Commit(CommitIntent::Open),
        disabled: commit_disabled,
    },
    MenuItem {
        label: "Amend last commit…",
        intent: AppIntent::Commit(CommitIntent::OpenAmend),
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
        intent: AppIntent::Submodules(SubmodulesIntent::Open),
        disabled: never,
    },
    MenuItem {
        label: "Worktrees…",
        intent: AppIntent::Worktrees(WorktreesIntent::OpenList),
        disabled: crate::features::worktrees::worktrees_disabled,
    },
    MenuItem {
        label: "Command log…",
        intent: AppIntent::CommandLog(CommandLogIntent::Open),
        disabled: never,
    },
    MenuItem {
        label: "Recent repositories…",
        intent: AppIntent::Repos(ReposIntent::OpenRecent),
        disabled: never,
    },
    MenuItem {
        label: "Quit…",
        intent: AppIntent::AskQuit,
        disabled: never,
    },
];

fn remove_disabled(state: &AppState) -> Option<Reason> {
    patch_rewrite_disabled(state, PatchChoice::Remove)
}

fn move_disabled(state: &AppState) -> Option<Reason> {
    patch_rewrite_disabled(state, PatchChoice::MoveToSelected)
}

fn to_index_disabled(state: &AppState) -> Option<Reason> {
    patch_rewrite_disabled(state, PatchChoice::ToIndex)
}

fn new_commit_disabled(state: &AppState) -> Option<Reason> {
    patch_rewrite_disabled(state, PatchChoice::NewCommit)
}

fn no_patch(state: &AppState) -> Option<Reason> {
    state
        .custom_patch
        .is_none()
        .then_some("No custom patch".into())
}

/// Each rewrite asks with `y` only before it runs; a reset only drops
/// Kika's selection, so it does not ask.
static CUSTOM_PATCH: &[MenuItem] = &[
    MenuItem {
        label: "Remove from commit",
        intent: AppIntent::Commits(CommitsIntent::CustomPatch(CustomPatchIntent::Choose(
            PatchChoice::Remove,
        ))),
        disabled: remove_disabled,
    },
    MenuItem {
        label: "Move to selected commit",
        intent: AppIntent::Commits(CommitsIntent::CustomPatch(CustomPatchIntent::Choose(
            PatchChoice::MoveToSelected,
        ))),
        disabled: move_disabled,
    },
    MenuItem {
        label: "Move to index",
        intent: AppIntent::Commits(CommitsIntent::CustomPatch(CustomPatchIntent::Choose(
            PatchChoice::ToIndex,
        ))),
        disabled: to_index_disabled,
    },
    MenuItem {
        label: "New commit after source…",
        intent: AppIntent::Commits(CommitsIntent::CustomPatch(CustomPatchIntent::Choose(
            PatchChoice::NewCommit,
        ))),
        disabled: new_commit_disabled,
    },
    MenuItem {
        label: "Reset patch",
        intent: AppIntent::Commits(CommitsIntent::CustomPatch(CustomPatchIntent::Reset)),
        disabled: no_patch,
    },
];

static STAGE: &[MenuItem] = &[
    MenuItem {
        label: "Selected file",
        intent: AppIntent::Files(FilesIntent::ToggleStage),
        disabled: files_stage_disabled,
    },
    MenuItem {
        label: "Everything",
        intent: AppIntent::Files(FilesIntent::ToggleStageAll),
        disabled: stage_all_disabled,
    },
];

static MERGE: &[MenuItem] = &[
    MenuItem {
        label: "Fast-forward only",
        intent: AppIntent::Operation(OperationIntent::Merge(MergeStrategy::FastForwardOnly)),
        disabled: merge_disabled,
    },
    MenuItem {
        label: "Merge commit",
        intent: AppIntent::Operation(OperationIntent::Merge(MergeStrategy::NoFastForward)),
        disabled: merge_disabled,
    },
    MenuItem {
        label: "Squash",
        intent: AppIntent::Operation(OperationIntent::Merge(MergeStrategy::Squash)),
        disabled: merge_disabled,
    },
    MenuItem {
        label: "Rebase current branch onto it",
        intent: AppIntent::Operation(OperationIntent::RebaseOnto),
        disabled: merge_disabled,
    },
];

static RESET: &[MenuItem] = &[
    MenuItem {
        label: "Soft: keep index and worktree",
        intent: AppIntent::Commits(CommitsIntent::ResetTo(ResetMode::Soft)),
        disabled: reset_disabled,
    },
    MenuItem {
        label: "Mixed: keep worktree, reset index",
        intent: AppIntent::Commits(CommitsIntent::ResetTo(ResetMode::Mixed)),
        disabled: reset_disabled,
    },
    MenuItem {
        label: "Hard: discard all changes…",
        intent: AppIntent::Commits(CommitsIntent::ResetTo(ResetMode::Hard)),
        disabled: reset_disabled,
    },
];

static STASH: &[MenuItem] = &[
    MenuItem {
        label: "Stash all changes…",
        intent: AppIntent::Stash(StashIntent::All),
        disabled: stash_all_disabled,
    },
    MenuItem {
        label: "Stash all, including untracked…",
        intent: AppIntent::Stash(StashIntent::IncludingUntracked),
        disabled: stash_untracked_disabled,
    },
    MenuItem {
        label: "Stash staged changes only…",
        intent: AppIntent::Stash(StashIntent::Staged),
        disabled: stash_staged_disabled,
    },
    MenuItem {
        label: "Stash selected files…",
        intent: AppIntent::Stash(StashIntent::File),
        disabled: stash_file_disabled,
    },
];

static BISECT: &[MenuItem] = &[
    MenuItem {
        label: "Mark selected bad (starts a bisect)",
        intent: AppIntent::Commits(CommitsIntent::Bisect(BisectIntent::MarkSelected(
            BisectMark::Bad,
        ))),
        disabled: bisect::selected_bad_disabled,
    },
    MenuItem {
        label: "Mark selected good (starts a bisect)",
        intent: AppIntent::Commits(CommitsIntent::Bisect(BisectIntent::MarkSelected(
            BisectMark::Good,
        ))),
        disabled: bisect::selected_good_disabled,
    },
    MenuItem {
        label: "Skip selected",
        intent: AppIntent::Commits(CommitsIntent::Bisect(BisectIntent::MarkSelected(
            BisectMark::Skip,
        ))),
        disabled: bisect::selected_skip_disabled,
    },
    MenuItem {
        label: "Reset bisect…",
        intent: AppIntent::Commits(CommitsIntent::Bisect(BisectIntent::AskReset)),
        disabled: bisect::reset_disabled,
    },
];

static COPY: &[MenuItem] = &[
    MenuItem {
        label: "Full hash",
        intent: AppIntent::Commits(CommitsIntent::Copy(CopyIntent::CopyText(CopyWhat::Hash))),
        disabled: no_commit_selected,
    },
    MenuItem {
        label: "Short hash",
        intent: AppIntent::Commits(CommitsIntent::Copy(CopyIntent::CopyText(
            CopyWhat::ShortHash,
        ))),
        disabled: no_commit_selected,
    },
    MenuItem {
        label: "Subject",
        intent: AppIntent::Commits(CommitsIntent::Copy(CopyIntent::CopyText(CopyWhat::Subject))),
        disabled: no_commit_selected,
    },
    MenuItem {
        label: "Full message",
        intent: AppIntent::Commits(CommitsIntent::Copy(CopyIntent::CopyText(CopyWhat::Message))),
        disabled: no_commit_selected,
    },
    MenuItem {
        label: "Forge URL",
        intent: AppIntent::Commits(CommitsIntent::Copy(CopyIntent::CopyText(
            CopyWhat::ForgeUrl,
        ))),
        disabled: commit_url_disabled,
    },
];

static PULL_REQUEST: &[MenuItem] = &[
    MenuItem {
        label: "Open in browser",
        intent: AppIntent::PullRequests(PullRequestsIntent::OpenForgeUrl),
        disabled: open_url_disabled,
    },
    MenuItem {
        label: "Copy URL",
        intent: AppIntent::PullRequests(PullRequestsIntent::CopyForgeUrl),
        disabled: copy_url_disabled,
    },
    MenuItem {
        label: "Create pull request…",
        intent: AppIntent::PullRequests(PullRequestsIntent::CreateStart),
        disabled: create_disabled,
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
        MenuId::PullRequest => PULL_REQUEST,
        MenuId::CustomPatch => CUSTOM_PATCH,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::FileEntry;
    use crate::git::status::EntryKind;
    use crate::keymap::OP_RUNNING;
    use crate::model::Context;
    use crate::test_support::load_list;

    fn file(path: &str, index: char, worktree: char, kind: EntryKind) -> FileEntry {
        FileEntry {
            path: path.into(),
            orig_path: None,
            index,
            worktree,
            kind,
        }
    }

    /// Each stash item's reason; all of them are fixed text.
    fn reasons(files: Vec<FileEntry>, selected: Option<usize>) -> Vec<Option<&'static str>> {
        let mut state = AppState::new(Context::Workspace);
        state.files.rows = files;
        match selected {
            Some(index) => state.files.selected = index,
            // Nothing marked. The list has a cursor whenever it has rows,
            // so this is a range over no listed file.
            None => state.files.range_anchor = Some("not listed".into()),
        }
        items(MenuId::Stash)
            .iter()
            .map(|item| match (item.disabled)(&state) {
                Some(std::borrow::Cow::Borrowed(reason)) => Some(reason),
                Some(owned) => panic!("not fixed text: {owned}"),
                None => None,
            })
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
        // Only an untracked file: only "including untracked" and "selected files".
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
    fn during_a_bisect_reset_merge_commit_and_amend_items_say_to_reset_it_first() {
        use crate::git::repo_state::Operation;
        use crate::git::status::{BranchStatus, Head};

        let mut state = AppState::new(Context::Workspace);
        state.files.rows = vec![file("a", 'M', '.', EntryKind::Changed)];
        state.commits.list.rows = vec![crate::git::log::CommitRow {
            oid: "abc".into(),
            short_oid: "abc".into(),
            parents: vec!["p".into()],
            author: "Ann".into(),
            time: 0,
            subject: "s".into(),
            subject_altered: false,
        }];
        load_list(
            &mut state.branches.list,
            Ok(vec![crate::git::branches::Branch {
                name: "topic".into(),
                head: false,
                upstream: None,
                track: crate::git::branches::Track::None,
                merged: false,
            }]),
        );
        state.branch = Some(BranchStatus {
            head: Head::Detached("abc".into()),
            upstream: None,
            ahead_behind: None,
            in_progress: vec![Operation::Bisect],
            bisect: None,
        });
        let reset_first = Some(crate::bisect::RESET_FIRST.into());
        let refused = items(MenuId::Reset)
            .iter()
            .chain(items(MenuId::Merge))
            .chain(&items(MenuId::Actions)[1..3]);
        for item in refused {
            assert_eq!((item.disabled)(&state), reset_first, "{}", item.label);
        }
        let labels: Vec<_> = items(MenuId::Actions)[1..3]
            .iter()
            .map(|i| i.label)
            .collect();
        assert_eq!(labels, ["Commit…", "Amend last commit…"]);
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
        state.files.rows = files;
        state.hold_op(1);
        for item in items(MenuId::Stash) {
            assert_eq!(
                (item.disabled)(&state),
                Some(OP_RUNNING.into()),
                "{}",
                item.label
            );
        }
    }
}
