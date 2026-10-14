//! The branches subtree's intents. Core wraps them in one variant,
//! `AppIntent::Branches`, and hands them to this feature's `update`, which
//! passes each sub-feature's own enum on to that sub-feature's `update`
//! (see `docs/adr/0002-nested-intents.md`).

use super::remotes::RemotesIntent;
use super::tags::TagsIntent;
use crate::effect::Tag;
use crate::git::branches::{Branch, LogRow};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BranchesIntent {
    /// The branch list was read; `tag` says which load.
    Loaded {
        tag: Tag,
        result: Result<Vec<Branch>, String>,
    },
    /// The log of the full ref `refname` was read; `tag` says which load.
    LogLoaded {
        tag: Tag,
        refname: Vec<u8>,
        result: Result<Vec<LogRow>, String>,
    },
    /// Show the Branches panel's next tab: local branches, remotes, tags.
    SwitchTab,
    /// Show the Branches panel's previous tab.
    PrevTab,
    /// Move the branch selection down or up.
    Next,
    Prev,
    /// Switch to the selected branch.
    Checkout,
    /// Ask for a name, to make a branch at the selected one.
    New,
    /// Ask for a new name for the selected branch, the old one filled in.
    Rename,
    /// Ask before deleting the selected branch.
    Delete,
    /// The new branch's name, typed into the `PromptKind::NewBranch` prompt.
    CreateNamed(String),
    /// Ask for a name, to make a branch at the detached `HEAD`.
    NewAtHead,
    /// The new branch's name, typed into the `PromptKind::NewBranchAtHead`
    /// prompt.
    CreateAtHeadNamed(String),
    /// The new name, typed into the `PromptKind::RenameBranch` prompt.
    RenameTo(String),
    /// Delete the branch the open question names: plainly, or with
    /// `force` when its commits are not merged.
    ConfirmDelete {
        force: bool,
    },
    /// Ask which remote branch the selected local branch should track.
    SetUpstream,
    /// The upstream, as `remote/branch`, typed into the
    /// `PromptKind::SetUpstream` prompt.
    SetUpstreamTo(String),
    /// Stop the selected local branch tracking its upstream.
    UnsetUpstream,
    /// The Remotes tab; its own `update` handles it.
    Remotes(RemotesIntent),
    /// The Tags tab and making tags; its own `update` handles it.
    Tags(TagsIntent),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent::AppIntent;

    #[test]
    fn a_child_intent_reaches_app_intent_through_its_parent() {
        assert_eq!(
            AppIntent::from(BranchesIntent::from(TagsIntent::Push)),
            AppIntent::Branches(BranchesIntent::Tags(TagsIntent::Push))
        );
    }
}
