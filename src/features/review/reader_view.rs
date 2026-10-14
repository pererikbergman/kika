//! The thread reader's box over the files screen: the lines `reader.rs`
//! rendered, from its scroll, each cut to the box. Reads state only;
//! GitHub's text was made terminal-safe when its answer was parsed.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, Padding, Paragraph};

use super::files::PrFiles;
use super::reader::{ThreadReader, reader_lines};
use super::threads::ThreadScope;
use super::threads_view::clip_line;
use crate::view::centered;
use kika_forge::discussion::PrDiscussion;

/// The smallest box drawn, when the panel has room for it.
const MIN_WIDTH: u16 = 20;
const MIN_HEIGHT: u16 = 6;

/// The box in the review panel's inside `area`: 90% of its width and 80%
/// of its height, centred.
fn reader_area(area: Rect) -> Rect {
    let width = (u32::from(area.width) * 9 / 10) as u16;
    let height = (u32::from(area.height) * 8 / 10) as u16;
    centered(area, width.max(MIN_WIDTH), height.max(MIN_HEIGHT))
}

/// The box's frame: a border and a column of room inside it.
fn frame_block() -> Block<'static> {
    Block::bordered().padding(Padding::horizontal(1))
}

/// The text inside the box, which the reader renders for.
pub fn reader_inner(area: Rect) -> Rect {
    frame_block().inner(reader_area(area))
}

/// Draw the open reader over the files screen in `area`.
pub fn render_reader(
    files: &PrFiles,
    discussion: Option<&PrDiscussion>,
    reader: &ThreadReader,
    frame: &mut Frame,
    area: Rect,
) {
    let popup = reader_area(area);
    let title = format!(" thread {}/{} ", reader.at + 1, reader.threads.len());
    let block = frame_block().title(title);
    let inner = block.inner(popup);
    let (width, rows) = (usize::from(inner.width), usize::from(inner.height));
    let fresh;
    let lines: &[Line<'static>] = match (&reader.rendered, discussion, reader.shown()) {
        (Some(drawn), _, _) if drawn.width == width => &drawn.lines,
        // Not rendered for this width yet: drawn here this once.
        (_, Some(discussion), Some(t)) => {
            let path = files
                .selected()
                .map(|row| row.file.path.display())
                .unwrap_or_default();
            fresh = reader_lines(&ThreadScope::of(files, discussion), t, path, width);
            &fresh
        }
        _ => &[],
    };
    let shown: Vec<Line<'static>> = lines
        .iter()
        .skip(reader.scroll)
        .take(rows)
        .map(|line| clip_line(line.clone(), width))
        .collect();
    frame.render_widget(Clear, popup);
    frame.render_widget(Paragraph::new(shown).block(block), popup);
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::features::review::reader::tests::{reader, two_files};
    use crate::features::review::state::tests::{press, type_char};
    use crate::features::review::threads::tests::{
        cache_diff, chunk, discussion_from, files_with, fixture_discussion,
    };
    use crate::features::review::threads_view::INLINE_BODY;
    use crate::model::AppState;
    use crate::view::{buffer_rows, width_of};
    use kika_forge::discussion::fixtures::hostile_discussion;
    use kika_forge::discussion::{PrDiscussion, ThreadComment};

    fn draw(state: &AppState, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| crate::view::render(state, frame))
            .unwrap();
        buffer_rows(terminal.backend().buffer())
    }

    fn row_with(rows: &[String], text: &str) -> usize {
        rows.iter()
            .position(|r| r.contains(text))
            .unwrap_or_else(|| panic!("{text:?} not in {rows:#?}"))
    }

    fn opened(discussion: PrDiscussion, file: usize) -> AppState {
        let mut state = files_with(&["src/cache.rs", "src/lib.rs"], discussion);
        state.size = (100, 30);
        for _ in 0..file {
            type_char(&mut state, 'j');
        }
        chunk(&mut state, cache_diff(), true);
        press(&mut state, KeyCode::Enter);
        state
    }

    #[test]
    fn a_resolved_thread_shows_every_comment_in_full() {
        let mut discussion = fixture_discussion();
        // cache.rs's resolved line 2 thread, with a reply.
        discussion.threads[1].comments.push(ThreadComment {
            author: "dave".into(),
            created: "2023-11-14T21:40:00Z".into(),
            body: "It was never called.\n\nSee the log.".into(),
        });
        discussion.threads[1].comments_total = 2;
        let state = opened(discussion, 0);
        assert_eq!(reader(&state).shown(), Some(1));
        let rows = draw(&state, 100, 30);
        let title = row_with(&rows, "thread 1/2");
        let header = row_with(&rows, "src/cache.rs · line 2 · resolved · 2 comments");
        assert!(header > title, "{rows:#?}");
        let carol = row_with(&rows, "carol · 2023-11-14 21:05");
        assert!(rows[carol + 1].contains("Why remove this?"), "{rows:#?}");
        let dave = row_with(&rows, "dave · 2023-11-14 21:40");
        assert!(rows[dave + 1].contains("It was never called."), "{rows:#?}");
        row_with(&rows, "See the log.");
    }

    #[test]
    fn a_long_comment_shows_in_full_without_a_cut() {
        let mut state = opened(fixture_discussion(), 0);
        type_char(&mut state, ']');
        assert_eq!(reader(&state).shown(), Some(0));
        let rows = draw(&state, 100, 30);
        row_with(&rows, "src/cache.rs · line 3 · unresolved · 2 comments");
        let alice = row_with(&rows, "alice · 2023-11-14 21:00");
        let eight = row_with(&rows, "eight");
        assert!(eight > alice + 1 + INLINE_BODY, "{rows:#?}");
        // Inside the box, which ends above the file screen's own cut mark.
        let inside: Vec<&String> = rows[alice..=eight].iter().collect();
        assert!(
            !inside.iter().any(|r| r.contains("more lines")),
            "{rows:#?}"
        );
        row_with(&rows, "bob · 2023-11-14 21:30");
    }

    #[test]
    fn an_outdated_thread_shows_where_it_was_and_its_lines() {
        let state = opened(fixture_discussion(), 1);
        assert_eq!(reader(&state).shown(), Some(3));
        let rows = draw(&state, 100, 30);
        let header = row_with(
            &rows,
            "src/lib.rs · outdated · was line 7 (new) · unresolved · 1 comment",
        );
        for (i, context) in [" context5", "-old6", "+new6", "+new7"].iter().enumerate() {
            assert!(
                rows[header + 1 + i].contains(&format!("│ {context}")),
                "{rows:#?}"
            );
        }
        row_with(&rows, "This name was wrong.");
    }

    #[test]
    fn a_thread_with_more_comments_than_read_says_how_many() {
        let mut discussion = fixture_discussion();
        let t = &mut discussion.threads[1];
        t.comments = (0..50)
            .map(|i| ThreadComment {
                author: format!("u{i}"),
                created: "2023-11-14T21:05:00Z".into(),
                body: "x".into(),
            })
            .collect();
        t.comments_total = 57;
        let mut state = opened(discussion, 0);
        for _ in 0..20 {
            press(&mut state, KeyCode::PageDown);
        }
        let rows = draw(&state, 100, 30);
        row_with(&rows, "7 more comments on GitHub");
    }

    #[test]
    fn scrolling_moves_the_text() {
        let mut state = two_files();
        state.size = (100, 16);
        type_char(&mut state, ']');
        press(&mut state, KeyCode::Enter);
        let rows = draw(&state, 100, 16);
        let header = row_with(&rows, "src/cache.rs · line 3");
        // The header and a blank line go off the top.
        for _ in 0..2 {
            type_char(&mut state, 'j');
        }
        let rows = draw(&state, 100, 16);
        assert!(
            !rows.iter().any(|r| r.contains("src/cache.rs · line 3")),
            "{rows:#?}"
        );
        assert!(
            rows[header].contains("alice · 2023-11-14 21:00"),
            "{rows:#?}"
        );
    }

    #[test]
    fn hostile_text_draws_escaped_and_the_border_holds_at_every_size() {
        let mut state = opened(discussion_from(&hostile_discussion()), 0);
        type_char(&mut state, ']');
        let rows = draw(&state, 100, 30);
        assert!(rows.iter().any(|r| r.contains("<U+202E>evil")), "{rows:#?}");
        for (w, h) in [(80u16, 24u16), (40, 10), (20, 6), (100, 30)] {
            state.size = (w, h);
            state.render_review_body();
            let rows = draw(&state, w, h);
            assert_eq!(rows.len(), usize::from(h));
            for row in &rows {
                assert!(width_of(row) <= usize::from(w), "{w}x{h}: {row:?}");
                assert!(!row.chars().any(char::is_control), "{row:?}");
            }
            // The box: a top border with its title, a bottom border, and
            // its two sides on every row between.
            let top = rows
                .iter()
                .position(|r| r.contains("┌ thread 2/2 "))
                .unwrap_or_else(|| panic!("{w}x{h}: {rows:#?}"));
            let top_row: Vec<char> = rows[top].chars().collect();
            let at = rows[top].find("┌ thread").unwrap();
            let left = rows[top][..at].chars().count();
            let right = left + top_row[left..].iter().position(|&c| c == '┐').unwrap();
            let bottom = (top + 1..rows.len())
                .find(|&y| rows[y].chars().nth(left) == Some('└'))
                .unwrap_or_else(|| panic!("{w}x{h}: {rows:#?}"));
            assert_eq!(rows[bottom].chars().nth(right), Some('┘'), "{rows:#?}");
            for row in &rows[top + 1..bottom] {
                let chars: Vec<char> = row.chars().collect();
                assert_eq!(chars.get(left), Some(&'│'), "{w}x{h}: {row:?}");
                assert_eq!(chars.get(right), Some(&'│'), "{w}x{h}: {row:?}");
            }
        }
    }
}
