use crate::effect::Effect;
use crate::git::diff::{DiffChunk, DiffLine};
use crate::intent::Scroll;

/// Lines read beyond the bottom of the pane, so scrolling rarely waits on git.
pub const DIFF_PREFETCH: usize = 200;

/// What the Diff pane shows: the lines streamed so far for the selected file,
/// plus where the user has scrolled to. Pure state; the streaming lives in
/// `diff_stream`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffPane {
    /// The request whose chunks are accepted; older ones are dropped.
    pub seq: Option<u64>,
    pub path: Option<String>,
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
}

impl DiffPane {
    /// Start showing request `seq` for `path`. The old lines stay on screen
    /// until the first chunk arrives, so switching files does not flicker.
    /// Returns how many lines to ask for.
    pub fn begin(&mut self, seq: u64, path: &str, rows: usize) -> usize {
        if self.path.as_deref() != Some(path) {
            self.path = Some(path.to_string());
            self.scroll = 0;
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
    pub fn apply(&mut self, seq: u64, result: Result<DiffChunk, String>) {
        if self.seq != Some(seq) {
            return;
        }
        if self.pending {
            self.lines.clear();
        }
        self.pending = false;
        self.show_loading = false;
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
        if self.done {
            self.scroll = self.scroll.min(self.max_scroll());
        }
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
