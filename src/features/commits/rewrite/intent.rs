//! The history rewrites' intents. They nest in `CommitsIntent::Rewrite`,
//! so core's one `AppIntent::Commits` arm reaches this feature's `update`
//! through the commits feature (see `docs/adr/0002-nested-intents.md`).

use super::{FixupBase, TodoFile};
use crate::effect::Tag;
use crate::features::commits::CommitsIntent;
use crate::git::commit_files::CommitId;
use crate::git::rebase_plan::{Direction, Edit};
use crate::op::GitOp;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RewriteIntent {
    /// Rewrite the selected commit with git's interactive rebase, or
    /// change the selected todo row's action while a rebase is stopped.
    Edit(Edit),
    /// Move the selected commit or todo row up (newer) or down (older).
    Move(Direction),
    /// Stop the rebase after the selected commit, or toggle the stop
    /// after the selected todo row.
    InsertBreak,
    /// Commit the staged change as a `fixup!` of the selected commit.
    CreateFixup,
    /// Ask, then fold every `fixup!` commit into the commit it fixes.
    Autosquash,
    /// Autosquash, as the question asked.
    ConfirmAutosquash,
    /// Drop a log range, as the question asked: the full ids it held then,
    /// newest first, so a range that changed since is not dropped.
    ConfirmDropRange(Vec<CommitId>),
    /// Find which commit last touched the staged lines.
    FindFixupBase,
    /// The commits the staged lines come from, or why they are not known.
    FixupBaseFound {
        tag: Tag,
        result: Result<FixupBase, String>,
    },
    /// The read of the commits a detached `HEAD` would leave behind: which
    /// op was planned, from which `HEAD`, and what was found or why not.
    LeftBehindChecked {
        tag: Tag,
        op: GitOp,
        head: CommitId,
        result: Result<Vec<CommitId>, String>,
    },
    /// Leave the commits behind, as the question asked.
    ConfirmLeaveCommits,
    /// The stopped rebase's todo list was read; `None` when there is none
    /// to edit, as in a rebase that is not interactive.
    TodoLoaded {
        tag: Tag,
        result: Result<Option<TodoFile>, String>,
    },
    /// An edited todo list was written, or why not.
    TodoWritten {
        tag: Tag,
        result: Result<(), String>,
    },
}

impl From<RewriteIntent> for CommitsIntent {
    fn from(intent: RewriteIntent) -> Self {
        Self::Rewrite(intent)
    }
}
