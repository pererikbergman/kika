use crate::git::blame::BlameLine;
use crate::git::diff::DiffLine;
use crate::git::path::RepoPath;
use crate::intent::Scroll;

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
        }
    }

    /// Take the load's result. The cursor starts on the first line.
    pub fn loaded(&mut self, result: Result<Vec<BlameLine>, String>) {
        self.seq = None;
        self.loaded = true;
        self.cursor = 0;
        self.scroll = 0;
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
        let rows = rows.max(1);
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + rows {
            self.scroll = self.cursor + 1 - rows;
        }
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
        self.scroll = scroll.min(self.lines.len().saturating_sub(rows.max(1)));
    }
}
