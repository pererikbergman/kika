//! The Command log's intents. Core wraps them in one variant,
//! `AppIntent::CommandLog`, and hands them to this feature's `update`
//! (see `docs/adr/0002-nested-intents.md`).

use kika_git::recording::CommandEvent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommandLogIntent {
    /// A git command started or ended; the runner reports every one.
    Logged(CommandEvent),
    /// Show the git commands Kika ran this session.
    Open,
    /// Move the Command log cursor a row down.
    Next,
    /// Move the Command log cursor a row up.
    Prev,
    /// Move the Command log cursor a page down.
    PageDown,
    /// Move the Command log cursor a page up.
    PageUp,
}
