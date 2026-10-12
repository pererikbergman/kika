use crate::diff_search::{DiffSearch, reveal};
use crate::effect::Effect;
use crate::git::diff::{DiffChunk, DiffLine};
use crate::git::path::RepoPath;
use crate::intent::Scroll;

/// Lines read beyond the bottom of the pane, so scrolling rarely waits on git.
pub const DIFF_PREFETCH: usize = 200;

/// What the Diff pane shows: the lines streamed so far for the selected file,
/// plus where the user has scrolled to. Pure state; the streaming lives in
/// `diff::stream`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
}

impl DiffPane {
    /// Start showing request `seq` for `path`. The old lines stay on screen
    /// until the first chunk arrives, so switching files does not flicker.
    /// Returns how many lines to ask for.
    pub fn begin(&mut self, seq: u64, path: &RepoPath, rows: usize) -> usize {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::diff::LineKind;

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
        pane.begin(1, &RepoPath::from("a.rs"), 10);
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
        pane.begin(2, &RepoPath::from("a.rs"), 10);
        assert!(pane.search.is_some(), "a reload of the same path");
        // The reload's lines replace the old ones, and are searched again.
        pane.apply(2, chunk(&["x", "foo", "foo"], true));
        assert_eq!(rows_of(&pane), [1, 2]);

        pane.begin(3, &RepoPath::from("b.rs"), 10);
        assert_eq!(pane.search, None);
    }

    #[test]
    fn a_jump_scrolls_to_the_match_and_asks_for_more_lines_when_short() {
        let mut texts = vec!["x"; 400];
        texts[350] = "foo";
        let mut pane = DiffPane::default();
        pane.begin(1, &RepoPath::from("a.rs"), 10);
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
