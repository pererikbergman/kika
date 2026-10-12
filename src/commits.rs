//! The Commits panel's state: the history of `HEAD` loaded so far, its
//! graph, the cursor and the push/merge markers. Pure; `AppState` turns
//! its answers into effects.

use crate::commit_files::CommitFilesList;
use crate::commit_graph::{GraphBuilder, GraphRow};
use crate::features::commits::reflog::ReflogList;
use crate::git::branch_tips::Tip;
use crate::git::commit_files::CommitId;
use crate::git::log::{CommitRow, LogPage, LogQuery, Markers};
use crate::list_filter;
use crate::rebase_todo::RebaseTodo;

/// Commits per page.
pub const PAGE: usize = 200;
/// The next page loads once the cursor is this close to the end.
pub const PREFETCH: usize = 50;

/// Where a commit stands against the remotes and the main branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    /// Reachable from the main branch.
    Merged,
    /// Not reachable from any remote-tracking ref.
    Unpushed,
    Pushed,
    /// Not known: no remotes, no main branch, or too many commits to tell.
    Unknown,
}

/// Everything the panel shows. Rows only grow by whole pages, in log
/// order, so the graph can be extended without looking back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitsPanel {
    pub rows: Vec<CommitRow>,
    /// One per row, in step with `rows`.
    pub graph: Vec<GraphRow>,
    /// Carries the lanes across pages.
    pub builder: GraphBuilder,
    /// The most lanes any loaded row has, so the graph column keeps one
    /// width while scrolling.
    pub lanes: usize,
    /// The newest commit of page 0; later pages start from it.
    pub tip: Option<String>,
    /// The last page is in.
    pub done: bool,
    /// Page 0 has answered at least once.
    pub loaded: bool,
    pub error: Option<String>,
    pub selected: usize,
    /// The page load in flight; only its result is applied.
    pub page_seq: Option<u64>,
    /// The page in flight comes after the rows shown, not in their place.
    pub more_in_flight: bool,
    pub markers: Markers,
    pub markers_seq: Option<u64>,
    /// Unix time page 0 loaded, which relative dates count from.
    pub loaded_at: i64,
    /// A commit opened to its files, shown in place of the log. `selected`
    /// stays where it was, so closing it returns to the same commit.
    pub open: Option<CommitFilesList>,
    /// The commit an open question is about, kept from when it was asked,
    /// since the question itself carries no data.
    pub pending: Option<CommitId>,
    /// Where every local branch points, to tell which sit on commits a
    /// rewrite would replace. Loaded with page 0; `None` until it is, or
    /// when it failed.
    pub tips: Option<Vec<Tip>>,
    /// Why the last read of the tips failed, kept apart from one that is
    /// still on its way.
    pub tips_error: Option<String>,
    pub tips_seq: Option<u64>,
    /// The todo list of a stopped rebase, shown above the log.
    pub todo: Option<RebaseTodo>,
    pub todo_seq: Option<u64>,
    /// The todo row under the cursor, by index into the list (the next to
    /// run is 0); `None` while the cursor is on the log.
    pub todo_selected: Option<usize>,
    /// The write of an edited todo list in flight; one at a time.
    pub todo_write_seq: Option<u64>,
    /// Which list the panel shows.
    pub tab: CommitsTab,
    /// `HEAD`'s reflog, read when its tab is first shown or an undo asks.
    pub reflog: ReflogList,
    /// Narrows the loaded rows as typed; empty shows them all.
    pub filter: String,
    /// The full id of the commit a range selection started on, so a reload
    /// keeps the range on the same commits.
    pub range_anchor: Option<String>,
    /// What git narrows the log to. Unlike `filter`, it changes which
    /// commits are loaded, not which loaded rows show.
    pub query: LogQuery,
    /// The look for the commit the staged lines come from, in flight;
    /// only its answer is applied.
    pub fixup_base_seq: Option<u64>,
}

/// The Commits panel's two lists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CommitsTab {
    /// The history of `HEAD`.
    #[default]
    Log,
    /// Everywhere `HEAD` has been, newest first.
    Reflog,
}

impl CommitsTab {
    pub fn other(self) -> Self {
        match self {
            CommitsTab::Log => CommitsTab::Reflog,
            CommitsTab::Reflog => CommitsTab::Log,
        }
    }
}

impl CommitsPanel {
    pub fn marker(&self, oid: &str) -> Marker {
        let markers = &self.markers;
        if markers
            .unmerged
            .as_ref()
            .is_some_and(|set| !set.contains(oid))
        {
            Marker::Merged
        } else if markers
            .unpushed
            .as_ref()
            .is_some_and(|set| set.contains(oid))
        {
            Marker::Unpushed
        } else if markers.unpushed.is_some() {
            Marker::Pushed
        } else {
            Marker::Unknown
        }
    }

    /// `id` as the log shows it, or its first seven digits once its row
    /// is no longer loaded.
    pub fn short_oid(&self, id: &CommitId) -> String {
        self.rows
            .iter()
            .find(|row| row.oid == id.as_str())
            .map_or_else(
                || id.as_str().chars().take(7).collect(),
                |row| row.short_oid.clone(),
            )
    }

    /// Narrow the log to `query`. Everything loaded under the old one is
    /// dropped, cursor and range included, since none of it need be in the
    /// new history; the fuzzy filter stays and applies to the new rows.
    /// The caller loads page 0.
    pub fn set_query(&mut self, query: LogQuery) {
        self.query = query;
        self.rows.clear();
        self.graph.clear();
        self.builder = GraphBuilder::default();
        self.lanes = 0;
        self.tip = None;
        self.done = false;
        self.loaded = false;
        self.error = None;
        self.selected = 0;
        self.range_anchor = None;
        self.open = None;
    }

    /// Page `skip` was asked for under `seq`.
    pub fn begin_page(&mut self, seq: u64, skip: usize) {
        self.page_seq = Some(seq);
        self.more_in_flight = skip > 0;
    }

    /// Apply a page if it is the one in flight. Page 0 replaces the rows,
    /// keeping the cursor on the same commit when it is still there; a
    /// later page is appended.
    pub fn page_loaded(&mut self, seq: u64, skip: usize, result: Result<(LogPage, i64), String>) {
        if self.page_seq != Some(seq) {
            return;
        }
        self.page_seq = None;
        self.more_in_flight = false;
        let (page, now) = match result {
            Ok(loaded) => loaded,
            Err(message) => {
                if skip == 0 {
                    self.error = Some(format!("Could not read the history: {message}"));
                    self.loaded = true;
                } else {
                    self.error = Some(format!("Could not load more commits: {message}"));
                    // Retrying on every key press would only repeat it;
                    // the next reload tries again.
                    self.done = true;
                }
                return;
            }
        };
        self.error = None;
        if skip == 0 {
            let old = self.rows.get(self.selected).map(|row| row.oid.clone());
            self.rows.clear();
            self.graph.clear();
            self.builder = GraphBuilder::default();
            self.lanes = 0;
            self.tip = page.rows.first().map(|row| row.oid.clone());
            self.loaded = true;
            self.loaded_at = now;
            self.append(page);
            self.selected = old
                .and_then(|oid| self.rows.iter().position(|row| row.oid == oid))
                .unwrap_or(self.selected)
                .min(self.rows.len().saturating_sub(1));
        } else if skip == self.rows.len() {
            self.append(page);
        }
    }

    fn append(&mut self, page: LogPage) {
        self.done = page.done;
        for row in &page.rows {
            let graph = self.builder.push(row);
            self.lanes = self.lanes.max(graph.cells.len());
            self.graph.push(graph);
        }
        self.rows.extend(page.rows);
    }

    /// Apply markers if they are the load in flight. A failure leaves every
    /// marker unknown rather than stale.
    pub fn markers_loaded(&mut self, seq: u64, result: Result<Markers, String>) {
        if self.markers_seq != Some(seq) {
            return;
        }
        self.markers_seq = None;
        self.markers = result.unwrap_or_default();
    }

    /// Apply branch tips if they are the load in flight. A failure leaves
    /// them unknown, so no rewrite takes it that no branch is there.
    pub fn tips_loaded(&mut self, seq: u64, result: Result<Vec<Tip>, String>) {
        if self.tips_seq == Some(seq) {
            self.tips_seq = None;
            (self.tips, self.tips_error) = match result {
                Ok(tips) => (Some(tips), None),
                Err(error) => (None, Some(error)),
            };
        }
    }

    /// Show `todo` above the log, or none. The cursor stays on its row
    /// when there still is one.
    pub fn set_todo(&mut self, todo: Option<RebaseTodo>) {
        let rows = todo.as_ref().map_or(0, RebaseTodo::len);
        self.todo_selected = self
            .todo_selected
            .filter(|_| rows > 0)
            .map(|i| i.min(rows - 1));
        self.todo = todo;
    }

    /// Move the cursor down (`1`) or up (`-1`) one row, through the todo
    /// rows and on into the log, which starts right below them.
    pub fn step(&mut self, delta: isize) {
        let rows = self.todo.as_ref().map_or(0, RebaseTodo::len);
        match (self.todo_selected, delta) {
            (Some(0), 1) => {
                self.todo_selected = None;
                // The log's first row the filter shows.
                self.selected = self.visible().first().copied().unwrap_or(0);
            }
            (Some(i), 1) => self.todo_selected = Some(i - 1),
            (Some(i), _) => self.todo_selected = Some((i + 1).min(rows.saturating_sub(1))),
            (None, -1)
                if rows > 0
                    && self
                        .visible()
                        .first()
                        .is_none_or(|&top| top == self.selected) =>
            {
                self.todo_selected = Some(0)
            }
            (None, _) => self.move_by(delta),
        }
    }

    pub fn move_by(&mut self, delta: isize) {
        if let Some(next) = list_filter::step(&self.visible(), self.selected, delta) {
            self.selected = next;
        }
    }

    /// What the filter matches a row against: what the row shows of it.
    fn filter_text(row: &CommitRow) -> String {
        format!("{} {} {}", row.short_oid, row.subject, row.author)
    }

    /// The indexes of the rows the filter shows, in log order.
    pub fn visible(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            return (0..self.rows.len()).collect();
        }
        let texts: Vec<String> = self.rows.iter().map(Self::filter_text).collect();
        list_filter::visible(&self.filter, texts.iter().map(String::as_str))
    }

    /// The filter hides the cursor's row, which happens only when it hides
    /// every row.
    pub fn selected_hidden(&self) -> bool {
        !self.filter.is_empty()
            && self
                .rows
                .get(self.selected)
                .is_some_and(|row| !list_filter::matches(&self.filter, &Self::filter_text(row)))
    }

    /// The rows the range covers, in log order; empty without one.
    pub fn range(&self) -> Vec<usize> {
        let Some(anchor) = &self.range_anchor else {
            return Vec::new();
        };
        match self.rows.iter().position(|row| &row.oid == anchor) {
            Some(at) => list_filter::range(&self.visible(), at, self.selected),
            None => Vec::new(),
        }
    }

    /// After the filter or the rows changed: a range whose first commit is
    /// gone is cleared, and a hidden cursor moves to the first row shown.
    pub fn reconcile(&mut self) {
        if let Some(anchor) = &self.range_anchor
            && !self.rows.iter().any(|row| &row.oid == anchor)
        {
            self.range_anchor = None;
        }
        if self.selected_hidden()
            && let Some(&first) = self.visible().first()
        {
            self.selected = first;
        }
    }

    /// The `skip` of the next page, when the cursor is near the end of what
    /// is loaded, more exists, and no page is already on its way.
    pub fn wants_more(&self) -> Option<usize> {
        let near_end = self.selected + PREFETCH >= self.rows.len();
        let ready = self.loaded && !self.done && self.page_seq.is_none();
        (ready && near_end && !self.rows.is_empty()).then_some(self.rows.len())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn set(oids: &[&str]) -> Option<HashSet<String>> {
        Some(oids.iter().map(|s| s.to_string()).collect())
    }

    fn panel(unmerged: Option<HashSet<String>>, unpushed: Option<HashSet<String>>) -> CommitsPanel {
        CommitsPanel {
            markers: Markers {
                main: None,
                unmerged,
                unpushed,
            },
            ..CommitsPanel::default()
        }
    }

    #[test]
    fn a_new_query_drops_what_was_loaded_but_keeps_the_fuzzy_filter() {
        let row = CommitRow {
            oid: "a1".into(),
            short_oid: "a1".into(),
            parents: vec![],
            author: "Ann".into(),
            time: 0,
            subject: "s".into(),
            subject_altered: false,
        };
        let mut panel = CommitsPanel::default();
        panel.begin_page(1, 0);
        panel.page_loaded(
            1,
            0,
            Ok((
                LogPage {
                    rows: vec![row.clone(), row],
                    done: true,
                },
                5,
            )),
        );
        panel.selected = 1;
        panel.range_anchor = Some("a1".into());
        panel.filter = "s".into();
        panel.tab = CommitsTab::Reflog;
        let query = LogQuery {
            path: None,
            author: Some("ann".into()),
        };

        panel.set_query(query.clone());

        assert_eq!(panel.query, query);
        assert!(panel.rows.is_empty() && panel.graph.is_empty());
        assert_eq!((panel.selected, panel.lanes), (0, 0));
        assert!(!panel.loaded && !panel.done && panel.tip.is_none());
        assert!(panel.range_anchor.is_none() && panel.open.is_none());
        assert_eq!(panel.filter, "s");
        assert_eq!(panel.tab, CommitsTab::Reflog);
    }

    #[test]
    fn a_commit_outside_the_unmerged_set_is_merged() {
        assert_eq!(panel(set(&["b"]), set(&["a"])).marker("a"), Marker::Merged);
    }

    #[test]
    fn an_unmerged_commit_in_the_unpushed_set_is_unpushed() {
        let panel = panel(set(&["a"]), set(&["a"]));
        assert_eq!(panel.marker("a"), Marker::Unpushed);
    }

    #[test]
    fn an_unmerged_commit_outside_a_known_unpushed_set_is_pushed() {
        assert_eq!(panel(set(&["a"]), set(&[])).marker("a"), Marker::Pushed);
        assert_eq!(panel(None, set(&["b"])).marker("a"), Marker::Pushed);
    }

    #[test]
    fn a_failed_tips_read_is_kept_apart_from_a_pending_one() {
        let mut panel = CommitsPanel {
            tips_seq: Some(3),
            ..CommitsPanel::default()
        };
        panel.tips_loaded(2, Ok(vec![]));
        assert_eq!((panel.tips.is_some(), panel.tips_seq), (false, Some(3)));
        panel.tips_loaded(3, Err("boom".into()));
        assert_eq!(panel.tips, None);
        assert_eq!(panel.tips_error.as_deref(), Some("boom"));
        panel.tips_seq = Some(4);
        panel.tips_loaded(4, Ok(vec![]));
        assert_eq!(panel.tips, Some(vec![]));
        assert_eq!(panel.tips_error, None);
    }

    #[test]
    fn with_neither_set_known_the_marker_is_unknown() {
        assert_eq!(panel(None, None).marker("a"), Marker::Unknown);
        assert_eq!(panel(set(&["a"]), None).marker("a"), Marker::Unknown);
    }
}
