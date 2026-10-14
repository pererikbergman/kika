use crate::diff_search::{DiffSearch, reveal};
use crate::effect::{Effect, Tag};
use crate::hunk_view::HunkView;
use crate::intent::Scroll;
use crate::model::{AppState, Focus};
use kika_git::diff::{DiffChunk, DiffLine, DiffRequest};
use kika_git::path::RepoPath;

/// Lines read beyond the bottom of the pane, so scrolling rarely waits on git.
pub const DIFF_PREFETCH: usize = 200;

/// What the Diff pane shows: the lines streamed so far for the selected file,
/// plus where the user has scrolled to. Pure state; the streaming lives in
/// `diff::stream`. Equality is the stream's: the hunk cursor is focus
/// state, so two panes showing the same lines are equal whatever it holds.
#[derive(Debug, Clone, Default, Eq)]
pub struct DiffPane {
    /// The request whose chunks are accepted; older ones are dropped.
    pub seq: Option<u64>,
    pub path: Option<RepoPath>,
    pub lines: Vec<DiffLine>,
    /// The stream has ended, so there is nothing more to demand.
    pub done: bool,
    /// No chunk has arrived for `seq` yet; the next one replaces `lines`.
    pub pending: bool,
    /// The load is slow enough that the pane says "Loading…".
    pub show_loading: bool,
    pub error: Option<String>,
    /// Index of the top visible line.
    pub scroll: usize,
    /// Lines asked of the stream so far.
    pub requested: usize,
    /// The text searched for and where it appears in the lines loaded so
    /// far. Kept across a reload of the same file, dropped for another.
    pub search: Option<DiffSearch>,
    /// The hunk cursor while the pane has the keys. Read it through
    /// `AppState::hunk_view`, which hides one left behind by a focus move.
    pub hunks: Option<HunkView>,
}

impl PartialEq for DiffPane {
    fn eq(&self, other: &Self) -> bool {
        // Spelled out, so a new field has to choose a side.
        let Self {
            seq,
            path,
            lines,
            done,
            pending,
            show_loading,
            error,
            scroll,
            requested,
            search,
            hunks: _,
        } = self;
        (
            seq,
            path,
            lines,
            done,
            pending,
            show_loading,
            error,
            scroll,
            requested,
            search,
        ) == (
            &other.seq,
            &other.path,
            &other.lines,
            &other.done,
            &other.pending,
            &other.show_loading,
            &other.error,
            &other.scroll,
            &other.requested,
            &other.search,
        )
    }
}

/// Where an opened diff starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffStart {
    /// A reopen of the same path keeps its scroll and search, as a reload
    /// with new options or a moved cursor should.
    Keep,
    /// Line 0 with no search, even for the same path: another commit's,
    /// stash's or pull request's file of the same name is new content.
    Top,
}

impl DiffPane {
    /// Start showing request `seq` for `path`. The old lines stay on screen
    /// until the first chunk arrives, so switching files does not flicker.
    /// Returns how many lines to ask for.
    pub fn open(&mut self, seq: u64, path: &RepoPath, rows: usize, start: DiffStart) -> usize {
        self.rewind(start);
        if self.path.as_ref() != Some(path) {
            self.path = Some(path.clone());
            self.scroll = 0;
            self.search = None;
        }
        self.seq = Some(seq);
        self.done = false;
        self.pending = true;
        self.show_loading = false;
        self.error = None;
        self.requested = self.wanted(rows);
        self.requested
    }

    /// With `Top`, forget the path so the next `open` starts at the top.
    /// Only `open` and a selection that loads nothing yet use it.
    pub(crate) fn rewind(&mut self, start: DiffStart) {
        if start == DiffStart::Top {
            self.path = None;
        }
    }

    /// Nothing to show: no lines, no request, no search. The hunk cursor
    /// stays: its life follows focus, not the stream.
    pub fn clear(&mut self) {
        let hunks = self.hunks.take();
        *self = Self {
            hunks,
            ..Self::default()
        };
    }

    /// Take a chunk of request `seq`; chunks of any other request are dropped.
    /// A search follows: a first chunk is searched afresh, a later one
    /// adds its matches.
    pub fn apply(&mut self, seq: u64, result: Result<DiffChunk, String>) {
        if self.seq != Some(seq) {
            return;
        }
        let replaced = self.pending;
        if replaced {
            self.lines.clear();
        }
        self.pending = false;
        self.show_loading = false;
        let first = self.lines.len();
        match result {
            Ok(chunk) => {
                self.lines.extend(chunk.lines);
                self.done = chunk.done;
            }
            Err(message) => {
                self.error = Some(message);
                self.done = true;
            }
        }
        if let Some(search) = &mut self.search {
            let texts = self.lines.iter().map(|l| l.text.as_str());
            if replaced {
                search.refresh_in(texts);
            } else {
                search.extend_in(texts.skip(first), first);
            }
        }
        if self.done {
            self.scroll = self.scroll.min(self.max_scroll());
        }
    }

    /// Search the lines loaded so far for `query`, from the top line
    /// shown; an empty query ends the search.
    pub fn set_search(&mut self, query: &str) {
        let texts = self.lines.iter().map(|l| l.text.as_str());
        self.search = (!query.is_empty()).then(|| DiffSearch::new_in(query, texts, self.scroll));
    }

    /// Scroll just enough to show the current match in a pane of `rows`,
    /// asking the stream for more if the prefetch window ran short.
    pub fn jump_to_match(&mut self, rows: usize) -> Vec<Effect> {
        let Some(found) = self.search.as_ref().and_then(|s| s.current_match()) else {
            return vec![];
        };
        self.scroll = reveal(found.row, self.scroll, rows);
        self.demand(rows)
    }

    /// Step the search forward, or back with `back`, and go there.
    /// Returns whether it wrapped round the end, and what to ask of the
    /// stream.
    pub fn step_search(&mut self, back: bool, rows: usize) -> (bool, Vec<Effect>) {
        let Some(search) = &mut self.search else {
            return (false, vec![]);
        };
        let wrapped = if back { search.prev() } else { search.next() };
        (wrapped, self.jump_to_match(rows))
    }

    /// Request `seq` is slow: show "Loading…" unless data already came.
    pub fn slow(&mut self, seq: u64) {
        if self.seq == Some(seq) && self.pending {
            self.show_loading = true;
        }
    }

    /// Move by a line or a page of `rows`, clamped to the lines loaded so far.
    pub fn scroll(&mut self, by: Scroll, rows: usize) -> Vec<Effect> {
        let scroll = match by {
            Scroll::LineDown => self.scroll.saturating_add(1),
            Scroll::LineUp => self.scroll.saturating_sub(1),
            Scroll::PageDown => self.scroll.saturating_add(rows),
            Scroll::PageUp => self.scroll.saturating_sub(rows),
        };
        self.scroll = scroll.min(self.max_scroll());
        self.demand(rows)
    }

    /// Ask the stream for more once the prefetch window would run short.
    pub fn demand(&mut self, rows: usize) -> Vec<Effect> {
        let Some(seq) = self.seq else { return vec![] };
        let want = self.wanted(rows);
        if self.done || want <= self.requested {
            return vec![];
        }
        self.requested = want;
        vec![Effect::DiffDemand { seq, want }]
    }

    fn wanted(&self, rows: usize) -> usize {
        self.scroll + rows + DIFF_PREFETCH
    }

    fn max_scroll(&self) -> usize {
        self.lines.len().saturating_sub(1)
    }
}

/// The hunk cursor in `hunks` while `focus` is the Diff pane. It borrows
/// only those two fields, so a caller holding it may still write others.
pub(crate) fn focused_hunks<'a>(
    focus: &Focus,
    hunks: &'a Option<HunkView>,
) -> Option<&'a HunkView> {
    hunks.as_ref().filter(|_| *focus == Focus::Diff)
}

/// [`focused_hunks`], to change.
pub(crate) fn focused_hunks_mut<'a>(
    focus: &Focus,
    hunks: &'a mut Option<HunkView>,
) -> Option<&'a mut HunkView> {
    hunks.as_mut().filter(|_| *focus == Focus::Diff)
}

/// The hunk cursor: only seen while the Diff pane has the keys, so one
/// left behind after focus moved away never acts.
impl AppState {
    /// The hunk cursor, while the Diff pane has the keys.
    pub(crate) fn hunk_view(&self) -> Option<&HunkView> {
        focused_hunks(&self.focus, &self.diff.hunks)
    }

    /// [`Self::hunk_view`], to change.
    pub(crate) fn hunk_view_mut(&mut self) -> Option<&mut HunkView> {
        focused_hunks_mut(&self.focus, &mut self.diff.hunks)
    }

    /// Give the Diff pane the keys with `view` as its cursor: the only way
    /// into `Focus::Diff`.
    pub(crate) fn focus_hunk_view(&mut self, view: HunkView) {
        self.diff.hunks = Some(view);
        self.focus = Focus::Diff;
    }
}

/// The request side of the Diff pane: every diff shown goes through
/// `open_diff`, and every pane emptied through `close_diff`.
impl AppState {
    /// Stream `request` into the Diff pane from `start`.
    pub(crate) fn open_diff(&mut self, request: DiffRequest, start: DiffStart) -> Vec<Effect> {
        let seq = self.take_seq();
        let want = self.diff.open(seq, &request.path, self.diff_rows(), start);
        // Diff streams report many times, so they stay out of the busy count.
        let tag = Tag {
            generation: self.generation,
            seq,
            user: false,
        };
        vec![
            Effect::LoadDiff { tag, request, want },
            Effect::DiffLoadingDelay { tag },
        ]
    }

    /// Stop the stream and empty the pane.
    pub(crate) fn close_diff(&mut self) -> Vec<Effect> {
        self.diff.clear();
        vec![Effect::CancelDiff]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Context;
    use crate::test_support::changed;
    use kika_git::diff::{DiffOptions, DiffSide, LineKind};

    fn chunk(texts: &[&str], done: bool) -> Result<DiffChunk, String> {
        let lines = texts
            .iter()
            .map(|t| DiffLine {
                kind: LineKind::Context,
                text: t.to_string(),
            })
            .collect();
        Ok(DiffChunk { lines, done })
    }

    /// A pane of `a.rs` with request 1's first chunk in.
    fn loaded(texts: &[&str]) -> DiffPane {
        let mut pane = DiffPane::default();
        pane.open(1, &RepoPath::from("a.rs"), 10, DiffStart::Keep);
        pane.apply(1, chunk(texts, false));
        pane
    }

    fn rows_of(pane: &DiffPane) -> Vec<usize> {
        let search = pane.search.as_ref().expect("a search");
        search.matches.iter().map(|m| m.row).collect()
    }

    #[test]
    fn a_new_chunk_adds_its_matches_and_keeps_the_current_one() {
        let mut pane = loaded(&["foo", "bar", "foo"]);
        pane.set_search("foo");
        pane.search.as_mut().unwrap().next();
        pane.apply(1, chunk(&["x", "foo"], true));
        assert_eq!(rows_of(&pane), [0, 2, 4]);
        assert_eq!(pane.search.as_ref().unwrap().current, Some(1));
        pane.set_search("");
        assert_eq!(pane.search, None, "an empty query ends the search");
    }

    #[test]
    fn a_new_path_drops_the_search_and_a_reload_of_the_same_path_keeps_it() {
        let mut pane = loaded(&["foo"]);
        pane.set_search("foo");
        pane.open(2, &RepoPath::from("a.rs"), 10, DiffStart::Keep);
        assert!(pane.search.is_some(), "a reload of the same path");
        // The reload's lines replace the old ones, and are searched again.
        pane.apply(2, chunk(&["x", "foo", "foo"], true));
        assert_eq!(rows_of(&pane), [1, 2]);

        pane.open(3, &RepoPath::from("b.rs"), 10, DiffStart::Keep);
        assert_eq!(pane.search, None);
    }

    /// A pane of `a.rs` scrolled to line 5 and searching, request 1 in.
    fn scrolled_and_searching() -> DiffPane {
        let mut texts = vec!["x"; 20];
        texts[12] = "foo";
        let mut pane = loaded(&texts);
        pane.scroll = 5;
        pane.set_search("foo");
        pane
    }

    #[test]
    fn opening_the_same_path_with_keep_keeps_the_scroll_and_the_search() {
        let mut pane = scrolled_and_searching();
        pane.open(2, &RepoPath::from("a.rs"), 10, DiffStart::Keep);
        assert_eq!(pane.scroll, 5);
        assert!(pane.search.is_some());
    }

    #[test]
    fn opening_the_same_path_from_the_top_drops_the_scroll_and_the_search() {
        let mut pane = scrolled_and_searching();
        pane.open(2, &RepoPath::from("a.rs"), 10, DiffStart::Top);
        assert_eq!(pane.scroll, 0);
        assert_eq!(pane.search, None);
        assert_eq!(pane.path, Some(RepoPath::from("a.rs")));
    }

    #[test]
    fn opening_another_path_starts_at_the_top_either_way() {
        for start in [DiffStart::Keep, DiffStart::Top] {
            let mut pane = scrolled_and_searching();
            pane.open(2, &RepoPath::from("b.rs"), 10, start);
            assert_eq!(pane.scroll, 0, "{start:?}");
            assert_eq!(pane.search, None, "{start:?}");
            assert_eq!(pane.path, Some(RepoPath::from("b.rs")), "{start:?}");
        }
    }

    #[test]
    fn opening_keeps_the_old_lines_until_the_first_chunk_and_asks_past_the_rows() {
        for start in [DiffStart::Keep, DiffStart::Top] {
            let mut pane = scrolled_and_searching();
            let old = pane.lines.clone();
            let want = pane.open(2, &RepoPath::from("a.rs"), 10, start);
            let scroll = pane.scroll;
            assert_eq!(want, scroll + 10 + DIFF_PREFETCH, "{start:?}");
            assert_eq!(pane.requested, want, "{start:?}");
            assert_eq!(pane.seq, Some(2), "{start:?}");
            assert!(pane.pending, "{start:?}");
            assert_eq!(pane.lines, old, "{start:?}: the old lines stay on screen");
            pane.apply(2, chunk(&["new"], true));
            assert_eq!(
                pane.lines.len(),
                1,
                "{start:?}: the first chunk replaces them"
            );
        }
    }

    #[test]
    fn clearing_leaves_an_empty_pane() {
        let mut pane = scrolled_and_searching();
        pane.clear();
        assert_eq!(pane, DiffPane::default());
    }

    #[test]
    fn clearing_keeps_the_hunk_cursor_and_empties_the_rest() {
        let mut pane = scrolled_and_searching();
        let view = HunkView::new("a.rs".into(), DiffSide::Worktree);
        pane.hunks = Some(view.clone());
        pane.clear();
        assert_eq!(pane.hunks, Some(view));
        assert_eq!(pane, DiffPane::default());
    }

    #[test]
    fn panes_are_equal_whatever_their_hunk_cursor() {
        let mut pane = scrolled_and_searching();
        let before = pane.clone();
        pane.hunks = Some(HunkView::new("a.rs".into(), DiffSide::Worktree));
        assert_eq!(pane, before);
        pane.scroll += 1;
        assert_ne!(pane, before, "a stream field still counts");
    }

    #[test]
    fn focus_hunk_view_gives_the_pane_the_keys_and_the_cursor() {
        let mut state = AppState::new(Context::Workspace);
        let view = HunkView::new("a.rs".into(), DiffSide::Index);
        state.focus_hunk_view(view.clone());
        assert_eq!(state.focus, Focus::Diff);
        assert_eq!(state.hunk_view(), Some(&view));
        assert_eq!(state.hunk_view_mut(), Some(&mut view.clone()));
    }

    #[test]
    fn a_cursor_left_behind_by_a_focus_move_is_never_seen() {
        let mut state = AppState::new(Context::Workspace);
        state.focus_hunk_view(HunkView::new("a.rs".into(), DiffSide::Worktree));
        state.focus = Focus::Files;
        assert!(state.diff.hunks.is_some(), "the cursor itself stays");
        assert_eq!(state.hunk_view(), None);
        assert_eq!(state.hunk_view_mut(), None);
    }

    #[test]
    fn open_diff_streams_the_request_under_a_fresh_background_tag() {
        let mut state = AppState::new(Context::Workspace);
        state.generation = 3;
        let seq = state.next_seq;
        let request = DiffRequest::for_entry(&changed("a.rs", '.', 'M'), DiffOptions::default());
        let effects = state.open_diff(request.clone(), DiffStart::Top);
        let tag = Tag {
            generation: 3,
            seq,
            user: false,
        };
        assert_eq!(
            effects,
            [
                Effect::LoadDiff {
                    tag,
                    request,
                    want: state.diff.requested,
                },
                Effect::DiffLoadingDelay { tag },
            ]
        );
        assert_eq!(state.diff.seq, Some(seq));
        assert_eq!(state.next_seq, seq + 1, "the seq is taken");
    }

    #[test]
    fn close_diff_cancels_the_stream_and_empties_the_pane() {
        let mut state = AppState::new(Context::Workspace);
        let request = DiffRequest::for_entry(&changed("a.rs", '.', 'M'), DiffOptions::default());
        state.open_diff(request, DiffStart::Keep);
        assert_eq!(state.close_diff(), [Effect::CancelDiff]);
        assert_eq!(state.diff, DiffPane::default());
    }

    #[test]
    fn a_jump_scrolls_to_the_match_and_asks_for_more_lines_when_short() {
        let mut texts = vec!["x"; 400];
        texts[350] = "foo";
        let mut pane = DiffPane::default();
        pane.open(1, &RepoPath::from("a.rs"), 10, DiffStart::Keep);
        assert_eq!(pane.requested, 10 + DIFF_PREFETCH);
        pane.apply(1, chunk(&texts, false));
        pane.set_search("foo");
        let effects = pane.jump_to_match(10);
        assert_eq!(pane.scroll, 341, "the match on the bottom row");
        assert_eq!(
            effects,
            [Effect::DiffDemand {
                seq: 1,
                want: 341 + 10 + DIFF_PREFETCH
            }]
        );
        // The match in view already: no move, nothing more to ask for.
        assert_eq!(pane.jump_to_match(10), vec![]);
        assert_eq!(pane.scroll, 341);
    }
}
