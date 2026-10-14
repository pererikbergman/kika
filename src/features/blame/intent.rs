//! The Blame feature's intents. Core wraps them in one variant,
//! `AppIntent::Blame`, and hands them to this feature's `update`
//! (see `docs/adr/0002-nested-intents.md`).

use crate::effect::Tag;
use crate::intent::Scroll;
use kika_git::blame::BlameLine;
use kika_git::diff::DiffLine;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BlameIntent {
    /// Blame the selected file: who last changed each line, and when.
    Open,
    /// A blame load finished; `tag` says which one.
    Loaded {
        tag: Tag,
        result: Result<Vec<BlameLine>, String>,
    },
    /// Move the blame cursor a line or a page down or up.
    Next,
    Prev,
    PageDown,
    PageUp,
    /// Show the commit that last changed the line under the blame cursor.
    OpenCommit,
    /// A commit load finished; `tag` says which one.
    CommitLoaded {
        tag: Tag,
        result: Result<Vec<DiffLine>, String>,
    },
    /// Back from the commit to the blame, at the same line.
    CloseCommit,
    ScrollCommit(Scroll),
    /// Back from the blame to the Files panel.
    Close,
}
