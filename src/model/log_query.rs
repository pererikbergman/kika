//! The log query and the diff mark on `AppState`: narrowing the Commits
//! log to a path or an author, and comparing a marked commit or local
//! branch with another one in the commit-files view.

use super::{AppState, Context, PendingDiff};
use crate::commit_files::CommitFilesList;
use crate::commits::CommitsTab;
use crate::diff_mark::{DiffEnd, DiffMark};
use crate::effect::{Effect, Tag};
use crate::git::commit_files::{CommitId, CommitRange};
use crate::git::log::LogQuery;
use crate::keymap::KeyScope;
use crate::layout::{Panel, SidePanel};
use crate::list_filter::{InputTarget, ListInput, LogFilterField};
use crate::popup::LineInput;

/// What Esc or a second `M` says once the mark is gone.
pub(super) const MARK_CLEARED: &str = "Diff mark cleared";

impl AppState {
    /// Type the path or author, starting from the one in force. A path
    /// that is not UTF-8 cannot be typed back, so it starts empty.
    pub(super) fn open_log_filter(&mut self, field: LogFilterField) {
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
    pub(super) fn accept_log_query(&mut self, input: ListInput) -> Vec<Effect> {
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
    pub(super) fn set_log_query(&mut self, query: LogQuery) -> Vec<Effect> {
        let mut effects = self.close_commit_files();
        self.commits.set_query(query);
        effects.push(self.first_commits_page());
        effects
    }

    /// `L` in Files: the selected file's history, by its exact name. The
    /// author half stays.
    pub(super) fn log_of_file(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        let Some(path) = self
            .selected
            .and_then(|i| self.files.get(i))
            .map(|entry| entry.path.clone())
        else {
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

    /// `M`: mark the selected log commit or local branch, or clear the
    /// mark when it is already on it.
    pub(super) fn toggle_diff_mark(&mut self) {
        let mark = match self.panel_focus() {
            KeyScope::Commits => {
                let Some(row) = self.commits.rows.get(self.commits.selected) else {
                    return;
                };
                let Some(id) = CommitId::parse(&row.oid) else {
                    return;
                };
                DiffMark::Commit {
                    id,
                    short: row.short_oid.clone(),
                }
            }
            KeyScope::Branches => match self.branches.selected_branch() {
                Some(branch) => DiffMark::Branch(branch.name.clone()),
                None => return,
            },
            _ => return,
        };
        if self.diff_mark.as_ref() == Some(&mark) {
            self.diff_mark = None;
            self.notice = Some(MARK_CLEARED.into());
        } else {
            self.notice_text = Some(format!("Marked {} for diffing", mark.label()));
            self.diff_mark = Some(mark);
        }
    }

    /// Enter in Branches: compare the mark with the selected branch.
    pub(super) fn open_branch_diff(&mut self) -> Vec<Effect> {
        if self.panel_focus() != KeyScope::Branches {
            return vec![];
        }
        let Some(name) = self.branches.selected_branch().map(|b| b.name.clone()) else {
            return vec![];
        };
        if matches!(&self.diff_mark, Some(DiffMark::Branch(marked)) if *marked == name) {
            return vec![];
        }
        let label = name.display();
        self.open_comparison(DiffEnd::Branch(name), label)
    }

    /// Read the mark and `target` as commits, to list what changed from
    /// the mark to `target`. Nothing without a mark.
    pub(super) fn open_comparison(&mut self, target: DiffEnd, label: String) -> Vec<Effect> {
        let Some((base, against)) = self.diff_mark.as_ref().map(|m| (m.end(), m.label())) else {
            return vec![];
        };
        let seq = self.take_seq();
        self.pending_diff = Some(PendingDiff {
            seq,
            against,
            target: label,
        });
        let tag = self.user_tag(seq);
        vec![Effect::ResolveDiff { tag, base, target }]
    }

    /// Both ends are commits now: open their files in the Commits panel,
    /// in place of the log, as a commit's files open.
    pub(super) fn diff_resolved(
        &mut self,
        tag: Tag,
        result: Result<CommitRange, String>,
    ) -> Vec<Effect> {
        if tag.user {
            self.busy = self.busy.saturating_sub(1);
        }
        let current = self.pending_diff.as_ref().map(|p| p.seq);
        if tag.generation != self.generation || current != Some(tag.seq) {
            return vec![];
        }
        let Some(pending) = self.pending_diff.take() else {
            return vec![];
        };
        let range = match result {
            Ok(range) => range,
            Err(message) => {
                let first = message.lines().next().unwrap_or_default();
                self.notice_text = Some(format!("Could not compare: {first}"));
                return vec![];
            }
        };
        let mut effects = self.close_commit_files();
        effects.extend(self.focus_panel(Panel::Side(SidePanel::Commits)));
        self.commits.tab = CommitsTab::Log;
        let seq = self.take_seq();
        let mut open = CommitFilesList::begin(range.clone(), pending.target, seq);
        open.against = Some(pending.against);
        self.commits.open = Some(open);
        let tag = self.user_tag(seq);
        effects.push(Effect::LoadCommitFiles { tag, range });
        effects
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyModifiers};

    use super::*;
    use crate::git::branches::{Branch, BranchName, Track};
    use crate::git::log::{CommitRow, LogPage};
    use crate::git::path::RepoPath;
    use crate::git::status::{EntryKind, FileEntry};
    use crate::intent::AppIntent;
    use crate::keymap::KeyPress;
    use crate::model::Focus;

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

    /// The full id of row `i`: hex, so it parses as a commit id.
    fn oid(i: usize) -> String {
        format!("{i:040x}")
    }

    fn page(n: usize, done: bool) -> LogPage {
        let rows = (0..n)
            .map(|i| CommitRow {
                oid: oid(i),
                short_oid: oid(i)[33..].to_string(),
                parents: vec![oid(i + 1)],
                author: "Ann".into(),
                time: 0,
                subject: format!("commit {i}"),
                subject_altered: false,
            })
            .collect();
        LogPage { rows, done }
    }

    /// Every page load in `effects`: its tip, skip and query.
    fn loads(effects: &[Effect]) -> Vec<(Tag, Option<String>, usize, LogQuery)> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::LoadCommits {
                    tag,
                    tip,
                    skip,
                    query,
                } => Some((*tag, tip.clone(), *skip, query.clone())),
                _ => None,
            })
            .collect()
    }

    /// Answer a page load; a short page that is not the last asks for
    /// the next one at once.
    fn answer(state: &mut AppState, tag: Tag, skip: usize, page: LogPage) -> Vec<Effect> {
        state.update(AppIntent::CommitsLoaded {
            tag,
            skip,
            result: Ok((page, 1_000)),
        })
    }

    /// Started in a repository, the Commits panel focused on `n` commits.
    fn with_log(n: usize) -> AppState {
        let mut state =
            AppState::new(Context::Workspace).with_location(Some("/r/top".into()), "/r/top".into());
        let effects = state.update(AppIntent::Start);
        let [(tag, ..)] = loads(&effects)[..] else {
            panic!("expected one page load in {effects:?}");
        };
        answer(&mut state, tag, 0, page(n, true));
        state.focus = Focus::Commits;
        state.expanded = SidePanel::Commits;
        state
    }

    fn id(i: usize) -> CommitId {
        CommitId::parse(&oid(i)).unwrap()
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
        assert_eq!(state.commits.selected, 2);

        let effects = type_text(&mut state, "Aann\n");

        assert_eq!(state.commits.query.author.as_deref(), Some("ann"));
        assert!(state.commits.rows.is_empty() && !state.commits.loaded);
        assert_eq!(state.commits.selected, 0);
        assert_eq!(state.input, None);
        let [(tag, None, 0, ref query)] = loads(&effects)[..] else {
            panic!("expected one first-page load in {effects:?}");
        };
        assert_eq!(*query, author("ann"));

        let more = loads(&answer(&mut state, tag, 0, page(3, false)));
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
        state.files = vec![FileEntry {
            path: raw.clone(),
            orig_path: None,
            index: '.',
            worktree: 'M',
            kind: EntryKind::Changed,
        }];
        state.selected = Some(0);
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
        assert_eq!(state.commits.rows.len(), 3);
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
        answer(&mut state, tag, 0, page(5, true));
        type_text(&mut state, "M/commit\nV");
        assert!(state.diff_mark.is_some());
        assert_eq!(state.commits.filter, "commit");
        assert!(state.commits.range_anchor.is_some());

        assert!(press(&mut state, KeyCode::Esc).is_empty());
        assert!(state.commits.range_anchor.is_none());
        assert_eq!(state.commits.filter, "commit");

        assert!(press(&mut state, KeyCode::Esc).is_empty());
        assert_eq!(state.commits.filter, "");
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

    /// The one `ResolveDiff` in `effects`.
    fn resolve(effects: &[Effect]) -> (Tag, DiffEnd, DiffEnd) {
        match effects {
            [Effect::ResolveDiff { tag, base, target }] => (*tag, base.clone(), target.clone()),
            other => panic!("expected one ResolveDiff, got {other:?}"),
        }
    }

    #[test]
    fn enter_on_another_commit_compares_it_with_the_marked_one() {
        let mut state = with_log(4);
        type_text(&mut state, "jjM");
        assert_eq!(
            state.notice_text.as_deref(),
            Some(format!("Marked {} for diffing", &oid(2)[33..]).as_str())
        );
        type_text(&mut state, "j");

        let (tag, base, target) = resolve(&press(&mut state, KeyCode::Enter));
        assert_eq!(
            (base, target),
            (DiffEnd::Commit(id(2)), DiffEnd::Commit(id(3)))
        );
        assert!(tag.user);

        let range = CommitRange {
            oid: id(3),
            parent: Some(id(2)),
        };
        let effects = state.update(AppIntent::DiffResolved {
            tag,
            result: Ok(range.clone()),
        });

        let open = state.commits.open.as_ref().expect("the comparison opens");
        assert_eq!(open.range.parent, Some(id(2)));
        assert_eq!(open.against.as_deref(), Some(&oid(2)[33..]));
        assert_eq!(open.short_oid, &oid(3)[33..]);
        assert_eq!(state.focus, Focus::Commits);
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::LoadCommitFiles { range: r, .. } if *r == range))
        );
        // Answered once: a repeat is stale.
        assert_eq!(
            state.update(AppIntent::DiffResolved {
                tag,
                result: Ok(range),
            }),
            vec![]
        );
    }

    #[test]
    fn enter_on_the_marked_commit_opens_it_against_its_parent_as_before() {
        let mut state = with_log(4);
        type_text(&mut state, "jM");

        let effects = press(&mut state, KeyCode::Enter);

        match effects.as_slice() {
            [Effect::LoadCommitFiles { range, .. }] => {
                assert_eq!((&range.oid, &range.parent), (&id(1), &Some(id(2))));
            }
            other => panic!("expected the commit's files, got {other:?}"),
        }
        assert_eq!(state.commits.open.as_ref().unwrap().against, None);
    }

    #[test]
    fn m_on_the_marked_commit_clears_the_mark() {
        let mut state = with_log(2);
        type_text(&mut state, "M");
        assert!(state.diff_mark.is_some());
        type_text(&mut state, "M");
        assert_eq!(state.diff_mark, None);
        assert_eq!(state.notice.as_deref(), Some(MARK_CLEARED));
    }

    fn branch(name: &str) -> Branch {
        Branch {
            name: name.into(),
            head: false,
            upstream: None,
            track: Track::None,
            merged: true,
        }
    }

    fn with_branches() -> AppState {
        let mut state = with_log(2);
        state
            .branches
            .loaded(Ok(vec![branch("main"), branch("topic")]));
        state.focus = Focus::Branches;
        state.expanded = SidePanel::Branches;
        state
    }

    #[test]
    fn enter_on_another_branch_compares_the_two_branches() {
        let mut state = with_branches();
        type_text(&mut state, "Mj");

        let (tag, base, target) = resolve(&press(&mut state, KeyCode::Enter));

        assert_eq!(base, DiffEnd::Branch(BranchName::from_bytes(b"main")));
        assert_eq!(target, DiffEnd::Branch(BranchName::from_bytes(b"topic")));
        state.update(AppIntent::DiffResolved {
            tag,
            result: Ok(CommitRange {
                oid: id(1),
                parent: Some(id(0)),
            }),
        });
        let open = state.commits.open.as_ref().unwrap();
        assert_eq!(
            (open.short_oid.as_str(), open.against.as_deref()),
            ("topic", Some("main"))
        );
        assert_eq!(state.focus, Focus::Commits);
        assert_eq!(state.panel_focus(), KeyScope::CommitFiles);
    }

    #[test]
    fn enter_in_branches_needs_a_mark_on_another_branch() {
        let mut state = with_branches();
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(
            state.notice.as_deref(),
            Some("Mark a commit or branch first (M)")
        );

        type_text(&mut state, "M");
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(
            state.notice.as_deref(),
            Some("Pick another branch or commit to diff against")
        );
    }

    #[test]
    fn a_failed_resolve_says_why_and_opens_nothing() {
        let mut state = with_branches();
        type_text(&mut state, "Mj");
        let busy = state.busy;
        let (tag, ..) = resolve(&press(&mut state, KeyCode::Enter));
        assert_eq!(state.busy, busy + 1);

        state.update(AppIntent::DiffResolved {
            tag,
            result: Err("No branch named topic".into()),
        });

        assert_eq!(state.commits.open, None);
        assert_eq!(
            state.notice_text.as_deref(),
            Some("Could not compare: No branch named topic")
        );
        assert_eq!(state.busy, busy);
    }
}
