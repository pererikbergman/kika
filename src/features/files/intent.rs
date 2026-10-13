//! The Files panel's intents. Core wraps them in one variant,
//! `AppIntent::Files`, and hands them to this feature's `update`
//! (see `docs/adr/0002-nested-intents.md`).

use super::IgnoreAdded;
use crate::effect::Tag;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FilesIntent {
    /// Move the file selection down or up.
    Next,
    Prev,
    /// Stage the selected file, or unstage it when it is fully staged.
    ToggleStage,
    /// Stage everything, or unstage everything when nothing is left to stage.
    ToggleStageAll,
    /// Select the first file whose path contains this text, ignoring case.
    GoToFile(String),
    /// Ask which pattern to add to `.gitignore` for the selected
    /// untracked file, offering the one that matches exactly it.
    OpenIgnore,
    /// The pattern the `.gitignore` prompt was accepted with.
    IgnorePatternAnswered(String),
    /// The pattern was added to `.gitignore`, or was there already, or
    /// why not. The text is the pattern as shown.
    IgnoreWritten {
        tag: Tag,
        result: Result<IgnoreAdded, String>,
    },
}
