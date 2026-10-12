//! The state a custom command keeps while its prompts are asked.

use super::command::{CustomCommand, Target};

/// A custom command waiting for its prompts. It holds a copy of the
/// command and the selection taken when its key was pressed, so a config
/// reload or a refresh while the user types cannot change what runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCommand {
    pub command: CustomCommand,
    pub target: Target,
    /// One per prompt answered so far, in order.
    pub answers: Vec<String>,
}
