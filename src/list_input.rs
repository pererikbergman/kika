//! The list filter and range handlers that many features share: open a
//! list's filter, apply and accept the bottom-bar input, start or clear a
//! range, and Esc in a list. They stay in core because they dispatch on
//! every list's scope. #322 adds the pane search here.

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
use crate::model::{AppState, Focus, no_matches};
use crate::popup::{LineInput, Popup};

/// Said when a changed filter drops the range made over the old rows.
pub(crate) const SELECTION_CLEARED: &str = "Selection cleared";
/// Said when a reload changed which files the Files range covers.
pub(crate) const RANGE_FILES_CHANGED: &str = "Selection cleared: the file list changed";

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
            KeyScope::Stash => (InputTarget::StashFilter, &self.stash.filter),
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
                if self.stash.set_filter(text) {
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
            KeyScope::Stash => self.stash.toggle_range(),
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
                self.stash.range_anchor.take().is_some(),
                &mut self.stash.filter,
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

#[cfg(test)]
mod tests {
    use super::*;
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
}
