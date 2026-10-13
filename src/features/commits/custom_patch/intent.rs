//! The custom patch's intents. They nest in `CommitsIntent::CustomPatch`,
//! so core's one `AppIntent::Commits` arm reaches this feature's `update`
//! through the commits feature (see `docs/adr/0002-nested-intents.md`).

use super::{PatchAction, PatchChoice};
use crate::features::commits::CommitsIntent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CustomPatchIntent {
    /// Pick the selected commit file whole into the custom patch, or drop
    /// it.
    ToggleFile,
    /// Show what can be done with the custom patch.
    OpenMenu,
    /// A rewrite chosen from the patch menu: ask its message first, or
    /// ask whether to go ahead.
    Choose(PatchChoice),
    /// The new commit's message for "New commit after source", as typed.
    CommitMessage(String),
    /// Run the rewrite, as the question asked.
    Confirm(PatchAction),
    /// Drop the custom patch. Only Kika's selection goes; the repository
    /// does not change.
    Reset,
}

impl From<CustomPatchIntent> for CommitsIntent {
    fn from(intent: CustomPatchIntent) -> Self {
        Self::CustomPatch(intent)
    }
}
