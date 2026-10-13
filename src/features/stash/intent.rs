//! The Stash panel's intents. Core wraps them in one variant,
//! `AppIntent::Stash`, and hands them to this feature's `update`
//! (see `docs/adr/0002-nested-intents.md`).

use super::{StashEntry, StashFile};
use crate::effect::Tag;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StashIntent {
    /// The stash list was read; `tag` says which load.
    Loaded {
        tag: Tag,
        result: Result<Vec<StashEntry>, String>,
    },
    /// Move the stash cursor down or up.
    Next,
    Prev,
    /// Show the selected stash's files in place of the list.
    OpenFiles,
    /// Back from a stash's files to the list, at the same entry.
    CloseFiles,
    /// Move the cursor in a stash's files down or up.
    FileNext,
    FilePrev,
    /// A stash's files were read; `tag` says which load.
    FilesLoaded {
        tag: Tag,
        result: Result<Vec<StashFile>, String>,
    },
    /// Apply the selected stash and keep it.
    Apply,
    /// Apply the selected stash and drop it.
    Pop,
    /// Ask before dropping the selected stash.
    Drop,
    /// Drop the stash the question was about.
    ConfirmDrop,
    /// Ask for a new message for the selected stash.
    Rename,
    /// The new message typed for the stash the prompt was about.
    RenameTo(String),
    /// Ask for a message, then stash every tracked change.
    All,
    /// Ask for a message, then stash every change, untracked files too.
    IncludingUntracked,
    /// Ask for a message, then stash only the staged changes.
    Staged,
    /// Ask for a message, then stash only the selected file.
    File,
    /// The message typed for the stash being made; empty for git's own.
    MessageAnswered(String),
}
