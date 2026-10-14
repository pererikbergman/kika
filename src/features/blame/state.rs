use crate::diff_search::{DiffSearch, reveal};
use crate::intent::Scroll;
use kika_git::blame::BlameLine;
use kika_git::diff::DiffLine;
use kika_git::path::RepoPath;

/// A blame of one file, shown in the Diff pane's place: each line with the
/// commit that last changed it, and a cursor to pick one. Pure state; the
/// load runs as an effect. A snapshot: it does not follow later edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameView {
    /// The blame load in flight, if any; only its result is taken.
    pub seq: Option<u64>,
    pub path: RepoPath,
    pub lines: Vec<BlameLine>,
    /// The load has landed, so empty `lines` means an empty file.
    pub loaded: bool,
    pub error: Option<String>,
    /// Index into `lines` of the selected line.
    pub cursor: usize,
    /// Index of the top visible line.
    pub scroll: usize,
    /// The commit opened from the cursor's line, drawn over the blame.
    pub commit: Option<CommitView>,
    /// The text searched for in the lines' text, never their commit,
    /// author or date. It stays while the commit view is open.
    pub search: Option<DiffSearch>,
}

/// One commit's message and full diff, opened from a blame line. Read-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitView {
    /// The commit load in flight, if any; only its result is taken.
    pub seq: Option<u64>,
    pub oid: String,
    pub lines: Vec<DiffLine>,
    pub loaded: bool,
    pub error: Option<String>,
    /// Index of the top visible line.
    pub scroll: usize,
    /// The text searched for in the commit's lines; the blame's own
    /// search is apart from it.
    pub search: Option<DiffSearch>,
}

impl BlameView {
    /// A blame of `path` whose load is request `seq`.
    pub fn new(path: RepoPath, seq: u64) -> Self {
        Self {
            seq: Some(seq),
            path,
            lines: Vec::new(),
            loaded: false,
            error: None,
            cursor: 0,
            scroll: 0,
            commit: None,
            search: None,
        }
    }

    /// Take the load's result. The cursor starts on the first line.
    pub fn loaded(&mut self, result: Result<Vec<BlameLine>, String>) {
        self.seq = None;
        self.loaded = true;
        self.cursor = 0;
        self.scroll = 0;
        self.search = None;
        match result {
            Ok(lines) => {
                self.lines = lines;
                self.error = None;
            }
            Err(message) => {
                self.lines.clear();
                self.error = Some(message);
            }
        }
    }

    /// The line under the cursor, once loaded.
    pub fn selected(&self) -> Option<&BlameLine> {
        self.lines.get(self.cursor)
    }

    /// Move the cursor `delta` lines, clamped to the file, keeping it
    /// inside a pane of `rows`.
    pub fn move_by(&mut self, delta: isize, rows: usize) {
        let last = self.lines.len().saturating_sub(1);
        self.cursor = self.cursor.saturating_add_signed(delta).min(last);
        self.scroll_to_cursor(rows);
    }

    /// Move the cursor a pane of `rows` down or up.
    pub fn page(&mut self, down: bool, rows: usize) {
        let rows = rows.max(1) as isize;
        self.move_by(if down { rows } else { -rows }, rows as usize);
    }

    /// Scroll just enough that the cursor is on screen.
    fn scroll_to_cursor(&mut self, rows: usize) {
        self.scroll = reveal(self.cursor, self.scroll, rows);
    }

    /// Search the lines' text for `query`, from the top line shown; an
    /// empty query ends the search.
    pub fn set_search(&mut self, query: &str) {
        let texts = self.lines.iter().map(|l| l.text.as_str());
        self.search = (!query.is_empty()).then(|| DiffSearch::new_in(query, texts, self.scroll));
    }

    /// Put the cursor on the current match's line, so Enter opens its
    /// commit, and scroll to it in a pane of `rows`.
    pub fn jump_to_match(&mut self, rows: usize) {
        if let Some(found) = self.search.as_ref().and_then(|s| s.current_match()) {
            self.cursor = found.row;
            self.scroll_to_cursor(rows);
        }
    }

    /// Step the search forward, or back with `back`, and go there.
    /// Returns whether it wrapped round the end.
    pub fn step_search(&mut self, back: bool, rows: usize) -> bool {
        let Some(search) = &mut self.search else {
            return false;
        };
        let wrapped = if back { search.prev() } else { search.next() };
        self.jump_to_match(rows);
        wrapped
    }
}

impl CommitView {
    /// Commit `oid` whose load is request `seq`.
    pub fn new(oid: String, seq: u64) -> Self {
        Self {
            seq: Some(seq),
            oid,
            lines: Vec::new(),
            loaded: false,
            error: None,
            scroll: 0,
            search: None,
        }
    }

    /// Take the load's result; a failure is shown in the pane.
    pub fn loaded(&mut self, result: Result<Vec<DiffLine>, String>) {
        self.seq = None;
        self.loaded = true;
        match result {
            Ok(lines) => self.lines = lines,
            Err(message) => self.error = Some(message),
        }
    }

    /// Move by a line or a page of `rows`, never past the last line.
    pub fn scroll(&mut self, by: Scroll, rows: usize) {
        let scroll = match by {
            Scroll::LineDown => self.scroll.saturating_add(1),
            Scroll::LineUp => self.scroll.saturating_sub(1),
            Scroll::PageDown => self.scroll.saturating_add(rows),
            Scroll::PageUp => self.scroll.saturating_sub(rows),
        };
        self.scroll = scroll.min(self.max_scroll(rows));
    }

    /// Search the commit's lines for `query`, from the top line shown; an
    /// empty query ends the search.
    pub fn set_search(&mut self, query: &str) {
        let texts = self.lines.iter().map(|l| l.text.as_str());
        self.search = (!query.is_empty()).then(|| DiffSearch::new_in(query, texts, self.scroll));
    }

    /// Scroll just enough to show the current match in a pane of `rows`,
    /// never past the last line, as [`Self::scroll`] stops.
    pub fn jump_to_match(&mut self, rows: usize) {
        if let Some(found) = self.search.as_ref().and_then(|s| s.current_match()) {
            self.scroll = reveal(found.row, self.scroll, rows).min(self.max_scroll(rows));
        }
    }

    /// Step the search forward, or back with `back`, and go there.
    /// Returns whether it wrapped round the end.
    pub fn step_search(&mut self, back: bool, rows: usize) -> bool {
        let Some(search) = &mut self.search else {
            return false;
        };
        let wrapped = if back { search.prev() } else { search.next() };
        self.jump_to_match(rows);
        wrapped
    }

    fn max_scroll(&self, rows: usize) -> usize {
        self.lines.len().saturating_sub(rows.max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kika_git::diff::LineKind;

    fn blame_line(text: &str) -> BlameLine {
        BlameLine {
            oid: "a".repeat(40),
            author: "foo".into(),
            date: "2024-01-02".into(),
            line_no: 1,
            text: text.into(),
        }
    }

    #[test]
    fn a_blame_search_reads_only_the_text_and_moves_the_cursor_to_a_match() {
        let mut view = BlameView::new(RepoPath::from("a.rs"), 1);
        let texts = ["x", "foo", "x", "x", "foo"];
        view.loaded(Ok(texts.iter().map(|t| blame_line(t)).collect()));
        view.set_search("foo");
        let rows: Vec<usize> = view
            .search
            .as_ref()
            .unwrap()
            .matches
            .iter()
            .map(|m| m.row)
            .collect();
        assert_eq!(rows, [1, 4], "the author is never searched");
        view.jump_to_match(2);
        assert_eq!((view.cursor, view.scroll), (1, 0));
        assert!(!view.step_search(false, 2));
        assert_eq!((view.cursor, view.scroll), (4, 3));
        assert!(view.step_search(false, 2), "wrapped");
        assert_eq!(view.cursor, 1);
        view.loaded(Ok(vec![blame_line("foo")]));
        assert_eq!(view.search, None, "a new load drops the search");
    }

    #[test]
    fn a_commit_search_scrolls_no_further_than_scrolling_does() {
        let mut view = CommitView::new("a".repeat(40), 1);
        let mut lines: Vec<DiffLine> = (0..10)
            .map(|_| DiffLine {
                kind: LineKind::Context,
                text: "x".into(),
            })
            .collect();
        lines[9].text = "foo".into();
        view.loaded(Ok(lines));
        view.set_search("foo");
        view.jump_to_match(4);
        assert_eq!(view.scroll, 6, "the last line on the bottom row");
        view.scroll = 9;
        view.jump_to_match(4);
        assert_eq!(view.scroll, 6, "clamped as a scroll is");
    }
}
