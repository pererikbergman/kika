//! Comparing commits' intents. They nest in `CommitsIntent::Compare`, so
//! core's one `AppIntent::Commits` arm reaches this feature's `update`
//! through the commits feature (see `docs/adr/0002-nested-intents.md`).

use crate::effect::Tag;
use crate::features::commits::CommitsIntent;
use kika_git::commit_files::CommitRange;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CompareIntent {
    /// Mark the selected commit or local branch to diff against, or clear
    /// the mark when it is already there.
    ToggleMark,
    /// Compare the diff mark with the selected local branch.
    Open,
    /// Both ends of a comparison were read as commits; `tag` says which.
    Resolved {
        tag: Tag,
        result: Result<CommitRange, String>,
    },
}

impl From<CompareIntent> for CommitsIntent {
    fn from(intent: CompareIntent) -> Self {
        Self::Compare(intent)
    }
}
