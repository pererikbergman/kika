//! The Tags tab's intents. They nest in `BranchesIntent::Tags`, so core's
//! one `AppIntent::Branches` arm reaches this feature's `update` through
//! the branches feature (see `docs/adr/0002-nested-intents.md`).

use crate::effect::Tag;
use crate::features::branches::BranchesIntent;
use kika_git::tags::TagRef;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TagsIntent {
    /// The tags were read; `tag` says which load.
    Loaded {
        tag: Tag,
        result: Result<Vec<TagRef>, String>,
    },
    /// Move the Tags tab's selection down or up.
    Next,
    Prev,
    /// Move the cursor by this many rows, clamped at both ends: a click
    /// or a wheel notch.
    MoveBy(isize),
    /// Ask for a name, to tag the selected commit with a lightweight tag.
    New,
    /// Ask for a name and then a message, to tag the selected commit with
    /// an annotated tag.
    NewAnnotated,
    /// The tag's name, typed into the `TagName` prompt.
    Named(String),
    /// The annotated tag's message, typed into the `TagMessage` prompt.
    Messaged(String),
    /// Ask before deleting the selected tag.
    Delete,
    /// Delete the tag the open question names.
    ConfirmDelete,
    /// Push the selected tag to the current branch's remote, or the only
    /// one.
    Push,
}

impl From<TagsIntent> for BranchesIntent {
    fn from(intent: TagsIntent) -> Self {
        Self::Tags(intent)
    }
}
