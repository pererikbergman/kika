//! Draws the Commits panel. Only the rows in the visible window are turned
//! into lines, so a history of any length draws in the same time.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::commit_graph::{GraphCell, GraphRow};
use crate::commits::{CommitsPanel, Marker};
use crate::model::AppState;
use crate::view::{PanelLook, elide_right, reversed, width_of};

/// Authors longer than this lose their end, so the subject keeps room.
const AUTHOR_WIDTH: usize = 16;

/// The panel: a placeholder until the first page, then one row per commit
/// in the window around the cursor. Collapsed, the `HEAD` commit's row.
pub(crate) fn render_commits_panel(
    state: &AppState,
    look: &PanelLook,
    expanded: bool,
    frame: &mut Frame,
    area: Rect,
) {
    let panel = &state.commits;
    let block = look.block(look.title("Commits"));
    let inner = block.inner(area);
    let dim = Style::new().fg(Color::DarkGray);
    let paragraph = if panel.rows.is_empty() {
        match &panel.error {
            _ if !panel.loaded => Paragraph::new("Loading…"),
            Some(error) => Paragraph::new(error.lines().map(Line::from).collect::<Vec<_>>())
                .wrap(Wrap { trim: false }),
            None => Paragraph::new(Line::styled("No commits yet", dim)),
        }
    } else if !expanded {
        Paragraph::new(commit_lines(panel, 0..1, None, usize::from(inner.width)))
    } else {
        Paragraph::new(window(panel, inner))
    };
    frame.render_widget(paragraph.block(block), area);
}

/// The rows that fit in `area`, scrolled so the cursor shows, plus a last
/// row saying a page is loading or failed.
fn window(panel: &CommitsPanel, area: Rect) -> Vec<Line<'static>> {
    let height = usize::from(area.height);
    let width = usize::from(area.width);
    let note = if panel.more_in_flight {
        Some(Line::styled(
            "Loading more…",
            Style::new().fg(Color::DarkGray),
        ))
    } else {
        panel
            .error
            .as_deref()
            .map(|error| Line::styled(error.to_string(), Style::new().fg(Color::Red)))
    };
    let room = if note.is_some() && height > 1 {
        height - 1
    } else {
        height
    };
    let start = (panel.selected + 1).saturating_sub(room);
    let end = (start + room).min(panel.rows.len());
    let mut lines = commit_lines(panel, start..end, Some(panel.selected), width);
    if end == panel.rows.len() {
        lines.extend(note);
    }
    lines
}

/// Lines for `rows[range]` only; the graph is padded to the widest of them.
fn commit_lines(
    panel: &CommitsPanel,
    range: std::ops::Range<usize>,
    selected: Option<usize>,
    width: usize,
) -> Vec<Line<'static>> {
    let graphs = &panel.graph[range.clone()];
    let lanes = graphs.iter().map(|g| g.cells.len()).max().unwrap_or(0);
    range
        .zip(graphs)
        .map(|(i, graph)| {
            let line = commit_line(panel, i, graph, lanes, width);
            if Some(i) == selected {
                reversed(line, width)
            } else {
                line
            }
        })
        .collect()
}

/// `<graph> <marker> <hash> <date> <author> <subject>`, the subject cut to fit.
fn commit_line(
    panel: &CommitsPanel,
    index: usize,
    graph: &GraphRow,
    lanes: usize,
    width: usize,
) -> Line<'static> {
    let row = &panel.rows[index];
    let marker = match panel.marker(&row.oid) {
        Marker::Unpushed => Span::styled("↑", Style::new().fg(Color::Yellow)),
        Marker::Merged => Span::styled("✓", Style::new().fg(Color::DarkGray)),
        Marker::Pushed | Marker::Unknown => Span::raw(" "),
    };
    let author = elide_right(&row.author, AUTHOR_WIDTH);
    let mut spans = vec![
        Span::styled(graph_text(graph, lanes), Style::new().fg(Color::Magenta)),
        marker,
        Span::raw(" "),
        Span::styled(row.short_oid.clone(), Style::new().fg(Color::Yellow)),
        Span::raw(" "),
        Span::styled(
            format!("{:>3}", relative_date(row.time, panel.loaded_at)),
            Style::new().fg(Color::DarkGray),
        ),
        Span::raw(" "),
        Span::styled(author, Style::new().fg(Color::Green)),
        Span::raw(" "),
    ];
    let used: usize = spans.iter().map(|s| width_of(&s.content)).sum();
    let subject = elide_right(&row.subject, width.saturating_sub(used));
    spans.push(Span::raw(subject));
    Line::from(spans)
}

/// Two columns per lane: the lane's glyph, then what joins it to the next.
/// A merge or branch-off draws a line from the commit to the lanes it touches.
fn graph_text(graph: &GraphRow, lanes: usize) -> String {
    let cells = &graph.cells;
    let commit = cells.iter().position(|c| *c == GraphCell::Commit);
    let joins = || {
        cells
            .iter()
            .enumerate()
            .filter(|(_, c)| matches!(c, GraphCell::MergeIn | GraphCell::BranchOut))
            .map(|(i, _)| i)
    };
    let (lo, hi) = match commit {
        Some(c) => (
            joins().chain([c]).min().unwrap_or(c),
            joins().chain([c]).max().unwrap_or(c),
        ),
        None => (0, 0),
    };
    let commit = commit.unwrap_or(0);
    let mut text = String::new();
    for i in 0..lanes {
        let glyph = match cells.get(i).copied().unwrap_or(GraphCell::Empty) {
            GraphCell::Commit => '●',
            GraphCell::Pipe => '│',
            GraphCell::MergeIn if i > commit => '╮',
            GraphCell::MergeIn => '╭',
            GraphCell::BranchOut if i > commit => '╯',
            GraphCell::BranchOut => '╰',
            GraphCell::Empty if lo < i && i < hi => '─',
            GraphCell::Empty => ' ',
        };
        text.push(glyph);
        text.push(if lo <= i && i < hi { '─' } else { ' ' });
    }
    text
}

/// How long ago `time` was at `now`, in the largest whole unit: `now`,
/// `5m`, `3h`, `2d`, `4w`, `7mo`, `2y`.
pub(crate) fn relative_date(time: i64, now: i64) -> String {
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    const WEEK: i64 = 7 * DAY;
    const MONTH: i64 = 30 * DAY;
    const YEAR: i64 = 365 * DAY;
    let ago = now.saturating_sub(time);
    match ago {
        _ if ago < MINUTE => "now".to_string(),
        _ if ago < HOUR => format!("{}m", ago / MINUTE),
        _ if ago < DAY => format!("{}h", ago / HOUR),
        _ if ago < WEEK => format!("{}d", ago / DAY),
        _ if ago < MONTH => format!("{}w", ago / WEEK),
        _ if ago < YEAR => format!("{}mo", ago / MONTH),
        _ => format!("{}y", ago / YEAR),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::{Duration, Instant};

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::style::Modifier;

    use super::*;
    use crate::commit_graph::GraphBuilder;
    use crate::git::log::{CommitRow, Markers};
    use crate::layout::{ScreenMode, SidePanel};
    use crate::model::{Context, Focus};
    use crate::view::{buffer_rows, render};

    const NOW: i64 = 1_700_000_000;

    fn row(oid: &str, parents: &[&str], author: &str, ago: i64, subject: &str) -> CommitRow {
        CommitRow {
            oid: oid.into(),
            short_oid: oid[..7.min(oid.len())].into(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
            author: author.into(),
            time: NOW - ago,
            subject: subject.into(),
        }
    }

    /// A workspace with the Commits panel focused, showing `rows`.
    fn showing(rows: Vec<CommitRow>) -> AppState {
        let mut state = AppState::new(Context::Workspace);
        state.files_loaded = true;
        state.focus = Focus::Commits;
        state.expanded = SidePanel::Commits;
        // The panel takes the screen, so whole rows show.
        state.screen_mode = ScreenMode::Full;
        let mut builder = GraphBuilder::default();
        state.commits.graph = rows.iter().map(|r| builder.push(r)).collect();
        state.commits.builder = builder;
        state.commits.rows = rows;
        state.commits.loaded = true;
        state.commits.done = true;
        state.commits.loaded_at = NOW;
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

    fn merge_rows() -> Vec<CommitRow> {
        vec![
            row(
                "m000000aaa",
                &["b000000aaa", "s000000aaa"],
                "Ann",
                30,
                "Merge side",
            ),
            row("s000000aaa", &["a000000aaa"], "Bob", 3 * 3600, "Side work"),
            row("b000000aaa", &["a000000aaa"], "Ann", 2 * 86400, "Main work"),
            row("a000000aaa", &[], "Ann", 3 * 365 * 86400, "Root"),
        ]
    }

    #[test]
    fn a_merge_draws_its_graph_hash_author_date_and_subject() {
        let rows = buffer_rows(&draw(&showing(merge_rows()), 100, 30));

        assert!(line_with(&rows, "Merge side").contains("●─╮   m000000 now Ann Merge side"));
        assert!(line_with(&rows, "Side work").contains("│ ●   s000000  3h Bob Side work"));
        assert!(line_with(&rows, "Main work").contains("● │   b000000  2d Ann Main work"));
        assert!(line_with(&rows, "Root").contains("●─╯   a000000  3y Ann Root"));
    }

    #[test]
    fn unpushed_and_merged_commits_carry_their_markers() {
        let mut state = showing(merge_rows());
        let set = |oids: &[&str]| -> Option<HashSet<String>> {
            Some(oids.iter().map(|s| s.to_string()).collect())
        };
        state.commits.markers = Markers {
            main: Some("refs/heads/main".into()),
            unmerged: set(&["m000000aaa", "s000000aaa"]),
            unpushed: set(&["m000000aaa"]),
        };
        let rows = buffer_rows(&draw(&state, 100, 30));

        assert!(line_with(&rows, "Merge side").contains("↑ m000000"));
        assert!(line_with(&rows, "Side work").contains("  s000000"));
        assert!(line_with(&rows, "Main work").contains("✓ b000000"));
        assert!(line_with(&rows, "Root").contains("✓ a000000"));
    }

    #[test]
    fn the_selected_row_is_highlighted() {
        let mut state = showing(merge_rows());
        state.commits.selected = 2;
        let buffer = draw(&state, 100, 30);
        let rows = buffer_rows(&buffer);
        let y = rows.iter().position(|r| r.contains("Main work")).unwrap();
        let x = rows[y].find("b000000").unwrap();
        let x = rows[y][..x].chars().count() as u16;
        let style = buffer[(x, y as u16)].style();
        assert!(style.add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn a_hundred_thousand_rows_draw_the_window_around_the_cursor_quickly() {
        let rows: Vec<CommitRow> = (0..100_000)
            .map(|i| {
                let parent = format!("{:010}", i + 1);
                let parents: &[&str] = if i + 1 < 100_000 { &[&parent] } else { &[] };
                row(
                    &format!("{i:010}"),
                    parents,
                    "Ann",
                    60,
                    &format!("subject {i}"),
                )
            })
            .collect();
        let mut state = showing(rows);
        state.commits.selected = 99_990;

        let started = Instant::now();
        let buffer = draw(&state, 100, 30);
        let took = started.elapsed();

        let rows = buffer_rows(&buffer);
        line_with(&rows, "subject 99990");
        assert!(!rows.iter().any(|r| r.contains("subject 0 ")));
        assert!(took < Duration::from_millis(500), "took {took:?}");
    }

    #[test]
    fn collapsed_the_panel_shows_the_head_commit() {
        let mut state = showing(merge_rows());
        state.focus = Focus::Files;
        state.expanded = SidePanel::Files;
        state.screen_mode = ScreenMode::Normal;
        let rows = buffer_rows(&draw(&state, 100, 30));
        line_with(&rows, "m000000 now Ann Merge");
        assert!(!rows.iter().any(|r| r.contains("Side work")));
    }

    #[test]
    fn placeholders_say_loading_empty_or_the_error() {
        let mut state = showing(vec![]);
        state.commits.loaded = false;
        line_with(&buffer_rows(&draw(&state, 100, 30)), "Loading…");
        state.commits.loaded = true;
        line_with(&buffer_rows(&draw(&state, 100, 30)), "No commits yet");
        state.commits.error = Some("fatal: bad thing".into());
        line_with(&buffer_rows(&draw(&state, 100, 30)), "fatal: bad thing");
    }

    #[test]
    fn a_page_in_flight_adds_a_loading_more_row_after_the_last_commit() {
        let mut state = showing(merge_rows());
        state.commits.done = false;
        state.commits.more_in_flight = true;
        let rows = buffer_rows(&draw(&state, 100, 30));
        let root = rows.iter().position(|r| r.contains("Root")).unwrap();
        assert!(rows[root + 1].contains("Loading more…"), "{rows:#?}");
    }

    #[test]
    fn relative_dates_use_the_largest_whole_unit() {
        let at = |ago| relative_date(NOW - ago, NOW);
        assert_eq!(at(0), "now");
        assert_eq!(at(59), "now");
        assert_eq!(at(-30), "now");
        assert_eq!(at(5 * 60), "5m");
        assert_eq!(at(3 * 3600 + 59), "3h");
        assert_eq!(at(2 * 86400), "2d");
        assert_eq!(at(4 * 7 * 86400), "4w");
        assert_eq!(at(7 * 30 * 86400), "7mo");
        assert_eq!(at(2 * 365 * 86400), "2y");
    }
}
