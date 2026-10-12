//! The Reflog tab's state: `HEAD`'s reflog as last read, its cursor, its
//! filter, and an undo or redo waiting for a fresh read to plan from.
//! Pure; `AppState` turns its answers into effects.

use crate::git::reflog::{Reflog, ReflogEntry};
use crate::git::undo::Direction;
use crate::list_filter;

/// Hex digits of an entry's id, as the tab draws it.
pub(crate) const SHORT_OID: usize = 7;

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
    /// Narrows the rows shown. Only ever matched, never passed to git, and
    /// undo or redo plans from every entry whatever it hides.
    pub filter: String,
}

impl ReflogList {
    /// Move the cursor by `delta` among the rows shown, stopping at either
    /// end.
    pub fn move_by(&mut self, delta: isize) {
        if let Some(next) = list_filter::step(&self.visible(), self.selected, delta) {
            self.selected = next;
        }
    }

    /// The indexes of the entries the filter shows, in list order.
    pub fn visible(&self) -> Vec<usize> {
        let entries = self.entries();
        if self.filter.is_empty() {
            return (0..entries.len()).collect();
        }
        let texts: Vec<String> = entries.iter().map(filter_text).collect();
        list_filter::visible(&self.filter, texts.iter().map(String::as_str))
    }

    /// The filter hides the cursor's row, which happens only when it hides
    /// every row.
    pub fn selected_hidden(&self) -> bool {
        !self.filter.is_empty()
            && self
                .entries()
                .get(self.selected)
                .is_some_and(|e| !list_filter::matches(&self.filter, &filter_text(e)))
    }

    /// Put a cursor the filter hides on the first row shown; leave it be
    /// when no row is.
    pub fn reconcile(&mut self) {
        if self.selected_hidden()
            && let Some(&first) = self.visible().first()
        {
            self.selected = first;
        }
    }

    /// Narrow the rows shown to those matching `text`.
    pub fn set_filter(&mut self, text: String) {
        self.filter = text;
        self.reconcile();
    }

    fn entries(&self) -> &[ReflogEntry] {
        self.data.as_ref().map_or(&[], |d| d.entries.as_slice())
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
                self.selected = self.selected.min(reflog.entries.len().saturating_sub(1));
                self.data = Some(reflog);
                self.error = None;
                self.reconcile();
            }
            Err(message) => self.error = Some(format!("Could not read the reflog: {message}")),
        }
        true
    }
}

/// An entry's id cut to [`SHORT_OID`] digits, or `?` when it is not hex.
pub(crate) fn short_id(entry: &ReflogEntry) -> String {
    entry.oid.as_ref().map_or_else(
        || "?".to_string(),
        |id| id.as_str().chars().take(SHORT_OID).collect(),
    )
}

/// The text the filter matches a row by: its short id and its message, as
/// drawn. `HEAD@{n}` is not drawn, so it is not matched.
fn filter_text(entry: &ReflogEntry) -> String {
    format!("{} {}", short_id(entry), entry.display())
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

    fn loaded_three() -> ReflogList {
        let mut list = ReflogList {
            seq: Some(1),
            ..ReflogList::default()
        };
        assert!(list.loaded(1, Ok(three())));
        list
    }

    #[test]
    fn the_filter_matches_the_short_id_or_the_message() {
        // second (ccccccc), first (bbbbbbb), root (aaaaaaa).
        let mut list = loaded_three();
        assert_eq!(list.visible(), [0, 1, 2]);
        list.set_filter("bbb".into());
        assert_eq!(list.visible(), [1]);
        list.set_filter("root".into());
        assert_eq!(list.visible(), [2]);
        list.set_filter("commit".into());
        assert_eq!(list.visible(), [0, 1, 2]);
        list.set_filter("HEAD@".into());
        assert!(list.visible().is_empty(), "HEAD@{{n}} is not drawn");
    }

    #[test]
    fn the_cursor_moves_over_shown_rows_only() {
        let mut list = loaded_three();
        list.set_filter("s".into()); // second, first; not root
        assert_eq!(list.visible(), [0, 1]);
        list.move_by(5);
        assert_eq!(list.selected, 1);
        list.set_filter("oo".into()); // second, root: two o each
        assert_eq!(list.visible(), [0, 2]);
        assert_eq!(
            list.selected, 0,
            "a hidden cursor goes to the first row shown"
        );
        list.move_by(1);
        assert_eq!(list.selected, 2, "steps over the hidden row");
        list.move_by(-1);
        assert_eq!(list.selected, 0);
    }

    #[test]
    fn a_filter_that_hides_every_row_leaves_the_cursor_be() {
        let mut list = loaded_three();
        list.move_by(1);
        list.set_filter("zzz".into());
        assert!(list.visible().is_empty());
        assert!(list.selected_hidden());
        assert_eq!(list.selected, 1);
        list.move_by(1);
        assert_eq!(list.selected, 1);
        list.set_filter(String::new());
        assert!(!list.selected_hidden());
    }

    #[test]
    fn a_reload_with_a_filter_keeps_the_cursor_on_a_shown_row() {
        let mut list = loaded_three();
        list.set_filter("root".into());
        assert_eq!(list.selected, 2);
        // A new commit lands on top: root is now index 3.
        let mut more = three();
        more.entries.insert(0, more.entries[0].clone());
        list.seq = Some(2);
        assert!(list.loaded(2, Ok(more)));
        assert_eq!(list.visible(), [3]);
        assert_eq!(list.selected, 3);
        // A shorter read clamps, then lands on a shown row.
        list.set_filter("second".into());
        list.selected = 1;
        list.seq = Some(3);
        let mut fewer = three();
        fewer.entries.truncate(2);
        assert!(list.loaded(3, Ok(fewer)));
        assert_eq!(list.visible(), [0]);
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
