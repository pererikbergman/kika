use crate::blame_view::{BlameView, CommitView};
use crate::commit_editor::{CommitEditor, Field};
use crate::diff_pane::DiffPane;
use crate::git::FileEntry;
use crate::git::blame::BlameLine;
use crate::git::diff::{DiffLine, DiffOptions, DiffSide, LineKind, sanitize, sanitize_body};
use crate::git::patch::{FileKind, PatchError};
use crate::git::repo_state::Operation;
use crate::git::status::Head;
use crate::hunk_view::{HunkView, NO_CHANGES, RowKind};
use crate::keymap::{KeyPattern, available, available_commands, registry};
use crate::model::{AppState, CommitFlow, Context, Focus};
use crate::popup_view;
use crate::rendered_view::RenderedDiff;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

/// Draw the state. Reads the model only; never mutates it.
pub fn render(state: &AppState, frame: &mut Frame) {
    let [main, status] = screen_layout(frame.area());

    match state.context {
        Context::Workspace => {
            let areas = workspace_layout(main);
            render_status_panel(state, frame, areas.status);
            render_files_panel(state, frame, areas.files);
            let label = state.active_renderer().map(|r| r.label.as_str());
            // R steps to the next renderer before it gets back to built-in.
            let last = state
                .renderer
                .is_some_and(|i| i + 1 >= state.renderers.len());
            // R is a Diff pane key.
            let hint = match (matches!(state.focus, Focus::Files), last) {
                (false, true) => "Press R for the built-in diff",
                (false, false) => "Press R for the next renderer",
                (true, true) => "Press enter, then R, for the built-in diff",
                (true, false) => "Press enter, then R, for the next renderer",
            };
            match (&state.focus, state.rendered_shown(), label) {
                (Focus::Files, Some(rendered), Some(label)) => {
                    let title = format!(" Diff{} · {label} ", options_note(state.diff_options));
                    let block = Block::bordered().title(title);
                    render_rendered(rendered, block, None, hint, frame, areas.diff);
                }
                (Focus::Files, ..) => {
                    render_diff_pane(&state.diff, state.diff_options, frame, areas.diff)
                }
                (Focus::Diff(view), Some(rendered), Some(label)) => {
                    let title = format!(" {} · {label} ", hunk_title(view, state.diff_options));
                    let block = focused_block(title);
                    render_rendered(rendered, block, Some(view), hint, frame, areas.diff);
                }
                (Focus::Diff(view), None, Some(label)) if state.rendered_for_shown().is_some() => {
                    // Short, so it fits the pane's title at 80 columns.
                    let suffix = format!("{label} (unmapped)");
                    render_hunk_view(view, state.diff_options, &suffix, frame, areas.diff)
                }
                (Focus::Diff(view), ..) => {
                    render_hunk_view(view, state.diff_options, "", frame, areas.diff)
                }
                (Focus::Blame(view), ..) => match &view.commit {
                    Some(commit) => render_commit_view(commit, frame, areas.diff),
                    None => render_blame(view, frame, areas.diff),
                },
            }
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
    popup_view::render_popups(state, frame, main);
    frame.render_widget(status_bar(state), status);
}

/// The screen split into the panels above and the one-row bar below.
pub(crate) fn screen_layout(area: Rect) -> [Rect; 2] {
    Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area)
}

/// Where the workspace panels go. Shared with the e2e harness, so it reads
/// the Files panel where it is drawn.
pub(crate) struct WorkspaceLayout {
    pub status: Rect,
    pub files: Rect,
    pub diff: Rect,
}

/// Height of the Status panel: three rows of text inside the border.
const STATUS_PANEL_HEIGHT: u16 = 5;

/// Rows the Files panel keeps before the Status panel gets any: its
/// borders and three files. Files is where the user works; Status only informs.
const FILES_MIN_HEIGHT: u16 = 5;

/// A left column of the Status panel over the Files panel, and the Diff
/// pane on the right. On a short screen the Status panel shrinks first,
/// and goes when it cannot show even one row.
pub(crate) fn workspace_layout(main: Rect) -> WorkspaceLayout {
    let [left, diff] =
        Layout::horizontal([Constraint::Percentage(30), Constraint::Fill(1)]).areas(main);
    let room = left.height.saturating_sub(FILES_MIN_HEIGHT);
    let status_height = match STATUS_PANEL_HEIGHT.min(room) {
        // Two borders and a row of text, or nothing.
        h if h < 3 => 0,
        h => h,
    };
    let [status, files] =
        Layout::vertical([Constraint::Length(status_height), Constraint::Fill(1)]).areas(left);
    WorkspaceLayout {
        status,
        files,
        diff,
    }
}

/// Where the user is: the branch, its upstream, and any operation in
/// progress. Names too wide for the panel lose their start to `…`, so the
/// counts and markers after them stay visible. Blank when no load has
/// succeeded yet, since the Files panel shows why; dimmed when the last
/// refresh failed, since it may be out of date.
fn render_status_panel(state: &AppState, frame: &mut Frame, area: Rect) {
    if area.is_empty() {
        return;
    }
    let stale = state.status_error.is_some() && state.branch.is_some();
    // Inside a submodule, say which one: the paths from the launch
    // repository. A long one loses its start, so the innermost name shows.
    let location: Vec<&str> = state.repo_stack.iter().map(|p| p.path.display()).collect();
    let location = if location.is_empty() {
        String::new()
    } else {
        let fixed = width_of(" Status ·  ") + if stale { width_of("(stale) ") } else { 0 };
        let room = usize::from(area.width.saturating_sub(2)).saturating_sub(fixed);
        format!(" · {}", elide_left(&location.join("/"), room.max(1)))
    };
    let block = if stale {
        Block::bordered()
            .title(format!(" Status{location} (stale) "))
            .style(Style::new().fg(Color::DarkGray))
    } else {
        Block::bordered().title(format!(" Status{location} "))
    };
    let width = usize::from(block.inner(area).width);
    let Some(branch) = &state.branch else {
        let text = if state.files_loaded { "" } else { "Loading…" };
        frame.render_widget(Paragraph::new(text).block(block), area);
        return;
    };
    let head = match &branch.head {
        Head::Branch(name) => fit(name, "", width),
        Head::Detached(oid) => fit("detached", &format!(" {}", short_hash(oid)), width),
        Head::Unborn(name) => fit(name, " (no commits)", width),
    };
    let upstream = match (&branch.upstream, branch.ahead_behind) {
        (None, _) => fit("no upstream", "", width),
        (Some(name), Some((ahead, behind))) => fit(name, &format!(" ↑{ahead} ↓{behind}"), width),
        (Some(name), None) => fit(name, " (gone)", width),
    };
    let operations: Vec<&str> = branch.in_progress.iter().map(|&op| doing(op)).collect();
    let lines = vec![
        Line::raw(head),
        Line::raw(upstream),
        Line::styled(
            elide_right(&operations.join(", "), width),
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ),
    ];
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// `name` then `suffix` in `width` columns. The name gives way first, so
/// the suffix (counts, markers) is cut only when it cannot fit at all.
fn fit(name: &str, suffix: &str, width: usize) -> String {
    let room = width.saturating_sub(width_of(suffix)).max(1);
    format!("{}{suffix}", elide_left(name, room))
}

/// Fit `text` into `max` columns by replacing its end with `…`.
fn elide_right(text: &str, max: usize) -> String {
    if width_of(text) <= max {
        return text.to_string();
    }
    let mut used = 0;
    let kept: String = text
        .chars()
        .take_while(|c| {
            used += width_of(c.encode_utf8(&mut [0; 4]));
            used < max
        })
        .collect();
    if max == 0 {
        String::new()
    } else {
        format!("{kept}…")
    }
}

/// Commit ids are shown at this length; enough to recognise one.
const SHORT_HASH: usize = 7;

fn short_hash(oid: &str) -> &str {
    oid.char_indices()
        .nth(SHORT_HASH)
        .map_or(oid, |(end, _)| &oid[..end])
}

/// What the repository is in the middle of, as the panel names it.
fn doing(op: Operation) -> &'static str {
    match op {
        Operation::Rebase => "rebasing",
        Operation::Merge => "merging",
        Operation::CherryPick => "cherry-picking",
        Operation::Bisect => "bisecting",
    }
}

/// The border colour of the commit box that has the cursor.
const FOCUSED: Color = Color::Cyan;

/// Popups wider than this are harder to read; it is a common message width.
pub(crate) const COMMIT_WIDTH: u16 = 72;

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
        CommitFlow::LoadingAmend { .. } => render_question(
            " Amend last commit ",
            "Loading the last commit…",
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
pub(crate) fn render_question(title: &str, question: &str, frame: &mut Frame, area: Rect) {
    let widest = question.lines().map(width_of).max().unwrap_or(0);
    let width = u16::try_from(widest + 4)
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
    let block = match (editing, editor.amend.is_some()) {
        (true, true) => Block::bordered().title(" Amend last commit "),
        (true, false) => Block::bordered().title(" Commit message "),
        (false, _) => Block::bordered()
            .title(" Committing… ")
            .title_bottom(" waiting for git · ctrl-c quits Kika "),
    };
    // Skipping hooks skips the user's own checks, so it is never quiet.
    let block = if editor.no_verify {
        block.title(Line::styled(" hooks skipped ", Style::new().fg(Color::Yellow)).right_aligned())
    } else {
        block
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
pub(crate) fn clip(line: &str, from: usize, width: usize) -> String {
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
pub(crate) fn wrap(text: &str, width: usize) -> Vec<String> {
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
    frame.render_widget(Paragraph::new(tail(&rows, usize::from(text.height))), text);
    frame.render_widget(
        Line::styled("Esc: back to the message", Style::new().fg(Color::Yellow)),
        hint,
    );
}

/// The last `room` of `rows`. When they do not all fit, the first row
/// says how many earlier ones are cut, since the end of git's output is
/// usually where it says what went wrong.
pub(crate) fn tail(rows: &[String], room: usize) -> Vec<Line<'_>> {
    let mut lines: Vec<Line> = Vec::new();
    let mut shown = rows;
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
    lines
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
        let hints = available(registry(), &state.keymap, focus, state)
            .into_iter()
            .filter(|(binding, _)| binding.bar);
        for (i, (binding, disabled)) in hints.enumerate() {
            let first = i == 0 && note.is_none();
            spans.push(Span::raw(if first { " " } else { "  " }));
            let key = state.keymap.keys(binding).first().map(KeyPattern::label);
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

/// A popup over `area` listing every key the focused panel answers to,
/// the user's own commands after the built-in ones. Rows that do not fit
/// are clipped.
fn render_help(state: &AppState, frame: &mut Frame, area: Rect) {
    let focus = state.panel_focus();
    let labels = |keys: &[KeyPattern]| {
        let labels: Vec<String> = keys.iter().map(KeyPattern::label).collect();
        labels.join("/")
    };
    let builtin = available(registry(), &state.keymap, focus, state)
        .into_iter()
        .map(|(binding, disabled)| {
            let keys = labels(state.keymap.keys(binding));
            (keys, binding.description.to_string(), disabled.is_some())
        });
    // The description is the user's text: one line, safe to draw.
    let commands = available_commands(&state.keymap, focus)
        .into_iter()
        .map(|command| {
            let description = sanitize(command.description.as_bytes());
            (labels(&command.keys), description, false)
        });
    let entries: Vec<(String, String, bool)> = builtin.chain(commands).collect();
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
pub(crate) fn centered(area: Rect, width: u16, height: u16) -> Rect {
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
fn reversed(mut line: Line<'_>, width: usize) -> Line<'_> {
    let pad = width.saturating_sub(line.width());
    line.spans.push(Span::raw(" ".repeat(pad)));
    line.style(Style::new().add_modifier(Modifier::REVERSED))
}

/// The diff options that differ from the defaults, for the pane title, so
/// a changed setting is never mistaken for the file's real diff.
fn options_note(options: DiffOptions) -> String {
    let default = DiffOptions::default();
    let mut note = String::new();
    if options.context != default.context {
        note.push_str(&format!(" · {} lines context", options.context));
    }
    if options.ignore_whitespace {
        note.push_str(" · whitespace hidden");
    }
    note
}

/// The Diff pane: an error, a delayed "Loading…", "No diff", or the visible lines.
fn render_diff_pane(diff: &DiffPane, options: DiffOptions, frame: &mut Frame, area: Rect) {
    let block = Block::bordered().title(format!(" Diff{} ", options_note(options)));
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

/// The focused Diff pane: one side of the file as hunks, the selected one
/// marked by a reversed `@@` row and a `▌` gutter, so it reads without colour.
/// In line mode the picked lines are marked that way instead.
/// `suffix`, when set, follows the title after a `·`.
fn render_hunk_view(
    view: &HunkView,
    options: DiffOptions,
    suffix: &str,
    frame: &mut Frame,
    area: Rect,
) {
    let title = hunk_title(view, options);
    let block = focused_block(if suffix.is_empty() {
        format!(" {title} ")
    } else {
        format!(" {title} · {suffix} ")
    });
    let inner = block.inner(area);
    let (rows, width) = (usize::from(inner.height), usize::from(inner.width));
    let pane = match &view.patch {
        None if view.loading() => Paragraph::new("Loading…"),
        None => Paragraph::new("Could not read the diff"),
        Some(patch) if patch.kind == FileKind::Binary => {
            Paragraph::new(PatchError::Binary.message())
        }
        Some(patch) if patch.kind == FileKind::Unsupported => {
            Paragraph::new(PatchError::Unsupported.message())
        }
        Some(patch) if patch.hunks.is_empty() => Paragraph::new(NO_CHANGES),
        Some(_) => {
            let picked = view.selected_range();
            let lines = view
                .rows()
                .into_iter()
                .skip(view.scroll)
                .take(rows)
                .map(|row| {
                    let in_hunk = row.hunk == Some(view.hunk);
                    // In line mode only the picked lines are marked.
                    let selected = if view.lines.is_some() {
                        in_hunk && row.line.is_some_and(|i| picked.contains(&i))
                    } else {
                        in_hunk
                    };
                    let gutter = if selected { "▌" } else { " " };
                    let style = match row.kind {
                        RowKind::Header => Style::new().add_modifier(Modifier::BOLD),
                        RowKind::HunkHeader => Style::new().fg(Color::Cyan),
                        RowKind::Added => Style::new().fg(Color::Green),
                        RowKind::Removed => Style::new().fg(Color::Red),
                        RowKind::Context => Style::new(),
                        RowKind::NoNewline => Style::new().fg(Color::DarkGray),
                    };
                    let text = match row.kind {
                        RowKind::Header | RowKind::HunkHeader => sanitize(&row.text),
                        RowKind::Context
                        | RowKind::Added
                        | RowKind::Removed
                        | RowKind::NoNewline => sanitize_body(&row.text),
                    };
                    let line = Line::from(vec![
                        Span::styled(gutter, Style::new().fg(FOCUSED)),
                        Span::styled(text, style),
                    ]);
                    let mark = match view.lines {
                        Some(_) => row.line.is_some(),
                        None => row.kind == RowKind::HunkHeader,
                    };
                    if selected && mark {
                        reversed(line, width)
                    } else {
                        line
                    }
                })
                .collect::<Vec<_>>();
            Paragraph::new(lines)
        }
    };
    frame.render_widget(pane.block(block), area);
}

/// The focused pane's title: the side, the mode and the options.
fn hunk_title(view: &HunkView, options: DiffOptions) -> String {
    let side = match view.side {
        DiffSide::Index => "Staged",
        DiffSide::Worktree | DiffSide::Untracked => "Unstaged",
    };
    // The mode is in words too, for when the bar is cut off.
    let mode = match view.lines {
        Some(_) => " · lines",
        None => "",
    };
    format!("{side}{mode}{}", options_note(options))
}

/// A renderer's rows, from the scroll, behind a one-column gutter. With
/// the Diff pane focused (`view`), the selection's rows get a `▌`, and in
/// line mode are reversed too, like the built-in view's.
fn render_rendered(
    rendered: &RenderedDiff,
    block: Block<'static>,
    view: Option<&HunkView>,
    hint: &str,
    frame: &mut Frame,
    area: Rect,
) {
    let inner = block.inner(area);
    let (rows, width) = (usize::from(inner.height), usize::from(inner.width));
    let pane = if let Some(error) = &rendered.error {
        // The way out first, so a long stderr cannot push it off the pane.
        let mut lines = vec![Line::from(hint.to_string()), Line::from("")];
        lines.extend(error.lines().map(Line::from));
        Paragraph::new(lines).wrap(Wrap { trim: false })
    } else if rendered.rendering() {
        Paragraph::new("Rendering…")
    } else if rendered.rows.is_empty() {
        Paragraph::new("No diff")
    } else {
        let selected = view.map(|view| {
            let picked = view.picked();
            (
                rendered.mapped_rows(view.hunk, picked.as_deref()),
                view.lines.is_some(),
            )
        });
        let lines = rendered
            .rows
            .iter()
            .enumerate()
            .skip(rendered.scroll)
            .take(rows)
            .map(|(at, row)| {
                let marked = selected
                    .as_ref()
                    .is_some_and(|(rows, _)| rows.contains(&at));
                let gutter = if marked { "▌" } else { " " };
                let mut spans = vec![Span::styled(gutter, Style::new().fg(FOCUSED))];
                spans.extend(elide_runs(&row.runs, width.saturating_sub(1)));
                let line = Line::from(spans);
                let lines_mode = selected.as_ref().is_some_and(|(_, lines)| *lines);
                if marked && lines_mode {
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

/// `runs` fitted to `max` columns: a row too wide (a renderer that
/// ignores the width, or a pane that shrank) ends in `…`, never half a
/// wide glyph.
fn elide_runs(runs: &[(Style, String)], max: usize) -> Vec<Span<'_>> {
    let total: usize = runs.iter().map(|(_, text)| width_of(text)).sum();
    if total <= max {
        return runs
            .iter()
            .map(|(style, text)| Span::styled(text.as_str(), *style))
            .collect();
    }
    let mut spans = Vec::new();
    let mut used = 0;
    for (style, text) in runs {
        let room = max.saturating_sub(used);
        if width_of(text) < room {
            used += width_of(text);
            spans.push(Span::styled(text.as_str(), *style));
            continue;
        }
        // The row goes on past this run, so it ends here, in `…`.
        let mut kept = 0;
        let cut: String = text
            .chars()
            .take_while(|c| {
                kept += width_of(c.encode_utf8(&mut [0; 4]));
                kept < room
            })
            .chain(std::iter::once('…'))
            .collect();
        if max > 0 {
            spans.push(Span::styled(cut, *style));
        }
        break;
    }
    spans
}

/// Columns the author takes in a blame row; longer names are elided.
const BLAME_AUTHOR: usize = 16;

/// The focused pane's frame: a bold title and a coloured border.
fn focused_block(title: String) -> Block<'static> {
    Block::bordered()
        .title(title)
        .title_style(Style::new().add_modifier(Modifier::BOLD))
        .border_style(Style::new().fg(FOCUSED))
}

/// The blame in the Diff pane's place: per line its commit, author, day
/// and number, then the text. The cursor row is reversed, so it reads
/// without colour.
fn render_blame(view: &BlameView, frame: &mut Frame, area: Rect) {
    let block = focused_block(format!(" Blame {} ", view.path.display()));
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
                let line = Line::from(blame_row(line, number_width));
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

/// `text` followed by spaces up to `width` columns.
fn pad(text: &str, width: usize) -> String {
    format!("{text}{}", " ".repeat(width.saturating_sub(width_of(text))))
}

/// A commit opened from the blame, in the same place: its message and
/// diff, coloured as the Diff pane colours them.
fn render_commit_view(view: &CommitView, frame: &mut Frame, area: Rect) {
    let block = focused_block(format!(" Commit {} ", short_hash(&view.oid)));
    let rows = usize::from(block.inner(area).height);
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
pub(crate) fn elide_left(text: &str, max: usize) -> String {
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
pub(crate) fn width_of(text: &str) -> usize {
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
    use crate::git::status::{BranchStatus, EntryKind};
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

    /// A workspace after a status load of `files` on branch `main`.
    fn loaded(files: Vec<FileEntry>) -> AppState {
        let mut state = AppState::new(Context::Workspace);
        state.files_loaded = true;
        state.files = files;
        state.branch = Some(branch(Head::Branch("main".into())));
        state
    }

    fn branch(head: Head) -> BranchStatus {
        BranchStatus {
            head,
            upstream: None,
            ahead_behind: None,
            in_progress: vec![],
        }
    }

    /// A loaded workspace with no changes on `branch`.
    fn on(branch: BranchStatus) -> AppState {
        let mut state = loaded(vec![]);
        state.branch = Some(branch);
        state
    }

    /// The three text rows inside the Status panel on an 80x24 screen.
    fn panel_rows(state: &AppState) -> Vec<String> {
        let rows = draw(state, 80, 24);
        assert!(rows[0].contains(" Status "), "{rows:#?}");
        assert!(rows[4].starts_with('└'), "{rows:#?}");
        rows[1..4].to_vec()
    }

    #[test]
    fn the_panel_shows_the_branch_and_its_upstream_counts() {
        let mut main = branch(Head::Branch("main".into()));
        main.upstream = Some("origin/main".into());
        main.ahead_behind = Some((2, 1));
        let rows = panel_rows(&on(main));
        assert!(rows[0].starts_with("│main "), "{rows:#?}");
        assert!(rows[1].starts_with("│origin/main ↑2 ↓1 "), "{rows:#?}");
        assert!(rows[2].starts_with("│ "), "{rows:#?}");
    }

    #[test]
    fn the_panel_says_when_there_is_no_upstream() {
        let rows = panel_rows(&on(branch(Head::Branch("topic".into()))));
        assert!(rows[0].starts_with("│topic "), "{rows:#?}");
        assert!(rows[1].starts_with("│no upstream "), "{rows:#?}");
    }

    #[test]
    fn the_panel_marks_an_upstream_that_is_gone() {
        let mut main = branch(Head::Branch("main".into()));
        main.upstream = Some("origin/main".into());
        let rows = panel_rows(&on(main));
        assert!(rows[1].starts_with("│origin/main (gone) "), "{rows:#?}");
    }

    #[test]
    fn a_detached_head_shows_the_short_hash() {
        let rows = panel_rows(&on(branch(Head::Detached("abc1234def5678".into()))));
        assert!(rows[0].starts_with("│detached abc1234 "), "{rows:#?}");
    }

    #[test]
    fn an_unborn_branch_says_it_has_no_commits() {
        let rows = panel_rows(&on(branch(Head::Unborn("main".into()))));
        assert!(rows[0].starts_with("│main (no commits) "), "{rows:#?}");
    }

    #[test]
    fn operations_in_progress_are_named_in_yellow() {
        let mut main = branch(Head::Branch("main".into()));
        main.in_progress = vec![Operation::Merge];
        let state = on(main.clone());
        assert!(panel_rows(&state)[2].starts_with("│merging "));
        let buffer = draw_buffer(&state, 80, 24);
        let (x, y) = find(&buffer, "merging");
        assert_eq!(buffer[(x, y)].style().fg, Some(Color::Yellow));
        // Bold too, so it stands out where yellow reads poorly.
        assert!(buffer[(x, y)].style().add_modifier.contains(Modifier::BOLD));

        main.in_progress = vec![Operation::Rebase];
        assert!(panel_rows(&on(main.clone()))[2].starts_with("│rebasing "));

        main.in_progress = vec![
            Operation::Rebase,
            Operation::Merge,
            Operation::CherryPick,
            Operation::Bisect,
        ];
        let rows = draw(&on(main), 200, 24);
        assert!(
            contains(&rows, "│rebasing, merging, cherry-picking, bisecting"),
            "{rows:#?}"
        );
    }

    #[test]
    fn the_panel_says_loading_before_the_first_load() {
        let rows = panel_rows(&AppState::new(Context::Workspace));
        assert!(rows[0].starts_with("│Loading… "), "{rows:#?}");
    }

    #[test]
    fn a_failed_first_load_leaves_the_panel_blank_not_loading() {
        let mut state = AppState::new(Context::Workspace);
        state.files_loaded = true;
        state.status_error = Some("fatal: not a git repository".into());
        let rows = panel_rows(&state);
        assert!(!contains(&rows, "Loading"), "{rows:#?}");
    }

    #[test]
    fn a_long_branch_name_is_elided_with_a_marker() {
        let rows = panel_rows(&on(branch(Head::Branch(format!("{}end", "b".repeat(100))))));
        // 22 columns inside the panel: the start gives way, the end stays.
        assert!(
            rows[0].starts_with(&format!("│…{}end││", "b".repeat(18))),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_long_upstream_keeps_its_counts_on_screen() {
        let mut main = branch(Head::Branch("main".into()));
        main.upstream = Some("origin/feature/a-really-long-status-panel".into());
        main.ahead_behind = Some((2, 1));
        let rows = panel_rows(&on(main.clone()));
        assert!(
            rows[1].starts_with("│…ng-status-panel ↑2 ↓1││"),
            "{rows:#?}"
        );

        main.ahead_behind = None;
        let rows = panel_rows(&on(main));
        assert!(
            rows[1].starts_with("│…") && rows[1].contains(" (gone)"),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_long_unborn_branch_keeps_no_commits_on_screen() {
        let rows = panel_rows(&on(branch(Head::Unborn("x".repeat(40)))));
        assert!(
            rows[0].starts_with("│…xxx") && rows[0].contains(" (no commits)"),
            "{rows:#?}"
        );
    }

    #[test]
    fn an_overflowing_operations_row_ends_in_a_marker() {
        let mut main = branch(Head::Branch("main".into()));
        main.in_progress = vec![Operation::Rebase, Operation::Merge, Operation::CherryPick];
        let rows = panel_rows(&on(main));
        assert!(
            rows[2].starts_with("│rebasing, merging, ch…││"),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_failed_refresh_marks_the_panel_stale() {
        let mut state = on(branch(Head::Branch("main".into())));
        state.status_error = Some("fatal: index file corrupt".into());
        let buffer = draw_buffer(&state, 80, 24);
        let rows = buffer_rows(&buffer);
        assert!(rows[0].contains(" Status (stale) "), "{rows:#?}");
        let (x, y) = find(&buffer, "main");
        assert_eq!(buffer[(x, y)].style().fg, Some(Color::DarkGray));
    }

    fn parent(path: &str) -> crate::model::ParentRepo {
        crate::model::ParentRepo {
            path: path.into(),
            files: vec![],
            files_loaded: true,
            branch: None,
            selected: None,
        }
    }

    #[test]
    fn inside_a_submodule_the_status_title_names_it() {
        let mut state = on(branch(Head::Branch("main".into())));
        state.repo_stack = vec![parent("sub")];
        let rows = draw(&state, 80, 24);
        assert!(rows[0].contains(" Status · sub "), "{rows:#?}");

        // Nested, and with a name that must not drive the terminal.
        state.repo_stack = vec![parent("libs/a"), parent("in\x1bner")];
        state.status_error = Some("boom".into());
        let rows = draw(&state, 160, 24);
        assert!(
            rows[0].contains(" Status · libs/a/in\u{FFFD}ner (stale) "),
            "{rows:#?}"
        );
        // A narrow panel keeps the innermost name.
        let rows = draw(&state, 80, 24);
        assert!(rows[0].contains(" Status · …er (stale) "), "{rows:#?}");
    }

    #[test]
    fn the_launch_repository_has_a_plain_status_title() {
        let state = on(branch(Head::Branch("main".into())));
        let rows = draw(&state, 80, 24);
        assert!(rows[0].starts_with("┌ Status ─"), "{rows:#?}");
    }

    /// The Files panel's title row and the row below it.
    fn files_rows(rows: &[String]) -> (usize, &String) {
        let title = rows
            .iter()
            .position(|row| row.contains(" Files "))
            .unwrap_or_else(|| panic!("no Files panel: {rows:#?}"));
        (title, &rows[title + 1])
    }

    #[test]
    fn a_short_terminal_shrinks_the_status_panel_before_files() {
        let mut state = loaded((0..10).map(|i| file(&format!("f{i}"))).collect());
        state.selected = Some(0);

        // 80x12: the full panel still fits above four file rows.
        let rows = draw(&state, 80, 12);
        assert!(rows[0].contains(" Status "), "{rows:#?}");
        let (title, first) = files_rows(&rows);
        assert_eq!(title, 5, "{rows:#?}");
        assert!(first.starts_with("│ M f0 "), "{rows:#?}");
        assert!(contains(&rows, "│ M f2 "), "{rows:#?}");

        // 80x8: no room for both, so Files gets the column.
        let rows = draw(&state, 80, 8);
        assert!(!contains(&rows, " Status "), "{rows:#?}");
        let (title, first) = files_rows(&rows);
        assert_eq!(title, 0, "{rows:#?}");
        assert!(first.starts_with("│ M f0 "), "{rows:#?}");
        assert!(contains(&rows, "│ M f3 "), "{rows:#?}");
    }

    #[test]
    fn a_medium_terminal_keeps_branch_and_upstream_rows() {
        let mut main = branch(Head::Branch("main".into()));
        main.upstream = Some("origin/main".into());
        main.ahead_behind = Some((0, 3));
        let mut state = on(main);
        state.files = vec![file("a.rs")];
        // Height 10: 9 rows over the bar; Files keeps 5, Status gets 4.
        let rows = draw(&state, 80, 10);
        assert!(rows[1].starts_with("│main "), "{rows:#?}");
        assert!(rows[2].starts_with("│origin/main ↑0 ↓3 "), "{rows:#?}");
        let (title, first) = files_rows(&rows);
        assert_eq!(title, 4, "{rows:#?}");
        assert!(first.starts_with("│ M a.rs "), "{rows:#?}");
    }

    #[test]
    fn tiny_terminals_do_not_panic_with_a_status_panel() {
        let mut main = branch(Head::Branch("a-long-branch-name".into()));
        main.upstream = Some("origin/a-long-branch-name".into());
        main.ahead_behind = Some((12, 345));
        main.in_progress = vec![Operation::Rebase, Operation::Merge];
        let state = on(main);
        for (w, h) in [
            (0, 0),
            (1, 1),
            (10, 2),
            (2, 10),
            (3, 3),
            (5, 4),
            (20, 6),
            (80, 24),
        ] {
            draw(&state, w, h);
        }
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
        // Tall enough for every Files key; a shorter screen clips the list.
        let rows = draw(&state, 80, 30);
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

    /// `state` with one global `[keys]` table applied.
    fn remapped(mut state: AppState, entries: &[(&str, &[&str])]) -> AppState {
        state.update(crate::intent::AppIntent::ConfigLoaded {
            repo: state.repo.clone(),
            keymap: crate::keymap::remap(entries),
            errors: vec![],
        });
        state
    }

    #[test]
    fn help_and_the_bar_show_the_remapped_keys() {
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        let mut state = remapped(state, &[("files.stage", &["x"])]);
        let rows = draw(&state, 80, 30);
        assert!(rows[29].starts_with(" x stage  a all"), "{rows:#?}");

        state.help_open = true;
        let rows = draw(&state, 80, 30);
        let stage = rows
            .iter()
            .find(|row| row.contains("Stage the selected file"))
            .unwrap_or_else(|| panic!("no stage row in {rows:#?}"));
        assert!(stage.contains(" x  "), "{stage:?}");
        assert!(!stage.contains("space"), "{stage:?}");
    }

    #[test]
    fn a_disabled_binding_leaves_help_and_the_bar() {
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        let mut state = remapped(state, &[("files.discard", &[])]);
        let rows = draw(&state, 80, 30);
        assert!(
            rows[29].starts_with(" space stage  a all  c commit  r refresh"),
            "{rows:#?}"
        );

        state.help_open = true;
        let rows = draw(&state, 80, 30);
        assert!(contains(&rows, "Stage the selected file"), "{rows:#?}");
        assert!(!contains(&rows, "Discard the selected file"), "{rows:#?}");
    }

    #[test]
    fn disabled_help_rows_are_dimmed() {
        let mut state = loaded(vec![]);
        state.help_open = true;
        let buffer = draw_buffer(&state, 80, 30);
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
        let rows = draw(&state, 40, 13);
        assert!(rows[5].contains(" Files "), "{rows:#?}");
        assert!(rows[6].starts_with("│Loading…"), "{rows:#?}");
        assert!(rows[12].contains("loading…"), "{rows:#?}");
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
            rows[23].starts_with(
                " space stage  a all  c commit  d discard  r refresh  bksp parent  ? keys  q quit"
            ),
            "{rows:#?}"
        );
    }

    #[test]
    fn workspace_says_no_changes_when_clean() {
        assert!(contains(&draw(&loaded(vec![]), 40, 13), "No changes"));
    }

    #[test]
    fn workspace_shows_status_error_wrapped_with_a_retry_hint() {
        let mut state = loaded(vec![]);
        state.status_error =
            Some("fatal: not a git repository (or any of the parent directories): .git".into());
        let rows = draw_files(&state, 40, 13);
        assert!(contains(&rows, "│fatal: not a git repository"), "{rows:#?}");
        assert!(contains(&rows, "directories): .git"), "{rows:#?}");
        assert!(contains(&rows, "│Press r to retry"), "{rows:#?}");
    }

    #[test]
    fn multi_line_error_keeps_its_lines() {
        let mut state = loaded(vec![]);
        state.status_error = Some("error: first\nhint: second".into());
        let rows = draw_files(&state, 40, 13);
        assert!(contains(&rows, "│error: first "), "{rows:#?}");
        assert!(contains(&rows, "│hint: second "), "{rows:#?}");
    }

    #[test]
    fn long_path_is_left_elided_to_fit_the_panel() {
        // Inner width 18: " M " leaves 15 columns for the path.
        let state = loaded(vec![file("src/deeply/nested/module/file.rs")]);
        let rows = draw_files(&state, 20, 11);
        assert_eq!(rows[6], "│ M …module/file.rs│", "{rows:#?}");
    }

    #[test]
    fn long_rename_keeps_the_new_path_and_elides_the_old_one_first() {
        // Inner width 28: "R  " + orig + " → " + path.
        let state = loaded(vec![renamed("src/old/location/a.rs", "src/new/b.rs")]);
        let rows = draw_files(&state, 30, 11);
        assert_eq!(rows[6], "│R  …tion/a.rs → src/new/b.rs│", "{rows:#?}");
    }

    #[test]
    fn very_long_rename_elides_both_paths() {
        let state = loaded(vec![renamed("old/name.rs", "src/brand/new/location/b.rs")]);
        let rows = draw_files(&state, 20, 11);
        assert_eq!(rows[6], "│R  … → …ation/b.rs│", "{rows:#?}");
    }

    #[test]
    fn wide_characters_are_measured_by_display_width() {
        // Each CJK glyph is two columns wide; inner width 14 leaves 11 for the path.
        let state = loaded(vec![file("日本語/日本語/長い名前.md")]);
        let rows = draw_files(&state, 16, 11);
        assert_eq!(rows[6], "│ M …い名前.md │", "{rows:#?}");
        assert!(width_of(&rows[6]) <= 16, "{rows:#?}");
    }

    #[test]
    fn a_reordering_file_name_is_drawn_with_a_visible_escape() {
        let state = loaded(vec![file("evil\u{202E}txt.exe")]);
        let width = 80;
        let mut terminal = Terminal::new(TestBackend::new(width, 12)).unwrap();
        terminal.draw(|frame| render(&state, frame)).unwrap();
        let buffer = terminal.backend().buffer();
        assert!(
            buffer
                .content()
                .iter()
                .all(|cell| !cell.symbol().contains('\u{202E}')),
            "a raw U+202E reached the buffer"
        );
        let rows = buffer_rows(buffer);
        let row = rows
            .iter()
            .find(|row| row.contains("evil"))
            .unwrap_or_else(|| panic!("{rows:#?}"));
        assert!(row.contains("evil<U+202E>txt.exe"), "{rows:#?}");
        assert!(width_of(row) <= usize::from(width), "{rows:#?}");
    }

    #[test]
    fn a_long_name_with_escapes_still_fits_its_ellipsis() {
        // Inner width 14 leaves 11 for the path; the escape counts its full width.
        let state = loaded(vec![file("a/long/\u{202E}folder/name\u{200B}")]);
        let rows = draw_files(&state, 16, 11);
        assert_eq!(rows[6], "│ M …me<U+200B>│", "{rows:#?}");
        assert!(width_of(&rows[6]) <= 16, "{rows:#?}");
    }

    #[test]
    fn overflowing_list_says_how_many_files_are_hidden() {
        // Height 12: the bar, the Status panel and two borders leave 4 rows,
        // so 3 files and "+N more".
        let files = (0..10).map(|i| file(&format!("f{i}"))).collect();
        let rows = draw_files(&loaded(files), 30, 12);
        assert!(contains(&rows, "│ M f2 "), "{rows:#?}");
        assert!(!contains(&rows, "│ M f3 "), "{rows:#?}");
        assert!(contains(&rows, "│+7 more "), "{rows:#?}");
    }

    #[test]
    fn list_that_fits_exactly_has_no_more_line() {
        let files = (0..4).map(|i| file(&format!("f{i}"))).collect();
        let rows = draw_files(&loaded(files), 30, 12);
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
        let buffer = draw_buffer(&state, 100, 13);
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
        // Height 12: 4 rows, so 3 files plus "+N more" at a time.
        let mut state = loaded((0..10).map(|i| file(&format!("f{i}"))).collect());
        state.selected = Some(5);
        let rows = draw_files(&state, 30, 12);
        assert!(contains(&rows, "│ M f5 "), "{rows:#?}");
        assert!(!contains(&rows, "│ M f2 "), "{rows:#?}");
        assert!(contains(&rows, "│+4 more "), "{rows:#?}");

        // Near the end the window shows the last files and no "more" row.
        state.selected = Some(8);
        let rows = draw_files(&state, 30, 12);
        for name in ["f6", "f7", "f8", "f9"] {
            assert!(contains(&rows, &format!("│ M {name} ")), "{rows:#?}");
        }
        assert!(!contains(&rows, "more"), "{rows:#?}");
    }

    #[test]
    fn a_one_row_list_shows_the_selection() {
        // Height 9: one row inside the Files border.
        let mut state = loaded((0..10).map(|i| file(&format!("f{i}"))).collect());
        state.selected = Some(4);
        let rows = draw_files(&state, 30, 9);
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
    fn an_amend_popup_is_titled_amend_last_commit() {
        let state = with_commit(CommitFlow::Editing(CommitEditor::amend("subj\n\nbody")));
        let rows = draw(&state, 80, 24);
        assert!(contains(&rows, " Amend last commit "), "{rows:#?}");
        assert!(!contains(&rows, " Commit message "), "{rows:#?}");
        assert!(contains(&rows, "subj"), "{rows:#?}");
        assert!(contains(&rows, "│body "), "{rows:#?}");
    }

    #[test]
    fn hooks_skipped_shows_only_when_skipping_hooks() {
        let mut editor = commit_editor("subj", &[""]);
        let rows = draw(&with_commit(CommitFlow::Editing(editor.clone())), 80, 24);
        assert!(!contains(&rows, "hooks skipped"), "{rows:#?}");

        editor.no_verify = true;
        let rows = draw(&with_commit(CommitFlow::Editing(editor.clone())), 80, 24);
        assert!(contains(&rows, " hooks skipped "), "{rows:#?}");
        // On the popup's title row, next to its title.
        assert_eq!(
            find_row(&rows, " hooks skipped "),
            find_row(&rows, " Commit message ")
        );
        let rows = draw(&with_commit(CommitFlow::Running(editor)), 80, 24);
        assert!(contains(&rows, " hooks skipped "), "{rows:#?}");
    }

    fn find_row(rows: &[String], needle: &str) -> Option<usize> {
        rows.iter().position(|row| row.contains(needle))
    }

    #[test]
    fn loading_the_last_commit_draws_a_small_box() {
        let state = with_commit(CommitFlow::LoadingAmend { seq: 3 });
        let rows = draw(&state, 80, 24);
        assert!(contains(&rows, "Loading the last commit…"), "{rows:#?}");
        assert!(
            rows[23].starts_with(" esc cancel  ctrl-c quit"),
            "{rows:#?}"
        );
    }

    #[test]
    fn tiny_terminals_do_not_panic_with_an_amend_popup() {
        let mut editor = CommitEditor::amend("subj");
        editor.no_verify = true;
        for commit in [
            CommitFlow::LoadingAmend { seq: 0 },
            CommitFlow::Editing(editor),
        ] {
            let state = with_commit(commit);
            for (w, h) in [(1, 1), (5, 3), (12, 4), (30, 6)] {
                draw(&state, w, h);
            }
        }
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
    fn fit_shortens_the_name_before_the_suffix() {
        assert_eq!(fit("origin/main", " ↑2 ↓1", 40), "origin/main ↑2 ↓1");
        assert_eq!(fit("origin/main", " ↑2 ↓1", 12), "…/main ↑2 ↓1");
        // No room for the name: one `…` stands in for it.
        assert_eq!(fit("origin/main", " ↑2 ↓1", 3), "… ↑2 ↓1");
        assert_eq!(fit("detached", " abc1234", 12), "…hed abc1234");
        assert_eq!(fit("main", "", 0), "…");
    }

    #[test]
    fn elide_right_keeps_the_start() {
        assert_eq!(elide_right("rebasing, merging", 40), "rebasing, merging");
        assert_eq!(elide_right("rebasing, merging", 10), "rebasing,…");
        assert_eq!(elide_right("日本語", 4), "日…");
        assert_eq!(elide_right("abc", 1), "…");
        assert_eq!(elide_right("abc", 0), "");
        assert_eq!(elide_right("", 0), "");
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

    const TWO_HUNKS: &[u8] = b"diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n\
        @@ -1,2 +1,2 @@\n-old one\n+new one\n same\n\
        @@ -9 +9 @@ fn \x1b[2Jx\n-old nine\n+new nine\n";

    /// A workspace with `a.rs` selected and the Diff pane focused on `side`,
    /// its patch loaded from `bytes`.
    fn hunks(side: DiffSide, bytes: &[u8]) -> AppState {
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        let mut view = HunkView::new("a.rs".into(), side);
        view.loaded(crate::git::patch::parse(bytes).unwrap(), 20);
        state.focus = Focus::Diff(view);
        state
    }

    #[test]
    fn the_focused_pane_is_titled_by_its_side_and_marked() {
        let buffer = draw_buffer(&hunks(DiffSide::Worktree, TWO_HUNKS), 100, 20);
        let rows = buffer_rows(&buffer);
        assert!(contains(&rows, " Unstaged "), "{rows:#?}");
        assert!(!contains(&rows, " Diff "), "{rows:#?}");
        let (x, y) = find(&buffer, " Unstaged ");
        assert_eq!(buffer[(x - 1, y)].style().fg, Some(Color::Cyan));

        let rows = draw(&hunks(DiffSide::Index, TWO_HUNKS), 100, 20);
        assert!(contains(&rows, " Staged "), "{rows:#?}");
        assert!(!contains(&rows, "· lines"), "{rows:#?}");
        let rows = draw(&hunks(DiffSide::Untracked, TWO_HUNKS), 100, 20);
        assert!(contains(&rows, " Unstaged "), "{rows:#?}");
    }

    #[test]
    fn the_selected_hunk_has_a_reversed_header_and_a_gutter() {
        let mut state = hunks(DiffSide::Worktree, TWO_HUNKS);
        let reversed_at = |state: &AppState, needle: &str| {
            let buffer = draw_buffer(state, 100, 20);
            let (x, y) = find(&buffer, needle);
            buffer[(x, y)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        };
        assert!(reversed_at(&state, "@@ -1,2 +1,2 @@"));
        assert!(!reversed_at(&state, "@@ -9 +9 @@"));
        let rows = draw(&state, 100, 20);
        assert!(contains(&rows, "▌-old one"), "{rows:#?}");
        assert!(contains(&rows, " -old nine"), "{rows:#?}");
        // The heading's control bytes never reach the terminal.
        assert!(contains(&rows, "@@ -9 +9 @@ fn \u{FFFD}[2Jx"), "{rows:#?}");

        if let Focus::Diff(view) = &mut state.focus {
            view.next(20);
        }
        assert!(!reversed_at(&state, "@@ -1,2 +1,2 @@"));
        assert!(reversed_at(&state, "@@ -9 +9 @@"));
        let rows = draw(&state, 100, 20);
        assert!(contains(&rows, "▌+new nine"), "{rows:#?}");
    }

    /// A diff of a file whose name holds a ZWNJ, adding a ZWNJ word, an emoji
    /// ZWJ sequence and a Devanagari conjunct, each followed by `X`.
    const JOINED: &str = "diff --git a/می\u{200C}خواهم.rs b/می\u{200C}خواهم.rs\n\
        --- a/می\u{200C}خواهم.rs\n+++ b/می\u{200C}خواهم.rs\n\
        @@ -1 +1,3 @@\n-old\n+می\u{200C}خواهمX\n+👨\u{200D}👩\u{200D}👧X\n+क्\u{200D}षX\n";

    #[test]
    fn a_hunk_body_keeps_script_joiners_and_its_file_header_escapes_them() {
        let rows = draw(&hunks(DiffSide::Worktree, JOINED.as_bytes()), 100, 20);
        assert!(contains(&rows, "+می\u{200C}خواهمX"), "{rows:#?}");
        assert!(contains(&rows, "+क्\u{200D}षX"), "{rows:#?}");
        assert!(contains(&rows, "+++ b/می<U+200C>خواهم.rs"), "{rows:#?}");
        // Emoji sequences stay escaped: their width depends on the terminal.
        assert!(contains(&rows, "+👨<U+200D>👩<U+200D>👧X"), "{rows:#?}");
    }

    /// Where the `+` of the added row starting with `first` is drawn.
    fn added_row(buffer: &ratatui::buffer::Buffer, first: char) -> (u16, u16) {
        let area = buffer.area;
        (0..area.height)
            .flat_map(|y| (0..area.width.saturating_sub(1)).map(move |x| (x, y)))
            .find(|&(x, y)| {
                buffer[(x, y)].symbol() == "+" && buffer[(x + 1, y)].symbol().starts_with(first)
            })
            .unwrap_or_else(|| panic!("no added row starting with {first:?}"))
    }

    #[test]
    fn text_after_a_joined_word_lands_where_a_terminal_draws_it() {
        // Columns as a terminal counts them, one per letter, two per emoji,
        // none for a joiner or virama: not `width_of`, which the layout uses.
        let width = 100;
        let buffer = draw_buffer(&hunks(DiffSide::Worktree, JOINED.as_bytes()), width, 20);
        for (first, columns) in [('م', 1 + 7), ('👨', 1 + 2 + 8 + 2 + 8 + 2), ('क', 1 + 2)] {
            let (x, y) = added_row(&buffer, first);
            assert_eq!(buffer[(x + columns, y)].symbol(), "X", "{first:?}");
            assert_eq!(buffer[(width - 1, y)].symbol(), "│", "{first:?}");
        }
    }

    #[test]
    fn a_clipped_joined_row_keeps_its_border_and_never_splits_a_cluster() {
        let long = "می\u{200C}خواهم क्\u{200D}ष 👨\u{200D}👩 ".repeat(8);
        let diff =
            format!("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+{long}\n");
        // Every width from 30 to 60 clips the row at a different point of it;
        // the tiny sizes only show that nothing splits or panics.
        let sizes = (30..=60).map(|w| (w, 12)).chain([(24, 8), (12, 5)]);
        for (width, height) in sizes {
            let buffer = draw_buffer(&hunks(DiffSide::Worktree, diff.as_bytes()), width, height);
            if height == 12 {
                let (_, y) = added_row(&buffer, 'م');
                assert_eq!(buffer[(width - 1, y)].symbol(), "│", "{width}x{height}");
            }
            for cell in buffer.content() {
                let lead = cell.symbol().chars().next();
                assert!(
                    !matches!(lead, Some('\u{200C}' | '\u{200D}' | '\u{094D}')),
                    "{width}x{height}: a cell starts with {lead:?}"
                );
            }
        }
    }

    #[test]
    fn in_line_mode_the_picked_lines_are_reversed_and_the_header_is_not() {
        let mut state = hunks(DiffSide::Worktree, TWO_HUNKS);
        if let Focus::Diff(view) = &mut state.focus {
            view.enter_lines();
            view.next_line(true);
        }
        let buffer = draw_buffer(&state, 100, 20);
        let reversed_at = |needle: &str| {
            let (x, y) = find(&buffer, needle);
            buffer[(x, y)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        };
        assert!(reversed_at("-old one"));
        assert!(reversed_at("+new one"));
        assert!(!reversed_at("same"));
        assert!(!reversed_at("@@ -1,2 +1,2 @@"));
        let rows = buffer_rows(&buffer);
        assert!(contains(&rows, "▌-old one"), "{rows:#?}");
        assert!(contains(&rows, "▌+new one"), "{rows:#?}");
        assert!(contains(&rows, "  same"), "{rows:#?}");
        assert!(contains(&rows, " @@ -1,2 +1,2 @@"), "{rows:#?}");
        assert!(contains(&rows, " Unstaged · lines "), "{rows:#?}");

        // A one-line range marks only that line.
        if let Focus::Diff(view) = &mut state.focus {
            view.next_line(false);
        }
        let rows = draw(&state, 100, 20);
        assert!(contains(&rows, " -old one"), "{rows:#?}");
        assert!(contains(&rows, "▌+new one"), "{rows:#?}");
        assert!(
            rows[19].starts_with(" space lines  esc hunks  d discard  t staged/unstaged"),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_binary_patch_says_how_to_stage_it() {
        let binary = b"diff --git a/a.rs b/a.rs\nBinary files a/a.rs and b/a.rs differ\n";
        let rows = draw(&hunks(DiffSide::Worktree, binary), 100, 20);
        assert!(
            contains(&rows, "Binary file: stage it from the Files panel"),
            "{rows:#?}"
        );
    }

    #[test]
    fn an_empty_side_or_a_load_says_so() {
        let rows = draw(&hunks(DiffSide::Index, b""), 100, 20);
        assert!(contains(&rows, "No changes in this view"), "{rows:#?}");

        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        let mut view = HunkView::new("a.rs".into(), DiffSide::Worktree);
        view.seq = Some(3);
        state.focus = Focus::Diff(view);
        let rows = draw(&state, 100, 20);
        assert!(contains(&rows, "Loading…"), "{rows:#?}");
    }

    #[test]
    fn help_in_the_diff_focus_lists_the_diff_keys() {
        let mut state = hunks(DiffSide::Worktree, TWO_HUNKS);
        state.help_open = true;
        let rows = draw(&state, 100, 30);
        assert!(contains(&rows, " Keys: Diff "), "{rows:#?}");
        assert!(contains(&rows, "Stage the selected hunk"), "{rows:#?}");
        assert!(contains(&rows, "esc/h/←"), "{rows:#?}");
        assert!(!contains(&rows, "Select the next file"), "{rows:#?}");
    }

    /// `state` with one global `[[commands]]` entry for the file list.
    fn with_files_command(mut state: AppState, description: &str) -> AppState {
        use crate::custom_command::{OutputMode, Panel, test_table};
        let mut table = test_table(
            Panel::Files,
            &["ctrl-t", "T"],
            &["true"],
            &[],
            OutputMode::None,
        );
        table.description = description.into();
        let keymap = crate::keymap::Keymap::default()
            .with_commands(&[table])
            .unwrap();
        state.update(crate::intent::AppIntent::ConfigLoaded {
            repo: state.repo.clone(),
            keymap,
            errors: vec![],
        });
        state
    }

    #[test]
    fn help_lists_a_command_in_its_panel_only_and_never_in_the_bar() {
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        let mut state = with_files_command(state, "Tidy \x1b[2Jthe file");
        let rows = draw(&state, 100, 40);
        assert!(!contains(&rows, "ctrl-t"), "{rows:#?}");

        state.help_open = true;
        let rows = draw(&state, 100, 40);
        let row = rows
            .iter()
            .find(|row| row.contains("Tidy"))
            .unwrap_or_else(|| panic!("no command row in {rows:#?}"));
        assert!(row.contains(" ctrl-t/T  "), "{row:?}");
        assert!(row.contains("Tidy \u{FFFD}[2Jthe file"), "{row:?}");
        // After the built-in rows.
        let at = |needle: &str| rows.iter().position(|r| r.contains(needle)).unwrap();
        assert!(at("Tidy") > at("Quit Kika"), "{rows:#?}");

        let mut diff = with_files_command(hunks(DiffSide::Worktree, TWO_HUNKS), "Tidy it");
        diff.help_open = true;
        let rows = draw(&diff, 100, 40);
        assert!(contains(&rows, " Keys: Diff "), "{rows:#?}");
        assert!(!contains(&rows, "Tidy"), "{rows:#?}");
    }

    #[test]
    fn the_bar_in_the_diff_focus_offers_the_hunk_keys() {
        let rows = draw(&hunks(DiffSide::Worktree, TWO_HUNKS), 100, 20);
        assert!(
            rows[19].starts_with(
                " space hunk  v lines  t staged/unstaged  d discard  esc back  r refresh"
            ),
            "{rows:#?}"
        );
    }

    #[test]
    fn tiny_terminals_do_not_panic_with_the_diff_focused() {
        let state = hunks(DiffSide::Worktree, TWO_HUNKS);
        for (w, h) in [(0, 0), (1, 1), (10, 2), (2, 10), (3, 3), (5, 4), (20, 6)] {
            draw(&state, w, h);
        }
    }

    #[test]
    fn the_diff_title_names_the_diff_options_that_differ_from_the_defaults() {
        use crate::git::diff::DiffOptions;
        let mut state = with_diff(vec![diff_line(LineKind::Added, "+new")]);
        let rows = draw(&state, 80, 12);
        assert!(contains(&rows, "┌ Diff ─"), "{rows:#?}");
        assert!(!contains(&rows, "context"), "{rows:#?}");

        state.diff_options = DiffOptions {
            context: 5,
            ignore_whitespace: false,
        };
        let rows = draw(&state, 80, 12);
        assert!(contains(&rows, "┌ Diff · 5 lines context ─"), "{rows:#?}");

        state.diff_options.ignore_whitespace = true;
        let rows = draw(&state, 80, 12);
        assert!(
            contains(&rows, "┌ Diff · 5 lines context · whitespace hidden ─"),
            "{rows:#?}"
        );

        state.diff_options.context = 3;
        let rows = draw(&state, 80, 12);
        assert!(contains(&rows, "┌ Diff · whitespace hidden ─"), "{rows:#?}");
    }

    #[test]
    fn the_focused_title_names_the_diff_options_too() {
        let mut state = hunks(DiffSide::Worktree, TWO_HUNKS);
        state.diff_options.context = 7;
        state.diff_options.ignore_whitespace = true;
        let rows = draw(&state, 100, 20);
        assert!(
            contains(&rows, " Unstaged · 7 lines context · whitespace hidden "),
            "{rows:#?}"
        );
    }

    #[test]
    fn help_lists_the_diff_option_keys_from_files_and_diff() {
        let mut files = loaded(vec![file("a.rs")]);
        files.selected = Some(0);
        let mut diff = hunks(DiffSide::Worktree, TWO_HUNKS);
        for state in [&mut files, &mut diff] {
            state.help_open = true;
            let rows = draw(state, 100, 40);
            let row = |needle: &str| {
                rows.iter()
                    .find(|row| row.contains(needle))
                    .unwrap_or_else(|| panic!("{needle:?} not in {rows:#?}"))
                    .clone()
            };
            assert!(
                row("Show more context lines").contains(" +/= "),
                "{rows:#?}"
            );
            assert!(row("Show fewer context lines").contains(" - "), "{rows:#?}");
            assert!(
                row("Hide or show whitespace-only").contains(" w "),
                "{rows:#?}"
            );
        }
    }

    const OID: &str = "abcdef1234567890abcdef1234567890abcdef12";

    fn blame_line(oid: &str, author: &str, line_no: usize, text: &str) -> BlameLine {
        BlameLine {
            oid: oid.into(),
            author: author.into(),
            date: "2024-01-02".into(),
            line_no,
            text: text.into(),
        }
    }

    /// A workspace showing a loaded blame of `a.rs` with `lines`.
    fn blamed(lines: Vec<BlameLine>, cursor: usize) -> AppState {
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        let mut view = BlameView::new("a.rs".into(), 0);
        view.loaded(Ok(lines));
        view.cursor = cursor;
        state.focus = Focus::Blame(view);
        state
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
        let lines = crate::git::blame::parse_blame(out.as_bytes()).unwrap();
        let buffer = draw_buffer(&blamed(lines, 0), 120, 20);
        let rows = buffer_rows(&buffer);
        assert!(contains(&rows, "می\u{200C}خواهمX"), "{rows:#?}");
        assert!(!contains(&rows, "<U+200C>"), "{rows:#?}");
        // Seven letters, one column each; the ZWNJ takes none.
        let (x, y) = find(&buffer, "م");
        assert_eq!(buffer[(x + 7, y)].symbol(), "X");
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

    /// Show `text` as renderer `delta` drew the shown diff, mapped to the
    /// focused view's patch when there is one.
    fn through_renderer(state: &mut AppState, text: &str) {
        use crate::external_diff::{ExternalRenderer, map_rows};
        use crate::rendered_view::RenderedDiff;

        state.renderers = vec![ExternalRenderer {
            label: "delta".into(),
            argv: vec!["delta".into()],
        }];
        state.renderer = Some(0);
        let side = match &state.focus {
            Focus::Diff(view) => view.side,
            _ => DiffSide::Worktree,
        };
        let mut rendered = RenderedDiff::new(1, "a.rs".into(), side);
        rendered.apply(1, Ok(crate::ansi::parse(text.as_bytes())));
        if let Focus::Diff(HunkView {
            patch: Some(patch), ..
        }) = &state.focus
        {
            rendered.map = map_rows(patch, &rendered.rows);
        }
        state.rendered = Some(rendered);
    }

    /// The view's `TWO_HUNKS`, decorated, with old line numbers in a gutter.
    const DECORATED: &str = "══ a.rs ══\n@1\n\x1b[31m1 │old one\x1b[0m\n\
        \x1b[32m  │new one\x1b[0m\n2 │same\n@9\n9 │old nine\n  │new nine\n";

    #[test]
    fn rendered_rows_replace_the_diff_until_cycled_back() {
        let mut state = with_diff(vec![diff_line(LineKind::Added, "+built in")]);
        through_renderer(&mut state, DECORATED);
        let buffer = draw_buffer(&state, 100, 20);
        let rows = buffer_rows(&buffer);
        assert!(contains(&rows, " Diff · delta "), "{rows:#?}");
        assert!(contains(&rows, "│ 1 │old one"), "{rows:#?}");
        assert!(!contains(&rows, "built in"), "{rows:#?}");
        let (x, y) = find(&buffer, "1 │old one");
        assert_eq!(buffer[(x, y)].style().fg, Some(Color::Red));

        state.update(crate::intent::AppIntent::CycleRenderer);
        let rows = draw(&state, 100, 20);
        assert!(contains(&rows, "+built in"), "{rows:#?}");
        assert!(contains(&rows, " Diff "), "{rows:#?}");
        assert!(!contains(&rows, "delta"), "{rows:#?}");
    }

    #[test]
    fn the_line_cursor_marks_only_the_rendered_row_of_its_line() {
        let mut state = hunks(DiffSide::Worktree, TWO_HUNKS);
        if let Focus::Diff(view) = &mut state.focus {
            view.enter_lines();
            view.next_line(false);
        }
        through_renderer(&mut state, DECORATED);
        let buffer = draw_buffer(&state, 100, 20);
        let rows = buffer_rows(&buffer);
        assert!(contains(&rows, " Unstaged · lines · delta "), "{rows:#?}");
        let marked: Vec<&String> = rows.iter().filter(|r| r.contains('▌')).collect();
        assert_eq!(marked.len(), 1, "{rows:#?}");
        assert!(marked[0].contains("▌  │new one"), "{rows:#?}");
        let (x, y) = find(&buffer, "│new one");
        assert!(
            buffer[(x, y)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn in_hunk_mode_the_hunks_rendered_rows_get_the_gutter() {
        let mut state = hunks(DiffSide::Worktree, TWO_HUNKS);
        through_renderer(&mut state, DECORATED);
        let rows = draw(&state, 100, 20);
        for marked in ["▌1 │old one", "▌  │new one", "▌2 │same"] {
            assert!(contains(&rows, marked), "{marked}: {rows:#?}");
        }
        assert!(contains(&rows, " 9 │old nine"), "{rows:#?}");
        assert!(contains(&rows, " @1"), "{rows:#?}");
    }

    #[test]
    fn a_side_by_side_rendering_falls_back_to_the_built_in_view() {
        let mut state = hunks(DiffSide::Worktree, TWO_HUNKS);
        if let Focus::Diff(view) = &mut state.focus {
            view.enter_lines();
        }
        // Each row ends with the new side, so `-old one` has no row.
        through_renderer(&mut state, "old one │ new one\nsame │ same\n");
        let rows = draw(&state, 100, 20);
        assert!(
            contains(&rows, " Unstaged · lines · delta (unmapped) "),
            "{rows:#?}"
        );
        assert!(contains(&rows, "▌-old one"), "{rows:#?}");
        assert!(!contains(&rows, "old one │ new one"), "{rows:#?}");
    }

    #[test]
    fn a_renderer_error_or_a_render_in_flight_says_so() {
        let mut state = with_diff(vec![diff_line(LineKind::Added, "+built in")]);
        through_renderer(&mut state, "");
        if let Some(rendered) = &mut state.rendered {
            rendered.error = Some("delta: bad \u{FFFD} flag".into());
        }
        let rows = draw(&state, 100, 20);
        assert!(contains(&rows, "delta: bad \u{FFFD} flag"), "{rows:#?}");
        assert!(
            contains(&rows, "Press enter, then R, for the built-in diff"),
            "{rows:#?}"
        );

        if let Some(rendered) = &mut state.rendered {
            rendered.error = None;
            rendered.seq = Some(2);
        }
        let rows = draw(&state, 100, 20);
        assert!(contains(&rows, "Rendering…"), "{rows:#?}");
    }

    #[test]
    fn a_row_wider_than_the_pane_ends_in_an_ellipsis() {
        let mut state = with_diff(vec![]);
        let wide = format!(
            "\x1b[31m{}\x1b[0m{}\n日本語日本語日本語日本語日本語\n",
            "a".repeat(20),
            "b".repeat(20)
        );
        through_renderer(&mut state, &wide);
        // 40 columns: the Diff pane's inside is 26 wide, 25 after the gutter.
        let rows = draw(&state, 40, 10);
        let expected = format!("│ {}{}…│", "a".repeat(20), "b".repeat(4));
        assert!(contains(&rows, &expected), "{rows:#?}");
        // No half glyph at the edge: 12 two-column glyphs, then `…`.
        assert!(contains(&rows, "│ 日本語日本語日本語日本語…"), "{rows:#?}");
        let wider = draw(&state, 100, 10);
        let whole = format!("{}{}", "a".repeat(20), "b".repeat(20));
        assert!(contains(&wider, &whole), "{wider:#?}");
    }

    #[test]
    fn the_way_out_of_an_error_stays_on_screen_and_names_the_next_step() {
        let mut state = with_diff(vec![]);
        through_renderer(&mut state, "");
        let long: Vec<String> = (0..60).map(|i| format!("stderr line {i}")).collect();
        if let Some(rendered) = &mut state.rendered {
            rendered.error = Some(long.join("\n"));
        }
        let rows = draw(&state, 80, 24);
        assert!(
            contains(&rows, "Press enter, then R, for the built-in diff"),
            "{rows:#?}"
        );
        assert!(contains(&rows, "stderr line 0"), "{rows:#?}");

        state
            .renderers
            .push(crate::external_diff::ExternalRenderer {
                label: "riff".into(),
                argv: vec!["riff".into()],
            });
        let rows = draw(&state, 80, 24);
        assert!(
            contains(&rows, "Press enter, then R, for the next renderer"),
            "{rows:#?}"
        );
    }

    #[test]
    fn tiny_terminals_do_not_panic_with_a_renderer_active() {
        let text: String = (0..50)
            .map(|i| format!("\x1b[32m{i} │日本 one {}\x1b[0m\n", "x".repeat(i)))
            .collect();
        let mut files = with_diff(vec![]);
        through_renderer(&mut files, &text);
        let mut hunk = hunks(DiffSide::Worktree, TWO_HUNKS);
        through_renderer(&mut hunk, DECORATED);
        let mut lines = hunk.clone();
        if let Focus::Diff(view) = &mut lines.focus {
            view.enter_lines();
        }
        through_renderer(&mut lines, DECORATED);
        let mut fallback = lines.clone();
        through_renderer(&mut fallback, "old one │ new one\n");
        let mut error = files.clone();
        if let Some(rendered) = &mut error.rendered {
            rendered.error = Some("bad\nnews".into());
        }
        let mut rendering = files.clone();
        if let Some(rendered) = &mut rendering.rendered {
            rendered.rows.clear();
            rendered.seq = Some(9);
        }
        for mut state in [files, hunk, lines, fallback, error, rendering] {
            for scroll in [0, 3, 500] {
                if let Some(rendered) = &mut state.rendered {
                    rendered.scroll = scroll;
                }
                for (w, h) in [(0, 0), (1, 1), (10, 3), (2, 10), (3, 3), (7, 5), (80, 24)] {
                    draw(&state, w, h);
                }
            }
        }
    }
}
