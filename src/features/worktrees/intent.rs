//! The Worktrees popup's intents. Core wraps them in one variant,
//! `AppIntent::Worktrees`, and hands them to this feature's `update`
//! (see `docs/adr/0002-nested-intents.md`).

use std::path::PathBuf;

use super::Worktree;
use crate::effect::Tag;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorktreesIntent {
    /// Open the list of worktrees and load it.
    OpenList,
    /// The worktree list was read; `tag` says which load.
    Loaded {
        tag: Tag,
        result: Result<Vec<Worktree>, String>,
    },
    /// Move the worktree cursor down or up.
    Next,
    Prev,
    /// Switch Kika to the worktree under the cursor.
    Open,
    /// Ask where to add a worktree for the Branches panel's local branch.
    Add,
    /// The typed path for the worktree `AddWorktree` asked about.
    AddAt(String),
    /// A worktree add finished: where it was made, or why not.
    Added {
        tag: Tag,
        result: Result<PathBuf, String>,
    },
    /// Ask before removing the worktree under the cursor.
    Remove,
    /// Remove the worktree the question named.
    ConfirmRemove,
    /// A worktree remove finished.
    Removed {
        tag: Tag,
        result: Result<(), String>,
    },
}
