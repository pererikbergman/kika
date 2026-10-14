//! The operation's intents. Core wraps them in one variant,
//! `AppIntent::Operation`, and hands them to this feature's `update`
//! (see `docs/adr/0002-nested-intents.md`). Continue, skip and abort stay
//! core intents, because bisect shares them.

use crate::effect::Tag;
use crate::git::commit_files::CommitId;
use crate::git::repo_state::Operation;
use crate::op::MergeStrategy;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OperationIntent {
    /// Merge the selected branch into the current one this way.
    Merge(MergeStrategy),
    /// Rebase the current branch onto the selected one.
    RebaseOnto,
    /// Abort this operation, as the question asked.
    ConfirmAbort(Operation),
    /// The check for work done since `operation` stopped, run when Skip
    /// was pressed from the detached `head` (or a branch): whether
    /// skipping would lose any, or why that is not known.
    StopWorkChecked {
        tag: Tag,
        operation: Operation,
        head: Option<CommitId>,
        result: Result<bool, String>,
    },
    /// Skip this operation's stopped commit, as the question asked.
    ConfirmSkip(Operation),
}
