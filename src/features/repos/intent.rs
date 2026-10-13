//! The repos feature's intents. Core wraps them in one variant,
//! `AppIntent::Repos`, and hands them to this feature's `update`
//! (see `docs/adr/0002-nested-intents.md`). Opening a repository stays the
//! core `AppIntent::OpenRepo`, because worktrees switch through it too.

use std::path::PathBuf;

use crate::effect::Tag;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReposIntent {
    /// Show the recent repositories, to switch to one.
    OpenRecent,
    /// The recent list as now recorded on disk, answering request `seq`.
    Loaded {
        seq: u64,
        list: Vec<PathBuf>,
    },
    /// Move the recent list's cursor down or up.
    Next,
    Prev,
    /// Open the row under the recent list's cursor.
    Select,
    /// Make the launch directory a repository.
    Init,
    /// `git init` finished: the new top level, or why it failed.
    Initialised {
        tag: Tag,
        result: Result<PathBuf, String>,
    },
}
