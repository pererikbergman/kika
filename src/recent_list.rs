//! The Recent repositories popup's state: a cursor, and whether to offer
//! making the launch directory a repository. Pure; the rows come from the
//! model's recent list each time, so a reload never leaves them stale.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentList {
    pub cursor: usize,
    /// Show "Initialise a repository here" first: Kika started outside one.
    pub offer_init: bool,
}

/// One row of the popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecentRow<'a> {
    Init,
    Repo(&'a Path),
}

impl RecentList {
    pub fn new(offer_init: bool) -> Self {
        Self {
            cursor: 0,
            offer_init,
        }
    }

    /// The rows over `recent`: the init row when offered, then every
    /// recorded repository but `current`, which is already open.
    pub fn rows<'a>(&self, recent: &'a [PathBuf], current: Option<&Path>) -> Vec<RecentRow<'a>> {
        let init = self.offer_init.then_some(RecentRow::Init);
        let repos = recent
            .iter()
            .filter(|repo| Some(repo.as_path()) != current)
            .map(|repo| RecentRow::Repo(repo));
        init.into_iter().chain(repos).collect()
    }

    /// Move the cursor by `delta` over `len` rows, stopping at either end.
    pub fn move_cursor(&mut self, delta: isize, len: usize) {
        self.cursor = self
            .cursor
            .saturating_add_signed(delta)
            .min(len.saturating_sub(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn the_init_row_comes_first_and_the_open_repo_is_left_out() {
        let recent = paths(&["/a", "/b", "/c"]);
        let list = RecentList::new(true);
        assert_eq!(
            list.rows(&recent, None),
            [
                RecentRow::Init,
                RecentRow::Repo(Path::new("/a")),
                RecentRow::Repo(Path::new("/b")),
                RecentRow::Repo(Path::new("/c")),
            ]
        );
        let list = RecentList::new(false);
        assert_eq!(
            list.rows(&recent, Some(Path::new("/b"))),
            [
                RecentRow::Repo(Path::new("/a")),
                RecentRow::Repo(Path::new("/c")),
            ]
        );
    }

    #[test]
    fn the_cursor_stays_inside_the_rows() {
        let mut list = RecentList::new(false);
        list.move_cursor(1, 0);
        assert_eq!(list.cursor, 0);
        list.move_cursor(5, 3);
        assert_eq!(list.cursor, 2);
        list.move_cursor(-9, 3);
        assert_eq!(list.cursor, 0);
    }
}
