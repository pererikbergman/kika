//! Answering a question that moves `HEAD`: a bisect or another operation
//! may have started outside Kika while it was open, so `y` asks the guard
//! the opening key asked once more before anything runs.

use super::AppState;
use crate::git::ops::GitOp;

impl AppState {
    /// Whether the guard the key that opened the question uses for `op`
    /// now refuses it; if so, its reason goes in the bar. A reset waits
    /// for any operation in progress, a checkout only for a bisect, as
    /// their keys do.
    pub(super) fn confirm_refused(&mut self, op: &GitOp) -> bool {
        let reason = match op {
            GitOp::Reset { .. } => crate::keymap::in_progress_disabled(self),
            GitOp::CheckoutCommit(_) => crate::bisect::head_move_disabled(self),
            _ => None,
        };
        let Some(reason) = reason else {
            return false;
        };
        self.notice = Some(reason);
        true
    }
}
