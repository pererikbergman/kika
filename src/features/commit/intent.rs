//! The commit popup's intents. Core wraps them in one variant,
//! `AppIntent::Commit`, and hands them to this feature's `update`
//! (see `docs/adr/0002-nested-intents.md`).

use crate::effect::Tag;
use crate::op::GitOp;
use crate::prompts::PromptFacts;

// `Probed` carries the commit op as it did in `AppIntent`, which wraps
// this enum and is larger still, so boxing it would save nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommitIntent {
    /// Open the commit popup from the Files panel.
    Open,
    /// Nothing was staged: yes, stage everything and commit.
    ConfirmStageAll,
    /// Close the commit popup; the index is left as it is. A typed
    /// message is only thrown away after `DiscardCommit`.
    Cancel,
    /// Yes, throw the typed message away.
    Discard,
    /// Back to the message instead of discarding it.
    Resume,
    /// Commit with the typed message.
    Submit,
    /// Ctrl+C in the commit popup: cancel while typing (asking before a
    /// typed message is lost), quit while git runs.
    Interrupt,
    /// Move the cursor between the subject and the body.
    ToggleField,
    /// Close the commit error and go back to the kept message.
    DismissError,
    /// Open the commit popup to amend the last commit, its message filled in.
    OpenAmend,
    /// `HEAD`'s message was read for amending; `tag` says which load.
    HeadMessageLoaded {
        tag: Tag,
        result: Result<String, String>,
    },
    /// Turn skipping the commit hooks on or off for this commit.
    ToggleNoVerify,
    /// Write the message in the user's editor, then commit with it.
    EditExternally,
    /// The editor opened by `EditCommitExternally` has closed: the message
    /// to commit, `None` if the user emptied it.
    MessageEdited {
        result: Result<Option<String>, String>,
    },
    /// The config and hooks a commit depends on were read; `facts` is
    /// `None` when they could not be. Decides where the commit runs.
    Probed {
        tag: Tag,
        op: GitOp,
        facts: Option<PromptFacts>,
    },
}
