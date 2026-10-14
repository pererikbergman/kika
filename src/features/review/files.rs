//! A pull request's changed files in review mode, read from local git:
//! check the head and base commits are here, fetch the missing ones once
//! from the forge's remote, list the files from the merge base, then
//! stream the selected file's diff into the same Diff pane the workspace
//! uses. The viewed marks come from GitHub's detail, matched by path.
//! The screen shows the whole pull request, or one of its commits picked
//! with `c` (`commit_view.rs`); the marks stay the pull request's, and
//! the selected file stays selected, by path, when the new view lists it.

use super::git::{CommitsHere, FileCounts, PrCommit, PrFileRow};
use super::state::{ReviewScreen, ReviewState};
use crate::diff_pane::DiffStart;
use crate::effect::{Effect, Tag};
use crate::intent::Scroll;
use crate::keymap::{OP_RUNNING, Reason};
use crate::model::{AppState, Context};
use kika_forge::review::ViewedState;
use kika_git::commit_files::{CommitFile, CommitId, CommitRange};
use kika_git::diff::DiffRequest;
use kika_git::path::RepoPath;

pub const STILL_MISSING: &str = "PR head is not available locally after fetch";
pub const FETCHED_BEFORE: &str =
    "PR head was fetched once this session and is still not here; fetch it with git, then press f";
pub const NO_FETCH_REMOTE: &str =
    "PR head is not here, and no remote names this repository on a [forge] github host";
const NO_DETAIL: &str = "Open a pull request first";
const DETAIL_LOADING: &str = "The pull request is still loading";
const NO_OIDS: &str = "GitHub gave no commit ids for this pull request";
const NOTHING_SEARCHED: &str = "Nothing searched yet (/)";
const IN_FILES: &str = "Go back to the pull request first (esc)";

/// Which half of the files screen has the keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesFocus {
    List,
    Diff,
}

/// Where the files screen is on its way to a list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilesStage {
    Checking,
    Fetching,
    Listing,
    Ready,
    /// Why there is no list; safe to draw.
    Failed(String),
}

/// One changed file with its counts and, when GitHub said, its viewed
/// state; `None` is unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrFile {
    pub file: CommitFile,
    pub counts: FileCounts,
    pub viewed: Option<ViewedState>,
}

/// A viewed mark GitHub is asked for, kept until it answers. One at a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewedMark {
    pub seq: u64,
    pub path: String,
    pub viewed: bool,
}

/// What the files screen shows: the whole pull request, or the commit at
/// this index of `PrFiles.commits`, oldest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReviewView {
    #[default]
    Whole,
    Commit(usize),
}

/// The files screen of pull request `number`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrFiles {
    pub number: u64,
    pub head: CommitId,
    pub base: CommitId,
    /// The range shown: merge base to head once resolved, or the picked
    /// commit against its first parent.
    pub range: Option<CommitRange>,
    /// Merge base to head, once the whole view's files have come.
    pub whole: Option<CommitRange>,
    pub view: ReviewView,
    /// The pull request's commits, oldest first, and whether the list was
    /// cut; read on the first `c`.
    pub commits: Option<(Vec<PrCommit>, bool)>,
    pub rows: Vec<PrFile>,
    pub cursor: usize,
    /// The file to select once the view being listed comes, picked by
    /// path; kept through a view that fails or lists nothing.
    pub keep: Option<RepoPath>,
    pub focus: FilesFocus,
    pub stage: FilesStage,
    /// This screen fetched already, so a commit still missing is an error
    /// rather than another fetch.
    pub fetched: bool,
    /// The stage's load in flight; any other answer is dropped.
    pub seq: Option<u64>,
    /// The viewed mark in flight on GitHub, if any.
    pub marking: Option<ViewedMark>,
    /// The review thread `]`/`[` went to, by index into the discussion's
    /// threads.
    pub thread: Option<usize>,
    /// A thread whose row has not streamed in yet, scrolled to when it does.
    pub jump: Option<usize>,
}

impl PrFiles {
    fn begin(number: u64, head: CommitId, base: CommitId, seq: u64) -> Self {
        Self {
            number,
            head,
            base,
            range: None,
            whole: None,
            view: ReviewView::Whole,
            commits: None,
            rows: Vec::new(),
            cursor: 0,
            keep: None,
            focus: FilesFocus::List,
            stage: FilesStage::Checking,
            fetched: false,
            seq: Some(seq),
            marking: None,
            thread: None,
            jump: None,
        }
    }

    pub fn selected(&self) -> Option<&PrFile> {
        self.rows.get(self.cursor)
    }

    /// Files marked viewed, of all listed.
    pub fn viewed_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| r.viewed == Some(ViewedState::Viewed))
            .count()
    }

    /// A load is in flight.
    #[cfg(test)]
    pub fn loading(&self) -> bool {
        matches!(
            self.stage,
            FilesStage::Checking | FilesStage::Fetching | FilesStage::Listing
        )
    }
}

/// The row of the file kept across a switch of view, or the first row
/// when the new view does not list it.
fn kept_cursor(rows: &[PrFile], keep: Option<&RepoPath>) -> usize {
    keep.and_then(|path| rows.iter().position(|r| &r.file.path == path))
        .unwrap_or(0)
}

/// `rows` with GitHub's viewed state put on each by exact path. A path
/// that is not UTF-8, or one GitHub did not list, is unknown.
fn with_viewed(rows: Vec<PrFileRow>, viewed: &[(String, ViewedState)]) -> Vec<PrFile> {
    rows.into_iter()
        .map(|row| {
            let state = std::str::from_utf8(row.file.path.as_bytes())
                .ok()
                .and_then(|path| viewed.iter().find(|(p, _)| p == path))
                .map(|(_, v)| *v);
            PrFile {
                file: row.file,
                counts: row.counts,
                viewed: state,
            }
        })
        .collect()
}

pub(super) fn files_screen(state: &AppState) -> Option<&PrFiles> {
    match state.review.screen {
        ReviewScreen::Files(_) => state.review.files.as_ref(),
        _ => None,
    }
}

/// The files screen is shown with the diff focused.
pub(crate) fn review_diff_focused(state: &AppState) -> bool {
    files_screen(state).is_some_and(|f| f.focus == FilesFocus::Diff)
}

/// The open detail's number, head and base, when `f` may open its files.
fn detail_oids(review: &ReviewState) -> Result<(u64, CommitId, CommitId), Reason> {
    let ReviewScreen::Detail(number) = review.screen else {
        return Err(NO_DETAIL.into());
    };
    let detail = match &review.detail {
        Some(detail) if !review.detail_fetch.loading => detail,
        _ => return Err(DETAIL_LOADING.into()),
    };
    match (&detail.head_oid, &detail.base_oid) {
        (Some(head), Some(base)) => Ok((number, head.clone(), base.clone())),
        _ => Err(NO_OIDS.into()),
    }
}

/// Why `f` cannot open the files: no loaded pull request with both
/// commit ids, no known repository on a forge host, or git is busy.
pub(crate) fn review_files_disabled(state: &AppState) -> Option<Reason> {
    if let Err(reason) = detail_oids(&state.review) {
        return Some(reason);
    }
    if state.review_repo().is_none() {
        return Some(super::state::NO_FORGE.into());
    }
    state.op_is_running().then_some(OP_RUNNING.into())
}

/// `r` asks GitHub about the pull request, which the files screen hides.
pub(crate) fn refresh_in_files(state: &AppState) -> Option<Reason> {
    matches!(state.review.screen, ReviewScreen::Files(_)).then_some(IN_FILES.into())
}

/// `n` and `N` step through a search there is.
pub(crate) fn review_search_step_disabled(state: &AppState) -> Option<Reason> {
    let searched = files_screen(state).is_some() && state.diff.search.is_some();
    (!searched).then_some(NOTHING_SEARCHED.into())
}

impl AppState {
    /// The repository review mode is on, from the badges or a review load.
    pub(super) fn review_repo(&self) -> Option<kika_forge::ForgeRepo> {
        self.forge.repo.clone().or_else(|| self.review.repo.clone())
    }

    /// Text rows the review diff shows, at the current screen size.
    pub(super) fn review_diff_rows(&self) -> usize {
        let screen = ratatui::layout::Rect::new(0, 0, self.size.0, self.size.1);
        let [main, _] = crate::layout::screen_layout(screen);
        super::view::files_diff_rows(main).max(1)
    }

    /// Rows of the pane the streamed diff is drawn in now.
    pub(crate) fn diff_rows(&self) -> usize {
        if files_screen(self).is_some() {
            self.review_diff_rows()
        } else {
            self.pane_rows()
        }
    }

    /// The files screen, if this answer is the one it waits for.
    pub(super) fn files_for(&mut self, tag: Tag) -> Option<&mut PrFiles> {
        if tag.generation != self.generation {
            return None;
        }
        let ReviewScreen::Files(number) = self.review.screen else {
            return None;
        };
        self.review
            .files
            .as_mut()
            .filter(|f| f.number == number && f.seq == Some(tag.seq))
    }

    pub(super) fn files_tag(&mut self) -> Tag {
        Tag {
            generation: self.generation,
            seq: self.take_seq(),
            user: false,
        }
    }

    /// `f`: show the open pull request's files, starting by checking its
    /// commits are here.
    pub(crate) fn open_review_files(&mut self) -> Vec<Effect> {
        if let Some(reason) = review_files_disabled(self) {
            self.notice = Some(reason);
            return vec![];
        }
        let Ok((number, head, base)) = detail_oids(&self.review) else {
            return vec![];
        };
        let tag = self.files_tag();
        self.review.screen = ReviewScreen::Files(number);
        self.review.files = Some(PrFiles::begin(number, head, base, tag.seq));
        self.check_review_commits(tag)
    }

    fn check_review_commits(&mut self, tag: Tag) -> Vec<Effect> {
        let (Some(files), Some(repo)) = (&self.review.files, self.review_repo()) else {
            return vec![];
        };
        vec![Effect::CheckReviewCommits {
            tag,
            head: files.head.clone(),
            base: files.base.clone(),
            hosts: self.forge.hosts.clone(),
            repo,
        }]
    }

    pub(crate) fn review_objects_checked(
        &mut self,
        tag: Tag,
        result: Result<CommitsHere, String>,
    ) -> Vec<Effect> {
        let fetched_heads = self.review.fetched_heads.clone();
        let Some(files) = self.files_for(tag) else {
            return vec![];
        };
        let key = (files.number, files.head.clone());
        let missing = match result {
            Err(message) => {
                files.stage = FilesStage::Failed(message);
                files.seq = None;
                return vec![];
            }
            Ok(CommitsHere::All) => {
                files.stage = FilesStage::Listing;
                let (head, base) = (files.head.clone(), files.base.clone());
                return vec![Effect::LoadReviewFiles { tag, head, base }];
            }
            Ok(CommitsHere::Missing { oids, remote }) => (oids, remote),
        };
        files.seq = None;
        let failed = if files.fetched {
            Some(STILL_MISSING)
        } else if fetched_heads.contains(&key) {
            Some(FETCHED_BEFORE)
        } else if missing.1.is_none() {
            Some(NO_FETCH_REMOTE)
        } else {
            None
        };
        if let Some(message) = failed {
            files.stage = FilesStage::Failed(message.into());
            return vec![];
        }
        let (oids, Some(remote)) = missing else {
            return vec![];
        };
        // A user effect holding the git slot, like a fetch from the
        // workspace: the busy mark shows and nothing else starts. The
        // files screen refuses while an op runs, so the slot is free.
        let taken = self.take_op();
        if let Some(files) = &mut self.review.files {
            match taken {
                Some(tag) => {
                    // Once per head for the session, whatever the fetch comes to.
                    files.fetched = true;
                    files.stage = FilesStage::Fetching;
                    files.seq = Some(tag.seq);
                }
                None => files.stage = FilesStage::Failed(OP_RUNNING.into()),
            }
        }
        let Some(tag) = taken else {
            return vec![];
        };
        self.review.fetched_heads.push(key);
        vec![Effect::FetchReviewCommits { tag, remote, oids }]
    }

    /// The fetch is the review's; questions it asks may be answered.
    pub(crate) fn review_fetch_running(&self, tag: Tag) -> bool {
        tag.generation == self.generation && self.holds_op(tag) && tag.user
    }

    pub(crate) fn review_head_fetched(
        &mut self,
        tag: Tag,
        result: Result<(), String>,
    ) -> Vec<Effect> {
        // Every user effect reports back once, stale or not.
        let held = self.holds_op(tag);
        self.release_op(tag);
        if held {
            // Git has gone; a question still open has nobody to answer.
            self.close_credential();
        }
        let Some(files) = self.files_for(tag) else {
            return vec![];
        };
        if let Err(message) = result {
            files.stage = FilesStage::Failed(message);
            files.seq = None;
            return vec![];
        }
        let next = self.files_tag();
        if let Some(files) = &mut self.review.files {
            files.stage = FilesStage::Checking;
            files.seq = Some(next.seq);
        }
        self.check_review_commits(next)
    }

    pub(crate) fn review_files_loaded(
        &mut self,
        tag: Tag,
        result: Result<(CommitRange, Vec<PrFileRow>), String>,
    ) -> Vec<Effect> {
        let viewed = self
            .review
            .detail
            .as_ref()
            .map(|d| d.viewed.clone())
            .unwrap_or_default();
        let Some(files) = self.files_for(tag) else {
            return vec![];
        };
        files.seq = None;
        match result {
            Ok((range, rows)) => {
                if files.view == ReviewView::Whole {
                    files.whole = Some(range.clone());
                }
                files.range = Some(range);
                files.rows = with_viewed(rows, &viewed);
                files.cursor = kept_cursor(&files.rows, files.keep.as_ref());
                // An empty list selects nothing, so the file is kept for the next view.
                if !files.rows.is_empty() {
                    files.keep = None;
                }
                files.stage = FilesStage::Ready;
            }
            Err(message) => {
                files.stage = FilesStage::Failed(message);
                return vec![];
            }
        }
        if files.rows.is_empty() {
            return self.close_diff();
        }
        // Another pull request's file, or the same file over another
        // range: its lines do not carry over, so start at its top.
        self.review_file_diff(DiffStart::Top)
    }

    /// Stream the selected file's diff over the range shown, from `start`.
    pub(super) fn review_file_diff(&mut self, start: DiffStart) -> Vec<Effect> {
        let request = self.review.files.as_ref().and_then(|files| {
            let range = files.range.as_ref()?;
            let row = files.selected()?;
            Some(DiffRequest::for_commit_file(
                range,
                &row.file,
                self.diff_options,
            ))
        });
        match request {
            Some(request) => self.open_diff(request, start),
            None => vec![],
        }
    }

    /// `j`/`k` on the file list: the next or previous file, and its diff.
    pub(super) fn move_review_file(&mut self, delta: isize) -> Vec<Effect> {
        let Some(files) = &mut self.review.files else {
            return vec![];
        };
        let next = files
            .cursor
            .saturating_add_signed(delta)
            .min(files.rows.len().saturating_sub(1));
        if next == files.cursor {
            return vec![];
        }
        files.cursor = next;
        files.thread = None;
        files.jump = None;
        self.review_file_diff(DiffStart::Keep)
    }

    pub(crate) fn toggle_review_files_focus(&mut self) {
        if let Some(files) = &mut self.review.files {
            files.focus = match files.focus {
                FilesFocus::List => FilesFocus::Diff,
                FilesFocus::Diff => FilesFocus::List,
            };
        }
    }

    /// Esc on the files screen: back to the same pull request's detail,
    /// the diff stream stopped.
    pub(super) fn close_review_files(&mut self) -> Vec<Effect> {
        let Some(files) = self.review.files.take() else {
            return vec![];
        };
        self.review.screen = ReviewScreen::Detail(files.number);
        self.close_diff()
    }

    /// Scroll the review diff, asking for more lines only past those asked.
    pub(crate) fn scroll_review_diff(&mut self, by: Scroll) -> Vec<Effect> {
        let rows = self.review_diff_rows();
        self.diff.scroll(by, rows)
    }

    /// Esc in the review diff: clear the search, else back to the list.
    pub(crate) fn review_diff_escape(&mut self) {
        if self.diff.search.take().is_none()
            && let Some(files) = &mut self.review.files
        {
            files.focus = FilesFocus::List;
        }
    }

    /// Whether review mode shows the files screen.
    pub(crate) fn review_files_shown(&self) -> bool {
        self.context != Context::Workspace && files_screen(self).is_some()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use crossterm::event::KeyCode;

    use super::*;
    use crate::diff_pane::{DIFF_PREFETCH, DiffPane};
    use crate::features::review::ReviewIntent;
    use crate::features::review::state::tests::{
        github, in_detail, opened_on, paged_detail, press, repo, type_char,
    };
    use crate::intent::AppIntent;
    use crate::keymap::KeyScope;
    use crate::test_support::diff_chunk;
    use kika_git::diff::{DiffChunk, DiffLine, LineKind};
    use kika_git::remotes::RemoteName;

    fn id(c: char) -> CommitId {
        CommitId::parse(&c.to_string().repeat(40)).unwrap()
    }

    fn head() -> CommitId {
        id('1')
    }

    fn base() -> CommitId {
        id('2')
    }

    fn range() -> CommitRange {
        CommitRange {
            oid: head(),
            parent: Some(id('3')),
        }
    }

    fn row(status: char, path: &str, counts: FileCounts) -> PrFileRow {
        PrFileRow {
            file: CommitFile {
                status,
                path: path.into(),
                orig_path: None,
            },
            counts,
        }
    }

    fn lines(added: u32, removed: u32) -> FileCounts {
        FileCounts::Lines { added, removed }
    }

    /// Paths as the fixture's viewed list has them, and one it lacks.
    fn rows() -> Vec<PrFileRow> {
        vec![
            row('M', "src/cache.rs", lines(3, 1)),
            row('M', "src/lib.rs", lines(1, 1)),
            row('A', "docs/café.md", lines(9, 0)),
            row('A', "logo.png", FileCounts::Binary),
        ]
    }

    fn check_tag(effects: &[Effect]) -> Tag {
        match effects {
            [
                Effect::CheckReviewCommits {
                    tag,
                    head: h,
                    base: b,
                    hosts,
                    repo: r,
                },
            ] => {
                assert_eq!((h, b), (&head(), &base()));
                assert_eq!((hosts, r), (&github(), &repo()));
                *tag
            }
            other => panic!("expected one check, got {other:?}"),
        }
    }

    /// The fixture's pull request open on its files, checking its commits.
    fn opened() -> (AppState, Tag) {
        let mut state = in_detail(Vec::new());
        let tag = check_tag(&type_char(&mut state, 'f'));
        assert_eq!(state.review.screen, ReviewScreen::Files(12));
        (state, tag)
    }

    fn missing(remote: Option<&str>) -> Result<CommitsHere, String> {
        Ok(CommitsHere::Missing {
            oids: vec![head()],
            remote: remote.map(RemoteName::from),
        })
    }

    fn stage(state: &AppState) -> FilesStage {
        state.review.files.as_ref().unwrap().stage.clone()
    }

    /// Open, find the commits here, and list `rows()`.
    fn ready() -> (AppState, Vec<Effect>) {
        let (mut state, tag) = opened();
        state.update(AppIntent::Review(ReviewIntent::ObjectsChecked {
            tag,
            result: Ok(CommitsHere::All),
        }));
        let effects = state.update(AppIntent::Review(ReviewIntent::FilesLoaded {
            tag,
            result: Ok((range(), rows())),
        }));
        (state, effects)
    }

    /// A pull request of 150 files whose marks came in two pages, open on
    /// its files screen with every file listed: the second page answered
    /// with `second`.
    pub(crate) fn large_ready(second: String) -> AppState {
        use kika_forge::review::fixtures::{detail_with_files, page_path};
        let mut detail = paged_detail(&[detail_with_files(0..100, 150, Some("c1")), second]);
        // The first queue row, which the detail answers.
        detail.number = 12;
        let mut state = opened_on(detail);
        let tag = check_tag(&type_char(&mut state, 'f'));
        state.update(AppIntent::Review(ReviewIntent::ObjectsChecked {
            tag,
            result: Ok(CommitsHere::All),
        }));
        let rows = (0..150)
            .map(|i| row('M', &page_path(i), lines(1, 0)))
            .collect();
        state.update(AppIntent::Review(ReviewIntent::FilesLoaded {
            tag,
            result: Ok((range(), rows)),
        }));
        state
    }

    #[test]
    fn a_file_past_the_first_hundred_shows_the_mark_its_later_page_gave() {
        use kika_forge::review::fixtures::{files_page, page_state};
        let state = large_ready(files_page(100..150, None));
        assert_eq!(state.review.detail.as_ref().unwrap().viewed.len(), 150);
        let files = state.review.files.as_ref().unwrap();
        assert_eq!(files.rows.len(), 150);
        assert_eq!(page_state(120), "VIEWED");
        assert_eq!(files.rows[120].viewed, Some(ViewedState::Viewed));
        assert_eq!(files.rows[121].viewed, Some(ViewedState::Dismissed));
        assert_eq!(files.rows[149].viewed, Some(ViewedState::Unviewed));
        assert!(files.rows.iter().all(|r| r.viewed.is_some()));
    }

    #[test]
    fn a_later_page_rate_limited_leaves_its_files_unknown_and_v_waits() {
        use kika_forge::REFRESH_LIMITED;
        use kika_forge::github::fixtures::GRAPHQL_RATE_LIMITED;
        let mut state = large_ready(GRAPHQL_RATE_LIMITED.to_string());
        let files = state.review.files.as_ref().unwrap();
        assert_eq!(files.rows[99].viewed, Some(ViewedState::Viewed));
        assert_eq!(files.rows[120].viewed, None);
        state.review.files.as_mut().unwrap().cursor = 120;
        assert_eq!(type_char(&mut state, 'v'), []);
        assert_eq!(state.notice.as_deref(), Some(REFRESH_LIMITED));
    }

    fn streamed(effects: &[Effect]) -> (&DiffRequest, usize) {
        effects
            .iter()
            .find_map(|e| match e {
                Effect::LoadDiff { request, want, .. } => Some((request, *want)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no diff streamed: {effects:?}"))
    }

    fn fetches(effects: &[Effect]) -> usize {
        effects
            .iter()
            .filter(|e| matches!(e, Effect::FetchReviewCommits { .. }))
            .count()
    }

    #[test]
    fn with_the_commits_here_f_lists_from_the_merge_base_and_streams_the_first_file() {
        let (mut state, tag) = opened();
        assert_eq!(stage(&state), FilesStage::Checking);
        let effects = state.update(AppIntent::Review(ReviewIntent::ObjectsChecked {
            tag,
            result: Ok(CommitsHere::All),
        }));
        assert_eq!(
            effects,
            [Effect::LoadReviewFiles {
                tag,
                head: head(),
                base: base()
            }]
        );
        assert_eq!(stage(&state), FilesStage::Listing);
        let effects = state.update(AppIntent::Review(ReviewIntent::FilesLoaded {
            tag,
            result: Ok((range(), rows())),
        }));
        assert_eq!(fetches(&effects), 0);
        let (request, want) = streamed(&effects);
        assert_eq!(request.commit, Some(range()));
        assert_eq!(request.path, "src/cache.rs".into());
        // Demand is measured in the review diff's rows, not the workspace's.
        assert_eq!(want, state.review_diff_rows() + DIFF_PREFETCH);
        assert_ne!(state.review_diff_rows(), state.pane_rows());

        let files = state.review.files.as_ref().unwrap();
        assert_eq!(files.stage, FilesStage::Ready);
        let viewed: Vec<Option<ViewedState>> = files.rows.iter().map(|r| r.viewed).collect();
        assert_eq!(
            viewed,
            [
                Some(ViewedState::Viewed),
                Some(ViewedState::Dismissed),
                Some(ViewedState::Unviewed),
                None,
            ]
        );
        assert_eq!(files.viewed_count(), 1);
        assert_eq!(state.panel_focus(), KeyScope::ReviewFiles);
    }

    #[cfg(unix)]
    #[test]
    fn a_local_name_that_is_not_utf8_is_unknown_not_matched_lossily() {
        let viewed = [("x\u{FFFD}".to_string(), ViewedState::Viewed)];
        let mut odd = row('M', "x", lines(1, 0));
        odd.file.path = kika_git::path::RepoPath::from_bytes(b"x\xff");
        let rows = with_viewed(vec![odd], &viewed);
        assert_eq!(rows[0].viewed, None);
    }

    #[test]
    fn missing_commits_are_fetched_once_from_the_matched_remote() {
        let (mut state, tag) = opened();
        let effects = state.update(AppIntent::Review(ReviewIntent::ObjectsChecked {
            tag,
            result: missing(Some("upstream")),
        }));
        let fetch_tag = match effects.as_slice() {
            [Effect::FetchReviewCommits { tag, remote, oids }] => {
                assert_eq!(*remote, RemoteName::from("upstream"));
                assert_eq!(*oids, [head()]);
                *tag
            }
            other => panic!("expected one fetch, got {other:?}"),
        };
        assert_eq!(stage(&state), FilesStage::Fetching);
        assert_eq!(
            (state.busy_count(), state.held_seq()),
            (1, Some(fetch_tag.seq))
        );

        // Fetched: look again. Still missing: an error, never a second fetch.
        let effects = state.update(AppIntent::Review(ReviewIntent::HeadFetched {
            tag: fetch_tag,
            result: Ok(()),
        }));
        assert_eq!((state.busy_count(), state.held_seq()), (0, None));
        let tag = check_tag(&effects);
        let effects = state.update(AppIntent::Review(ReviewIntent::ObjectsChecked {
            tag,
            result: missing(Some("upstream")),
        }));
        assert_eq!(effects, []);
        assert_eq!(stage(&state), FilesStage::Failed(STILL_MISSING.into()));

        // `f` again, even after leaving review mode, fetches nothing more
        // for the same head.
        press(&mut state, KeyCode::Esc);
        let tag = check_tag(&type_char(&mut state, 'f'));
        let effects = state.update(AppIntent::Review(ReviewIntent::ObjectsChecked {
            tag,
            result: missing(Some("upstream")),
        }));
        assert_eq!(fetches(&effects), 0);
        assert_eq!(stage(&state), FilesStage::Failed(FETCHED_BEFORE.into()));
        state.review.restart();
        assert_eq!(state.review.fetched_heads, [(12, head())]);
    }

    #[test]
    fn a_failed_fetch_says_why_and_frees_the_git_slot() {
        let (mut state, tag) = opened();
        let effects = state.update(AppIntent::Review(ReviewIntent::ObjectsChecked {
            tag,
            result: missing(Some("origin")),
        }));
        let [Effect::FetchReviewCommits { tag, .. }] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        let effects = state.update(AppIntent::Review(ReviewIntent::HeadFetched {
            tag: *tag,
            result: Err("fatal: could not read from remote".into()),
        }));
        assert_eq!(effects, []);
        assert_eq!(
            stage(&state),
            FilesStage::Failed("fatal: could not read from remote".into())
        );
        assert_eq!((state.busy_count(), state.held_seq()), (0, None));
    }

    #[test]
    fn the_review_fetch_may_ask_for_a_password_but_nothing_else_may() {
        let (mut state, tag) = opened();
        let effects = state.update(AppIntent::Review(ReviewIntent::ObjectsChecked {
            tag,
            result: missing(Some("origin")),
        }));
        let [Effect::FetchReviewCommits { tag, .. }] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        let ask = |tag| AppIntent::CredentialAsked {
            tag,
            prompt: "Password:".into(),
            confirm: false,
        };
        let stray = Tag { seq: 999, ..*tag };
        assert_eq!(state.update(ask(stray)), [Effect::AnswerCredential(None)]);
        assert_eq!(state.update(ask(*tag)), []);
        assert_eq!(state.key_focus(), KeyScope::Credential);
        // The fetch ends: the question goes with it.
        state.update(AppIntent::Review(ReviewIntent::HeadFetched {
            tag: *tag,
            result: Err("cancelled".into()),
        }));
        assert!(state.popups.is_empty());
    }

    #[test]
    fn a_stale_fetch_result_leaves_the_running_fetchs_question_open() {
        let (mut state, tag) = opened();
        let effects = state.update(AppIntent::Review(ReviewIntent::ObjectsChecked {
            tag,
            result: missing(Some("origin")),
        }));
        let [Effect::FetchReviewCommits { tag, .. }] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        state.update(AppIntent::CredentialAsked {
            tag: *tag,
            prompt: "Password:".into(),
            confirm: false,
        });
        assert_eq!(state.key_focus(), KeyScope::Credential);

        let stale = Tag {
            seq: tag.seq + 99,
            ..*tag
        };
        state.update(AppIntent::Review(ReviewIntent::HeadFetched {
            tag: stale,
            result: Err("cancelled".into()),
        }));

        assert_eq!(state.key_focus(), KeyScope::Credential);
        assert!(state.holds_op(*tag));
    }

    #[test]
    fn with_the_slot_taken_nothing_is_fetched_and_the_screen_says_why() {
        let (mut state, tag) = opened();
        state.hold_op(77);

        let effects = state.update(AppIntent::Review(ReviewIntent::ObjectsChecked {
            tag,
            result: missing(Some("origin")),
        }));

        assert_eq!(effects, []);
        assert_eq!(stage(&state), FilesStage::Failed(OP_RUNNING.into()));
        assert_eq!(state.review.fetched_heads, []);
        assert_eq!(state.held_seq(), Some(77));
    }

    #[test]
    fn with_no_remote_for_the_repository_nothing_is_fetched_and_the_screen_says_why() {
        let (mut state, tag) = opened();
        let effects = state.update(AppIntent::Review(ReviewIntent::ObjectsChecked {
            tag,
            result: missing(None),
        }));
        assert_eq!(effects, []);
        assert_eq!(stage(&state), FilesStage::Failed(NO_FETCH_REMOTE.into()));
        assert_eq!((state.busy_count(), state.held_seq()), (0, None));
        assert!(state.review.fetched_heads.is_empty());
    }

    #[test]
    fn stale_answers_are_dropped_at_every_stage() {
        let (mut state, tag) = opened();
        let old = Tag {
            seq: tag.seq - 1,
            ..tag
        };
        let other = Tag {
            generation: tag.generation + 1,
            ..tag
        };
        for stale in [old, other] {
            let effects = state.update(AppIntent::Review(ReviewIntent::ObjectsChecked {
                tag: stale,
                result: Ok(CommitsHere::All),
            }));
            assert_eq!(effects, []);
            assert_eq!(stage(&state), FilesStage::Checking);
        }
        state.update(AppIntent::Review(ReviewIntent::ObjectsChecked {
            tag,
            result: missing(Some("origin")),
        }));
        let fetch = Tag {
            seq: state.held_seq().unwrap(),
            user: true,
            ..tag
        };
        let stale = Tag {
            generation: tag.generation + 1,
            ..fetch
        };
        assert_eq!(
            state.update(AppIntent::Review(ReviewIntent::HeadFetched {
                tag: stale,
                result: Ok(())
            })),
            []
        );
        assert_eq!(stage(&state), FilesStage::Fetching);
        let tag = check_tag(&state.update(AppIntent::Review(ReviewIntent::HeadFetched {
            tag: fetch,
            result: Ok(()),
        })));
        state.update(AppIntent::Review(ReviewIntent::ObjectsChecked {
            tag,
            result: Ok(CommitsHere::All),
        }));
        for stale in [
            old,
            Tag {
                generation: 9,
                ..tag
            },
        ] {
            let effects = state.update(AppIntent::Review(ReviewIntent::FilesLoaded {
                tag: stale,
                result: Ok((range(), rows())),
            }));
            assert_eq!(effects, []);
            assert_eq!(stage(&state), FilesStage::Listing);
        }
    }

    #[test]
    fn esc_goes_back_to_the_same_pull_request_and_stops_the_diff() {
        let (mut state, effects) = ready();
        streamed(&effects);
        let effects = press(&mut state, KeyCode::Esc);
        assert_eq!(effects, [Effect::CancelDiff]);
        assert_eq!(state.review.screen, ReviewScreen::Detail(12));
        assert!(state.review.detail.is_some());
        assert_eq!(state.review.files, None);
        assert_eq!(state.diff, DiffPane::default());
        assert_eq!(state.panel_focus(), KeyScope::Review);
        // An answer to the files screen that has gone changes nothing.
        let tag = Tag {
            generation: state.generation,
            seq: 1,
            user: false,
        };
        assert_eq!(
            state.update(AppIntent::Review(ReviewIntent::FilesLoaded {
                tag,
                result: Ok((range(), rows()))
            })),
            []
        );
    }

    #[test]
    fn a_workspace_status_load_landing_late_leaves_the_review_diff_alone() {
        use kika_git::FileEntry;
        use kika_git::status::{BranchStatus, EntryKind, Head, RepoStatus};
        let entry = FileEntry {
            path: "a.txt".into(),
            orig_path: None,
            index: '.',
            worktree: 'M',
            kind: EntryKind::Changed,
        };
        for files in [vec![entry], Vec::new()] {
            let (mut state, _) = ready();
            let diff = state.diff.clone();
            state.files.seq = Some(500);
            state.set_busy(state.busy_count() + 1);
            let status = RepoStatus {
                branch: BranchStatus {
                    head: Head::Detached("abc".into()),
                    upstream: None,
                    ahead_behind: None,
                    in_progress: Vec::new(),
                    bisect: None,
                },
                files,
            };
            let effects = state.update(AppIntent::StatusLoaded {
                tag: Tag {
                    generation: state.generation,
                    seq: 500,
                    user: true,
                },
                result: Ok(status),
            });
            assert!(
                !effects
                    .iter()
                    .any(|e| matches!(e, Effect::LoadDiff { .. } | Effect::CancelDiff)),
                "{effects:?}"
            );
            assert_eq!(state.diff, diff);
        }
    }

    #[test]
    fn back_in_the_workspace_an_open_commit_file_streams_again() {
        use crate::test_support::commit_files_list;
        let (mut state, _) = ready();
        let open = commit_files_list(range(), "1111111", 1, Some(vec![rows()[0].file.clone()]));
        state.commits.open = Some(open);
        press(&mut state, KeyCode::Esc);
        press(&mut state, KeyCode::Esc);
        let effects = press(&mut state, KeyCode::Esc);
        assert_eq!(state.context, Context::Workspace);
        assert_eq!(streamed(&effects).0.commit, Some(range()));
    }

    #[test]
    fn f_is_off_until_a_loaded_pull_request_has_both_commit_ids() {
        let mut state = in_detail(Vec::new());
        state.review.detail.as_mut().unwrap().head_oid = None;
        assert_eq!(type_char(&mut state, 'f'), []);
        assert_eq!(state.notice.as_deref(), Some(NO_OIDS));
        assert_eq!(state.review.screen, ReviewScreen::Detail(12));

        let mut state = in_detail(Vec::new());
        state.review.detail_fetch.loading = true;
        assert_eq!(type_char(&mut state, 'f'), []);
        assert_eq!(state.notice.as_deref(), Some(DETAIL_LOADING));

        let mut state = in_detail(Vec::new());
        state.hold_op(77);
        assert_eq!(type_char(&mut state, 'f'), []);
        assert_eq!(state.notice.as_deref(), Some(OP_RUNNING));

        // Not from the queue either.
        let mut state = in_detail(Vec::new());
        press(&mut state, KeyCode::Esc);
        assert_eq!(type_char(&mut state, 'f'), []);
        assert_eq!(state.notice.as_deref(), Some(NO_DETAIL));
    }

    #[test]
    fn j_and_k_pick_a_file_and_tab_gives_the_diff_the_keys() {
        let (mut state, _) = ready();
        let effects = type_char(&mut state, 'j');
        assert_eq!(streamed(&effects).0.path, "src/lib.rs".into());
        assert_eq!(type_char(&mut state, 'k').len(), 2);
        assert_eq!(type_char(&mut state, 'k'), []);
        press(&mut state, KeyCode::Tab);
        assert_eq!(state.panel_focus(), KeyScope::ReviewDiff);
        // j now scrolls the diff, and the file stays.
        type_char(&mut state, 'j');
        assert_eq!(state.review.files.as_ref().unwrap().cursor, 0);
        press(&mut state, KeyCode::Tab);
        assert_eq!(state.panel_focus(), KeyScope::ReviewFiles);
        // `r` would ask GitHub about a screen that is not shown.
        assert_eq!(type_char(&mut state, 'r'), []);
        assert_eq!(state.notice.as_deref(), Some(IN_FILES));
    }

    /// The streamed diff's first chunk: `n` lines, every tenth with "hit".
    fn load_lines(state: &mut AppState, n: usize, done: bool) {
        let tag = Tag {
            generation: state.generation,
            seq: state.diff.seq.unwrap(),
            user: false,
        };
        let lines = (0..n)
            .map(|i| DiffLine {
                kind: LineKind::Context,
                text: if i % 10 == 0 {
                    format!("hit {i}")
                } else {
                    format!("line {i}")
                },
            })
            .collect();
        state.update(diff_chunk(tag, DiffChunk { lines, done }));
    }

    fn current_row(state: &AppState) -> Option<usize> {
        let search = state.diff.search.as_ref()?;
        search.current_match().map(|m| m.row)
    }

    #[test]
    fn the_focused_diff_searches_its_loaded_lines_and_n_and_n_cycle() {
        let (mut state, _) = ready();
        load_lines(&mut state, 100, true);
        press(&mut state, KeyCode::Tab);
        assert_eq!(type_char(&mut state, 'n'), []);
        assert_eq!(state.notice.as_deref(), Some("Nothing searched yet (/)"));
        type_char(&mut state, '/');
        assert_eq!(
            state.input.as_ref().map(|i| i.target),
            Some(crate::list_filter::InputTarget::PreviewSearch)
        );
        for c in "hit".chars() {
            type_char(&mut state, c);
        }
        press(&mut state, KeyCode::Enter);
        assert_eq!(state.input, None);
        let search = state.diff.search.clone().unwrap();
        assert_eq!(search.matches.len(), 10);
        assert_eq!(current_row(&state), Some(0));
        type_char(&mut state, 'n');
        assert_eq!(current_row(&state), Some(10));
        state.update(AppIntent::Key(crate::keymap::KeyPress::new(
            KeyCode::Char('N'),
            crossterm::event::KeyModifiers::SHIFT,
        )));
        type_char(&mut state, 'N');
        assert_eq!(current_row(&state), Some(90));
        assert_eq!(state.notice.as_deref(), Some("Search wrapped"));
        // The match is scrolled into view.
        let rows = state.review_diff_rows();
        assert!(state.diff.scroll <= 90 && 90 < state.diff.scroll + rows);
        // Esc clears the search, then gives the list the keys.
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.diff.search, None);
        assert_eq!(state.panel_focus(), KeyScope::ReviewDiff);
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.panel_focus(), KeyScope::ReviewFiles);
    }

    #[test]
    fn a_search_finds_lines_that_stream_in_after_it() {
        let (mut state, _) = ready();
        load_lines(&mut state, 5, false);
        press(&mut state, KeyCode::Tab);
        type_char(&mut state, '/');
        for c in "hit".chars() {
            type_char(&mut state, c);
        }
        press(&mut state, KeyCode::Enter);
        assert_eq!(state.diff.search.as_ref().unwrap().matches.len(), 1);
        load_lines(&mut state, 20, true);
        let search = state.diff.search.clone().unwrap();
        assert_eq!(search.matches.len(), 3);
    }

    #[test]
    fn scrolling_asks_for_more_lines_only_past_those_asked_for() {
        let (mut state, effects) = ready();
        let (_, want) = streamed(&effects);
        load_lines(&mut state, want, false);
        press(&mut state, KeyCode::Tab);
        let rows = state.review_diff_rows();
        let seq = state.diff.seq.unwrap();
        // A page down asks for that much more, in the review diff's rows.
        let effects = press(&mut state, KeyCode::PageDown);
        assert_eq!(
            effects,
            [Effect::DiffDemand {
                seq,
                want: rows + rows + DIFF_PREFETCH,
            }]
        );
        // Back up and down again stays within what was asked for.
        assert_eq!(type_char(&mut state, 'k'), []);
        assert_eq!(type_char(&mut state, 'j'), []);
        assert_eq!(state.diff.scroll, rows);
    }
}
