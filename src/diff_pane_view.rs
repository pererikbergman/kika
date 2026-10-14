//! The Diff pane's drawing: the hunk cursor's view, the streamed lines,
//! and the search marks and position notes that review and blame draw
//! their diff lines with too. State is in `diff_pane.rs` and
//! `hunk_view.rs`; this only reads it.

use crate::diff_pane::DiffPane;
use crate::diff_search::DiffSearch;
use crate::features::commits;
use crate::features::diff::view::hunk_title;
use crate::hunk_view::{HunkView, NO_CHANGES, RowKind, drawn_text};
use crate::view::{FOCUSED, PanelLook, clip_cut, continues_glyph, reversed};
use kika_git::diff::{DiffLine, DiffOptions, EmojiZwj, LineKind};
use kika_git::patch::{FileKind, PatchError};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use std::collections::HashSet;

/// Where in the streamed diff the pane is: ` · <first shown>/<loaded>`,
/// `+` while more are coming; empty with nothing to show.
pub(crate) fn diff_position(diff: &DiffPane) -> String {
    if diff.lines.is_empty() || diff.error.is_some() {
        return String::new();
    }
    let first = (diff.scroll + 1).min(diff.lines.len());
    let more = if diff.done { "" } else { "+" };
    format!(" · {first}/{}{more}", diff.lines.len())
}

/// The inside of a streamed Diff pane at `inner`'s size: an error, a
/// delayed "Loading…", "No diff", or the visible lines, with the
/// search's matches marked on them.
pub(crate) fn diff_pane_body(diff: &DiffPane, inner: Rect) -> Paragraph<'static> {
    let (rows, width) = (usize::from(inner.height), usize::from(inner.width));
    if let Some(error) = &diff.error {
        let lines = error.lines().map(|l| Line::from(l.to_string()));
        return Paragraph::new(lines.collect::<Vec<_>>()).wrap(Wrap { trim: false });
    }
    if diff.show_loading {
        return Paragraph::new("Loading…");
    }
    if diff.done && diff.lines.is_empty() {
        return Paragraph::new("No diff");
    }
    // Only the visible slice: a 50,000-line diff costs a screenful per frame.
    let start = diff.scroll.min(diff.lines.len());
    let end = start.saturating_add(rows).min(diff.lines.len());
    Paragraph::new(
        diff.lines[start..end]
            .iter()
            .zip(start..)
            .map(|(line, at)| styled_diff_line(line, width, &marks_of(&diff.search, at)))
            .collect::<Vec<_>>(),
    )
}

/// The focused Diff pane: one side of the file as hunks, the selected one
/// marked by a reversed `@@` row and a `▌` gutter, so it reads without colour.
/// In line mode the picked lines are marked that way instead.
/// `suffix`, when set, follows the title after a `·`.
pub(crate) fn render_hunk_view(
    pane: HunkPane,
    options: DiffOptions,
    emoji: EmojiZwj,
    suffix: &str,
    look: &PanelLook,
    frame: &mut Frame,
    area: Rect,
) {
    let view = pane.view;
    let block = look.block(hunk_title(view, options, suffix, look));
    let inner = block.inner(area);
    let (rows, width) = (usize::from(inner.height), usize::from(inner.width));
    // A commit's change gives a second column to what the patch holds.
    let picked_ids = pane.picks;
    let gutters = if picked_ids.is_some() { 2 } else { 1 };
    let pane = match &view.patch {
        None if view.loading() => Paragraph::new("Loading…"),
        None => Paragraph::new("Could not read the diff"),
        Some(patch) if patch.kind == FileKind::Binary => {
            Paragraph::new(PatchError::Binary.message())
        }
        Some(patch) if patch.kind == FileKind::Unsupported => {
            Paragraph::new(PatchError::Unsupported.message())
        }
        Some(patch) if patch.hunks.is_empty() => Paragraph::new(NO_CHANGES),
        Some(_) => {
            let picked = view.selected_range();
            let lines = view
                .rows()
                .into_iter()
                .enumerate()
                .skip(view.scroll)
                .take(rows)
                .map(|(at, row)| {
                    let in_hunk = row.hunk == Some(view.hunk);
                    // In line mode only the picked lines are marked.
                    let selected = if view.lines.is_some() {
                        in_hunk && row.line.is_some_and(|i| picked.contains(&i))
                    } else {
                        in_hunk
                    };
                    let gutter = if selected { "▌" } else { " " };
                    let style = match row.kind {
                        RowKind::Header => Style::new().add_modifier(Modifier::BOLD),
                        RowKind::HunkHeader => Style::new().fg(Color::Cyan),
                        RowKind::Added => Style::new().fg(Color::Green),
                        RowKind::Removed => Style::new().fg(Color::Red),
                        RowKind::Context => Style::new(),
                        RowKind::NoNewline => Style::new().fg(Color::DarkGray),
                    };
                    let mut spans = vec![Span::styled(gutter, Style::new().fg(FOCUSED))];
                    if let Some(ids) = picked_ids {
                        let held = matches!((row.hunk, row.line), (Some(h), Some(l)) if ids.contains(&(h, l)));
                        let mark = if held { "●" } else { " " };
                        spans.push(Span::styled(mark, commits::view::PATCH_MARK));
                    }
                    spans.extend(clipped_marked(
                        &drawn_text(&row, emoji),
                        width.saturating_sub(gutters),
                        style,
                        &marks_of(&view.search, at),
                    ));
                    let line = Line::from(spans);
                    let mark = match view.lines {
                        Some(_) => row.line.is_some(),
                        None => row.kind == RowKind::HunkHeader,
                    };
                    if selected && mark {
                        reversed(line, width)
                    } else {
                        line
                    }
                })
                .collect::<Vec<_>>();
            Paragraph::new(lines)
        }
    };
    frame.render_widget(pane.block(block), area);
}

/// The focused hunk view, with the `(hunk, line)` rows a custom patch
/// holds when the view is of the commit it takes changes from. Core works
/// the rows out, so the pane knows nothing of the patch.
#[derive(Clone, Copy)]
pub(crate) struct HunkPane<'a> {
    pub(crate) view: &'a HunkView,
    pub(crate) picks: Option<&'a HashSet<(usize, usize)>>,
}

/// [`search_marks`] of a pane's search, if it has one.
pub(crate) fn marks_of(search: &Option<DiffSearch>, at: usize) -> Vec<(usize, usize, Style)> {
    search
        .as_ref()
        .map_or_else(Vec::new, |s| search_marks(s, at))
}

/// Matches of the search on row `at`: char ranges and their style, the
/// current match standing out from the rest.
fn search_marks(search: &DiffSearch, at: usize) -> Vec<(usize, usize, Style)> {
    // Matches are in row order, so this row's are found by bisection.
    let from = search.matches.partition_point(|m| m.row < at);
    let to = search.matches.partition_point(|m| m.row <= at);
    search.matches[from..to]
        .iter()
        .zip(from..)
        .map(|(m, i)| {
            let style = if Some(i) == search.current {
                CURRENT_MATCH
            } else {
                MATCH
            };
            (m.start, m.end, style)
        })
        .collect()
}

/// A search match, and the one `n` and `N` step from.
pub(crate) const MATCH: Style = Style::new().fg(Color::Black).bg(Color::Yellow);
/// The current one is also bold and underlined, so it stands out without
/// colour.
pub(crate) const CURRENT_MATCH: Style = Style::new()
    .fg(Color::Black)
    .bg(Color::LightRed)
    .add_modifier(Modifier::BOLD.union(Modifier::UNDERLINED));

/// `text` in `style`, split so each char range in `marks` (in order, not
/// overlapping) takes its own style on top. Ranges past the end of a
/// clipped text are cut to it. Each range widens to whole glyphs: the
/// buffer measures each span on its own, so a span that ended inside an
/// emoji sequence or before a combining mark would split what `clip` laid
/// out as one glyph, and shift the rest of the row. A glyph holding the
/// current match keeps its style even when another match shares it.
fn marked_spans(text: String, style: Style, marks: &[(usize, usize, Style)]) -> Vec<Span<'static>> {
    if marks.is_empty() {
        return vec![Span::styled(text, style)];
    }
    let continues = continues_glyph(&text);
    let chars: Vec<char> = text.chars().collect();
    let inside = |i: usize| continues.get(i).copied().unwrap_or(false);
    let piece = |from: usize, to: usize| {
        chars[from.min(chars.len())..to.min(chars.len())]
            .iter()
            .collect::<String>()
    };
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut at = 0;
    // The mark the last marked span carries.
    let mut last_mark = None;
    for &(start, end, mark) in marks {
        let mut start = start.min(chars.len()).max(at);
        while start > at && inside(start) {
            start -= 1;
        }
        let mut end = end.min(chars.len());
        while inside(end) {
            end += 1;
        }
        // A match in the glyph the last one widened to: that span already
        // holds it, and takes this match's style on top, unless it holds
        // the current match, which keeps its own look so `n` and `N` always
        // show where they stopped.
        if end <= start {
            if at > 0
                && last_mark != Some(CURRENT_MATCH)
                && let Some(last) = spans.last_mut()
            {
                last.style = last.style.patch(mark);
                last_mark = Some(mark);
            }
            continue;
        }
        spans.push(Span::styled(piece(at, start), style));
        spans.push(Span::styled(piece(start, end), style.patch(mark)));
        last_mark = Some(mark);
        at = end;
    }
    spans.push(Span::styled(piece(at, chars.len()), style));
    spans.retain(|span| !span.content.is_empty());
    spans
}

/// `line` clipped to `width` from its start, with `marks` drawn over
/// `style`. A cut line's `…` stays in `style`: it stands for text off
/// screen, not for the match, and a marked `…` would read as matched.
pub(crate) fn clipped_marked(
    line: &str,
    width: usize,
    style: Style,
    marks: &[(usize, usize, Style)],
) -> Vec<Span<'static>> {
    let (text, cut) = clip_cut(line, 0, width);
    if !cut {
        return marked_spans(text, style, marks);
    }
    let ellipsis = text.chars().count() - 1;
    let marks: Vec<_> = marks
        .iter()
        .map(|&(start, end, mark)| (start, end.min(ellipsis), mark))
        .filter(|&(start, end, _)| start < end)
        .collect();
    marked_spans(text, style, &marks)
}

/// Where a search is, for a pane's title: " · match i of n", with `+` on
/// the count while `more` lines can still come, or " · no matches". Never
/// the query: the bottom bar shows that, escaped.
pub(crate) fn found_note(search: Option<&DiffSearch>, more: bool) -> String {
    let more = if more { "+" } else { "" };
    match search {
        Some(search) => match search.current {
            Some(i) => format!(" · match {} of {}{more}", i + 1, search.matches.len()),
            None => " · no matches".to_string(),
        },
        None => String::new(),
    }
}

/// Colour by kind; `---`/`+++` arrive as headers, so they are never red or
/// green. A line wider than `width` ends in `…`, so a cut is never silent.
/// `marks` are search matches, drawn over the colour.
pub(crate) fn styled_diff_line(
    line: &DiffLine,
    width: usize,
    marks: &[(usize, usize, Style)],
) -> Line<'static> {
    let style = match line.kind {
        LineKind::Added => Style::new().fg(Color::Green),
        LineKind::Removed => Style::new().fg(Color::Red),
        LineKind::Hunk => Style::new().fg(Color::Cyan),
        LineKind::Header => Style::new().add_modifier(Modifier::BOLD),
        LineKind::Context => Style::new(),
    };
    Line::from(clipped_marked(&line.text, width, style, marks))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_widen_to_whole_glyphs_and_a_second_mark_in_one_glyph_restyles_it() {
        let family = "👨\u{200D}👩";
        let text = format!("Q{family}X");
        let spans = |marks: &[(usize, usize, Style)]| -> Vec<(String, Style)> {
            marked_spans(text.clone(), Style::new(), marks)
                .into_iter()
                .map(|span| (span.content.into_owned(), span.style))
                .collect()
        };
        let plain = Style::new();
        // `👩` (chars 3..4) and the ZWJ (2..3) both widen to the sequence.
        for range in [(3, 4), (2, 3), (1, 2), (1, 4)] {
            assert_eq!(
                spans(&[(range.0, range.1, MATCH)]),
                [
                    ("Q".to_string(), plain),
                    (family.to_string(), MATCH),
                    ("X".to_string(), plain),
                ],
                "{range:?}"
            );
        }
        // Two matches in the one sequence: the current one wins.
        assert_eq!(
            spans(&[(1, 2, MATCH), (3, 4, CURRENT_MATCH)]),
            [
                ("Q".to_string(), plain),
                (family.to_string(), MATCH.patch(CURRENT_MATCH)),
                ("X".to_string(), plain),
            ]
        );
        // A combining mark stays with its letter too.
        let accented = marked_spans("e\u{301}x".into(), plain, &[(0, 1, MATCH)]);
        assert_eq!(accented[0].content, "e\u{301}");
    }

    #[test]
    fn the_current_match_keeps_its_style_when_a_later_match_shares_its_glyph() {
        let text = "e\u{301}\u{301}x".to_string();
        let spans = marked_spans(text, Style::new(), &[(1, 2, CURRENT_MATCH), (2, 3, MATCH)]);
        assert_eq!(spans[0].content, "e\u{301}\u{301}");
        assert_eq!(spans[0].style, CURRENT_MATCH);
        assert_eq!(spans[0].style.bg, CURRENT_MATCH.bg);
        let bold_underlined = Modifier::BOLD | Modifier::UNDERLINED;
        assert!(spans[0].style.add_modifier.contains(bold_underlined));
        assert_eq!(spans[1].content, "x");
    }

    #[test]
    fn a_marked_clip_drops_the_marks_from_its_ellipsis() {
        let spans = |line: &str, width, marks: &[(usize, usize, Style)]| -> Vec<(String, Style)> {
            clipped_marked(line, width, Style::new(), marks)
                .into_iter()
                .map(|span| (span.content.into_owned(), span.style))
                .collect()
        };
        let plain = Style::new();
        // A match across the cut stops before the `…`; one past it is gone.
        assert_eq!(
            spans("abcdefghij", 5, &[(2, 8, MATCH), (8, 9, CURRENT_MATCH)]),
            [
                ("ab".to_string(), plain),
                ("cd".to_string(), MATCH),
                ("…".to_string(), plain),
            ]
        );
        // A match only on the cut cell is dropped.
        assert_eq!(
            spans("abcdefghij", 5, &[(4, 5, MATCH)]),
            [("abcd…".to_string(), plain)]
        );
        // Uncut text keeps its marks, a `…` of its own included.
        assert_eq!(
            spans("abc…", 5, &[(3, 4, MATCH)]),
            [("abc".to_string(), plain), ("…".to_string(), MATCH)]
        );
    }
}
