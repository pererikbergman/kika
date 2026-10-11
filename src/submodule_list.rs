//! The Submodules popup's state: the list as last loaded, or why it could
//! not be, and the cursor. Pure; the model loads it and moves the cursor.

use crate::git::submodule::Submodule;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmoduleList {
    /// The load whose result is awaited; older ones are dropped.
    pub seq: u64,
    /// `None` until the first load lands.
    pub items: Option<Vec<Submodule>>,
    pub error: Option<String>,
    pub cursor: usize,
}

impl SubmoduleList {
    /// Waiting for load `seq`, with nothing to show yet.
    pub fn loading(seq: u64) -> Self {
        Self {
            seq,
            items: None,
            error: None,
            cursor: 0,
        }
    }

    /// Take a load's result. The cursor stays where it was, inside the list.
    pub fn loaded(&mut self, result: Result<Vec<Submodule>, String>) {
        match result {
            Ok(items) => {
                self.cursor = self.cursor.min(items.len().saturating_sub(1));
                self.items = Some(items);
                self.error = None;
            }
            Err(message) => {
                self.items = None;
                self.error = Some(message);
            }
        }
    }

    /// Move the cursor by `delta`, stopping at either end.
    pub fn move_cursor(&mut self, delta: isize) {
        let last = self.items.as_ref().map_or(0, |i| i.len().saturating_sub(1));
        self.cursor = self.cursor.saturating_add_signed(delta).min(last);
    }

    /// The submodule under the cursor, once there is one.
    pub fn selected(&self) -> Option<&Submodule> {
        self.items.as_ref()?.get(self.cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::submodule::SubmoduleState;

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
        list.move_cursor(1);
        assert_eq!(list.cursor, 0);
        assert_eq!(list.selected(), None);
        list.loaded(Ok(vec![sub("a"), sub("b")]));
        list.move_cursor(1);
        list.move_cursor(1);
        assert_eq!(list.selected(), Some(&sub("b")));
        list.move_cursor(-5);
        assert_eq!(list.cursor, 0);
    }

    #[test]
    fn a_shorter_reload_pulls_the_cursor_in_and_an_error_replaces_the_list() {
        let mut list = SubmoduleList::loading(1);
        list.loaded(Ok(vec![sub("a"), sub("b")]));
        list.move_cursor(1);
        list.loaded(Ok(vec![sub("a")]));
        assert_eq!(list.cursor, 0);
        list.loaded(Err("boom".into()));
        assert_eq!(list.items, None);
        assert_eq!(list.error.as_deref(), Some("boom"));
        list.loaded(Ok(vec![]));
        assert_eq!((list.items, list.error), (Some(vec![]), None));
    }
}
