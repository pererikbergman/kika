//! The files of one commit, opened from the Commits panel in place of the
//! log, and its filter. Pure; `AppState` turns its answers into effects.

use crate::filtered_list::{FilteredList, Row};
use crate::git::commit_files::{CommitFile, CommitRange};
use crate::git::path::RepoPath;
use crate::rows::drawn_path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitFilesList {
    pub range: CommitRange,
    /// For the panel title: the commit's short id, or the label of the
    /// commit or branch compared.
    pub short_oid: String,
    /// The diff mark's label when this compares against it rather than
    /// showing one commit; for the title.
    pub against: Option<String>,
    /// The files, with the cursor, filter and load in flight. Each open
    /// is a fresh list, so it starts on the first file and with no
    /// filter. The filter is only ever matched, never passed to git.
    pub list: FilteredList<CommitFile>,
}

impl Row for CommitFile {
    type Key = RepoPath;

    fn key(&self) -> RepoPath {
        self.path.clone()
    }

    /// Its path as drawn, with a rename's old name.
    fn filter_text(&self) -> String {
        drawn_path(self)
    }
}

impl CommitFilesList {
    /// The list of `range`, loading under `seq`.
    pub fn begin(range: CommitRange, short_oid: String, seq: u64) -> Self {
        let mut list = FilteredList::default();
        list.begin(seq);
        Self {
            range,
            short_oid,
            against: None,
            list,
        }
    }

    pub fn selected_file(&self) -> Option<&CommitFile> {
        self.list.selected_row()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::Tag;
    use crate::git::commit_files::CommitId;

    fn tag(seq: u64) -> Tag {
        Tag {
            generation: 0,
            seq,
            user: false,
        }
    }

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
        assert!(!list.list.loaded(tag(6), 0, Ok(vec![file("old")])));
        assert!(!list.list.loaded);
        assert!(list.list.loaded(tag(7), 0, Ok(vec![file("a"), file("b")])));
        assert_eq!(list.selected_file(), Some(&file("a")));
        assert!(!list.list.loaded(tag(7), 0, Ok(vec![])), "applied twice");
    }

    #[test]
    fn a_failed_load_keeps_the_error_and_no_files() {
        let mut list = list();
        list.list.loaded(tag(7), 0, Err("bad".into()));
        assert_eq!(
            (list.list.loaded, list.list.error.as_deref()),
            (true, Some("bad"))
        );
        assert_eq!(list.selected_file(), None);
    }

    #[test]
    fn the_cursor_stops_at_the_ends() {
        let mut list = list();
        list.list.loaded(tag(7), 0, Ok(vec![file("a"), file("b")]));
        assert!(!list.list.move_by(-1));
        assert!(list.list.move_by(1));
        assert!(!list.list.move_by(1));
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
        list.list.loaded(
            tag(7),
            0,
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
        assert_eq!(list.list.visible(), [0, 1, 2]);
        list.list.set_filter("src/m".into());
        assert_eq!(list.list.visible(), [0]);
        list.list.set_filter("old/name".into());
        assert_eq!(list.list.visible(), [1], "the old name of a rename");
        list.list.set_filter("src".into());
        assert_eq!(list.list.visible(), [0, 1]);
        list.list.set_filter("M".into());
        assert!(
            list.list.visible().is_empty(),
            "the status letter is not matched"
        );
    }

    #[test]
    fn the_cursor_moves_over_shown_rows_only() {
        let mut list = loaded_three();
        list.list.set_filter("rs".into()); // main.rs, new.rs
        assert_eq!(list.list.visible(), [0, 1]);
        assert!(list.list.move_by(5));
        assert_eq!(list.list.selected, 1);
        assert!(!list.list.move_by(1), "stops at the last row shown");
        list.list.set_filter("md".into());
        assert_eq!(list.list.selected, 2, "a hidden cursor moves");
        list.list.set_filter("in".into());
        assert_eq!(list.list.selected, 2, "the cursor is shown");
        assert_eq!(list.list.visible(), [0, 2]);
        assert!(list.list.move_by(-1));
        assert_eq!(list.list.selected, 0, "steps over the hidden row");
        assert!(list.list.move_by(1));
        assert_eq!(list.list.selected, 2);
    }

    #[test]
    fn a_filter_that_hides_every_row_leaves_the_cursor_be() {
        let mut list = loaded_three();
        list.list.move_by(1);
        list.list.set_filter("zzz".into());
        assert!(list.list.visible().is_empty());
        assert!(list.list.selected_hidden());
        assert_eq!(list.list.selected, 1);
        assert!(!list.list.move_by(1));
        assert_eq!(list.list.selected, 1);
        list.list.set_filter(String::new());
        assert!(!list.list.selected_hidden());
    }

    #[test]
    fn a_load_with_a_filter_puts_the_cursor_on_a_shown_row() {
        let mut list = list();
        list.list.filter = "readme".into();
        list.list
            .loaded(tag(7), 0, Ok(vec![file("a"), file("README")]));
        assert_eq!(list.list.selected, 1);
        assert!(!list.list.selected_hidden());
        assert!(
            super::tests::list().list.filter.is_empty(),
            "each open starts empty"
        );
    }
}
