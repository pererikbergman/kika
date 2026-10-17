//! A request's review threads on its files screen: read from the forge
//! right after the detail (`LoadPrDiscussion`), placed on the lines of the
//! streamed diff by `kika_forge::anchor`, and stepped through with `]` and
//! `[`. A thread whose row has not streamed in yet is waited for: core
//! calls [`AppState::review_jump_landed`] after each diff chunk.

use std::collections::HashMap;

use super::files::{FilesStage, PrFiles, ReviewView, files_screen};
use super::state::{NOT_READY, ReviewFetch, ReviewProblem, ReviewScreen};
use crate::diff_pane::{DIFF_PREFETCH, DiffStart};
use crate::effect::{Effect, Tag};
use crate::keymap::Reason;
use crate::model::AppState;
use kika_forge::ForgeRepo;
use kika_forge::anchor::{self, AnchorView, Placement, RowNumber};
use kika_forge::discussion::PrDiscussion;
use kika_forge::forge::{Words, words_or_default};
use kika_forge::review::ReviewError;
use kika_git::commit_files::CommitId;
use kika_git::patch::{LineId, LineSide};
use kika_git::path::RepoPath;

const THREADS_LOADING: &str = "Review threads are still loading";
const THREADS_FAILED: &str = "Review threads could not be read";
const NO_THREADS: &str = "No unresolved review threads";
const NOT_LOADED: &str = "Review threads are not loaded";
const NO_NEXT: &str = "No unresolved thread after this one";
const NO_PREV: &str = "No unresolved thread before this one";

/// Rows kept above a thread's line when it is scrolled to.
const ABOVE: usize = 3;

/// One file's threads, by index into the discussion's: those drawn above
/// the diff (outdated, about the file, on another view, or not in this
/// diff) in GitHub's order, and those drawn under a line, by that line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FileThreads {
    pub top: Vec<usize>,
    pub lines: Vec<(LineId, usize)>,
}

/// What placing a thread needs: the discussion, the view shown and the
/// head the files screen reviews; and, for its labels, how the forge
/// names the request.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ThreadScope<'a> {
    pub discussion: &'a PrDiscussion,
    pub view: AnchorView,
    pub head: &'a CommitId,
    pub words: &'static Words,
}

impl<'a> ThreadScope<'a> {
    pub(crate) fn of(files: &'a PrFiles, discussion: &'a PrDiscussion) -> Self {
        let view = match files.view {
            ReviewView::Whole => AnchorView::WholePr,
            ReviewView::Commit(_) => AnchorView::Commit,
        };
        Self {
            discussion,
            view,
            head: &files.head,
            words: words_or_default(None),
        }
    }

    /// The scope with its labels in `words`, for what is drawn.
    pub(crate) fn in_words(self, words: &'static Words) -> Self {
        Self { words, ..self }
    }

    /// Where thread `t` goes in this view; `None` when there is no such
    /// thread.
    pub(crate) fn placement(&self, t: usize) -> Option<Placement> {
        let thread = self.discussion.threads.get(t)?;
        Some(anchor::place(
            thread,
            self.view,
            self.head,
            self.discussion.head_oid.as_ref(),
        ))
    }

    /// The threads of the local file `path`, matched as viewed marks are:
    /// by its name as UTF-8, so a name that is not matches nothing. With
    /// `done_rows`, the numbers of the whole streamed diff, a line thread
    /// whose line is not in it goes to the top, "not in this diff".
    pub(crate) fn file_threads(
        &self,
        path: &RepoPath,
        done_rows: Option<&[RowNumber]>,
    ) -> FileThreads {
        let Ok(path) = std::str::from_utf8(path.as_bytes()) else {
            return FileThreads::default();
        };
        let mut threads = FileThreads::default();
        for (t, thread) in self.discussion.threads.iter().enumerate() {
            if thread.path != path {
                continue;
            }
            match self.placement(t) {
                Some(Placement::Line { end, .. }) => threads.lines.push((end, t)),
                _ => threads.top.push(t),
            }
        }
        if let Some(numbers) = done_rows {
            let found = first_rows(numbers, threads.lines.iter().map(|(id, _)| *id));
            let (here, gone): (Vec<_>, Vec<_>) = threads
                .lines
                .into_iter()
                .partition(|(id, _)| found.contains_key(id));
            threads.lines = here;
            threads.top.extend(gone.into_iter().map(|(_, t)| t));
            threads.top.sort_unstable();
        }
        // By line number; on a tie the old side first. Stable, so GitHub's
        // order breaks what is left.
        threads
            .lines
            .sort_by_key(|(id, _)| (id.line, id.side != LineSide::Old));
        threads
    }
}

/// The first row of each of `ids` in one pass over `numbers`.
pub(crate) fn first_rows(
    numbers: &[RowNumber],
    ids: impl Iterator<Item = LineId>,
) -> HashMap<LineId, usize> {
    let mut wanted: HashMap<LineId, Option<usize>> = ids.map(|id| (id, None)).collect();
    if wanted.is_empty() {
        return HashMap::new();
    }
    for (row, &(old, new)) in numbers.iter().enumerate() {
        for id in [old.map(LineId::removed), new.map(LineId::added)]
            .into_iter()
            .flatten()
        {
            if let Some(slot @ None) = wanted.get_mut(&id) {
                *slot = Some(row);
            }
        }
    }
    wanted
        .into_iter()
        .filter_map(|(id, row)| Some((id, row?)))
        .collect()
}

/// Every unresolved thread on a listed file, in the order `]` visits
/// them: by file, then each file's top block, then its lines.
fn nav_order(files: &PrFiles, scope: &ThreadScope) -> Vec<(usize, usize)> {
    let mut order = Vec::new();
    for (i, row) in files.rows.iter().enumerate() {
        let threads = scope.file_threads(&row.file.path, None);
        for t in threads
            .top
            .iter()
            .copied()
            .chain(threads.lines.iter().map(|(_, t)| *t))
        {
            if !scope.discussion.threads[t].resolved {
                order.push((i, t));
            }
        }
    }
    order
}

/// The files screen, ready, with the discussion read.
pub(super) fn ready_threads(state: &AppState) -> Result<(&PrFiles, &PrDiscussion), Reason> {
    let files = files_screen(state)
        .filter(|f| f.stage == FilesStage::Ready && state.review_files_shown())
        .ok_or(NOT_READY)?;
    let fetch = &state.review.discussion_fetch;
    if fetch.loading {
        return Err(THREADS_LOADING.into());
    }
    if fetch.error.is_some() {
        return Err(THREADS_FAILED.into());
    }
    let discussion = state.review.discussion.as_ref().ok_or(NOT_LOADED)?;
    Ok((files, discussion))
}

/// The diff title's word on the threads: loading, unavailable, the
/// position of the one `]`/`[` went to, or how many are unresolved. `+`
/// when GitHub has more than were read. Empty with nothing asked.
pub(crate) fn thread_note(state: &AppState) -> String {
    let fetch = &state.review.discussion_fetch;
    if fetch.loading {
        return " · threads loading…".into();
    }
    if fetch.error.is_some() {
        return " · threads unavailable (esc, r to retry)".into();
    }
    let (Some(files), Some(discussion)) = (files_screen(state), &state.review.discussion) else {
        return String::new();
    };
    let order = nav_order(files, &ThreadScope::of(files, discussion));
    let cut = if discussion.threads_total > discussion.threads.len() as u64 {
        "+"
    } else {
        ""
    };
    match files
        .thread
        .and_then(|t| order.iter().position(|&(_, x)| x == t))
    {
        Some(at) => format!(" · thread {}/{}{cut}", at + 1, order.len()),
        None => format!(" · {}{cut} unresolved", order.len()),
    }
}

/// Why `]` and `[` cannot step: the files are not shown and listed, the
/// threads are loading or failed, or no listed file has an unresolved one.
pub(crate) fn thread_nav_disabled(state: &AppState) -> Option<Reason> {
    match ready_threads(state) {
        Err(reason) => Some(reason),
        Ok((files, discussion)) => nav_order(files, &ThreadScope::of(files, discussion))
            .is_empty()
            .then_some(NO_THREADS.into()),
    }
}

impl AppState {
    /// Read the open pull request's threads and conversation, after its
    /// detail and only where the detail could go.
    pub(super) fn load_review_discussion(&mut self, number: u64) -> Vec<Effect> {
        let (hosts, repo) = match self.review_target() {
            Ok(target) => target,
            Err(problem) => {
                self.review.discussion_fetch = ReviewFetch {
                    seq: None,
                    loading: false,
                    error: Some(problem),
                };
                return vec![];
            }
        };
        let tag = self.review_tag();
        self.review.discussion_fetch = ReviewFetch {
            seq: Some(tag.seq),
            loading: true,
            error: None,
        };
        vec![Effect::LoadPrDiscussion {
            tag,
            hosts,
            repo,
            number,
            limited: self.review_limits(),
        }]
    }

    pub(crate) fn discussion_loaded(
        &mut self,
        tag: Tag,
        number: u64,
        repo: Option<ForgeRepo>,
        result: Result<Option<PrDiscussion>, ReviewError>,
    ) {
        let shown = matches!(
            self.review.screen,
            ReviewScreen::Detail(n) | ReviewScreen::Files(n) | ReviewScreen::Conversation(n)
                if n == number
        );
        if !shown || !self.review_current(tag, &self.review.discussion_fetch) {
            return;
        }
        self.review.discussion_fetch.loading = false;
        match result {
            Ok(found) => {
                self.review_answered(repo, None);
                self.review.discussion_fetch.error =
                    found.is_none().then_some(ReviewProblem::Missing(number));
                self.review.discussion = found;
            }
            Err(error) => {
                self.review.discussion_fetch.error = Some(self.review_failed(repo, error));
            }
        }
        // Thread indexes were into the discussion that has gone. The
        // reply's thread is taken on a failed read too, so no later read
        // reopens it.
        let reopen = self.review.files.as_mut().and_then(|files| {
            files.thread = None;
            files.jump = None;
            files.reader = None;
            files.reopen_thread.take()
        });
        // A reply was posted from the reader: back on its thread.
        if let Some(node_id) = reopen {
            self.reopen_thread_reader(&node_id);
        }
        // A comment was posted from the conversation: it is the last item.
        if matches!(self.review.screen, ReviewScreen::Conversation(_)) {
            self.review.conversation = None;
            self.render_review_conversation();
            self.scroll_review_conversation(isize::MAX);
        }
    }

    /// `]`, or `[` with `back`: the next or previous unresolved thread,
    /// across the files, without wrapping round.
    pub(crate) fn step_review_thread(&mut self, back: bool) -> Vec<Effect> {
        if let Some(reason) = thread_nav_disabled(self) {
            self.notice = Some(reason);
            return vec![];
        }
        let Ok((files, discussion)) = ready_threads(self) else {
            return vec![];
        };
        let order = nav_order(files, &ThreadScope::of(files, discussion));
        let at = files
            .thread
            .and_then(|t| order.iter().position(|&(_, x)| x == t));
        let cursor = files.cursor;
        let target = match (at, back) {
            (Some(at), false) => order.get(at + 1),
            (Some(at), true) => at.checked_sub(1).and_then(|at| order.get(at)),
            (None, false) => order.iter().find(|(file, _)| *file >= cursor),
            (None, true) => order.iter().rev().find(|(file, _)| *file <= cursor),
        };
        let Some(&(file, thread)) = target else {
            self.notice = Some(if back { NO_PREV } else { NO_NEXT }.into());
            return vec![];
        };
        let mut effects = Vec::new();
        if let Some(files) = &mut self.review.files {
            files.thread = Some(thread);
            files.jump = None;
            if files.cursor != file {
                files.cursor = file;
                effects = self.review_file_diff(DiffStart::Top);
            }
        }
        effects.extend(self.land_thread(thread));
        effects
    }

    /// Where thread `t` is in the files screen's view.
    fn thread_placement(&self, t: usize) -> Option<Placement> {
        let files = self.review.files.as_ref()?;
        let discussion = self.review.discussion.as_ref()?;
        ThreadScope::of(files, discussion).placement(t)
    }

    fn set_jump(&mut self, t: Option<usize>) {
        if let Some(files) = &mut self.review.files {
            files.jump = t;
        }
    }

    /// Scroll to thread `t`: the top for one above the diff, a few rows
    /// above its line once that has streamed in, else wait for it.
    fn land_thread(&mut self, t: usize) -> Vec<Effect> {
        let Some(Placement::Line { end, .. }) = self.thread_placement(t) else {
            self.diff.scroll = 0;
            self.set_jump(None);
            return vec![];
        };
        if let Some(effects) = self.scroll_to_line(end) {
            return effects;
        }
        self.set_jump(Some(t));
        let Some(seq) = self.diff.seq else {
            return vec![];
        };
        // The line's row is at least about its number down the diff.
        let want = self.diff.requested.max(end.line as usize + DIFF_PREFETCH);
        self.diff.requested = want;
        vec![Effect::DiffDemand { seq, want }]
    }

    /// Scroll to the row of `end` when the diff can say where it is: its
    /// row has streamed in, or the stream ended without it and the thread
    /// shows at the top. `None` while it may still come.
    fn scroll_to_line(&mut self, end: LineId) -> Option<Vec<Effect>> {
        if self.diff.pending {
            return None;
        }
        if let Some(row) = anchor::row_of(&self.diff.lines, end) {
            self.diff.scroll = row.saturating_sub(ABOVE);
            self.set_jump(None);
            let rows = self.review_diff_rows();
            return Some(self.diff.demand(rows));
        }
        if self.diff.done {
            self.diff.scroll = 0;
            self.set_jump(None);
            return Some(vec![]);
        }
        None
    }

    /// After a diff chunk: scroll to the thread `]`/`[` is waiting for if
    /// its row came, or ask for more lines when the stream has given all
    /// it was asked for. Core calls it; nothing when no thread waits.
    pub(crate) fn review_jump_landed(&mut self) -> Vec<Effect> {
        if !self.review_files_shown() {
            return vec![];
        }
        let Some(t) = self.review.files.as_ref().and_then(|f| f.jump) else {
            return vec![];
        };
        let Some(Placement::Line { end, .. }) = self.thread_placement(t) else {
            self.diff.scroll = 0;
            self.set_jump(None);
            return vec![];
        };
        if let Some(effects) = self.scroll_to_line(end) {
            return effects;
        }
        match self.diff.seq {
            Some(seq) if !self.diff.pending && self.diff.lines.len() >= self.diff.requested => {
                let want = self.diff.requested + DIFF_PREFETCH;
                self.diff.requested = want;
                vec![Effect::DiffDemand { seq, want }]
            }
            _ => vec![],
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use crossterm::event::KeyCode;

    use super::*;
    use crate::features::review::state::tests::{
        detail_load, github, in_queues, press, repo, type_char,
    };
    use crate::features::review::{CommitsHere, PrFileRow, ReviewIntent};
    use crate::intent::AppIntent;
    use crate::test_support::diff_chunk;
    use kika_forge::ForgeStatus;
    use kika_forge::discussion::fixtures::DISCUSSION;
    use kika_git::commit_files::{CommitFile, CommitRange};
    use kika_git::diff::{Classifier, DiffChunk, DiffLine, EmojiZwj};

    const NOW: i64 = 1_700_000_000;

    pub(crate) fn fixture_discussion() -> PrDiscussion {
        discussion_from(DISCUSSION)
    }

    pub(crate) fn discussion_from(answer: &str) -> PrDiscussion {
        let runner = kika_forge::github::FakeGh::new().reply(kika_forge::github::answer(answer));
        let host = github()[0].clone();
        let sink = kika_git::recording::CommandSink::new();
        kika_forge::github::Gh::fixture(&runner, &sink, &host, NOW)
            .fetch_discussion(&repo(), 42)
            .unwrap()
            .unwrap()
    }

    fn discussion_load(effects: &[Effect]) -> (Tag, u64) {
        let loads: Vec<_> = effects
            .iter()
            .filter_map(|e| match e {
                Effect::LoadPrDiscussion {
                    tag,
                    number,
                    hosts,
                    repo: r,
                    ..
                } => {
                    assert_eq!((hosts, r), (&github(), &Some(repo())));
                    Some((*tag, *number))
                }
                _ => None,
            })
            .collect();
        match loads.as_slice() {
            [one] => *one,
            other => panic!("expected one discussion load, got {other:?} in {effects:?}"),
        }
    }

    fn no_discussion_load(effects: &[Effect]) {
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::LoadPrDiscussion { .. })),
            "{effects:?}"
        );
    }

    /// The queue open on pull request 12, its detail loaded, and the
    /// discussion load it asked for.
    pub(crate) fn detail_then_load() -> (AppState, Tag) {
        let mut state = in_queues();
        let (tag, number) = detail_load(&press(&mut state, KeyCode::Enter));
        let effects = state.update(AppIntent::Review(ReviewIntent::PrDetailLoaded {
            tag,
            number,
            repo: Some(repo()),
            result: Ok(Some(crate::features::review::state::tests::fixture_detail())),
        }));
        let (tag, number) = discussion_load(&effects);
        assert_eq!(number, 12);
        (state, tag)
    }

    pub(crate) fn answer(
        state: &mut AppState,
        tag: Tag,
        result: Result<Option<PrDiscussion>, ReviewError>,
    ) {
        state.update(AppIntent::Review(ReviewIntent::DiscussionLoaded {
            tag,
            number: 12,
            repo: Some(repo()),
            result,
        }));
    }

    #[test]
    fn a_loaded_detail_asks_for_its_discussion_once() {
        let (mut state, tag) = detail_then_load();
        assert!(state.review.discussion_fetch.loading);
        assert_eq!(state.review.discussion_fetch.seq, Some(tag.seq));
        // `r` waits for it.
        assert_eq!(type_char(&mut state, 'r'), []);
        assert_eq!(state.notice.as_deref(), Some("Already loading"));
        answer(&mut state, tag, Ok(Some(fixture_discussion())));
        assert!(!state.review.discussion_fetch.loading);
        assert_eq!(state.review.discussion, Some(fixture_discussion()));
        // Now `r` reads the detail again, then the discussion again.
        let (tag, number) = detail_load(&type_char(&mut state, 'r'));
        let effects = state.update(AppIntent::Review(ReviewIntent::PrDetailLoaded {
            tag,
            number,
            repo: Some(repo()),
            result: Ok(Some(crate::features::review::state::tests::fixture_detail())),
        }));
        discussion_load(&effects);
    }

    #[test]
    fn a_missing_or_failed_detail_asks_for_no_discussion() {
        for result in [
            Ok(None),
            Err(ReviewError::Status(ForgeStatus::Offline)),
            Err(ReviewError::Limited { until: NOW + 60 }),
        ] {
            let mut state = in_queues();
            let (tag, number) = detail_load(&press(&mut state, KeyCode::Enter));
            let effects = state.update(AppIntent::Review(ReviewIntent::PrDetailLoaded {
                tag,
                number,
                repo: Some(repo()),
                result,
            }));
            no_discussion_load(&effects);
            assert!(!state.review.discussion_fetch.loading);
        }
    }

    #[test]
    fn a_stale_or_other_pull_requests_discussion_is_dropped() {
        let (mut state, tag) = detail_then_load();
        for stale in [
            Tag {
                seq: tag.seq + 1,
                ..tag
            },
            Tag {
                generation: tag.generation + 1,
                ..tag
            },
        ] {
            answer(&mut state, stale, Ok(Some(fixture_discussion())));
            assert_eq!(state.review.discussion, None);
            assert!(state.review.discussion_fetch.loading);
        }
        state.update(AppIntent::Review(ReviewIntent::DiscussionLoaded {
            tag,
            number: 7,
            repo: Some(repo()),
            result: Ok(Some(fixture_discussion())),
        }));
        assert_eq!(state.review.discussion, None);
        // Back on the queue, the answer is for nothing shown.
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.review.discussion_fetch, ReviewFetch::default());
        answer(&mut state, tag, Ok(Some(fixture_discussion())));
        assert_eq!(state.review.discussion, None);
    }

    #[test]
    fn a_rate_limited_discussion_records_the_limit() {
        let (mut state, tag) = detail_then_load();
        answer(
            &mut state,
            tag,
            Err(ReviewError::Limited { until: NOW + 90 }),
        );
        assert_eq!(state.forge.limited.get("github.com"), Some(&(NOW + 90)));
        assert_eq!(
            state.review.discussion_fetch.error,
            Some(ReviewProblem::Limited(NOW + 90))
        );
        // A missing pull request says so too.
        let (mut state, tag) = detail_then_load();
        answer(&mut state, tag, Ok(None));
        assert_eq!(
            state.review.discussion_fetch.error,
            Some(ReviewProblem::Missing(12))
        );
    }

    fn file_row(path: &str) -> PrFileRow {
        PrFileRow {
            file: CommitFile {
                status: 'M',
                path: path.into(),
                orig_path: None,
            },
            counts: crate::features::review::git::FileCounts::Lines {
                added: 1,
                removed: 1,
            },
        }
    }

    fn whole() -> CommitRange {
        CommitRange {
            oid: CommitId::parse(&"1".repeat(40)).unwrap(),
            parent: Some(CommitId::parse(&"3".repeat(40)).unwrap()),
        }
    }

    /// Pull request 12's files screen, ready on `paths`, with
    /// `discussion` read.
    pub(crate) fn files_with(paths: &[&str], discussion: PrDiscussion) -> AppState {
        let (mut state, tag) = detail_then_load();
        answer(&mut state, tag, Ok(Some(discussion)));
        let effects = type_char(&mut state, 'f');
        let [
            Effect::CheckReviewCommits { tag, .. },
            Effect::LoadPendingReview { .. },
        ] = effects.as_slice()
        else {
            panic!("{effects:?}");
        };
        let tag = *tag;
        state.update(AppIntent::Review(ReviewIntent::ObjectsChecked {
            tag,
            result: Ok(CommitsHere::All),
        }));
        state.update(AppIntent::Review(ReviewIntent::FilesLoaded {
            tag,
            result: Ok((whole(), paths.iter().map(|p| file_row(p)).collect())),
        }));
        assert_eq!(
            state.review.files.as_ref().unwrap().stage,
            FilesStage::Ready
        );
        state
    }

    pub(crate) fn cache_diff() -> Vec<DiffLine> {
        let mut classifier = Classifier::new(EmojiZwj::Escape);
        [
            "diff --git a/src/cache.rs b/src/cache.rs",
            "--- a/src/cache.rs",
            "+++ b/src/cache.rs",
            "@@ -1,4 +1,4 @@",
            " use std::fmt;",
            "-fn old() {}",
            "+fn new() {}",
            " let cache = 1;",
            " fn tail() {}",
        ]
        .iter()
        .map(|t| classifier.classify(t.as_bytes()))
        .collect()
    }

    /// `n` context rows of a hunk from line 1, after a file header.
    fn long_diff(n: usize) -> Vec<DiffLine> {
        let mut classifier = Classifier::new(EmojiZwj::Escape);
        let mut texts = vec![
            "diff --git a/src/lib.rs b/src/lib.rs".to_string(),
            "--- a/src/lib.rs".to_string(),
            "+++ b/src/lib.rs".to_string(),
            format!("@@ -1,{n} +1,{n} @@"),
        ];
        texts.extend((1..=n).map(|i| format!(" line {i}")));
        texts
            .iter()
            .map(|t| classifier.classify(t.as_bytes()))
            .collect()
    }

    pub(crate) fn chunk(state: &mut AppState, lines: Vec<DiffLine>, done: bool) -> Vec<Effect> {
        let tag = Tag {
            generation: state.generation,
            seq: state.diff.seq.unwrap(),
            user: false,
        };
        state.update(diff_chunk(tag, DiffChunk { lines, done }))
    }

    fn selected(state: &AppState) -> (usize, Option<usize>) {
        let files = state.review.files.as_ref().unwrap();
        (files.cursor, files.thread)
    }

    fn streamed_path(effects: &[Effect]) -> Option<RepoPath> {
        effects.iter().find_map(|e| match e {
            Effect::LoadDiff { request, .. } => Some(request.path.clone()),
            _ => None,
        })
    }

    #[test]
    fn placing_keeps_line_threads_by_line_and_the_rest_on_top() {
        let state = files_with(&["src/cache.rs", "src/lib.rs"], fixture_discussion());
        let files = state.review.files.as_ref().unwrap();
        let scope = ThreadScope::of(files, state.review.discussion.as_ref().unwrap());
        // Old line 2 before new line 3.
        assert_eq!(
            scope.file_threads(&"src/cache.rs".into(), None),
            FileThreads {
                top: vec![],
                lines: vec![(LineId::removed(2), 1), (LineId::added(3), 0)],
            }
        );
        assert_eq!(
            scope.file_threads(&"src/lib.rs".into(), None),
            FileThreads {
                top: vec![3],
                lines: vec![(LineId::added(5), 2)],
            }
        );
        assert_eq!(
            scope.file_threads(&"docs/café.md".into(), None).top,
            vec![4]
        );
        // Once the whole diff is in, a line not in it goes to the top.
        let numbers = anchor::row_numbers(&cache_diff());
        let lib = scope.file_threads(&"src/lib.rs".into(), Some(&numbers));
        assert_eq!((lib.top, lib.lines), (vec![2, 3], vec![]));
        let cache = scope.file_threads(&"src/cache.rs".into(), Some(&numbers));
        assert_eq!(cache.lines.len(), 2);
        // A local name that is not UTF-8 matches nothing.
        #[cfg(unix)]
        {
            let odd = RepoPath::from_bytes(b"src/cache.rs\xff");
            assert_eq!(scope.file_threads(&odd, None), FileThreads::default());
        }
    }

    #[test]
    fn right_bracket_visits_unresolved_threads_across_files_and_left_goes_back() {
        let mut discussion = fixture_discussion();
        // An unresolved thread on a file not listed is never visited.
        let mut unlisted = discussion.threads[0].clone();
        unlisted.path = "not/listed.rs".into();
        discussion.threads.push(unlisted);
        let mut state = files_with(&["src/cache.rs", "src/lib.rs", "docs/café.md"], discussion);
        chunk(&mut state, cache_diff(), true);
        assert_eq!(selected(&state), (0, None));

        // The first unresolved one: cache.rs's line 3 (the resolved line 2
        // is skipped). Its row is there: three rows above it shows.
        let effects = type_char(&mut state, ']');
        assert_eq!(selected(&state), (0, Some(0)));
        assert_eq!(streamed_path(&effects), None);
        assert_eq!(state.diff.scroll, 7 - 3);

        // The next is lib.rs's outdated one: the cursor moves and that file
        // streams from its top, where the thread is.
        let effects = type_char(&mut state, ']');
        assert_eq!(selected(&state), (1, Some(3)));
        assert_eq!(streamed_path(&effects), Some("src/lib.rs".into()));
        assert_eq!(state.diff.scroll, 0);
        assert_eq!(state.review.files.as_ref().unwrap().jump, None);

        // No more: the selection stays and the notice says so.
        assert_eq!(type_char(&mut state, ']'), []);
        assert_eq!(selected(&state), (1, Some(3)));
        assert_eq!(state.notice.as_deref(), Some(NO_NEXT));

        let effects = type_char(&mut state, '[');
        assert_eq!(selected(&state), (0, Some(0)));
        assert_eq!(streamed_path(&effects), Some("src/cache.rs".into()));
        assert_eq!(type_char(&mut state, '['), []);
        assert_eq!(selected(&state), (0, Some(0)));
        assert_eq!(state.notice.as_deref(), Some(NO_PREV));
    }

    #[test]
    fn with_none_selected_the_step_starts_from_the_selected_file() {
        let mut state = files_with(&["src/cache.rs", "src/lib.rs"], fixture_discussion());
        type_char(&mut state, 'j');
        assert_eq!(selected(&state), (1, None));
        type_char(&mut state, '[');
        assert_eq!(selected(&state), (1, Some(3)));
        // Moving to another file forgets the thread.
        type_char(&mut state, 'k');
        assert_eq!(selected(&state), (0, None));
        type_char(&mut state, '[');
        assert_eq!(selected(&state), (0, Some(0)));
    }

    #[test]
    fn a_thread_whose_row_has_not_streamed_is_waited_for() {
        let mut discussion = fixture_discussion();
        // The outdated lib.rs thread, now current on new line 300.
        let t = &mut discussion.threads[3];
        t.outdated = false;
        t.line = Some(300);
        let mut state = files_with(&["src/lib.rs"], discussion.clone());
        chunk(&mut state, long_diff(50), false);
        let seq = state.diff.seq.unwrap();
        let effects = type_char(&mut state, ']');
        assert_eq!(selected(&state), (0, Some(3)));
        assert_eq!(state.review.files.as_ref().unwrap().jump, Some(3));
        assert_eq!(
            effects,
            [Effect::DiffDemand {
                seq,
                want: 300 + DIFF_PREFETCH,
            }]
        );
        assert_eq!(state.diff.scroll, 0);

        // A chunk without the row changes nothing yet; the one with it
        // scrolls there and stops waiting.
        let more: Vec<DiffLine> = long_diff(400).into_iter().skip(54).take(100).collect();
        chunk(&mut state, more, false);
        assert_eq!(state.review.files.as_ref().unwrap().jump, Some(3));
        let rest: Vec<DiffLine> = long_diff(400).into_iter().skip(154).collect();
        chunk(&mut state, rest, true);
        // Line 300 is row 303: four header rows, from line 1.
        assert_eq!(state.diff.scroll, 303 - 3);
        assert_eq!(state.review.files.as_ref().unwrap().jump, None);

        // A stream that ends without the row leaves the top, where the
        // thread then shows.
        let mut state = files_with(&["src/lib.rs"], discussion);
        chunk(&mut state, long_diff(20), false);
        type_char(&mut state, ']');
        assert_eq!(state.review.files.as_ref().unwrap().jump, Some(3));
        state.diff.scroll = 5;
        chunk(&mut state, Vec::new(), true);
        assert_eq!(state.diff.scroll, 0);
        assert_eq!(state.review.files.as_ref().unwrap().jump, None);
    }

    #[test]
    fn a_wait_asks_for_more_once_the_stream_gave_what_was_asked() {
        let mut discussion = fixture_discussion();
        let t = &mut discussion.threads[3];
        t.outdated = false;
        t.line = Some(5000);
        let mut state = files_with(&["src/lib.rs"], discussion);
        chunk(&mut state, long_diff(10), false);
        type_char(&mut state, ']');
        let asked = state.diff.requested;
        assert_eq!(asked, 5000 + DIFF_PREFETCH);
        let seq = state.diff.seq.unwrap();
        // Removed rows: the new side, where the thread is, stays behind.
        let filler: Vec<DiffLine> = (0..asked)
            .map(|_| DiffLine {
                kind: kika_git::diff::LineKind::Removed,
                text: "-x".into(),
            })
            .collect();
        let effects = chunk(&mut state, filler, false);
        assert!(state.diff.lines.len() >= asked);
        assert_eq!(
            effects,
            [Effect::DiffDemand {
                seq,
                want: asked + DIFF_PREFETCH,
            }]
        );
        // Off the files screen nothing waits.
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.review_jump_landed(), []);
    }

    #[test]
    fn the_keys_say_why_they_cannot_step() {
        // Not on the files screen.
        let (mut state, _) = detail_then_load();
        assert_eq!(thread_nav_disabled(&state).as_deref(), Some(NOT_READY));
        // Files listing, threads still loading.
        type_char(&mut state, 'f');
        assert_eq!(thread_nav_disabled(&state).as_deref(), Some(NOT_READY));
        let mut ready = files_with(&["src/cache.rs"], fixture_discussion());
        ready.review.discussion_fetch.loading = true;
        assert_eq!(type_char(&mut ready, ']'), []);
        assert_eq!(ready.notice.as_deref(), Some(THREADS_LOADING));
        ready.review.discussion_fetch.loading = false;
        ready.review.discussion_fetch.error = Some(ReviewProblem::Status(ForgeStatus::Offline));
        assert_eq!(type_char(&mut ready, '['), []);
        assert_eq!(ready.notice.as_deref(), Some(THREADS_FAILED));
        // Only resolved threads, or none on a listed file.
        let only_docs = files_with(&["docs/café.md"], fixture_discussion());
        assert_eq!(thread_nav_disabled(&only_docs).as_deref(), Some(NO_THREADS));
        let mut none = files_with(&["src/cache.rs"], fixture_discussion());
        none.review.discussion = None;
        assert_eq!(thread_nav_disabled(&none).as_deref(), Some(NOT_LOADED));
    }

    #[test]
    fn a_commit_view_steps_to_the_top_block_and_a_pick_forgets_the_thread() {
        let mut state = files_with(&["src/cache.rs", "src/lib.rs"], fixture_discussion());
        chunk(&mut state, cache_diff(), true);
        type_char(&mut state, ']');
        assert_eq!(selected(&state), (0, Some(0)));
        let files = state.review.files.as_mut().unwrap();
        files.view = ReviewView::Commit(0);
        let scope = ThreadScope::of(files, state.review.discussion.as_ref().unwrap());
        assert_eq!(scope.placement(0), Some(Placement::OtherView));
        let cache = scope.file_threads(&"src/cache.rs".into(), None);
        assert_eq!((cache.top, cache.lines), (vec![0, 1], vec![]));
        state.review.files.as_mut().unwrap().thread = None;
        state.diff.scroll = 4;
        type_char(&mut state, ']');
        assert_eq!(selected(&state), (0, Some(0)));
        assert_eq!(state.diff.scroll, 0);
    }
}
