//! The Branches panel's Remotes tab: each remote as a header row with its
//! remote-tracking branches below it, and the selection over both. Pure;
//! the model loads the list and moves the selection.

use crate::git::remotes::{Remote, RemoteBranch, RemoteName};

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
#[derive(PartialEq, Eq)]
enum RowKey {
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
    }

    fn key(&self, row: RemoteRow) -> RowKey {
        match row {
            RemoteRow::Remote(i) => RowKey::Remote(self.remotes[i].name.0.clone()),
            RemoteRow::Branch(i, j) => RowKey::Branch(self.remotes[i].branches[j].refname.clone()),
        }
    }

    /// Move the selection by `delta`, stopping at either end. Says whether
    /// it moved.
    pub fn move_by(&mut self, delta: isize) -> bool {
        let Some(current) = self.selected else {
            return false;
        };
        let next = current
            .saturating_add_signed(delta)
            .min(self.rows().len().saturating_sub(1));
        self.selected = Some(next);
        next != current
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
