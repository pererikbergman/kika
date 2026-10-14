//! The Stash panel: the stash list (a `FilteredList`, which owns its
//! cursor, filter, range and loads), and an entry opened to its files
//! (another `FilteredList`). Pure; `AppState` turns its answers into
//! effects.

use super::git::{StashEntry, StashFile, StashMode, StashRef};
use crate::filtered_list::{FilteredList, Row};
use crate::rows::drawn_path;
use kika_git::commit_files::CommitId;
use kika_git::path::RepoPath;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StashList {
    /// The entries, with the cursor, filter, range and load in flight.
    /// A range anchors on an entry's id, so a reload keeps it on the
    /// same stash though its number moved.
    pub list: FilteredList<StashEntry>,
    /// An entry opened to its files, shown in place of the list.
    /// The cursor stays where it was, so closing it returns to the entry.
    pub open: Option<StashFilesList>,
    /// The entry a rename prompt is about, kept from when it opened.
    pub pending: Option<StashRef>,
}

impl Row for StashEntry {
    type Key = CommitId;

    fn key(&self) -> CommitId {
        self.oid.clone()
    }

    /// Its row as drawn, `stash@{N}  <subject>`. The subject was made
    /// safe when read.
    fn filter_text(&self) -> String {
        format!("{}  {}", self.stash_ref().arg(), self.subject)
    }
}

/// One stash entry's files, like a commit's in the Commits panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashFilesList {
    /// For the panel title.
    pub stash: StashRef,
    /// The files, with the cursor and load in flight. Each open is a
    /// fresh list, so it starts on the first file. It has no filter.
    pub list: FilteredList<StashFile>,
}

impl Row for StashFile {
    type Key = RepoPath;

    fn key(&self) -> RepoPath {
        self.file.path.clone()
    }

    /// Its path as drawn. The list has no filter, so this is never matched.
    fn filter_text(&self) -> String {
        drawn_path(&self.file)
    }
}

impl StashFilesList {
    /// The files of `stash`, loading under `seq`.
    pub fn begin(stash: StashRef, seq: u64) -> Self {
        let mut list = FilteredList::default();
        list.begin(seq);
        Self { stash, list }
    }

    pub fn selected_file(&self) -> Option<&StashFile> {
        self.list.selected_row()
    }
}

/// What a stash push will take, fixed when the menu item was chosen, so a
/// refresh while the message is typed cannot change which files go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashDraft {
    pub mode: StashMode,
    pub paths: Vec<RepoPath>,
    /// Old sides of staged renames, restored after the push.
    pub restore: Vec<RepoPath>,
    /// Staged deletions, reset to `HEAD` in the index before the push.
    pub unstage: Vec<RepoPath>,
    /// Drafted from the Files range, which the push then uses up.
    pub from_range: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::Tag;
    use kika_git::commit_files::{CommitFile, CommitId, CommitRange};

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

    fn tag(seq: u64) -> Tag {
        Tag {
            generation: 0,
            seq,
            user: false,
        }
    }

    fn reload(list: &mut FilteredList<StashEntry>, seq: u64, entries: Vec<StashEntry>) {
        list.begin(seq);
        assert!(list.loaded(tag(seq), 0, Ok(entries)));
    }

    fn list(oids: &[&str]) -> FilteredList<StashEntry> {
        let mut list = FilteredList::default();
        reload(&mut list, 1, entries(oids));
        list
    }

    fn subjects(list: &FilteredList<StashEntry>, rows: &[usize]) -> Vec<String> {
        rows.iter().map(|&i| list.rows[i].subject.clone()).collect()
    }

    fn named(subjects: &[&str]) -> FilteredList<StashEntry> {
        let mut list = FilteredList::default();
        let entries = subjects
            .iter()
            .enumerate()
            .map(|(i, s)| StashEntry {
                subject: (*s).into(),
                ..entry(i, &format!("a{i}"))
            })
            .collect();
        reload(&mut list, 1, entries);
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
    fn a_reload_keeps_the_range_on_the_same_stash_or_clears_it_when_gone() {
        let mut list = list(&["a1", "a2", "a3"]);
        list.move_by(1);
        list.toggle_range();
        list.move_by(1);
        assert_eq!(list.range(), [1, 2]);
        // A new stash on top renumbers every entry.
        reload(&mut list, 2, entries(&["f0", "a1", "a2", "a3"]));
        assert_eq!(list.range_anchor, Some(CommitId::parse("a2").unwrap()));
        assert_eq!(list.range(), [2, 3]);
        // The anchor's stash is gone.
        reload(&mut list, 3, entries(&["f0", "a1", "a3"]));
        assert_eq!(list.range_anchor, None);
        assert!(list.range().is_empty());
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
        assert!(!open.list.loaded(tag(6), 0, Ok(vec![file("old")])));
        assert!(open.list.loaded(tag(7), 0, Ok(vec![file("a"), file("b")])));
        assert_eq!(open.selected_file(), Some(&file("a")));
        assert!(!open.list.move_by(-1));
        assert!(open.list.move_by(1));
        assert!(!open.list.move_by(1));
        assert_eq!(open.selected_file(), Some(&file("b")));

        let mut failed = StashFilesList::begin(entry(0, "a1").stash_ref(), 7);
        failed.list.loaded(tag(7), 0, Err("bad".into()));
        assert_eq!(failed.list.error.as_deref(), Some("bad"));
        assert_eq!(failed.selected_file(), None);
    }
}
