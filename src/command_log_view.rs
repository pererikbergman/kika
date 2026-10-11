//! Draws the Command log popup. Reads state only.

use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::command_log::{CommandLog, CommandLogView, LogEntry};
use crate::git::recording::CommandOutcome;
use crate::view::clip;

/// Wide enough for `not started`, so the times line up.
const STATE_WIDTH: usize = 11;
/// Wide enough for `999ms` and `99.9s`.
const ELAPSED_WIDTH: usize = 5;

/// The whole of `area`, since command lines are long: one row per command,
/// oldest at the top, scrolled so the cursor row is in view. The cursor
/// row is reversed while the log is on top, and failures are red.
pub fn render(log: &CommandLog, view: &CommandLogView, top: bool, frame: &mut Frame, area: Rect) {
    frame.render_widget(Clear, area);
    let room = usize::from(area.width.saturating_sub(2));
    let shown = usize::from(area.height.saturating_sub(2)).max(1);
    let entries = log.entries();
    let mut title = " Command log ".to_string();
    let lines: Vec<Line> = if log.is_empty() {
        vec![Line::raw(" No git commands yet.")]
    } else {
        let cursor = view.cursor.min(entries.len() - 1);
        let first = cursor.saturating_sub(shown - 1);
        if shown < entries.len() {
            title = format!(" Command log {}/{} ", cursor + 1, entries.len());
        }
        entries
            .iter()
            .enumerate()
            .skip(first)
            .take(shown)
            .map(|(i, entry)| {
                let mut style = if failed(entry) {
                    Style::new().fg(Color::Red)
                } else {
                    Style::new()
                };
                if top && i == cursor {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                Line::styled(clip(&row(entry), 0, room), style)
            })
            .collect()
    };
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(title)),
        area,
    );
}

/// ` <state> <elapsed> [terminal] <line>`, the columns padded to line up.
fn row(entry: &LogEntry) -> String {
    let marker = if entry.attached { "[terminal] " } else { "" };
    let elapsed = entry.elapsed.map(elapsed).unwrap_or_default();
    format!(
        " {:<STATE_WIDTH$} {elapsed:>ELAPSED_WIDTH$} {marker}{}",
        state(entry),
        entry.line
    )
}

fn state(entry: &LogEntry) -> String {
    match &entry.outcome {
        None => "running".into(),
        Some(CommandOutcome::Ok) => "ok".into(),
        Some(CommandOutcome::Failed(None)) => "failed".into(),
        Some(CommandOutcome::Failed(Some(exit))) => format!("failed {exit}"),
        Some(CommandOutcome::NotStarted) => "not started".into(),
        Some(CommandOutcome::Stopped) => "stopped".into(),
    }
}

/// A stopped command was given up on by Kika, which is not a failure.
fn failed(entry: &LogEntry) -> bool {
    matches!(
        entry.outcome,
        Some(CommandOutcome::Failed(_) | CommandOutcome::NotStarted)
    )
}

/// `12ms` under a second, `1.4s` above.
fn elapsed(time: Duration) -> String {
    if time < Duration::from_secs(1) {
        format!("{}ms", time.as_millis())
    } else {
        format!("{:.1}s", time.as_secs_f64())
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::git::recording::CommandEvent;
    use crate::git::runner::GitCommand;
    use crate::intent::AppIntent;
    use crate::model::{AppState, Context};
    use crate::view::{buffer_rows, render as render_app};

    fn draw(state: &AppState, width: u16, height: u16) -> (ratatui::buffer::Buffer, Vec<String>) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render_app(state, frame)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let rows = buffer_rows(&buffer);
        (buffer, rows)
    }

    fn log(state: &mut AppState, event: CommandEvent) {
        state.update(AppIntent::CommandLogged(event));
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
        state.update(AppIntent::OpenCommandLog);
    }

    #[test]
    fn rows_show_state_time_marker_and_a_redacted_line_and_update_live() {
        let mut state = AppState::new(Context::Workspace);
        let url = GitCommand::new("remote")
            .arg("add")
            .arg("--")
            .arg("o")
            .arg("https://u:tok@h/r");
        let url_line = crate::git::command_line::display_line(&url);
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
            state.update(AppIntent::CommandLogPrev);
        }
        let (_, rows) = draw(&state, 60, 12);
        line_with(&rows, " Command log 1/30 ");
        line_with(&rows, "git cmd00");
    }

    #[test]
    fn times_read_in_milliseconds_then_seconds() {
        assert_eq!(elapsed(Duration::from_micros(400)), "0ms");
        assert_eq!(elapsed(Duration::from_millis(12)), "12ms");
        assert_eq!(elapsed(Duration::from_millis(999)), "999ms");
        assert_eq!(elapsed(Duration::from_millis(1400)), "1.4s");
        assert_eq!(elapsed(Duration::from_secs(75)), "75.0s");
    }

    #[test]
    fn every_outcome_has_a_state_word() {
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
            assert_eq!(state(&entry), word);
            assert_eq!(failed(&entry), failure, "{word}");
        }
    }
}
