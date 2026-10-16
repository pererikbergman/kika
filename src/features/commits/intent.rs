//! The commits subtree's intents. Core wraps them in one variant,
//! `AppIntent::Commits`, and hands them to this feature's `update`, which
//! passes each sub-feature's own enum on to that sub-feature's `update`
//! (see `docs/adr/0002-nested-intents.md`).

use super::bisect::BisectIntent;
use super::compare::CompareIntent;
use super::copy::CopyIntent;
use super::custom_patch::CustomPatchIntent;
use super::git::Tip;
use super::reflog::ReflogIntent;
use super::rewrite::RewriteIntent;
use crate::effect::Tag;
use crate::list_filter::LogFilterField;
use crate::op::ResetMode;
use kika_git::commit_files::CommitFile;
use kika_git::log::{LogPage, Markers};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommitsIntent {
    /// A page of the log finished; `tag` says which load, `skip` which
    /// page. `Ok` also carries the unix time it was read at.
    PageLoaded {
        tag: Tag,
        skip: usize,
        result: Result<(LogPage, i64), String>,
    },
    /// The push and merge markers finished; `tag` says which load.
    MarkersLoaded {
        tag: Tag,
        result: Result<Markers, String>,
    },
    /// The branch tips were read; `tag` says which load.
    TipsLoaded {
        tag: Tag,
        result: Result<Vec<Tip>, String>,
    },
    /// Move the commit cursor down or up.
    Next,
    Prev,
    /// Move the commit cursor by this many rows, through the rebase todo
    /// as `Next` and `Prev` do, clamped at both ends: a click or a wheel
    /// notch. At most one page load, however far it moves.
    MoveBy(isize),
    /// Show the selected commit's files in place of the log.
    OpenFiles,
    /// Back from a commit's files to the log, at the same commit.
    CloseFiles,
    /// Move the cursor in a commit's files down or up.
    FileNext,
    FilePrev,
    /// Move the file cursor by this many rows, clamped at both ends: a
    /// click or a wheel notch. One diff load, however far it moves.
    FileMoveBy(isize),
    /// A commit's files were read; `tag` says which load.
    FilesLoaded {
        tag: Tag,
        result: Result<Vec<CommitFile>, String>,
    },
    /// Show the Commits panel's other tab: the log or the reflog.
    SwitchTab,
    /// Type the path or the author git narrows the log to.
    OpenLogFilter(LogFilterField),
    /// Show the selected file's history in the Commits panel.
    LogOfFile,
    /// Ask before checking out the selected commit as a detached `HEAD`.
    Checkout,
    /// Make a commit that undoes the selected one.
    Revert,
    /// Reset the current branch to the selected commit. A hard reset asks
    /// first.
    ResetTo(ResetMode),
    /// Reset hard, as the question asked.
    ConfirmResetHard,
    /// The reflog and undo/redo; its own `update` handles it.
    Reflog(ReflogIntent),
    /// History rewrites and the stopped rebase's todo list.
    Rewrite(RewriteIntent),
    /// Copying commits and their text.
    Copy(CopyIntent),
    /// The custom patch built from commit files.
    CustomPatch(CustomPatchIntent),
    /// Marking commits in a bisect, and ending it.
    Bisect(BisectIntent),
    /// Comparing two commits or branches.
    Compare(CompareIntent),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent::AppIntent;

    #[test]
    fn a_child_intent_reaches_app_intent_through_its_parent() {
        assert_eq!(
            AppIntent::from(CommitsIntent::from(ReflogIntent::Undo)),
            AppIntent::Commits(CommitsIntent::Reflog(ReflogIntent::Undo))
        );
    }
}
