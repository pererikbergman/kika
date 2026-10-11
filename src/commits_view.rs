//! Draws the Commits panel. Only the rows in the visible window are turned
//! into lines, so a history of any length draws in the same time.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::commit_files::CommitFilesList;
use crate::commit_graph::{GraphCell, GraphRow};
use crate::commits::{CommitsPanel, Marker};
use crate::git::commit_files::CommitFile;
use crate::model::AppState;
use crate::view::{PanelLook, elide_right, reversed, width_of};

/// Authors longer than this lose their end, so the subject keeps room.
const AUTHOR_WIDTH: usize = 16;
/// Columns the subject keeps before the date and then the author give way.
const SUBJECT_MIN: usize = 12;

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
    if let Some(open) = panel.open.as_ref().filter(|_| expanded) {
        return render_commit_files(open, look, frame, area);
    }
    let block = look.block(look.title("Commits"));
    let inner = block.inner(area);
    let dim = Style::new().fg(Color::DarkGray);
    let paragraph = if panel.rows.is_empty() {
        match &panel.error {
            _ if !panel.loaded => Paragraph::new("Loading…"),
            Some(error) => {
                let mut lines: Vec<Line> = error.lines().map(Line::from).collect();
                if let Some(first) = lines.first_mut() {
                    first.spans.insert(0, Span::raw("! "));
                }
                lines.push(Line::from("Press r to retry"));
                Paragraph::new(lines).wrap(Wrap { trim: false })
            }
            None => Paragraph::new(Line::styled("No commits yet", dim)),
        }
    } else if !expanded {
        Paragraph::new(commit_lines(panel, 0..1, None, usize::from(inner.width)))
    } else {
        Paragraph::new(window(panel, inner))
    };
    frame.render_widget(paragraph.block(block), area);
}

/// A commit's files in place of the log: a placeholder until they load,
/// then one row per file, scrolled so the cursor shows.
fn render_commit_files(open: &CommitFilesList, look: &PanelLook, frame: &mut Frame, area: Rect) {
    let block = look.block(look.title(&format!("Commit {}", open.short_oid)));
    let inner = block.inner(area);
    let (height, width) = (usize::from(inner.height), usize::from(inner.width));
    let paragraph = match &open.error {
        _ if !open.loaded => Paragraph::new("Loading…"),
        Some(error) => {
            let mut lines: Vec<Line> = error.lines().map(Line::from).collect();
            if let Some(first) = lines.first_mut() {
                first.spans.insert(0, Span::raw("! "));
            }
            Paragraph::new(lines).wrap(Wrap { trim: false })
        }
        None if open.files.is_empty() => Paragraph::new(Line::styled(
            "No files changed",
            Style::new().fg(Color::DarkGray),
        )),
        None => {
            let start = (open.selected + 1).saturating_sub(height.max(1));
            let end = (start + height).min(open.files.len());
            Paragraph::new(
                (start..end)
                    .map(|i| {
                        let line = commit_file_line(&open.files[i], width);
                        if i == open.selected {
                            reversed(line, width)
                        } else {
                            line
                        }
                    })
                    .collect::<Vec<_>>(),
            )
        }
    };
    frame.render_widget(paragraph.block(block), area);
}

/// `<status> <path>`, or `<status> <old> → <new>` for a rename or copy,
/// cut to `width`.
fn commit_file_line(file: &CommitFile, width: usize) -> Line<'static> {
    let colour = match file.status {
        'A' => Color::Green,
        'D' => Color::Red,
        'M' => Color::Yellow,
        'R' | 'C' => Color::Cyan,
        _ => Color::Reset,
    };
    let path = match &file.orig_path {
        Some(orig) => format!("{} → {}", orig.display(), file.path.display()),
        None => file.path.display().to_string(),
    };
    Line::from(vec![
        Span::styled(file.status.to_string(), Style::new().fg(colour)),
        Span::raw(" "),
        Span::raw(elide_right(&path, width.saturating_sub(2))),
    ])
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
        panel.error.as_deref().map(|error| {
            let first = error.lines().next().unwrap_or_default();
            let text = format!("! {first} (press r to retry)");
            Line::styled(elide_right(&text, width), Style::new().fg(Color::Red))
        })
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

/// Which columns a row has room for, worked out once per draw so every row
/// lines up. The subject keeps [`SUBJECT_MIN`] columns: the date goes
/// first, then the author.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Columns {
    /// Graph lanes shown; wider rows end in `…`.
    lanes: usize,
    hash: usize,
    date: bool,
    /// 0 drops the author.
    author: usize,
}

impl Columns {
    fn fit(panel: &CommitsPanel, range: std::ops::Range<usize>, width: usize) -> Self {
        // The widest row loaded so far, not just on screen, so the columns
        // stay put while scrolling; never more than a third of the width.
        let lanes = panel.lanes.max(1).min((width / 6).max(1));
        let hash = panel.rows[range]
            .iter()
            .map(|row| width_of(&row.short_oid))
            .max()
            .unwrap_or(0);
        let fixed = lanes * 2 + 2 + hash + 1;
        let left = width.saturating_sub(fixed);
        let date = left >= SUBJECT_MIN + 4;
        let left = if date { left - 4 } else { left };
        let author = left.saturating_sub(SUBJECT_MIN + 1).min(AUTHOR_WIDTH);
        Self {
            lanes,
            hash,
            date,
            author: if author < 4 { 0 } else { author },
        }
    }
}

/// Lines for `rows[range]` only.
fn commit_lines(
    panel: &CommitsPanel,
    range: std::ops::Range<usize>,
    selected: Option<usize>,
    width: usize,
) -> Vec<Line<'static>> {
    let columns = Columns::fit(panel, range.clone(), width);
    range
        .clone()
        .zip(&panel.graph[range])
        .map(|(i, graph)| {
            let line = commit_line(panel, i, graph, columns, width);
            if Some(i) == selected {
                reversed(line, width)
            } else {
                line
            }
        })
        .collect()
}

/// `<graph> <marker> <hash> <date> <author> <subject>`, padded so each
/// column starts at the same place on every row, the subject cut to fit.
fn commit_line(
    panel: &CommitsPanel,
    index: usize,
    graph: &GraphRow,
    columns: Columns,
    width: usize,
) -> Line<'static> {
    let row = &panel.rows[index];
    let marker = match panel.marker(&row.oid) {
        Marker::Unpushed => Span::styled(
            "↑",
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ),
        Marker::Merged => Span::styled("✓", Style::new().add_modifier(Modifier::DIM)),
        Marker::Pushed | Marker::Unknown => Span::raw(" "),
    };
    let mut spans = vec![
        Span::styled(
            graph_text(graph, columns.lanes),
            Style::new().fg(Color::Magenta),
        ),
        marker,
        Span::raw(" "),
        Span::styled(
            pad(&row.short_oid, columns.hash),
            Style::new().fg(Color::Cyan),
        ),
        Span::raw(" "),
    ];
    if columns.date {
        spans.push(Span::styled(
            format!("{:>3} ", relative_date(row.time, panel.loaded_at)),
            Style::new().fg(Color::DarkGray),
        ));
    }
    if columns.author > 0 {
        let author = elide_right(&row.author, columns.author);
        spans.push(Span::styled(
            format!("{} ", pad(&author, columns.author)),
            Style::new().fg(Color::Green),
        ));
    }
    let used: usize = spans.iter().map(|s| width_of(&s.content)).sum();
    spans.push(Span::raw(elide_right(
        &row.subject,
        width.saturating_sub(used),
    )));
    Line::from(spans)
}

/// `text` padded with spaces to `width` display columns.
fn pad(text: &str, width: usize) -> String {
    let fill = width.saturating_sub(width_of(text));
    format!("{text}{}", " ".repeat(fill))
}

/// Two columns per lane: the lane's glyph, then what joins it to the next.
/// A merge or branch-off draws a line from the commit to the lanes it
/// touches, crossing lanes on the way with `┼`. A row with more than
/// `lanes` lanes ends in `…`.
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
    let mut text: Vec<char> = Vec::new();
    for i in 0..lanes.max(cells.len()) {
        let inside = lo < i && i < hi;
        let glyph = match cells.get(i).copied().unwrap_or(GraphCell::Empty) {
            GraphCell::Commit => '●',
            GraphCell::Pipe if inside => '┼',
            GraphCell::Pipe => '│',
            GraphCell::MergeIn if i > commit => '╮',
            GraphCell::MergeIn => '╭',
            GraphCell::BranchOut if i > commit => '╯',
            GraphCell::BranchOut => '╰',
            GraphCell::Empty if inside => '─',
            GraphCell::Empty => ' ',
        };
        text.push(glyph);
        text.push(if lo <= i && i < hi { '─' } else { ' ' });
    }
    if cells.len() > lanes {
        text.truncate(lanes * 2 - 1);
        text.push('…');
    }
    text.into_iter().collect()
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

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    use super::*;
    use crate::git::log::{CommitRow, LogPage, Markers};
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

    /// A workspace with the Commits panel focused and zoomed to the whole
    /// screen, `rows` loaded as page 0.
    fn showing(rows: Vec<CommitRow>) -> AppState {
        let mut state = AppState::new(Context::Workspace);
        state.files_loaded = true;
        state.focus = Focus::Commits;
        state.expanded = SidePanel::Commits;
        state.screen_mode = ScreenMode::Full;
        state.commits.begin_page(0, 0);
        state
            .commits
            .page_loaded(0, 0, Ok((LogPage { rows, done: true }, NOW)));
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

    /// The display column where `needle` starts on its row.
    fn column_of(rows: &[String], needle: &str) -> usize {
        let line = line_with(rows, needle);
        width_of(&line[..line.find(needle).unwrap()])
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

    /// The graph cells of a hand-built row, for glyph tests.
    fn graph(cells: &[GraphCell]) -> GraphRow {
        GraphRow {
            cells: cells.to_vec(),
        }
    }

    #[test]
    fn a_merge_draws_its_graph_hash_author_date_and_subject() {
        let rows = buffer_rows(&draw(&showing(merge_rows()), 100, 30));
        let author = |name: &str| format!("{name:<16} ");

        let expect = [
            ("●─╮   m000000 now ", "Ann", "Merge side"),
            ("│ ●   s000000  3h ", "Bob", "Side work"),
            ("● │   b000000  2d ", "Ann", "Main work"),
            ("●─╯   a000000  3y ", "Ann", "Root"),
        ];
        for (head, name, subject) in expect {
            let text = format!("{head}{}{subject}", author(name));
            assert!(
                line_with(&rows, subject).contains(&text),
                "{text:?}\n{rows:#?}"
            );
        }
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
        let buffer = draw(&state, 100, 30);
        let rows = buffer_rows(&buffer);

        assert!(line_with(&rows, "Merge side").contains("↑ m000000"));
        assert!(line_with(&rows, "Side work").contains("  s000000"));
        assert!(line_with(&rows, "Main work").contains("✓ b000000"));
        assert!(line_with(&rows, "Root").contains("✓ a000000"));
        // The marker and the hash are told apart by more than the glyph.
        let y = rows.iter().position(|r| r.contains("Merge side")).unwrap() as u16;
        let x = column_of(&rows, "↑") as u16;
        assert_ne!(buffer[(x, y)].style().fg, buffer[(x + 2, y)].style().fg);
    }

    #[test]
    fn the_selected_row_is_highlighted() {
        let mut state = showing(merge_rows());
        state.commits.selected = 2;
        let buffer = draw(&state, 100, 30);
        let rows = buffer_rows(&buffer);
        let y = rows.iter().position(|r| r.contains("Main work")).unwrap();
        let x = column_of(&rows, "b000000") as u16;
        let style = buffer[(x, y as u16)].style();
        assert!(style.add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn a_hundred_thousand_rows_build_lines_only_for_the_window_at_the_cursor() {
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

        let lines = window(&state.commits, Rect::new(0, 0, 60, 20));
        assert_eq!(lines.len(), 20, "one line per visible row, no more");
        let text = |line: &Line| {
            line.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };
        assert!(
            text(&lines[19]).contains("subject 99990"),
            "the cursor's row is last"
        );
        assert!(text(&lines[0]).contains("subject 99971"));

        let rows = buffer_rows(&draw(&state, 100, 30));
        line_with(&rows, "subject 99990");
    }

    #[test]
    fn at_80_by_24_the_panel_keeps_the_subject_and_drops_the_date_and_author() {
        let mut state = showing(merge_rows());
        state.screen_mode = ScreenMode::Normal;
        let rows = buffer_rows(&draw(&state, 80, 24));

        let merge = line_with(&rows, "m000000");
        assert!(merge.contains("m000000 Merge side"), "{rows:#?}");
        assert!(!merge.contains("Ann") && !merge.contains("now"));
        // Half the screen has room for the date and a short author column.
        state.screen_mode = ScreenMode::Half;
        let rows = buffer_rows(&draw(&state, 80, 24));
        let merge = line_with(&rows, "m000000");
        assert!(
            merge.contains("m000000 now Ann     Merge side"),
            "{rows:#?}"
        );
        // Every subject starts in the same column.
        let at = column_of(&rows, "Merge");
        for subject in ["Side work", "Main work", "Root"] {
            assert_eq!(column_of(&rows, subject), at, "{subject}");
        }
    }

    #[test]
    fn a_tiny_panel_draws_without_panicking_and_keeps_the_hash() {
        let mut state = showing(merge_rows());
        for (width, height) in [(20, 6), (12, 4), (4, 3), (1, 1)] {
            let rows = buffer_rows(&draw(&state, width, height));
            if width >= 20 {
                line_with(&rows, "m000000");
            }
        }
        state.screen_mode = ScreenMode::Normal;
        for (width, height) in [(20, 6), (12, 4), (4, 3)] {
            draw(&state, width, height);
        }
    }

    #[test]
    fn wide_characters_keep_the_columns_aligned_and_end_in_an_ellipsis() {
        let rows = vec![
            row(
                "w000000aaa",
                &["n000000aaa"],
                "山田太郎",
                60,
                "絵文字 😀 の件名がとても長い場合のテスト",
            ),
            row("n000000aaa", &[], "Bob", 60, "plain"),
        ];
        let state = showing(rows);
        let rows = buffer_rows(&draw(&state, 60, 10));

        let wide = line_with(&rows, "w000000");
        assert!(wide.contains('…'), "{wide:?}");
        assert!(wide.ends_with('│') || wide.ends_with('┃'), "{wide:?}");
        assert_eq!(column_of(&rows, "絵文字"), column_of(&rows, "plain"));
    }

    #[test]
    fn the_graph_keeps_one_width_while_scrolling_and_caps_it() {
        // A wide octopus at the top, then a long linear tail.
        let mut rows = vec![row(
            "o000000aaa",
            &["l000000000", "x1", "x2", "x3"],
            "Ann",
            60,
            "octopus",
        )];
        rows.extend((0..40).map(|i| {
            let parent = format!("l{:09}", i + 1);
            row(
                &format!("l{i:09}"),
                &[&parent],
                "Ann",
                60,
                &format!("tail {i}"),
            )
        }));
        let mut state = showing(rows);
        let at_top = column_of(&buffer_rows(&draw(&state, 100, 12)), "tail 0");
        state.commits.selected = 40;
        let scrolled = column_of(&buffer_rows(&draw(&state, 100, 12)), "tail 39");
        assert_eq!(
            at_top, scrolled,
            "columns do not jump when the merge scrolls away"
        );

        // Narrow, the graph takes at most a third of the width and says so.
        let lines = commit_lines(&state.commits, 0..1, None, 18);
        let text: String = lines[0].spans[0].content.to_string();
        assert_eq!(text, "●─╮─╮…");
        assert!(text.ends_with('…'), "{text:?}");
    }

    #[test]
    fn graph_glyphs_for_an_octopus_a_left_merge_and_a_crossed_lane() {
        use GraphCell::*;
        assert_eq!(
            graph_text(&graph(&[Commit, MergeIn, MergeIn, MergeIn]), 4),
            "●─╮─╮─╮ "
        );
        assert_eq!(graph_text(&graph(&[MergeIn, Commit]), 2), "╭─● ");
        assert_eq!(graph_text(&graph(&[Commit, Pipe, BranchOut]), 3), "●─┼─╯ ");
        assert_eq!(graph_text(&graph(&[Pipe, Commit, Empty]), 3), "│ ●   ");
    }

    #[test]
    fn collapsed_the_panel_shows_the_head_commit() {
        let mut state = showing(merge_rows());
        state.focus = Focus::Files;
        state.expanded = SidePanel::Files;
        state.screen_mode = ScreenMode::Normal;
        let rows = buffer_rows(&draw(&state, 100, 30));
        line_with(&rows, "m000000 now");
        assert!(!rows.iter().any(|r| r.contains("Side work")));
    }

    #[test]
    fn placeholders_say_loading_empty_or_the_error_and_how_to_retry() {
        let mut state = showing(vec![]);
        state.commits.loaded = false;
        line_with(&buffer_rows(&draw(&state, 100, 30)), "Loading…");
        state.commits.loaded = true;
        line_with(&buffer_rows(&draw(&state, 100, 30)), "No commits yet");
        state.commits.begin_page(1, 0);
        state
            .commits
            .page_loaded(1, 0, Err("fatal: bad thing".into()));
        let rows = buffer_rows(&draw(&state, 100, 30));
        line_with(&rows, "! Could not read the history: fatal: bad thing");
        line_with(&rows, "Press r to retry");
    }

    #[test]
    fn a_failed_later_page_says_so_after_the_last_commit_and_names_r() {
        let mut state = showing(merge_rows());
        state.commits.begin_page(1, 4);
        state.commits.page_loaded(1, 4, Err("fatal: gone".into()));
        let rows = buffer_rows(&draw(&state, 100, 30));
        let root = rows.iter().position(|r| r.contains("Root")).unwrap();
        assert!(
            rows[root + 1]
                .contains("! Could not load more commits: fatal: gone (press r to retry)"),
            "{rows:#?}"
        );
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

    // --- A commit's files ---

    use crate::commit_files::CommitFilesList;
    use crate::git::commit_files::{CommitFile, CommitId, CommitRange};
    use crate::git::diff::{DiffLine, LineKind};

    fn commit_file(status: char, path: &str, orig: Option<&str>) -> CommitFile {
        CommitFile {
            status,
            path: path.into(),
            orig_path: orig.map(Into::into),
        }
    }

    /// `showing(merge_rows())` with `a000000` opened, its files answered
    /// with `result` unless it is `None`.
    fn opened(result: Option<Result<Vec<CommitFile>, String>>) -> AppState {
        let mut state = showing(merge_rows());
        let range = CommitRange {
            oid: CommitId::parse("a000000").unwrap(),
            parent: None,
        };
        let mut open = CommitFilesList::begin(range, "a000000".into(), 3);
        if let Some(result) = result {
            open.loaded(3, result);
        }
        state.commits.open = Some(open);
        state
    }

    fn five_files() -> Vec<CommitFile> {
        vec![
            commit_file('A', "added.rs", None),
            commit_file('D', "deleted.rs", None),
            commit_file('M', "modified.rs", None),
            commit_file('R', "old.rs", Some("older.rs")),
            commit_file('T', "typed", None),
        ]
    }

    #[test]
    fn an_open_commit_lists_its_files_with_coloured_letters_and_renames_as_arrows() {
        let mut state = opened(Some(Ok(five_files())));
        state.commits.open.as_mut().unwrap().move_by(3);
        let buffer = draw(&state, 80, 20);
        let rows = buffer_rows(&buffer);

        assert!(rows[0].contains("Commit a000000"), "{rows:#?}");
        assert!(!rows.iter().any(|r| r.contains("Main work")), "{rows:#?}");
        line_with(&rows, "R older.rs → old.rs");
        for (needle, colour) in [
            ("A added.rs", Color::Green),
            ("D deleted.rs", Color::Red),
            ("M modified.rs", Color::Yellow),
            ("R older.rs", Color::Cyan),
            ("T typed", Color::Reset),
        ] {
            let y = rows.iter().position(|r| r.contains(needle)).unwrap() as u16;
            let x = column_of(&rows, needle) as u16;
            assert_eq!(buffer[(x, y)].style().fg, Some(colour), "{needle}");
        }
        let reversed = |needle: &str| {
            let y = rows.iter().position(|r| r.contains(needle)).unwrap() as u16;
            let x = column_of(&rows, needle) as u16 + 2;
            buffer[(x, y)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        };
        assert!(reversed("R older.rs"));
        assert!(!reversed("A added.rs"));
    }

    #[test]
    fn the_file_list_scrolls_to_keep_the_cursor_on_screen() {
        let files = (0..40)
            .map(|i| commit_file('M', &format!("file{i:02}"), None))
            .collect();
        let mut state = opened(Some(Ok(files)));
        state.commits.open.as_mut().unwrap().move_by(30);
        let rows = buffer_rows(&draw(&state, 80, 12));
        line_with(&rows, "M file30");
        assert!(!rows.iter().any(|r| r.contains("file00")), "{rows:#?}");
    }

    #[test]
    fn an_open_commit_says_loading_failed_or_empty() {
        for (result, needle) in [
            (None, "Loading…"),
            (
                Some(Err("fatal: bad object".to_string())),
                "! fatal: bad object",
            ),
            (Some(Ok(vec![])), "No files changed"),
        ] {
            let rows = buffer_rows(&draw(&opened(result), 80, 12));
            line_with(&rows, needle);
            line_with(&rows, "Commit a000000");
        }
    }

    #[test]
    fn collapsed_with_a_commit_open_the_panel_still_shows_head() {
        let mut state = opened(Some(Ok(five_files())));
        state.screen_mode = ScreenMode::Normal;
        state.focus = Focus::Files;
        state.expanded = SidePanel::Files;
        let rows = buffer_rows(&draw(&state, 100, 30));
        line_with(&rows, "Merge side");
        assert!(!rows.iter().any(|r| r.contains("added.rs")), "{rows:#?}");
    }

    #[test]
    fn the_diff_pane_draws_the_open_commits_file_diff() {
        let mut state = opened(Some(Ok(five_files())));
        state.screen_mode = ScreenMode::Normal;
        let seq = 9;
        state.diff.begin(seq, &"added.rs".into(), 20);
        state.diff.apply(
            seq,
            Ok(crate::git::diff::DiffChunk {
                lines: vec![
                    DiffLine {
                        kind: LineKind::Hunk,
                        text: "@@ -0,0 +1 @@".into(),
                    },
                    DiffLine {
                        kind: LineKind::Added,
                        text: "+fn added() {}".into(),
                    },
                ],
                done: true,
            }),
        );
        let rows = buffer_rows(&draw(&state, 120, 20));
        line_with(&rows, "+fn added() {}");
        line_with(&rows, "added.rs");
        line_with(&rows, "Commit a000000");
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
