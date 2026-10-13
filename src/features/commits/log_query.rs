//! The log query on `AppState`: narrowing the Commits log to a path or
//! an author.

use super::CommitsTab;
use crate::effect::Effect;
use crate::git::log::LogQuery;
use crate::keymap::KeyScope;
use crate::layout::{Panel, SidePanel};
use crate::list_filter::{InputTarget, ListInput, LogFilterField};
use crate::model::{AppState, Context};
use crate::popup::LineInput;

impl AppState {
    /// Type the path or author, starting from the one in force. A path
    /// that is not UTF-8 cannot be typed back, so it starts empty.
    pub(crate) fn open_log_filter(&mut self, field: LogFilterField) {
        if self.panel_focus() != KeyScope::Commits {
            return;
        }
        let query = &self.commits.query;
        let text = match field {
            LogFilterField::Path => query
                .path
                .as_ref()
                .and_then(|path| std::str::from_utf8(path.as_bytes()).ok())
                .filter(|text| !text.contains(char::is_control))
                .unwrap_or_default(),
            LogFilterField::Author => query.author.as_deref().unwrap_or_default(),
        };
        self.input = Some(ListInput {
            target: field.target(),
            line: LineInput::with_text(text),
        });
    }

    /// Enter on a path or author: set that half of the query (blank clears
    /// it) and run it. Text git cannot be given keeps the input open.
    pub(crate) fn accept_log_query(&mut self, input: ListInput) -> Vec<Effect> {
        let mut query = self.commits.query.clone();
        let parsed = match input.target {
            InputTarget::LogPath => LogQuery::path_from(&input.line.text).map(|p| query.path = p),
            _ => LogQuery::author_from(&input.line.text).map(|a| query.author = a),
        };
        if let Err(why) = parsed {
            self.notice = Some(why.into());
            self.input = Some(input);
            return vec![];
        }
        if query == self.commits.query {
            return vec![];
        }
        self.set_log_query(query)
    }

    /// Narrow the log to `query` and load its first page. A commit's files
    /// open from the old log close, since the rows they came from are gone.
    pub(crate) fn set_log_query(&mut self, query: LogQuery) -> Vec<Effect> {
        let mut effects = self.close_commit_files();
        self.commits.set_query(query);
        effects.push(self.first_commits_page());
        effects
    }

    /// `L` in Files: the selected file's history, by its exact name. The
    /// author half stays.
    pub(crate) fn log_of_file(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        let Some(path) = self.files.selected_row().map(|entry| entry.path.clone()) else {
            return vec![];
        };
        let query = LogQuery {
            path: Some(path),
            author: self.commits.query.author.clone(),
        };
        let mut effects = self.focus_panel(Panel::Side(SidePanel::Commits));
        self.commits.tab = CommitsTab::Log;
        effects.extend(self.set_log_query(query));
        effects
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyModifiers};

    use super::*;
    use crate::git::path::RepoPath;
    use crate::git::status::{EntryKind, FileEntry};
    use crate::intent::AppIntent;
    use crate::keymap::KeyPress;
    use crate::model::Focus;
    use crate::test_support::{answer, hex_page, loads, oid, with_log};

    fn press(state: &mut AppState, code: KeyCode) -> Vec<Effect> {
        state.update(AppIntent::Key(KeyPress::new(code, KeyModifiers::NONE)))
    }

    /// Each char pressed in turn, `\n` as Enter; every effect in order.
    fn type_text(state: &mut AppState, text: &str) -> Vec<Effect> {
        text.chars()
            .flat_map(|c| {
                let code = if c == '\n' {
                    KeyCode::Enter
                } else {
                    KeyCode::Char(c)
                };
                press(state, code)
            })
            .collect()
    }

    fn author(text: &str) -> LogQuery {
        LogQuery {
            path: None,
            author: Some(text.into()),
        }
    }

    #[test]
    fn an_author_typed_on_the_bar_reloads_the_log_with_it_and_paging_keeps_it() {
        let mut state = with_log(5);
        type_text(&mut state, "jj");
        assert_eq!(state.commits.list.selected, 2);

        let effects = type_text(&mut state, "Aann\n");

        assert_eq!(state.commits.query.author.as_deref(), Some("ann"));
        assert!(state.commits.list.rows.is_empty() && !state.commits.list.loaded);
        assert_eq!(state.commits.list.selected, 0);
        assert_eq!(state.input, None);
        let [(tag, None, 0, ref query)] = loads(&effects)[..] else {
            panic!("expected one first-page load in {effects:?}");
        };
        assert_eq!(*query, author("ann"));

        let more = loads(&answer(&mut state, tag, 0, hex_page(3, false)));
        let [(_, Some(ref tip), 3, ref query)] = more[..] else {
            panic!("expected one prefetch in {more:?}");
        };
        assert_eq!((tip.as_str(), query), (oid(0).as_str(), &author("ann")));

        let refresh = loads(&press(&mut state, KeyCode::Char('r')));
        let [(_, None, 0, ref query)] = refresh[..] else {
            panic!("expected one reload in {refresh:?}");
        };
        assert_eq!(*query, author("ann"));
    }

    #[test]
    fn the_log_of_a_file_takes_its_exact_name_and_keeps_the_author() {
        let mut state = with_log(3);
        state.commits.query = author("ann");
        let raw = RepoPath::from_bytes(b"x\xff.rs");
        state.files.rows = vec![FileEntry {
            path: raw.clone(),
            orig_path: None,
            index: '.',
            worktree: 'M',
            kind: EntryKind::Changed,
        }];
        state.files.selected = 0;
        state.focus = Focus::Files;
        state.commits.tab = CommitsTab::Reflog;

        let effects = press(&mut state, KeyCode::Char('L'));

        let query = &state.commits.query;
        assert_eq!(query.path.as_ref().unwrap().as_bytes(), b"x\xff.rs");
        assert_eq!(query.author.as_deref(), Some("ann"));
        assert_eq!(state.focus, Focus::Commits);
        assert_eq!(state.commits.tab, CommitsTab::Log);
        assert_eq!(loads(&effects).len(), 1);

        // The bar cannot hold a name that is not text: it starts empty.
        press(&mut state, KeyCode::Char('L'));
        assert_eq!(state.input.as_ref().unwrap().line.text, "");
    }

    #[test]
    fn keys_typed_into_the_log_input_are_text_and_esc_leaves_the_query() {
        let mut state = with_log(3);
        state.commits.query = author("bob");
        assert!(press(&mut state, KeyCode::Char('A')).is_empty());
        assert_eq!(state.input.as_ref().unwrap().line.text, "bob");
        for c in ['q', '?', 'M', 'd'] {
            assert_eq!(press(&mut state, KeyCode::Char(c)), vec![], "{c}");
        }
        assert_eq!(state.input.as_ref().unwrap().line.text, "bobq?Md");
        assert!(!state.should_quit && state.diff_mark.is_none());

        assert_eq!(press(&mut state, KeyCode::Esc), vec![]);

        assert_eq!(state.input, None);
        assert_eq!(state.commits.query, author("bob"));
        assert_eq!(state.commits.list.rows.len(), 3);
    }

    #[test]
    fn text_git_cannot_take_keeps_the_input_open_and_says_why() {
        let mut state = with_log(3);
        type_text(&mut state, "L");
        state.input.as_mut().unwrap().line = LineInput::with_text("a\u{1b}b");

        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);

        assert!(state.input.is_some());
        assert_eq!(
            state.notice.as_deref(),
            Some("A path cannot contain control characters")
        );
        assert!(!state.commits.query.is_active());
    }

    #[test]
    fn esc_clears_the_range_the_filter_the_mark_and_the_query_in_turn_then_quits() {
        let mut state = with_log(5);
        let effects = type_text(&mut state, "Lsrc/\n");
        let [(tag, ..)] = loads(&effects)[..] else {
            panic!("expected one load in {effects:?}");
        };
        assert_eq!(state.commits.query.path, Some(RepoPath::from("src")));
        answer(&mut state, tag, 0, hex_page(5, true));
        type_text(&mut state, "M/commit\nV");
        assert!(state.diff_mark.is_some());
        assert_eq!(state.commits.list.filter, "commit");
        assert!(state.commits.list.range_anchor.is_some());

        assert!(press(&mut state, KeyCode::Esc).is_empty());
        assert!(state.commits.list.range_anchor.is_none());
        assert_eq!(state.commits.list.filter, "commit");

        assert!(press(&mut state, KeyCode::Esc).is_empty());
        assert_eq!(state.commits.list.filter, "");
        assert!(state.diff_mark.is_some());

        assert!(press(&mut state, KeyCode::Esc).is_empty());
        assert!(state.diff_mark.is_none());
        assert!(state.commits.query.is_active());

        let effects = press(&mut state, KeyCode::Esc);
        assert!(!state.commits.query.is_active());
        assert_eq!(loads(&effects).len(), 1, "the whole log reloads");
        assert!(!state.should_quit);

        press(&mut state, KeyCode::Esc);
        assert!(state.should_quit);
    }
}
