//! The bisect's intents. They nest in `CommitsIntent::Bisect`, so core's
//! one `AppIntent::Commits` arm reaches this feature's `update` through
//! the commits feature (see `docs/adr/0002-nested-intents.md`).

use crate::features::commits::CommitsIntent;
use crate::git::bisect::BisectMark;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BisectIntent {
    /// Mark the selected commit in the Commits panel; with no bisect in
    /// progress, the mark starts one.
    MarkSelected(BisectMark),
    /// Mark the commit git checked out to test.
    MarkHead(BisectMark),
    /// Ask before resetting the bisect (the Bisect menu's reset item).
    AskReset,
    /// End the bisect, as the question asked.
    ConfirmReset,
}

impl From<BisectIntent> for CommitsIntent {
    fn from(intent: BisectIntent) -> Self {
        Self::Bisect(intent)
    }
}
