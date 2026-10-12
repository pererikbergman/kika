use super::ansi::StyledRow;
use crate::git::diff::DiffSide;
use crate::git::path::RepoPath;
use crate::intent::Scroll;

/// One side of a file's diff as an external renderer drew it, with each
/// row mapped back to the patch line it shows. Pure state; the renderer
/// runs as an effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedDiff {
    /// The render in flight, if any; only its result is taken. The last
    /// rows stay on screen meanwhile, so a re-render does not flicker.
    pub seq: Option<u64>,
    pub path: RepoPath,
    pub side: DiffSide,
    pub rows: Vec<StyledRow>,
    /// Per row, `(hunk, line index in the hunk)` of the patch line it
    /// shows; `None` for decoration. Empty when there is no patch to map
    /// to, and always for a diff tool, whose rows show no patch lines.
    pub map: Vec<Option<(usize, usize)>>,
    /// Index of the top visible row.
    pub scroll: usize,
    pub error: Option<String>,
}

impl RenderedDiff {
    /// A render of `path`'s `side` that has not reported yet.
    pub fn new(seq: u64, path: RepoPath, side: DiffSide) -> Self {
        Self {
            seq: Some(seq),
            path,
            side,
            rows: Vec::new(),
            map: Vec::new(),
            scroll: 0,
            error: None,
        }
    }

    /// The renderer is running and there is nothing to show yet.
    pub fn rendering(&self) -> bool {
        self.seq.is_some() && self.rows.is_empty() && self.error.is_none()
    }

    /// Take the result of render `seq`; any other is dropped. Returns
    /// whether it was taken, so the caller can map the new rows.
    pub fn apply(&mut self, seq: u64, result: Result<Vec<StyledRow>, String>) -> bool {
        if self.seq != Some(seq) {
            return false;
        }
        self.seq = None;
        match result {
            Ok(rows) => {
                self.rows = rows;
                self.error = None;
            }
            Err(message) => {
                self.rows.clear();
                self.error = Some(message);
            }
        }
        self.map.clear();
        self.scroll = self.scroll.min(self.max_scroll());
        true
    }

    /// Move by a line or a page of `rows`, clamped to the rows there are.
    pub fn scroll(&mut self, by: Scroll, rows: usize) {
        let scroll = match by {
            Scroll::LineDown => self.scroll.saturating_add(1),
            Scroll::LineUp => self.scroll.saturating_sub(1),
            Scroll::PageDown => self.scroll.saturating_add(rows),
            Scroll::PageUp => self.scroll.saturating_sub(rows),
        };
        self.scroll = scroll.min(self.max_scroll());
    }

    /// Keep the `selection` rows inside a pane of `rows`. When they are
    /// below, the first goes as high as the last needs, so as much as
    /// fits shows.
    pub fn scroll_to(&mut self, selection: &[usize], rows: usize) {
        let (Some(&first), Some(&last)) = (selection.first(), selection.last()) else {
            return;
        };
        let rows = rows.max(1);
        if first < self.scroll {
            self.scroll = first;
        } else if last >= self.scroll + rows {
            self.scroll = (last + 1 - rows).min(first);
        }
    }

    /// The rows that show hunk `hunk`, or only its lines `lines` when set.
    pub fn mapped_rows(&self, hunk: usize, lines: Option<&[usize]>) -> Vec<usize> {
        self.map
            .iter()
            .enumerate()
            .filter_map(|(row, at)| match at {
                Some((h, i)) if *h == hunk && lines.is_none_or(|l| l.contains(i)) => Some(row),
                _ => None,
            })
            .collect()
    }

    /// Whether every one of `lines` of `hunk` has a row, or with `None`,
    /// whether any row of the hunk does: the cursor can only be shown on
    /// rows the renderer drew.
    pub fn shows(&self, hunk: usize, lines: Option<&[usize]>) -> bool {
        match lines {
            None => !self.mapped_rows(hunk, None).is_empty(),
            Some(lines) => lines.iter().all(|i| self.map.contains(&Some((hunk, *i)))),
        }
    }

    fn max_scroll(&self) -> usize {
        self.rows.len().saturating_sub(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::diff::renderers::ansi::parse;

    fn rendered(rows: usize) -> RenderedDiff {
        let mut view = RenderedDiff::new(1, "a.rs".into(), DiffSide::Worktree);
        let text: String = (0..rows).map(|i| format!("row {i}\n")).collect();
        assert!(view.apply(1, Ok(parse(text.as_bytes()))));
        view
    }

    #[test]
    fn only_the_awaited_render_is_taken() {
        let mut view = RenderedDiff::new(4, "a.rs".into(), DiffSide::Worktree);
        assert!(view.rendering());
        assert!(!view.apply(3, Ok(parse(b"old\n"))));
        assert!(view.rows.is_empty());
        assert!(view.apply(4, Err("boom".into())));
        assert_eq!(view.error.as_deref(), Some("boom"));
        assert!(!view.rendering());
        assert!(!view.apply(4, Ok(parse(b"late\n"))), "taken once");
    }

    #[test]
    fn scrolling_is_clamped_to_the_rows() {
        let mut view = rendered(10);
        view.scroll(Scroll::PageDown, 100);
        assert_eq!(view.scroll, 9);
        view.scroll(Scroll::LineUp, 5);
        assert_eq!(view.scroll, 8);
        view.scroll(Scroll::PageUp, 100);
        assert_eq!(view.scroll, 0);
    }

    #[test]
    fn scroll_to_brings_the_selection_into_view() {
        let mut view = rendered(30);
        view.scroll_to(&[12, 13, 14], 5);
        assert_eq!(view.scroll, 10, "the last row at the bottom");
        view.scroll_to(&[12], 5);
        assert_eq!(view.scroll, 10, "already visible");
        view.scroll_to(&[3, 4], 5);
        assert_eq!(view.scroll, 3);
        view.scroll_to(&[20, 29], 5);
        assert_eq!(view.scroll, 20, "taller than the pane: its top shows");
        view.scroll_to(&[], 5);
        assert_eq!(view.scroll, 20);
    }

    #[test]
    fn mapped_rows_pick_the_hunk_or_its_lines() {
        let mut view = rendered(6);
        view.map = vec![
            None,
            Some((0, 0)),
            Some((0, 1)),
            None,
            Some((1, 0)),
            Some((1, 2)),
        ];
        assert_eq!(view.mapped_rows(0, None), [1, 2]);
        assert_eq!(view.mapped_rows(1, Some(&[2])), [5]);
        assert!(view.shows(1, None));
        assert!(view.shows(1, Some(&[0, 2])));
        assert!(!view.shows(1, Some(&[1, 2])), "line 1 has no row");
        assert!(!view.shows(2, None));
    }
}
