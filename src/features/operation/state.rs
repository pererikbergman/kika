//! The operation's state: the check for work since the stop that a Skip
//! waits on.

use crate::keymap::KeyScope;
use crate::popup::Popup;

/// The stop-work check in flight, and where the user was when it started.
/// Its answer only asks or skips if they are still there, so a key meant
/// for something else cannot answer the question, and one check runs at a
/// time, so two quick presses never stack two questions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StopWorkCheck {
    pub seq: u64,
    pub popups: Vec<Popup>,
    pub focus: KeyScope,
}
