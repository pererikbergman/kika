//! The list filter and range handlers that many features share: open a
//! list's filter, apply and accept the bottom-bar input, start or clear a
//! range, and Esc in a list. They stay in core because they dispatch on
//! every list's scope.
//! It also holds the pane search: opening a pane's search, `n`/`N`, and
//! Esc that clears a pane's search before it closes the pane.

use crate::diff_search::DiffSearch;
use crate::effect::Effect;
use crate::features::blame::BlameView;
use crate::features::commits::compare::MARK_CLEARED;
use crate::features::review;
use crate::git::FileEntry;
use crate::git::log::LogQuery;
use crate::git::path::RepoPath;
use crate::intent::AppIntent;
use crate::keymap::KeyScope;
use crate::list_filter::{self, InputTarget, ListInput};
use crate::model::{AppState, Focus};
use crate::popup::{LineInput, Popup};

/// Said when a changed filter drops the range made over the old rows.
pub(crate) const SELECTION_CLEARED: &str = "Selection cleared";
/// Said when a reload changed which files the Files range covers.
pub(crate) const RANGE_FILES_CHANGED: &str = "Selection cleared: the file list changed";

/// The bottom-bar input for a search, starting from its query.
pub(crate) fn search_input(target: InputTarget, search: &Option<DiffSearch>) -> ListInput {
    let text = search.as_ref().map_or("", |s| s.query.as_str());
    ListInput {
        target,
        line: LineInput::with_text(text),
    }
}

/// A search is open and found nothing, so there is no match to go to.
pub(crate) fn no_matches(search: &Option<DiffSearch>) -> bool {
    search.as_ref().is_some_and(|s| s.current.is_none())
}

/// List filters and range selections.
impl AppState {
    /// The files the range covers, in order; empty without one.
    pub fn files_range(&self) -> Vec<usize> {
        let anchor = self
            .files_anchor
            .as_ref()
            .and_then(|anchor| self.files.iter().position(|f| &f.path == anchor));
        match (anchor, self.selected) {
            (Some(anchor), Some(cursor)) => {
                list_filter::range(&self.files_visible(), anchor, cursor)
            }
            _ => Vec::new(),
        }
    }

    /// The files a Files action on "the selected files" takes: the range
    /// when one is set there, else the file under the cursor. Says which.
    pub(crate) fn files_marked(&self) -> (Vec<&FileEntry>, bool) {
        let from_range = self.files_anchor.is_some() && self.focus == Focus::Files;
        let marked = if from_range {
            self.files_range()
        } else {
            self.selected.into_iter().collect()
        };
        let entries = marked.iter().filter_map(|&i| self.files.get(i)).collect();
        (entries, from_range)
    }

    /// The paths of the files the range covers, in order.
    pub(crate) fn files_range_paths(&self) -> Vec<RepoPath> {
        self.files_range()
            .into_iter()
            .map(|i| self.files[i].path.clone())
            .collect()
    }

    /// Type into the focused list's filter, starting from its text.
    pub(crate) fn open_filter(&mut self) {
        if self.key_focus() == KeyScope::Worktrees {
            if let Some(list) = self.worktree_list() {
                self.input = Some(ListInput {
                    target: InputTarget::WorktreesFilter,
                    line: LineInput::with_text(&list.filter),
                });
            }
            return;
        }
        if self.key_focus() == KeyScope::CommandLog {
            if let Some(Popup::CommandLog(view)) = self.popups.last() {
                self.input = Some(ListInput {
                    target: InputTarget::CommandLogFilter,
                    line: LineInput::with_text(&view.filter),
                });
            }
            return;
        }
        let (target, text) = match self.panel_focus() {
            KeyScope::Files => (InputTarget::FilesFilter, &self.files_filter),
            KeyScope::Commits => (InputTarget::CommitsFilter, &self.commits.filter),
            KeyScope::Branches => (InputTarget::BranchesFilter, &self.branches.filter),
            KeyScope::Remotes => (InputTarget::RemotesFilter, &self.branches.remotes.filter),
            KeyScope::Tags => (InputTarget::TagsFilter, &self.branches.tags.filter),
            KeyScope::Reflog => (InputTarget::ReflogFilter, &self.commits.reflog.filter),
            KeyScope::CommitFiles => match &self.commits.open {
                Some(open) => (InputTarget::CommitFilesFilter, &open.filter),
                None => return,
            },
            KeyScope::Stash => (InputTarget::StashFilter, &self.stash.list.filter),
            KeyScope::Review if review::queue_keys_disabled(self).is_none() => {
                (InputTarget::ReviewFilter, &self.review.filter)
            }
            _ => return,
        };
        self.input = Some(ListInput {
            target,
            line: LineInput::with_text(text),
        });
    }

    /// Filter or search with the input's text as it is now. A changed
    /// filter clears the range, which was made over the rows shown before,
    /// and moves a cursor it hides to the first row it shows.
    pub(crate) fn apply_input(&mut self) -> Vec<Effect> {
        let Some(input) = &self.input else {
            return vec![];
        };
        let text = input.line.text.clone();
        match input.target {
            InputTarget::FilesFilter => {
                if text != self.files_filter {
                    self.files_filter = text;
                    if self.files_anchor.take().is_some() {
                        self.notice = Some(SELECTION_CLEARED.into());
                    }
                }
                if self.files_selected_hidden()
                    && let Some(&first) = self.files_visible().first()
                {
                    return self.select(first);
                }
            }
            InputTarget::CommitsFilter => {
                if text != self.commits.filter {
                    self.commits.filter = text;
                    if self.commits.range_anchor.take().is_some() {
                        self.notice = Some(SELECTION_CLEARED.into());
                    }
                }
                self.commits.reconcile();
            }
            InputTarget::BranchesFilter | InputTarget::RemotesFilter | InputTarget::TagsFilter => {
                return self.apply_branches_filter(input.target, text);
            }
            // A reflog row drives nothing, so a moved cursor loads nothing.
            InputTarget::ReflogFilter => self.commits.reflog.set_filter(text),
            // Only matched; the file the cursor moved to is loaded by its
            // own path, as a step would.
            InputTarget::CommitFilesFilter => {
                let moved = self
                    .commits
                    .open
                    .as_mut()
                    .is_some_and(|open| open.set_filter(text));
                if moved {
                    return self.commit_file_diff();
                }
            }
            // A stash row drives nothing, so a moved cursor loads nothing.
            InputTarget::StashFilter => {
                if self.stash.list.set_filter(text) {
                    self.notice = Some(SELECTION_CLEARED.into());
                }
            }
            InputTarget::DiffSearch => {
                if let Focus::Diff(view) = &mut self.focus {
                    view.set_search(&text, self.emoji_zwj);
                }
            }
            InputTarget::PreviewSearch => self.diff.set_search(&text),
            InputTarget::BlameSearch => {
                if let Focus::Blame(view) = &mut self.focus {
                    view.set_search(&text);
                }
            }
            InputTarget::BlameCommitSearch => {
                if let Focus::Blame(BlameView {
                    commit: Some(commit),
                    ..
                }) = &mut self.focus
                {
                    commit.set_search(&text);
                }
            }
            InputTarget::ReviewFilter => {
                self.review.filter = text;
                self.review.reconcile();
            }
            InputTarget::WorktreesFilter => {
                if let Some(list) = self.worktree_list_mut() {
                    list.filter = text;
                    list.reconcile();
                }
            }
            InputTarget::CommandLogFilter => {
                if let Some(Popup::CommandLog(view)) = self.popups.last_mut() {
                    view.filter = text;
                    view.reconcile(&self.command_log);
                }
            }
            // Git runs the query on Enter, not on every key.
            InputTarget::LogPath | InputTarget::LogAuthor => {}
        }
        vec![]
    }

    /// Close the input. The filter stays as typed; a search goes to its
    /// current match; a log query is read and run, or stays open saying
    /// why it cannot be.
    pub(crate) fn input_accept(&mut self) -> Vec<Effect> {
        let Some(input) = self.input.take() else {
            return vec![];
        };
        if input.target.is_log_query() {
            return self.accept_log_query(input);
        }
        let rows = self.diff_rows();
        let mut effects = vec![];
        let found = match (input.target, &mut self.focus) {
            (InputTarget::DiffSearch, Focus::Diff(view)) => {
                let found = !no_matches(&view.search);
                if found {
                    view.jump_to_match(rows);
                }
                found
            }
            (InputTarget::BlameSearch, Focus::Blame(view)) => {
                let found = !no_matches(&view.search);
                if found {
                    view.jump_to_match(rows);
                }
                found
            }
            (
                InputTarget::BlameCommitSearch,
                Focus::Blame(BlameView {
                    commit: Some(commit),
                    ..
                }),
            ) => {
                let found = !no_matches(&commit.search);
                if found {
                    commit.jump_to_match(rows);
                }
                found
            }
            (InputTarget::PreviewSearch, _) => {
                let found = !no_matches(&self.diff.search);
                if found {
                    effects = self.diff.jump_to_match(rows);
                }
                found
            }
            _ => true,
        };
        if !found {
            self.notice = Some("No matches".into());
        }
        effects
    }

    /// Start a range at the cursor, or clear the one there is.
    pub(crate) fn toggle_range(&mut self) {
        match self.panel_focus() {
            KeyScope::Files if self.files_anchor.is_some() => self.files_anchor = None,
            KeyScope::Files if !self.files_selected_hidden() => {
                self.files_anchor = self
                    .selected
                    .and_then(|i| self.files.get(i))
                    .map(|f| f.path.clone());
            }
            KeyScope::Commits if self.commits.range_anchor.is_some() => {
                self.commits.range_anchor = None;
            }
            KeyScope::Commits if !self.commits.selected_hidden() => {
                let row = self.commits.rows.get(self.commits.selected);
                self.commits.range_anchor = row.map(|r| r.oid.clone());
            }
            KeyScope::Branches => self.branches.toggle_range(),
            KeyScope::Remotes => self.branches.remotes.toggle_range(),
            KeyScope::Tags => self.branches.tags.toggle_range(),
            KeyScope::Stash => self.stash.list.toggle_range(),
            _ => {}
        }
    }

    /// Esc in a list: clear the range, else the filter, else (in Files)
    /// the preview's search, else (in Commits) the diff mark, else the log
    /// query, else quit as Esc always has. In the Worktrees and Command log
    /// popups: clear the filter, else close the popup. The Reflog tab has
    /// no range: clear the filter, else quit.
    pub(crate) fn list_escape(&mut self) -> Vec<Effect> {
        if self.key_focus() == KeyScope::CommandLog {
            if let Some(Popup::CommandLog(view)) = self.popups.last_mut()
                && !view.filter.is_empty()
            {
                view.filter.clear();
                view.reconcile(&self.command_log);
                return vec![];
            }
            return self.update(AppIntent::PopPopup);
        }
        if self.key_focus() == KeyScope::Worktrees {
            if let Some(list) = self.worktree_list_mut()
                && !list.filter.is_empty()
            {
                list.filter.clear();
                return vec![];
            }
            return self.update(AppIntent::PopPopup);
        }
        let focus = self.panel_focus();
        let branches = &mut self.branches;
        let (cleared, filter) = match focus {
            KeyScope::Files => (self.files_anchor.take().is_some(), &mut self.files_filter),
            KeyScope::Commits => (
                self.commits.range_anchor.take().is_some(),
                &mut self.commits.filter,
            ),
            KeyScope::Branches => (branches.range_anchor.take().is_some(), &mut branches.filter),
            KeyScope::Remotes => (
                branches.remotes.range_anchor.take().is_some(),
                &mut branches.remotes.filter,
            ),
            KeyScope::Tags => (
                branches.tags.range_anchor.take().is_some(),
                &mut branches.tags.filter,
            ),
            KeyScope::Reflog => (false, &mut self.commits.reflog.filter),
            KeyScope::Stash => (
                self.stash.list.range_anchor.take().is_some(),
                &mut self.stash.list.filter,
            ),
            _ => return self.update(AppIntent::Quit),
        };
        if cleared {
            return vec![];
        }
        if !filter.is_empty() {
            filter.clear();
            return vec![];
        }
        if focus == KeyScope::Files && self.diff.search.take().is_some() {
            return vec![];
        }
        if self.panel_focus() == KeyScope::Commits {
            if self.diff_mark.take().is_some() {
                self.notice = Some(MARK_CLEARED.into());
                return vec![];
            }
            if self.commits.query.is_active() {
                self.notice = Some("Log filter cleared".into());
                return self.set_log_query(LogQuery::default());
            }
        }
        self.update(AppIntent::Quit)
    }
}

/// The pane search.
impl AppState {
    /// Type a search for the focused pane, starting from its query.
    pub(crate) fn open_search(&mut self) {
        let (target, search) = match &self.focus {
            Focus::Diff(view) => (InputTarget::DiffSearch, &view.search),
            Focus::Blame(view) => match &view.commit {
                Some(commit) => (InputTarget::BlameCommitSearch, &commit.search),
                None => (InputTarget::BlameSearch, &view.search),
            },
            _ => return,
        };
        self.input = Some(search_input(target, search));
    }

    /// `n` or `N`: the next or previous match of the search the key focus
    /// owns, saying so when it wrapped. A side panel steps its preview's.
    pub(crate) fn step_search(&mut self, back: bool) -> Vec<Effect> {
        let rows = self.diff_rows();
        let mut effects = vec![];
        // `None` when the search found nothing to step to.
        let wrapped = match (self.panel_focus(), &mut self.focus) {
            (KeyScope::Diff, Focus::Diff(view)) if no_matches(&view.search) => None,
            (KeyScope::Diff, Focus::Diff(view)) => Some(view.step_search(back, rows)),
            (KeyScope::Blame, Focus::Blame(view)) if no_matches(&view.search) => None,
            (KeyScope::Blame, Focus::Blame(view)) => Some(view.step_search(back, rows)),
            (
                KeyScope::BlameCommit,
                Focus::Blame(BlameView {
                    commit: Some(commit),
                    ..
                }),
            ) => (!no_matches(&commit.search)).then(|| commit.step_search(back, rows)),
            (
                KeyScope::Files
                | KeyScope::CommitFiles
                | KeyScope::StashFiles
                | KeyScope::ReviewDiff,
                _,
            ) => (!no_matches(&self.diff.search)).then(|| {
                let (wrapped, demand) = self.diff.step_search(back, rows);
                effects = demand;
                wrapped
            }),
            _ => Some(false),
        };
        match wrapped {
            None => self.notice = Some("No matches".into()),
            Some(true) => self.notice = Some("Search wrapped".into()),
            Some(false) => {}
        }
        effects
    }

    /// Esc in the blame, its commit, or a commit's or a stash's files:
    /// clear that pane's search, else close the pane as Esc did before. A
    /// commit's files clear their filter before the search.
    pub(crate) fn pane_escape(&mut self) -> Vec<Effect> {
        let scope = self.panel_focus();
        // The cursor stays: the filter showed it, or there were no rows.
        if scope == KeyScope::CommitFiles
            && let Some(open) = &mut self.commits.open
            && !open.filter.is_empty()
        {
            open.filter.clear();
            return vec![];
        }
        let cleared = match (scope, &mut self.focus) {
            (
                KeyScope::BlameCommit,
                Focus::Blame(BlameView {
                    commit: Some(commit),
                    ..
                }),
            ) => commit.search.take().is_some(),
            (KeyScope::Blame, Focus::Blame(view)) => view.search.take().is_some(),
            (KeyScope::CommitFiles | KeyScope::StashFiles, _) => self.diff.search.take().is_some(),
            _ => false,
        };
        if cleared {
            return vec![];
        }
        let close = match scope {
            KeyScope::Blame => AppIntent::CloseBlame,
            KeyScope::BlameCommit => AppIntent::CloseBlameCommit,
            KeyScope::CommitFiles => AppIntent::CloseCommitFiles,
            KeyScope::StashFiles => AppIntent::CloseStashFiles,
            _ => return vec![],
        };
        self.update(close)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::Tag;
    use crate::git::diff::DiffChunk;
    use crate::layout::SidePanel;
    use crate::test_support::*;
    use crossterm::event::KeyCode;

    fn has_run_op(effects: &[Effect]) -> bool {
        effects.iter().any(|e| matches!(e, Effect::RunOp { .. }))
    }

    #[test]
    fn typing_a_files_filter_narrows_the_list_and_esc_clears_it_then_quits() {
        let files = vec![
            changed("src/model.rs", '.', 'M'),
            changed("src/view.rs", '.', 'M'),
            changed("docs/mode.md", '.', 'M'),
            changed("src/main.rs", '.', 'M'),
        ];
        let (mut state, _) = with_files(files, 24);
        press_char(&mut state, 'j');
        assert_eq!(state.selected, Some(1));
        type_text(&mut state, "/mod");
        assert_eq!(state.key_focus(), KeyScope::Input);
        assert_eq!(state.files_filter, "mod");
        assert_eq!(state.files_visible(), [0, 2]);
        assert_eq!(
            state.selected,
            Some(0),
            "the hidden cursor moved to a shown row"
        );

        press(&mut state, KeyCode::Enter);
        assert_eq!(state.input, None);
        assert_eq!(state.files_filter, "mod", "Enter keeps the filter");
        press_char(&mut state, 'j');
        assert_eq!(state.selected, Some(2), "j skips the hidden row");
        press_char(&mut state, 'j');
        assert_eq!(state.selected, Some(2));
        press_char(&mut state, 'k');
        assert_eq!(state.selected, Some(0));

        press(&mut state, KeyCode::Esc);
        assert_eq!(state.files_filter, "");
        assert!(!state.should_quit);
        press(&mut state, KeyCode::Esc);
        assert!(
            state.should_quit,
            "with nothing to clear, Esc quits as before"
        );
    }

    #[test]
    fn esc_in_the_filter_input_clears_the_filter_and_closes_it() {
        let (mut state, _) = with_files(three_files(), 24);
        type_text(&mut state, "/b");
        assert_eq!(state.files_visible(), [1]);
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.input, None);
        assert_eq!(state.files_filter, "");
        assert_eq!(state.files_visible(), [0, 1, 2]);
        assert!(!state.should_quit);
    }

    #[test]
    fn typing_a_commits_filter_narrows_the_loaded_rows() {
        let mut state = with_commits(5, true);
        assert_eq!(state.key_focus(), KeyScope::Commits);
        type_text(&mut state, "/commit 3");
        assert_eq!(state.commits.filter, "commit 3");
        assert_eq!(state.commits.visible(), [3]);
        assert_eq!(state.commits.selected, 3);
        press(&mut state, KeyCode::Enter);
        press_char(&mut state, 'k');
        assert_eq!(
            state.commits.selected, 3,
            "nothing else is shown to move to"
        );
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.commits.filter, "");
        assert_eq!(state.commits.visible().len(), 5);
    }

    #[test]
    fn q_question_mark_and_d_are_typed_into_the_filter() {
        let (mut state, _) = with_files(three_files(), 24);
        press_char(&mut state, '/');
        let mut effects = Vec::new();
        for c in ['q', '?', 'd'] {
            effects.extend(press_char(&mut state, c));
        }
        assert_eq!(state.files_filter, "q?d");
        assert!(!state.should_quit);
        assert!(!state.help_open);
        assert!(state.popups.is_empty());
        assert!(!has_run_op(&effects), "{effects:?}");
    }

    /// Two hunks; "needle" once in the first changed line of the second
    /// and twice in the next. Rows 8 and 9 hold the matches.
    const NEEDLES: &[u8] = b"diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n\
        @@ -1,2 +1,2 @@\n-one\n+two\n ctx\n\
        @@ -20,2 +20,2 @@\n-needle\n+needle needle\n end\n";

    fn current(state: &AppState) -> Option<usize> {
        view(state).search.as_ref().and_then(|s| s.current)
    }

    #[test]
    fn a_diff_search_jumps_cycles_wraps_and_clears() {
        let mut state = focused(three_files(), NEEDLES);
        type_text(&mut state, "/needle");
        assert_eq!(view(&state).search.as_ref().unwrap().matches.len(), 3);
        assert_eq!(view(&state).hunk, 0, "typing does not jump");
        press(&mut state, KeyCode::Enter);
        assert_eq!(state.input, None);
        assert_eq!(view(&state).hunk, 1, "Enter goes to the first match's hunk");
        assert_eq!(current(&state), Some(0));

        press_char(&mut state, 'n');
        press_char(&mut state, 'n');
        assert_eq!(current(&state), Some(2));
        assert_eq!(state.notice, None);
        press_char(&mut state, 'n');
        assert_eq!(current(&state), Some(0));
        assert_eq!(state.notice, Some("Search wrapped".into()));
        press_char(&mut state, 'N');
        assert_eq!(current(&state), Some(2));
        assert_eq!(state.notice, Some("Search wrapped".into()));

        press_char(&mut state, 'v');
        press_char(&mut state, '/');
        assert_eq!(state.notice, Some("Leave line mode first".into()));
        assert_eq!(state.input, None);
        press(&mut state, KeyCode::Esc);

        press(&mut state, KeyCode::Esc);
        assert!(
            matches!(state.focus, Focus::Diff(_)),
            "Esc only cleared the search"
        );
        assert_eq!(view(&state).search, None);
        press_char(&mut state, 'n');
        assert_eq!(state.notice, Some("Nothing searched yet (/)".into()));
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.focus, Focus::Files);
        assert!(!state.should_quit);
    }

    // --- Search in the diff preview, the blame and its commit ---

    /// The tag of the diff load among `effects`.
    fn load_diff_tag(effects: &[Effect]) -> Tag {
        effects
            .iter()
            .find_map(|e| match e {
                Effect::LoadDiff { tag, .. } => Some(*tag),
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected a LoadDiff in {effects:?}"))
    }

    /// 60 preview lines, "foo" on rows 30 and 50, more still to come.
    fn preview_loaded(state: &mut AppState, tag: Tag) {
        let lines = (0..60)
            .map(|i| line(if i == 30 || i == 50 { "+foo" } else { " x" }))
            .collect();
        state.update(AppIntent::DiffLoaded {
            tag,
            result: Ok(DiffChunk { lines, done: false }),
        });
    }

    fn preview_current(state: &AppState) -> Option<usize> {
        state.diff.search.as_ref().and_then(|s| s.current)
    }

    /// ctrl-f, `foo`, Enter: the preview scrolls to the first match, and
    /// `n` and `N` cycle round both ways.
    fn search_the_preview_for_foo(state: &mut AppState) {
        ctrl(state, 'f');
        assert_eq!(state.key_focus(), KeyScope::Input);
        type_text(state, "foo");
        assert_eq!(state.diff.scroll, 0, "typing does not jump");
        press(state, KeyCode::Enter);
        assert_eq!(state.input, None);
        // 21 rows: row 30 is the bottom one.
        assert_eq!(state.diff.scroll, 10);
        assert_eq!(preview_current(state), Some(0));

        press_char(state, 'n');
        assert_eq!((preview_current(state), state.diff.scroll), (Some(1), 30));
        assert_eq!(state.notice, None);
        press_char(state, 'n');
        assert_eq!((preview_current(state), state.diff.scroll), (Some(0), 30));
        assert_eq!(state.notice, Some("Search wrapped".into()));
        press_char(state, 'N');
        assert_eq!(preview_current(state), Some(1));
        assert_eq!(state.notice, Some("Search wrapped".into()));
        press_char(state, 'N');
        assert_eq!(preview_current(state), Some(0));
    }

    #[test]
    fn ctrl_f_searches_the_files_preview_and_esc_clears_range_filter_search_then_quits() {
        let (mut state, effects) = with_files(three_files(), 24);
        preview_loaded(&mut state, diff_tag(&effects));
        type_text(&mut state, "/a\n");
        press_char(&mut state, 'V');
        search_the_preview_for_foo(&mut state);
        assert_eq!(state.diff.path, Some("a.rs".into()));

        press(&mut state, KeyCode::Esc);
        assert_eq!(state.files_anchor, None, "the range goes first");
        assert!(state.diff.search.is_some());
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.files_filter, "", "then the filter");
        assert!(state.diff.search.is_some());
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.diff.search, None, "then the search");
        assert!(!state.should_quit);
        press_char(&mut state, 'n');
        assert_eq!(state.notice, Some("Nothing searched yet (ctrl-f)".into()));
        press(&mut state, KeyCode::Esc);
        assert!(state.should_quit, "then Esc quits as before");
    }

    /// Files with a found preview search for "foo".
    fn files_searching_foo() -> AppState {
        let (mut state, effects) = with_files(three_files(), 24);
        preview_loaded(&mut state, diff_tag(&effects));
        ctrl(&mut state, 'f');
        type_text(&mut state, "foo\n");
        assert!(state.diff.search.is_some());
        assert_eq!(state.focus, Focus::Files);
        state
    }

    #[test]
    fn switching_the_side_panel_clears_the_preview_search() {
        for (key, side) in [('1', SidePanel::Status), ('4', SidePanel::Commits)] {
            let mut state = files_searching_foo();
            press_char(&mut state, key);
            assert_eq!(state.expanded, side, "{key}");
            assert_eq!(state.diff.search, None, "{key}");
        }
        for back in [false, true] {
            let mut state = files_searching_foo();
            state.update(AppIntent::CycleFocus { back });
            assert_ne!(state.expanded, SidePanel::Files, "back: {back}");
            assert_eq!(state.diff.search, None, "back: {back}");
        }
    }

    #[test]
    fn the_diff_pane_and_back_to_files_keeps_the_preview_search() {
        let mut state = files_searching_foo();
        state.update(AppIntent::FocusPanel(crate::layout::Panel::Diff));
        assert!(matches!(state.focus, Focus::Diff(_)), "{:?}", state.focus);
        state.update(AppIntent::FocusLeft);
        assert_eq!(state.focus, Focus::Files);
        assert!(state.diff.search.is_some());
        // Files again by its number: the side panel did not change.
        press_char(&mut state, '2');
        assert_eq!(state.focus, Focus::Files);
        assert!(state.diff.search.is_some());
    }

    #[test]
    fn esc_in_the_preview_search_input_clears_it_and_stays_in_files() {
        let mut state = files_searching_foo();
        ctrl(&mut state, 'f');
        assert!(state.input.is_some());
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.input, None);
        assert_eq!(state.diff.search, None);
        assert_eq!(state.focus, Focus::Files);
        assert!(!state.should_quit);
    }

    #[test]
    fn esc_in_the_blame_search_input_clears_it_and_keeps_the_blame() {
        let mut state = blamed_foo();
        type_text(&mut state, "/foo\n");
        assert!(blame(&state).search.is_some());
        press_char(&mut state, '/');
        assert_eq!(
            state.input.as_ref().map(|i| i.target),
            Some(InputTarget::BlameSearch)
        );
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.input, None);
        assert_eq!(blame(&state).search, None);
    }

    #[test]
    fn esc_in_the_blame_commit_search_input_clears_it_and_keeps_the_commit() {
        let mut state = blamed_foo();
        let tag = match press(&mut state, KeyCode::Enter).as_slice() {
            [Effect::LoadCommit { tag, .. }] => *tag,
            other => panic!("expected one LoadCommit, got {other:?}"),
        };
        state.update(AppIntent::CommitLoaded {
            tag,
            result: Ok(vec![line("+foo")]),
        });
        type_text(&mut state, "/foo\n");
        assert!(blame(&state).commit.as_ref().unwrap().search.is_some());
        press_char(&mut state, '/');
        assert_eq!(
            state.input.as_ref().map(|i| i.target),
            Some(InputTarget::BlameCommitSearch)
        );
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.input, None);
        let commit = blame(&state).commit.as_ref().expect("still open");
        assert_eq!(commit.search, None);
    }

    #[test]
    fn a_preview_search_with_no_match_says_so_on_enter_and_n() {
        let (mut state, effects) = with_files(three_files(), 24);
        preview_loaded(&mut state, diff_tag(&effects));
        ctrl(&mut state, 'f');
        type_text(&mut state, "zzz\n");
        assert_eq!(state.notice, Some("No matches".into()));
        state.notice = None;
        press_char(&mut state, 'n');
        assert_eq!(state.notice, Some("No matches".into()));
    }

    #[test]
    fn the_preview_search_waits_for_a_file() {
        let (mut state, _) = with_files(vec![], 24);
        state.focus = Focus::Files;
        assert_eq!(state.diff.path, None);
        ctrl(&mut state, 'f');
        assert_eq!(state.input, None);
        assert_eq!(state.notice, Some("No file selected".into()));
    }

    #[test]
    fn a_commits_files_preview_is_searched_and_esc_clears_it_before_going_back() {
        let mut state = on_commit_five();
        let tag = load_diff_tag(&opened(&mut state, two_commit_files()));
        preview_loaded(&mut state, tag);
        assert_eq!(state.key_focus(), KeyScope::CommitFiles);
        search_the_preview_for_foo(&mut state);

        press(&mut state, KeyCode::Esc);
        assert_eq!(state.diff.search, None);
        assert_eq!(state.key_focus(), KeyScope::CommitFiles, "still open");
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.commits.open, None);
        assert_eq!(state.key_focus(), KeyScope::Commits);
        assert!(!state.should_quit);
    }

    #[test]
    fn esc_in_a_commits_files_clears_the_filter_then_the_search_then_goes_back() {
        let mut state = on_commit_five();
        let tag = load_diff_tag(&opened(&mut state, two_commit_files()));
        preview_loaded(&mut state, tag);
        search_the_preview_for_foo(&mut state);
        type_text(&mut state, "/lib\n");
        assert_eq!(open_files(&state).filter, "lib");

        assert_eq!(press(&mut state, KeyCode::Esc), vec![]);
        assert_eq!(open_files(&state).filter, "", "the filter goes first");
        assert!(state.diff.search.is_some(), "the search stays");
        assert_eq!(state.key_focus(), KeyScope::CommitFiles);
        assert!(!state.should_quit);
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.diff.search, None, "then the search");
        assert_eq!(state.key_focus(), KeyScope::CommitFiles);
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.commits.open, None, "then back to the log");
        assert_eq!(state.key_focus(), KeyScope::Commits);
        assert_eq!(state.commits.selected, 5, "on the same commit");
        assert!(!state.should_quit);

        // With neither set, one Esc goes back.
        let mut state = commit_files_open();
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.commits.open, None);
        assert_eq!(state.commits.selected, 5);
        assert!(!state.should_quit);
    }

    #[test]
    fn a_stashs_files_preview_is_searched_and_esc_clears_it_before_going_back() {
        let mut state = on_stash_one();
        let tag = load_diff_tag(&stash_opened(&mut state));
        preview_loaded(&mut state, tag);
        assert_eq!(state.key_focus(), KeyScope::StashFiles);
        search_the_preview_for_foo(&mut state);

        press(&mut state, KeyCode::Esc);
        assert_eq!(state.diff.search, None);
        assert_eq!(state.key_focus(), KeyScope::StashFiles, "still open");
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.stash.open, None);
        assert_eq!(state.key_focus(), KeyScope::Stash);
        assert!(!state.should_quit);
    }

    /// A loaded blame of five committed lines, "foo" on lines 2 and 4.
    fn blamed_foo() -> AppState {
        let (mut state, tag) = blaming(24);
        let lines = ["fn a", "foo", "x", "foo bar", "y"]
            .iter()
            .enumerate()
            .map(|(i, text)| crate::git::blame::BlameLine {
                text: text.to_string(),
                ..blame_line(OID, i + 1)
            })
            .collect();
        state.update(AppIntent::BlameLoaded {
            tag,
            result: Ok(lines),
        });
        state
    }

    #[test]
    fn the_blame_and_its_commit_each_search_on_their_own() {
        let mut state = blamed_foo();
        type_text(&mut state, "/foo");
        assert_eq!(blame(&state).cursor, 0, "typing does not move the cursor");
        press(&mut state, KeyCode::Enter);
        assert_eq!(blame(&state).cursor, 1);
        press_char(&mut state, 'n');
        assert_eq!(blame(&state).cursor, 3);
        press_char(&mut state, 'n');
        assert_eq!(blame(&state).cursor, 1);
        assert_eq!(state.notice, Some("Search wrapped".into()));
        press_char(&mut state, 'N');
        assert_eq!(blame(&state).cursor, 3);

        // Enter opens the match's line's commit.
        let tag = match press(&mut state, KeyCode::Enter).as_slice() {
            [Effect::LoadCommit { tag, oid }] => {
                assert_eq!(oid, OID);
                *tag
            }
            other => panic!("expected one LoadCommit, got {other:?}"),
        };
        press_char(&mut state, '/');
        assert_eq!(state.input, None, "nothing to search while it loads");
        assert_eq!(state.notice, Some("Loading…".into()));
        state.update(AppIntent::CommitLoaded {
            tag,
            result: Ok(vec![line("commit abc"), line("+foo"), line(" ctx")]),
        });
        press_char(&mut state, '/');
        assert_eq!(
            state
                .input
                .as_ref()
                .map(|i| (i.target, i.line.text.as_str())),
            Some((InputTarget::BlameCommitSearch, "")),
            "the commit's own search, not the blame's"
        );
        type_text(&mut state, "ctx\n");
        let commit = blame(&state).commit.as_ref().unwrap();
        let found = commit.search.as_ref().unwrap();
        assert_eq!((found.query.as_str(), found.current), ("ctx", Some(0)));
        assert_eq!(blame(&state).search.as_ref().unwrap().query, "foo");

        press(&mut state, KeyCode::Esc);
        let commit = blame(&state).commit.as_ref().expect("still open");
        assert_eq!(commit.search, None);
        press(&mut state, KeyCode::Esc);
        assert_eq!(blame(&state).commit, None);
        assert_eq!(state.panel_focus(), KeyScope::Blame);
        assert!(blame(&state).search.is_some(), "the blame kept its search");

        press(&mut state, KeyCode::Esc);
        assert_eq!(blame(&state).search, None);
        press_char(&mut state, 'n');
        assert_eq!(state.notice, Some("Nothing searched yet (/)".into()));
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.focus, Focus::Files);
        assert!(!state.should_quit);
    }

    #[test]
    fn keys_typed_into_each_new_search_are_only_text() {
        let typed = "q?d\u{202E}";
        let check = |mut state: AppState, open: &dyn Fn(&mut AppState), target| {
            open(&mut state);
            assert_eq!(state.input.as_ref().map(|i| i.target), Some(target));
            let mut effects = Vec::new();
            for c in typed.chars() {
                effects.extend(press_char(&mut state, c));
            }
            assert_eq!(state.input.as_ref().unwrap().line.text, typed);
            assert!(!state.should_quit && !state.help_open, "{target:?}");
            assert!(state.popups.is_empty(), "{target:?}");
            effects.extend(press(&mut state, KeyCode::Enter));
            for effect in &effects {
                assert!(
                    !matches!(
                        effect,
                        Effect::RunOp { .. }
                            | Effect::ProbeCommit { .. }
                            | Effect::CheckLeftBehind { .. }
                    ),
                    "{target:?}: {effect:?}"
                );
                let shown = format!("{effect:?}");
                assert!(!shown.contains("q?d"), "{target:?}: {shown}");
            }
        };
        let ctrl_f = |state: &mut AppState| {
            ctrl(state, 'f');
        };
        let slash = |state: &mut AppState| {
            press_char(state, '/');
        };

        let (mut files, effects) = with_files(three_files(), 24);
        preview_loaded(&mut files, diff_tag(&effects));
        check(files, &ctrl_f, InputTarget::PreviewSearch);

        let mut commit_files = on_commit_five();
        let tag = load_diff_tag(&opened(&mut commit_files, two_commit_files()));
        preview_loaded(&mut commit_files, tag);
        check(commit_files, &ctrl_f, InputTarget::PreviewSearch);

        let mut stash_files = on_stash_one();
        let tag = load_diff_tag(&stash_opened(&mut stash_files));
        preview_loaded(&mut stash_files, tag);
        check(stash_files, &ctrl_f, InputTarget::PreviewSearch);

        check(blamed_foo(), &slash, InputTarget::BlameSearch);

        let mut commit = blamed_foo();
        let tag = match press(&mut commit, KeyCode::Enter).as_slice() {
            [Effect::LoadCommit { tag, .. }] => *tag,
            other => panic!("expected one LoadCommit, got {other:?}"),
        };
        commit.update(AppIntent::CommitLoaded {
            tag,
            result: Ok(vec![line("+foo")]),
        });
        check(commit, &slash, InputTarget::BlameCommitSearch);
    }

    #[test]
    fn the_focused_commit_file_diff_is_still_searched_with_slash() {
        let mut state = on_commit_five();
        let tag = load_diff_tag(&opened(&mut state, two_commit_files()));
        preview_loaded(&mut state, tag);
        state.update(AppIntent::FocusPanel(crate::layout::Panel::Diff));
        let Focus::Diff(view) = &state.focus else {
            panic!("expected the commit file's hunks, got {:?}", state.focus);
        };
        assert!(view.commit.is_some());
        press_char(&mut state, '/');
        assert_eq!(
            state.input.as_ref().map(|i| i.target),
            Some(InputTarget::DiffSearch)
        );
    }
}
