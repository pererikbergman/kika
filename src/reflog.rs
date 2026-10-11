//! The Reflog tab's state: `HEAD`'s reflog as last read, its cursor, and
//! an undo or redo waiting for a fresh read to plan from. Pure; `AppState`
//! turns its answers into effects.

use crate::git::reflog::Reflog;
use crate::git::undo::Direction;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReflogList {
    /// `None` until the first read lands.
    pub data: Option<Reflog>,
    pub error: Option<String>,
    pub selected: usize,
    /// The read in flight; only its result is applied.
    pub seq: Option<u64>,
    /// An undo or redo is planned once the read in flight lands, so a plan
    /// never comes from a list older than the key press.
    pub planning: Option<Direction>,
}

impl ReflogList {
    pub fn move_by(&mut self, delta: isize) {
        let len = self.data.as_ref().map_or(0, |d| d.entries.len());
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(len.saturating_sub(1));
    }

    /// Apply a read if it is the one in flight, keeping the cursor in the
    /// list. A failure keeps what was shown, since it is still the best
    /// known, and says why beside it. Returns whether it was applied.
    pub fn loaded(&mut self, seq: u64, result: Result<Reflog, String>) -> bool {
        if self.seq != Some(seq) {
            return false;
        }
        self.seq = None;
        match result {
            Ok(reflog) => {
                self.data = Some(reflog);
                self.error = None;
                self.move_by(0);
            }
            Err(message) => self.error = Some(format!("Could not read the reflog: {message}")),
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::reflog::parse_records;

    fn three() -> Reflog {
        Reflog {
            entries: parse_records(include_bytes!("git/fixtures/reflog/commit.bin")),
            ..Reflog::default()
        }
    }

    #[test]
    fn the_cursor_stays_in_the_list() {
        let mut list = ReflogList::default();
        list.move_by(1);
        assert_eq!(list.selected, 0);
        list.seq = Some(1);
        assert!(list.loaded(1, Ok(three())));
        list.move_by(5);
        assert_eq!(list.selected, 2);
        list.move_by(-1);
        assert_eq!(list.selected, 1);
        list.seq = Some(2);
        list.loaded(2, Ok(Reflog::default()));
        assert_eq!(list.selected, 0);
    }

    #[test]
    fn only_the_read_in_flight_is_applied_and_a_failure_keeps_the_list() {
        let mut list = ReflogList {
            seq: Some(2),
            ..ReflogList::default()
        };
        assert!(!list.loaded(1, Ok(three())));
        assert_eq!(list.data, None);
        assert!(list.loaded(2, Ok(three())));
        list.seq = Some(3);
        assert!(list.loaded(3, Err("fatal: x".into())));
        assert_eq!(list.data, Some(three()));
        assert_eq!(
            list.error.as_deref(),
            Some("Could not read the reflog: fatal: x")
        );
        assert_eq!(list.seq, None);
    }
}
