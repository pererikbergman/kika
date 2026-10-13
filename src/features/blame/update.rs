//! The Blame feature's `update` handlers: opening a blame and taking its
//! load, moving its cursor, and opening, scrolling and closing the commit
//! of a line.

use super::keys::blame_disabled;
use super::state::{BlameView, CommitView};
use crate::effect::{Effect, Tag};
use crate::git::blame::BlameLine;
use crate::git::diff::DiffLine;
use crate::intent::Scroll;
use crate::model::{AppState, Context, Focus};

/// The blame and the commit opened from it.
impl AppState {
    /// Blame the selected file from the Files panel. A user effect, so the
    /// busy indicator shows while git works.
    pub(crate) fn open_blame(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace || self.focus != Focus::Files {
            return vec![];
        }
        if let Some(reason) = blame_disabled(self) {
            self.notice = Some(reason);
            return vec![];
        }
        let Some(path) = self
            .selected
            .and_then(|i| self.files.get(i))
            .map(|f| f.path.clone())
        else {
            return vec![];
        };
        let seq = self.take_seq();
        self.focus = Focus::Blame(BlameView::new(path.clone(), seq));
        let tag = self.user_tag(seq);
        vec![Effect::LoadBlame { tag, path }]
    }

    pub(crate) fn blame_loaded(&mut self, tag: Tag, result: Result<Vec<BlameLine>, String>) {
        // Every user effect reports back once, stale or not.
        self.user_done(tag);
        let Focus::Blame(view) = &mut self.focus else {
            return;
        };
        if tag.generation != self.generation || view.seq != Some(tag.seq) {
            return;
        }
        view.loaded(result);
    }

    /// Change the blame's cursor, given the pane's rows.
    pub(crate) fn with_blame(&mut self, change: impl FnOnce(&mut BlameView, usize)) {
        let rows = self.pane_rows();
        if let Focus::Blame(view) = &mut self.focus
            && view.commit.is_none()
        {
            change(view, rows);
        }
    }

    /// Open the commit that last changed the cursor's line. A line edited
    /// in the worktree has no commit to show.
    pub(crate) fn open_blame_commit(&mut self) -> Vec<Effect> {
        let Focus::Blame(view) = &self.focus else {
            return vec![];
        };
        let Some(line) = view.selected().filter(|_| view.commit.is_none()) else {
            return vec![];
        };
        if line.is_uncommitted() {
            self.notice = Some("Not committed yet".into());
            return vec![];
        }
        let oid = line.oid.clone();
        let seq = self.take_seq();
        if let Focus::Blame(view) = &mut self.focus {
            view.commit = Some(CommitView::new(oid.clone(), seq));
        }
        let tag = self.user_tag(seq);
        vec![Effect::LoadCommit { tag, oid }]
    }

    pub(crate) fn commit_loaded(&mut self, tag: Tag, result: Result<Vec<DiffLine>, String>) {
        self.user_done(tag);
        if tag.generation != self.generation {
            return;
        }
        if let Focus::Blame(BlameView {
            commit: Some(commit),
            ..
        }) = &mut self.focus
            && commit.seq == Some(tag.seq)
        {
            commit.loaded(result);
        }
    }

    /// Close the commit, back on the same blame line.
    pub(crate) fn close_blame_commit(&mut self) {
        if let Focus::Blame(view) = &mut self.focus {
            view.commit = None;
        }
    }

    /// Scroll the open commit, given the pane's rows.
    pub(crate) fn scroll_commit(&mut self, by: Scroll) {
        let rows = self.pane_rows();
        if let Focus::Blame(BlameView {
            commit: Some(commit),
            ..
        }) = &mut self.focus
        {
            commit.scroll(by, rows);
        }
    }

    /// Back to the side panel the blame was opened from.
    pub(crate) fn close_blame(&mut self) -> Vec<Effect> {
        if matches!(self.focus, Focus::Blame(_)) {
            self.focus = Focus::side(self.expanded);
            return self.render_if_stale();
        }
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::path::RepoPath;
    use crate::intent::AppIntent;
    use crate::keymap::KeyScope;
    use crate::test_support::*;
    use crossterm::event::KeyCode;

    #[test]
    fn b_on_a_tracked_modified_file_opens_its_blame_and_loads_it_once() {
        let (state, _) = blaming(24);
        let view = blame(&state);
        assert_eq!(view.path, RepoPath::from("a.rs"));
        assert!(!view.loaded);
        assert_eq!(state.panel_focus(), KeyScope::Blame);
        assert_eq!(state.busy_count(), 1);
    }

    #[test]
    fn b_on_an_untracked_or_deleted_file_says_why_and_does_nothing() {
        for entry in [
            untracked("new.txt"),
            changed("gone.rs", '.', 'D'),
            changed("staged-gone.rs", 'D', '.'),
        ] {
            let (mut state, _) = with_files(vec![entry.clone()], 24);
            let effects = press(&mut state, KeyCode::Char('b'));
            assert_eq!(effects, vec![], "{entry:?}");
            assert_eq!(state.focus, Focus::Files, "{entry:?}");
            assert!(state.notice.is_some(), "{entry:?}");
        }
        let mut state = AppState::new(Context::Workspace);
        assert_eq!(press(&mut state, KeyCode::Char('b')), vec![]);
        assert_eq!(state.notice, Some("No file selected".into()));
    }

    #[test]
    fn a_stale_blame_result_is_ignored_but_still_lowers_busy() {
        let (mut state, tag) = blaming(24);
        let stale = Tag {
            seq: tag.seq + 100,
            ..tag
        };
        state.update(AppIntent::BlameLoaded {
            tag: stale,
            result: Ok(vec![blame_line(OID, 1)]),
        });
        assert!(!blame(&state).loaded);
        assert_eq!(state.busy_count(), 0);

        let old_generation = Tag {
            generation: tag.generation + 1,
            ..tag
        };
        state.update(AppIntent::BlameLoaded {
            tag: old_generation,
            result: Ok(vec![blame_line(OID, 1)]),
        });
        assert!(!blame(&state).loaded);

        state.update(AppIntent::BlameLoaded {
            tag,
            result: Ok(vec![blame_line(OID, 1)]),
        });
        assert!(blame(&state).loaded);
        assert_eq!(blame(&state).lines.len(), 1);
    }

    #[test]
    fn a_blame_result_after_leaving_the_blame_is_dropped() {
        let (mut state, tag) = blaming(24);
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.focus, Focus::Files);
        state.update(AppIntent::BlameLoaded {
            tag,
            result: Ok(vec![blame_line(OID, 1)]),
        });
        assert_eq!(state.focus, Focus::Files);
        assert_eq!(state.busy_count(), 0);
    }

    #[test]
    fn the_blame_cursor_stays_inside_the_lines_and_the_visible_rows() {
        // Height 10: 7 rows in the pane.
        let mut state = blamed(20, 10);
        let rows = state.pane_rows();
        assert_eq!(rows, 7);
        let in_view = |state: &AppState| {
            let view = blame(state);
            assert!(view.cursor < view.lines.len());
            assert!(view.scroll <= view.cursor && view.cursor < view.scroll + rows);
            view.cursor
        };
        press(&mut state, KeyCode::Char('k'));
        assert_eq!(in_view(&state), 0);
        for _ in 0..8 {
            press(&mut state, KeyCode::Char('j'));
        }
        assert_eq!(in_view(&state), 8);
        assert_eq!(blame(&state).scroll, 2);
        press(&mut state, KeyCode::Down);
        press(&mut state, KeyCode::Up);
        assert_eq!(in_view(&state), 8);
        press(&mut state, KeyCode::PageDown);
        assert_eq!(in_view(&state), 15);
        press(&mut state, KeyCode::PageDown);
        assert_eq!(in_view(&state), 19);
        press(&mut state, KeyCode::Char('j'));
        assert_eq!(in_view(&state), 19);
        press(&mut state, KeyCode::PageUp);
        assert_eq!(in_view(&state), 12);
        for _ in 0..3 {
            press(&mut state, KeyCode::PageUp);
        }
        assert_eq!(in_view(&state), 0);
        assert_eq!(blame(&state).scroll, 0);
    }

    #[test]
    fn moving_in_an_empty_or_unloaded_blame_does_nothing() {
        let (mut state, _) = blaming(24);
        for code in [KeyCode::Char('j'), KeyCode::PageDown, KeyCode::Enter] {
            assert_eq!(press(&mut state, code), vec![]);
        }
        assert_eq!(blame(&state).cursor, 0);
        assert_eq!(blame(&state).commit, None);
    }

    #[test]
    fn enter_opens_the_lines_commit_and_esc_goes_back_to_the_same_line_then_files() {
        let mut state = blamed(5, 24);
        press(&mut state, KeyCode::Char('j'));
        press(&mut state, KeyCode::Char('j'));
        let tag = match press(&mut state, KeyCode::Enter).as_slice() {
            [Effect::LoadCommit { tag, oid }] => {
                assert_eq!(oid, OID);
                assert!(tag.user);
                *tag
            }
            other => panic!("expected one LoadCommit, got {other:?}"),
        };
        assert_eq!(state.panel_focus(), KeyScope::BlameCommit);
        assert_eq!(state.busy_count(), 1);
        // The blame's keys are not the commit's: Enter does nothing more.
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);

        let lines = vec![line("commit abc"), line("+added")];
        state.update(AppIntent::CommitLoaded {
            tag,
            result: Ok(lines.clone()),
        });
        assert_eq!(state.busy_count(), 0);
        let commit = blame(&state).commit.as_ref().expect("commit open");
        assert!(commit.loaded);
        assert_eq!(commit.lines, lines);

        press(&mut state, KeyCode::Esc);
        assert_eq!(state.panel_focus(), KeyScope::Blame);
        assert_eq!(blame(&state).cursor, 2);
        assert_eq!(blame(&state).commit, None);

        press(&mut state, KeyCode::Esc);
        assert_eq!(state.focus, Focus::Files);
        assert!(!state.should_quit);
    }

    #[test]
    fn enter_on_an_uncommitted_line_says_so_and_loads_nothing() {
        let mut state = blamed(3, 24);
        press(&mut state, KeyCode::Char('j'));
        press(&mut state, KeyCode::Char('j'));
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.notice, Some("Not committed yet".into()));
        assert_eq!(blame(&state).commit, None);
        assert_eq!(state.panel_focus(), KeyScope::Blame);
    }

    #[test]
    fn a_stale_commit_result_is_ignored() {
        let mut state = blamed(3, 24);
        let tag = match press(&mut state, KeyCode::Enter).as_slice() {
            [Effect::LoadCommit { tag, .. }] => *tag,
            other => panic!("expected one LoadCommit, got {other:?}"),
        };
        state.update(AppIntent::CommitLoaded {
            tag: Tag {
                seq: tag.seq + 100,
                ..tag
            },
            result: Ok(vec![line("x")]),
        });
        assert!(!blame(&state).commit.as_ref().unwrap().loaded);
        assert_eq!(state.busy_count(), 0);
        // Closed before it landed: the result has nowhere to go.
        press(&mut state, KeyCode::Esc);
        state.update(AppIntent::CommitLoaded {
            tag,
            result: Ok(vec![line("x")]),
        });
        assert_eq!(blame(&state).commit, None);
    }

    #[test]
    fn the_commit_view_scrolls_inside_its_lines() {
        let mut state = blamed(3, 10);
        let tag = match press(&mut state, KeyCode::Enter).as_slice() {
            [Effect::LoadCommit { tag, .. }] => *tag,
            other => panic!("expected one LoadCommit, got {other:?}"),
        };
        state.update(AppIntent::CommitLoaded {
            tag,
            result: Ok((0..20).map(|i| line(&i.to_string())).collect()),
        });
        let scroll = |state: &AppState| blame(state).commit.as_ref().unwrap().scroll;
        press(&mut state, KeyCode::Char('j'));
        press(&mut state, KeyCode::Down);
        assert_eq!(scroll(&state), 2);
        press(&mut state, KeyCode::Char('k'));
        assert_eq!(scroll(&state), 1);
        press(&mut state, KeyCode::PageDown);
        press(&mut state, KeyCode::PageDown);
        // 20 lines in 7 rows: the last page starts at 13.
        assert_eq!(scroll(&state), 13);
        press(&mut state, KeyCode::PageUp);
        press(&mut state, KeyCode::Up);
        assert_eq!(scroll(&state), 5);
        // The blame cursor did not move.
        assert_eq!(blame(&state).cursor, 0);
    }
}
