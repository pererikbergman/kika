//! The pending review on screen: the rows picked and the pending comments
//! marked on the files screen's diff, the composer box a comment is typed
//! in, and the list of pending comments. Reads state only; every comment
//! text and path is made terminal-safe here, as it is drawn.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Padding, Paragraph};

use super::comment_editor::{CommentEditor, Composer, ComposerKind};
use super::comment_pick::{CommentPick, live_pick};
use super::files::{PrFiles, ReviewView};
use super::pending::{CommentTarget, PendingSlot, lines_label};
use super::state::ReviewState;
use super::submit::comments_label;
use super::threads_view::{CommentMarks, clip_line, dim};
use crate::model::AppState;
use crate::view::{centered, elide_left, width_of};
use kika_forge::anchor::{RowNumber, row_in, to_line_id};
use kika_forge::discussion::PrDiscussion;
use kika_forge::pending::PendingComment;
use kika_git::safe_text::sanitize;

/// A discussion with no threads, for marking comments on a diff whose
/// threads were not read.
pub fn no_discussion() -> PrDiscussion {
    PrDiscussion {
        head_oid: None,
        threads: Vec::new(),
        threads_total: 0,
        conversation: Vec::new(),
        conversation_total: 0,
        conversation_capped: false,
    }
}

/// The pending comments on the selected file, in the whole pull
/// request's view, whose lines GitHub's numbers name.
fn file_comments<'a>(review: &'a ReviewState, files: &PrFiles) -> Vec<&'a PendingComment> {
    let path = files
        .selected()
        .and_then(|row| std::str::from_utf8(row.file.path.as_bytes()).ok());
    let comments = review
        .pending
        .as_ref()
        .and_then(PendingSlot::review)
        .map(|r| r.comments.as_slice())
        .unwrap_or_default();
    match path {
        Some(path) if files.view == ReviewView::Whole => {
            comments.iter().filter(|c| c.path == path).collect()
        }
        _ => Vec::new(),
    }
}

/// Whether the diff has a pick or a pending comment to mark.
pub fn has_comment_marks(state: &AppState, files: &PrFiles) -> bool {
    live_pick(state).is_some() || !file_comments(&state.review, files).is_empty()
}

/// The marks on the shown diff, whose rows are numbered `numbers`.
pub fn comment_marks(state: &AppState, files: &PrFiles, numbers: &[RowNumber]) -> CommentMarks {
    let mut marks = CommentMarks {
        picked: live_pick(state).map(CommentPick::span),
        ..CommentMarks::default()
    };
    for comment in file_comments(&state.review, files) {
        if let Some(row) = row_in(numbers, to_line_id(comment.end)) {
            let first = comment.body.lines().next().unwrap_or_default();
            marks.under.entry(row).or_default().push(first.to_string());
        }
    }
    marks
}

/// The smallest box drawn, when the panel has room for it.
const MIN_WIDTH: u16 = 24;
const MIN_HEIGHT: u16 = 6;

/// A box over the files screen in `area`: 80% of its width and 60% of
/// its height, centred.
fn box_area(area: Rect) -> Rect {
    let width = (u32::from(area.width) * 8 / 10) as u16;
    let height = (u32::from(area.height) * 6 / 10) as u16;
    centered(area, width.max(MIN_WIDTH), height.max(MIN_HEIGHT))
}

fn frame_block(title: String) -> Block<'static> {
    Block::bordered()
        .padding(Padding::horizontal(1))
        .title(title)
}

/// Draw the pending list, then the composer over it, whichever is open.
pub fn render_pending(review: &ReviewState, frame: &mut Frame, area: Rect) {
    if let (Some(cursor), Some(slot)) = (review.pending_list, &review.pending) {
        render_list(slot, cursor, frame, area);
    }
    if let Some(composer) = &review.composer {
        let comments = review.pending_count();
        render_composer(composer, comments, review.composer_posting(), frame, area);
    }
}

/// `path:R10-R14` of a line comment, made safe to draw.
fn where_label(target: &CommentTarget) -> String {
    let path = sanitize(target.path.as_bytes());
    format!("{path}:{}", lines_label(target.start, target.end))
}

/// The cue on a composer whose text goes to GitHub, longest first: the
/// first that fits is drawn.
const POSTS_CUES: [&str; 3] = [
    " · ctrl-s posts to GitHub ",
    " ctrl-s posts to GitHub ",
    " posts to GitHub ",
];
const POSTING_CUES: [&str; 2] = [" · posting… ", " posting… "];
/// The fewest columns a reply's label is cut to before it is dropped.
const MIN_LABEL: usize = 8;

/// The composer's title for a box `room` columns inside: what is written
/// and where it goes; a review's summary also counts the `comments` it
/// carries. For a post the cue that it goes to GitHub, or that
/// it is on its way, is kept whole; the label gives way first.
fn composer_title(composer: &Composer, comments: usize, posting: bool, room: usize) -> String {
    let head = match &composer.kind {
        ComposerKind::Line {
            target,
            editing: Some(_),
        } => return format!(" Edit comment on {} ", where_label(target)),
        ComposerKind::Line {
            target,
            editing: None,
        } => return format!(" Comment on {} ", where_label(target)),
        ComposerKind::Reply { label, .. } => {
            let label = sanitize(label.as_bytes());
            let fixed = width_of(" Reply to ") + width_of(POSTS_CUES[0]);
            let cut = room.saturating_sub(fixed);
            if cut < MIN_LABEL.min(width_of(&label)) {
                String::new()
            } else {
                format!(" Reply to {}", elide_left(&label, cut))
            }
        }
        ComposerKind::General { number, .. } => format!(" Comment on #{number}"),
        ComposerKind::Review { number, event } => format!(
            " Review summary: {} #{number} ({})",
            event.label(),
            comments_label(comments)
        ),
    };
    let cues: &[&str] = if posting { &POSTING_CUES } else { &POSTS_CUES };
    let (with_head, alone) = cues
        .split_first()
        .map_or((None, cues), |(f, r)| (Some(*f), r));
    if let Some(cue) = with_head.filter(|_| !head.is_empty()) {
        let title = format!("{head}{cue}");
        if width_of(&title) <= room {
            return title;
        }
    }
    let fits = alone.iter().find(|cue| width_of(cue) <= room);
    fits.or(alone.last())
        .copied()
        .unwrap_or_default()
        .to_string()
}

fn render_composer(
    composer: &Composer,
    comments: usize,
    posting: bool,
    frame: &mut Frame,
    area: Rect,
) {
    let popup = box_area(area);
    let room = usize::from(popup.width).saturating_sub(2);
    let block = frame_block(clip_title(
        &composer_title(composer, comments, posting, room),
        popup.width,
    ));
    let inner = block.inner(popup);
    let lines = editor_lines(
        &composer.text,
        usize::from(inner.width),
        usize::from(inner.height),
    );
    frame.render_widget(Clear, popup);
    frame.render_widget(Paragraph::new(lines).block(block), popup);
}

/// A title cut to fit a box `width` wide.
fn clip_title(title: &str, width: u16) -> String {
    let line = clip_line(
        Line::raw(title.to_string()),
        usize::from(width).saturating_sub(2),
    );
    line.spans.into_iter().map(|s| s.content).collect()
}

/// The editor's text wrapped at `width` columns, with the cursor's cell
/// reversed, scrolled so the cursor's row is among the last `rows`.
fn editor_lines(editor: &CommentEditor, width: usize, rows: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut cursor_row = 0;
    for (r, text) in editor.lines.iter().enumerate() {
        // Each char drawn safe on its own, so the cursor's column holds.
        let mut cells: Vec<(String, bool)> = text
            .chars()
            .enumerate()
            .map(|(c, ch)| {
                (
                    sanitize(ch.to_string().as_bytes()),
                    r == editor.row && c == editor.col,
                )
            })
            .collect();
        if r == editor.row && editor.col >= cells.len() {
            cells.push((" ".into(), true));
        }
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut used = 0;
        for (cell, here) in cells {
            let w = width_of(&cell);
            if used + w > width && used > 0 {
                out.push(Line::from(std::mem::take(&mut spans)));
                used = 0;
            }
            if here {
                cursor_row = out.len();
            }
            let style = if here {
                Style::new().add_modifier(Modifier::REVERSED)
            } else {
                Style::new()
            };
            spans.push(Span::styled(cell, style));
            used += w;
        }
        out.push(Line::from(spans));
    }
    let start = (cursor_row + 1).saturating_sub(rows.max(1));
    out.into_iter().skip(start).take(rows).collect()
}

fn render_list(slot: &PendingSlot, cursor: usize, frame: &mut Frame, area: Rect) {
    let comments = slot
        .review()
        .map(|r| r.comments.as_slice())
        .unwrap_or_default();
    let popup = box_area(area);
    let count = match comments.len() {
        1 => "1 comment".to_string(),
        n => format!("{n} comments"),
    };
    // Borders and no padding rows: the rows the list has.
    let rows = usize::from(popup.height.saturating_sub(2)).max(1);
    // Where the cursor is, when the list runs past the box.
    let at = if comments.len() > rows {
        format!(" · {}/{}", cursor + 1, comments.len())
    } else {
        String::new()
    };
    let block = frame_block(clip_title(
        &format!(" Pending review #{} · {count}{at} ", slot.number),
        popup.width,
    ));
    let inner = block.inner(popup);
    let width = usize::from(inner.width);
    let start = (cursor + 1).saturating_sub(rows);
    let lines: Vec<Line<'static>> = comments
        .iter()
        .enumerate()
        .skip(start)
        .take(rows)
        .map(|(i, c)| list_row(c, i == cursor, width))
        .collect();
    frame.render_widget(Clear, popup);
    frame.render_widget(Paragraph::new(lines).block(block), popup);
}

/// `path:L12  first line`, cut to `width`, reversed under the cursor.
fn list_row(comment: &PendingComment, cursor: bool, width: usize) -> Line<'static> {
    let place = format!(
        "{}:{}",
        sanitize(comment.path.as_bytes()),
        lines_label(comment.start, comment.end)
    );
    let first = sanitize(comment.body.lines().next().unwrap_or_default().as_bytes());
    let mut line = Line::from(vec![
        Span::styled(place, Style::new().fg(Color::Cyan)),
        Span::raw("  "),
        Span::styled(first, if cursor { Style::new() } else { dim() }),
    ]);
    if cursor {
        line = line.style(Style::new().add_modifier(Modifier::REVERSED));
    }
    clip_line(line, width)
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    use super::*;
    use crate::features::review::pending::tests::{
        comment, commenting, right, type_text, with_review,
    };
    use crate::features::review::state::tests::{press, type_char};
    use crate::model::AppState;
    use crate::view::buffer_rows;
    use kika_forge::pending::PendingReview;

    fn draw(state: &AppState) -> Buffer {
        draw_at(state, 100, 30)
    }

    fn draw_at(state: &AppState, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| crate::view::render(state, frame))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn row_with(rows: &[String], text: &str) -> usize {
        rows.iter()
            .position(|r| r.contains(text))
            .unwrap_or_else(|| panic!("{text:?} not in {rows:#?}"))
    }

    /// Whether the cell where `text` starts on row `y` is drawn reversed.
    fn reversed_at(buffer: &Buffer, rows: &[String], text: &str) -> bool {
        let y = row_with(rows, text);
        let col = rows[y][..rows[y].find(text).unwrap()].chars().count();
        let cell = &buffer[(col as u16, y as u16)];
        cell.modifier.contains(Modifier::REVERSED)
    }

    #[test]
    fn the_rows_picked_are_drawn_reversed() {
        let mut state = commenting();
        type_char(&mut state, 'a');
        type_char(&mut state, 'v');
        type_char(&mut state, 'j');
        let buffer = draw(&state);
        let rows = buffer_rows(&buffer);
        assert!(reversed_at(&buffer, &rows, "+fn new() {}"));
        assert!(reversed_at(&buffer, &rows, " use std::fmt;"));
        assert!(!reversed_at(&buffer, &rows, "-fn old() {}"));
    }

    #[test]
    fn a_pending_comment_shows_under_its_line_and_the_title_counts_it() {
        let state = with_review(PendingReview {
            comments: vec![comment(1, right(1), "hi\nmore")],
        });
        let rows = buffer_rows(&draw(&state));
        let line = row_with(&rows, "+fn new() {}");
        assert!(rows[line + 1].contains("✎ pending: hi"), "{rows:#?}");
        assert!(!rows.iter().any(|r| r.contains("more")), "{rows:#?}");
        assert!(rows.iter().any(|r| r.contains("· 1 pending")), "{rows:#?}");
        // None pending: no count.
        let rows = buffer_rows(&draw(&commenting()));
        assert!(!rows.iter().any(|r| r.contains("pending")), "{rows:#?}");
    }

    #[test]
    fn hostile_text_is_drawn_without_its_escapes() {
        let mut hostile = comment(1, right(1), "\u{1b}[31mred\u{7}");
        hostile.path = "src/cache.rs".into();
        let mut state = with_review(PendingReview {
            comments: vec![hostile.clone()],
        });
        let rows = buffer_rows(&draw(&state));
        assert!(
            rows.iter()
                .all(|r| !r.contains('\u{1b}') && !r.contains('\u{7}'))
        );
        assert!(
            rows.iter()
                .any(|r| r.contains("✎ pending: ") && r.contains("[31mred"))
        );
        // The list draws it safe too, and so does the composer.
        type_char(&mut state, 'P');
        let rows = buffer_rows(&draw(&state));
        assert!(rows.iter().all(|r| !r.contains('\u{1b}')), "{rows:#?}");
        type_char(&mut state, 'e');
        let rows = buffer_rows(&draw(&state));
        assert!(rows.iter().all(|r| !r.contains('\u{1b}')), "{rows:#?}");
    }

    #[test]
    fn the_composer_and_the_list_draw_their_text() {
        let mut state = commenting();
        type_char(&mut state, 'a');
        press(&mut state, KeyCode::Enter);
        type_text(&mut state, "looks good");
        press(&mut state, KeyCode::Enter);
        type_text(&mut state, "q?");
        let rows = buffer_rows(&draw(&state));
        assert!(
            rows.iter()
                .any(|r| r.contains("Comment on src/cache.rs:R1")),
            "{rows:#?}"
        );
        assert!(rows.iter().any(|r| r.contains("looks good")), "{rows:#?}");
        assert!(rows.iter().any(|r| r.contains("q?")), "{rows:#?}");
        assert!(
            rows.iter().any(|r| r.contains("q and ? are typed")),
            "{rows:#?}"
        );

        let range = kika_forge::pending::PendingComment {
            start: Some(right(1)),
            ..comment(2, right(3), "a range")
        };
        let mut state = with_review(PendingReview {
            comments: vec![comment(1, right(1), "first line\nsecond"), range],
        });
        type_char(&mut state, 'P');
        let rows = buffer_rows(&draw(&state));
        assert!(
            rows.iter()
                .any(|r| r.contains("Pending review #12 · 2 comments")),
            "{rows:#?}"
        );
        assert!(
            rows.iter()
                .any(|r| r.contains("src/cache.rs:R1  first line")),
            "{rows:#?}"
        );
        assert!(
            rows.iter()
                .any(|r| r.contains("src/cache.rs:R1-R3  a range")),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_long_line_wraps_and_the_cursor_row_stays_in_the_box() {
        let mut editor = CommentEditor::default();
        for c in "abcdefghij".chars() {
            editor.edit(crate::keymap::KeyPress::new(
                KeyCode::Char(c),
                crossterm::event::KeyModifiers::NONE,
            ));
        }
        let lines = editor_lines(&editor, 4, 2);
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(text, ["efgh", "ij "]);
    }

    /// The composer with a long, wide and multi-line body, the list of
    /// several comments, and a pick: each drawn at every size.
    fn each_popup() -> Vec<(&'static str, AppState)> {
        let long = format!("{} 日本語の長い行 😀\tend\nsecond", "word ".repeat(40));
        let mut composer = commenting();
        type_char(&mut composer, 'a');
        press(&mut composer, KeyCode::Enter);
        for c in long.chars() {
            let key = match c {
                '\n' => KeyCode::Enter,
                '\t' => KeyCode::Tab,
                c => KeyCode::Char(c),
            };
            press(&mut composer, key);
        }
        let mut list = with_review(PendingReview {
            comments: (1..=12).map(|id| comment(id, right(1), &long)).collect(),
        });
        type_char(&mut list, 'P');
        let mut pick = commenting();
        type_char(&mut pick, 'a');
        type_char(&mut pick, 'v');
        type_char(&mut pick, 'j');
        vec![("composer", composer), ("list", list), ("pick", pick)]
    }

    #[test]
    fn the_popups_and_the_pick_draw_at_80x24_and_on_tiny_terminals() {
        for (what, mut state) in each_popup() {
            for (width, height) in [(80, 24), (40, 12), (24, 8), (10, 4), (3, 2), (1, 1), (5, 0)] {
                state.size = (width, height);
                let buffer = draw_at(&state, width, height);
                let rows = buffer_rows(&buffer);
                assert!(
                    rows.iter().all(|r| width_of(r) <= usize::from(width)),
                    "{what} {width}x{height}: {rows:#?}"
                );
            }
            // At 80x24 each one still shows what it is about.
            state.size = (80, 24);
            let rows = buffer_rows(&draw_at(&state, 80, 24));
            let want = match what {
                "composer" => "second",
                "list" => "Pending review #12 · 12 comments · 1/12",
                _ => "+fn new() {}",
            };
            assert!(rows.iter().any(|r| r.contains(want)), "{what}: {rows:#?}");
        }
        // On a small panel the composer still has a row of text.
        let (_, mut composer) = each_popup().remove(0);
        composer.size = (40, 12);
        let rows = buffer_rows(&draw_at(&composer, 40, 12));
        assert!(rows.iter().any(|r| r.contains("second")), "{rows:#?}");
    }

    #[test]
    fn the_list_says_where_the_cursor_is_once_it_runs_past_the_box() {
        let (_, mut list) = each_popup().remove(1);
        list.size = (80, 24);
        for _ in 0..11 {
            type_char(&mut list, 'j');
        }
        let rows = buffer_rows(&draw_at(&list, 80, 24));
        assert!(
            rows.iter().any(|r| r.contains("12 comments · 12/12")),
            "{rows:#?}"
        );
        // A list that fits says only its count.
        let mut short = with_review(PendingReview {
            comments: vec![comment(1, right(1), "one")],
        });
        type_char(&mut short, 'P');
        let rows = buffer_rows(&draw(&short));
        assert!(
            rows.iter()
                .any(|r| r.contains("Pending review #12 · 1 comment ")),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_search_match_inside_the_pick_stays_marked() {
        let mut state = commenting();
        state.diff.set_search("fmt");
        type_char(&mut state, 'a');
        type_char(&mut state, 'v');
        type_char(&mut state, 'j');
        let buffer = draw(&state);
        let rows = buffer_rows(&buffer);
        let y = row_with(&rows, " use std::fmt;");
        let col = rows[y][..rows[y].find("fmt;").unwrap()].chars().count();
        let cell = &buffer[(col as u16, y as u16)];
        // Reversed for the pick, and still bold and underlined for the match.
        assert!(cell.modifier.contains(Modifier::REVERSED));
        assert!(
            cell.modifier
                .contains(Modifier::BOLD | Modifier::UNDERLINED)
        );
    }

    #[test]
    fn wide_characters_and_tabs_wrap_with_the_cursor_on_its_cell() {
        let text = |lines: &[Line<'static>]| -> Vec<String> {
            lines
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect()
        };
        let mut editor = CommentEditor::from_text("a😀b");
        // A wide char that would cross the edge goes to the next row.
        assert_eq!(text(&editor_lines(&editor, 2, 5)), ["a", "😀", "b "]);
        assert_eq!(text(&editor_lines(&editor, 3, 5)), ["a😀", "b "]);
        // The cursor on the wide char reverses that cell.
        editor.edit(crate::keymap::KeyPress::new(
            KeyCode::Left,
            crossterm::event::KeyModifiers::NONE,
        ));
        editor.edit(crate::keymap::KeyPress::new(
            KeyCode::Left,
            crossterm::event::KeyModifiers::NONE,
        ));
        let lines = editor_lines(&editor, 3, 5);
        let cursor: Vec<&str> = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .filter(|s| s.style.add_modifier.contains(Modifier::REVERSED))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(cursor, ["😀"]);
        // A tab is drawn as spaces, its cell the cursor's when on it.
        let tab = CommentEditor::from_text("\tx");
        assert_eq!(text(&editor_lines(&tab, 10, 5)), ["    x "]);
        assert_eq!(text(&editor_lines(&tab, 4, 5)), ["    ", "x "]);
    }
}
