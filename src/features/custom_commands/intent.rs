//! The Custom commands feature's intents. Core wraps them in one variant,
//! `AppIntent::CustomCommands`, and hands them to this feature's `update`
//! (see `docs/adr/0002-nested-intents.md`).

use super::{CommandFailure, OutputMode};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CustomCommandsIntent {
    /// The answer to a custom command's current prompt.
    PromptAnswered(String),
    /// A custom command finished. `Ok` holds the captured output, empty
    /// unless `output` is `Popup`; `Err` says how it failed. Text is
    /// terminal-safe.
    Finished {
        title: String,
        output: OutputMode,
        result: Result<String, CommandFailure>,
    },
    /// Stop the captured command that is running, if one is.
    Cancel,
}
