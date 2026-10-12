//! The Branches panel's Remotes tab: each remote as a header row with its
//! remote-tracking branches below it, and the selection over both. Pure;
//! the model loads the list and moves the selection.

use crate::git::remotes::{Remote, RemoteBranch, RemoteName};
use crate::list_filter;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemotesList {
    pub remotes: Vec<Remote>,
    /// False until the first load lands, to tell "loading" from "none".
    pub loaded: bool,
    pub error: Option<String>,
    /// Index into [`RemotesList::rows`].
    pub selected: Option<usize>,
    /// The list load whose result is awaited; older ones are dropped.
    pub list_seq: Option<u64>,
    /// What an open prompt acts on, taken when it opened, so a reload
    /// while the user types cannot change it.
    pub pending: Option<RemotePending>,
    /// What the tab is narrowed to, kept for the session.
    pub filter: String,
    /// The row a range selection started on, so a reload keeps the range
    /// on the same rows.
    pub range_anchor: Option<RowKey>,
}

/// One row of the tab: a remote's header, or one of its branches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteRow {
    /// `remotes[i]`.
    Remote(usize),
    /// `remotes[i].branches[j]`.
    Branch(usize, usize),
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

impl RemotesList {
    /// Every row in drawing order: each remote, then its branches.
    pub fn rows(&self) -> Vec<RemoteRow> {
        self.remotes
            .iter()
            .enumerate()
            .flat_map(|(i, remote)| {
                std::iter::once(RemoteRow::Remote(i))
                    .chain((0..remote.branches.len()).map(move |j| RemoteRow::Branch(i, j)))
            })
            .collect()
    }

    /// Take a list load's result. The selection stays on the same row; if
    /// that row is gone, at the same index. A failure keeps the last list,
    /// which may be out of date.
    pub fn loaded(&mut self, result: Result<Vec<Remote>, String>) {
        self.loaded = true;
        let remotes = match result {
            Ok(remotes) => remotes,
            Err(message) => {
                self.error = Some(message);
                return;
            }
        };
        self.error = None;
        let old = self.selected_row().map(|row| self.key(row));
        self.remotes = remotes;
        let rows = self.rows();
        self.selected = if rows.is_empty() {
            None
        } else {
            old.and_then(|old| rows.iter().position(|&row| self.key(row) == old))
                .or_else(|| self.selected.map(|i| i.min(rows.len() - 1)))
                .or(Some(0))
        };
        self.reconcile();
    }

    /// What `row` is, to find it again after a reload.
    pub(crate) fn key(&self, row: RemoteRow) -> RowKey {
        match row {
            RemoteRow::Remote(i) => RowKey::Remote(self.remotes[i].name.0.clone()),
            RemoteRow::Branch(i, j) => RowKey::Branch(self.remotes[i].branches[j].refname.clone()),
        }
    }

    /// Move the selection by `delta` over the rows the filter shows,
    /// stopping at either end. Says whether it moved.
    pub fn move_by(&mut self, delta: isize) -> bool {
        let Some(current) = self.selected else {
            return false;
        };
        let Some(next) = list_filter::step(&self.visible(), current, delta) else {
            return false;
        };
        self.selected = Some(next);
        next != current
    }

    /// The indexes into [`RemotesList::rows`] the filter shows. A branch
    /// row matches as `remote/branch`; a header shows when its remote's
    /// name matches or any of its branches shows, so a shown branch is
    /// never without its remote.
    pub fn visible(&self) -> Vec<usize> {
        let rows = self.rows();
        if self.filter.is_empty() {
            return (0..rows.len()).collect();
        }
        let mut shown = vec![false; rows.len()];
        // Walked backwards, so each header knows whether a branch below
        // it shows.
        let mut branch_shown = false;
        for (at, &row) in rows.iter().enumerate().rev() {
            shown[at] = match row {
                RemoteRow::Branch(..) => {
                    let matched = self.row_matches(row);
                    branch_shown |= matched;
                    matched
                }
                RemoteRow::Remote(_) => {
                    let matched = branch_shown || self.row_matches(row);
                    branch_shown = false;
                    matched
                }
            };
        }
        (0..rows.len()).filter(|&at| shown[at]).collect()
    }

    /// Whether the filter matches what `row` is, on its own.
    fn row_matches(&self, row: RemoteRow) -> bool {
        let text = match row {
            RemoteRow::Remote(i) => self.remotes[i].name.display(),
            RemoteRow::Branch(i, j) => self.remotes[i].branches[j].display(),
        };
        list_filter::matches(&self.filter, &text)
    }

    /// The filter hides the selected row, which happens only when it
    /// hides every row.
    pub fn selected_hidden(&self) -> bool {
        if self.filter.is_empty() {
            return false;
        }
        match self.selected_row() {
            None => false,
            Some(row @ RemoteRow::Branch(..)) => !self.row_matches(row),
            Some(row @ RemoteRow::Remote(i)) => {
                let branches = 0..self.remotes[i].branches.len();
                !self.row_matches(row)
                    && !branches
                        .into_iter()
                        .any(|j| self.row_matches(RemoteRow::Branch(i, j)))
            }
        }
    }

    /// The rows the range covers, as indexes into
    /// [`RemotesList::rows`]; empty without one.
    pub fn range(&self) -> Vec<usize> {
        let Some(anchor) = &self.range_anchor else {
            return Vec::new();
        };
        let rows = self.rows();
        let at = rows.iter().position(|&row| &self.key(row) == anchor);
        match (at, self.selected) {
            (Some(at), Some(cursor)) => list_filter::range(&self.visible(), at, cursor),
            _ => Vec::new(),
        }
    }

    /// Narrow the list to `text`. A changed filter drops the range, which
    /// was made over the rows shown before; says whether it did.
    pub fn set_filter(&mut self, text: String) -> bool {
        let mut cleared = false;
        if text != self.filter {
            self.filter = text;
            cleared = self.range_anchor.take().is_some();
        }
        self.reconcile();
        cleared
    }

    /// Start a range at the selected row, or clear the one there is. A
    /// row the filter hides starts none.
    pub fn toggle_range(&mut self) {
        if self.range_anchor.take().is_none() && !self.selected_hidden() {
            self.range_anchor = self.selected_row().map(|row| self.key(row));
        }
    }

    /// After the filter or the list changed: a range whose first row is
    /// gone is cleared, and a hidden selection moves to the first row
    /// shown.
    pub fn reconcile(&mut self) {
        if let Some(anchor) = &self.range_anchor
            && !self.rows().iter().any(|&row| &self.key(row) == anchor)
        {
            self.range_anchor = None;
        }
        if self.selected_hidden()
            && let Some(&first) = self.visible().first()
        {
            self.selected = Some(first);
        }
    }

    pub fn selected_row(&self) -> Option<RemoteRow> {
        self.selected.and_then(|i| self.rows().get(i).copied())
    }

    /// The selected remote, or the remote of the selected branch.
    pub fn selected_remote(&self) -> Option<&Remote> {
        match self.selected_row()? {
            RemoteRow::Remote(i) | RemoteRow::Branch(i, _) => self.remotes.get(i),
        }
    }

    /// The selected branch; `None` on a header row.
    pub fn selected_branch(&self) -> Option<&RemoteBranch> {
        match self.selected_row()? {
            RemoteRow::Remote(_) => None,
            RemoteRow::Branch(i, j) => self.remotes.get(i)?.branches.get(j),
        }
    }

    /// The loaded branch written `remote/branch`, exactly.
    pub fn branch_named(&self, display: &str) -> Option<&RemoteBranch> {
        self.remotes
            .iter()
            .flat_map(|r| &r.branches)
            .find(|b| b.display() == display)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        list.loaded(Ok(remotes));
        list
    }

    #[test]
    fn rows_are_each_remote_then_its_branches() {
        let list = list(vec![remote("a", &["x", "y"]), remote("b", &[])]);
        assert_eq!(
            list.rows(),
            [
                RemoteRow::Remote(0),
                RemoteRow::Branch(0, 0),
                RemoteRow::Branch(0, 1),
                RemoteRow::Remote(1),
            ]
        );
        assert_eq!(list.selected, Some(0));
        assert_eq!(list.selected_remote().unwrap().name, "a".into());
        assert_eq!(list.selected_branch(), None);
    }

    #[test]
    fn moving_stops_at_the_ends_and_reaches_headers_and_branches() {
        let mut list = list(vec![remote("a", &["x"]), remote("b", &[])]);
        assert!(!list.move_by(-1));
        assert!(list.move_by(1));
        assert_eq!(list.selected_branch().unwrap().display(), "a/x");
        assert_eq!(list.selected_remote().unwrap().name, "a".into());
        assert!(list.move_by(5));
        assert_eq!(list.selected_row(), Some(RemoteRow::Remote(1)));
        assert!(!list.move_by(1));
        assert!(!RemotesList::default().move_by(1));
    }

    #[test]
    fn a_reload_keeps_the_same_row_or_else_the_place() {
        let mut list = list(vec![remote("a", &["x", "y"]), remote("b", &["z"])]);
        list.move_by(2);
        // A new remote above moves `a/y` down.
        list.loaded(Ok(vec![
            remote("0", &["n"]),
            remote("a", &["x", "y"]),
            remote("b", &["z"]),
        ]));
        assert_eq!(list.selected_branch().unwrap().display(), "a/y");
        assert_eq!(list.selected, Some(4));

        list.move_by(1);
        assert_eq!(list.selected_row(), Some(RemoteRow::Remote(2)));
        list.loaded(Ok(vec![remote("a", &["x", "y"]), remote("b", &["z"])]));
        assert_eq!(list.selected_remote().unwrap().name, "b".into());

        // Gone: the same index, or the last row.
        list.loaded(Ok(vec![remote("a", &["x", "y", "w"])]));
        assert_eq!(list.selected, Some(3));
        list.loaded(Ok(vec![remote("a", &[])]));
        assert_eq!(list.selected, Some(0));
        list.loaded(Ok(vec![]));
        assert_eq!(list.selected, None);
    }

    #[test]
    fn a_matching_branch_shows_with_its_header_and_a_matching_remote_shows_its_header() {
        let mut list = list(vec![
            remote("origin", &["main", "topic"]),
            remote("fork", &["x"]),
            remote("up", &[]),
        ]);
        list.filter = "topic".into();
        assert_eq!(list.visible(), [0, 2], "origin's header, then origin/topic");
        list.filter = "fork".into();
        // The branch matches as `fork/x`, so it shows too.
        assert_eq!(list.visible(), [3, 4]);
        list.filter = "up".into();
        assert_eq!(list.visible(), [5], "a remote with no branches");
        list.filter = "zzz".into();
        assert!(list.visible().is_empty());
    }

    #[test]
    fn moving_skips_hidden_rows_and_a_filter_moves_a_hidden_selection() {
        let mut list = list(vec![remote("a", &["x", "y"]), remote("b", &["y"])]);
        list.filter = "y".into();
        assert_eq!(list.visible(), [0, 2, 3, 4]);
        list.reconcile();
        assert_eq!(list.selected, Some(0));
        assert!(list.move_by(1));
        assert_eq!(list.selected_branch().unwrap().display(), "a/y");
        list.filter = "b/".into();
        assert!(list.selected_hidden());
        list.reconcile();
        assert_eq!(list.selected_row(), Some(RemoteRow::Remote(1)));
        // A reload under a filter lands on a shown row.
        list.selected = Some(0);
        list.loaded(Ok(vec![remote("a", &["x", "y"]), remote("b", &["y"])]));
        assert_eq!(list.selected_row(), Some(RemoteRow::Remote(1)));
    }

    #[test]
    fn a_range_follows_its_rows_and_a_reload_without_its_anchor_clears_it() {
        let mut list = list(vec![remote("a", &["x"]), remote("b", &[])]);
        list.range_anchor = Some(list.key(RemoteRow::Branch(0, 0)));
        list.move_by(5);
        assert_eq!(list.range(), [1, 2]);
        list.loaded(Ok(vec![
            remote("0", &[]),
            remote("a", &["x"]),
            remote("b", &[]),
        ]));
        assert_eq!(list.range(), [2, 3]);
        list.loaded(Ok(vec![remote("a", &[]), remote("b", &[])]));
        assert_eq!(list.range_anchor, None);
    }

    #[test]
    fn a_failed_reload_keeps_the_list_and_says_why() {
        let mut list = list(vec![remote("a", &[])]);
        list.loaded(Err("broken".into()));
        assert_eq!(list.remotes.len(), 1);
        assert_eq!(list.error.as_deref(), Some("broken"));
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
