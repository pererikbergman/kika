//! The Stash panel: the stash list, its cursor, and an entry opened to its
//! files. Pure; `AppState` turns its answers into effects.

use crate::git::ops::StashMode;
use crate::git::path::RepoPath;
use crate::git::stash::{StashEntry, StashFile, StashRef};

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
        true
    }

    /// Move the cursor, stopping at the ends.
    pub fn move_by(&mut self, delta: isize) {
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.entries.len().saturating_sub(1));
    }

    pub fn selected_entry(&self) -> Option<&StashEntry> {
        self.entries.get(self.selected)
    }
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
