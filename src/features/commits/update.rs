//! The Commits panel's `update` handlers: paging the log, the markers and
//! branch tips that land beside it, and switching to the Reflog tab.

use super::CommitsTab;
use super::git::Tip;
use crate::effect::{Effect, Tag};
use crate::git::log::{LogPage, Markers};
use crate::model::{AppState, Context};

/// The Commits panel: its log pages, markers and branch tips, and its tabs.
impl AppState {
    /// Load page 0 of the log under the current query, superseding any
    /// page in flight.
    pub(crate) fn first_commits_page(&mut self) -> Effect {
        let tag = self.background_tag();
        self.commits.begin_page(tag.seq, 0);
        Effect::LoadCommits {
            tag,
            tip: None,
            skip: 0,
            query: self.commits.query.clone(),
        }
    }

    /// Show the Commits panel's other tab, reading the reflog the first
    /// time it is shown.
    pub(crate) fn switch_commits_tab(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        self.commits.tab = self.commits.tab.other();
        let reflog = &self.commits.reflog;
        let unread = reflog.data.is_none() && reflog.error.is_none() && reflog.seq.is_none();
        if self.commits.tab == CommitsTab::Reflog && unread {
            self.load_reflog(true)
        } else {
            vec![]
        }
    }

    /// Load the next page when the cursor nears the end of the log.
    pub(crate) fn more_commits(&mut self) -> Vec<Effect> {
        let Some(skip) = self.commits.wants_more() else {
            return vec![];
        };
        let tag = self.background_tag();
        self.commits.begin_page(tag.seq, skip);
        vec![Effect::LoadCommits {
            tag,
            tip: self.commits.tip.clone(),
            skip,
            query: self.commits.query.clone(),
        }]
    }

    /// A log page landed: take it if it is still current, then ask for the
    /// next one if the cursor is already near the end.
    pub(crate) fn commits_loaded(
        &mut self,
        tag: Tag,
        skip: usize,
        result: Result<(LogPage, i64), String>,
    ) -> Vec<Effect> {
        if tag.generation != self.generation {
            return vec![];
        }
        self.commits.page_loaded(tag.seq, skip, result);
        self.commits.reconcile();
        self.more_commits()
    }

    /// The merged/pushed markers landed.
    pub(crate) fn markers_loaded(&mut self, tag: Tag, result: Result<Markers, String>) {
        if tag.generation == self.generation {
            self.commits.markers_loaded(tag.seq, result);
        }
    }

    /// The branch tips landed; the history rewrites check them.
    pub(crate) fn branch_tips_loaded(&mut self, tag: Tag, result: Result<Vec<Tip>, String>) {
        if tag.generation == self.generation {
            self.commits.tips_loaded(tag.seq, result);
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;

    use super::*;
    use crate::features::commits::panel::{PAGE, PREFETCH};
    use crate::git::log::Markers;
    use crate::intent::AppIntent;
    use crate::keymap::KeyScope;
    use crate::model::Focus;
    use crate::test_support::{
        TOP, commits_load, commits_loaded, markers_tag, page, press, with_commits,
    };

    #[test]
    fn startup_loads_the_first_page_and_the_markers_without_waiting_for_focus() {
        let mut state =
            AppState::new(Context::Workspace).with_location(Some(TOP.into()), TOP.into());
        let effects = state.update(AppIntent::Start);
        let (tag, tip, skip) = commits_load(&effects);
        assert_eq!((tip, skip), (None, 0));
        assert!(!tag.user, "the panel loads quietly");
        markers_tag(&effects);
        assert_eq!(state.focus, Focus::Files);
    }

    #[test]
    fn a_loaded_page_fills_the_rows_and_the_graph() {
        let state = with_commits(3, true);
        let commits = &state.commits;
        assert!(commits.loaded && commits.done);
        assert_eq!(commits.rows.len(), 3);
        assert_eq!(commits.graph.len(), 3);
        assert_eq!(commits.tip.as_deref(), Some("c0"));
        assert_eq!(commits.loaded_at, 1_000);
        assert_eq!(commits.page_seq, None);
    }

    #[test]
    fn j_and_k_move_the_commit_cursor_within_the_list() {
        let mut state = with_commits(3, true);
        assert_eq!(state.key_focus(), KeyScope::Commits);
        for _ in 0..5 {
            press(&mut state, KeyCode::Char('j'));
        }
        assert_eq!(state.commits.selected, 2);
        press(&mut state, KeyCode::Up);
        assert_eq!(state.commits.selected, 1);
    }

    #[test]
    fn nearing_the_end_loads_the_next_page_once_from_the_same_tip() {
        let mut state = with_commits(PAGE, false);
        state.commits.selected = PAGE - PREFETCH - 2;
        assert_eq!(state.update(AppIntent::CommitNext), vec![]);

        let effects = state.update(AppIntent::CommitNext);
        let (tag, tip, skip) = commits_load(&effects);
        assert_eq!((tip.as_deref(), skip), (Some("c0"), PAGE));
        assert_eq!(effects.len(), 1);
        // Not again while that page is on its way.
        assert_eq!(state.update(AppIntent::CommitNext), vec![]);
        assert!(state.commits.more_in_flight);

        state.update(commits_loaded(tag, PAGE, page(PAGE, 10, true)));
        assert_eq!(state.commits.rows.len(), PAGE + 10);
        assert_eq!(state.commits.graph.len(), PAGE + 10);
        assert_eq!(state.commits.rows[PAGE].oid, format!("c{PAGE}"));
        assert!(!state.commits.more_in_flight);
    }

    #[test]
    fn a_done_page_stops_further_loads() {
        let mut state = with_commits(10, true);
        for _ in 0..20 {
            assert_eq!(state.update(AppIntent::CommitNext), vec![]);
        }
        assert_eq!(state.commits.selected, 9);
    }

    #[test]
    fn stale_pages_and_markers_are_dropped() {
        let mut state = with_commits(2, true);
        let effects = state.update(AppIntent::RefreshStatus);
        let (old, _, _) = commits_load(&effects);
        let old_markers = markers_tag(&effects);
        let effects = state.update(AppIntent::RefreshStatus);
        let (new, _, _) = commits_load(&effects);
        let new_markers = markers_tag(&effects);

        state.update(commits_loaded(old, 0, page(50, 3, true)));
        let unpushed = Markers {
            unpushed: Some(["c0".to_string()].into()),
            ..Markers::default()
        };
        state.update(AppIntent::MarkersLoaded {
            tag: old_markers,
            result: Ok(unpushed.clone()),
        });
        assert_eq!(
            state.commits.rows[0].oid, "c0",
            "the superseded page is dropped"
        );
        assert_eq!(state.commits.markers, Markers::default());

        // Right seq, but for a repository left since.
        let mut other = state.clone();
        other.generation += 1;
        other.update(commits_loaded(new, 0, page(50, 3, true)));
        other.update(AppIntent::MarkersLoaded {
            tag: new_markers,
            result: Ok(unpushed.clone()),
        });
        assert_eq!(other.commits.rows[0].oid, "c0");
        assert_eq!(other.commits.markers, Markers::default());

        state.update(commits_loaded(new, 0, page(50, 3, true)));
        state.update(AppIntent::MarkersLoaded {
            tag: new_markers,
            result: Ok(unpushed.clone()),
        });
        assert_eq!(state.commits.rows[0].oid, "c50");
        assert_eq!(state.commits.markers, unpushed);
    }

    #[test]
    fn a_reload_keeps_the_old_rows_until_it_lands_then_the_cursor_on_the_same_commit() {
        let mut state = with_commits(5, true);
        state.commits.selected = 2;
        let (tag, _, _) = commits_load(&state.update(AppIntent::RefreshStatus));
        assert_eq!(state.commits.rows.len(), 5, "shown while the reload runs");

        // A new commit on top moves c2 one row down.
        let mut newer = page(1, 4, true);
        newer.rows.insert(0, page(99, 1, true).rows.remove(0));
        newer.rows.splice(1..1, page(0, 1, true).rows);
        state.update(commits_loaded(tag, 0, newer));
        assert_eq!(state.commits.rows[state.commits.selected].oid, "c2");
        assert_eq!(state.commits.tip.as_deref(), Some("c99"));

        // Gone: the cursor stays near where it was, inside the list.
        state.commits.selected = 5;
        let (tag, _, _) = commits_load(&state.update(AppIntent::RefreshStatus));
        state.update(commits_loaded(tag, 0, page(10, 2, true)));
        assert_eq!(state.commits.selected, 1);
    }
}
