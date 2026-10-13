//! The Branches panel's state: the local list as last loaded, the
//! selection, the Remotes and Tags tabs, and the selected branch's log
//! shown in the Diff pane's place. Pure; the model loads them and moves the selection.

use super::remotes::RemotesList;
use super::tags::{PendingTag, TagsList};
use crate::filtered_list::{FilteredList, Row};
use crate::git::branches::{Branch, BranchName, LogRow};
use crate::git::diff::terminal_safe;
use crate::intent::Scroll;

/// Which list the Branches panel shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BranchesTab {
    #[default]
    Local,
    Remotes,
    Tags,
}

impl BranchesTab {
    /// The tab to the right, wrapping round.
    pub fn next(self) -> Self {
        match self {
            BranchesTab::Local => BranchesTab::Remotes,
            BranchesTab::Remotes => BranchesTab::Tags,
            BranchesTab::Tags => BranchesTab::Local,
        }
    }

    /// The tab to the left, wrapping round.
    pub fn prev(self) -> Self {
        match self {
            BranchesTab::Local => BranchesTab::Tags,
            BranchesTab::Remotes => BranchesTab::Local,
            BranchesTab::Tags => BranchesTab::Remotes,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BranchesPanel {
    /// The local branches as last loaded. The cursor follows a branch by
    /// name across reloads.
    pub list: FilteredList<Branch>,
    pub log: BranchLog,
    /// The branch an open create, rename or set-upstream prompt acts on,
    /// taken when the prompt opened, so a reload while the user types
    /// cannot change it.
    pub pending: Option<BranchName>,
    pub tab: BranchesTab,
    pub remotes: RemotesList,
    pub tags: TagsList,
    /// The tag the open name or message prompt will make.
    pub pending_tag: Option<PendingTag>,
}

/// A failed reload keeps the last branches, which may be out of date.
impl Row for Branch {
    type Key = BranchName;
    const KEEP_ROWS_ON_ERROR: bool = true;
    fn key(&self) -> BranchName {
        self.name.clone()
    }
    fn filter_text(&self) -> String {
        self.name.display()
    }
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
    pub fn selected_branch(&self) -> Option<&Branch> {
        self.list.selected_row()
    }

    /// The branch `HEAD` is on, if it is listed.
    pub fn current(&self) -> Option<&Branch> {
        self.list.rows.iter().find(|b| b.head)
    }

    /// Drop the range on `tab`, which does not outlive the tab or the op
    /// it was asked for. The filter stays.
    pub fn clear_range(&mut self, tab: BranchesTab) {
        match tab {
            BranchesTab::Local => self.list.range_anchor = None,
            BranchesTab::Remotes => self.remotes.list.range_anchor = None,
            BranchesTab::Tags => self.tags.range_anchor = None,
        }
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
            .or_else(|| refname.strip_prefix(b"refs/tags/"))
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
    use crate::test_support::load_list;

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
        load_list(&mut panel.list, Ok(vec![]));
        assert_eq!((panel.list.loaded, panel.selected_branch()), (true, None));
        load_list(&mut panel.list, Ok(list(&["main", "a"])));
        assert_eq!(panel.list.selected, 0);
    }

    #[test]
    fn a_reload_keeps_the_name_or_else_the_place() {
        let mut panel = BranchesPanel::default();
        load_list(&mut panel.list, Ok(list(&["main", "a", "b", "c"])));
        panel.list.selected = 2;
        load_list(&mut panel.list, Ok(list(&["b", "main", "a", "c"])));
        assert_eq!(panel.selected_branch().unwrap().name, "b".into());
        panel.list.selected = 3;
        load_list(&mut panel.list, Ok(list(&["main", "a"])));
        assert_eq!(panel.list.selected, 1);
    }

    #[test]
    fn a_failed_reload_keeps_the_list_and_says_why() {
        let mut panel = BranchesPanel::default();
        load_list(&mut panel.list, Ok(list(&["main"])));
        load_list(&mut panel.list, Err("broken".into()));
        assert_eq!(panel.list.rows.len(), 1);
        assert_eq!(panel.list.error.as_deref(), Some("broken"));
    }

    #[test]
    fn moving_stops_at_the_ends_and_says_whether_it_moved() {
        let mut panel = BranchesPanel::default();
        assert!(!panel.list.move_by(1));
        load_list(&mut panel.list, Ok(list(&["main", "a"])));
        assert!(panel.list.move_by(1));
        assert!(!panel.list.move_by(1));
        assert!(panel.list.move_by(-5));
        assert_eq!(panel.list.selected, 0);
    }

    #[test]
    fn moving_skips_the_branches_the_filter_hides() {
        let mut panel = BranchesPanel::default();
        load_list(&mut panel.list, Ok(list(&["main", "a1", "b", "a2"])));
        panel.list.filter = "a".into();
        panel.list.reconcile();
        assert_eq!(panel.list.visible(), [0, 1, 3], "main matches a too");
        panel.list.selected = 1;
        assert!(panel.list.move_by(1));
        assert_eq!(panel.list.selected, 3, "b is hidden");
        assert!(!panel.list.move_by(1));
        assert!(panel.list.move_by(-1));
        assert_eq!(panel.list.selected, 1);
    }

    #[test]
    fn a_filter_that_hides_the_selection_moves_it_to_the_first_shown() {
        let mut panel = BranchesPanel::default();
        load_list(&mut panel.list, Ok(list(&["main", "x", "y1", "y2"])));
        panel.list.selected = 1;
        panel.list.filter = "y".into();
        assert!(panel.list.selected_hidden());
        panel.list.reconcile();
        assert_eq!(panel.list.selected, 2);
        // A filter that hides every branch leaves the selection alone.
        panel.list.filter = "zzz".into();
        panel.list.reconcile();
        assert_eq!(panel.list.selected, 2);
        assert!(panel.list.selected_hidden());
        assert!(!panel.list.move_by(1));
    }

    #[test]
    fn a_range_runs_over_shown_branches_and_a_reload_without_its_anchor_clears_it() {
        let mut panel = BranchesPanel::default();
        load_list(&mut panel.list, Ok(list(&["a", "b", "c", "d"])));
        panel.list.range_anchor = Some("b".into());
        panel.list.selected = 3;
        assert_eq!(panel.list.range(), [1, 2, 3]);
        // A reload that keeps the anchor keeps the range on the same names.
        load_list(&mut panel.list, Ok(list(&["0", "a", "b", "c", "d"])));
        assert_eq!(panel.list.range(), [2, 3, 4]);
        load_list(&mut panel.list, Ok(list(&["a", "c", "d"])));
        assert_eq!(panel.list.range_anchor, None);
        assert!(panel.list.range().is_empty());
    }

    #[test]
    fn a_reload_moves_a_selection_the_filter_hides_to_the_first_shown() {
        let mut panel = BranchesPanel::default();
        panel.list.filter = "t".into();
        load_list(&mut panel.list, Ok(list(&["main", "topic", "wip"])));
        assert_eq!(panel.list.selected, 1);
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
        log.begin(3, b"refs/tags/v1.0".to_vec());
        assert_eq!(log.name().as_deref(), Some("v1.0"));
    }

    #[test]
    fn the_tabs_cycle_forward_and_back() {
        use BranchesTab::*;
        assert_eq!(BranchesTab::default(), Local);
        assert_eq!(
            [Local.next(), Remotes.next(), Tags.next()],
            [Remotes, Tags, Local]
        );
        assert_eq!(
            [Local.prev(), Remotes.prev(), Tags.prev()],
            [Tags, Local, Remotes]
        );
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
