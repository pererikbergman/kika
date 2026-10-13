//! The Commits panel's `update` handlers: paging the log, the markers and
//! branch tips that land beside it, switching to the Reflog tab, and the
//! selected commit's files opened in place of the log.

use super::git::Tip;
use super::{CommitFilesList, CommitsTab};
use crate::diff_mark::{DiffEnd, DiffMark};
use crate::diff_pane::DiffPane;
use crate::effect::{Effect, Tag};
use crate::git::commit_files::{CommitFile, CommitId, CommitRange};
use crate::git::diff::DiffRequest;
use crate::git::log::{LogPage, Markers};
use crate::model::{AppState, Context, Focus};

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

/// A commit opened to its files from the Commits panel. The Diff pane
/// shows the selected file's change in that commit until it closes.
impl AppState {
    /// Open the selected commit's files in place of the log. A user
    /// effect, so the busy indicator shows while git lists them.
    pub(crate) fn open_commit_files(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace
            || self.focus != Focus::Commits
            || self.commits.open.is_some()
        {
            return vec![];
        }
        let Some(row) = self.commits.rows.get(self.commits.selected) else {
            return vec![];
        };
        if let Some(mark) = &self.diff_mark
            && let Some(id) = CommitId::parse(&row.oid)
            && !matches!(mark, DiffMark::Commit { id: marked, .. } if *marked == id)
        {
            let target = row.short_oid.clone();
            return self.open_comparison(DiffEnd::Commit(id), target);
        }
        let range = match CommitRange::of(row) {
            Ok(range) => range,
            Err(message) => {
                self.commits.error = Some(message);
                return vec![];
            }
        };
        let short_oid = row.short_oid.clone();
        let seq = self.take_seq();
        self.commits.open = Some(CommitFilesList::begin(range.clone(), short_oid, seq));
        self.busy += 1;
        let tag = Tag {
            generation: self.generation,
            seq,
            user: true,
        };
        vec![Effect::LoadCommitFiles { tag, range }]
    }

    /// Show the first file's diff, or nothing for a commit with no files.
    pub(crate) fn commit_files_loaded(
        &mut self,
        tag: Tag,
        result: Result<Vec<CommitFile>, String>,
    ) -> Vec<Effect> {
        // Every user effect reports back once, stale or not.
        if tag.user {
            self.busy = self.busy.saturating_sub(1);
        }
        if tag.generation != self.generation {
            return vec![];
        }
        let Some(open) = &mut self.commits.open else {
            return vec![];
        };
        if !open.loaded(tag.seq, result) {
            return vec![];
        }
        if open.selected_file().is_none() {
            self.diff = DiffPane::default();
            return vec![Effect::CancelDiff];
        }
        // Another commit's file: start at its top, whatever was shown.
        self.diff.path = None;
        self.commit_file_diff()
    }

    pub(crate) fn move_commit_file(&mut self, delta: isize) -> Vec<Effect> {
        let moved = self
            .commits
            .open
            .as_mut()
            .is_some_and(|open| open.move_by(delta));
        if moved {
            self.commit_file_diff()
        } else {
            vec![]
        }
    }

    /// Stream the selected commit file's diff, if one is selected.
    pub(crate) fn commit_file_diff(&mut self) -> Vec<Effect> {
        let request = self.commits.open.as_ref().and_then(|open| {
            let file = open.selected_file()?;
            Some(DiffRequest::for_commit_file(
                &open.range,
                file,
                self.diff_options,
            ))
        });
        match request {
            Some(request) => self.stream_diff(request),
            None => vec![],
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
        TOP, commits_load, commits_loaded, diff_request, files_load, markers_tag, on_commit_five,
        opened, page, press, range_of, two_commit_files, with_commits,
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

    // --- A commit's files ---

    #[test]
    fn enter_opens_the_commit_against_its_first_parent_as_a_user_load() {
        let mut state = on_commit_five();
        let busy = state.busy;

        let (tag, range) = files_load(&press(&mut state, KeyCode::Enter));

        assert_eq!(range, range_of("c5", "c6"));
        assert!(tag.user);
        assert_eq!(state.busy, busy + 1);
        let open = state.commits.open.as_ref().unwrap();
        assert_eq!((open.short_oid.as_str(), open.loaded), ("c5", false));
        assert_eq!(state.key_focus(), KeyScope::CommitFiles);
        assert_eq!(state.commits.selected, 5);
    }

    #[test]
    fn a_commit_whose_id_or_parent_is_not_hex_opens_nothing_and_says_why() {
        for (oid, parent) in [("--output=x", "c6"), ("c5", "-p")] {
            let mut state = on_commit_five();
            state.commits.rows[5].oid = oid.into();
            state.commits.rows[5].parents = vec![parent.into()];

            assert_eq!(state.update(AppIntent::OpenCommitFiles), vec![]);

            assert_eq!(state.commits.open, None);
            let error = state.commits.error.as_deref().unwrap();
            assert!(error.starts_with("not a commit id"), "{error}");
        }
    }

    #[test]
    fn enter_with_no_commits_is_disabled() {
        let mut state = on_commit_five();
        state.commits.rows.clear();
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.notice, Some("No commits".into()));
    }

    #[test]
    fn the_loaded_files_show_the_first_ones_diff_in_that_commit_and_j_the_next() {
        let mut state = on_commit_five();
        let busy = state.busy;
        let range = range_of("c5", "c6");

        let effects = opened(&mut state, two_commit_files());

        assert_eq!(state.busy, busy);
        let request = diff_request(&effects);
        assert_eq!(request.commit, Some(range.clone()));
        assert_eq!(request.path, "src/lib.rs".into());
        assert_eq!(state.diff.path, Some("src/lib.rs".into()));

        let request = diff_request(&press(&mut state, KeyCode::Char('j')));
        assert_eq!(request.commit, Some(range));
        assert_eq!(
            (request.path, request.orig_path),
            ("new.rs".into(), Some("old.rs".into()))
        );
        // The ends of the list load nothing more.
        assert_eq!(press(&mut state, KeyCode::Down), vec![]);
        let request = diff_request(&state.update(AppIntent::CommitFilePrev));
        assert_eq!(request.path, "src/lib.rs".into());
    }

    #[test]
    fn a_stale_or_other_repositorys_file_list_is_dropped_but_lowers_busy() {
        let stale: [fn(Tag) -> Tag; 2] = [
            |tag| Tag {
                seq: tag.seq + 1,
                ..tag
            },
            |tag| Tag {
                generation: tag.generation + 1,
                ..tag
            },
        ];
        for stale in stale {
            let mut state = on_commit_five();
            let (tag, _) = files_load(&press(&mut state, KeyCode::Enter));
            let busy = state.busy;
            let effects = state.update(AppIntent::CommitFilesLoaded {
                tag: stale(tag),
                result: Ok(two_commit_files()),
            });
            assert_eq!(effects, vec![]);
            assert_eq!(state.busy, busy - 1);
            assert!(!state.commits.open.as_ref().unwrap().loaded);
        }
    }

    #[test]
    fn a_commit_with_no_files_or_a_failed_list_empties_the_diff() {
        for result in [Ok(vec![]), Err("bad object".to_string())] {
            let mut state = on_commit_five();
            let (tag, _) = files_load(&press(&mut state, KeyCode::Enter));
            let effects = state.update(AppIntent::CommitFilesLoaded { tag, result });
            assert_eq!(effects, vec![Effect::CancelDiff]);
            assert_eq!(state.diff, DiffPane::default());
        }
    }
}
