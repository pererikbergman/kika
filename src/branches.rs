//! The Branches panel's state: the local list as last loaded, the
//! selection, the Remotes tab, and the selected branch's log shown in the
//! Diff pane's place. Pure; the model loads them and moves the selection.

use crate::git::branches::{Branch, BranchName, LogRow};
use crate::git::diff::terminal_safe;
use crate::intent::Scroll;
use crate::remotes::RemotesList;

/// Which list the Branches panel shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BranchesTab {
    #[default]
    Local,
    Remotes,
}

impl BranchesTab {
    pub fn other(self) -> Self {
        match self {
            BranchesTab::Local => BranchesTab::Remotes,
            BranchesTab::Remotes => BranchesTab::Local,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BranchesPanel {
    pub branches: Vec<Branch>,
    /// False until the first load lands, to tell "loading" from "none".
    pub loaded: bool,
    pub error: Option<String>,
    pub selected: Option<usize>,
    /// The list load whose result is awaited; older ones are dropped.
    pub list_seq: Option<u64>,
    pub log: BranchLog,
    /// The branch an open create, rename or set-upstream prompt acts on,
    /// taken when the prompt opened, so a reload while the user types
    /// cannot change it.
    pub pending: Option<BranchName>,
    pub tab: BranchesTab,
    pub remotes: RemotesList,
}

/// The selected branch's newest commits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BranchLog {
    /// The load whose result is awaited.
    pub seq: Option<u64>,
    /// The full ref the rows, or the load under way, belong to.
    pub for_ref: Option<Vec<u8>>,
    pub rows: Vec<LogRow>,
    pub error: Option<String>,
    /// The first row shown.
    pub scroll: usize,
}

impl BranchesPanel {
    /// Take a list load's result. The selection stays on the same name;
    /// if that branch is gone, near the same index. A failure keeps the
    /// last list, which may be out of date.
    pub fn loaded(&mut self, result: Result<Vec<Branch>, String>) {
        self.loaded = true;
        let branches = match result {
            Ok(branches) => branches,
            Err(message) => {
                self.error = Some(message);
                return;
            }
        };
        self.error = None;
        let old = self.selected_branch().map(|b| b.name.clone());
        self.branches = branches;
        self.selected = if self.branches.is_empty() {
            None
        } else {
            let last = self.branches.len() - 1;
            old.and_then(|name| self.branches.iter().position(|b| b.name == name))
                .or_else(|| self.selected.map(|i| i.min(last)))
                .or(Some(0))
        };
    }

    /// Move the selection by `delta`, stopping at either end. Says whether
    /// it moved.
    pub fn move_by(&mut self, delta: isize) -> bool {
        let Some(current) = self.selected else {
            return false;
        };
        let next = current
            .saturating_add_signed(delta)
            .min(self.branches.len().saturating_sub(1));
        self.selected = Some(next);
        next != current
    }

    pub fn selected_branch(&self) -> Option<&Branch> {
        self.selected.and_then(|i| self.branches.get(i))
    }

    /// The branch `HEAD` is on, if it is listed.
    pub fn current(&self) -> Option<&Branch> {
        self.branches.iter().find(|b| b.head)
    }
}

impl BranchLog {
    /// Wait for load `seq` of the log of `refname`, a full ref. Another
    /// branch's rows go at once; the same branch's stay until the new ones
    /// land.
    pub fn begin(&mut self, seq: u64, refname: Vec<u8>) {
        if self.for_ref.as_ref() != Some(&refname) {
            self.rows.clear();
            self.scroll = 0;
        }
        self.seq = Some(seq);
        self.for_ref = Some(refname);
        self.error = None;
    }

    /// The branch the log is of, as the user writes it: `main` or
    /// `origin/main`. Terminal-safe.
    pub fn name(&self) -> Option<String> {
        let refname = self.for_ref.as_ref()?;
        let short = refname
            .strip_prefix(b"refs/heads/")
            .or_else(|| refname.strip_prefix(b"refs/remotes/"))
            .unwrap_or(refname);
        Some(terminal_safe(short))
    }

    /// Take load `seq`'s result, unless a newer load was started.
    pub fn apply(&mut self, seq: u64, result: Result<Vec<LogRow>, String>) {
        if self.seq != Some(seq) {
            return;
        }
        self.seq = None;
        match result {
            Ok(rows) => {
                self.scroll = self.scroll.min(rows.len().saturating_sub(1));
                self.rows = rows;
            }
            Err(message) => {
                self.rows.clear();
                self.error = Some(message);
            }
        }
    }

    /// Move by a line or a page of `page` rows, never past the last row.
    pub fn scroll(&mut self, by: Scroll, page: usize) {
        let scroll = match by {
            Scroll::LineDown => self.scroll.saturating_add(1),
            Scroll::LineUp => self.scroll.saturating_sub(1),
            Scroll::PageDown => self.scroll.saturating_add(page),
            Scroll::PageUp => self.scroll.saturating_sub(page),
        };
        self.scroll = scroll.min(self.rows.len().saturating_sub(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::branches::Track;

    fn branch(name: &str) -> Branch {
        Branch {
            name: name.into(),
            head: false,
            upstream: None,
            track: Track::None,
            merged: true,
        }
    }

    fn list(names: &[&str]) -> Vec<Branch> {
        names.iter().map(|n| branch(n)).collect()
    }

    fn row(subject: &str) -> LogRow {
        LogRow {
            short_oid: "abc1234".into(),
            date: "2024-01-01".into(),
            author: "a".into(),
            subject: subject.into(),
        }
    }

    #[test]
    fn the_first_load_selects_the_first_branch_and_an_empty_one_nothing() {
        let mut panel = BranchesPanel::default();
        panel.loaded(Ok(vec![]));
        assert_eq!((panel.loaded, panel.selected), (true, None));
        panel.loaded(Ok(list(&["main", "a"])));
        assert_eq!(panel.selected, Some(0));
    }

    #[test]
    fn a_reload_keeps_the_name_or_else_the_place() {
        let mut panel = BranchesPanel::default();
        panel.loaded(Ok(list(&["main", "a", "b", "c"])));
        panel.selected = Some(2);
        panel.loaded(Ok(list(&["b", "main", "a", "c"])));
        assert_eq!(panel.selected_branch().unwrap().name, "b".into());
        panel.selected = Some(3);
        panel.loaded(Ok(list(&["main", "a"])));
        assert_eq!(panel.selected, Some(1));
    }

    #[test]
    fn a_failed_reload_keeps_the_list_and_says_why() {
        let mut panel = BranchesPanel::default();
        panel.loaded(Ok(list(&["main"])));
        panel.loaded(Err("broken".into()));
        assert_eq!(panel.branches.len(), 1);
        assert_eq!(panel.error.as_deref(), Some("broken"));
    }

    #[test]
    fn moving_stops_at_the_ends_and_says_whether_it_moved() {
        let mut panel = BranchesPanel::default();
        assert!(!panel.move_by(1));
        panel.loaded(Ok(list(&["main", "a"])));
        assert!(panel.move_by(1));
        assert!(!panel.move_by(1));
        assert!(panel.move_by(-5));
        assert_eq!(panel.selected, Some(0));
    }

    #[test]
    fn a_log_for_another_branch_clears_and_a_stale_result_is_dropped() {
        let mut log = BranchLog::default();
        log.begin(1, b"refs/heads/a".to_vec());
        log.apply(1, Ok(vec![row("one"), row("two")]));
        log.scroll(Scroll::LineDown, 10);
        log.begin(2, b"refs/heads/a".to_vec());
        assert_eq!((log.rows.len(), log.scroll), (2, 1), "same branch keeps");
        log.begin(3, b"refs/remotes/a".to_vec());
        assert_eq!((log.rows.len(), log.scroll), (0, 0));
        log.apply(2, Ok(vec![row("stale")]));
        assert!(log.rows.is_empty());
        log.apply(3, Err("bad".into()));
        assert_eq!(log.error.as_deref(), Some("bad"));
    }

    #[test]
    fn the_log_names_its_branch_as_the_user_writes_it() {
        let mut log = BranchLog::default();
        assert_eq!(log.name(), None);
        log.begin(1, b"refs/heads/feature/x".to_vec());
        assert_eq!(log.name().as_deref(), Some("feature/x"));
        log.begin(2, b"refs/remotes/origin/\x1bmain".to_vec());
        assert_eq!(log.name().as_deref(), Some("origin/\u{FFFD}main"));
    }

    #[test]
    fn the_tab_switches_back_and_forth() {
        assert_eq!(BranchesTab::default(), BranchesTab::Local);
        assert_eq!(BranchesTab::Local.other(), BranchesTab::Remotes);
        assert_eq!(BranchesTab::Remotes.other(), BranchesTab::Local);
    }

    #[test]
    fn the_log_scrolls_by_lines_and_pages_inside_its_rows() {
        let mut log = BranchLog::default();
        log.begin(1, b"refs/heads/a".to_vec());
        log.apply(1, Ok((0..5).map(|i| row(&i.to_string())).collect()));
        log.scroll(Scroll::PageDown, 3);
        assert_eq!(log.scroll, 3);
        log.scroll(Scroll::PageDown, 3);
        assert_eq!(log.scroll, 4);
        log.scroll(Scroll::LineUp, 3);
        log.scroll(Scroll::PageUp, 3);
        assert_eq!(log.scroll, 0);
    }
}
