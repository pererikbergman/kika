//! Every menu Kika shows, declared once like the keymap. An item's result
//! is an intent, and it can be disabled with a reason, as a key can.

use crate::bisect;
use crate::clipboard::CopyWhat;
use crate::custom_patch::PatchChoice;
use crate::features::operation::keys::merge_disabled;
use crate::git::FileEntry;
use crate::git::bisect::BisectMark;
use crate::git::path::RepoPath;
use crate::git::repo_state::Operation;
use crate::git::status::EntryKind;
use crate::intent::AppIntent;
use crate::keymap::{
    OP_RUNNING, Reason, amend_disabled, commit_disabled, files_stage_disabled, list_guard, never,
    no_commit_selected, patch_rewrite_disabled, reset_disabled, stage_all_disabled,
};
use crate::model::AppState;
use crate::model::pull_request::{
    commit_url_disabled, copy_url_disabled, create_disabled, open_url_disabled,
};
use crate::op::{GitOp, MergeStrategy, ResetMode, leading_dirs};
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
    state.files.is_empty().then_some("No files".into())
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
        disabled: crate::features::worktrees::worktrees_disabled,
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
        intent: AppIntent::ChoosePatchAction(PatchChoice::Remove),
        disabled: remove_disabled,
    },
    MenuItem {
        label: "Move to selected commit",
        intent: AppIntent::ChoosePatchAction(PatchChoice::MoveToSelected),
        disabled: move_disabled,
    },
    MenuItem {
        label: "Move to index",
        intent: AppIntent::ChoosePatchAction(PatchChoice::ToIndex),
        disabled: to_index_disabled,
    },
    MenuItem {
        label: "New commit after source…",
        intent: AppIntent::ChoosePatchAction(PatchChoice::NewCommit),
        disabled: new_commit_disabled,
    },
    MenuItem {
        label: "Reset patch",
        intent: AppIntent::ResetPatch,
        disabled: no_patch,
    },
];

static STAGE: &[MenuItem] = &[
    MenuItem {
        label: "Selected file",
        intent: AppIntent::ToggleStage,
        disabled: files_stage_disabled,
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
fn stash_blocked(state: &AppState) -> Option<Reason> {
    if state.op_seq.is_some() {
        Some(OP_RUNNING.into())
    } else if state.files.iter().any(|f| f.kind == EntryKind::Conflicted) {
        Some("Resolve conflicts first".into())
    } else {
        None
    }
}

/// Untracked files stay behind, so they alone are nothing to stash.
pub(crate) fn stash_all_disabled(state: &AppState) -> Option<Reason> {
    stash_blocked(state).or_else(|| {
        (!state.files.iter().any(|f| f.kind != EntryKind::Untracked))
            .then_some("No changes to stash".into())
    })
}

pub(crate) fn stash_untracked_disabled(state: &AppState) -> Option<Reason> {
    stash_blocked(state).or_else(|| {
        state
            .files
            .is_empty()
            .then_some("No changes to stash".into())
    })
}

pub(crate) fn stash_staged_disabled(state: &AppState) -> Option<Reason> {
    stash_blocked(state).or_else(|| {
        (!state.files.iter().any(|f| !matches!(f.index, '.' | '?')))
            .then_some("Nothing staged".into())
    })
}

/// Why a set with a staged deletion and an untracked file is refused;
/// see `GitOp::stash_files_for`.
pub(crate) const STASH_APART: &str = "Stash deleted files apart from untracked";

/// Why a staged rename is refused when a file sits at its old path again.
pub(crate) use crate::op::STASH_RENAME_TAKEN;

/// Why a staged deletion is refused when a file sits at its path again.
pub(crate) use crate::op::STASH_DELETED_TAKEN;

/// Stashing selected files acts on the marked set: the Files range when
/// one is set, else the row under the cursor. A filter that hides the
/// cursor's row refuses it, as other row actions are refused. A staged
/// rename is refused when any listed entry sits at its old path or at a
/// directory that path needs: the restore after the push would overwrite
/// or unlink that file. A staged deletion is refused the same way when any
/// other entry sits at its path or a directory it needs: git checks
/// `HEAD`'s version out there after the push. Names are compared ignoring
/// ASCII case, since a case-insensitive filesystem folds them; on another
/// one this only refuses more.
pub(crate) fn stash_file_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, true)
        .or_else(|| stash_blocked(state))
        .or_else(|| {
            let (entries, _) = state.files_marked();
            let sets: Vec<_> = (entries.iter())
                .filter_map(|e| GitOp::stash_file_for(e).map(|set| (*e, set)))
                .collect();
            // Whether an entry other than `own` sits at `path` or a
            // directory it needs.
            let taken = |own: Option<&FileEntry>, path: &RepoPath| {
                let mut at = leading_dirs(path);
                at.push(path.clone());
                at.iter().any(|taken| {
                    (state.files.iter())
                        .filter(|f| !own.is_some_and(|own| std::ptr::eq(*f, own)))
                        .any(|f| f.path.as_bytes().eq_ignore_ascii_case(taken.as_bytes()))
                })
            };
            let renamed_taken = (sets.iter())
                .flat_map(|(_, set)| &set.restore)
                .any(|old| taken(None, old));
            let deleted_taken = (sets.iter())
                .flat_map(|(own, set)| set.unstage.iter().map(move |gone| (*own, gone)))
                .any(|(own, gone)| taken(Some(own), gone));
            if entries.is_empty() {
                Some("No file selected".into())
            } else if renamed_taken {
                Some(STASH_RENAME_TAKEN.into())
            } else if deleted_taken {
                Some(STASH_DELETED_TAKEN.into())
            } else if GitOp::stash_files_for(&entries).is_none() {
                Some(STASH_APART.into())
            } else {
                None
            }
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
        label: "Stash selected files…",
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
    MenuItem {
        label: "Forge URL",
        intent: AppIntent::CopyCommitText(CopyWhat::ForgeUrl),
        disabled: commit_url_disabled,
    },
];

static PULL_REQUEST: &[MenuItem] = &[
    MenuItem {
        label: "Open in browser",
        intent: AppIntent::OpenForgeUrl,
        disabled: open_url_disabled,
    },
    MenuItem {
        label: "Copy URL",
        intent: AppIntent::CopyForgeUrl,
        disabled: copy_url_disabled,
    },
    MenuItem {
        label: "Create pull request…",
        intent: AppIntent::CreatePrStart,
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

    /// Each stash item's reason; all of them are fixed text.
    fn reasons(files: Vec<FileEntry>, selected: Option<usize>) -> Vec<Option<&'static str>> {
        let mut state = AppState::new(Context::Workspace);
        state.files = files;
        state.selected = selected;
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
        state.files = vec![file("a", 'M', '.', EntryKind::Changed)];
        state.commits.rows = vec![crate::git::log::CommitRow {
            oid: "abc".into(),
            short_oid: "abc".into(),
            parents: vec!["p".into()],
            author: "Ann".into(),
            time: 0,
            subject: "s".into(),
            subject_altered: false,
        }];
        state.branches.loaded(Ok(vec![crate::git::branches::Branch {
            name: "topic".into(),
            head: false,
            upstream: None,
            track: crate::git::branches::Track::None,
            merged: false,
        }]));
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
        state.files = files;
        state.op_seq = Some(1);
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
