//! The Commits panel's state: the history of `HEAD` loaded so far, its
//! graph, the cursor and the push/merge markers. Pure; `AppState` turns
//! its answers into effects.

use crate::commit_files::CommitFilesList;
use crate::commit_graph::{GraphBuilder, GraphRow};
use crate::git::commit_files::CommitId;
use crate::git::log::{CommitRow, LogPage, Markers};
use crate::reflog::ReflogList;

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
    /// Which list the panel shows.
    pub tab: CommitsTab,
    /// `HEAD`'s reflog, read when its tab is first shown or an undo asks.
    pub reflog: ReflogList,
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

    pub fn move_by(&mut self, delta: isize) {
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.rows.len().saturating_sub(1));
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
    fn with_neither_set_known_the_marker_is_unknown() {
        assert_eq!(panel(None, None).marker("a"), Marker::Unknown);
        assert_eq!(panel(set(&["a"]), None).marker("a"), Marker::Unknown);
    }
}
