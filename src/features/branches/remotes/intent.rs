//! The Remotes tab's intents. They nest in `BranchesIntent::Remotes`, so
//! core's one `AppIntent::Branches` arm reaches this feature's `update`
//! through the branches feature (see `docs/adr/0002-nested-intents.md`).

use crate::effect::Tag;
use crate::features::branches::BranchesIntent;
use kika_git::remotes::Remote;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemotesIntent {
    /// The remotes and their branches were read; `tag` says which load.
    Loaded {
        tag: Tag,
        result: Result<Vec<Remote>, String>,
    },
    /// Move the Remotes tab's selection down or up, over remote and
    /// branch rows alike.
    Next,
    Prev,
    /// Move the cursor by this many rows, clamped at both ends: a click
    /// or a wheel notch.
    MoveBy(isize),
    /// Ask for a local name, to check out the selected remote branch as a
    /// new branch tracking it.
    Track,
    /// The local name, typed into the `TrackBranch` prompt.
    TrackNamed(String),
    /// Ask for a new remote's name, then its URL.
    Add,
    /// The new remote's name, typed into the `RemoteName` prompt.
    AddNamed(String),
    /// The new remote's URL, typed into the `RemoteUrl` prompt.
    AddUrl(String),
    /// Ask for a new name for the selected remote, the old one filled in.
    Rename,
    /// The new name, typed into the `PromptKind::RenameRemote` prompt.
    RenameTo(String),
    /// Ask before removing the selected remote.
    Remove,
    /// Remove the remote the open question names.
    ConfirmRemove,
}

impl From<RemotesIntent> for BranchesIntent {
    fn from(intent: RemotesIntent) -> Self {
        Self::Remotes(intent)
    }
}
