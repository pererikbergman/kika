use std::collections::BTreeSet;

use crate::diff_search::{DiffSearch, reveal};
use crate::git::commit_files::CommitRange;
use crate::git::diff::{DiffSide, EmojiZwj, sanitize, sanitize_body};
use crate::git::patch::{self, FilePatch, LineId, PatchLineKind};
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
    /// The commit whose change of `path` this is; `None` for the working
    /// tree. A commit's lines are picked for the custom patch, not staged.
    pub commit: Option<CommitRange>,
    /// `None` until the first load lands, or after one fails.
    pub patch: Option<FilePatch>,
    /// Index of the selected hunk.
    pub hunk: usize,
    /// Index of the top visible row.
    pub scroll: usize,
    /// Line mode: picking changed lines inside the selected hunk.
    pub lines: Option<LineCursor>,
    /// The text searched for and where it appears. A new view starts
    /// without one, so leaving the pane or changing files drops it.
    pub search: Option<DiffSearch>,
    /// The emoji policy `search` was built with, so a reloaded patch is
    /// searched as it is drawn.
    search_emoji: EmojiZwj,
}

/// A range of changed lines in the selected hunk, from `anchor` to
/// `cursor` either way round. Both index the hunk's `lines` and always sit
/// on changed lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineCursor {
    pub anchor: usize,
    pub cursor: usize,
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
    /// Index in the hunk's `lines`; `None` for `@@`, header and marker rows.
    pub line: Option<usize>,
    pub text: Vec<u8>,
}

impl HunkView {
    pub fn new(path: RepoPath, side: DiffSide) -> Self {
        Self {
            seq: None,
            path,
            side,
            commit: None,
            patch: None,
            hunk: 0,
            scroll: 0,
            lines: None,
            search: None,
            search_emoji: EmojiZwj::Escape,
        }
    }

    /// `path`'s change in the commit `range` names.
    pub fn for_commit(path: RepoPath, range: CommitRange) -> Self {
        Self {
            commit: Some(range),
            ..Self::new(path, DiffSide::Index)
        }
    }

    /// Every changed line of the loaded patch, from every hunk.
    pub fn all_lines(&self) -> BTreeSet<LineId> {
        self.patch
            .iter()
            .flat_map(|p| &p.hunks)
            .flat_map(|h| h.line_ids())
            .flatten()
            .collect()
    }

    /// The ids of the selected hunk's changed lines.
    pub fn hunk_lines(&self) -> BTreeSet<LineId> {
        self.patch
            .as_ref()
            .and_then(|p| p.hunks.get(self.hunk))
            .map(|h| h.line_ids().into_iter().flatten().collect())
            .unwrap_or_default()
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
        self.hunk_moved(rows);
    }

    /// Move the cursor up a hunk, stopping at the first.
    pub fn prev(&mut self, rows: usize) {
        self.hunk = self.hunk.saturating_sub(1);
        self.hunk_moved(rows);
    }

    /// Line indexes only mean something in their own hunk, so line mode
    /// starts over in the new one.
    fn hunk_moved(&mut self, rows: usize) {
        if self.lines.is_some() {
            self.enter_lines();
        }
        self.scroll_to_hunk(rows);
    }

    /// Take a freshly loaded patch. The cursor keeps its index, clamped, so
    /// after staging a hunk the next one is under it. Line mode stays on,
    /// on the changed line with the same number among the hunk's changes
    /// (or the last), so after staging a line the next one is under it.
    pub fn loaded(&mut self, patch: FilePatch, rows: usize) {
        let nth = self.lines.map(|lines| {
            let changes = self.changes();
            changes.iter().filter(|&&i| i < lines.cursor).count()
        });
        self.patch = Some(patch);
        self.hunk = self.hunk.min(self.hunk_count().saturating_sub(1));
        if let Some(nth) = nth {
            let changes = self.changes();
            self.lines = changes
                .get(nth)
                .or(changes.last())
                .map(|&cursor| LineCursor {
                    anchor: cursor,
                    cursor,
                });
        }
        self.scroll_to_hunk(rows);
        self.scroll_to_cursor(rows);
        if let Some(mut search) = self.search.take() {
            search.refresh(&self.drawn_rows(self.search_emoji));
            self.search = Some(search);
        }
    }

    /// Search the rows as drawn with `emoji` for `query`, from the top row
    /// shown; an empty query ends the search.
    pub fn set_search(&mut self, query: &str, emoji: EmojiZwj) {
        self.search_emoji = emoji;
        self.search = (!query.is_empty())
            .then(|| DiffSearch::new(query, &self.drawn_rows(emoji), self.scroll));
    }

    /// Go to the current match: its hunk becomes the selected one, and the
    /// pane scrolls just enough to show its row.
    pub fn jump_to_match(&mut self, rows: usize) {
        let Some(found) = self.search.as_ref().and_then(|s| s.current_match()) else {
            return;
        };
        let row = found.row;
        if let Some(hunk) = self.rows().get(row).and_then(|r| r.hunk) {
            self.hunk = hunk;
        }
        self.scroll = reveal(row, self.scroll, rows);
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

    /// The text of each row as the pane draws it, so search finds what the
    /// user sees.
    pub fn drawn_rows(&self, emoji: EmojiZwj) -> Vec<String> {
        self.rows()
            .iter()
            .map(|row| drawn_text(row, emoji))
            .collect()
    }

    /// Start picking lines, on the selected hunk's first change.
    pub fn enter_lines(&mut self) {
        self.lines = self.changes().first().map(|&first| LineCursor {
            anchor: first,
            cursor: first,
        });
    }

    pub fn exit_lines(&mut self) {
        self.lines = None;
    }

    /// Move to the next changed line of the hunk, stopping at the last.
    /// With `extend` the anchor stays, growing or shrinking the range.
    pub fn next_line(&mut self, extend: bool) {
        let changes = self.changes();
        self.move_cursor(extend, |cursor| {
            changes.iter().copied().find(|&i| i > cursor)
        });
    }

    /// Move to the previous changed line of the hunk, stopping at the first.
    pub fn prev_line(&mut self, extend: bool) {
        let changes = self.changes();
        self.move_cursor(extend, |cursor| {
            changes.iter().copied().rev().find(|&i| i < cursor)
        });
    }

    fn move_cursor(&mut self, extend: bool, to: impl Fn(usize) -> Option<usize>) {
        let Some(lines) = &mut self.lines else {
            return;
        };
        lines.cursor = to(lines.cursor).unwrap_or(lines.cursor);
        if !extend {
            lines.anchor = lines.cursor;
        }
    }

    /// Indexes of the changed lines in the selected range, in order.
    pub fn selected_range(&self) -> Vec<usize> {
        let Some(lines) = self.lines else {
            return Vec::new();
        };
        let (from, to) = (
            lines.anchor.min(lines.cursor),
            lines.anchor.max(lines.cursor),
        );
        self.changes()
            .into_iter()
            .filter(|i| (from..=to).contains(i))
            .collect()
    }

    /// The picked lines in line mode; `None` in hunk mode, where the
    /// whole hunk is what the cursor marks.
    pub fn picked(&self) -> Option<Vec<usize>> {
        self.lines.map(|_| self.selected_range())
    }

    /// The ids of the changed lines in the selected range; context between
    /// them is skipped. Empty outside line mode.
    pub fn selected_lines(&self) -> BTreeSet<LineId> {
        let Some(hunk) = self.patch.as_ref().and_then(|p| p.hunks.get(self.hunk)) else {
            return BTreeSet::new();
        };
        let ids = hunk.line_ids();
        self.selected_range()
            .into_iter()
            .filter_map(|i| ids[i])
            .collect()
    }

    /// Where to open the worktree file: the line cursor's line in line
    /// mode, else the selected hunk's first change, numbered on the new
    /// side (a removed line opens where it was). `None` on the staged side,
    /// whose numbers are the index's, not the file's.
    pub fn editor_line(&self) -> Option<u32> {
        if self.side == DiffSide::Index {
            return None;
        }
        let hunk = self.patch.as_ref()?.hunks.get(self.hunk)?;
        let at = match self.lines {
            Some(lines) => lines.cursor,
            None => hunk
                .lines
                .iter()
                .position(|l| l.kind != PatchLineKind::Context)?,
        };
        let first = hunk.new_start.saturating_add(u32::from(hunk.new_len == 0));
        let before = hunk.lines.iter().take(at);
        let line = before
            .filter(|l| l.kind != PatchLineKind::Removed)
            .fold(first, |n, _| n.saturating_add(1));
        Some(line.max(1))
    }

    /// Indexes of the selected hunk's changed lines.
    fn changes(&self) -> Vec<usize> {
        let Some(hunk) = self.patch.as_ref().and_then(|p| p.hunks.get(self.hunk)) else {
            return Vec::new();
        };
        hunk.lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.kind != PatchLineKind::Context)
            .map(|(i, _)| i)
            .collect()
    }

    /// Keep the line cursor's row inside a pane of `rows`.
    pub fn scroll_to_cursor(&mut self, rows: usize) {
        let Some(lines) = self.lines else {
            return;
        };
        let Some(at) = self
            .rows()
            .iter()
            .position(|r| r.hunk == Some(self.hunk) && r.line == Some(lines.cursor))
        else {
            return;
        };
        let rows = rows.max(1);
        if at < self.scroll {
            self.scroll = at;
        } else if at >= self.scroll + rows {
            self.scroll = at + 1 - rows;
        }
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
                line: None,
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
                line: None,
                text,
            });
            for (at, line) in hunk.lines.iter().enumerate() {
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
                    line: Some(at),
                    text,
                });
                if line.no_newline {
                    rows.push(Row {
                        kind: RowKind::NoNewline,
                        hunk: Some(index),
                        line: None,
                        text: b"\\ No newline at end of file".to_vec(),
                    });
                }
            }
        }
        rows
    }
}

/// A row's text as drawn: terminal-safe, header rows strictly so, file
/// lines keeping the joiners their script needs, and emoji joiners as
/// `emoji` says. The view and the search both use it, with the same
/// policy, so a match is always where it shows.
pub fn drawn_text(row: &Row, emoji: EmojiZwj) -> String {
    match row.kind {
        RowKind::Header | RowKind::HunkHeader => sanitize(&row.text),
        RowKind::Context | RowKind::Added | RowKind::Removed | RowKind::NoNewline => {
            sanitize_body(&row.text, emoji)
        }
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
    fn the_editor_line_is_the_first_change_or_the_cursor_on_the_new_side() {
        let mut view = view();
        assert_eq!(view.editor_line(), Some(1));
        view.next(20);
        // ` d` is context at 9, so `-e` sat at new line 10.
        assert_eq!(view.editor_line(), Some(10));
        view.enter_lines();
        view.next_line(false);
        assert_eq!(view.editor_line(), Some(10));

        // A hunk that only removes opens after the line before it.
        let mut gone = HunkView::new("x".into(), DiffSide::Worktree);
        gone.loaded(
            parse(b"diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -4,2 +3,0 @@\n-a\n-b\n").unwrap(),
            20,
        );
        assert_eq!(gone.editor_line(), Some(4));

        let mut staged = HunkView::new("x".into(), DiffSide::Index);
        staged.loaded(parse(TWO).unwrap(), 20);
        assert_eq!(staged.editor_line(), None);
        assert_eq!(
            HunkView::new("x".into(), DiffSide::Worktree).editor_line(),
            None
        );
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

    /// One hunk: `-a +A`, context `m`, `-b +B`, context `z`.
    const SPLIT: &[u8] = b"diff --git a/x b/x\n--- a/x\n+++ b/x\n\
        @@ -1,4 +1,4 @@\n-a\n+A\n m\n-b\n+B\n z\n";

    fn split() -> HunkView {
        let mut view = HunkView::new("x".into(), DiffSide::Worktree);
        view.loaded(parse(SPLIT).unwrap(), 20);
        view
    }

    fn at(cursor: usize, anchor: usize) -> Option<LineCursor> {
        Some(LineCursor { anchor, cursor })
    }

    #[test]
    fn line_mode_starts_on_the_first_change_and_exits() {
        let mut view = view();
        view.next(20);
        view.enter_lines();
        // Hunk 1 starts with context ` d`.
        assert_eq!(view.lines, at(1, 1));
        view.exit_lines();
        assert_eq!(view.lines, None);
        // Nothing loaded: nothing to pick.
        let mut empty = HunkView::new("x".into(), DiffSide::Worktree);
        empty.enter_lines();
        assert_eq!(empty.lines, None);
    }

    #[test]
    fn line_moves_skip_context_and_stop_at_the_hunk_ends() {
        let mut view = split();
        view.enter_lines();
        view.prev_line(false);
        assert_eq!(view.lines, at(0, 0));
        view.next_line(false);
        assert_eq!(view.lines, at(1, 1));
        // Over the context line `m`.
        view.next_line(false);
        assert_eq!(view.lines, at(3, 3));
        view.next_line(false);
        view.next_line(false);
        assert_eq!(view.lines, at(4, 4), "never onto `z` or past the hunk");
        view.prev_line(false);
        view.prev_line(false);
        assert_eq!(view.lines, at(1, 1));
    }

    #[test]
    fn extending_builds_a_range_either_way() {
        let mut view = split();
        view.enter_lines();
        view.next_line(true);
        view.next_line(true);
        assert_eq!(view.lines, at(3, 0));
        assert_eq!(view.selected_range(), [0, 1, 3]);
        assert_eq!(
            view.selected_lines(),
            [LineId::removed(1), LineId::added(1), LineId::removed(3)].into()
        );
        // Back past the anchor flips the range.
        view.next_line(false);
        view.prev_line(true);
        view.prev_line(true);
        assert_eq!(view.lines, at(1, 4));
        assert_eq!(
            view.selected_lines(),
            [LineId::added(1), LineId::removed(3), LineId::added(3)].into()
        );
        view.exit_lines();
        assert!(view.selected_lines().is_empty());
    }

    #[test]
    fn rows_say_which_hunk_line_they_draw() {
        let rows = view().rows();
        let lines: Vec<Option<usize>> = rows.iter().map(|r| r.line).collect();
        assert_eq!(
            lines,
            [
                None,
                None,
                None,
                None,
                Some(0),
                Some(1),
                Some(2),
                None,
                Some(0),
                Some(1),
                Some(2),
                None
            ]
        );
    }

    #[test]
    fn a_reload_keeps_line_mode_on_the_same_change_or_the_last() {
        let mut view = split();
        view.enter_lines();
        view.next_line(false);
        view.next_line(true);
        // `+A` was staged: `a` is context now, and the 2nd change is `+B`.
        let staged = b"diff --git a/x b/x\n--- a/x\n+++ b/x\n\
            @@ -1,4 +1,4 @@\n A\n m\n-b\n+B\n z\n";
        view.loaded(parse(staged).unwrap(), 20);
        assert_eq!(view.lines, at(3, 3), "range collapsed onto the next change");
        let one = b"diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +0,0 @@\n-a\n";
        view.loaded(parse(one).unwrap(), 20);
        assert_eq!(view.lines, at(0, 0), "clamped to the last change");
        view.loaded(parse(b"").unwrap(), 20);
        assert_eq!(view.lines, None, "no hunk left");
    }

    #[test]
    fn moving_to_another_hunk_in_line_mode_starts_on_its_first_change() {
        let mut view = view();
        view.enter_lines();
        view.next_line(true);
        view.next(20);
        assert_eq!(view.lines, at(1, 1));
        assert_eq!(view.selected_lines(), [LineId::removed(10)].into());
        view.prev(20);
        assert_eq!(view.lines, at(0, 0));
    }

    #[test]
    fn the_line_cursor_is_kept_on_screen() {
        let mut view = split();
        view.enter_lines();
        for _ in 0..3 {
            view.next_line(false);
        }
        // Rows: 3 header, `@@` at 3, lines from 4; `+B` is row 8.
        view.scroll_to_cursor(2);
        assert_eq!(view.scroll, 7);
        view.prev_line(false);
        view.prev_line(false);
        view.prev_line(false);
        view.scroll_to_cursor(2);
        assert_eq!(view.scroll, 4);
    }

    #[test]
    fn search_reads_the_rows_with_the_emoji_policy_they_are_drawn_with() {
        let family = "👨\u{200D}👩";
        let diff = format!("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+{family}\n");
        let mut view = HunkView::new("x".into(), DiffSide::Worktree);
        view.loaded(parse(diff.as_bytes()).unwrap(), 20);
        view.set_search(family, EmojiZwj::Keep);
        assert!(
            view.search
                .as_ref()
                .is_some_and(|s| s.current_match().is_some())
        );
        view.set_search(family, EmojiZwj::Escape);
        assert!(
            view.search
                .as_ref()
                .is_some_and(|s| s.current_match().is_none())
        );
        view.set_search("👨<U+200D>👩", EmojiZwj::Escape);
        assert!(
            view.search
                .as_ref()
                .is_some_and(|s| s.current_match().is_some())
        );
    }

    #[test]
    fn a_search_jumps_to_its_hunk_and_follows_reloads() {
        let mut view = view();
        view.set_search("+g", EmojiZwj::Escape);
        let search = view.search.as_ref().unwrap();
        assert_eq!(search.current_match().map(|m| m.row), Some(10));
        view.jump_to_match(3);
        assert_eq!(view.hunk, 1);
        assert_eq!(view.scroll, 8, "row 10 is the last of three shown");
        // A reload keeps the search on the same row's match.
        view.loaded(parse(TWO).unwrap(), 3);
        assert_eq!(view.search.as_ref().unwrap().matches.len(), 1);
        view.set_search("", EmojiZwj::Escape);
        assert_eq!(view.search, None);
    }
}
