//! One list of rows with a cursor, a filter, a range and the load in
//! flight. Pure: the panels hold one and ask it where the cursor goes,
//! which rows show, which a range covers and whether a load still counts.
//! The matching rules themselves are `list_filter`'s.
//!
//! The Stash panel is the first list on it. The other panel lists still
//! carry their own copies, which differ in small ways; each is absorbed
//! as follows when it moves here, without a change in behaviour:
//! - `move_by` returns `()` on some lists and `bool` on others. Here it
//!   says whether the cursor moved; callers that ignored it write
//!   `{ list.move_by(1); }`.
//! - A list whose selection is an `Option<usize>` (tags) is `None` exactly
//!   when it is empty: `selected_row()` answers that, and an adapter can
//!   expose `rows.is_empty().then_some(selected)`.
//! - Some lists check their load's seq themselves and some only the
//!   generation, in `update.rs`. `loaded(tag, generation, ..)` checks
//!   both, so those outside checks move in here. A list that truly has no
//!   tag would get an `apply(result)` that skips the check.
//! - On a failed load some lists keep their last rows (tags) and others
//!   clear them (stash, worktrees). Here they are cleared; keeping them
//!   would be a per-row-type policy such as a `KEEP_ROWS_ON_ERROR` const
//!   on [`Row`].
//! - A list that keeps its cursor by place only, never following a row
//!   (worktrees), would use a variant, or `Key = ()`.
//! - A list whose rows live outside it (the command log, drawn from the
//!   log it is handed) is out of reach of this module.

use crate::effect::Tag;
use crate::list_filter;

/// A row a [`FilteredList`] can hold.
pub(crate) trait Row {
    /// What identifies a row across reloads: the cursor follows it and a
    /// range anchors on it.
    type Key: Clone + PartialEq + std::fmt::Debug;
    fn key(&self) -> Self::Key;
    /// The text the filter matches: the row as drawn.
    fn filter_text(&self) -> String;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FilteredList<R: Row> {
    pub rows: Vec<R>,
    pub selected: usize,
    /// What the list is narrowed to, kept for the session. Only ever
    /// matched against the rows, never passed to git.
    pub filter: String,
    /// The key of the row a range started on, so a reload keeps the range
    /// on the same row though its place moved.
    pub range_anchor: Option<R::Key>,
    /// The load in flight; only its result is applied.
    pub seq: Option<u64>,
    /// The list has answered, so an empty one means "nothing to list".
    pub loaded: bool,
    pub error: Option<String>,
}

/// By hand: a derive would ask for `R: Default`, which rows need not be.
impl<R: Row> Default for FilteredList<R> {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            selected: 0,
            filter: String::new(),
            range_anchor: None,
            seq: None,
            loaded: false,
            error: None,
        }
    }
}

impl<R: Row> FilteredList<R> {
    /// The list is loading under `seq`. What it shows stays until it lands.
    pub fn begin(&mut self, seq: u64) {
        self.seq = Some(seq);
    }

    /// Apply a result if it is the load in flight in this repository's
    /// `generation`. The cursor stays on the same row if it is still
    /// listed, else on the same place, clamped. Returns whether it was
    /// applied.
    pub fn loaded(&mut self, tag: Tag, generation: u64, result: Result<Vec<R>, String>) -> bool {
        if tag.generation != generation || self.seq != Some(tag.seq) {
            return false;
        }
        self.seq = None;
        self.loaded = true;
        let was = self.selected_row().map(Row::key);
        match result {
            Ok(rows) => {
                self.rows = rows;
                self.error = None;
            }
            Err(message) => {
                self.rows.clear();
                self.error = Some(message);
            }
        }
        self.selected = was
            .and_then(|key| self.position(&key))
            .unwrap_or(self.selected)
            .min(self.rows.len().saturating_sub(1));
        self.reconcile();
        true
    }

    /// Move the cursor over the rows the filter shows, stopping at the
    /// ends. Returns whether it moved.
    pub fn move_by(&mut self, delta: isize) -> bool {
        match list_filter::step(&self.visible(), self.selected, delta) {
            Some(next) if next != self.selected => {
                self.selected = next;
                true
            }
            _ => false,
        }
    }

    /// The indexes of the rows the filter shows, in list order.
    pub fn visible(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            return (0..self.rows.len()).collect();
        }
        let texts: Vec<String> = self.rows.iter().map(Row::filter_text).collect();
        list_filter::visible(&self.filter, texts.iter().map(String::as_str))
    }

    /// The filter hides the cursor's row, which happens only when it hides
    /// every row.
    pub fn selected_hidden(&self) -> bool {
        !self.filter.is_empty()
            && self
                .selected_row()
                .is_some_and(|row| !list_filter::matches(&self.filter, &row.filter_text()))
    }

    /// The rows the range covers, in list order; empty without one, or
    /// while its first row is hidden.
    pub fn range(&self) -> Vec<usize> {
        let anchor = self
            .range_anchor
            .as_ref()
            .and_then(|anchor| self.position(anchor));
        anchor.map_or_else(Vec::new, |anchor| {
            list_filter::range(&self.visible(), anchor, self.selected)
        })
    }

    /// Narrow the list to `text`. A changed filter drops the range, which
    /// was made over the rows shown before; says whether it did.
    pub fn set_filter(&mut self, text: String) -> bool {
        let mut cleared = false;
        if text != self.filter {
            self.filter = text;
            cleared = self.range_anchor.take().is_some();
        }
        self.reconcile();
        cleared
    }

    /// Start a range at the cursor's row, or clear the one there is. A row
    /// the filter hides starts none.
    pub fn toggle_range(&mut self) {
        if self.range_anchor.take().is_none() && !self.selected_hidden() {
            self.range_anchor = self.selected_row().map(Row::key);
        }
    }

    /// After the filter or the rows changed: a range whose first row is
    /// gone is cleared, and a hidden cursor moves to the first row shown.
    pub fn reconcile(&mut self) {
        if let Some(anchor) = &self.range_anchor
            && self.position(anchor).is_none()
        {
            self.range_anchor = None;
        }
        if self.selected_hidden()
            && let Some(&first) = self.visible().first()
        {
            self.selected = first;
        }
    }

    /// The row under the cursor; `None` exactly when the list is empty.
    pub fn selected_row(&self) -> Option<&R> {
        self.rows.get(self.selected)
    }

    fn position(&self, key: &R::Key) -> Option<usize> {
        self.rows.iter().position(|row| &row.key() == key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A row keyed by `id`, drawn as `text`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Item {
        id: u32,
        text: &'static str,
    }

    impl Row for Item {
        type Key = u32;
        fn key(&self) -> u32 {
            self.id
        }
        fn filter_text(&self) -> String {
            self.text.into()
        }
    }

    const GENERATION: u64 = 3;

    fn tag(seq: u64) -> Tag {
        Tag {
            generation: GENERATION,
            seq,
            user: false,
        }
    }

    /// Rows keyed by `ids`, all drawn alike.
    fn ids(ids: &[u32]) -> Vec<Item> {
        ids.iter().map(|&id| Item { id, text: "row" }).collect()
    }

    /// Rows keyed by their place, drawn as `texts`.
    fn texts(texts: &[&'static str]) -> Vec<Item> {
        texts
            .iter()
            .zip(0..)
            .map(|(&text, id)| Item { id, text })
            .collect()
    }

    fn reload(list: &mut FilteredList<Item>, seq: u64, rows: Vec<Item>) {
        list.begin(seq);
        assert!(list.loaded(tag(seq), GENERATION, Ok(rows)));
    }

    fn list(rows: Vec<Item>) -> FilteredList<Item> {
        let mut list = FilteredList::default();
        reload(&mut list, 1, rows);
        list
    }

    #[test]
    fn a_load_under_a_stale_seq_is_rejected() {
        let mut list = FilteredList::default();
        list.begin(5);
        assert!(!list.loaded(tag(4), GENERATION, Ok(ids(&[1]))));
        assert!(!list.loaded);
        assert_eq!(list.seq, Some(5), "still in flight");
    }

    #[test]
    fn a_load_from_another_generation_is_rejected() {
        let mut list = FilteredList::default();
        list.begin(5);
        assert!(!list.loaded(tag(5), GENERATION + 1, Ok(ids(&[1]))));
        assert!(!list.loaded);
        assert!(list.rows.is_empty());
        assert_eq!(list.seq, Some(5), "still in flight");
    }

    #[test]
    fn the_load_in_flight_is_applied_once() {
        let mut list = FilteredList::default();
        list.begin(5);
        assert!(list.loaded(tag(5), GENERATION, Ok(ids(&[1, 2]))));
        assert_eq!((list.rows.len(), list.loaded, list.seq), (2, true, None));
        assert!(
            !list.loaded(tag(5), GENERATION, Ok(vec![])),
            "applied twice"
        );
        assert_eq!(list.rows.len(), 2);
    }

    #[test]
    fn a_reload_keeps_the_cursor_on_the_same_row_or_clamps_it() {
        let mut list = list(ids(&[1, 2, 3]));
        assert!(list.move_by(1));
        // A new row on top moves `2` down one.
        reload(&mut list, 2, ids(&[0, 1, 2, 3]));
        assert_eq!(list.selected, 2);
        // `2` is gone: the same place.
        reload(&mut list, 3, ids(&[0, 1, 3]));
        assert_eq!(list.selected, 2);
        // Fewer rows than the place: the last.
        reload(&mut list, 4, ids(&[0]));
        assert_eq!(list.selected, 0);
        reload(&mut list, 5, vec![]);
        assert_eq!((list.selected, list.selected_row()), (0, None));
    }

    #[test]
    fn a_failed_load_clears_the_rows_and_keeps_the_error() {
        let mut list = list(ids(&[1]));
        list.begin(2);
        assert!(list.loaded(tag(2), GENERATION, Err("bad".into())));
        assert_eq!((list.loaded, list.error.as_deref()), (true, Some("bad")));
        assert!(list.rows.is_empty());
        reload(&mut list, 3, ids(&[1]));
        assert_eq!(list.error, None, "a good load clears it");
    }

    #[test]
    fn the_cursor_stops_at_the_ends_and_says_whether_it_moved() {
        let mut list = list(ids(&[1, 2]));
        assert!(!list.move_by(-1));
        assert_eq!(list.selected, 0);
        assert!(list.move_by(5));
        assert_eq!(list.selected, 1);
        assert!(!list.move_by(1));
        assert!(!FilteredList::<Item>::default().move_by(1), "empty");
    }

    #[test]
    fn the_cursor_skips_hidden_rows_and_a_hidden_one_moves_to_the_first_shown() {
        let mut list = list(texts(&["keep 0", "drop 1", "keep 2", "drop 3", "keep 4"]));
        list.move_by(1);
        assert_eq!(list.selected, 1);
        list.set_filter("keep".into());
        assert_eq!(list.visible(), [0, 2, 4]);
        assert_eq!(list.selected, 0, "the hidden cursor moved");
        list.move_by(1);
        assert_eq!(list.selected, 2);
        list.move_by(5);
        assert_eq!(list.selected, 4);
        list.set_filter("zzz".into());
        assert!(list.selected_hidden());
        assert_eq!(list.selected, 4, "nowhere to go");
        assert!(!list.move_by(-1));
        assert_eq!(list.selected, 4);
        list.set_filter(String::new());
        assert_eq!(list.visible(), [0, 1, 2, 3, 4]);
        assert!(!list.selected_hidden());
    }

    #[test]
    fn a_changed_filter_clears_the_range_and_says_so() {
        let mut list = list(texts(&["a", "b", "c"]));
        list.toggle_range();
        assert!(!list.set_filter(String::new()), "an unchanged filter");
        assert!(list.range_anchor.is_some());
        assert!(list.set_filter("b".into()));
        assert_eq!(list.range_anchor, None);
        assert!(!list.set_filter("c".into()), "no range to clear");
    }

    #[test]
    fn a_range_spans_only_shown_rows() {
        let mut list = list(texts(&["a x", "b", "c x", "d x"]));
        list.move_by(1);
        list.toggle_range();
        list.move_by(2);
        assert_eq!(list.range(), [1, 2, 3]);
        list.move_by(-3);
        assert_eq!(list.range(), [0, 1]);
        list.toggle_range();
        assert_eq!((list.range_anchor, list.range()), (None, vec![]));
        // A range made under a filter covers only the rows it shows.
        list.set_filter("x".into());
        list.toggle_range();
        list.move_by(2);
        assert_eq!(list.range(), [0, 2, 3]);
    }

    #[test]
    fn a_hidden_row_starts_no_range() {
        let mut list = list(texts(&["a", "b"]));
        list.set_filter("zzz".into());
        assert!(list.selected_hidden());
        list.toggle_range();
        assert_eq!(list.range_anchor, None);
    }

    #[test]
    fn a_reload_keeps_the_range_on_the_same_row_or_clears_it_when_gone() {
        let mut list = list(ids(&[1, 2, 3]));
        list.move_by(1);
        list.toggle_range();
        list.move_by(1);
        assert_eq!(list.range(), [1, 2]);
        // A new row on top moves every row down.
        reload(&mut list, 2, ids(&[0, 1, 2, 3]));
        assert_eq!(list.range_anchor, Some(2));
        assert_eq!(list.range(), [2, 3]);
        // The anchor's row is gone.
        reload(&mut list, 3, ids(&[0, 1, 3]));
        assert_eq!(list.range_anchor, None);
        assert!(list.range().is_empty());
    }

    #[test]
    fn a_reload_under_a_filter_lands_on_a_shown_row() {
        let mut list = list(texts(&["x", "keep"]));
        list.set_filter("keep".into());
        assert_eq!(list.selected, 1);
        // Its row is gone, and the same place is now hidden.
        let rows = vec![
            Item {
                id: 10,
                text: "keep",
            },
            Item { id: 11, text: "x" },
        ];
        reload(&mut list, 2, rows);
        assert_eq!(list.selected, 0);
    }
}
