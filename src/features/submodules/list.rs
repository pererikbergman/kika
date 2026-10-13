//! The Submodules popup's state: the list (a `FilteredList`, which owns
//! its rows, error, cursor and load in flight). It has no filter. Pure;
//! the model loads it and moves the cursor.

use crate::filtered_list::{FilteredList, Row};
use crate::git::path::RepoPath;
use crate::git::submodule::Submodule;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmoduleList {
    /// The submodules, with the cursor and load in flight. A reload keeps
    /// the cursor at its place, clamped.
    pub list: FilteredList<Submodule>,
}

impl Row for Submodule {
    type Key = RepoPath;
    const FOLLOW_ROW: bool = false;

    fn key(&self) -> RepoPath {
        self.path.clone()
    }

    /// Its path as drawn. The list has no filter, so this is never matched.
    fn filter_text(&self) -> String {
        self.path.display().to_string()
    }
}

impl SubmoduleList {
    /// Waiting for load `seq`, with nothing to show yet.
    pub fn loading(seq: u64) -> Self {
        let mut list = FilteredList::default();
        list.begin(seq);
        Self { list }
    }

    /// The submodule under the cursor, once there is one.
    pub fn selected(&self) -> Option<&Submodule> {
        self.list.selected_row()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::submodule::SubmoduleState;
    use crate::test_support::load_list;

    fn sub(path: &str) -> Submodule {
        Submodule {
            path: path.into(),
            oid: "abc".into(),
            state: SubmoduleState::UpToDate,
        }
    }

    #[test]
    fn the_cursor_stays_inside_the_list() {
        let mut list = SubmoduleList::loading(1);
        list.list.move_by(1);
        assert_eq!(list.list.selected, 0);
        assert_eq!(list.selected(), None);
        load_list(&mut list.list, Ok(vec![sub("a"), sub("b")]));
        list.list.move_by(1);
        list.list.move_by(1);
        assert_eq!(list.selected(), Some(&sub("b")));
        list.list.move_by(-5);
        assert_eq!(list.list.selected, 0);
    }

    #[test]
    fn a_shorter_reload_pulls_the_cursor_in_and_an_error_replaces_the_list() {
        let mut list = SubmoduleList::loading(1);
        load_list(&mut list.list, Ok(vec![sub("a"), sub("b")]));
        list.list.move_by(1);
        load_list(&mut list.list, Ok(vec![sub("a")]));
        assert_eq!(list.list.selected, 0);
        load_list(&mut list.list, Err("boom".into()));
        assert_eq!(list.list.rows, vec![]);
        assert_eq!(list.list.error.as_deref(), Some("boom"));
        load_list(&mut list.list, Ok(vec![]));
        assert_eq!((list.list.rows, list.list.error), (vec![], None));
    }
}
