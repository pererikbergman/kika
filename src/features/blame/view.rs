//! Draws the blame and the commit opened from it, in the Diff pane's place.
//! Reads state only.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};

use super::state::{BlameView, CommitView};
use crate::diff_pane_view::{clipped_marked, found_note, marks_of, styled_diff_line};
use crate::view::{PanelLook, elide_right, pad, reversed, short_hash};
use kika_git::blame::BlameLine;

/// Columns the author takes in a blame row; longer names are elided.
const BLAME_AUTHOR: usize = 16;

/// The blame in the Diff pane's place: per line its commit, author, day
/// and number, then the text. The cursor row is reversed, so it reads
/// without colour. A search marks its matches in the text.
pub(crate) fn render_blame(view: &BlameView, look: &PanelLook, frame: &mut Frame, area: Rect) {
    let found = found_note(view.search.as_ref(), false);
    let block = look.block(look.path_title("Blame", view.path.display(), &found));
    let inner = block.inner(area);
    let (rows, width) = (usize::from(inner.height), usize::from(inner.width));
    let pane = if let Some(error) = &view.error {
        Paragraph::new(error.lines().map(Line::from).collect::<Vec<_>>()).wrap(Wrap { trim: false })
    } else if !view.loaded {
        Paragraph::new("Loading…")
    } else if view.lines.is_empty() {
        Paragraph::new("Empty file")
    } else {
        let number_width = view
            .lines
            .iter()
            .map(|l| l.line_no)
            .max()
            .unwrap_or(0)
            .to_string()
            .len();
        let lines = view
            .lines
            .iter()
            .enumerate()
            .skip(view.scroll)
            .take(rows)
            .map(|(i, line)| {
                let row = blame_row(line, number_width);
                // Matches are in the text, after the drawn columns.
                let prefix = row.chars().count() - line.text.chars().count();
                let marks: Vec<_> = marks_of(&look.palette, &view.search, i)
                    .into_iter()
                    .map(|(start, end, style)| (start + prefix, end + prefix, style))
                    .collect();
                let line = Line::from(clipped_marked(&row, width, Style::new(), &marks));
                if i == view.cursor {
                    reversed(line, width)
                } else {
                    line
                }
            })
            .collect::<Vec<_>>();
        Paragraph::new(lines)
    };
    frame.render_widget(pane.block(block), area);
}

/// `<oid> <author> <date> <line no> <text>`, columns aligned. A line not
/// committed yet has no author or day, so it says so across both.
fn blame_row(line: &BlameLine, number_width: usize) -> String {
    let who = if line.is_uncommitted() {
        pad("Not committed yet", BLAME_AUTHOR + 1 + "YYYY-MM-DD".len())
    } else {
        format!(
            "{} {}",
            pad(&elide_right(&line.author, BLAME_AUTHOR), BLAME_AUTHOR),
            line.date
        )
    };
    format!(
        "{} {who} {:>number_width$} {}",
        short_hash(&line.oid),
        line.line_no,
        line.text
    )
}

/// A commit opened from the blame, in the same place: its message and
/// diff, coloured as the Diff pane colours them.
pub(crate) fn render_commit_view(
    view: &CommitView,
    look: &PanelLook,
    frame: &mut Frame,
    area: Rect,
) {
    let found = found_note(view.search.as_ref(), false);
    let block = look.block(look.title(&format!("Commit {}{found}", short_hash(&view.oid))));
    let inner = block.inner(area);
    let (rows, width) = (usize::from(inner.height), usize::from(inner.width));
    let pane = if let Some(error) = &view.error {
        Paragraph::new(error.lines().map(Line::from).collect::<Vec<_>>()).wrap(Wrap { trim: false })
    } else if !view.loaded {
        Paragraph::new("Loading…")
    } else {
        let start = view.scroll.min(view.lines.len());
        let end = start.saturating_add(rows).min(view.lines.len());
        Paragraph::new(
            view.lines[start..end]
                .iter()
                .zip(start..)
                .map(|(line, at)| {
                    let marks = marks_of(&look.palette, &view.search, at);
                    styled_diff_line(&look.palette, line, width, &marks)
                })
                .collect::<Vec<_>>(),
        )
    };
    frame.render_widget(pane.block(block), area);
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Modifier};

    use super::*;
    use crate::model::Focus;
    use crate::test_support::OID;
    use crate::test_support::screen::{
        assert_no_split_cluster, blame_line, blamed, contains, diff_line, draw, draw_buffer, file,
        find, loaded,
    };
    use crate::view::buffer_rows;
    use kika_git::blame::{BlameLine, Blamed, parse_blame};
    use kika_git::diff::{EmojiZwj, LineKind};

    /// Porcelain blame output that must parse to text lines.
    fn parsed(out: &str, emoji: EmojiZwj) -> Vec<BlameLine> {
        match parse_blame(out.as_bytes(), emoji) {
            Ok(Blamed::Lines(lines)) => lines,
            other => panic!("expected text lines, got {other:?}"),
        }
    }

    fn reversed_at(buffer: &ratatui::buffer::Buffer, needle: &str) -> bool {
        let (x, y) = find(buffer, needle);
        buffer[(x, y)]
            .style()
            .add_modifier
            .contains(Modifier::REVERSED)
    }

    #[test]
    fn a_loaded_blame_shows_each_lines_commit_author_date_and_text() {
        let zero = "0".repeat(40);
        let mut lines: Vec<BlameLine> = (1..=9)
            .map(|i| blame_line(OID, "Ann", i, &format!("text {i}")))
            .collect();
        lines.push(blame_line(&zero, "Not Committed Yet", 10, "edited"));
        lines[1].author = "Bartholomew Longname-Smith".into();
        let state = blamed(lines, 1);
        let buffer = draw_buffer(&state, 120, 20);
        let rows = buffer_rows(&buffer);
        assert!(contains(&rows, " Blame a.rs "), "{rows:#?}");
        assert!(
            contains(&rows, "abcdef1 Ann              2024-01-02  1 text 1"),
            "{rows:#?}"
        );
        assert!(
            contains(&rows, "abcdef1 Bartholomew Lon… 2024-01-02  2 text 2"),
            "{rows:#?}"
        );
        assert!(
            contains(&rows, "0000000 Not committed yet           10 edited"),
            "{rows:#?}"
        );
        assert!(reversed_at(&buffer, "abcdef1 Bartholomew"));
        assert!(!reversed_at(
            &buffer,
            "abcdef1 Ann              2024-01-02  1"
        ));
        // The whole cursor row is highlighted, past its text.
        let (x, y) = find(&buffer, "abcdef1 Bartholomew");
        assert!(
            buffer[(x + 60, y)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn a_blamed_word_keeps_its_zwnj() {
        let out = format!("{OID} 1 1 1\nauthor Ann\n\tمی\u{200C}خواهمX\n");
        let lines = parsed(&out, EmojiZwj::Escape);
        let buffer = draw_buffer(&blamed(lines, 0), 120, 20);
        let rows = buffer_rows(&buffer);
        assert!(contains(&rows, "می\u{200C}خواهمX"), "{rows:#?}");
        assert!(!contains(&rows, "<U+200C>"), "{rows:#?}");
        // Seven letters, one column each; the ZWNJ takes none.
        let (x, y) = find(&buffer, "م");
        assert_eq!(buffer[(x + 7, y)].symbol(), "X");
    }

    #[test]
    fn with_keep_a_blamed_emoji_sequence_is_one_cell_and_the_border_holds() {
        let family = "👨\u{200D}👩\u{200D}👧";
        let out = format!("{OID} 1 1 1\nauthor Ann\n\t{family}X\n");
        let lines = parsed(&out, EmojiZwj::Keep);
        let buffer = draw_buffer(&blamed(lines, 0), 120, 20);
        let area = buffer.area;
        let (x, y) = (0..area.height)
            .flat_map(|y| (0..area.width).map(move |x| (x, y)))
            .find(|&(x, y)| buffer[(x, y)].symbol() == family)
            .expect("the sequence in one cell");
        assert_eq!(buffer[(x + 2, y)].symbol(), "X");
        assert_eq!(buffer[(119, y)].symbol(), "┃");

        let long = format!("می\u{200C}خواهم क्\u{200D}ष {family} ").repeat(8);
        let out = format!("{OID} 1 1 1\nauthor Ann\n\t{long}\n");
        let lines = parsed(&out, EmojiZwj::Keep);
        let state = blamed(lines, 0);
        for width in 30..=60 {
            let buffer = draw_buffer(&state, width, 12);
            let (_, y) = find(&buffer, "abcdef1 Ann");
            assert_eq!(buffer[(width - 1, y)].symbol(), "┃", "{width}");
            assert_no_split_cluster(&buffer, family, &width.to_string());
        }
    }

    #[test]
    fn only_the_scrolled_window_of_a_blame_is_drawn() {
        let lines = (1..=30)
            .map(|i| blame_line(OID, "Ann", i, &format!("text {i}")))
            .collect();
        let mut state = blamed(lines, 25);
        if let Focus::Blame(view) = &mut state.focus {
            view.scroll = 20;
        }
        let rows = draw(&state, 100, 10);
        assert!(contains(&rows, "21 text 21"), "{rows:#?}");
        assert!(!contains(&rows, "20 text 20"), "{rows:#?}");
    }

    #[test]
    fn a_loading_or_failed_blame_says_so() {
        let mut state = loaded(vec![file("a.rs")]);
        state.focus = Focus::Blame(BlameView::new("a.rs".into(), 0));
        let rows = draw(&state, 100, 10);
        assert!(contains(&rows, " Blame a.rs "), "{rows:#?}");
        assert!(contains(&rows, "Loading…"), "{rows:#?}");

        let mut view = BlameView::new("a.rs".into(), 0);
        view.loaded(Err("fatal: no such path 'a.rs' in HEAD".into()));
        state.focus = Focus::Blame(view);
        let rows = draw(&state, 100, 10);
        assert!(contains(&rows, "fatal: no such path"), "{rows:#?}");
    }

    #[test]
    fn the_commit_view_shows_its_header_and_coloured_diff() {
        use LineKind::*;
        let mut state = blamed(vec![blame_line(OID, "Ann", 1, "x")], 0);
        let mut commit = CommitView::new(OID.into(), 1);
        commit.loaded(Ok(vec![
            diff_line(Header, &format!("commit {OID}")),
            diff_line(Header, "Author: Ann <ann@a>"),
            diff_line(Header, "    Why it changed"),
            diff_line(Header, "diff --git a/a.rs b/a.rs"),
            diff_line(Hunk, "@@ -1 +1 @@"),
            diff_line(Removed, "-old"),
            diff_line(Added, "+new"),
        ]));
        if let Focus::Blame(view) = &mut state.focus {
            view.commit = Some(commit);
        }
        let buffer = draw_buffer(&state, 100, 14);
        let rows = buffer_rows(&buffer);
        assert!(contains(&rows, " Commit abcdef1 "), "{rows:#?}");
        assert!(!contains(&rows, " Blame "), "{rows:#?}");
        assert!(contains(&rows, "Author: Ann <ann@a>"), "{rows:#?}");
        assert!(contains(&rows, "    Why it changed"), "{rows:#?}");
        let style = |needle| {
            let (x, y) = find(&buffer, needle);
            buffer[(x, y)].style()
        };
        assert_eq!(style("-old").fg, Some(Color::Red));
        assert_eq!(style("+new").fg, Some(Color::Green));
        assert!(style("Author:").add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn a_loading_or_failed_commit_says_so() {
        let mut state = blamed(vec![blame_line(OID, "Ann", 1, "x")], 0);
        if let Focus::Blame(view) = &mut state.focus {
            view.commit = Some(CommitView::new(OID.into(), 1));
        }
        let rows = draw(&state, 100, 10);
        assert!(contains(&rows, "Loading…"), "{rows:#?}");

        let mut commit = CommitView::new(OID.into(), 1);
        commit.loaded(Err("fatal: bad object".into()));
        if let Focus::Blame(view) = &mut state.focus {
            view.commit = Some(commit);
        }
        let rows = draw(&state, 100, 10);
        assert!(contains(&rows, "fatal: bad object"), "{rows:#?}");
    }

    #[test]
    fn tiny_terminals_do_not_panic_with_a_blame_or_commit() {
        let lines = vec![blame_line(OID, "Ann", 1, "x")];
        let mut state = blamed(lines, 0);
        for (w, h) in [(0, 0), (1, 1), (5, 3), (20, 4)] {
            draw(&state, w, h);
        }
        if let Focus::Blame(view) = &mut state.focus {
            view.commit = Some(CommitView::new(OID.into(), 1));
        }
        for (w, h) in [(0, 0), (1, 1), (5, 3), (20, 4)] {
            draw(&state, w, h);
        }
    }
}
