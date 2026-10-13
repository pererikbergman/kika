//! The Files panel's rows on a `FilteredList`, which owns their cursor,
//! filter, range and load in flight. Core holds the list as
//! `AppState::files`, because the cursor drives the diff that other
//! features read.

use crate::filtered_list::Row;
use crate::git::path::RepoPath;
use crate::git::status::FileEntry;

impl Row for FileEntry {
    type Key = RepoPath;
    /// A failed status keeps the files shown.
    const KEEP_ROWS_ON_ERROR: bool = true;

    fn key(&self) -> RepoPath {
        self.path.clone()
    }

    /// Its path, as drawn.
    fn filter_text(&self) -> String {
        self.path.display().to_string()
    }
}
