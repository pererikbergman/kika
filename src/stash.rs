//! The Stash panel: the stash list, its cursor, its filter and range, and
//! an entry opened to its files. Pure; `AppState` turns its answers into
//! effects.

use crate::git::commit_files::CommitId;
use crate::git::ops::StashMode;
use crate::git::path::RepoPath;
use crate::git::stash::{StashEntry, StashFile, StashRef};
use crate::list_filter;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StashList {
    pub entries: Vec<StashEntry>,
    pub selected: usize,
    /// The load in flight; only its result is applied.
    pub seq: Option<u64>,
    /// The list has answered, so an empty one means "no stashes".
    pub loaded: bool,
    pub error: Option<String>,
    /// An entry opened to its files, shown in place of the list.
    /// `selected` stays where it was, so closing it returns to the entry.
    pub open: Option<StashFilesList>,
    /// The entry a rename prompt is about, kept from when it opened.
    pub pending: Option<StashRef>,
    /// What the list is narrowed to, kept for the session. Only ever
    /// matched against the rows, never passed to git.
    pub filter: String,
    /// The id of the entry a range selection started on, so a reload
    /// keeps the range on the same stash though its number moved.
    pub range_anchor: Option<CommitId>,
}

impl StashList {
    /// The list is loading under `seq`. What it shows stays until it lands.
    pub fn begin(&mut self, seq: u64) {
        self.seq = Some(seq);
    }

    /// Apply a result if it is the load in flight. The cursor stays on the
    /// same stash if it is still listed, else on the same place, clamped.
    /// Returns whether it was applied.
    pub fn loaded(&mut self, seq: u64, result: Result<Vec<StashEntry>, String>) -> bool {
        if self.seq != Some(seq) {
            return false;
        }
        self.seq = None;
        self.loaded = true;
        let was = self.selected_entry().map(|e| e.oid.clone());
        match result {
            Ok(entries) => {
                self.entries = entries;
                self.error = None;
            }
            Err(message) => {
                self.entries.clear();
                self.error = Some(message);
            }
        }
        self.selected = was
            .and_then(|oid| self.entries.iter().position(|e| e.oid == oid))
            .unwrap_or(self.selected)
            .min(self.entries.len().saturating_sub(1));
        self.reconcile();
        true
    }

    /// Move the cursor over the rows the filter shows, stopping at the
    /// ends.
    pub fn move_by(&mut self, delta: isize) {
        if let Some(next) = list_filter::step(&self.visible(), self.selected, delta) {
            self.selected = next;
        }
    }

    /// The indexes of the entries the filter shows, in list order.
    pub fn visible(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            return (0..self.entries.len()).collect();
        }
        let texts: Vec<String> = self.entries.iter().map(filter_text).collect();
        list_filter::visible(&self.filter, texts.iter().map(String::as_str))
    }

    /// The filter hides the cursor's entry, which happens only when it
    /// hides every entry.
    pub fn selected_hidden(&self) -> bool {
        !self.filter.is_empty()
            && self
                .selected_entry()
                .is_some_and(|e| !list_filter::matches(&self.filter, &filter_text(e)))
    }

    /// The entries the range covers, in list order; empty without one, or
    /// while its first entry is hidden.
    pub fn range(&self) -> Vec<usize> {
        let anchor = self
            .range_anchor
            .as_ref()
            .and_then(|anchor| self.entries.iter().position(|e| &e.oid == anchor));
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

    /// Start a range at the cursor's entry, or clear the one there is. An
    /// entry the filter hides starts none.
    pub fn toggle_range(&mut self) {
        if self.range_anchor.take().is_none() && !self.selected_hidden() {
            self.range_anchor = self.selected_entry().map(|e| e.oid.clone());
        }
    }

    /// After the filter or the list changed: a range whose first entry is
    /// gone is cleared, and a hidden cursor moves to the first row shown.
    pub fn reconcile(&mut self) {
        if let Some(anchor) = &self.range_anchor
            && !self.entries.iter().any(|e| &e.oid == anchor)
        {
            self.range_anchor = None;
        }
        if self.selected_hidden()
            && let Some(&first) = self.visible().first()
        {
            self.selected = first;
        }
    }

    pub fn selected_entry(&self) -> Option<&StashEntry> {
        self.entries.get(self.selected)
    }
}

/// The text the filter matches an entry by: its row as drawn,
/// `stash@{N}  <subject>`. The subject was made safe when read.
pub(crate) fn filter_text(entry: &StashEntry) -> String {
    format!("{}  {}", entry.stash_ref().arg(), entry.subject)
}

/// One stash entry's files, like a commit's in the Commits panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashFilesList {
    /// For the panel title.
    pub stash: StashRef,
    pub files: Vec<StashFile>,
    pub selected: usize,
    /// The load in flight; only its result is applied.
    pub seq: Option<u64>,
    /// The list has answered, so an empty one means "no files".
    pub loaded: bool,
    pub error: Option<String>,
}

impl StashFilesList {
    /// The files of `stash`, loading under `seq`.
    pub fn begin(stash: StashRef, seq: u64) -> Self {
        Self {
            stash,
            files: Vec::new(),
            selected: 0,
            seq: Some(seq),
            loaded: false,
            error: None,
        }
    }

    /// Apply a result if it is the load in flight, with the cursor on the
    /// first file. Returns whether it was applied.
    pub fn loaded(&mut self, seq: u64, result: Result<Vec<StashFile>, String>) -> bool {
        if self.seq != Some(seq) {
            return false;
        }
        self.seq = None;
        self.loaded = true;
        self.selected = 0;
        match result {
            Ok(files) => {
                self.files = files;
                self.error = None;
            }
            Err(message) => {
                self.files.clear();
                self.error = Some(message);
            }
        }
        true
    }

    /// Move the cursor, stopping at the ends. Returns whether it moved.
    pub fn move_by(&mut self, delta: isize) -> bool {
        let next = self
            .selected
            .saturating_add_signed(delta)
            .min(self.files.len().saturating_sub(1));
        let moved = next != self.selected;
        self.selected = next;
        moved
    }

    pub fn selected_file(&self) -> Option<&StashFile> {
        self.files.get(self.selected)
    }
}

/// What a stash push will take, fixed when the menu item was chosen, so a
/// refresh while the message is typed cannot change which files go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashDraft {
    pub mode: StashMode,
    pub paths: Vec<RepoPath>,
    /// Drafted from the Files range, which the push then uses up.
    pub from_range: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::commit_files::{CommitFile, CommitId, CommitRange};

    fn entry(index: usize, oid: &str) -> StashEntry {
        StashEntry {
            index,
            oid: CommitId::parse(oid).unwrap(),
            base: CommitId::parse("ba5e").unwrap(),
            untracked: None,
            subject: format!("On main: {oid}"),
        }
    }

    fn entries(oids: &[&str]) -> Vec<StashEntry> {
        oids.iter().enumerate().map(|(i, o)| entry(i, o)).collect()
    }

    fn list(oids: &[&str]) -> StashList {
        let mut list = StashList::default();
        list.begin(1);
        list.loaded(1, Ok(entries(oids)));
        list
    }

    #[test]
    fn only_the_load_in_flight_is_applied() {
        let mut list = StashList::default();
        list.begin(5);
        assert!(!list.loaded(4, Ok(entries(&["a1"]))));
        assert!(!list.loaded);
        assert!(list.loaded(5, Ok(entries(&["a1", "a2"]))));
        assert_eq!(list.entries.len(), 2);
        assert!(!list.loaded(5, Ok(vec![])), "applied twice");
    }

    #[test]
    fn a_reload_keeps_the_cursor_on_the_same_stash_or_clamps_it() {
        let mut list = list(&["a1", "a2", "a3"]);
        list.move_by(1);
        // A new stash on top moves `a2` down one.
        list.begin(2);
        list.loaded(2, Ok(entries(&["f0", "a1", "a2", "a3"])));
        assert_eq!(list.selected, 2);
        // `a2` is gone: the same place.
        list.begin(3);
        list.loaded(3, Ok(entries(&["f0", "a1", "a3"])));
        assert_eq!(list.selected, 2);
        // Fewer entries than the place: the last.
        list.begin(4);
        list.loaded(4, Ok(entries(&["f0"])));
        assert_eq!(list.selected, 0);
        list.begin(5);
        list.loaded(5, Ok(vec![]));
        assert_eq!((list.selected, list.selected_entry()), (0, None));
    }

    #[test]
    fn a_failed_load_keeps_the_error_and_no_entries() {
        let mut list = list(&["a1"]);
        list.begin(2);
        list.loaded(2, Err("bad".into()));
        assert_eq!((list.loaded, list.error.as_deref()), (true, Some("bad")));
        assert!(list.entries.is_empty());
    }

    #[test]
    fn the_cursor_stops_at_the_ends() {
        let mut list = list(&["a1", "a2"]);
        list.move_by(-1);
        assert_eq!(list.selected, 0);
        list.move_by(5);
        assert_eq!(list.selected, 1);
    }

    fn subjects(list: &StashList, rows: &[usize]) -> Vec<String> {
        rows.iter()
            .map(|&i| list.entries[i].subject.clone())
            .collect()
    }

    fn named(subjects: &[&str]) -> StashList {
        let mut list = StashList::default();
        list.begin(1);
        let entries = subjects
            .iter()
            .enumerate()
            .map(|(i, s)| StashEntry {
                subject: (*s).into(),
                ..entry(i, &format!("a{i}"))
            })
            .collect();
        list.loaded(1, Ok(entries));
        list
    }

    #[test]
    fn the_filter_matches_the_row_as_drawn_with_the_shared_rules() {
        let mut list = named(&["WIP on main: login", "On dev: Logout", "On main: tidy"]);
        list.set_filter("login".into());
        assert_eq!(list.visible(), [0]);
        // Lowercase ignores case; an uppercase char makes it exact.
        list.set_filter("logout".into());
        assert_eq!(list.visible(), [1]);
        list.set_filter("Logout".into());
        assert_eq!(list.visible(), [1]);
        list.set_filter("LOGOUT".into());
        assert!(list.visible().is_empty());
        // The `stash@{N}` label matches too, as a subsequence.
        list.set_filter("stash@{2}".into());
        assert_eq!(subjects(&list, &list.visible()), ["On main: tidy"]);
        list.set_filter("main tdy".into());
        assert_eq!(list.visible(), [2]);
        list.set_filter(String::new());
        assert_eq!(list.visible(), [0, 1, 2]);
    }

    #[test]
    fn the_cursor_steps_over_hidden_rows_and_a_hidden_one_moves_to_the_first_shown() {
        let mut list = named(&["keep 0", "drop 1", "keep 2", "drop 3", "keep 4"]);
        list.move_by(1);
        assert_eq!(list.selected, 1);
        list.set_filter("keep".into());
        assert_eq!(list.selected, 0, "the hidden cursor moved");
        list.move_by(1);
        assert_eq!(list.selected, 2);
        list.move_by(5);
        assert_eq!(list.selected, 4);
        list.set_filter("zzz".into());
        assert!(list.selected_hidden());
        assert_eq!(list.selected, 4, "nowhere to go");
        list.move_by(-1);
        assert_eq!(list.selected, 4);
    }

    #[test]
    fn a_changed_filter_clears_the_range_and_says_so() {
        let mut list = named(&["a", "b", "c"]);
        list.toggle_range();
        assert!(!list.set_filter(String::new()), "an unchanged filter");
        assert!(list.range_anchor.is_some());
        assert!(list.set_filter("b".into()));
        assert_eq!(list.range_anchor, None);
        assert!(!list.set_filter("c".into()), "no range to clear");
    }

    #[test]
    fn a_range_spans_shown_rows_and_a_hidden_row_starts_none() {
        let mut list = named(&["a", "b", "c", "d"]);
        list.move_by(1);
        list.toggle_range();
        list.move_by(2);
        assert_eq!(list.range(), [1, 2, 3]);
        list.move_by(-3);
        assert_eq!(list.range(), [0, 1]);
        list.toggle_range();
        assert_eq!((list.range_anchor.as_ref(), list.range()), (None, vec![]));
        list.set_filter("zzz".into());
        list.toggle_range();
        assert_eq!(list.range_anchor, None);
    }

    #[test]
    fn a_reload_keeps_the_range_on_the_same_stash_or_clears_it_when_gone() {
        let mut list = list(&["a1", "a2", "a3"]);
        list.move_by(1);
        list.toggle_range();
        list.move_by(1);
        assert_eq!(list.range(), [1, 2]);
        // A new stash on top renumbers every entry.
        list.begin(2);
        list.loaded(2, Ok(entries(&["f0", "a1", "a2", "a3"])));
        assert_eq!(list.range_anchor, Some(CommitId::parse("a2").unwrap()));
        assert_eq!(list.range(), [2, 3]);
        // The anchor's stash is gone.
        list.begin(3);
        list.loaded(3, Ok(entries(&["f0", "a1", "a3"])));
        assert_eq!(list.range_anchor, None);
        assert!(list.range().is_empty());
    }

    #[test]
    fn a_reload_under_a_filter_lands_on_a_shown_row() {
        let mut list = named(&["x", "keep"]);
        list.set_filter("keep".into());
        assert_eq!(list.selected, 1);
        list.begin(2);
        // Its stash is gone, and the same place is now hidden.
        let entries = vec![
            StashEntry {
                subject: "keep".into(),
                ..entry(0, "b0")
            },
            StashEntry {
                subject: "x".into(),
                ..entry(1, "b1")
            },
        ];
        list.loaded(2, Ok(entries));
        assert_eq!(list.selected, 0);
    }

    fn file(path: &str) -> StashFile {
        StashFile {
            range: CommitRange {
                oid: CommitId::parse("a1").unwrap(),
                parent: None,
            },
            file: CommitFile {
                status: 'M',
                path: path.into(),
                orig_path: None,
            },
        }
    }

    #[test]
    fn an_entrys_files_load_once_and_the_cursor_stops_at_the_ends() {
        let mut open = StashFilesList::begin(entry(0, "a1").stash_ref(), 7);
        assert!(!open.loaded(6, Ok(vec![file("old")])));
        assert!(open.loaded(7, Ok(vec![file("a"), file("b")])));
        assert_eq!(open.selected_file(), Some(&file("a")));
        assert!(!open.move_by(-1));
        assert!(open.move_by(1));
        assert!(!open.move_by(1));
        assert_eq!(open.selected_file(), Some(&file("b")));

        let mut failed = StashFilesList::begin(entry(0, "a1").stash_ref(), 7);
        failed.loaded(7, Err("bad".into()));
        assert_eq!(failed.error.as_deref(), Some("bad"));
        assert_eq!(failed.selected_file(), None);
    }
}
