//! Draws the Commits panel's Reflog tab: one row per entry, newest first.
//! Every message is repository data, so it is drawn only through its
//! sanitised display.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use super::git::ReflogEntry;
use super::list::{ReflogList, short_id};
use crate::view::{elide_right, no_matches, reversed, scrolled, width_of};

/// The tab's lines for an inside of `height` by `width`: a placeholder
/// until the first read, then the rows the filter shows around the cursor,
/// any error first.
pub(crate) fn reflog_lines(list: &ReflogList, height: usize, width: usize) -> Vec<Line<'static>> {
    let dim = Style::new().fg(Color::DarkGray);
    let mut lines: Vec<Line<'static>> = Vec::new();
    if let Some(error) = &list.error {
        // Already terminal-safe: git's stderr through `terminal_safe`.
        lines.extend(
            error
                .lines()
                .map(|l| Line::from(elide_right(&format!("! {l}"), width))),
        );
        lines.push(Line::from("Press r to retry"));
    }
    let Some(reflog) = &list.data else {
        if list.error.is_none() {
            lines.push(Line::from("Loading…"));
        }
        return lines;
    };
    if reflog.entries.is_empty() {
        lines.push(Line::styled("No reflog yet", dim));
        return lines;
    }
    let visible = list.visible();
    if visible.is_empty() {
        lines.push(no_matches());
        return lines;
    }
    let room = height.saturating_sub(lines.len()).max(1);
    lines.extend(
        scrolled(&visible, Some(list.selected), room)
            .iter()
            .map(|&i| {
                let line = entry_line(&reflog.entries[i], width);
                if i == list.selected {
                    reversed(line, width)
                } else {
                    line
                }
            }),
    );
    lines
}

/// `<short id> <label> <rest of the message>`, cut to `width`. An id that
/// is not hex shows as `?`.
fn entry_line(entry: &ReflogEntry, width: usize) -> Line<'static> {
    let short = short_id(entry);
    let label = entry.label();
    let display = entry.display();
    let rest = display
        .strip_prefix(&label)
        .map(|r| r.trim_start_matches(':').trim_start())
        .unwrap_or(&display);
    let short = elide_right(&short, width);
    let label = elide_right(&label, width.saturating_sub(width_of(&short) + 1));
    let used = width_of(&short) + 1 + width_of(&label) + 1;
    let rest = elide_right(rest, width.saturating_sub(used));
    Line::from(vec![
        Span::styled(short, Style::new().fg(Color::Yellow)),
        Span::raw(" "),
        Span::styled(label, Style::new().fg(Color::Cyan)),
        Span::raw(" "),
        Span::raw(rest),
    ])
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::style::Modifier;

    use crate::commits::CommitsTab;
    use crate::features::commits::reflog::git::{Reflog, parse_records};
    use crate::layout::{ScreenMode, SidePanel};
    use crate::model::{AppState, Context, Focus};
    use crate::view::{buffer_rows, render};

    fn showing(bytes: &[u8]) -> AppState {
        let mut state = AppState::new(Context::Workspace);
        state.files_loaded = true;
        state.focus = Focus::Commits;
        state.expanded = SidePanel::Commits;
        state.screen_mode = ScreenMode::Full;
        state.commits.tab = CommitsTab::Reflog;
        state.commits.reflog.data = Some(Reflog {
            entries: parse_records(bytes),
            ..Reflog::default()
        });
        state
    }

    fn draw(state: &AppState, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(state, frame)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn line_with<'a>(rows: &'a [String], needle: &str) -> &'a str {
        rows.iter()
            .find(|row| row.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} not on screen:\n{}", rows.join("\n")))
    }

    #[test]
    fn the_tab_lists_each_entry_with_its_id_label_and_message() {
        let state = showing(include_bytes!("../../../git/fixtures/reflog/checkout.bin"));
        let rows = buffer_rows(&draw(&state, 100, 20));
        line_with(&rows, "ccccccc checkout moving from main to side");
        line_with(&rows, "ccccccc commit second");
        line_with(&rows, "bbbbbbb commit first");
        line_with(&rows, "aaaaaaa commit (initial) root");
        line_with(&rows, "Commits · Log [Reflog]");
    }

    #[test]
    fn the_selected_row_is_reversed_and_follows_the_cursor() {
        let mut state = showing(include_bytes!("../../../git/fixtures/reflog/commit.bin"));
        state.commits.reflog.selected = 1;
        let buffer = draw(&state, 100, 20);
        let rows = buffer_rows(&buffer);
        let y = rows
            .iter()
            .position(|r| r.contains("commit first"))
            .unwrap();
        let x = rows[y].find("bbbbbbb").unwrap();
        assert!(
            buffer[(x as u16, y as u16)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        let other = rows
            .iter()
            .position(|r| r.contains("commit second"))
            .unwrap();
        let x = rows[other].find("ccccccc").unwrap();
        assert!(
            !buffer[(x as u16, other as u16)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn a_hostile_message_draws_sanitised_and_a_bad_id_as_a_question_mark() {
        let mut bytes = b"zz 11 commit: \x1b[2J\r\xe2\x80\xaeevil\0".to_vec();
        bytes.extend_from_slice(include_bytes!("../../../git/fixtures/reflog/hostile.bin"));
        let state = showing(&bytes);
        let rows = buffer_rows(&draw(&state, 100, 20));
        line_with(&rows, "? commit \u{FFFD}[2J");
        line_with(&rows, "commit \u{FFFD}[2J clear screen");
        for row in &rows {
            assert!(
                !row.chars().any(|c| c.is_control() || c == '\u{202E}'),
                "{row:?}"
            );
        }
    }

    #[test]
    fn a_filter_shows_only_matching_rows_and_draws_sanitised_in_the_title() {
        let mut state = showing(include_bytes!("../../../git/fixtures/reflog/checkout.bin"));
        state.commits.reflog.set_filter("aaa".into());
        let rows = buffer_rows(&draw(&state, 100, 20));
        line_with(&rows, "aaaaaaa commit (initial) root");
        line_with(&rows, "Reflog · /aaa 1 of 4");
        assert!(!rows.iter().any(|r| r.contains("ccccccc")), "{rows:#?}");
        assert!(!rows.iter().any(|r| r.contains("[Reflog]")), "{rows:#?}");

        // A hostile filter matches nothing and draws made safe.
        state
            .commits
            .reflog
            .set_filter("x\x1b[2J\u{202E}\r\0".into());
        let rows = buffer_rows(&draw(&state, 100, 20));
        line_with(&rows, "No matches");
        line_with(&rows, "Reflog · /");
        line_with(&rows, " 0 of 4");
        for row in &rows {
            assert!(
                !row.chars().any(|c| c.is_control() || c == '\u{202E}'),
                "{row:?}"
            );
        }
        // With no filter the tabs come back.
        state.commits.reflog.set_filter(String::new());
        line_with(
            &buffer_rows(&draw(&state, 100, 20)),
            "Commits · Log [Reflog]",
        );
    }

    #[test]
    fn a_filter_scrolls_its_shown_rows_to_the_cursor() {
        // Twelve entries; the filter keeps the even ones.
        let bytes: Vec<u8> = (0..12)
            .flat_map(|i| {
                let word = if i % 2 == 0 { "keep" } else { "drop" };
                format!(
                    "{} {} commit: {word}{i:02}\0",
                    "a".repeat(40),
                    "b".repeat(40)
                )
                .into_bytes()
            })
            .collect();
        let mut state = showing(&bytes);
        state.commits.reflog.set_filter("keep".into());
        for _ in 0..5 {
            state.commits.reflog.move_by(1);
        }
        assert_eq!(state.commits.reflog.selected, 10);
        let rows = buffer_rows(&draw(&state, 60, 6));
        line_with(&rows, "keep10");
        assert!(!rows.iter().any(|r| r.contains("drop")), "{rows:#?}");
    }

    #[test]
    fn placeholders_say_loading_empty_or_the_error() {
        let mut state = showing(b"");
        state.commits.reflog.data = None;
        line_with(&buffer_rows(&draw(&state, 100, 20)), "Loading…");
        state.commits.reflog.data = Some(Reflog::default());
        line_with(&buffer_rows(&draw(&state, 100, 20)), "No reflog yet");
        state.commits.reflog.data = None;
        state.commits.reflog.error = Some("Could not read the reflog: fatal: x".into());
        let rows = buffer_rows(&draw(&state, 100, 20));
        line_with(&rows, "! Could not read the reflog: fatal: x");
        line_with(&rows, "Press r to retry");
    }

    #[test]
    fn small_and_tiny_screens_draw_without_panicking_and_cut_with_an_ellipsis() {
        let mut state = showing(include_bytes!("../../../git/fixtures/reflog/checkout.bin"));
        let rows = buffer_rows(&draw(&state, 80, 24));
        line_with(&rows, "ccccccc checkout moving from main to side");
        for (w, h) in [(20, 5), (1, 1), (3, 3)] {
            draw(&state, w, h);
        }
        state.commits.reflog.error =
            Some(format!("Could not read the reflog: {}", "x".repeat(300)));
        let rows = buffer_rows(&draw(&state, 80, 24));
        line_with(&rows, "! Could not read the reflog: xxx");
        assert!(rows.iter().any(|r| r.contains("x…")), "{rows:#?}");
    }

    #[test]
    fn the_title_marks_the_log_tab_when_it_is_shown() {
        let mut state = showing(b"");
        state.commits.tab = CommitsTab::Log;
        state.commits.loaded = true;
        line_with(
            &buffer_rows(&draw(&state, 100, 20)),
            "Commits · [Log] Reflog",
        );
    }
}
