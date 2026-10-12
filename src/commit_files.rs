//! The files of one commit, opened from the Commits panel in place of the
//! log. Pure; `AppState` turns its answers into effects.

use crate::git::commit_files::{CommitFile, CommitRange};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitFilesList {
    pub range: CommitRange,
    /// For the panel title: the commit's short id, or the label of the
    /// commit or branch compared.
    pub short_oid: String,
    /// The diff mark's label when this compares against it rather than
    /// showing one commit; for the title.
    pub against: Option<String>,
    pub files: Vec<CommitFile>,
    pub selected: usize,
    /// The load in flight; only its result is applied.
    pub seq: Option<u64>,
    /// The list has answered, so an empty one means "no files".
    pub loaded: bool,
    pub error: Option<String>,
}

impl CommitFilesList {
    /// The list of `range`, loading under `seq`.
    pub fn begin(range: CommitRange, short_oid: String, seq: u64) -> Self {
        Self {
            range,
            short_oid,
            against: None,
            files: Vec::new(),
            selected: 0,
            seq: Some(seq),
            loaded: false,
            error: None,
        }
    }

    /// Apply a result if it is the load in flight, with the cursor on the
    /// first file. Returns whether it was applied.
    pub fn loaded(&mut self, seq: u64, result: Result<Vec<CommitFile>, String>) -> bool {
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

    pub fn selected_file(&self) -> Option<&CommitFile> {
        self.files.get(self.selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::commit_files::CommitId;

    fn list() -> CommitFilesList {
        let range = CommitRange {
            oid: CommitId::parse("abc").unwrap(),
            parent: None,
        };
        CommitFilesList::begin(range, "abc".into(), 7)
    }

    fn file(path: &str) -> CommitFile {
        CommitFile {
            status: 'M',
            path: path.into(),
            orig_path: None,
        }
    }

    #[test]
    fn only_the_load_in_flight_is_applied() {
        let mut list = list();
        assert!(!list.loaded(6, Ok(vec![file("old")])));
        assert!(!list.loaded);
        assert!(list.loaded(7, Ok(vec![file("a"), file("b")])));
        assert_eq!(list.selected_file(), Some(&file("a")));
        assert!(!list.loaded(7, Ok(vec![])), "applied twice");
    }

    #[test]
    fn a_failed_load_keeps_the_error_and_no_files() {
        let mut list = list();
        list.loaded(7, Err("bad".into()));
        assert_eq!((list.loaded, list.error.as_deref()), (true, Some("bad")));
        assert_eq!(list.selected_file(), None);
    }

    #[test]
    fn the_cursor_stops_at_the_ends() {
        let mut list = list();
        list.loaded(7, Ok(vec![file("a"), file("b")]));
        assert!(!list.move_by(-1));
        assert!(list.move_by(1));
        assert!(!list.move_by(1));
        assert_eq!(list.selected_file(), Some(&file("b")));
    }
}
