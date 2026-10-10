use crate::commit_editor::{CommitEditor, Field};
use crate::diff_pane::DiffPane;
use crate::git::FileEntry;
use crate::git::diff::{DiffLine, LineKind};
use crate::keymap::{KeyPattern, available, registry};
use crate::model::{AppState, CommitFlow, Context};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

/// Draw the state. Reads the model only; never mutates it.
pub fn render(state: &AppState, frame: &mut Frame) {
    let [main, status] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());

    match state.context {
        Context::Workspace => {
            let [files, diff] =
                Layout::horizontal([Constraint::Percentage(30), Constraint::Fill(1)]).areas(main);
            render_files_panel(state, frame, files);
            render_diff_pane(&state.diff, frame, diff);
        }
        Context::Review { number } => {
            frame.render_widget(
                Paragraph::new("").block(Block::bordered().title(format!(" Review #{number} "))),
                main,
            );
        }
    }
    if state.help_open {
        render_help(state, frame, main);
    }
    render_commit(&state.commit, frame, main);
    frame.render_widget(status_bar(state), status);
}

/// The border colour of the commit box that has the cursor.
const FOCUSED: Color = Color::Cyan;

/// Popups wider than this are harder to read; it is a common message width.
const COMMIT_WIDTH: u16 = 72;

/// The commit popup for the current step, over `area`.
fn render_commit(commit: &CommitFlow, frame: &mut Frame, area: Rect) {
    match commit {
        CommitFlow::Closed => {}
        CommitFlow::ConfirmStageAll => render_question(
            " Commit ",
            "Nothing is staged. Stage all changes and commit? (y/n)",
            frame,
            area,
        ),
        CommitFlow::ConfirmDiscard(_) => render_question(
            " Discard message ",
            "Discard the typed message? (y/n)",
            frame,
            area,
        ),
        CommitFlow::Editing(editor) => render_commit_editor(editor, true, frame, area),
        CommitFlow::Running(editor) => render_commit_editor(editor, false, frame, area),
        CommitFlow::Failed { output, .. } => render_commit_failed(output, frame, area),
    }
}

/// A small popup asking `question`, wrapped to fit and grown to hold it,
/// so the answer keys at its end are never cut off.
fn render_question(title: &str, question: &str, frame: &mut Frame, area: Rect) {
    let width = u16::try_from(width_of(question) + 4)
        .unwrap_or(u16::MAX)
        .min(area.width);
    // One column of padding on each side inside the border.
    let rows = wrap(question, usize::from(width.saturating_sub(4)));
    let height = u16::try_from(rows.len() + 2).unwrap_or(u16::MAX);
    let popup = centered(area, width, height);
    let lines: Vec<Line> = rows
        .into_iter()
        .map(|row| Line::raw(format!(" {row}")))
        .collect();
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(title)),
        popup,
    );
}

/// The message popup: a one-line Subject box over a Body box. While
/// `editing`, the focused box is marked and holds the terminal cursor.
/// When there is no room for both, only the focused one is drawn.
fn render_commit_editor(editor: &CommitEditor, editing: bool, frame: &mut Frame, area: Rect) {
    // Two outer borders, a three-row Subject box and a Body box of ten text rows.
    let popup = centered(area, COMMIT_WIDTH, 2 + 3 + 12);
    frame.render_widget(Clear, popup);
    let block = if editing {
        Block::bordered().title(" Commit message ")
    } else {
        Block::bordered()
            .title(" Committing… ")
            .title_bottom(" waiting for git · ctrl-c quits Kika ")
    };
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let focus = editing.then_some(editor.field);
    let subject = TextBox {
        title: "Subject",
        lines: std::slice::from_ref(&editor.subject),
        cursor: (focus == Some(Field::Subject)).then_some((0, editor.col)),
    };
    let body = TextBox {
        title: "Body",
        lines: &editor.body,
        cursor: (focus == Some(Field::Body)).then_some((editor.row, editor.col)),
    };
    // Each box needs three rows to show one line of text.
    let cursor = if inner.height >= 6 {
        let [top, bottom] =
            Layout::vertical([Constraint::Length(3), Constraint::Fill(1)]).areas(inner);
        subject.render(frame, top).or(body.render(frame, bottom))
    } else if focus == Some(Field::Body) {
        body.render(frame, inner)
    } else {
        subject.render(frame, inner)
    };
    if let Some(position) = cursor {
        frame.set_cursor_position(position);
    }
}

/// A bordered box of text lines. With a `(row, col)` cursor it is focused:
/// marked, and scrolled so the cursor is in view. Without one it shows its
/// start. Text cut off at either side is marked with `…`.
struct TextBox<'a> {
    title: &'a str,
    lines: &'a [String],
    cursor: Option<(usize, usize)>,
}

impl TextBox<'_> {
    /// Draw into `area`; return the cursor's screen position if it shows.
    fn render(&self, frame: &mut Frame, area: Rect) -> Option<(u16, u16)> {
        let block = match self.cursor {
            Some(_) => Block::bordered()
                .title(format!(" ▸ {} ", self.title))
                .title_style(Style::new().add_modifier(Modifier::BOLD))
                .border_style(Style::new().fg(FOCUSED)),
            None => Block::bordered().title(format!(" {} ", self.title)),
        };
        let inner = block.inner(area);
        if inner.is_empty() {
            frame.render_widget(block, area);
            return None;
        }
        let (width, height) = (usize::from(inner.width), usize::from(inner.height));
        let (row, col) = self.cursor.unwrap_or_default();
        let line = self.lines.get(row).map_or("", String::as_str);
        let before: String = line.chars().take(col).collect();
        let x = width_of(&before);
        // Scroll just enough that the cursor's row is inside, and its column
        // is inside with one column to spare for a `…` after it.
        let top = (row + 1).saturating_sub(height);
        let left = (x + 2).saturating_sub(width.max(2));
        let text: Vec<Line> = self
            .lines
            .iter()
            .skip(top)
            .take(height)
            .map(|line| Line::raw(clip(line, left, width)))
            .collect();
        frame.render_widget(Paragraph::new(text).block(block), area);
        let at = |offset: usize, start: u16| start + u16::try_from(offset).unwrap_or(u16::MAX);
        self.cursor
            .map(|_| (at(x - left, inner.x), at(row - top, inner.y)))
    }
}

/// One terminal cell of clipped text.
#[derive(Clone)]
enum Cell {
    Blank,
    Glyph(String),
    /// The right half of the wide glyph before it.
    Rest,
}

/// Columns `from..from + width` of `line`. A wide glyph cut by an edge
/// becomes blank, and a `…` replaces the edge cell where text is cut off.
fn clip(line: &str, from: usize, width: usize) -> String {
    let mut cells = vec![Cell::Blank; width];
    let mut col: usize = 0;
    for c in line.chars() {
        let w = width_of(c.encode_utf8(&mut [0; 4]));
        if w == 0 {
            // A combining mark stays with the glyph before it.
            if let Some(Cell::Glyph(glyph)) = col
                .checked_sub(1)
                .and_then(|last| last.checked_sub(from))
                .and_then(|i| cells.get_mut(i))
            {
                glyph.push(c);
            }
            continue;
        }
        if col >= from && col + w <= from + width {
            cells[col - from] = Cell::Glyph(c.to_string());
            for rest in &mut cells[col - from + 1..col - from + w] {
                *rest = Cell::Rest;
            }
        }
        col += w;
    }
    if width > 0 && from > 0 && col > 0 {
        if let Some(Cell::Rest) = cells.get(1) {
            cells[1] = Cell::Blank;
        }
        cells[0] = Cell::Glyph("…".into());
    }
    if width > 0 && col > from + width {
        if let Cell::Rest = cells[width - 1] {
            cells[width - 2] = Cell::Blank;
        }
        cells[width - 1] = Cell::Glyph("…".into());
    }
    cells
        .iter()
        .map(|cell| match cell {
            Cell::Blank => " ",
            Cell::Glyph(glyph) => glyph,
            Cell::Rest => "",
        })
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// `text` broken into rows of at most `width` columns, at a space where
/// one fits, else mid-word. Leading indentation is kept.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let mut rest = line;
        let first = rows.len();
        while width_of(rest) > width {
            let mut used = 0;
            let mut cut = 0;
            let mut space = None;
            for (i, c) in rest.char_indices() {
                used += width_of(c.encode_utf8(&mut [0; 4]));
                if used > width {
                    // A space just past the edge is the best place to break.
                    if c == ' ' {
                        space = Some(i);
                    }
                    break;
                }
                cut = i + c.len_utf8();
                if c == ' ' && i > 0 {
                    space = Some(i);
                }
            }
            // A glyph wider than the row still has to go somewhere.
            let cut = cut.max(rest.chars().next().map_or(0, char::len_utf8));
            let (row, next) = match space {
                Some(i) => (&rest[..i], &rest[i + 1..]),
                None => (&rest[..cut], &rest[cut..]),
            };
            rows.push(row.to_string());
            rest = next;
        }
        // An empty line is one row; an empty tail after a break is none.
        if !rest.is_empty() || rows.len() == first {
            rows.push(rest.to_string());
        }
    }
    rows
}

/// The output of a failed commit, wrapped. When it does not fit, the end is
/// shown, since that is where hooks usually say what went wrong, and the
/// first row says how much is cut. The way back is always on the last row.
fn render_commit_failed(output: &str, frame: &mut Frame, area: Rect) {
    let width = COMMIT_WIDTH.min(area.width);
    let rows = wrap(output, usize::from(width.saturating_sub(2)).max(1));
    let height = u16::try_from(rows.len() + 3).unwrap_or(u16::MAX);
    let popup = centered(area, width, height);
    frame.render_widget(Clear, popup);
    let block = Block::bordered().title(" Commit failed ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let [text, hint] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(inner);
    let room = usize::from(text.height);
    let mut lines: Vec<Line> = Vec::new();
    let mut shown = &rows[..];
    if rows.len() > room && room >= 2 {
        shown = &rows[rows.len() - (room - 1)..];
        lines.push(Line::styled(
            format!("… {} earlier lines", rows.len() - shown.len()),
            Style::new().fg(Color::DarkGray),
        ));
    } else if rows.len() > room {
        shown = &rows[rows.len() - room..];
    }
    lines.extend(shown.iter().map(|row| Line::raw(row.as_str())));
    frame.render_widget(Paragraph::new(lines), text);
    frame.render_widget(
        Line::styled("Esc: back to the message", Style::new().fg(Color::Yellow)),
        hint,
    );
}

/// The busy indicator, then why the last key did nothing, else the last
/// operation's error, else the key hints. The indicator goes first so a
/// narrow bar clips the rest, not it. Hints come from the keymap, so the bar
/// always matches what the keys do; disabled ones are dimmed.
fn status_bar(state: &AppState) -> Line<'_> {
    let mut spans = Vec::new();
    if state.busy > 0 {
        spans.push(Span::raw(" loading… "));
    }
    if let Some(notice) = state.notice {
        spans.push(Span::styled(
            format!(" {notice}"),
            Style::new().fg(Color::Yellow),
        ));
    } else if let Some(error) = &state.op_error {
        spans.push(Span::styled(
            format!(" error: {}", error.lines().next().unwrap_or_default()),
            Style::new().fg(Color::Red),
        ));
    } else {
        let focus = state.key_focus();
        let note = focus.bar_note();
        if let Some(note) = note {
            spans.push(Span::styled(
                format!(" {note}"),
                Style::new().fg(Color::DarkGray),
            ));
        }
        let hints = available(registry(), focus, state)
            .into_iter()
            .filter(|(binding, _)| binding.bar);
        for (i, (binding, disabled)) in hints.enumerate() {
            let first = i == 0 && note.is_none();
            spans.push(Span::raw(if first { " " } else { "  " }));
            let key = binding.keys.first().map(KeyPattern::label);
            let hint = format!("{} {}", key.unwrap_or_default(), binding.label);
            spans.push(Span::styled(hint, dimmed_if(disabled.is_some())));
        }
    }
    Line::from(spans)
}

fn dimmed_if(disabled: bool) -> Style {
    if disabled {
        Style::new().fg(Color::DarkGray)
    } else {
        Style::new()
    }
}

/// A popup over `area` listing every key the focused panel answers to.
/// Rows that do not fit are clipped.
fn render_help(state: &AppState, frame: &mut Frame, area: Rect) {
    let focus = state.panel_focus();
    let entries: Vec<(String, &str, bool)> = available(registry(), focus, state)
        .into_iter()
        .map(|(binding, disabled)| {
            let keys: Vec<String> = binding.keys.iter().map(KeyPattern::label).collect();
            (keys.join("/"), binding.description, disabled.is_some())
        })
        .collect();
    let key_width = entries
        .iter()
        .map(|(k, _, _)| width_of(k))
        .max()
        .unwrap_or(0);
    let lines: Vec<Line> = entries
        .iter()
        .map(|(keys, description, disabled)| {
            let pad = key_width - width_of(keys);
            Line::styled(
                format!(" {keys}{}  {description} ", " ".repeat(pad)),
                dimmed_if(*disabled),
            )
        })
        .collect();
    let title = format!(" Keys: {} ", focus.title());
    let content_width = lines
        .iter()
        .map(Line::width)
        .chain([width_of(&title)])
        .max()
        .unwrap_or(0);
    let popup = centered(
        area,
        u16::try_from(content_width + 2).unwrap_or(u16::MAX),
        u16::try_from(lines.len() + 2).unwrap_or(u16::MAX),
    );
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(title)),
        popup,
    );
}

/// A `width` x `height` rectangle centred in `area`, shrunk to fit it.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

/// The Files panel: an error, a placeholder until the first load, or one line per file.
fn render_files_panel(state: &AppState, frame: &mut Frame, area: Rect) {
    let block = Block::bordered().title(" Files ");
    let inner = block.inner(area);
    let panel = if let Some(error) = &state.status_error {
        let mut lines: Vec<Line> = error.lines().map(Line::from).collect();
        lines.push(Line::from("Press r to retry"));
        Paragraph::new(lines).wrap(Wrap { trim: false })
    } else if !state.files_loaded {
        Paragraph::new("Loading…")
    } else if state.files.is_empty() {
        Paragraph::new("No changes")
    } else {
        Paragraph::new(file_lines(&state.files, state.selected, inner))
    };
    frame.render_widget(panel.block(block), area);
}

/// The files that fit in `area`, scrolled so `selected` is visible and drawn
/// reversed. When files below the window are cut, the last row says how many,
/// so the cut is never silent.
fn file_lines(files: &[FileEntry], selected: Option<usize>, area: Rect) -> Vec<Line<'static>> {
    let rows = usize::from(area.height);
    let width = usize::from(area.width);
    let line = |(i, f): (usize, &FileEntry)| {
        let line = file_line(f, width);
        if Some(i) == selected {
            reversed(line, width)
        } else {
            line
        }
    };
    if files.len() <= rows {
        return files.iter().enumerate().map(line).collect();
    }
    let selected = selected.unwrap_or(0);
    if rows <= 1 {
        // No room for a "+N more" row; the selection matters more.
        return files
            .iter()
            .enumerate()
            .skip(selected)
            .take(rows)
            .map(line)
            .collect();
    }
    // One row is kept for "+N more" unless the window reaches the end.
    let shown = rows - 1;
    let start = selected.saturating_sub(shown - 1);
    if start + rows >= files.len() {
        let start = files.len() - rows;
        return files.iter().enumerate().skip(start).map(line).collect();
    }
    let mut lines: Vec<Line> = files
        .iter()
        .enumerate()
        .skip(start)
        .take(shown)
        .map(line)
        .collect();
    lines.push(Line::from(format!("+{} more", files.len() - start - shown)));
    lines
}

/// Highlight a whole row, not just its text, by padding it to `width`.
fn reversed(mut line: Line<'static>, width: usize) -> Line<'static> {
    let pad = width.saturating_sub(line.width());
    line.spans.push(Span::raw(" ".repeat(pad)));
    line.style(Style::new().add_modifier(Modifier::REVERSED))
}

/// The Diff pane: an error, a delayed "Loading…", "No diff", or the visible lines.
fn render_diff_pane(diff: &DiffPane, frame: &mut Frame, area: Rect) {
    let block = Block::bordered().title(" Diff ");
    let rows = usize::from(block.inner(area).height);
    let pane = if let Some(error) = &diff.error {
        Paragraph::new(error.lines().map(Line::from).collect::<Vec<_>>()).wrap(Wrap { trim: false })
    } else if diff.show_loading {
        Paragraph::new("Loading…")
    } else if diff.done && diff.lines.is_empty() {
        Paragraph::new("No diff")
    } else {
        // Only the visible slice: a 50,000-line diff costs a screenful per frame.
        let start = diff.scroll.min(diff.lines.len());
        let end = start.saturating_add(rows).min(diff.lines.len());
        Paragraph::new(
            diff.lines[start..end]
                .iter()
                .map(styled_diff_line)
                .collect::<Vec<_>>(),
        )
    };
    frame.render_widget(pane.block(block), area);
}

/// Colour by kind; `---`/`+++` arrive as headers, so they are never red or green.
fn styled_diff_line(line: &DiffLine) -> Line<'_> {
    let style = match line.kind {
        LineKind::Added => Style::new().fg(Color::Green),
        LineKind::Removed => Style::new().fg(Color::Red),
        LineKind::Hunk => Style::new().fg(Color::Cyan),
        LineKind::Header => Style::new().add_modifier(Modifier::BOLD),
        LineKind::Context => Style::new(),
    };
    Line::styled(line.text.as_str(), style)
}

/// `XY path`, or `XY orig → path` for renames and copies, fitted to `width`
/// columns. Paths lose their start first, since the file name is at the end;
/// for renames the new path keeps as much room as it can.
fn file_line(entry: &FileEntry, width: usize) -> Line<'static> {
    let letters = entry.status_letters();
    let prefix = width_of(&letters) + 1;
    let text = match &entry.orig_path {
        None => {
            let path = elide_left(entry.path.display(), width.saturating_sub(prefix));
            format!("{letters} {path}")
        }
        Some(orig) => {
            let room = width.saturating_sub(prefix + width_of(" → "));
            let orig_room = room
                .saturating_sub(width_of(entry.path.display()))
                .max(1)
                .min(width_of(orig.display()));
            let orig = elide_left(orig.display(), orig_room);
            let path = elide_left(entry.path.display(), room.saturating_sub(orig_room));
            format!("{letters} {orig} → {path}")
        }
    };
    Line::from(text)
}

/// Fit `text` into `max` columns by replacing its start with `…`.
fn elide_left(text: &str, max: usize) -> String {
    if width_of(text) <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut kept = 0;
    let start = text
        .char_indices()
        .rev()
        .take_while(|(_, c)| {
            kept += width_of(c.encode_utf8(&mut [0; 4]));
            kept < max
        })
        .last()
        .map_or(text.len(), |(i, _)| i);
    format!("…{}", &text[start..])
}

/// Display width in terminal columns, measured the way ratatui lays text out.
fn width_of(text: &str) -> usize {
    Span::raw(text).width()
}

/// A drawn buffer's rows as text, for tests.
#[cfg(test)]
pub(crate) fn buffer_rows(buffer: &ratatui::buffer::Buffer) -> Vec<String> {
    (0..buffer.area.height)
        .map(|y| row_text(buffer, y, 0..buffer.area.width))
        .collect()
}

/// Columns `xs` of row `y` as text. The blank cell after a wide glyph is
/// skipped so the text reads like the screen.
#[cfg(test)]
pub(crate) fn row_text(
    buffer: &ratatui::buffer::Buffer,
    y: u16,
    xs: std::ops::Range<u16>,
) -> String {
    let mut row = String::new();
    let mut x = xs.start;
    while x < xs.end {
        let symbol = buffer[(x, y)].symbol();
        row.push_str(symbol);
        x += width_of(symbol).max(1) as u16;
    }
    row
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::commit_editor::{CommitEditor, Field};
    use crate::git::diff::{DiffLine, LineKind};
    use crate::git::status::EntryKind;
    use crate::model::CommitFlow;

    /// Draw into a `width` x `height` buffer and return its rows as text.
    fn draw(state: &AppState, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(state, frame)).unwrap();
        buffer_rows(terminal.backend().buffer())
    }

    /// Draw a workspace whose Files panel is exactly `panel` columns wide and
    /// return only that panel's columns of each row.
    fn draw_files(state: &AppState, panel: u16, height: u16) -> Vec<String> {
        let width = (panel..panel * 4)
            .find(|&w| {
                let [files, _] =
                    Layout::horizontal([Constraint::Percentage(30), Constraint::Fill(1)])
                        .areas::<2>(Rect::new(0, 0, w, 1));
                files.width == panel
            })
            .expect("a total width for this panel width");
        draw(state, width, height)
            .into_iter()
            .map(|row| {
                let mut used = 0;
                row.chars()
                    .take_while(|c| {
                        used += width_of(c.encode_utf8(&mut [0; 4])).max(1);
                        used <= usize::from(panel)
                    })
                    .collect()
            })
            .collect()
    }

    fn contains(rows: &[String], needle: &str) -> bool {
        rows.iter().any(|row| row.contains(needle))
    }

    fn file(path: &str) -> FileEntry {
        FileEntry {
            path: path.into(),
            orig_path: None,
            index: '.',
            worktree: 'M',
            kind: EntryKind::Changed,
        }
    }

    fn renamed(orig: &str, path: &str) -> FileEntry {
        FileEntry {
            path: path.into(),
            orig_path: Some(orig.into()),
            index: 'R',
            worktree: '.',
            kind: EntryKind::Renamed,
        }
    }

    fn loaded(files: Vec<FileEntry>) -> AppState {
        let mut state = AppState::new(Context::Workspace);
        state.files_loaded = true;
        state.files = files;
        state
    }

    #[test]
    fn review_bar_shows_only_global_keys() {
        let rows = draw(&AppState::new(Context::Review { number: 1 }), 40, 5);
        assert!(rows[4].starts_with(" ? keys  q quit "), "{rows:#?}");
    }

    #[test]
    fn the_bar_offers_close_while_help_is_open() {
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        state.help_open = true;
        let rows = draw(&state, 80, 20);
        assert!(rows[19].starts_with(" ? close  q quit "), "{rows:#?}");
    }

    #[test]
    fn a_disabled_hint_is_dimmed() {
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        let buffer = draw_buffer(&state, 80, 8);
        let (x, y) = find(&buffer, "space stage");
        assert_ne!(buffer[(x, y)].style().fg, Some(Color::DarkGray));
        let (x, y) = find(&buffer, "r refresh");
        assert_ne!(buffer[(x, y)].style().fg, Some(Color::DarkGray));

        state.op_seq = Some(1);
        let buffer = draw_buffer(&state, 80, 8);
        let (x, y) = find(&buffer, "space stage");
        assert_eq!(buffer[(x, y)].style().fg, Some(Color::DarkGray));
        let (x, y) = find(&buffer, "r refresh");
        assert_ne!(buffer[(x, y)].style().fg, Some(Color::DarkGray));
    }

    #[test]
    fn a_notice_replaces_the_hints_in_yellow_after_loading() {
        let mut state = loaded(vec![file("a.rs")]);
        state.notice = Some("An operation is running");
        state.op_error = Some("older failure".into());
        state.busy = 1;
        let buffer = draw_buffer(&state, 80, 8);
        let rows = draw(&state, 80, 8);
        assert!(
            rows[7].starts_with(" loading…  An operation is running"),
            "{rows:#?}"
        );
        assert!(!contains(&rows, "space stage"), "{rows:#?}");
        assert!(!contains(&rows, "older failure"), "{rows:#?}");
        let (x, y) = find(&buffer, "An operation");
        assert_eq!(buffer[(x, y)].style().fg, Some(Color::Yellow));
    }

    #[test]
    fn help_lists_the_keys_of_the_focused_panel() {
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        state.help_open = true;
        let rows = draw(&state, 80, 24);
        assert!(contains(&rows, " Keys: Files "), "{rows:#?}");
        let row = |needle: &str| {
            rows.iter()
                .find(|row| row.contains(needle))
                .unwrap_or_else(|| panic!("{needle:?} not in {rows:#?}"))
                .clone()
        };
        assert!(
            row("Stage the selected file").contains("space "),
            "{rows:#?}"
        );
        assert!(row("Reload the list").contains(" r "), "{rows:#?}");
        assert!(row("Quit Kika").contains("q/esc/ctrl-c "), "{rows:#?}");
        assert!(row("Select the next file").contains("j/↓ "), "{rows:#?}");
        // Help's own close key is in the bar, not in the panel's list.
        assert!(!contains(&rows, "Close this menu"), "{rows:#?}");
    }

    #[test]
    fn disabled_help_rows_are_dimmed() {
        let mut state = loaded(vec![]);
        state.help_open = true;
        let buffer = draw_buffer(&state, 80, 24);
        let (x, y) = find(&buffer, "Stage the selected file");
        assert_eq!(buffer[(x, y)].style().fg, Some(Color::DarkGray));
        let (x, y) = find(&buffer, "Quit Kika");
        assert_ne!(buffer[(x, y)].style().fg, Some(Color::DarkGray));
    }

    #[test]
    fn tiny_terminals_do_not_panic_with_help_open() {
        let mut state = loaded(vec![file("a.rs")]);
        state.help_open = true;
        state.notice = Some("No file selected");
        for context in [Context::Workspace, Context::Review { number: 2 }] {
            state.context = context;
            for (w, h) in [(0, 0), (1, 1), (10, 2), (2, 10), (3, 3), (5, 4), (20, 6)] {
                draw(&state, w, h);
            }
        }
    }

    #[test]
    fn review_context_shows_number_in_title() {
        let rows = draw(&AppState::new(Context::Review { number: 123 }), 30, 5);
        assert!(rows[0].contains("Review #123"), "{rows:#?}");
    }

    #[test]
    fn workspace_shows_loading_before_status_arrives() {
        let mut state = AppState::new(Context::Workspace);
        state.busy = 1;
        let rows = draw(&state, 40, 8);
        assert!(rows[0].contains(" Files "), "{rows:#?}");
        assert!(contains(&rows, "│Loading…"), "{rows:#?}");
        assert!(rows[7].contains("loading…"), "{rows:#?}");
    }

    #[test]
    fn workspace_lists_files_with_status_letters_and_renames() {
        let mut state = loaded(vec![renamed("old.txt", "new.txt")]);
        state.files.push(FileEntry {
            path: "new_file".into(),
            orig_path: None,
            index: '?',
            worktree: '?',
            kind: EntryKind::Untracked,
        });
        let rows = draw(&state, 80, 24);
        assert!(contains(&rows, "│R  old.txt → new.txt "), "{rows:#?}");
        assert!(contains(&rows, "│?? new_file "), "{rows:#?}");
        assert!(!contains(&rows, "loading…"), "{rows:#?}");
        assert!(
            rows[23].starts_with(" space stage  a all  c commit  r refresh  ? keys  q quit"),
            "{rows:#?}"
        );
    }

    #[test]
    fn workspace_says_no_changes_when_clean() {
        assert!(contains(&draw(&loaded(vec![]), 40, 8), "No changes"));
    }

    #[test]
    fn workspace_shows_status_error_wrapped_with_a_retry_hint() {
        let mut state = loaded(vec![]);
        state.status_error =
            Some("fatal: not a git repository (or any of the parent directories): .git".into());
        let rows = draw_files(&state, 40, 8);
        assert!(contains(&rows, "│fatal: not a git repository"), "{rows:#?}");
        assert!(contains(&rows, "directories): .git"), "{rows:#?}");
        assert!(contains(&rows, "│Press r to retry"), "{rows:#?}");
    }

    #[test]
    fn multi_line_error_keeps_its_lines() {
        let mut state = loaded(vec![]);
        state.status_error = Some("error: first\nhint: second".into());
        let rows = draw_files(&state, 40, 8);
        assert!(contains(&rows, "│error: first "), "{rows:#?}");
        assert!(contains(&rows, "│hint: second "), "{rows:#?}");
    }

    #[test]
    fn long_path_is_left_elided_to_fit_the_panel() {
        // Inner width 18: " M " leaves 15 columns for the path.
        let state = loaded(vec![file("src/deeply/nested/module/file.rs")]);
        let rows = draw_files(&state, 20, 6);
        assert_eq!(rows[1], "│ M …module/file.rs│", "{rows:#?}");
    }

    #[test]
    fn long_rename_keeps_the_new_path_and_elides_the_old_one_first() {
        // Inner width 28: "R  " + orig + " → " + path.
        let state = loaded(vec![renamed("src/old/location/a.rs", "src/new/b.rs")]);
        let rows = draw_files(&state, 30, 6);
        assert_eq!(rows[1], "│R  …tion/a.rs → src/new/b.rs│", "{rows:#?}");
    }

    #[test]
    fn very_long_rename_elides_both_paths() {
        let state = loaded(vec![renamed("old/name.rs", "src/brand/new/location/b.rs")]);
        let rows = draw_files(&state, 20, 6);
        assert_eq!(rows[1], "│R  … → …ation/b.rs│", "{rows:#?}");
    }

    #[test]
    fn wide_characters_are_measured_by_display_width() {
        // Each CJK glyph is two columns wide; inner width 14 leaves 11 for the path.
        let state = loaded(vec![file("日本語/日本語/長い名前.md")]);
        let rows = draw_files(&state, 16, 6);
        assert_eq!(rows[1], "│ M …い名前.md │", "{rows:#?}");
        assert!(width_of(&rows[1]) <= 16, "{rows:#?}");
    }

    #[test]
    fn overflowing_list_says_how_many_files_are_hidden() {
        // Height 7: one status row and two borders leave 4 rows, so 3 files and "+N more".
        let files = (0..10).map(|i| file(&format!("f{i}"))).collect();
        let rows = draw_files(&loaded(files), 30, 7);
        assert!(contains(&rows, "│ M f2 "), "{rows:#?}");
        assert!(!contains(&rows, "│ M f3 "), "{rows:#?}");
        assert!(contains(&rows, "│+7 more "), "{rows:#?}");
    }

    #[test]
    fn list_that_fits_exactly_has_no_more_line() {
        let files = (0..4).map(|i| file(&format!("f{i}"))).collect();
        let rows = draw_files(&loaded(files), 30, 7);
        assert!(contains(&rows, "│ M f3 "), "{rows:#?}");
        assert!(!contains(&rows, "more"), "{rows:#?}");
    }

    #[test]
    fn busy_indicator_survives_a_narrow_status_bar() {
        let mut state = loaded(vec![]);
        state.busy = 1;
        let rows = draw(&state, 12, 4);
        assert!(rows[3].contains("loading…"), "{rows:#?}");
    }

    #[test]
    fn tiny_terminals_do_not_panic() {
        let mut state = loaded((0..5).map(|i| file(&format!("file-{i}"))).collect());
        state
            .files
            .push(renamed("a/very/long/old", "a/very/long/new"));
        for (w, h) in [(0, 0), (1, 1), (10, 2), (2, 10), (3, 3), (5, 4)] {
            draw(&state, w, h);
            state.status_error = Some("boom".into());
            draw(&state, w, h);
            state.status_error = None;
        }
    }

    #[test]
    fn an_op_error_replaces_the_hints_with_its_first_line_in_red() {
        let mut state = loaded(vec![file("a.rs")]);
        state.op_error = Some("fatal: Unable to create index.lock\nhint: another git".into());
        let buffer = draw_buffer(&state, 80, 8);
        let rows = draw(&state, 80, 8);
        assert!(
            rows[7].starts_with(" error: fatal: Unable to create index.lock"),
            "{rows:#?}"
        );
        assert!(!contains(&rows, "hint: another git"), "{rows:#?}");
        assert!(!contains(&rows, "space stage"), "{rows:#?}");
        let (x, y) = find(&buffer, "error:");
        assert_eq!(buffer[(x, y)].style().fg, Some(Color::Red));
    }

    #[test]
    fn an_op_error_keeps_the_busy_indicator_in_front() {
        let mut state = loaded(vec![file("a.rs")]);
        state.op_error = Some("boom".into());
        state.busy = 1;
        let rows = draw(&state, 80, 8);
        assert!(rows[7].starts_with(" loading…  error: boom"), "{rows:#?}");
    }

    #[test]
    fn tiny_terminals_do_not_panic_with_an_op_error() {
        let mut state = loaded(vec![file("a.rs")]);
        state.op_error = Some("fatal: a long error line\nsecond".into());
        for busy in [0, 1] {
            state.busy = busy;
            for (w, h) in [(0, 0), (1, 1), (10, 2), (2, 10), (3, 3), (5, 4)] {
                draw(&state, w, h);
            }
        }
    }

    fn draw_buffer(state: &AppState, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(state, frame)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn diff_line(kind: LineKind, text: &str) -> DiffLine {
        DiffLine {
            kind,
            text: text.into(),
        }
    }

    /// A workspace with one file selected and `lines` fully loaded.
    fn with_diff(lines: Vec<DiffLine>) -> AppState {
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        state.diff.seq = Some(0);
        state.diff.path = Some("a.rs".into());
        state.diff.lines = lines;
        state.diff.done = true;
        state
    }

    /// Where `needle` starts on screen, by cell.
    fn find(buffer: &ratatui::buffer::Buffer, needle: &str) -> (u16, u16) {
        let first = needle.chars().next().unwrap().to_string();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                let rest: String = (x..buffer.area.width)
                    .take(needle.chars().count())
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect();
                if buffer[(x, y)].symbol() == first && rest == needle {
                    return (x, y);
                }
            }
        }
        panic!("{needle:?} not on screen");
    }

    #[test]
    fn diff_lines_are_coloured_inside_the_diff_block() {
        use LineKind::*;
        let state = with_diff(vec![
            diff_line(Header, "diff --git a/a.rs b/a.rs"),
            diff_line(Header, "--- a/a.rs"),
            diff_line(Header, "+++ b/a.rs"),
            diff_line(Hunk, "@@ -1 +1 @@"),
            diff_line(Removed, "-old"),
            diff_line(Added, "+new"),
            diff_line(Context, " same"),
        ]);
        let buffer = draw_buffer(&state, 80, 12);
        let (title_x, title_y) = find(&buffer, " Diff ");
        let style = |needle| {
            let (x, y) = find(&buffer, needle);
            assert!(
                x >= title_x && y > title_y,
                "{needle:?} is inside the block"
            );
            buffer[(x, y)].style()
        };

        assert_eq!(style("-old").fg, Some(Color::Red));
        assert_eq!(style("+new").fg, Some(Color::Green));
        assert_eq!(style("@@ -1").fg, Some(Color::Cyan));
        let header = style("diff --git");
        assert!(header.add_modifier.contains(Modifier::BOLD));
        for needle in ["--- a/a.rs", "+++ b/a.rs"] {
            let header = style(needle);
            assert!(header.add_modifier.contains(Modifier::BOLD), "{needle}");
            assert!(
                matches!(header.fg, None | Some(Color::Reset)),
                "{needle} is not coloured as a change"
            );
        }
        assert!(matches!(style(" same").fg, None | Some(Color::Reset)));
    }

    #[test]
    fn only_the_scrolled_window_of_a_long_diff_is_drawn() {
        let mut state = with_diff(
            (0..50_000)
                .map(|i| diff_line(LineKind::Context, &format!("line {i}")))
                .collect(),
        );
        state.diff.scroll = 1_000;
        let rows = draw(&state, 80, 10);
        assert!(contains(&rows, "│line 1000 "), "{rows:#?}");
        assert!(contains(&rows, "│line 1006 "), "{rows:#?}");
        assert!(!contains(&rows, "line 999 "), "{rows:#?}");
        assert!(!contains(&rows, "line 1007 "), "{rows:#?}");
    }

    #[test]
    fn slow_diff_shows_loading_in_the_diff_pane() {
        let mut state = with_diff(vec![diff_line(LineKind::Added, "+old file")]);
        state.diff.pending = true;
        state.diff.done = false;
        state.diff.show_loading = true;
        let rows = draw(&state, 80, 8);
        assert!(contains(&rows, "│Loading…"), "{rows:#?}");
        assert!(!contains(&rows, "+old file"), "{rows:#?}");
    }

    #[test]
    fn a_pending_diff_keeps_showing_the_previous_lines() {
        let mut state = with_diff(vec![diff_line(LineKind::Added, "+old file")]);
        state.diff.pending = true;
        state.diff.done = false;
        let rows = draw(&state, 80, 8);
        assert!(contains(&rows, "│+old file "), "{rows:#?}");
        assert!(!contains(&rows, "Loading"), "{rows:#?}");
    }

    #[test]
    fn an_empty_finished_diff_says_no_diff() {
        let rows = draw(&with_diff(vec![]), 80, 8);
        assert!(contains(&rows, "│No diff"), "{rows:#?}");
    }

    #[test]
    fn a_diff_error_is_shown_in_the_diff_pane() {
        let mut state = with_diff(vec![]);
        state.diff.error = Some("fatal: bad object".into());
        let rows = draw(&state, 80, 8);
        assert!(contains(&rows, "│fatal: bad object"), "{rows:#?}");
        assert!(!contains(&rows, "No diff"), "{rows:#?}");
    }

    #[test]
    fn the_selected_file_row_is_reversed_across_the_panel() {
        let mut state = loaded(vec![file("a.rs"), file("b.rs")]);
        state.selected = Some(1);
        let buffer = draw_buffer(&state, 100, 8);
        let (x, y) = find(&buffer, " M b.rs");
        assert!(
            buffer[(x, y)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        // The padding after the name is highlighted too.
        assert!(
            buffer[(x + 10, y)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        let (x, y) = find(&buffer, " M a.rs");
        assert!(
            !buffer[(x, y)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn a_selection_below_the_first_screen_stays_visible() {
        // Height 7: 4 rows, so 3 files plus "+N more" at a time.
        let mut state = loaded((0..10).map(|i| file(&format!("f{i}"))).collect());
        state.selected = Some(5);
        let rows = draw_files(&state, 30, 7);
        assert!(contains(&rows, "│ M f5 "), "{rows:#?}");
        assert!(!contains(&rows, "│ M f2 "), "{rows:#?}");
        assert!(contains(&rows, "│+4 more "), "{rows:#?}");

        // Near the end the window shows the last files and no "more" row.
        state.selected = Some(8);
        let rows = draw_files(&state, 30, 7);
        for name in ["f6", "f7", "f8", "f9"] {
            assert!(contains(&rows, &format!("│ M {name} ")), "{rows:#?}");
        }
        assert!(!contains(&rows, "more"), "{rows:#?}");
    }

    #[test]
    fn a_one_row_list_shows_the_selection() {
        // Height 4: one row inside the Files border.
        let mut state = loaded((0..10).map(|i| file(&format!("f{i}"))).collect());
        state.selected = Some(4);
        let rows = draw_files(&state, 30, 4);
        assert!(contains(&rows, "│ M f4 "), "{rows:#?}");
    }

    #[test]
    fn tiny_terminals_do_not_panic_with_a_diff_loaded() {
        let mut state = with_diff(
            (0..100)
                .map(|i| diff_line(LineKind::Added, &format!("+\u{65e5}\u{672c} {i}")))
                .collect(),
        );
        state.files = (0..20).map(|i| file(&format!("file-{i}"))).collect();
        for (selected, scroll) in [(0, 0), (19, 99), (10, 500)] {
            state.selected = Some(selected);
            state.diff.scroll = scroll;
            for (w, h) in [(0, 0), (1, 1), (10, 2), (2, 10), (3, 3), (5, 4), (7, 5)] {
                draw(&state, w, h);
            }
        }
    }

    fn commit_editor(subject: &str, body: &[&str]) -> CommitEditor {
        let mut editor = CommitEditor::new(false);
        editor.subject = subject.into();
        editor.body = body.iter().map(|line| line.to_string()).collect();
        editor.col = subject.chars().count();
        editor
    }

    fn with_commit(commit: CommitFlow) -> AppState {
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        state.commit = commit;
        state
    }

    #[test]
    fn the_stage_all_confirm_asks_and_the_bar_offers_its_keys() {
        let rows = draw(&with_commit(CommitFlow::ConfirmStageAll), 80, 12);
        assert!(contains(&rows, " Commit "), "{rows:#?}");
        assert!(
            contains(
                &rows,
                "Nothing is staged. Stage all changes and commit? (y/n)"
            ),
            "{rows:#?}"
        );
        assert!(rows[11].starts_with(" y stage all  n cancel"), "{rows:#?}");
    }

    #[test]
    fn the_editor_shows_subject_and_body_with_the_cursor_in_the_subject() {
        let editor = commit_editor("Fix the thing", &["line 1", "line 2"]);
        let state = with_commit(CommitFlow::Editing(editor));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| render(&state, frame)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let rows = buffer_rows(&buffer);

        assert!(contains(&rows, " Commit message "), "{rows:#?}");
        assert!(contains(&rows, " Subject "), "{rows:#?}");
        assert!(contains(&rows, " Body "), "{rows:#?}");
        let (x, y) = find(&buffer, "Fix the thing");
        assert!(contains(&rows, "│line 1 "), "{rows:#?}");
        assert!(contains(&rows, "│line 2 "), "{rows:#?}");
        let line1 = find(&buffer, "line 1");
        assert!(line1.1 > y, "the body is below the subject");
        // The cursor sits after the subject, and its box is highlighted.
        let cursor = terminal.get_cursor_position().unwrap();
        assert_eq!((cursor.x, cursor.y), (x + 13, y));
        assert_eq!(buffer[(x - 1, y)].style().fg, Some(FOCUSED));
        assert_ne!(buffer[(line1.0 - 1, line1.1)].style().fg, Some(FOCUSED));
        assert!(
            rows[23].starts_with(
                " q and ? are typed  ctrl-s commit  Tab subject/body  esc cancel  ctrl-c quit"
            ),
            "{rows:#?}"
        );
    }

    #[test]
    fn the_cursor_follows_the_body_row_and_col() {
        let mut editor = commit_editor("s", &["ab", "日本x", "cd"]);
        editor.field = Field::Body;
        editor.row = 1;
        editor.col = 2;
        let state = with_commit(CommitFlow::Editing(editor));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| render(&state, frame)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        // `find` reads one cell per char, so locate the wide line by its neighbour.
        let (x, below) = find(&buffer, "cd");
        let y = below - 1;
        assert_eq!(buffer[(x, y)].symbol(), "日");
        let cursor = terminal.get_cursor_position().unwrap();
        // Two wide glyphs before the cursor: four columns.
        assert_eq!((cursor.x, cursor.y), (x + 4, y));
        assert_eq!(buffer[(x - 1, y)].style().fg, Some(FOCUSED));
    }

    #[test]
    fn a_long_body_scrolls_to_keep_the_cursor_row_visible() {
        let lines: Vec<String> = (0..40).map(|i| format!("body {i}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut editor = commit_editor("s", &refs);
        editor.field = Field::Body;
        editor.row = 39;
        editor.col = 0;
        let state = with_commit(CommitFlow::Editing(editor));
        let rows = draw(&state, 80, 24);
        assert!(contains(&rows, "│body 39 "), "{rows:#?}");
        assert!(!contains(&rows, "│body 0 "), "{rows:#?}");
    }

    #[test]
    fn only_the_focused_box_scrolls_to_the_cursor() {
        let long = |start: &str| format!("{start}{}", "x".repeat(100));
        let editor = commit_editor(&long("subject"), &[&long("body")]);
        let rows = draw(&with_commit(CommitFlow::Editing(editor)), 80, 24);
        assert!(contains(&rows, "│bodyxxx"), "{rows:#?}");
        assert!(!contains(&rows, "│subjectxxx"), "{rows:#?}");

        let mut editor = commit_editor(&long("subject"), &[&long("body")]);
        editor.field = Field::Body;
        editor.col = 104;
        let rows = draw(&with_commit(CommitFlow::Editing(editor)), 80, 24);
        assert!(contains(&rows, "│subjectxxx"), "{rows:#?}");
        assert!(!contains(&rows, "│bodyxxx"), "{rows:#?}");
    }

    #[test]
    fn a_running_commit_says_committing() {
        let state = with_commit(CommitFlow::Running(commit_editor("subj", &[""])));
        let rows = draw(&state, 80, 24);
        assert!(contains(&rows, " Committing… "), "{rows:#?}");
        assert!(contains(&rows, "subj"), "{rows:#?}");
    }

    #[test]
    fn a_failed_commit_shows_the_output_and_how_to_go_back() {
        let state = with_commit(CommitFlow::Failed {
            editor: commit_editor("subj", &[""]),
            output: "lint failed\nsrc/a.rs:3: unused import".into(),
        });
        let rows = draw(&state, 80, 24);
        assert!(contains(&rows, " Commit failed "), "{rows:#?}");
        assert!(contains(&rows, "lint failed"), "{rows:#?}");
        assert!(contains(&rows, "src/a.rs:3: unused import"), "{rows:#?}");
        assert!(contains(&rows, "Esc: back to the message"), "{rows:#?}");
    }

    #[test]
    fn long_failure_output_shows_its_tail_and_says_what_is_cut() {
        let output: Vec<String> = (0..100).map(|i| format!("error {i}")).collect();
        let state = with_commit(CommitFlow::Failed {
            editor: commit_editor("subj", &[""]),
            output: output.join("\n"),
        });
        let rows = draw(&state, 80, 24);
        // 23 rows: two borders and the hint leave 20, one of them the cut line.
        assert!(contains(&rows, "│… 81 earlier lines "), "{rows:#?}");
        assert!(contains(&rows, "│error 81 "), "{rows:#?}");
        assert!(contains(&rows, "│error 99 "), "{rows:#?}");
        assert!(!contains(&rows, "│error 80 "), "{rows:#?}");
        assert!(contains(&rows, "Esc: back to the message"), "{rows:#?}");
    }

    #[test]
    fn failure_lines_wider_than_the_popup_wrap_and_count_as_rows() {
        let state = with_commit(CommitFlow::Failed {
            editor: commit_editor("subj", &[""]),
            output: format!("{}\nlast", "w".repeat(100)),
        });
        let rows = draw(&state, 40, 12);
        assert!(
            contains(&rows, &format!("│{}│", "w".repeat(38))),
            "{rows:#?}"
        );
        assert!(contains(&rows, "│last "), "{rows:#?}");
        assert!(!contains(&rows, "earlier"), "{rows:#?}");
    }

    #[test]
    fn the_running_popup_says_how_to_get_out() {
        let state = with_commit(CommitFlow::Running(commit_editor("subj", &[""])));
        let rows = draw(&state, 80, 24);
        assert!(contains(&rows, "ctrl-c quits Kika"), "{rows:#?}");
    }

    #[test]
    fn the_discard_confirm_asks_before_throwing_the_message_away() {
        let state = with_commit(CommitFlow::ConfirmDiscard(commit_editor("subj", &[""])));
        let rows = draw(&state, 80, 12);
        assert!(
            contains(&rows, "Discard the typed message? (y/n)"),
            "{rows:#?}"
        );
        assert!(rows[11].starts_with(" y discard  n keep"), "{rows:#?}");
    }

    #[test]
    fn a_narrow_confirm_wraps_and_keeps_y_n_visible() {
        let rows = draw(&with_commit(CommitFlow::ConfirmStageAll), 40, 12);
        assert!(contains(&rows, "(y/n)"), "{rows:#?}");
        assert!(contains(&rows, "Nothing is staged."), "{rows:#?}");
    }

    /// Draw and return the buffer and where the terminal cursor ended up.
    fn draw_with_cursor(
        state: &AppState,
        width: u16,
        height: u16,
    ) -> (ratatui::buffer::Buffer, (u16, u16)) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(state, frame)).unwrap();
        let cursor = terminal.get_cursor_position().unwrap();
        (terminal.backend().buffer().clone(), (cursor.x, cursor.y))
    }

    #[test]
    fn wrap_breaks_at_spaces_keeps_indentation_and_splits_long_words() {
        assert_eq!(
            wrap("  --> src/a.rs:3 unused", 16),
            ["  --> src/a.rs:3", "unused"]
        );
        assert_eq!(wrap("one two three", 7), ["one two", "three"]);
        assert_eq!(wrap("abcdefgh", 3), ["abc", "def", "gh"]);
        assert_eq!(wrap("a\n\nb", 5), ["a", "", "b"]);
        // A glyph wider than the row still makes progress.
        assert_eq!(wrap("日本", 1), ["日", "本"]);
        assert_eq!(wrap("", 0), Vec::<String>::new());
    }

    #[test]
    fn clip_marks_cut_text_and_blanks_split_wide_glyphs() {
        assert_eq!(clip("abcdef", 0, 4), "abc…");
        assert_eq!(clip("abcdef", 1, 4), "…cd…");
        assert_eq!(clip("abcdef", 3, 4), "…ef");
        assert_eq!(clip("ab", 0, 4), "ab");
        assert_eq!(clip("日本語", 1, 4), "…本…");
        assert_eq!(clip("日本語", 1, 5), "…本語");
        assert_eq!(clip("日本語", 0, 3), "日…");
        assert_eq!(clip("anything", 0, 0), "");
    }

    #[test]
    fn the_focused_box_is_marked_in_text_not_only_colour() {
        let state = with_commit(CommitFlow::Editing(commit_editor("s", &[""])));
        let rows = draw(&state, 80, 24);
        assert!(contains(&rows, "▸ Subject "), "{rows:#?}");
        assert!(!contains(&rows, "▸ Body "), "{rows:#?}");
    }

    #[test]
    fn a_short_terminal_still_shows_the_focused_body_and_its_cursor() {
        let mut editor = commit_editor("subj", &["body text"]);
        editor.field = Field::Body;
        editor.col = 4;
        let state = with_commit(CommitFlow::Editing(editor));
        for height in [6, 8, 9] {
            let (buffer, cursor) = draw_with_cursor(&state, 80, height);
            let rows = buffer_rows(&buffer);
            assert!(contains(&rows, "▸ Body "), "{height}: {rows:#?}");
            let (x, y) = find(&buffer, "body text");
            assert_eq!(cursor, (x + 4, y), "{height}: {rows:#?}");
        }
    }

    #[test]
    fn a_long_subject_scrolls_with_a_marker_and_the_cursor_stays_inside() {
        let editor = commit_editor(&format!("start{}", "a".repeat(95)), &[""]);
        let state = with_commit(CommitFlow::Editing(editor));
        let (buffer, cursor) = draw_with_cursor(&state, 80, 24);
        let rows = buffer_rows(&buffer);
        // The popup is 72 wide, so the Subject box has 68 text columns.
        let (x, y) = find(&buffer, "▸ Subject");
        // The title is " ▸ Subject ", so the box's inside starts one left of "▸".
        let inner_x = x - 1;
        assert_eq!(buffer[(inner_x, y + 1)].symbol(), "…", "{rows:#?}");
        assert!(!contains(&rows, "start"), "{rows:#?}");
        // The cursor is after the last char, one column short of the edge.
        assert_eq!(cursor, (inner_x + 66, y + 1));
        assert_eq!(buffer[(inner_x + 65, y + 1)].symbol(), "a");
    }

    #[test]
    fn a_wide_glyph_cut_by_the_scroll_edge_does_not_shift_the_cursor() {
        // A 71-column screen makes the Subject box 67 wide: an odd scroll
        // offset cuts a two-column glyph in half.
        let editor = commit_editor(&"日".repeat(60), &[""]);
        let state = with_commit(CommitFlow::Editing(editor));
        let (buffer, cursor) = draw_with_cursor(&state, 71, 24);
        let (x, y) = find(&buffer, "▸ Subject");
        // The title is " ▸ Subject ", so the box's inside starts one left of "▸".
        let inner_x = x - 1;
        assert_eq!(buffer[(inner_x, y + 1)].symbol(), "…");
        assert_eq!(cursor, (inner_x + 65, y + 1));
        assert_eq!(buffer[(inner_x + 63, y + 1)].symbol(), "日");
    }

    #[test]
    fn an_unfocused_line_cut_at_the_right_ends_in_a_marker() {
        let editor = commit_editor("s", &[&"b".repeat(100)]);
        let state = with_commit(CommitFlow::Editing(editor));
        let rows = draw(&state, 80, 24);
        assert!(
            contains(&rows, &format!("│{}…│", "b".repeat(67))),
            "{rows:#?}"
        );
    }

    #[test]
    fn tiny_terminals_do_not_panic_with_a_commit_popup() {
        let mut editor = commit_editor("a long subject line", &["one", "two", "three"]);
        editor.field = Field::Body;
        editor.row = 2;
        editor.col = 5;
        let flows = [
            CommitFlow::ConfirmStageAll,
            CommitFlow::Editing(editor.clone()),
            CommitFlow::Running(editor.clone()),
            CommitFlow::Failed {
                editor,
                output: "a long hook failure line that wraps\nand more".into(),
            },
        ];
        for flow in flows {
            let state = with_commit(flow);
            for (w, h) in [(0, 0), (1, 1), (10, 2), (2, 10), (3, 3), (5, 4), (20, 6)] {
                draw(&state, w, h);
            }
        }
    }

    #[test]
    fn a_20_by_6_screen_still_shows_the_typed_subject() {
        let state = with_commit(CommitFlow::Editing(commit_editor("subj", &[""])));
        let rows = draw(&state, 20, 6);
        assert!(contains(&rows, "subj"), "{rows:#?}");
    }
}
