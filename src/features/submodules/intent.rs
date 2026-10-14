//! The Submodules feature's intents. Core wraps them in one variant,
//! `AppIntent::Submodules`, and hands them to this feature's `update`
//! (see `docs/adr/0002-nested-intents.md`).

use crate::effect::Tag;
use kika_git::submodule::Submodule;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SubmodulesIntent {
    /// Open the list of submodules and load it.
    Open,
    /// The submodule list was read; `tag` says which load.
    Loaded {
        tag: Tag,
        result: Result<Vec<Submodule>, String>,
    },
    /// Move the submodule cursor down or up.
    Next,
    Prev,
    /// Initialize or update the submodule under the cursor.
    Init,
    Update,
    /// Work in the submodule under the cursor as if Kika started there.
    Enter,
    /// Back to the repository the current one was entered from.
    Leave,
}
