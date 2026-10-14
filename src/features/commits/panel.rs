//! The Commits panel's state: the history of `HEAD` loaded so far, its
//! graph, the cursor and the push/merge markers. Pure; `AppState` turns
//! its answers into effects.

use super::commit_files::CommitFilesList;
use super::git::Tip;
use super::graph::{GraphBuilder, GraphRow};
use super::reflog::ReflogList;
use super::rewrite::RebaseTodo;
use crate::effect::Tag;
use crate::filtered_list::{FilteredList, Row};
use kika_git::commit_files::CommitId;
use kika_git::log::{CommitRow, LogPage, LogQuery, Markers};

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

/// A log row in the list: it follows its commit by full id, and a failed
/// page 0 keeps the log shown.
impl Row for CommitRow {
    type Key = String;
    const KEEP_ROWS_ON_ERROR: bool = true;
    fn key(&self) -> String {
        self.oid.clone()
    }
    /// What the row shows of it.
    fn filter_text(&self) -> String {
        format!("{} {} {}", self.short_oid, self.subject, self.author)
    }
}

/// Everything the panel shows. Rows only grow by whole pages, in log
/// order, so the graph can be extended without looking back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitsPanel {
    /// The log loaded so far, with its cursor, fuzzy filter, range and
    /// the page load in flight.
    pub list: FilteredList<CommitRow>,
    /// One per row, in step with `list.rows`.
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
    /// The page in flight comes after the rows shown, not in their place.
    pub more_in_flight: bool,
    pub markers: Markers,
    pub markers_seq: Option<u64>,
    /// Unix time page 0 loaded, which relative dates count from.
    pub loaded_at: i64,
    /// A commit opened to its files, shown in place of the log. The cursor
    /// stays where it was, so closing it returns to the same commit.
    pub open: Option<CommitFilesList>,
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
    /// What git narrows the log to. Unlike the list's filter, it changes which
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
        self.list
            .rows
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
        self.list.rows.clear();
        self.list.selected = 0;
        self.list.range_anchor = None;
        self.list.loaded = false;
        self.list.error = None;
        self.graph.clear();
        self.builder = GraphBuilder::default();
        self.lanes = 0;
        self.tip = None;
        self.done = false;
        self.open = None;
    }

    /// Page `skip` was asked for under `seq`.
    pub fn begin_page(&mut self, seq: u64, skip: usize) {
        self.list.begin(seq);
        self.more_in_flight = skip > 0;
    }

    /// Apply a page if it is the one in flight in this repository's
    /// `generation`. Page 0 replaces the rows, keeping the cursor on the
    /// same commit when it is still there; a later page is appended.
    /// Returns whether it was applied.
    pub fn page_loaded(
        &mut self,
        tag: Tag,
        generation: u64,
        skip: usize,
        result: Result<(LogPage, i64), String>,
    ) -> bool {
        let (rows, done, now) = match result {
            Ok((page, now)) => (Ok(page.rows), page.done, now),
            // A failed later page ends the log: retrying on every key
            // press would only repeat it; the next reload tries again.
            // (`done` and `now` are not used for a failed page 0.)
            Err(message) => (Err(message), true, 0),
        };
        let failed = rows.is_err();
        let applied = if skip == 0 {
            let rows = rows.map_err(|m| format!("Could not read the history: {m}"));
            let applied = self.list.loaded(tag, generation, rows);
            if applied && !failed {
                self.graph.clear();
                self.builder = GraphBuilder::default();
                self.lanes = 0;
                self.extend_graph(0);
                self.tip = self.list.rows.first().map(|row| row.oid.clone());
                self.done = done;
                self.loaded_at = now;
            }
            applied
        } else {
            let rows = rows.map_err(|m| format!("Could not load more commits: {m}"));
            let applied = self.list.append(tag, generation, skip, rows);
            if applied {
                self.done = done;
                if !failed {
                    self.extend_graph(skip);
                }
            }
            applied
        };
        if self.list.seq.is_none() {
            self.more_in_flight = false;
        }
        applied
    }

    /// Graph rows for the rows from `from` on, which are new.
    fn extend_graph(&mut self, from: usize) {
        for row in &self.list.rows[from..] {
            let graph = self.builder.push(row);
            self.lanes = self.lanes.max(graph.cells.len());
            self.graph.push(graph);
        }
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
                self.list.selected = self.list.visible().first().copied().unwrap_or(0);
            }
            (Some(i), 1) => self.todo_selected = Some(i - 1),
            (Some(i), _) => self.todo_selected = Some((i + 1).min(rows.saturating_sub(1))),
            (None, -1)
                if rows > 0
                    && self
                        .list
                        .visible()
                        .first()
                        .is_none_or(|&top| top == self.list.selected) =>
            {
                self.todo_selected = Some(0)
            }
            (None, _) => {
                self.list.move_by(delta);
            }
        }
    }

    /// The `skip` of the next page, when the cursor is near the end of what
    /// is loaded, more exists, and no page is already on its way.
    pub fn wants_more(&self) -> Option<usize> {
        let list = &self.list;
        let near_end = list.selected + PREFETCH >= list.rows.len();
        let ready = list.loaded && !self.done && list.seq.is_none();
        (ready && near_end && !list.rows.is_empty()).then_some(list.rows.len())
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
            Tag {
                generation: 0,
                seq: 1,
                user: false,
            },
            0,
            0,
            Ok((
                LogPage {
                    rows: vec![row.clone(), row],
                    done: true,
                },
                5,
            )),
        );
        panel.list.selected = 1;
        panel.list.range_anchor = Some("a1".into());
        panel.list.filter = "s".into();
        panel.tab = CommitsTab::Reflog;
        let query = LogQuery {
            path: None,
            author: Some("ann".into()),
        };

        panel.set_query(query.clone());

        assert_eq!(panel.query, query);
        assert!(panel.list.rows.is_empty() && panel.graph.is_empty());
        assert_eq!((panel.list.selected, panel.lanes), (0, 0));
        assert!(!panel.list.loaded && !panel.done && panel.tip.is_none());
        assert!(panel.list.range_anchor.is_none() && panel.open.is_none());
        assert_eq!(panel.list.filter, "s");
        assert_eq!(panel.tab, CommitsTab::Reflog);
    }

    /// A straight line of commits `from..to`, each the child of the next.
    fn line(from: usize, to: usize, done: bool) -> Result<(LogPage, i64), String> {
        let rows = (from..to)
            .map(|i| CommitRow {
                oid: format!("c{i}"),
                short_oid: format!("c{i}"),
                parents: vec![format!("c{}", i + 1)],
                author: "Ann".into(),
                time: 0,
                subject: format!("commit {i}"),
                subject_altered: false,
            })
            .collect();
        Ok((LogPage { rows, done }, 5))
    }

    #[test]
    fn graph_rows_stay_in_step_across_pages_and_a_reload() {
        let tag = |seq| Tag {
            generation: 0,
            seq,
            user: false,
        };
        let mut panel = CommitsPanel::default();
        panel.begin_page(1, 0);
        assert!(panel.page_loaded(tag(1), 0, 0, line(0, 3, false)));
        assert_eq!(panel.graph.len(), panel.list.rows.len());

        panel.begin_page(2, 3);
        assert!(panel.page_loaded(tag(2), 0, 3, line(3, 5, false)));
        assert_eq!((panel.list.rows.len(), panel.graph.len()), (5, 5));

        panel.list.move_by(4);
        panel.begin_page(3, 0);
        assert!(panel.page_loaded(tag(3), 0, 0, line(1, 5, false)));
        assert_eq!((panel.list.rows.len(), panel.graph.len()), (4, 4));
        assert_eq!(panel.tip.as_deref(), Some("c1"));
        assert_eq!(panel.list.selected, 3, "the cursor followed c4");

        let (rows, graph) = (panel.list.rows.clone(), panel.graph.clone());
        panel.begin_page(4, 4);
        assert!(panel.more_in_flight);
        assert!(panel.page_loaded(tag(4), 0, 4, Err("gone".into())));
        assert_eq!(
            (panel.list.rows.clone(), panel.graph.clone()),
            (rows, graph)
        );
        assert_eq!(panel.list.selected, 3);
        assert!(panel.done && !panel.more_in_flight);
        assert_eq!(
            panel.list.error.as_deref(),
            Some("Could not load more commits: gone")
        );
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
