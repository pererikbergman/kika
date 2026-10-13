//! The operation's intents. Core wraps them in one variant,
//! `AppIntent::Operation`, and hands them to this feature's `update`
//! (see `docs/adr/0002-nested-intents.md`). Continue, skip and abort stay
//! core intents, because bisect shares them.

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
}
