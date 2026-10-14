//! Draws the Command log popup. Reads state only.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, Paragraph};

use super::log::{CommandLog, CommandLogView, LogEntry, row_text};
use crate::view::{clip, elide_right, pad, width_of};
use kika_git::recording::CommandOutcome;
use kika_git::safe_text::sanitize;

/// The whole of `area`, since command lines are long: one row per shown
/// command, oldest at the top, scrolled so the cursor row is in view. The
/// cursor row is reversed while the log is on top and its row is shown,
/// and failures are red.
pub fn render(log: &CommandLog, view: &CommandLogView, top: bool, frame: &mut Frame, area: Rect) {
    frame.render_widget(Clear, area);
    let room = usize::from(area.width.saturating_sub(2));
    let shown = usize::from(area.height.saturating_sub(2)).max(1);
    let entries = log.entries();
    let visible = view.visible(log);
    let hidden = view.selected_hidden(log);
    // Where the cursor sits among the shown rows. A hidden one scrolls
    // from the top; one past the end, as in an empty log, is on the last.
    let at = match visible.iter().position(|&i| i == view.cursor) {
        Some(at) => at,
        None if hidden => 0,
        None => visible.len().saturating_sub(1),
    };
    let place = (!hidden && !visible.is_empty()).then_some(at);
    let title = title(&view.filter, visible.len(), log.len(), place, shown, room);
    let lines: Vec<Line> = if log.is_empty() {
        vec![Line::raw(" No git commands yet.")]
    } else if visible.is_empty() {
        vec![Line::raw(" No matches ")]
    } else {
        let first = at.saturating_sub(shown - 1);
        visible
            .iter()
            .skip(first)
            .take(shown)
            .map(|&i| {
                let entry = &entries[i];
                let mut style = if failed(entry) {
                    Style::new().fg(Color::Red)
                } else {
                    Style::new()
                };
                if top && !hidden && i == visible[at] {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                // Padded, so the cursor's reversed bar fills the row.
                Line::styled(pad(&clip(&row_text(entry), 0, room), room), style)
            })
            .collect()
    };
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(title)),
        area,
    );
}

/// ` Command log `, or with a filter ` Command log · /<filter> <n> of
/// <total> `. Rows cut by a short screen add where the cursor (`at`, among
/// the shown rows) is in them. The filter is drawn sanitised and loses its
/// end so the title fits `room` columns.
fn title(
    filter: &str,
    matched: usize,
    total: usize,
    at: Option<usize>,
    shown: usize,
    room: usize,
) -> String {
    let place = match at {
        Some(at) if shown < matched => Some(format!("{}/{matched}", at + 1)),
        _ => None,
    };
    if filter.is_empty() {
        return match place {
            Some(place) => format!(" Command log {place} "),
            None => " Command log ".to_string(),
        };
    }
    let place = place.map(|p| format!(" · {p}")).unwrap_or_default();
    let fixed = format!(" Command log · / {matched} of {total}{place} ");
    let query = elide_right(
        &sanitize(filter.as_bytes()),
        room.saturating_sub(width_of(&fixed)),
    );
    format!(" Command log · /{query} {matched} of {total}{place} ")
}

/// A stopped command was given up on by Kika, which is not a failure.
fn failed(entry: &LogEntry) -> bool {
    matches!(
        entry.outcome,
        Some(CommandOutcome::Failed(_) | CommandOutcome::NotStarted)
    )
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use std::time::Duration;

    use super::*;
    use crate::features::command_log::CommandLogIntent;
    use crate::intent::AppIntent;
    use crate::model::{AppState, Context};
    use crate::popup::Popup;
    use crate::view::{buffer_rows, render as render_app};
    use kika_git::recording::CommandEvent;
    use kika_git::runner::GitCommand;

    fn draw(state: &AppState, width: u16, height: u16) -> (ratatui::buffer::Buffer, Vec<String>) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render_app(state, frame)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let rows = buffer_rows(&buffer);
        (buffer, rows)
    }

    fn log(state: &mut AppState, event: CommandEvent) {
        state.update(AppIntent::CommandLog(CommandLogIntent::Logged(event)));
    }

    fn started(id: u64, line: &str, attached: bool) -> CommandEvent {
        CommandEvent::Started {
            id,
            line: line.into(),
            attached,
        }
    }

    fn finished(id: u64, outcome: CommandOutcome, ms: u64) -> CommandEvent {
        CommandEvent::Finished {
            id,
            outcome,
            elapsed: Duration::from_millis(ms),
        }
    }

    /// The row holding `needle`, and its index.
    fn line_with<'a>(rows: &'a [String], needle: &str) -> (usize, &'a str) {
        rows.iter()
            .enumerate()
            .find(|(_, row)| row.contains(needle))
            .map(|(y, row)| (y, row.as_str()))
            .unwrap_or_else(|| panic!("{needle:?} not on screen: {rows:#?}"))
    }

    fn open(state: &mut AppState) {
        state.update(AppIntent::CommandLog(CommandLogIntent::Open));
    }

    #[test]
    fn rows_show_state_time_marker_and_a_redacted_line_and_update_live() {
        let mut state = AppState::new(Context::Workspace);
        let url = GitCommand::new("remote")
            .arg("add")
            .arg("--")
            .arg("o")
            .arg("https://u:tok@h/r");
        let url_line = kika_git::command_line::display_line(&url);
        log(&mut state, started(0, "git status", false));
        log(&mut state, finished(0, CommandOutcome::Ok, 12));
        log(&mut state, started(1, "git commit -F …/msg", true));
        log(
            &mut state,
            finished(1, CommandOutcome::Failed(Some("status 1".into())), 1400),
        );
        log(&mut state, started(2, &url_line, false));
        log(&mut state, finished(2, CommandOutcome::NotStarted, 0));
        log(&mut state, started(3, "git fetch --all", false));
        open(&mut state);

        let (buffer, rows) = draw(&state, 100, 20);
        line_with(&rows, " Command log ");
        let (_, ok) = line_with(&rows, "git status");
        assert!(ok.contains(" ok           12ms git status"), "{ok}");
        let (y, attached) = line_with(&rows, "git commit");
        assert!(
            attached.contains(" failed status 1  1.4s [terminal] git commit -F …/msg"),
            "{attached}"
        );
        let x = attached.find("failed").unwrap();
        let x = attached[..x].chars().count() as u16;
        assert_eq!(buffer[(x, y as u16)].style().fg, Some(Color::Red));
        let (_, redacted) = line_with(&rows, "git remote add");
        assert!(
            redacted.contains(" not started   0ms git remote add -- o https://h/r"),
            "{redacted}"
        );
        let (y, running) = line_with(&rows, "git fetch");
        assert!(
            running.contains(" running           git fetch --all"),
            "{running}"
        );
        let x = running.find("running").unwrap();
        let x = running[..x].chars().count() as u16;
        assert!(
            buffer[(x, y as u16)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED),
            "the newest row has the cursor"
        );
        assert!(rows.iter().all(|row| !row.contains("tok")), "{rows:#?}");

        // A command started while open shows at the bottom, with the cursor.
        log(&mut state, started(4, "git diff", false));
        let (buffer, rows) = draw(&state, 100, 20);
        let (fetch_y, _) = line_with(&rows, "git fetch");
        let (diff_y, diff) = line_with(&rows, "git diff");
        assert_eq!(diff_y, fetch_y + 1);
        let x = diff[..diff.find("running").unwrap()].chars().count() as u16;
        assert!(
            buffer[(x, diff_y as u16)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn an_empty_log_says_so() {
        let mut state = AppState::new(Context::Workspace);
        open(&mut state);
        let (_, rows) = draw(&state, 80, 12);
        line_with(&rows, " Command log ");
        line_with(&rows, "No git commands yet.");
    }

    #[test]
    fn a_long_log_scrolls_to_the_cursor_and_says_where_it_is() {
        let mut state = AppState::new(Context::Workspace);
        for id in 0..30 {
            log(&mut state, started(id, &format!("git cmd{id:02}"), false));
        }
        open(&mut state);
        let (_, rows) = draw(&state, 60, 12);
        line_with(&rows, " Command log 30/30 ");
        line_with(&rows, "git cmd29");
        assert!(rows.iter().all(|row| !row.contains("git cmd00")));

        for _ in 0..29 {
            state.update(AppIntent::CommandLog(CommandLogIntent::Prev));
        }
        let (_, rows) = draw(&state, 60, 12);
        line_with(&rows, " Command log 1/30 ");
        line_with(&rows, "git cmd00");
    }

    fn reversed(buffer: &ratatui::buffer::Buffer, x: u16, y: usize) -> bool {
        buffer[(x, y as u16)]
            .style()
            .add_modifier
            .contains(Modifier::REVERSED)
    }

    /// The log open with `n` short commands, the last one the cursor's.
    fn short_log(n: u64) -> AppState {
        let mut state = AppState::new(Context::Workspace);
        for id in 0..n {
            log(&mut state, started(id, &format!("git cmd{id}"), false));
            log(&mut state, finished(id, CommandOutcome::Ok, 1));
        }
        open(&mut state);
        state
    }

    #[test]
    fn the_cursor_bar_fills_the_row_and_other_rows_are_plain() {
        let state = short_log(3);
        let (buffer, rows) = draw(&state, 80, 24);
        let last_inner = 78;
        let (cursor_y, _) = line_with(&rows, "git cmd2");
        assert!(reversed(&buffer, 1, cursor_y));
        assert!(reversed(&buffer, last_inner, cursor_y), "{rows:#?}");
        for other in ["git cmd0", "git cmd1"] {
            let (y, _) = line_with(&rows, other);
            assert!(
                (1..=last_inner).all(|x| !reversed(&buffer, x, y)),
                "{other}"
            );
        }
    }

    #[test]
    fn a_log_under_another_popup_marks_no_row() {
        let state = short_log(3);
        let Some(Popup::CommandLog(view)) = state.popups.last() else {
            panic!("the log is open");
        };
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
        terminal
            .draw(|frame| render(&state.command_log, view, false, frame, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        for y in 0..10 {
            for x in 0..60 {
                assert!(!reversed(buffer, x, y), "({x}, {y})");
            }
        }
    }

    #[test]
    fn a_long_line_at_80_by_24_keeps_its_columns_and_the_border() {
        let mut state = AppState::new(Context::Workspace);
        let long = format!("git add -- {}", "x".repeat(200));
        log(&mut state, started(0, &long, false));
        log(&mut state, finished(0, CommandOutcome::Ok, 12));
        open(&mut state);
        let (_, rows) = draw(&state, 80, 24);
        let (_, row) = line_with(&rows, "git add");
        assert!(
            row.starts_with("│ ok           12ms git add -- xxx"),
            "{row}"
        );
        assert!(row.ends_with("x…│"), "{row}");
        assert_eq!(row.chars().count(), 80, "{row}");
    }

    #[test]
    fn a_wide_glyph_at_the_edge_never_crosses_the_border() {
        // Each offset puts a two-column glyph across the right edge once.
        for offset in 0..2 {
            let mut state = AppState::new(Context::Workspace);
            let line = format!("git add -- {}{}", "a".repeat(offset), "漢字".repeat(40));
            log(&mut state, started(0, &line, false));
            open(&mut state);
            let (buffer, rows) = draw(&state, 80, 24);
            let (y, row) = line_with(&rows, "git add");
            assert_eq!(buffer[(79, y as u16)].symbol(), "│", "{row}");
            assert!(row.ends_with('│'), "{row}");
            assert!(crate::view::width_of(row) == 80, "{row}");
        }
    }

    #[test]
    fn a_tiny_terminal_draws_without_panicking() {
        for (width, height) in [(1, 1), (2, 2), (5, 3), (10, 2), (3, 10), (80, 1)] {
            for n in [0, 3] {
                let state = short_log(n);
                draw(&state, width, height);
            }
        }
    }

    /// [`short_log`] with `filter` set on the open log.
    fn filtered(n: u64, filter: &str) -> AppState {
        let mut state = short_log(n);
        let log = state.command_log.clone();
        if let Some(Popup::CommandLog(view)) = state.popups.last_mut() {
            view.filter = filter.into();
            view.reconcile(&log);
        }
        state
    }

    /// The text of the one reversed row, if any.
    fn reversed_row(buffer: &ratatui::buffer::Buffer, rows: &[String]) -> Option<String> {
        rows.iter()
            .enumerate()
            .find(|(y, _)| reversed(buffer, 1, *y))
            .map(|(_, row)| row.clone())
    }

    #[test]
    fn a_filter_draws_only_its_rows_and_counts_them_in_the_title() {
        let state = filtered(12, "cmd1");
        let (buffer, rows) = draw(&state, 80, 24);
        line_with(&rows, "┌ Command log · /cmd1 3 of 12 ─");
        for shown in ["git cmd1 ", "git cmd10", "git cmd11"] {
            line_with(&rows, shown);
        }
        for gone in ["git cmd0", "git cmd2", "git cmd9"] {
            assert!(!rows.iter().any(|r| r.contains(gone)), "{gone} drawn");
        }
        let cursor = reversed_row(&buffer, &rows).expect("a cursor row");
        assert!(cursor.contains("git cmd11"), "{cursor}");

        // The cursor bar sits on the filtered row it is on.
        let mut state = state;
        state.update(AppIntent::CommandLog(CommandLogIntent::Prev));
        let (buffer, rows) = draw(&state, 80, 24);
        let cursor = reversed_row(&buffer, &rows).expect("a cursor row");
        assert!(cursor.contains("git cmd10"), "{cursor}");
    }

    #[test]
    fn a_filtered_log_scrolls_among_its_shown_rows() {
        // `cmd1`, `cmd10`..`cmd19`: 11 rows in 6 lines.
        let mut state = filtered(20, "cmd1");
        for _ in 0..10 {
            state.update(AppIntent::CommandLog(CommandLogIntent::Prev));
        }
        let (buffer, rows) = draw(&state, 60, 8);
        line_with(&rows, " Command log · /cmd1 11 of 20 · 1/11 ");
        let cursor = reversed_row(&buffer, &rows).expect("a cursor row");
        assert!(cursor.contains("git cmd1 "), "{cursor}");
    }

    #[test]
    fn nothing_matching_says_so_and_marks_no_row() {
        let state = filtered(3, "zzz");
        let (buffer, rows) = draw(&state, 80, 24);
        line_with(&rows, " Command log · /zzz 0 of 3 ");
        line_with(&rows, " No matches ");
        assert_eq!(reversed_row(&buffer, &rows), None);
    }

    #[test]
    fn a_hostile_filter_is_drawn_escaped_in_the_title() {
        let state = filtered(3, "x\u{202E}y\x1b[31m");
        let (_, rows) = draw(&state, 80, 24);
        assert!(
            rows.iter()
                .all(|row| !row.contains('\x1b') && !row.contains('\u{202E}')),
            "{rows:#?}"
        );
        let (_, title) = line_with(&rows, " Command log · /x");
        assert!(title.contains("[31m"), "{title}");
        assert!(title.contains(" 0 of 3 "), "{title}");
    }

    #[test]
    fn a_long_filter_fits_the_title_inside_the_border() {
        let state = filtered(3, &"c".repeat(200));
        let (_, rows) = draw(&state, 40, 8);
        let (_, title) = line_with(&rows, " Command log · /");
        assert!(title.contains(" 0 of 3 "), "{title}");
        assert_eq!(crate::view::width_of(title), 40, "{title}");

        // Narrower than the fixed part alone: cut, the corners kept.
        let (buffer, rows) = draw(&state, 12, 8);
        let top = &rows[0];
        assert_eq!(crate::view::width_of(top), 12, "{top}");
        assert!(top.starts_with('┌') && top.ends_with('┐'), "{top}");
        assert!(top.contains("Comm"), "{top}");
        assert!(!top.contains("ccc"), "{top}");
        assert_eq!(buffer[(11, 1)].symbol(), "│");
    }

    #[test]
    fn a_filtered_log_past_the_screen_says_where_the_cursor_is() {
        let state = filtered(20, "cmd1");
        let (_, rows) = draw(&state, 60, 8);
        line_with(&rows, " Command log · /cmd1 11 of 20 · 11/11 ");
        // All rows fit: no place.
        let (_, rows) = draw(&state, 60, 24);
        line_with(&rows, " Command log · /cmd1 11 of 20 ─");
        // Nothing shown: no place either.
        let state = filtered(40, "zzz");
        let (_, rows) = draw(&state, 60, 8);
        line_with(&rows, " Command log · /zzz 0 of 40 ─");
    }

    #[test]
    fn a_filtered_log_on_a_tiny_terminal_draws_without_panicking() {
        let long = "c".repeat(200);
        for filter in ["cmd1", "zzz", "x\u{202E}\x1b[31m", long.as_str()] {
            let state = filtered(12, filter);
            for (width, height) in [(1, 1), (2, 2), (5, 3), (10, 2), (3, 10), (80, 1), (80, 24)] {
                let (_, rows) = draw(&state, width, height);
                for row in &rows {
                    assert_eq!(crate::view::width_of(row), usize::from(width), "{row}");
                    assert!(!row.contains('\x1b') && !row.contains('\u{202E}'), "{row}");
                }
            }
        }
    }

    #[test]
    fn the_bar_draws_the_filter_as_it_is_typed_and_sanitised() {
        use crossterm::event::{KeyCode, KeyModifiers};

        use crate::keymap::{KeyPress, KeyScope};
        use crate::list_filter::{InputTarget, ListInput};
        use crate::popup::LineInput;

        let mut state = short_log(12);
        state.update(AppIntent::Key(KeyPress::new(
            KeyCode::Char('/'),
            KeyModifiers::NONE,
        )));
        assert_eq!(state.key_focus(), KeyScope::Input);
        for c in "cmd1".chars() {
            state.update(AppIntent::Key(KeyPress::new(
                KeyCode::Char(c),
                KeyModifiers::NONE,
            )));
        }
        let (_, rows) = draw(&state, 80, 24);
        let bar = rows.last().unwrap();
        assert!(bar.contains(" /cmd1"), "the bar draws the input: {bar}");
        line_with(&rows, " Command log · /cmd1 3 of 12 ");

        // Text put in the input any other way is still drawn safely.
        state.input = Some(ListInput {
            target: InputTarget::CommandLogFilter,
            line: LineInput::with_text("y\x1b[2J\u{202E}\x07"),
        });
        let (_, rows) = draw(&state, 80, 24);
        assert!(
            rows.iter().all(|row| !row.contains('\x1b')
                && !row.contains('\x07')
                && !row.contains('\u{202E}')),
            "{rows:#?}"
        );
        let bar = rows.last().unwrap();
        assert!(bar.contains(" /y"), "{bar}");
        assert!(bar.contains("[2J"), "sanitised: {bar}");
    }

    #[test]
    fn failed_and_unstarted_commands_are_failures() {
        let entry = |outcome| LogEntry {
            id: 0,
            line: "git x".into(),
            attached: false,
            outcome,
            elapsed: None,
        };
        let cases = [
            (None, "running", false),
            (Some(CommandOutcome::Ok), "ok", false),
            (Some(CommandOutcome::Failed(None)), "failed", true),
            (
                Some(CommandOutcome::Failed(Some("signal 2".into()))),
                "failed signal 2",
                true,
            ),
            (Some(CommandOutcome::NotStarted), "not started", true),
            (Some(CommandOutcome::Stopped), "stopped", false),
        ];
        for (outcome, word, failure) in cases {
            let entry = entry(outcome);
            assert_eq!(failed(&entry), failure, "{word}");
        }
    }
}
