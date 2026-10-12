//! The files of one commit, opened from the Commits panel in place of the
//! log, and its filter. Pure; `AppState` turns its answers into effects.

use crate::git::commit_files::{CommitFile, CommitRange};
use crate::list_filter;

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
    /// Narrows the rows shown. Only ever matched, never passed to git, and
    /// each open starts without one.
    pub filter: String,
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
            filter: String::new(),
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
        self.reconcile();
        true
    }

    /// Move the cursor among the rows shown, stopping at the ends. Returns
    /// whether it moved.
    pub fn move_by(&mut self, delta: isize) -> bool {
        match list_filter::step(&self.visible(), self.selected, delta) {
            Some(next) if next != self.selected => {
                self.selected = next;
                true
            }
            _ => false,
        }
    }

    /// The indexes of the files the filter shows, in list order.
    pub fn visible(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            return (0..self.files.len()).collect();
        }
        let texts: Vec<String> = self.files.iter().map(drawn_path).collect();
        list_filter::visible(&self.filter, texts.iter().map(String::as_str))
    }

    /// The filter hides the cursor's row, which happens only when it hides
    /// every row.
    pub fn selected_hidden(&self) -> bool {
        !self.filter.is_empty()
            && self
                .files
                .get(self.selected)
                .is_some_and(|f| !list_filter::matches(&self.filter, &drawn_path(f)))
    }

    /// Put a cursor the filter hides on the first row shown; leave it be
    /// when no row is. Returns whether it moved.
    pub fn reconcile(&mut self) -> bool {
        if self.selected_hidden()
            && let Some(&first) = self.visible().first()
        {
            self.selected = first;
            return true;
        }
        false
    }

    /// Narrow the rows shown to those matching `text`. Returns whether the
    /// cursor moved, so the caller loads the file it moved to.
    pub fn set_filter(&mut self, text: String) -> bool {
        self.filter = text;
        self.reconcile()
    }

    pub fn selected_file(&self) -> Option<&CommitFile> {
        self.files.get(self.selected)
    }
}

/// A row's path as drawn, with the old name before it for a rename or
/// copy. The filter matches this and not the status letter, so a row
/// matches by what the user reads in it.
pub(crate) fn drawn_path(file: &CommitFile) -> String {
    match &file.orig_path {
        Some(orig) => format!("{} → {}", orig.display(), file.path.display()),
        None => file.path.display().to_string(),
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

    fn renamed(from: &str, to: &str) -> CommitFile {
        CommitFile {
            status: 'R',
            path: to.into(),
            orig_path: Some(from.into()),
        }
    }

    fn loaded_three() -> CommitFilesList {
        let mut list = list();
        list.loaded(
            7,
            Ok(vec![
                file("src/main.rs"),
                renamed("old/name.rs", "src/new.rs"),
                file("docs/main.md"),
            ]),
        );
        list
    }

    #[test]
    fn the_filter_matches_the_drawn_path_with_a_renames_old_name() {
        let mut list = loaded_three();
        assert_eq!(list.visible(), [0, 1, 2]);
        list.set_filter("src/m".into());
        assert_eq!(list.visible(), [0]);
        list.set_filter("old/name".into());
        assert_eq!(list.visible(), [1], "the old name of a rename");
        list.set_filter("src".into());
        assert_eq!(list.visible(), [0, 1]);
        list.set_filter("M".into());
        assert!(
            list.visible().is_empty(),
            "the status letter is not matched"
        );
    }

    #[test]
    fn the_cursor_moves_over_shown_rows_only() {
        let mut list = loaded_three();
        list.set_filter("rs".into()); // main.rs, new.rs
        assert_eq!(list.visible(), [0, 1]);
        assert!(list.move_by(5));
        assert_eq!(list.selected, 1);
        assert!(!list.move_by(1), "stops at the last row shown");
        assert!(list.set_filter("md".into()), "a hidden cursor moves");
        assert_eq!(list.selected, 2);
        assert!(!list.set_filter("in".into()), "the cursor is shown");
        assert_eq!(list.visible(), [0, 2]);
        assert!(list.move_by(-1));
        assert_eq!(list.selected, 0, "steps over the hidden row");
        assert!(list.move_by(1));
        assert_eq!(list.selected, 2);
    }

    #[test]
    fn a_filter_that_hides_every_row_leaves_the_cursor_be() {
        let mut list = loaded_three();
        list.move_by(1);
        assert!(!list.set_filter("zzz".into()));
        assert!(list.visible().is_empty());
        assert!(list.selected_hidden());
        assert_eq!(list.selected, 1);
        assert!(!list.move_by(1));
        assert_eq!(list.selected, 1);
        list.set_filter(String::new());
        assert!(!list.selected_hidden());
    }

    #[test]
    fn a_load_with_a_filter_puts_the_cursor_on_a_shown_row() {
        let mut list = list();
        list.filter = "readme".into();
        list.loaded(7, Ok(vec![file("a"), file("README")]));
        assert_eq!(list.selected, 1);
        assert!(!list.selected_hidden());
        assert!(
            super::tests::list().filter.is_empty(),
            "each open starts empty"
        );
    }
}
