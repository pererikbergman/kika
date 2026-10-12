//! Draws the Commits panel. Only the rows in the visible window are turned
//! into lines, so a history of any length draws in the same time.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::commit_files::CommitFilesList;
use crate::commit_graph::{GraphCell, GraphRow};
use crate::commits::{CommitsPanel, CommitsTab, Marker};
use crate::copied::CopiedCommits;
use crate::diff_mark::DiffMark;
use crate::git::commit_files::CommitFile;
use crate::git::commit_files::CommitId;
use crate::git::diff::terminal_safe;
use crate::git::rebase_plan::{TodoAction, TodoEntry, branch_name};
use crate::model::AppState;
use crate::reflog_view::reflog_lines;
use crate::view::{
    PanelLook, elide_right, filter_title, query_title, range_row, reversed, width_of,
};

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
    let reflog = expanded && panel.tab == CommitsTab::Reflog;
    if let Some(open) = panel.open.as_ref().filter(|_| expanded && !reflog) {
        return render_commit_files(open, look, frame, area);
    }
    // The tab shown is in brackets. Where the whole title does not fit,
    // the copied count shortens to `+N`, then the panel's name goes, and
    // the count goes before the tabs do.
    let tabs = match panel.tab {
        CommitsTab::Log => "[Log] Reflog",
        CommitsTab::Reflog => "Log [Reflog]",
    };
    let name = if expanded {
        format!("Commits · {tabs}")
    } else {
        "Commits".to_string()
    };
    let mut last = if expanded { tabs } else { "Commits" }.to_string();
    // A query, a filter or a range on the log names itself in place of
    // the tabs, the query first.
    let mut name = name;
    let selected = match panel.range_anchor {
        Some(_) => format!(" · {} selected", panel.range().len()),
        None => String::new(),
    };
    let mut base = "Commits".to_string();
    if expanded && !reflog && panel.query.is_active() {
        // With a filter as well, the query leaves it half the room.
        let reserve = if panel.filter.is_empty() {
            width_of(&selected)
        } else {
            look.title_room() / 2
        };
        let query = &panel.query;
        base = query_title(
            look,
            "Commits",
            query.path.as_ref().map(|p| p.display()),
            query.author.as_deref(),
            reserve,
        );
        name = format!("{base}{selected}");
        last.clone_from(&name);
    }
    if expanded && !reflog && !panel.filter.is_empty() {
        let counts = format!(
            "{} of {} loaded{selected}",
            panel.visible().len(),
            panel.rows.len()
        );
        name = filter_title(look, &base, &panel.filter, &counts);
        last.clone_from(&name);
    } else if expanded && !reflog && !selected.is_empty() {
        name = format!("{base}{selected}");
        last.clone_from(&name);
    }
    let last = last.as_str();
    let candidates = match state.copied.len() {
        0 => vec![name, last.to_string()],
        n => vec![
            format!("{name} · {n} copied"),
            format!("{name} +{n}"),
            format!("{last} +{n}"),
            last.to_string(),
        ],
    };
    let title = candidates
        .iter()
        .find(|text| look.fits(text))
        .map_or(last, String::as_str);
    let title = look.title(title);
    let block = look.block(title);
    let inner = block.inner(area);
    if reflog {
        let lines = reflog_lines(
            &panel.reflog,
            usize::from(inner.height),
            usize::from(inner.width),
        );
        frame.render_widget(Paragraph::new(lines).block(block), area);
        return;
    }
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
            None if panel.query.is_active() && panel.done => {
                Paragraph::new(Line::styled("No commits match", dim))
            }
            None => Paragraph::new(Line::styled("No commits yet", dim)),
        }
    } else if !expanded {
        let width = usize::from(inner.width);
        let marks = Marks::of(state);
        Paragraph::new(commit_lines(panel, &marks, &[0], None, &[], width))
    } else if panel.visible().is_empty() {
        Paragraph::new(Line::styled("No matches", dim))
    } else {
        Paragraph::new(window(panel, &Marks::of(state), inner))
    };
    frame.render_widget(paragraph.block(block), area);
}

/// A commit's files in place of the log: a placeholder until they load,
/// then one row per file, scrolled so the cursor shows.
fn render_commit_files(open: &CommitFilesList, look: &PanelLook, frame: &mut Frame, area: Rect) {
    let title = match &open.against {
        Some(against) => format!("Files · {} vs {against}", open.short_oid),
        None => format!("Commit {}", open.short_oid),
    };
    let block = look.block(look.title(&title));
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
pub(crate) fn commit_file_line(file: &CommitFile, width: usize) -> Line<'static> {
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
/// row saying a page is loading or failed. A stopped rebase's todo list
/// comes first, right above the log it will extend.
fn window(panel: &CommitsPanel, marks: &Marks, area: Rect) -> Vec<Line<'static>> {
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
    let head = todo_lines(panel, width);
    let shown = panel.visible();
    let at = shown.iter().position(|&i| i == panel.selected).unwrap_or(0);
    let cursor = match (panel.todo_selected, head.len()) {
        // Below the heading, the list in reverse: row 0 is the lowest.
        (Some(i), len) if len > 0 => len - 1 - i,
        (_, len) => len + at,
    };
    let start = (cursor + 1).saturating_sub(room);
    let skip = head.len();
    let mut lines: Vec<Line> = head.into_iter().skip(start).take(room).collect();
    let log_start = start.saturating_sub(skip).min(shown.len());
    let end = (start + room)
        .saturating_sub(skip)
        .clamp(log_start, shown.len());
    let range = panel.range();
    let selected = panel.todo_selected.is_none().then_some(panel.selected);
    lines.extend(commit_lines(
        panel,
        marks,
        &shown[log_start..end],
        selected,
        &range,
        width,
    ));
    if end == shown.len() {
        lines.extend(note);
    }
    lines
}

/// The todo list as rows: a dim heading, then one row per step with the
/// last to run at the top, so it reads like the history it will make.
/// Empty when no rebase is stopped.
fn todo_lines(panel: &CommitsPanel, width: usize) -> Vec<Line<'static>> {
    let Some(todo) = &panel.todo else {
        return Vec::new();
    };
    let dim = Style::new().fg(Color::DarkGray);
    let mut rows: Vec<Line<'static>> = if todo.editable.is_ok() {
        todo.entries
            .iter()
            .map(|entry| todo_line(entry, width))
            .collect()
    } else {
        todo.raw_lines()
            .into_iter()
            .map(|line| Line::raw(elide_right(&terminal_safe(line), width)))
            .collect()
    };
    // A list Kika does not edit says so; its line count would also count
    // lines that are not steps.
    let heading = if todo.editable.is_ok() {
        let left = todo
            .entries
            .iter()
            .filter(|entry| matches!(entry, TodoEntry::Commit { .. }))
            .count();
        format!("Rebase todo ({left} left)")
    } else {
        "Rebase todo (read-only)".to_string()
    };
    if let Some(i) = panel.todo_selected.filter(|&i| i < rows.len()) {
        rows[i] = reversed(std::mem::take(&mut rows[i]), width);
    }
    rows.reverse();
    let mut lines = vec![Line::styled(elide_right(&heading, width), dim)];
    lines.extend(rows);
    lines
}

/// `<action> <hash> <subject>`, or a dim `break` or `→ <branch>`.
fn todo_line(entry: &TodoEntry, width: usize) -> Line<'static> {
    let dim = Style::new().fg(Color::DarkGray);
    match entry {
        TodoEntry::Commit { action, oid, rest } => {
            let colour = match action {
                TodoAction::Pick => Color::Reset,
                TodoAction::Drop => Color::Red,
                _ => Color::Yellow,
            };
            let short: String = oid.chars().take(7).collect();
            let mut spans = vec![
                Span::styled(format!("{:<6} ", action.word()), Style::new().fg(colour)),
                Span::styled(format!("{short} "), Style::new().fg(Color::Cyan)),
            ];
            let used: usize = spans.iter().map(|s| width_of(&s.content)).sum();
            let subject = elide_right(&terminal_safe(rest), width.saturating_sub(used));
            spans.push(Span::raw(subject));
            Line::from(spans)
        }
        TodoEntry::Break => Line::styled("break", dim),
        TodoEntry::UpdateRef(name) => {
            Line::styled(elide_right(&format!("→ {}", branch_name(name)), width), dim)
        }
    }
}

/// Which columns a row has room for, worked out once per draw so every row
/// lines up. The subject keeps [`SUBJECT_MIN`] columns: the date goes
/// first, then the author.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Columns {
    /// Graph lanes shown; wider rows end in `…`. Zero while a filter or a
    /// query hides rows, since lanes would not join across them.
    lanes: usize,
    /// Two columns before the hash for the copied mark, kept on every row
    /// while anything is copied so the hashes line up.
    mark: bool,
    /// Two more for the diff mark, kept on every row while a commit is
    /// marked.
    diff_mark: bool,
    hash: usize,
    date: bool,
    /// 0 drops the author.
    author: usize,
}

impl Columns {
    fn fit(panel: &CommitsPanel, marks: &Marks, indexes: &[usize], width: usize) -> Self {
        let mark = !marks.copied.is_empty();
        let diff_mark = marks.diff.is_some();
        // The widest row loaded so far, not just on screen, so the columns
        // stay put while scrolling; never more than a third of the width.
        let lanes = if panel.filter.is_empty() && !panel.query.is_active() {
            panel.lanes.max(1).min((width / 6).max(1))
        } else {
            0
        };
        let hash = indexes
            .iter()
            .map(|&i| width_of(&panel.rows[i].short_oid))
            .max()
            .unwrap_or(0);
        let fixed = lanes * 2 + 2 + 2 * usize::from(mark) + 2 * usize::from(diff_mark) + hash + 1;
        let left = width.saturating_sub(fixed);
        let date = left >= SUBJECT_MIN + 4;
        let left = if date { left - 4 } else { left };
        let author = left.saturating_sub(SUBJECT_MIN + 1).min(AUTHOR_WIDTH);
        Self {
            lanes,
            mark,
            diff_mark,
            hash,
            date,
            author: if author < 4 { 0 } else { author },
        }
    }
}

/// Lines for the rows at `indexes` only. The `selected` row and those in
/// `range` are drawn reversed.
fn commit_lines(
    panel: &CommitsPanel,
    marks: &Marks,
    indexes: &[usize],
    selected: Option<usize>,
    range: &[usize],
    width: usize,
) -> Vec<Line<'static>> {
    let columns = Columns::fit(panel, marks, indexes, width);
    indexes
        .iter()
        .map(|&i| {
            let line = commit_line(panel, marks, i, &panel.graph[i], columns, width);
            range_row(line, i, selected, range, width)
        })
        .collect()
}

/// `<graph> <marker> <copied> <hash> <date> <author> <subject>`, padded
/// so each column starts at the same place on every row, the subject cut
/// to fit.
fn commit_line(
    panel: &CommitsPanel,
    marks: &Marks,
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
    let mut spans = Vec::new();
    if columns.lanes > 0 {
        spans.push(Span::styled(
            graph_text(graph, columns.lanes),
            Style::new().fg(Color::Magenta),
        ));
    }
    spans.extend([marker, Span::raw(" ")]);
    if columns.mark {
        let is_copied = CommitId::parse(&row.oid).is_some_and(|id| marks.copied.contains(&id));
        spans.push(if is_copied {
            Span::styled(
                "+ ",
                Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )
        } else {
            Span::raw("  ")
        });
    }
    if columns.diff_mark {
        spans.push(if marks.diff == Some(row.oid.as_str()) {
            Span::styled(DIFF_MARK, diff_mark_style())
        } else {
            Span::raw("  ")
        });
    }
    spans.extend([
        Span::styled(
            pad(&row.short_oid, columns.hash),
            Style::new().fg(Color::Cyan),
        ),
        Span::raw(" "),
    ]);
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

/// What marks a commit row besides its own state: the copied set and the
/// commit marked for diffing.
struct Marks<'a> {
    copied: &'a CopiedCommits,
    /// The marked commit's full id, when the mark is a commit.
    diff: Option<&'a str>,
}

impl<'a> Marks<'a> {
    fn of(state: &'a AppState) -> Self {
        let diff = match &state.diff_mark {
            Some(DiffMark::Commit { id, .. }) => Some(id.as_str()),
            _ => None,
        };
        Self {
            copied: &state.copied,
            diff,
        }
    }
}

/// The diff mark's column, on a marked commit or branch row.
pub(crate) const DIFF_MARK: &str = "* ";

pub(crate) fn diff_mark_style() -> Style {
    Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD)
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

    /// `merge_rows` shown, with `b000000` and `a000000` copied.
    fn two_copied() -> AppState {
        let mut state = showing(merge_rows());
        for oid in ["b000000aaa", "a000000aaa"] {
            state.copied.toggle(crate::copied::CopiedCommit {
                id: CommitId::parse(oid).unwrap(),
                short: oid[..7].into(),
                subject: String::new(),
            });
        }
        state
    }

    #[test]
    fn a_copied_row_draws_a_bold_cyan_plus_before_its_hash_and_columns_stay_aligned() {
        let buffer = draw(&two_copied(), 100, 30);
        let rows = buffer_rows(&buffer);
        assert!(line_with(&rows, "Main work").contains("+ b000000"));
        assert!(line_with(&rows, "Root").contains("+ a000000"));
        assert!(line_with(&rows, "Side work").contains("  s000000"));
        assert!(!line_with(&rows, "Side work").contains('+'));
        assert_eq!(
            column_of(&rows, "s000000"),
            column_of(&rows, "b000000"),
            "{rows:#?}"
        );
        assert_eq!(column_of(&rows, "Side work"), column_of(&rows, "Main work"));
        let y = rows.iter().position(|r| r.contains("Main work")).unwrap();
        let x = rows[y].chars().take_while(|&c| c != '+').count();
        let style = buffer[(x as u16, y as u16)].style();
        assert_eq!(style.fg, Some(Color::Cyan));
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn the_title_counts_the_copied_commits_expanded_and_collapsed() {
        let mut state = two_copied();
        line_with(
            &buffer_rows(&draw(&state, 100, 30)),
            "Commits · [Log] Reflog · 2 copied",
        );
        state.focus = Focus::Files;
        state.expanded = SidePanel::Files;
        state.screen_mode = ScreenMode::Normal;
        line_with(&buffer_rows(&draw(&state, 100, 30)), "Commits · 2 copied");
        // With nothing copied there is no count.
        state.copied.clear();
        let rows = buffer_rows(&draw(&state, 100, 30));
        assert!(!rows.iter().any(|r| r.contains("copied")), "{rows:#?}");
    }

    #[test]
    fn a_narrow_panel_shortens_the_count_then_drops_it_before_the_tabs() {
        let state = two_copied();
        let title = |width| buffer_rows(&draw(&state, width, 20))[0].clone();
        assert!(title(60).contains("Commits · [Log] Reflog · 2 copied"));
        let short = title(42);
        assert!(short.contains("Commits · [Log] Reflog +2"), "{short}");
        let shorter = title(32);
        assert!(shorter.contains("[4] [Log] Reflog +2"), "{shorter}");
        let tabs = title(26);
        assert!(tabs.contains("[Log] Reflog"), "{tabs}");
        assert!(!tabs.contains('+'), "{tabs}");
    }

    #[test]
    fn at_80_by_24_the_copied_set_shows_in_the_title_and_the_bar() {
        let mut state = two_copied();
        state.screen_mode = ScreenMode::Normal;
        let rows = buffer_rows(&draw(&state, 80, 24));
        line_with(&rows, "[4] [Log] Reflog +2");
        line_with(&rows, "+ b000000");
        assert!(
            rows[23].starts_with(" 2 copied  v paste  C clear  enter files"),
            "{}",
            rows[23]
        );

        // Collapsed, behind another panel: the count stays, and the bar
        // does not offer a key that only the Commits panel takes.
        state.focus = Focus::Files;
        state.expanded = SidePanel::Files;
        let rows = buffer_rows(&draw(&state, 80, 24));
        line_with(&rows, "[4] Commits · 2 copied");
        assert!(rows[23].starts_with(" 2 copied  "), "{}", rows[23]);
        assert!(!rows[23].contains("C clear"), "{}", rows[23]);
        // With nothing copied, clear stays out of the bar.
        state.focus = Focus::Commits;
        state.expanded = SidePanel::Commits;
        state.copied.clear();
        let rows = buffer_rows(&draw(&state, 80, 24));
        assert!(!rows[23].contains("copied"), "{}", rows[23]);
        assert!(!rows[23].contains("C clear"), "{}", rows[23]);
    }

    #[test]
    fn a_tiny_screen_with_commits_copied_draws_without_panicking() {
        let mut state = two_copied();
        for mode in [ScreenMode::Normal, ScreenMode::Full] {
            state.screen_mode = mode;
            for (width, height) in [(20, 8), (12, 5), (4, 3), (1, 1)] {
                draw(&state, width, height);
            }
        }
        let rows = buffer_rows(&draw(&state, 20, 8));
        assert!(rows.iter().any(|r| r.contains('+')), "{rows:#?}");
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

    fn stopped_rebase(state: &mut AppState) {
        use crate::rebase_todo::{RebaseTodo, TodoFile};
        let todo = b"pick 1111111aaa first step\nbreak\nreword 2222222bbb second \x1b[31mstep\n\
                     update-ref refs/heads/stack\n# help\n";
        state.commits.set_todo(Some(RebaseTodo::load(TodoFile {
            path: "/r/.git/rebase-merge/git-rebase-todo".into(),
            todo: todo.to_vec(),
            done: Some(b"pick 0000000 done\n".to_vec()),
        })));
    }

    /// A stopped rebase whose todo holds `exec`, so Kika shows it
    /// read-only.
    fn read_only_rebase(state: &mut AppState) {
        use crate::rebase_todo::{RebaseTodo, TodoFile};
        state.commits.set_todo(Some(RebaseTodo::load(TodoFile {
            path: "/r/.git/rebase-merge/git-rebase-todo".into(),
            todo: b"pick 1111111 one\nexec make test-everything --verbose\n".to_vec(),
            done: None,
        })));
    }

    /// A stopped rebase with `n` steps left, longer than a small panel.
    fn long_rebase(state: &mut AppState, n: usize) {
        use crate::rebase_todo::{RebaseTodo, TodoFile};
        let todo: String = (0..n).map(|i| format!("pick {i:07x} step {i}\n")).collect();
        state.commits.set_todo(Some(RebaseTodo::load(TodoFile {
            path: "/r/.git/rebase-merge/git-rebase-todo".into(),
            todo: todo.into_bytes(),
            done: None,
        })));
    }

    /// The bottom bar's text at `width` columns, after an edit key on the
    /// read-only todo.
    fn read_only_notice(width: u16) -> String {
        use crossterm::event::{KeyCode, KeyModifiers};
        let mut state = showing(merge_rows());
        read_only_rebase(&mut state);
        state.commits.todo_selected = Some(0);
        state.update(crate::intent::AppIntent::Key(crate::keymap::KeyPress::new(
            KeyCode::Char('d'),
            KeyModifiers::NONE,
        )));
        assert!(state.notice.is_some());
        let rows = buffer_rows(&draw(&state, width, 24));
        rows.last().unwrap().trim_end().to_string()
    }

    #[test]
    fn the_read_only_notice_shows_how_to_fix_it_first() {
        let wide = read_only_notice(80);
        assert!(wide.contains("--edit-todo"), "{wide:?}");
        assert!(wide.contains("line 2"), "{wide:?}");
        let narrow = read_only_notice(40);
        assert!(narrow.ends_with('…'), "{narrow:?}");
        assert!(narrow.starts_with(" Read-only"), "{narrow:?}");
    }

    /// Every size the panel might get, from a full screen to one cell.
    const SIZES: [(u16, u16); 3] = [(80, 24), (20, 5), (1, 1)];

    #[test]
    fn todo_rows_with_a_filtered_log_draw_at_any_size() {
        let mut state = showing(merge_rows());
        stopped_rebase(&mut state);
        state.commits.filter = "work".into();
        state.commits.selected = 2;
        for (width, height) in SIZES {
            draw(&state, width, height);
            state.commits.todo_selected = Some(1);
            draw(&state, width, height);
            state.commits.todo_selected = None;
        }
    }

    #[test]
    fn the_log_cursor_lands_on_its_row_below_the_todo_with_a_range_and_a_filter() {
        let mut state = showing(merge_rows());
        stopped_rebase(&mut state);
        state.commits.filter = "work".into();
        // `Side work` and `Main work` show; the range runs from one to the other.
        state.commits.range_anchor = Some("s000000aaa".into());
        state.commits.selected = 2;
        for (width, height) in SIZES {
            draw(&state, width, height);
        }
        let buffer = draw(&state, 100, 30);
        let rows = buffer_rows(&buffer);
        assert!(!rows.iter().any(|r| r.contains("Merge side")), "filtered");
        let y = rows.iter().position(|r| r.contains("Main work")).unwrap() as u16;
        let x = column_of(&rows, "b000000") as u16;
        assert!(
            buffer[(x, y)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        let todo = rows.iter().position(|r| r.contains("first step")).unwrap() as u16;
        let x = column_of(&rows, "1111111") as u16;
        assert!(
            !buffer[(x, todo)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn a_long_todo_in_a_short_panel_scrolls_without_a_panic() {
        let mut state = showing(merge_rows());
        long_rebase(&mut state, 40);
        for selected in [None, Some(0), Some(20), Some(39)] {
            state.commits.todo_selected = selected;
            for height in 1..=3 {
                draw(&state, 80, height);
            }
            for (width, height) in SIZES {
                draw(&state, width, height);
            }
        }
        // The cursor's row shows even with the heading scrolled away.
        state.commits.todo_selected = Some(20);
        let rows = buffer_rows(&draw(&state, 80, 8));
        assert!(rows.iter().any(|r| r.contains("step 20")), "{rows:#?}");
        assert!(!rows.iter().any(|r| r.contains("Rebase todo")), "{rows:#?}");
    }

    #[test]
    fn a_read_only_todo_draws_at_any_width() {
        let mut state = showing(merge_rows());
        read_only_rebase(&mut state);
        for selected in [None, Some(1)] {
            state.commits.todo_selected = selected;
            for width in 0..=10 {
                draw(&state, width, 24);
            }
            for (width, height) in SIZES {
                draw(&state, width, height);
            }
        }
    }

    #[test]
    fn a_stopped_rebase_shows_its_todo_above_the_log_last_to_run_first() {
        let mut state = showing(merge_rows());
        stopped_rebase(&mut state);
        state.commits.todo_selected = Some(0);
        let buffer = draw(&state, 100, 30);
        let rows = buffer_rows(&buffer);
        let at = |needle: &str| {
            rows.iter()
                .position(|r| r.contains(needle))
                .unwrap_or_else(|| panic!("{needle:?} not on screen:\n{}", rows.join("\n")))
        };
        let heading = at("Rebase todo (2 left)");
        assert_eq!(at("→ stack"), heading + 1);
        assert_eq!(at("reword 2222222 second"), heading + 2);
        assert_eq!(at("break"), heading + 3);
        assert_eq!(at("pick   1111111 first step"), heading + 4);
        assert_eq!(at("Merge side"), heading + 5);
        assert!(!rows.iter().any(|r| r.contains('\x1b')));
        let x = column_of(&rows, "1111111") as u16;
        let y = at("first step") as u16;
        assert!(
            buffer[(x, y)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        // The log's cursor is not drawn while the todo has it.
        let x = column_of(&rows, "m000000") as u16;
        let y = at("Merge side") as u16;
        assert!(
            !buffer[(x, y)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        let y = at("Rebase todo") as u16;
        assert_eq!(buffer[(1, y)].style().fg, Some(Color::DarkGray));
    }

    #[test]
    fn the_window_scrolls_from_the_todo_into_the_log() {
        let mut state = showing(merge_rows());
        stopped_rebase(&mut state);
        // Six rows inside the borders: the heading scrolls away.
        state.commits.selected = 3;
        let rows = buffer_rows(&draw(&state, 100, 8));
        assert!(rows.iter().any(|r| r.contains("Root")), "{rows:#?}");
        assert!(!rows.iter().any(|r| r.contains("Rebase todo")), "{rows:#?}");
        state.commits.todo_selected = Some(3);
        let rows = buffer_rows(&draw(&state, 100, 8));
        assert!(rows.iter().any(|r| r.contains("Rebase todo")), "{rows:#?}");
    }

    #[test]
    fn a_todo_kika_does_not_edit_shows_its_lines_as_they_are() {
        use crate::rebase_todo::{RebaseTodo, TodoFile};
        let mut state = showing(merge_rows());
        state.commits.set_todo(Some(RebaseTodo::load(TodoFile {
            path: "/r/.git/rebase-merge/git-rebase-todo".into(),
            todo: b"pick 1111111 one\nexec make test\n".to_vec(),
            done: None,
        })));
        let rows = buffer_rows(&draw(&state, 100, 30));
        let heading = rows
            .iter()
            .position(|r| r.contains("Rebase todo (read-only)"))
            .unwrap();
        assert!(rows[heading + 1].contains("exec make test"));
        assert!(rows[heading + 2].contains("pick 1111111 one"));
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

        let lines = window(&state.commits, &Marks::of(&state), Rect::new(0, 0, 60, 20));
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
        let lines = commit_lines(&state.commits, &Marks::of(&state), &[0], None, &[], 18);
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

    #[test]
    fn a_filter_shows_the_matching_rows_without_the_graph() {
        let mut state = showing(merge_rows());
        state.commits.filter = "work".into();
        state.commits.selected = 1;
        let rows = buffer_rows(&draw(&state, 100, 30));
        assert!(
            line_with(&rows, "Commits · /work 2 of 4 loaded").contains("Commits"),
            "{rows:#?}"
        );
        assert!(line_with(&rows, "Side work").contains("s000000"));
        line_with(&rows, "Main work");
        assert!(!rows.iter().any(|r| r.contains("Merge side")), "{rows:#?}");
        assert!(!rows.iter().any(|r| r.contains('●')), "no graph: {rows:#?}");

        state.commits.filter = "zzz".into();
        let rows = buffer_rows(&draw(&state, 100, 30));
        line_with(&rows, "No matches");
    }

    fn queried(path: Option<&str>, author: Option<&str>) -> AppState {
        let mut state = showing(merge_rows());
        state.commits.query = crate::git::log::LogQuery {
            path: path.map(Into::into),
            author: author.map(Into::into),
        };
        state
    }

    #[test]
    fn a_log_query_names_itself_in_the_title_made_safe_and_hides_the_graph() {
        let state = queried(Some("src/a.rs"), Some("ann\u{202E}x"));
        let rows = buffer_rows(&draw(&state, 100, 30));
        line_with(&rows, "Commits · path: src/a.rs · author: ann<U+202E>x");
        assert!(!rows.iter().any(|r| r.contains('\u{202E}')), "{rows:#?}");
        assert!(!rows.iter().any(|r| r.contains('●')), "no graph: {rows:#?}");
        line_with(&rows, "Main work");

        // The fuzzy filter's counts come after the query.
        let mut state = queried(None, Some("ann"));
        state.commits.filter = "work".into();
        let rows = buffer_rows(&draw(&state, 100, 30));
        line_with(&rows, "Commits · author: ann · /work 2 of 4 loaded");
    }

    #[test]
    fn a_long_query_is_cut_to_the_title_keeping_the_file_name() {
        let long = format!("{}/name.rs", "dir/".repeat(40));
        let state = queried(Some(&long), Some(&"a".repeat(80)));
        let rows = buffer_rows(&draw(&state, 80, 24));
        let title = line_with(&rows, "[4] path: …");
        assert!(title.contains("name.rs · author: aaa"), "{title}");
        assert!(width_of(title) <= 80, "{title}");
    }

    #[test]
    fn an_empty_finished_query_says_nothing_matches() {
        let mut state = queried(None, Some("zed"));
        state.commits.set_query(state.commits.query.clone());
        state.commits.begin_page(1, 0);
        state.commits.page_loaded(
            1,
            0,
            Ok((
                LogPage {
                    rows: vec![],
                    done: true,
                },
                NOW,
            )),
        );
        line_with(&buffer_rows(&draw(&state, 100, 30)), "No commits match");
    }

    #[test]
    fn the_marked_commit_draws_a_star_and_the_columns_stay_aligned() {
        let mut state = showing(merge_rows());
        state.diff_mark = Some(DiffMark::Commit {
            id: CommitId::parse("b000000aaa").unwrap(),
            short: "b000000".into(),
        });
        let buffer = draw(&state, 100, 30);
        let rows = buffer_rows(&buffer);
        assert!(
            line_with(&rows, "Main work").contains("* b000000"),
            "{rows:#?}"
        );
        assert!(
            line_with(&rows, "Side work").contains("  s000000"),
            "{rows:#?}"
        );
        assert_eq!(column_of(&rows, "b000000"), column_of(&rows, "s000000"));
        let y = rows.iter().position(|r| r.contains("Main work")).unwrap() as u16;
        let x = column_of(&rows, "* b000000") as u16;
        assert_eq!(buffer[(x, y)].style().fg, Some(Color::Magenta));
    }

    #[test]
    fn a_comparison_names_both_ends_in_the_title() {
        let mut state = opened(Some(Ok(five_files())));
        let open = state.commits.open.as_mut().unwrap();
        open.short_oid = "a1b2c3d".into();
        open.against = Some("main".into());
        let rows = buffer_rows(&draw(&state, 80, 20));
        assert!(rows[0].contains("Files · a1b2c3d vs main"), "{rows:#?}");
    }

    #[test]
    fn a_range_is_counted_in_the_title() {
        let mut state = showing(merge_rows());
        state.commits.range_anchor = Some("m000000aaa".into());
        state.commits.selected = 2;
        let rows = buffer_rows(&draw(&state, 100, 30));
        line_with(&rows, "Commits · 3 selected");
    }
}
