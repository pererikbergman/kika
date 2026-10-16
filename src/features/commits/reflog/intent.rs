//! The reflog's intents. They nest in `CommitsIntent::Reflog`, so core's
//! one `AppIntent::Commits` arm reaches this feature's `update` through
//! the commits feature (see `docs/adr/0002-nested-intents.md`).

use super::Reflog;
use crate::effect::Tag;
use crate::features::commits::CommitsIntent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReflogIntent {
    /// Move the Reflog tab's selection down or up.
    Next,
    Prev,
    /// Move the cursor by this many rows, clamped at both ends: a click
    /// or a wheel notch.
    MoveBy(isize),
    /// `HEAD`'s reflog was read; `tag` says which load.
    Loaded {
        tag: Tag,
        result: Result<Reflog, String>,
    },
    /// Reverse the last action, or reapply the last one undone. Each reads
    /// the reflog afresh, then asks before anything moves.
    Undo,
    Redo,
    /// Run the undo or redo the open question describes.
    ConfirmTravel,
}

impl From<ReflogIntent> for CommitsIntent {
    fn from(intent: ReflogIntent) -> Self {
        Self::Reflog(intent)
    }
}
