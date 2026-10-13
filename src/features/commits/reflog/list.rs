//! The Reflog tab's state: `HEAD`'s reflog as last read (a
//! `FilteredList`, which owns its entries, error, cursor, filter and read
//! in flight), where `HEAD` was when it was read, and an undo or redo
//! waiting for a fresh read to plan from. Pure; `AppState` turns its
//! answers into effects.

use super::git::{Reflog, ReflogEntry};
use super::undo::Direction;
use crate::effect::Tag;
use crate::filtered_list::{FilteredList, Row};
use crate::git::commit_files::CommitId;

/// Hex digits of an entry's id, as the tab draws it.
pub(crate) const SHORT_OID: usize = 7;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReflogList {
    /// The entries, newest first, with the cursor, filter and read in
    /// flight. A failed read keeps the entries shown, since they are
    /// still the best known, and a reload keeps the cursor at its place:
    /// each operation adds an entry on top. The filter is only ever
    /// matched, never passed to git, and undo or redo plans from every
    /// entry whatever it hides.
    pub list: FilteredList<ReflogEntry>,
    /// The rest of the last good read; `None` until one lands.
    pub head: Option<ReflogHead>,
    /// An undo or redo is planned once the read in flight lands, so a plan
    /// never comes from a list older than the key press.
    pub planning: Option<Direction>,
}

/// What a read says beside the entries: where `HEAD` is and its branch's
/// own reflog, which the undo planner needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReflogHead {
    pub head: Option<CommitId>,
    pub head_ref: Option<Vec<u8>>,
    pub branch_entries: Vec<ReflogEntry>,
}

impl Row for ReflogEntry {
    type Key = Option<CommitId>;
    const KEEP_ROWS_ON_ERROR: bool = true;
    const FOLLOW_ROW: bool = false;

    fn key(&self) -> Option<CommitId> {
        self.oid.clone()
    }

    /// Its short id and its message, as drawn. `HEAD@{n}` is not drawn,
    /// so it is not matched.
    fn filter_text(&self) -> String {
        format!("{} {}", short_id(self), self.display())
    }
}

impl ReflogList {
    /// Apply a read if it is the one in flight in this repository's
    /// `generation`, saying why a failure failed. Returns whether it was
    /// applied.
    pub fn read(&mut self, tag: Tag, generation: u64, result: Result<Reflog, String>) -> bool {
        let (entries, head) = match result {
            Ok(reflog) => (
                Ok(reflog.entries),
                Some(ReflogHead {
                    head: reflog.head,
                    head_ref: reflog.head_ref,
                    branch_entries: reflog.branch_entries,
                }),
            ),
            Err(message) => (Err(format!("Could not read the reflog: {message}")), None),
        };
        if !self.list.loaded(tag, generation, entries) {
            return false;
        }
        if head.is_some() {
            self.head = head;
        }
        true
    }

    /// With no commits there is no reflog: an empty list, read, and
    /// nothing in flight. The filter is kept.
    pub fn unborn(&mut self) {
        self.list.rows.clear();
        self.list.loaded = true;
        self.list.error = None;
        self.list.seq = None;
        self.list.selected = 0;
        self.head = Some(ReflogHead::default());
    }

    /// The last good read whole, for the undo planner; `None` before one.
    pub fn reflog(&self) -> Option<Reflog> {
        let head = self.head.as_ref()?;
        Some(Reflog {
            head: head.head.clone(),
            head_ref: head.head_ref.clone(),
            entries: self.list.rows.clone(),
            branch_entries: head.branch_entries.clone(),
        })
    }
}

/// An entry's id cut to [`SHORT_OID`] digits, or `?` when it is not hex.
pub(crate) fn short_id(entry: &ReflogEntry) -> String {
    entry.oid.as_ref().map_or_else(
        || "?".to_string(),
        |id| id.as_str().chars().take(SHORT_OID).collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::commits::reflog::git::parse_records;

    fn tag(seq: u64) -> Tag {
        Tag {
            generation: 0,
            seq,
            user: false,
        }
    }

    fn three() -> Reflog {
        Reflog {
            entries: parse_records(include_bytes!("../../../git/fixtures/reflog/commit.bin")),
            ..Reflog::default()
        }
    }

    #[test]
    fn the_cursor_stays_in_the_list() {
        let mut list = ReflogList::default();
        list.list.move_by(1);
        assert_eq!(list.list.selected, 0);
        list.list.begin(1);
        assert!(list.read(tag(1), 0, Ok(three())));
        list.list.move_by(5);
        assert_eq!(list.list.selected, 2);
        list.list.move_by(-1);
        assert_eq!(list.list.selected, 1);
        list.list.begin(2);
        list.read(tag(2), 0, Ok(Reflog::default()));
        assert_eq!(list.list.selected, 0);
    }

    fn loaded_three() -> ReflogList {
        let mut list = ReflogList::default();
        list.list.begin(1);
        assert!(list.read(tag(1), 0, Ok(three())));
        list
    }

    #[test]
    fn the_filter_matches_the_short_id_or_the_message() {
        // second (ccccccc), first (bbbbbbb), root (aaaaaaa).
        let mut list = loaded_three();
        assert_eq!(list.list.visible(), [0, 1, 2]);
        list.list.set_filter("bbb".into());
        assert_eq!(list.list.visible(), [1]);
        list.list.set_filter("root".into());
        assert_eq!(list.list.visible(), [2]);
        list.list.set_filter("commit".into());
        assert_eq!(list.list.visible(), [0, 1, 2]);
        list.list.set_filter("HEAD@".into());
        assert!(list.list.visible().is_empty(), "HEAD@{{n}} is not drawn");
    }

    #[test]
    fn the_cursor_moves_over_shown_rows_only() {
        let mut list = loaded_three();
        list.list.set_filter("s".into()); // second, first; not root
        assert_eq!(list.list.visible(), [0, 1]);
        list.list.move_by(5);
        assert_eq!(list.list.selected, 1);
        list.list.set_filter("oo".into()); // second, root: two o each
        assert_eq!(list.list.visible(), [0, 2]);
        assert_eq!(
            list.list.selected, 0,
            "a hidden cursor goes to the first row shown"
        );
        list.list.move_by(1);
        assert_eq!(list.list.selected, 2, "steps over the hidden row");
        list.list.move_by(-1);
        assert_eq!(list.list.selected, 0);
    }

    #[test]
    fn a_filter_that_hides_every_row_leaves_the_cursor_be() {
        let mut list = loaded_three();
        list.list.move_by(1);
        list.list.set_filter("zzz".into());
        assert!(list.list.visible().is_empty());
        assert!(list.list.selected_hidden());
        assert_eq!(list.list.selected, 1);
        list.list.move_by(1);
        assert_eq!(list.list.selected, 1);
        list.list.set_filter(String::new());
        assert!(!list.list.selected_hidden());
    }

    #[test]
    fn a_reload_with_a_filter_keeps_the_cursor_on_a_shown_row() {
        let mut list = loaded_three();
        list.list.set_filter("root".into());
        assert_eq!(list.list.selected, 2);
        // A new commit lands on top: root is now index 3.
        let mut more = three();
        more.entries.insert(0, more.entries[0].clone());
        list.list.begin(2);
        assert!(list.read(tag(2), 0, Ok(more)));
        assert_eq!(list.list.visible(), [3]);
        assert_eq!(list.list.selected, 3);
        // A shorter read clamps, then lands on a shown row.
        list.list.set_filter("second".into());
        list.list.selected = 1;
        list.list.begin(3);
        let mut fewer = three();
        fewer.entries.truncate(2);
        assert!(list.read(tag(3), 0, Ok(fewer)));
        assert_eq!(list.list.visible(), [0]);
        assert_eq!(list.list.selected, 0);
    }

    #[test]
    fn only_the_read_in_flight_is_applied_and_a_failure_keeps_the_list() {
        let mut list = ReflogList::default();
        list.list.begin(2);
        assert!(!list.read(tag(1), 0, Ok(three())));
        assert_eq!(list.reflog(), None);
        assert!(list.read(tag(2), 0, Ok(three())));
        list.list.begin(3);
        assert!(list.read(tag(3), 0, Err("fatal: x".into())));
        assert_eq!(list.reflog(), Some(three()));
        assert_eq!(
            list.list.error.as_deref(),
            Some("Could not read the reflog: fatal: x")
        );
        assert_eq!(list.list.seq, None);
    }
}
