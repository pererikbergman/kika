//! The Branches panel's Remotes tab: each remote as a header row with its
//! remote-tracking branches below it, and the selection over both. Pure;
//! the model loads the list and moves the selection.

use crate::filtered_list::{FilteredList, Row};
use crate::git::remotes::{Remote, RemoteBranch, RemoteName};
use crate::list_filter;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemotesList {
    /// Every row in drawing order: each remote, then its branches. The
    /// cursor follows a row by [`RowKey`] across reloads.
    pub list: FilteredList<RemoteLine>,
    /// What an open prompt acts on, taken when it opened, so a reload
    /// while the user types cannot change it.
    pub pending: Option<RemotePending>,
}

/// One row of the tab: a remote's header, or one of its branches. A
/// header holds the remote as loaded, its branches included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteLine {
    Remote(Remote),
    Branch(RemoteBranch),
}

/// The target of an open Remotes prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemotePending {
    /// The remote branch a new local branch will track.
    Track(RemoteBranch),
    /// The remote being renamed.
    Rename(RemoteName),
    /// The name, already checked, of the remote whose URL is asked for.
    AddUrl { name: String },
}

/// What a row is, so a reload can find it again: a remote's name or a
/// branch's ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RowKey {
    Remote(Vec<u8>),
    Branch(Vec<u8>),
}

/// A failed reload keeps the last rows, which may be out of date.
impl Row for RemoteLine {
    type Key = RowKey;
    const KEEP_ROWS_ON_ERROR: bool = true;

    fn key(&self) -> RowKey {
        match self {
            RemoteLine::Remote(remote) => RowKey::Remote(remote.name.0.clone()),
            RemoteLine::Branch(branch) => RowKey::Branch(branch.refname.clone()),
        }
    }

    /// A branch row matches as `remote/branch`.
    fn filter_text(&self) -> String {
        match self {
            RemoteLine::Remote(remote) => remote.name.display(),
            RemoteLine::Branch(branch) => branch.display(),
        }
    }

    /// A header shows when its remote's name matches or any of its
    /// branches shows, so a shown branch is never without its remote.
    fn visible(filter: &str, rows: &[Self]) -> Vec<usize> {
        let mut shown = vec![false; rows.len()];
        // Walked backwards, so each header knows whether a branch below
        // it shows.
        let mut branch_shown = false;
        for (at, row) in rows.iter().enumerate().rev() {
            let matched = list_filter::matches(filter, &row.filter_text());
            shown[at] = match row {
                RemoteLine::Branch(_) => {
                    branch_shown |= matched;
                    matched
                }
                RemoteLine::Remote(_) => {
                    let header = branch_shown || matched;
                    branch_shown = false;
                    header
                }
            };
        }
        (0..rows.len()).filter(|&at| shown[at]).collect()
    }
}

/// A load's remotes as rows in drawing order: each remote, then its
/// branches.
pub(crate) fn lines(remotes: Vec<Remote>) -> Vec<RemoteLine> {
    remotes
        .into_iter()
        .flat_map(|remote| {
            let branches = remote.branches.clone();
            std::iter::once(RemoteLine::Remote(remote))
                .chain(branches.into_iter().map(RemoteLine::Branch))
        })
        .collect()
}

impl RemotesList {
    /// The remotes as loaded, in order.
    pub fn remotes(&self) -> impl Iterator<Item = &Remote> {
        self.list.rows.iter().filter_map(|row| match row {
            RemoteLine::Remote(remote) => Some(remote),
            RemoteLine::Branch(_) => None,
        })
    }

    /// The selected remote, or the remote of the selected branch.
    pub fn selected_remote(&self) -> Option<&Remote> {
        let above = self.list.rows.get(..=self.list.selected)?;
        above.iter().rev().find_map(|row| match row {
            RemoteLine::Remote(remote) => Some(remote),
            RemoteLine::Branch(_) => None,
        })
    }

    /// The selected branch; `None` on a header row.
    pub fn selected_branch(&self) -> Option<&RemoteBranch> {
        match self.list.selected_row()? {
            RemoteLine::Remote(_) => None,
            RemoteLine::Branch(branch) => Some(branch),
        }
    }

    /// The loaded branch written `remote/branch`, exactly.
    pub fn branch_named(&self, display: &str) -> Option<&RemoteBranch> {
        self.remotes()
            .flat_map(|r| &r.branches)
            .find(|b| b.display() == display)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::load_list;

    fn remote(name: &str, branches: &[&str]) -> Remote {
        Remote {
            name: name.into(),
            url: Some(format!("/srv/{name}")),
            configured: true,
            branches: branches
                .iter()
                .map(|short| RemoteBranch {
                    remote: name.into(),
                    short: short.as_bytes().to_vec(),
                    refname: format!("refs/remotes/{name}/{short}").into_bytes(),
                })
                .collect(),
        }
    }

    fn list(remotes: Vec<Remote>) -> RemotesList {
        let mut list = RemotesList::default();
        load_list(&mut list.list, Ok(lines(remotes)));
        list
    }

    /// Whether `row` is the header of the remote named `name`.
    fn is_header(row: Option<&RemoteLine>, name: &str) -> bool {
        matches!(row, Some(RemoteLine::Remote(r)) if r.name == name.into())
    }

    #[test]
    fn rows_are_each_remote_then_its_branches() {
        let list = list(vec![remote("a", &["x", "y"]), remote("b", &[])]);
        assert_eq!(
            list.list.rows.iter().map(Row::key).collect::<Vec<_>>(),
            [
                RowKey::Remote(b"a".to_vec()),
                RowKey::Branch(b"refs/remotes/a/x".to_vec()),
                RowKey::Branch(b"refs/remotes/a/y".to_vec()),
                RowKey::Remote(b"b".to_vec()),
            ]
        );
        assert_eq!(list.list.selected, 0);
        assert_eq!(list.selected_remote().unwrap().name, "a".into());
        assert_eq!(list.selected_branch(), None);
    }

    #[test]
    fn moving_stops_at_the_ends_and_reaches_headers_and_branches() {
        let mut list = list(vec![remote("a", &["x"]), remote("b", &[])]);
        assert!(!list.list.move_by(-1));
        assert!(list.list.move_by(1));
        assert_eq!(list.selected_branch().unwrap().display(), "a/x");
        assert_eq!(list.selected_remote().unwrap().name, "a".into());
        assert!(list.list.move_by(5));
        assert!(is_header(list.list.selected_row(), "b"));
        assert!(!list.list.move_by(1));
        assert!(!RemotesList::default().list.move_by(1));
    }

    #[test]
    fn a_reload_keeps_the_same_row_or_else_the_place() {
        let mut list = list(vec![remote("a", &["x", "y"]), remote("b", &["z"])]);
        list.list.move_by(2);
        // A new remote above moves `a/y` down.
        load_list(
            &mut list.list,
            Ok(lines(vec![
                remote("0", &["n"]),
                remote("a", &["x", "y"]),
                remote("b", &["z"]),
            ])),
        );
        assert_eq!(list.selected_branch().unwrap().display(), "a/y");
        assert_eq!(list.list.selected, 4);

        list.list.move_by(1);
        assert!(is_header(list.list.selected_row(), "b"));
        load_list(
            &mut list.list,
            Ok(lines(vec![remote("a", &["x", "y"]), remote("b", &["z"])])),
        );
        assert_eq!(list.selected_remote().unwrap().name, "b".into());

        // Gone: the same index, or the last row.
        load_list(
            &mut list.list,
            Ok(lines(vec![remote("a", &["x", "y", "w"])])),
        );
        assert_eq!(list.list.selected, 3);
        load_list(&mut list.list, Ok(lines(vec![remote("a", &[])])));
        assert_eq!(list.list.selected, 0);
        load_list(&mut list.list, Ok(lines(vec![])));
        assert_eq!(list.list.selected_row(), None);
    }

    #[test]
    fn a_matching_branch_shows_with_its_header_and_a_matching_remote_shows_its_header() {
        let mut list = list(vec![
            remote("origin", &["main", "topic"]),
            remote("fork", &["x"]),
            remote("up", &[]),
        ]);
        list.list.filter = "topic".into();
        assert_eq!(
            list.list.visible(),
            [0, 2],
            "origin's header, then origin/topic"
        );
        list.list.filter = "fork".into();
        // The branch matches as `fork/x`, so it shows too.
        assert_eq!(list.list.visible(), [3, 4]);
        list.list.filter = "up".into();
        assert_eq!(list.list.visible(), [5], "a remote with no branches");
        list.list.filter = "zzz".into();
        assert!(list.list.visible().is_empty());
    }

    #[test]
    fn moving_skips_hidden_rows_and_a_filter_moves_a_hidden_selection() {
        let mut list = list(vec![remote("a", &["x", "y"]), remote("b", &["y"])]);
        list.list.filter = "y".into();
        assert_eq!(list.list.visible(), [0, 2, 3, 4]);
        list.list.reconcile();
        assert_eq!(list.list.selected, 0);
        assert!(list.list.move_by(1));
        assert_eq!(list.selected_branch().unwrap().display(), "a/y");
        list.list.filter = "b/".into();
        assert!(list.list.selected_hidden());
        list.list.reconcile();
        assert!(is_header(list.list.selected_row(), "b"));
        // A reload under a filter lands on a shown row.
        list.list.selected = 0;
        load_list(
            &mut list.list,
            Ok(lines(vec![remote("a", &["x", "y"]), remote("b", &["y"])])),
        );
        assert!(is_header(list.list.selected_row(), "b"));
    }

    #[test]
    fn a_range_follows_its_rows_and_a_reload_without_its_anchor_clears_it() {
        let mut list = list(vec![remote("a", &["x"]), remote("b", &[])]);
        list.list.range_anchor = Some(RowKey::Branch(b"refs/remotes/a/x".to_vec()));
        list.list.move_by(5);
        assert_eq!(list.list.range(), [1, 2]);
        load_list(
            &mut list.list,
            Ok(lines(vec![
                remote("0", &[]),
                remote("a", &["x"]),
                remote("b", &[]),
            ])),
        );
        assert_eq!(list.list.range(), [2, 3]);
        load_list(
            &mut list.list,
            Ok(lines(vec![remote("a", &[]), remote("b", &[])])),
        );
        assert_eq!(list.list.range_anchor, None);
    }

    #[test]
    fn a_failed_reload_keeps_the_list_and_says_why() {
        let mut list = list(vec![remote("a", &[])]);
        load_list(&mut list.list, Err("broken".into()));
        assert_eq!(list.remotes().count(), 1);
        assert_eq!(list.list.error.as_deref(), Some("broken"));
    }

    #[test]
    fn a_branch_is_found_only_by_its_exact_display_name() {
        let list = list(vec![remote("origin", &["main"]), remote("fork", &["x"])]);
        assert_eq!(
            list.branch_named("fork/x").map(|b| b.refname.as_slice()),
            Some(&b"refs/remotes/fork/x"[..])
        );
        assert!(list.branch_named("fork/").is_none());
        assert!(list.branch_named("main").is_none());
    }
}
