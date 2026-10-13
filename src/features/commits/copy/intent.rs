//! Copying commits' intents. They nest in `CommitsIntent::Copy`, so
//! core's one `AppIntent::Commits` arm reaches this feature's `update`
//! through the commits feature (see `docs/adr/0002-nested-intents.md`).

use crate::clipboard::CopyWhat;
use crate::effect::Tag;
use crate::features::commits::CommitsIntent;
use crate::git::commit_files::CommitId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CopyIntent {
    /// Add the selected commit to the copied set, or take it out.
    Toggle,
    /// Empty the copied set.
    Clear,
    /// Cherry-pick the copied commits onto the current branch.
    Paste,
    /// Put the selected commit's hash, subject or message on the system
    /// clipboard.
    CopyText(CopyWhat),
    /// A commit's full message was read to copy it; `tag` says which read.
    MessageLoaded {
        tag: Tag,
        oid: CommitId,
        result: Result<String, String>,
    },
}

impl From<CopyIntent> for CommitsIntent {
    fn from(intent: CopyIntent) -> Self {
        Self::Copy(intent)
    }
}
