use crate::git::diff::DiffSide;
use crate::git::patch::{self, FilePatch, PatchLineKind};
use crate::git::path::RepoPath;
use crate::intent::Scroll;

/// The Diff pane while it has focus: one side of the selected file's
/// whole diff, parsed into hunks, with a cursor on one of them. Pure state;
/// the load runs as an effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HunkView {
    /// The patch load in flight, if any; only its result is taken. The
    /// last patch stays on screen meanwhile, so a reload does not flicker.
    pub seq: Option<u64>,
    pub path: RepoPath,
    pub side: DiffSide,
    /// `None` until the first load lands, or after one fails.
    pub patch: Option<FilePatch>,
    /// Index of the selected hunk.
    pub hunk: usize,
    /// Index of the top visible row.
    pub scroll: usize,
}

/// What the pane says, and a key refusal says, when a side has no hunks.
pub const NO_CHANGES: &str = "No changes in this view";

/// What a drawn row is, which decides its style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Header,
    /// A hunk's `@@` line.
    HunkHeader,
    Context,
    Added,
    Removed,
    /// Git's "no newline at end of file" marker.
    NoNewline,
}

/// One display line, raw: the view sanitizes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub kind: RowKind,
    /// The hunk the row belongs to; `None` for header rows.
    pub hunk: Option<usize>,
    pub text: Vec<u8>,
}

impl HunkView {
    pub fn new(path: RepoPath, side: DiffSide) -> Self {
        Self {
            seq: None,
            path,
            side,
            patch: None,
            hunk: 0,
            scroll: 0,
        }
    }

    /// A load is in flight, so the hunks on screen may be about to change.
    pub fn loading(&self) -> bool {
        self.seq.is_some()
    }

    pub fn hunk_count(&self) -> usize {
        self.patch.as_ref().map_or(0, |p| p.hunks.len())
    }

    /// Move the cursor down a hunk, stopping at the last.
    pub fn next(&mut self, rows: usize) {
        self.hunk = (self.hunk + 1).min(self.hunk_count().saturating_sub(1));
        self.scroll_to_hunk(rows);
    }

    /// Move the cursor up a hunk, stopping at the first.
    pub fn prev(&mut self, rows: usize) {
        self.hunk = self.hunk.saturating_sub(1);
        self.scroll_to_hunk(rows);
    }

    /// Take a freshly loaded patch. The cursor keeps its index, clamped, so
    /// after staging a hunk the next one is under it.
    pub fn loaded(&mut self, patch: FilePatch, rows: usize) {
        self.patch = Some(patch);
        self.hunk = self.hunk.min(self.hunk_count().saturating_sub(1));
        self.scroll_to_hunk(rows);
    }

    /// Scroll by a line or a page of `rows`, clamped to the rows there are.
    pub fn scroll(&mut self, by: Scroll, rows: usize) {
        let scroll = match by {
            Scroll::LineDown => self.scroll.saturating_add(1),
            Scroll::LineUp => self.scroll.saturating_sub(1),
            Scroll::PageDown => self.scroll.saturating_add(rows),
            Scroll::PageUp => self.scroll.saturating_sub(rows),
        };
        self.scroll = scroll.min(self.rows().len().saturating_sub(1));
    }

    /// Keep the selected hunk's `@@` row inside a pane of `rows`. When it
    /// is below, it goes to the top, so as much of the hunk as fits shows.
    pub fn scroll_to_hunk(&mut self, rows: usize) {
        let Some(at) = self
            .rows()
            .iter()
            .position(|r| r.kind == RowKind::HunkHeader && r.hunk == Some(self.hunk))
        else {
            self.scroll = 0;
            return;
        };
        if at < self.scroll || at >= self.scroll + rows.max(1) {
            self.scroll = at;
        }
    }

    /// Every display line in order: the header, then each hunk's `@@` line
    /// and its lines. Line endings are left out.
    pub fn rows(&self) -> Vec<Row> {
        let Some(patch) = &self.patch else {
            return Vec::new();
        };
        let trim = |text: &[u8]| {
            let text = text.strip_suffix(b"\n").unwrap_or(text);
            text.strip_suffix(b"\r").unwrap_or(text).to_vec()
        };
        let mut rows: Vec<Row> = patch
            .header
            .iter()
            .map(|line| Row {
                kind: RowKind::Header,
                hunk: None,
                text: trim(line),
            })
            .collect();
        for (index, hunk) in patch.hunks.iter().enumerate() {
            let mut text = format!(
                "@@ -{} +{} @@",
                patch::range(hunk.old_start.into(), hunk.old_len),
                patch::range(hunk.new_start.into(), hunk.new_len)
            )
            .into_bytes();
            text.extend_from_slice(&trim(&hunk.heading));
            rows.push(Row {
                kind: RowKind::HunkHeader,
                hunk: Some(index),
                text,
            });
            for line in &hunk.lines {
                let (kind, prefix) = match line.kind {
                    PatchLineKind::Context => (RowKind::Context, b' '),
                    PatchLineKind::Added => (RowKind::Added, b'+'),
                    PatchLineKind::Removed => (RowKind::Removed, b'-'),
                };
                let mut text = vec![prefix];
                text.extend(trim(&line.text));
                rows.push(Row {
                    kind,
                    hunk: Some(index),
                    text,
                });
                if line.no_newline {
                    rows.push(Row {
                        kind: RowKind::NoNewline,
                        hunk: Some(index),
                        text: b"\\ No newline at end of file".to_vec(),
                    });
                }
            }
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::patch::parse;

    /// Two hunks of 3 lines each after a 3-line header: rows 3 and 7 are `@@`.
    const TWO: &[u8] = b"diff --git a/x b/x\n--- a/x\n+++ b/x\n\
        @@ -1,2 +1,2 @@ f\n-a\n+b\n c\n\
        @@ -9,2 +9,2 @@\n d\n-e\n+g\r\n\\ No newline at end of file\n";

    fn view() -> HunkView {
        let mut view = HunkView::new("x".into(), DiffSide::Worktree);
        view.loaded(parse(TWO).unwrap(), 20);
        view
    }

    #[test]
    fn rows_list_the_header_then_each_hunk() {
        let rows = view().rows();
        let texts: Vec<String> = rows
            .iter()
            .map(|r| String::from_utf8_lossy(&r.text).into_owned())
            .collect();
        assert_eq!(
            texts,
            [
                "diff --git a/x b/x",
                "--- a/x",
                "+++ b/x",
                "@@ -1,2 +1,2 @@ f",
                "-a",
                "+b",
                " c",
                "@@ -9,2 +9,2 @@",
                " d",
                "-e",
                "+g",
                "\\ No newline at end of file",
            ]
        );
        assert_eq!(rows[3].kind, RowKind::HunkHeader);
        assert_eq!(rows[3].hunk, Some(0));
        assert_eq!(rows[8].hunk, Some(1));
        assert_eq!(rows[0].hunk, None);
        assert_eq!(rows[11].kind, RowKind::NoNewline);
    }

    #[test]
    fn next_and_prev_stop_at_the_ends() {
        let mut view = view();
        view.prev(20);
        assert_eq!(view.hunk, 0);
        view.next(20);
        view.next(20);
        assert_eq!(view.hunk, 1);
        view.prev(20);
        assert_eq!(view.hunk, 0);
    }

    #[test]
    fn a_reload_keeps_the_index_but_clamps_it() {
        let mut view = view();
        view.next(20);
        let one = b"diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n";
        view.loaded(parse(one).unwrap(), 20);
        assert_eq!(view.hunk, 0);
        view.loaded(parse(b"").unwrap(), 20);
        assert_eq!(view.hunk, 0);
        assert_eq!(view.scroll, 0);
    }

    #[test]
    fn moving_to_a_hunk_below_the_pane_scrolls_it_to_the_top() {
        let mut view = view();
        view.next(5);
        assert_eq!(view.scroll, 7);
        // Back up: hunk 0's `@@` is above the window, so it comes into view.
        view.prev(5);
        assert_eq!(view.scroll, 3);
        // Already visible: no jump.
        view.scroll = 1;
        view.scroll_to_hunk(5);
        assert_eq!(view.scroll, 1);
    }

    #[test]
    fn scrolling_is_clamped_to_the_rows() {
        let mut view = view();
        view.scroll(Scroll::PageDown, 100);
        assert_eq!(view.scroll, 11);
        view.scroll(Scroll::LineUp, 5);
        assert_eq!(view.scroll, 10);
        view.scroll(Scroll::PageUp, 100);
        assert_eq!(view.scroll, 0);
    }
}
