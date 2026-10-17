//! Draws the popup stack, bottom to top, so a submenu sits on its parent.
//! Reads state only.

use std::path::Path;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::features::command_log;
use crate::features::repos::{RecentList, RecentRow};
use crate::features::submodules::SubmoduleList;
use crate::menu::{self, MenuId};
use crate::model::AppState;
use crate::pick_list::PickList;
use crate::popup::{CredentialPrompt, ErrorPopup, LineInput, Popup};
use crate::reason_fit::fit_reason;
use crate::theme::Palette;
use crate::view::{
    COMMIT_WIDTH, centered, clip, elide_left, elide_right, render_question, tail, width_of, wrap,
};
use kika_git::safe_text::sanitize;

/// Wide enough for a path, narrow enough to look like a question.
const PROMPT_WIDTH: u16 = 50;

/// Every popup on the stack over `area`, the top one last. Each one is
/// nudged down and right by its depth, so the ones below stay in sight.
/// Only the top one, which has the keys, marks its cursor.
pub fn render_popups(state: &AppState, frame: &mut Frame, area: Rect) {
    let count = state.popups.len();
    for (depth, popup) in state.popups.iter().enumerate() {
        let area = nudge(area, depth);
        let top = depth + 1 == count;
        match popup {
            Popup::Submodules(_) | Popup::Recent(_) => {
                if let Some(popup) = list_popup(state, popup, top, area) {
                    draw_list_popup(popup, false, frame);
                }
            }
            Popup::Menu { .. } | Popup::PickList(_) => {
                if let Some(popup) = list_popup(state, popup, top, area) {
                    draw_list_popup(popup, true, frame);
                }
            }
            Popup::Prompt { kind, input } => {
                render_prompt(&state.prompt_title(*kind), input, top, frame, area)
            }
            Popup::Error(error) => render_error(&state.palette, error, frame, area),
            Popup::Question(q) => render_question(&format!(" {} ", q.title), &q.text, frame, area),
            Popup::Worktrees(list) => {
                crate::features::worktrees::view::render_worktrees(state, list, top, frame, area)
            }
            Popup::Credential(prompt) if prompt.confirm => {
                let text = format!("{}\n(y/n)", prompt.prompt);
                render_question(CREDENTIAL_TITLE, &text, frame, area)
            }
            Popup::Credential(prompt) => {
                render_credential(&state.palette, prompt, top, frame, area)
            }
            Popup::CommandLog(view) => command_log::view::render(
                &state.palette,
                &state.command_log,
                view,
                top,
                frame,
                area,
            ),
        }
    }
}

/// Where a list popup is drawn and which of its lines are rows. The view
/// draws from it and the mouse tests a click against it, so a click lands
/// on the row drawn under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ListBox {
    /// The popup's outer rect, its border included.
    pub rect: Rect,
    /// The first line shown inside the border, the header counted.
    pub first: usize,
    /// Lines above the rows that are not rows: a note, or what the list
    /// waits for or why it failed.
    pub header: usize,
    /// How many rows there are to select.
    pub len: usize,
    /// The cursor's row, while it is marked.
    pub cursor: Option<usize>,
}

impl ListBox {
    /// The row drawn at `at`; `None` on the border, on the header and
    /// below the last row.
    pub(crate) fn row_at(&self, at: Position) -> Option<usize> {
        let inner = Block::bordered().inner(self.rect);
        if !inner.contains(at) {
            return None;
        }
        let line = self.first + usize::from(at.y - inner.y);
        line.checked_sub(self.header).filter(|&row| row < self.len)
    }
}

/// A list popup laid out: its box, its title and every line it holds,
/// which the box's `first` scrolls through.
pub(crate) struct ListPopup {
    pub list: ListBox,
    pub title: String,
    pub lines: Vec<Line<'static>>,
}

/// The list box of the top popup, nudged as it is drawn over `area`, when
/// that popup is a list; `None` for any other popup or none.
pub(crate) fn top_list_box(state: &AppState, area: Rect) -> Option<ListBox> {
    let depth = state.popups.len().checked_sub(1)?;
    let area = nudge(area, depth);
    match state.popups.last()? {
        Popup::Worktrees(list) => {
            Some(crate::features::worktrees::view::worktrees_popup(state, list, true, area).list)
        }
        Popup::CommandLog(view) => Some(command_log::view::log_box(&state.command_log, view, area)),
        popup => list_popup(state, popup, true, area).map(|popup| popup.list),
    }
}

/// The menu, submodule, recent or pick list `popup` laid out in `area`.
fn list_popup(state: &AppState, popup: &Popup, top: bool, area: Rect) -> Option<ListPopup> {
    match popup {
        Popup::Menu { id, cursor } => Some(menu_popup(state, *id, top.then_some(*cursor), area)),
        Popup::Submodules(list) => Some(submodules_popup(list, top, area)),
        Popup::Recent(list) => Some(recent_popup(state, list, top, area)),
        Popup::PickList(list) => Some(pick_list_popup(list, top, area)),
        _ => None,
    }
}

/// The shape of a list popup's lines: `header` notes, then `len` rows,
/// then any notes below them.
#[derive(Clone, Copy)]
struct Rows {
    header: usize,
    len: usize,
}

impl Rows {
    /// Every line a row.
    fn all(len: usize) -> Self {
        Rows { header: 0, len }
    }

    /// Only notes, no row.
    fn none() -> Self {
        Rows { header: 0, len: 0 }
    }
}

/// `lines` padded to one `width`, shaped as `rows` says, the `cursor` row
/// reversed, centred in `area` and scrolled so the cursor's line shows.
fn fitted(
    area: Rect,
    title: String,
    lines: Vec<(String, Style)>,
    width: usize,
    Rows { header, len }: Rows,
    cursor: Option<usize>,
) -> ListPopup {
    let cursor = cursor.filter(|&row| row < len);
    let lines: Vec<Line<'static>> = lines
        .into_iter()
        .enumerate()
        .map(|(i, (text, mut style))| {
            let pad = " ".repeat(width.saturating_sub(width_of(&text)));
            if cursor.is_some_and(|row| header + row == i) {
                style = style.add_modifier(Modifier::REVERSED);
            }
            Line::styled(format!("{text}{pad}"), style)
        })
        .collect();
    let rect = centered(
        area,
        u16::try_from(width + 2).unwrap_or(u16::MAX),
        u16::try_from(lines.len() + 2).unwrap_or(u16::MAX),
    );
    let shown = usize::from(rect.height.saturating_sub(2)).max(1);
    let first = cursor.map_or(0, |row| (header + row).saturating_sub(shown - 1));
    ListPopup {
        list: ListBox {
            rect,
            first,
            header,
            len,
            cursor,
        },
        title,
        lines,
    }
}

/// Draw a laid-out list popup over what is below it, with `▲`/`▼` on the
/// border where lines are cut when `marks` asks for them.
pub(crate) fn draw_list_popup(popup: ListPopup, marks: bool, frame: &mut Frame) {
    let ListPopup { list, title, lines } = popup;
    frame.render_widget(Clear, list.rect);
    let shown = usize::from(list.rect.height.saturating_sub(2)).max(1);
    let mut block = Block::bordered().title(title);
    if marks && list.first > 0 {
        block = block.title(Line::raw("▲").right_aligned());
    }
    if marks && list.first + shown < lines.len() {
        block = block.title_bottom(Line::raw("▼").right_aligned());
    }
    let first = u16::try_from(list.first).unwrap_or(u16::MAX);
    frame.render_widget(
        Paragraph::new(lines).scroll((first, 0)).block(block),
        list.rect,
    );
}

/// `area` with its top-left corner pulled in, so a popup centred in it
/// sits `depth` rows lower and twice that further right, room allowing.
fn nudge(area: Rect, depth: usize) -> Rect {
    let down = u16::try_from(depth.saturating_mul(2))
        .unwrap_or(u16::MAX)
        .min(area.height);
    let right = down.saturating_mul(2).min(area.width);
    Rect::new(
        area.x + right,
        area.y + down,
        area.width - right,
        area.height - down,
    )
}

/// A box of items with the `cursor` row reversed, scrolled so that row is
/// in view on a short screen, with `▲`/`▼` on the border where rows are
/// cut. A menu below the top has no cursor. A disabled item is dimmed and
/// italic, not only grey, and says why after its label. A row wider than
/// the screen ends in `…`, as the bar's notice does: the reason gives way
/// first, so the label stays whole while it fits. A reason ending in a
/// path keeps the file name, and one with marked advice keeps the advice
/// (`fit_reason`).
fn menu_popup(state: &AppState, id: MenuId, cursor: Option<usize>, area: Rect) -> ListPopup {
    let title = format!(" {} ", id.title());
    let room = usize::from(area.width.saturating_sub(2));
    let disabled = Style::new()
        .fg(state.palette.dim)
        .add_modifier(Modifier::DIM | Modifier::ITALIC);
    let rows: Vec<(String, Style)> = menu::items(id)
        .iter()
        .map(|item| match (item.disabled)(state) {
            Some(reason) => {
                let head = format!(" {}  ", item.label);
                let left = room.saturating_sub(width_of(&head) + 1);
                let row = format!("{head}{} ", fit_reason(&reason, left));
                (elide_right(&row, room), disabled)
            }
            None => (
                elide_right(&format!(" {} ", item.label), room),
                Style::new(),
            ),
        })
        .collect();
    let width = rows
        .iter()
        .map(|(text, _)| width_of(text))
        .chain([width_of(&title)])
        .max()
        .unwrap_or(0);
    let len = rows.len();
    fitted(area, title, rows, width, Rows::all(len), cursor)
}

/// Hex digits of a commit the list shows, enough to tell commits apart.
const SHORT_OID: usize = 7;

/// One row per submodule: git's marker, the path, the short commit and the
/// marker in words. A path too long for the screen loses its start to `…`,
/// so the commit and the state, which carry the meaning, stay in sight.
/// The cursor row is reversed while the list is on top, and kept in view
/// on a short screen. Until a load lands, and when it fails or finds
/// nothing, that is said instead; a failure wraps to the box.
fn submodules_popup(list: &SubmoduleList, top: bool, area: Rect) -> ListPopup {
    let title = " Submodules ";
    // Text columns inside the borders.
    let room = usize::from(area.width.saturating_sub(2));
    let items = &list.list.rows;
    let (notes, rows): (Vec<String>, Vec<String>) = match (list.list.loaded, &list.list.error) {
        (_, Some(error)) => {
            let safe: Vec<String> = error.lines().map(|l| sanitize(l.as_bytes())).collect();
            let notes = wrap(&safe.join("\n"), room.saturating_sub(2).max(1))
                .into_iter()
                .map(|row| format!(" {row} "))
                .collect();
            (notes, Vec::new())
        }
        (false, None) => (vec![" Loading… ".into()], Vec::new()),
        (true, None) if items.is_empty() => (vec![" No submodules ".into()], Vec::new()),
        (true, None) => {
            let rows = items
                .iter()
                .map(|sub| {
                    let oid: String = sub.oid.chars().take(SHORT_OID).collect();
                    let head = format!(" {} ", sub.state.marker());
                    let tail = format!(" {oid} {} ", sub.state.word());
                    let path_room = room
                        .saturating_sub(width_of(&head) + width_of(&tail))
                        .max(1);
                    format!("{head}{}{tail}", elide_left(sub.path.display(), path_room))
                })
                .collect();
            (Vec::new(), rows)
        }
    };
    let cursor = (top && list.selected().is_some()).then_some(list.list.selected);
    let shape = if rows.is_empty() {
        Rows::none()
    } else {
        Rows::all(rows.len())
    };
    let lines: Vec<String> = notes.into_iter().chain(rows).collect();
    let width = lines
        .iter()
        .map(|row| width_of(row))
        .chain([width_of(title)])
        .max()
        .unwrap_or(0)
        .min(room);
    let lines = lines.into_iter().map(|row| (row, Style::new())).collect();
    fitted(area, title.into(), lines, width, shape, cursor)
}

/// A path as one safe row: control characters, newlines included, cannot
/// reach the terminal.
fn path_text(path: &Path) -> String {
    sanitize(path.as_os_str().as_encoded_bytes())
}

/// The recent repositories, one per row, with "Initialise a repository
/// here" first when Kika started outside one, and that said above them.
/// Long paths lose their start to `…`, where the name that tells them
/// apart is not. The cursor row is reversed while the list is on top.
fn recent_popup(state: &AppState, list: &RecentList, top: bool, area: Rect) -> ListPopup {
    let title = " Recent repositories ";
    let room = usize::from(area.width.saturating_sub(2));
    let fit = |text: &str| format!(" {} ", elide_left(text, room.saturating_sub(2).max(1)));
    let mut lines = Vec::new();
    if state.repo.is_none() {
        // The path gives way, never the words that say what is wrong.
        let said = "Not a git repository: ";
        let path_room = room.saturating_sub(2 + width_of(said)).max(1);
        let path = elide_left(&path_text(&state.launch), path_room);
        lines.push(format!(" {said}{path} "));
    }
    let header = lines.len();
    let rows = list.rows(&state.recent, state.repo.as_deref());
    lines.extend(rows.iter().map(|row| match row {
        RecentRow::Init => fit("Initialise a repository here"),
        RecentRow::Repo(path) => fit(&path_text(path)),
    }));
    // Said below the rows, so it is not one of them.
    if !rows.iter().any(|row| matches!(row, RecentRow::Repo(_))) {
        lines.push(fit("No recent repositories"));
    }
    let cursor = (top && list.cursor < rows.len()).then_some(list.cursor);
    let width = lines
        .iter()
        .map(|row| width_of(row))
        .chain([width_of(title)])
        .max()
        .unwrap_or(0)
        .min(room);
    let shape = Rows {
        header,
        len: rows.len(),
    };
    let lines = lines.into_iter().map(|row| (row, Style::new())).collect();
    fitted(area, title.into(), lines, width, shape, cursor)
}

/// The rows to pick from, under the list's purpose, with the cursor row
/// reversed while the list is on top. Long rows are cut at the end, and
/// rows scrolled out of a short screen are marked `▲`/`▼` on the border.
fn pick_list_popup(list: &PickList, top: bool, area: Rect) -> ListPopup {
    let title = format!(" {} ", list.purpose.title());
    let room = usize::from(area.width.saturating_sub(2));
    let rows: Vec<String> = list
        .rows
        .iter()
        .map(|row| format!(" {} ", clip(row, 0, room.saturating_sub(2).max(1))))
        .collect();
    let width = rows
        .iter()
        .map(|row| width_of(row))
        .chain([width_of(&title)])
        .max()
        .unwrap_or(0)
        .min(room);
    let shape = Rows::all(rows.len());
    let lines = rows.into_iter().map(|row| (row, Style::new())).collect();
    fitted(area, title, lines, width, shape, top.then_some(list.cursor))
}

/// A one-line box holding the typed text, and the terminal cursor when it
/// is the `top` popup. Long text scrolls to keep the cursor inside, with
/// `…` where it is cut.
fn render_prompt(title: &str, input: &LineInput, top: bool, frame: &mut Frame, area: Rect) {
    let popup = centered(area, PROMPT_WIDTH, 3);
    frame.render_widget(Clear, popup);
    let block = Block::bordered().title(format!(" {title} "));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    render_input_line(&input.text, input.col, top, frame, inner);
}

/// One line of typed `text` with the cursor at char `col`, marked while
/// the popup is on `top`. Long text scrolls to keep the cursor inside,
/// with `…` where it is cut.
fn render_input_line(text: &str, col: usize, top: bool, frame: &mut Frame, area: Rect) {
    if area.is_empty() {
        return;
    }
    let width = usize::from(area.width);
    let before: String = text.chars().take(col).collect();
    let x = width_of(&before);
    // One column to spare after the cursor for a `…`.
    let left = (x + 2).saturating_sub(width.max(2));
    frame.render_widget(Line::raw(clip(text, left, width)), area);
    if top {
        let column = u16::try_from(x - left).unwrap_or(u16::MAX);
        frame.set_cursor_position((area.x.saturating_add(column), area.y));
    }
}

const CREDENTIAL_TITLE: &str = " Git is asking ";

/// Git's or ssh's question over a line for the answer. A password or
/// passphrase is drawn as one `•` per typed char, never as itself.
fn render_credential(
    palette: &Palette,
    prompt: &CredentialPrompt,
    top: bool,
    frame: &mut Frame,
    area: Rect,
) {
    let width = COMMIT_WIDTH.min(area.width);
    let text_width = usize::from(width.saturating_sub(4)).max(1);
    let rows = wrap(&prompt.prompt, text_width);
    let height = u16::try_from(rows.len() + 3).unwrap_or(u16::MAX);
    let popup = centered(area, width, height);
    frame.render_widget(Clear, popup);
    let block = Block::bordered().title(CREDENTIAL_TITLE);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let [question, answer] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(inner);
    let rows: Vec<String> = rows.iter().map(|row| format!(" {row}")).collect();
    frame.render_widget(
        Paragraph::new(tail(palette, &rows, usize::from(question.height))),
        question,
    );
    // One column of padding, as the question has.
    let answer = Rect {
        x: answer.x.saturating_add(1),
        width: answer.width.saturating_sub(2),
        ..answer
    };
    let line = prompt.input.line();
    let shown = if prompt.shows_typed_text() {
        line.text.clone()
    } else {
        "•".repeat(line.text.chars().count())
    };
    render_input_line(&shown, line.col, top, frame, answer);
}

/// What failed in the red border's title, git's words below it, then what
/// to do next and how to close. Long output shows its tail. On a short
/// screen the next step gives way first, so git's words always get a row.
fn render_error(palette: &Palette, error: &ErrorPopup, frame: &mut Frame, area: Rect) {
    let width = COMMIT_WIDTH.min(area.width);
    let text_width = usize::from(width.saturating_sub(2)).max(1);
    let rows = wrap(&error.message, text_width);
    let next = wrap(&error.next, text_width);
    let height = u16::try_from(rows.len() + next.len() + 3).unwrap_or(u16::MAX);
    let popup = centered(area, width, height);
    frame.render_widget(Clear, popup);
    let block = Block::bordered()
        .title(format!(" {} ", error.title))
        .border_style(Style::new().fg(palette.error));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    // The hint takes one row and the message keeps at least one.
    let next_height = u16::try_from(next.len())
        .unwrap_or(u16::MAX)
        .min(inner.height.saturating_sub(2));
    let [text, next_area, hint] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(next_height),
        Constraint::Length(1),
    ])
    .areas(inner);
    frame.render_widget(
        Paragraph::new(tail(palette, &rows, usize::from(text.height))),
        text,
    );
    let yellow = Style::new().fg(palette.warning);
    let next: Vec<Line> = next
        .iter()
        .map(|row| Line::from(Span::styled(row.as_str(), yellow)))
        .collect();
    frame.render_widget(Paragraph::new(next), next_area);
    frame.render_widget(Line::raw("Esc: close"), hint);
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::style::Color;

    use super::*;
    use crate::confirm_question::quit_question;
    use crate::features::commits::CommitsIntent;
    use crate::features::commits::copy::CopyIntent;
    use crate::features::pull_requests::create_question;
    use crate::filtered_list::FilteredList;
    use crate::model::Context;
    use crate::op::GitOp;
    use crate::popup::DiscardWhat;
    use crate::popup::PromptKind;
    use crate::test_support::load_list;
    use crate::view::{buffer_rows, render};
    use kika_git::FileEntry;
    use kika_git::status::EntryKind;
    use kika_git::submodule::{Submodule, SubmoduleState};

    fn draw(state: &AppState, width: u16, height: u16) -> (Buffer, Vec<String>) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(state, frame)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let rows = buffer_rows(&buffer);
        (buffer, rows)
    }

    fn contains(rows: &[String], needle: &str) -> bool {
        rows.iter().any(|row| row.contains(needle))
    }

    /// Where `needle` starts on screen, by row and char column.
    fn find(rows: &[String], needle: &str) -> (u16, u16) {
        for (y, row) in rows.iter().enumerate() {
            if let Some(i) = row.find(needle) {
                let x = row[..i].chars().count();
                return (x as u16, y as u16);
            }
        }
        panic!("{needle:?} not on screen: {rows:#?}");
    }

    fn workspace(files: &[&str]) -> AppState {
        let mut state = AppState::new(Context::Workspace);
        state.files.loaded = true;
        state.files.rows = files
            .iter()
            .map(|path| FileEntry {
                path: (*path).into(),
                orig_path: None,
                index: '.',
                worktree: 'M',
                kind: EntryKind::Changed,
            })
            .collect();
        state
    }

    fn error() -> ErrorPopup {
        ErrorPopup::new(
            "Could not stage",
            "fatal: Unable to create '/repo/.git/index.lock': File exists.",
            "The file list was reloaded. Fix the cause above, then try again.",
        )
    }

    #[test]
    fn a_menu_draws_its_title_items_and_a_dimmed_reason() {
        let mut state = workspace(&[]);
        state.popups = vec![Popup::Menu {
            id: MenuId::Actions,
            cursor: 4,
        }];
        let (buffer, rows) = draw(&state, 80, 24);
        assert!(contains(&rows, " Actions "), "{rows:#?}");
        for label in ["Stage…", "Refresh", "Quit…"] {
            assert!(contains(&rows, label), "{label}: {rows:#?}");
        }
        assert!(contains(&rows, " Commit…  Nothing to commit "), "{rows:#?}");
        let (x, y) = find(&rows, "Nothing to commit");
        assert_eq!(buffer[(x, y)].style().fg, Some(Color::DarkGray));
        let (x, y) = find(&rows, "Refresh");
        assert!(
            buffer[(x, y)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        let (x, y) = find(&rows, "Stage…");
        assert_ne!(buffer[(x, y)].style().fg, Some(Color::DarkGray));
        assert!(
            !buffer[(x, y)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn under_no_color_a_menu_draws_no_colour_and_keeps_its_cursor() {
        let mut state = workspace(&[]);
        state.palette = crate::theme::Palette::NO_COLOR;
        state.popups = vec![Popup::Menu {
            id: MenuId::Actions,
            cursor: 4,
        }];
        let (buffer, rows) = draw(&state, 80, 24);
        assert!(contains(&rows, " Commit…  Nothing to commit "), "{rows:#?}");
        crate::test_support::assert_no_colour(&buffer);
        let (x, y) = find(&rows, "Refresh");
        assert!(
            buffer[(x, y)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn the_stash_menus_refusal_of_a_mixed_range_shows_whole_at_80_columns() {
        let mut state = workspace(&["gone.rs", "u.txt"]);
        state.files.rows[0].index = 'D';
        state.files.rows[0].worktree = '.';
        state.files.rows[1].index = '?';
        state.files.rows[1].worktree = '?';
        state.files.rows[1].kind = EntryKind::Untracked;
        state.focus = crate::model::Focus::Files;
        state.files.range_anchor = Some("gone.rs".into());
        state.files.selected = 1;
        state.popups = vec![Popup::Menu {
            id: MenuId::Stash,
            cursor: 0,
        }];
        let (_, rows) = draw(&state, 80, 24);
        let row = format!(
            " Stash selected files…  {} ",
            crate::features::stash::keys::STASH_APART
        );
        let (x, y) = find(&rows, &row);
        // The row ends inside the box, with its right border after it.
        let after: String = rows[y as usize]
            .chars()
            .skip(x as usize + row.chars().count())
            .collect();
        assert!(after.starts_with('│'), "{rows:#?}");
    }

    /// The Stash menu open over the Files panel, the cursor on the first
    /// row of `files`.
    fn stash_menu_over(files: Vec<FileEntry>) -> AppState {
        let mut state = AppState::new(Context::Workspace);
        state.files.loaded = true;
        state.files.rows = files;
        state.focus = crate::model::Focus::Files;
        state.popups = vec![Popup::Menu {
            id: MenuId::Stash,
            cursor: 0,
        }];
        state
    }

    fn renamed(path: &str, old: &str) -> FileEntry {
        FileEntry {
            path: path.into(),
            orig_path: Some(old.into()),
            index: 'R',
            worktree: '.',
            kind: EntryKind::Renamed,
        }
    }

    /// The text of the menu row starting at `label`, up to the box's right
    /// border, which must be on the same screen row.
    fn menu_row(rows: &[String], label: &str) -> String {
        let (x, y) = find(rows, label);
        let row: String = rows[y as usize].chars().skip(x as usize).collect();
        let end = row
            .find('│')
            .unwrap_or_else(|| panic!("no border: {rows:#?}"));
        row[..end].to_string()
    }

    fn untracked(path: &str) -> FileEntry {
        FileEntry {
            path: path.into(),
            orig_path: None,
            index: '?',
            worktree: '?',
            kind: EntryKind::Untracked,
        }
    }

    #[test]
    fn a_long_stash_refusal_is_cut_with_an_ellipsis_inside_the_menu() {
        let label = " Stash selected files…  ";
        // A reason ending in a path keeps the file name: the path is cut
        // from the left, the words before it from the right.
        let old = "src/features/stash/push.rs";
        let path_taken = stash_menu_over(vec![renamed("new.rs", old), untracked(old)]);
        for width in [80, 40] {
            let (_, rows) = draw(&path_taken, width, 24);
            let row = menu_row(&rows, " Stash selected files…");
            assert!(row.starts_with(label), "{width}: {rows:#?}");
            let shown = row[label.len()..].trim_end();
            if width == 80 {
                assert!(
                    row.starts_with(" Stash selected files…  A file now sits at"),
                    "{rows:#?}"
                );
                assert!(shown.ends_with("…/push.rs"), "{shown:?}");
            } else {
                assert!(shown.ends_with("push.rs"), "{width}: {shown:?}");
                assert!(shown.starts_with('…'), "{width}: {shown:?}");
            }
        }
        // A reason with marked advice keeps the advice: the words before
        // it give way first.
        let case_only = stash_menu_over(vec![renamed("old.txt", "Old.txt")]);
        let advice = "use Stash all changes… and apply it on this commit";
        for (width, height) in [(80, 24), (60, 24), (40, 12)] {
            let (_, rows) = draw(&case_only, width, height);
            let row = menu_row(&rows, " Stash selected files…");
            assert!(row.starts_with(label), "{width}: {rows:#?}");
            let shown = row[label.len()..].trim_end();
            match width {
                80 => assert_eq!(shown, advice, "{rows:#?}"),
                // Too narrow for the advice whole: its start stays.
                _ => {
                    assert!(shown.starts_with("use Stash"), "{width}: {shown:?}");
                    assert!(shown.ends_with('…'), "{width}: {shown:?}");
                }
            }
        }
    }

    /// The text inside the box titled `title`, row by row, joined with
    /// spaces, so wrapped words read as one line.
    fn popup_text(rows: &[String], title: &str) -> String {
        let (x, y) = find(rows, title);
        let left = usize::from(x) - 2;
        let top: Vec<char> = rows[usize::from(y)].chars().collect();
        let right = (left + 1..top.len())
            .find(|&i| top[i] == '┐')
            .unwrap_or_else(|| panic!("no corner: {rows:#?}"));
        let inside = rows[usize::from(y) + 1..].iter().map_while(|row| {
            let row: Vec<char> = row.chars().collect();
            (row[left] == '│').then(|| {
                let text: String = row[left + 1..right].iter().collect();
                text.trim().to_string()
            })
        });
        inside.collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn a_case_rename_stash_refusal_shows_whole_in_the_error_popup() {
        use crate::features::stash::apply::{STASH_CASE_BASE, STASH_CASE_DIRTY};
        use crate::op_slot::{failure_next, failure_title};
        let op = crate::op::GitOp::StashPop(crate::features::stash::git::StashRef {
            index: 0,
            oid: kika_git::commit_files::CommitId::parse("5a5a5a").unwrap(),
        });
        for (message, key) in [
            (STASH_CASE_BASE, "apply it on the commit it was made on."),
            (STASH_CASE_DIRTY, "Commit or stash them first."),
        ] {
            for (width, height) in [(80, 24), (40, 12)] {
                let mut state = workspace(&["a.rs"]);
                state.popups = vec![Popup::Error(ErrorPopup::new(
                    failure_title(&op),
                    message,
                    failure_next(&op),
                ))];
                let (_, rows) = draw(&state, width, height);
                let text = popup_text(&rows, "Could not pop the stash");
                assert!(contains(&rows, "Could not pop the stash"), "{rows:#?}");
                assert!(text.contains(message), "{width}x{height}: {rows:#?}");
                assert!(text.contains(key), "{width}x{height}: {rows:#?}");
                assert!(text.contains("The stash was kept."), "{rows:#?}");
                assert!(contains(&rows, "Esc: close"), "{width}x{height}: {rows:#?}");
            }
        }
    }

    #[test]
    fn a_taken_path_with_wide_chars_and_a_marker_keeps_its_file_name_whole() {
        let old = "日本/a\u{202E}b/語c\u{200B}d.rs";
        let state = stash_menu_over(vec![renamed("new.rs", old), untracked(old)]);
        for width in [80, 40] {
            let (_, rows) = draw(&state, width, 24);
            let row = menu_row(&rows, " Stash selected files…");
            let shown = row.trim_end();
            assert!(shown.ends_with("<U+200B>d.rs"), "{width}: {rows:#?}");
            if width == 80 {
                assert!(shown.ends_with("…/語c<U+200B>d.rs"), "{rows:#?}");
            }
            for (i, _) in shown.match_indices("<U+") {
                assert!(
                    shown[i..].starts_with("<U+200B>") || shown[i..].starts_with("<U+202E>"),
                    "{width}: {shown:?}"
                );
            }
        }
    }

    #[test]
    fn a_menu_too_narrow_for_a_label_cuts_the_whole_row() {
        let state = stash_menu_over(vec![renamed("old.txt", "Old.txt")]);
        let (_, rows) = draw(&state, 16, 24);
        let row = menu_row(&rows, " Stash select");
        assert!(row.trim_end().ends_with('…'), "{rows:#?}");
    }

    #[test]
    fn a_submenu_draws_over_its_parent() {
        let mut state = workspace(&["a.rs"]);
        state.popups = vec![
            Popup::Menu {
                id: MenuId::Actions,
                cursor: 0,
            },
            Popup::Menu {
                id: MenuId::Stage,
                cursor: 0,
            },
        ];
        let (_, rows) = draw(&state, 80, 24);
        assert!(contains(&rows, " Actions "), "{rows:#?}");
        assert!(contains(&rows, " Stage "), "{rows:#?}");
        assert!(contains(&rows, "Selected file"), "{rows:#?}");
        // The submenu sits below and right of its parent's top corner.
        let parent = find(&rows, " Actions ");
        let child = find(&rows, " Stage ");
        assert!(child.1 > parent.1 && child.0 > parent.0, "{rows:#?}");
        assert!(
            rows[23].starts_with(" enter select  esc back  ctrl-c quit"),
            "{rows:#?}"
        );
    }

    #[test]
    fn the_error_popup_says_what_failed_and_what_to_do() {
        let mut state = workspace(&["a.rs"]);
        state.popups = vec![Popup::Error(error())];
        let (buffer, rows) = draw(&state, 80, 24);
        assert!(contains(&rows, " Could not stage "), "{rows:#?}");
        assert!(contains(&rows, "index.lock': File exists."), "{rows:#?}");
        assert!(contains(&rows, "The file list was reloaded."), "{rows:#?}");
        assert!(contains(&rows, "Esc: close"), "{rows:#?}");
        let (x, y) = find(&rows, "The file list");
        assert_eq!(buffer[(x, y)].style().fg, Some(Color::Yellow));
        let (x, y) = find(&rows, " Could not stage ");
        assert_eq!(buffer[(x - 1, y)].style().fg, Some(Color::Red));
        assert!(rows[23].starts_with(" esc close  ctrl-c quit"), "{rows:#?}");
    }

    #[test]
    fn a_rejected_edit_draws_the_advice_git_message_and_the_key() {
        let mut state = workspace(&["a.rs"]);
        let error = state.edited_hunk_error("error: patch with only garbage at <stdin>:4");
        state.popups = vec![Popup::Error(error)];
        let (buffer, rows) = draw(&state, 80, 24);
        assert!(
            contains(&rows, " Could not stage the edited hunk "),
            "{rows:#?}"
        );
        assert!(
            contains(&rows, "The edited hunk is not a patch git can apply."),
            "{rows:#?}"
        );
        assert!(
            contains(&rows, "error: patch with only garbage at <stdin>:4"),
            "{rows:#?}"
        );
        let (x, y) = find(&rows, "Press e to edit the hunk again.");
        assert_eq!(buffer[(x, y)].style().fg, Some(Color::Yellow));
    }

    #[test]
    fn a_long_error_shows_its_tail_and_keeps_the_next_step() {
        let message: Vec<String> = (0..100).map(|i| format!("error {i}")).collect();
        let mut state = workspace(&["a.rs"]);
        state.popups = vec![Popup::Error(ErrorPopup::new(
            "Could not stage",
            &message.join("\n"),
            "Try again.",
        ))];
        let (_, rows) = draw(&state, 80, 24);
        assert!(contains(&rows, "earlier lines"), "{rows:#?}");
        assert!(contains(&rows, "│error 99 "), "{rows:#?}");
        assert!(!contains(&rows, "│error 0 "), "{rows:#?}");
        assert!(contains(&rows, "Try again."), "{rows:#?}");
        assert!(contains(&rows, "Esc: close"), "{rows:#?}");
    }

    #[test]
    fn dropped_key_changes_show_the_last_one_and_the_hint_at_any_size() {
        use crate::config::ConfigError;
        use crate::intent::AppIntent;
        use crate::keymap::Keymap;
        let errors: Vec<ConfigError> = [
            ("s", "files.stash", "files.stage"),
            ("c", "files.commit", "files.blame"),
            ("space", "files.stage", "files.hunks"),
        ]
        .iter()
        .map(|(key, a, b)| ConfigError {
            path: "/home/me/.config/kika/config.toml".into(),
            message: format!(
                r#"key "{key}" is bound to both "{a}" and "{b}"; the change to "{b}" was dropped"#
            ),
            dropped_binding: true,
        })
        .collect();
        for (width, height) in [(80, 24), (40, 12)] {
            let mut state = workspace(&["a.rs"]);
            state.update(AppIntent::ConfigLoaded {
                repo: state.repo.clone(),
                keymap: Keymap::default(),
                errors: errors.clone(),
            });
            let (_, rows) = draw(&state, width, height);
            assert!(contains(&rows, " Config error "), "{rows:#?}");
            assert!(contains(&rows, r#""files.hunks""#), "{rows:#?}");
            assert!(contains(&rows, "Esc: close"), "{rows:#?}");
            if width == 80 {
                assert!(contains(&rows, r#""files.stash""#), "{rows:#?}");
                assert!(contains(&rows, "used the rest of"), "{rows:#?}");
            }
        }
    }

    #[test]
    fn a_dropped_command_is_named_with_its_description_made_safe() {
        use crate::config::ConfigError;
        use crate::features::custom_commands::{OutputMode, Panel, test_table};
        use crate::intent::AppIntent;
        use crate::keymap::{Keymap, KeysTable, Trust};
        let keys: KeysTable = [("files.stage".to_string(), vec!["ctrl-t".to_string()])].into();
        let (keymap, _, kept) =
            Keymap::build_layer(&Keymap::default(), &keys, Trust::Global).unwrap();
        let mut table = test_table(Panel::Files, &["ctrl-t"], &["true"], &[], OutputMode::None);
        table.description = "wipe\u{1b}[2J".into();
        let (keymap, dropped) = keymap.with_commands(&[table], &kept).unwrap();
        assert!(keymap.commands.is_empty());
        let [message] = dropped.as_slice() else {
            panic!("expected one drop: {dropped:?}");
        };
        assert!(message.contains('\u{1b}'), "the message keeps the raw text");

        let mut state = workspace(&["a.rs"]);
        state.update(AppIntent::ConfigLoaded {
            repo: state.repo.clone(),
            keymap,
            errors: vec![ConfigError {
                path: "/home/me/.config/kika/config.toml".into(),
                message: message.clone(),
                dropped_binding: true,
            }],
        });
        let (buffer, rows) = draw(&state, 120, 24);
        assert!(contains(&rows, " Config error "), "{rows:#?}");
        assert!(contains(&rows, r#""files.stage""#), "{rows:#?}");
        assert!(contains(&rows, "command 1"), "{rows:#?}");
        assert!(contains(&rows, "wipe"), "{rows:#?}");
        assert!(
            buffer
                .content()
                .iter()
                .all(|cell| !cell.symbol().contains('\u{1b}')),
            "{rows:#?}"
        );
    }

    #[test]
    fn the_confirm_asks_and_the_bar_offers_yes_and_no() {
        let mut state = workspace(&["a.rs"]);
        state.popups = vec![Popup::Question(quit_question())];
        let (_, rows) = draw(&state, 80, 12);
        assert!(contains(&rows, " Quit "), "{rows:#?}");
        assert!(contains(&rows, "Quit Kika? (y/n)"), "{rows:#?}");
        assert!(rows[11].starts_with(" y yes  n no"), "{rows:#?}");
    }

    #[test]
    fn the_abort_question_wraps_whole_inside_a_narrow_terminal() {
        use kika_git::repo_state::Operation;

        let mut state = workspace(&["a.rs"]);
        let ask = crate::features::operation::abort_question(Operation::Rebase).unwrap();
        state.popups = vec![Popup::Question(ask.clone())];
        let (_, rows) = draw(&state, 40, 12);
        for needle in [" Abort rebase ", "Abort the rebase", "(y/n)"] {
            assert!(contains(&rows, needle), "{needle}: {rows:#?}");
        }
        assert!(rows[11].starts_with(" y abort  n keep"), "{rows:#?}");

        // Read the box back row by row: every text row is closed by the
        // right border, and together they hold the whole question.
        let (x, top) = find(&rows, " Abort rebase ");
        let chars = |y: u16| rows[usize::from(y)].chars().collect::<Vec<_>>();
        let left = usize::from(x) - 1;
        let right = chars(top)
            .iter()
            .rposition(|c| *c == '┐')
            .expect("top-right corner");
        let mut text = Vec::new();
        for y in top + 1.. {
            let row = chars(y);
            if row[left] == '└' {
                break;
            }
            assert_eq!(row[left], '│', "{rows:#?}");
            assert_eq!(row[right], '│', "row {y} is cut off: {rows:#?}");
            let inner: String = row[left + 1..right].iter().collect();
            text.push(inner.trim().to_string());
        }
        let text = text.join(" ");
        assert!(text.contains("Resolved conflicts are lost."), "{rows:#?}");
        assert_eq!(text, ask.text, "{rows:#?}");
    }

    #[test]
    fn the_skip_question_wraps_whole_inside_a_narrow_terminal() {
        use crate::features::operation::{skip_question, skip_question_unchecked};
        use kika_git::repo_state::Operation;

        let mut state = workspace(&["a.rs"]);
        let ask = skip_question(Operation::Rebase, None).unwrap();
        state.popups = vec![Popup::Question(ask.clone())];
        let (_, rows) = draw(&state, 40, 12);
        for needle in [" Skip rebase commit ", "Skip the commit", "(y/n)"] {
            assert!(contains(&rows, needle), "{needle}: {rows:#?}");
        }
        assert!(rows[11].starts_with(" y skip  n keep"), "{rows:#?}");
        assert_eq!(
            box_text(&rows, " Skip rebase commit "),
            ask.text,
            "{rows:#?}"
        );

        // The longest title and text, roomy and cramped: drawn whole, or
        // at least without a panic.
        let ask = skip_question_unchecked(Operation::CherryPick, None).unwrap();
        state.popups = vec![Popup::Question(ask.clone())];
        let (_, rows) = draw(&state, 80, 24);
        assert_eq!(
            box_text(&rows, " Skip cherry-pick commit "),
            ask.text,
            "{rows:#?}"
        );
        assert!(rows[23].starts_with(" y skip  n keep"), "{rows:#?}");
        draw(&state, 20, 6);
    }

    /// The text inside the popup box titled `title`, row by row, every
    /// row closed by the right border.
    fn box_text(rows: &[String], title: &str) -> String {
        let (x, top) = find(rows, title);
        let chars = |y: u16| rows[usize::from(y)].chars().collect::<Vec<_>>();
        let left = usize::from(x) - 1;
        let right = chars(top)
            .iter()
            .rposition(|c| *c == '┐')
            .expect("top-right corner");
        let mut text = Vec::new();
        for y in top + 1.. {
            let row = chars(y);
            if row[left] == '└' {
                break;
            }
            assert_eq!(row[right], '│', "row {y} is cut off: {rows:#?}");
            let inner: String = row[left + 1..right].iter().collect();
            text.push(inner.trim().to_string());
        }
        text.join(" ")
    }

    #[test]
    fn the_autosquash_question_shows_whole_and_only_y_folds() {
        let mut state = workspace(&["a.rs"]);
        let ask = crate::features::commits::rewrite::autosquash_question();
        state.popups = vec![Popup::Question(ask.clone())];
        for (width, height) in [(80, 24), (40, 12)] {
            let (_, rows) = draw(&state, width, height);
            assert_eq!(box_text(&rows, " Autosquash "), ask.text, "{rows:#?}");
            let bar = &rows[usize::from(height) - 1];
            assert!(bar.starts_with(" y fold  n keep"), "{rows:#?}");
            assert!(!bar.contains("enter"), "{rows:#?}");
        }
        // Too small for the box: drawing still works.
        let (_, rows) = draw(&state, 16, 5);
        assert_eq!(rows.len(), 5);
    }

    #[test]
    fn the_drop_range_question_counts_the_commits_and_only_y_drops() {
        use kika_git::commit_files::CommitId;
        let mut state = workspace(&["a.rs"]);
        let ids = ['a', 'b', 'c'].map(|d| CommitId::parse(&d.to_string().repeat(40)).unwrap());
        let ask = crate::features::commits::rewrite::drop_range_question(ids.to_vec());
        state.popups = vec![Popup::Question(ask)];
        let (_, rows) = draw(&state, 80, 24);
        assert_eq!(
            box_text(&rows, " Drop commits "),
            "Drop 3 commits and their changes from the history? (y/n)",
            "{rows:#?}"
        );
        let bar = &rows[23];
        assert!(bar.starts_with(" y drop  n keep  ctrl-c quit"), "{rows:#?}");
        assert!(!bar.contains("enter"), "{rows:#?}");
    }

    /// `workspace` with a custom patch of one file from `bbbbbbb`.
    fn with_patch() -> AppState {
        use crate::features::commits::custom_patch::CustomPatch;
        use kika_git::commit_files::{CommitId, CommitRange};
        let mut state = workspace(&["a.rs"]);
        state.files.rows.clear();
        let range = CommitRange {
            oid: CommitId::parse(&"b".repeat(40)).unwrap(),
            parent: CommitId::parse(&"a".repeat(40)),
        };
        let mut patch = CustomPatch::new(range, "bbbbbbb".into());
        patch.toggle_file(&"a.rs".into());
        state.custom_patch = Some(patch);
        state
    }

    #[test]
    fn the_custom_patch_question_keeps_y_n_in_sight_at_every_size() {
        use crate::features::commits::custom_patch::PatchAction;
        use kika_git::commit_files::CommitId;
        let long = "A rather long message with wide 漢字漢字 and 👩\u{200D}💻 in it, past any box";
        let actions = [
            PatchAction::Remove,
            PatchAction::MoveTo(CommitId::parse(&"c".repeat(40)).unwrap()),
            PatchAction::ToIndex,
            PatchAction::NewCommit("Split".into()),
            PatchAction::NewCommit(long.into()),
        ];
        for action in actions {
            let mut state = with_patch();
            state.popups = vec![Popup::Question(state.patch_action_question(action.clone()))];
            for (width, height) in [(80, 24), (40, 12)] {
                let (_, rows) = draw(&state, width, height);
                let text = box_text(&rows, " Custom patch ");
                assert!(
                    text.contains("(y/n)"),
                    "{action:?} {width}x{height}: {rows:#?}"
                );
                assert!(text.contains("bbbbbbb"), "{rows:#?}");
                assert!(text.contains("put back"), "{rows:#?}");
                let bar = &rows[usize::from(height) - 1];
                assert!(bar.starts_with(" y rewrite  n keep"), "{rows:#?}");
                assert!(!bar.contains("enter"), "{rows:#?}");
            }
            // The answer keys come with the first line.
            let (_, rows) = draw(&state, 30, 8);
            assert!(contains(&rows, "(y/n)"), "{action:?}: {rows:#?}");
            // Too small for the box: drawing still works.
            let (_, rows) = draw(&state, 12, 4);
            assert_eq!(rows.len(), 4);
        }
        // A long message is cut, not shown whole.
        let mut state = with_patch();
        state.popups = vec![Popup::Question(
            state.patch_action_question(PatchAction::NewCommit(long.into())),
        )];
        let (_, rows) = draw(&state, 80, 24);
        let text = box_text(&rows, " Custom patch ");
        assert!(text.contains("…\"?"), "{rows:#?}");
        assert!(!text.contains("past any box"), "{rows:#?}");
    }

    #[test]
    fn the_custom_patch_menu_lists_its_five_items_with_reasons_in_words() {
        let mut state = with_patch();
        state.popups = vec![Popup::Menu {
            id: MenuId::CustomPatch,
            cursor: 0,
        }];
        let (_, rows) = draw(&state, 80, 24);
        assert!(contains(&rows, " Custom patch "), "{rows:#?}");
        for label in [
            "Remove from commit",
            "Move to selected commit",
            "Move to index",
            "New commit after source…",
            "Reset patch",
        ] {
            assert!(contains(&rows, label), "{label}: {rows:#?}");
        }
        // The patch's commit is not loaded: each rewrite says so in words.
        assert!(
            contains(&rows, "Remove from commit  The patch's commit is not"),
            "{rows:#?}"
        );
        let reset = rows.iter().find(|r| r.contains("Reset patch")).unwrap();
        assert!(!reset.contains("No custom patch"), "{rows:#?}");
        for (width, height) in [(40, 12), (16, 6)] {
            let (_, rows) = draw(&state, width, height);
            assert_eq!(rows.len(), usize::from(height));
        }
        let (_, rows) = draw(&state, 40, 12);
        assert!(contains(&rows, "Custom patch"), "{rows:#?}");
    }

    #[test]
    fn the_discard_question_names_the_file_and_only_y_discards() {
        let mut state = workspace(&["src/a.rs"]);
        state.popups = vec![Popup::Question(DiscardWhat::File.into_question(
            "src/a.rs".into(),
            GitOp::DiscardWorktree(vec!["src/a.rs".into()]),
        ))];
        let (_, rows) = draw(&state, 80, 12);
        assert!(contains(&rows, " Discard file "), "{rows:#?}");
        assert!(
            contains(&rows, "Discard all unstaged changes to this file? (y/n)"),
            "{rows:#?}"
        );
        // The path sits on the row below the question, lined up with it.
        let (x, y) = find(&rows, "Discard all unstaged");
        let below: String = rows[usize::from(y) + 1]
            .chars()
            .skip(usize::from(x))
            .take("src/a.rs".len())
            .collect();
        assert_eq!(below, "src/a.rs", "{rows:#?}");
        assert!(
            rows[11].starts_with(" y discard  n keep  ctrl-c quit"),
            "{rows:#?}"
        );
    }

    fn delete_branch(force: bool) -> Popup {
        Popup::Question(crate::features::branches::delete_branch_question(
            "topic".into(),
            GitOp::DeleteBranch {
                name: "topic".into(),
                force,
            },
        ))
    }

    #[test]
    fn deleting_an_unmerged_branch_warns_and_offers_only_force() {
        let mut state = workspace(&["a.rs"]);
        state.popups = vec![delete_branch(true)];
        let (_, rows) = draw(&state, 80, 12);
        assert!(contains(&rows, " Force delete branch "), "{rows:#?}");
        assert!(
            contains(&rows, "topic is not fully merged; its commits may be lost."),
            "{rows:#?}"
        );
        assert!(
            contains(&rows, "Press D to force delete, n or Esc to cancel"),
            "{rows:#?}"
        );
    }

    #[test]
    fn deleting_a_merged_branch_asks_plainly() {
        let mut state = workspace(&["a.rs"]);
        state.popups = vec![delete_branch(false)];
        let (_, rows) = draw(&state, 80, 12);
        assert!(contains(&rows, " Delete branch "), "{rows:#?}");
        assert!(contains(&rows, "Delete branch topic? (y/n)"), "{rows:#?}");
        assert!(!contains(&rows, "not fully merged"), "{rows:#?}");
    }

    #[test]
    fn the_detached_checkout_question_names_the_commit() {
        let mut state = workspace(&["a.rs"]);
        let id = kika_git::commit_files::CommitId::parse("abcdef0123").unwrap();
        let short_oid = state.commits.short_oid(&id);
        state.popups = vec![Popup::Question(
            crate::features::commits::checkout_question(short_oid, id, None),
        )];
        let (_, rows) = draw(&state, 100, 12);
        assert!(contains(&rows, " Detached checkout "), "{rows:#?}");
        assert!(
            contains(&rows, "Check out abcdef0 as a detached HEAD?"),
            "{rows:#?}"
        );
    }

    #[test]
    fn the_hard_reset_question_names_the_target_and_offers_no_enter() {
        use crate::features::commits::reset_hard_question;
        use crate::op::ResetMode;
        use kika_git::commit_files::CommitId;
        let mut state = workspace(&["a.rs"]);
        state.popups = vec![Popup::Question(reset_hard_question(
            "abc1234".into(),
            GitOp::Reset {
                to: CommitId::parse("abc1234").unwrap(),
                mode: ResetMode::Hard,
            },
            None,
        ))];
        let (_, rows) = draw(&state, 100, 12);
        assert!(contains(&rows, " Reset hard "), "{rows:#?}");
        assert!(
            contains(&rows, "Reset to abc1234 and throw away"),
            "{rows:#?}"
        );
        assert!(
            rows[11].starts_with(" y reset  n keep  ctrl-c quit"),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_pick_list_shows_its_purpose_and_rows_with_the_cursor_reversed() {
        use crate::pick_list::{PickList, PickPurpose};
        let mut state = workspace(&["a.rs"]);
        let rows = vec![
            "origin  acme/widgets".into(),
            "upstream  org/widgets".into(),
        ];
        state.popups = vec![Popup::PickList(PickList::new(
            PickPurpose::TargetRemote,
            rows,
            1,
        ))];
        let (buffer, rows) = draw(&state, 100, 12);
        assert!(contains(&rows, " Target remote "), "{rows:#?}");
        let (x, y) = find(&rows, "upstream  org/widgets");
        assert!(
            buffer[(x, y)].modifier.contains(Modifier::REVERSED),
            "{rows:#?}"
        );
        let (x, y) = find(&rows, "origin  acme/widgets");
        assert!(!buffer[(x, y)].modifier.contains(Modifier::REVERSED));
        assert!(
            rows[11].starts_with(" enter pick  esc close  ctrl-c quit"),
            "{rows:#?}"
        );
    }

    #[test]
    fn the_create_question_names_where_what_and_the_title_and_offers_no_enter() {
        use crate::features::pull_requests::PrDraft;
        use kika_forge::ForgeRepo;
        let repo = |owner: &str| ForgeRepo {
            host: "github.com".into(),
            owner: owner.into(),
            name: "widgets".into(),
        };
        let mut state = workspace(&["a.rs"]);
        state.pr_draft = Some(PrDraft {
            branch: "topic".into(),
            head_remote: "origin".into(),
            head_repo: repo("me"),
            head: "topic".into(),
            targets: vec![],
            target: Some(("upstream".into(), repo("acme"))),
            bases: vec!["main".into()],
            base: Some("main".into()),
            title: "Add login".into(),
        });
        state.popups = vec![Popup::Question(create_question(&state))];
        let (_, rows) = draw(&state, 100, 14);
        assert!(contains(&rows, " Create pull request "), "{rows:#?}");
        assert!(
            contains(&rows, "acme/widgets: me:topic → main"),
            "{rows:#?}"
        );
        assert!(contains(&rows, "Add login"), "{rows:#?}");
        assert!(
            rows[13].starts_with(" y create  n cancel  ctrl-c quit"),
            "{rows:#?}"
        );
    }

    /// A 200-character remote row and a wide-character branch row.
    fn long_and_wide_rows() -> Vec<String> {
        vec![
            format!("origin  acme/{}", "w".repeat(200)),
            "upstream  組織/日本語のリポジトリ-🦀".into(),
        ]
    }

    /// A create draft whose names and title are far longer than a screen.
    fn long_draft() -> crate::features::pull_requests::PrDraft {
        use kika_forge::ForgeRepo;
        let repo = |owner: &str| ForgeRepo {
            host: "github.com".into(),
            owner: owner.into(),
            name: "w".repeat(90),
        };
        let head = format!("feature/{}-日本語", "x".repeat(120));
        crate::features::pull_requests::PrDraft {
            branch: head.as_str().into(),
            head_remote: "origin".into(),
            head_repo: repo("me"),
            head,
            targets: vec![],
            target: Some(("upstream".into(), repo("acme"))),
            bases: vec!["release/2026-10".into()],
            base: Some("release/2026-10".into()),
            title: format!("Add login {} 🦀 ログイン", "word ".repeat(80)),
        }
    }

    /// Every pull request popup over `state`, one at a time, drawn at
    /// `width` x `height`; the rows of each.
    fn each_pull_request_popup(width: u16, height: u16) -> Vec<(&'static str, Vec<String>)> {
        use crate::pick_list::{PickList, PickPurpose};
        let mut state = workspace(&["a.rs"]);
        load_list(
            &mut state.branches.list,
            Ok(vec![kika_git::branches::Branch {
                name: format!("feature/{}-日本語", "x".repeat(120))
                    .as_str()
                    .into(),
                head: false,
                upstream: Some("origin/x".into()),
                track: kika_git::branches::Track::None,
                merged: false,
            }]),
        );
        state.pr_draft = Some(long_draft());
        let popups = [
            (
                "menu",
                Popup::Menu {
                    id: MenuId::PullRequest,
                    cursor: 2,
                },
            ),
            (
                "pick",
                Popup::PickList(PickList::new(
                    PickPurpose::TargetRemote,
                    long_and_wide_rows(),
                    1,
                )),
            ),
            (
                "title",
                Popup::Prompt {
                    kind: PromptKind::PullRequestTitle,
                    input: LineInput::with_text(&long_draft().title),
                },
            ),
            ("confirm", Popup::Question(create_question(&state))),
            (
                "merge",
                Popup::Question(
                    crate::popup::MergePrConfirm {
                        number: 7,
                        title: "Make the widget cache 組織 🦀 ".repeat(6),
                        base: "main".into(),
                        head: "topic".into(),
                        method: kika_forge::merge::MergeMethod::Squash,
                        head_oid: kika_git::commit_files::CommitId::parse(&"a".repeat(40)).unwrap(),
                        repo: kika_forge::ForgeRepo {
                            host: "github.com".into(),
                            owner: "acme".into(),
                            name: "widgets".into(),
                        },
                        host: kika_forge::ForgeHost::parse("github.com").unwrap(),
                        unstable: true,
                        words: kika_forge::forge::words_or_default(None),
                    }
                    .into_question(),
                ),
            ),
        ];
        popups
            .into_iter()
            .map(|(name, popup)| {
                state.popups = vec![popup];
                (name, draw(&state, width, height).1)
            })
            .collect()
    }

    #[test]
    fn the_pull_request_popups_draw_at_every_size_without_panicking() {
        for (width, height) in [(80, 24), (30, 10), (24, 6), (20, 5), (3, 3), (1, 1)] {
            for (name, rows) in each_pull_request_popup(width, height) {
                assert_eq!(rows.len(), usize::from(height), "{name} {width}x{height}");
            }
        }
    }

    #[test]
    fn at_80_by_24_each_pull_request_popup_shows_what_matters() {
        let drawn = each_pull_request_popup(80, 24);
        let rows = |name: &str| &drawn.iter().find(|(n, _)| *n == name).unwrap().1;
        let menu = rows("menu");
        for label in ["Open in browser", "Copy URL", "Create pull request…"] {
            assert!(contains(menu, label), "{menu:#?}");
        }
        let pick = rows("pick");
        assert!(contains(pick, " Target remote "), "{pick:#?}");
        // The long row is cut with an ellipsis inside the box; the wide one
        // is whole.
        let long = pick.iter().find(|r| r.contains("origin  acme/")).unwrap();
        assert!(long.contains("w… │") || long.contains("w…│"), "{long:?}");
        assert!(
            contains(pick, "upstream  組織/日本語のリポジトリ-🦀"),
            "{pick:#?}"
        );
        assert!(
            contains(rows("title"), " Pull request title "),
            "{:#?}",
            rows("title")
        );
        let confirm = rows("confirm");
        assert!(contains(confirm, " Create pull request "), "{confirm:#?}");
        assert!(
            contains(confirm, "Press y to create it on GitHub"),
            "{confirm:#?}"
        );
        let merge = rows("merge");
        assert!(contains(merge, " Merge pull request "), "{merge:#?}");
        assert!(
            contains(merge, "Press y to merge it on GitHub"),
            "{merge:#?}"
        );
        assert!(
            contains(merge, "Merge #7 Make the widget cache"),
            "{merge:#?}"
        );
        assert!(contains(merge, "into main by squash?"), "{merge:#?}");
        assert!(contains(merge, "Some checks have not passed"), "{merge:#?}");
    }

    #[test]
    fn the_merge_question_on_gitlab_names_the_merge_request_and_gitlab() {
        let mut state = AppState::new(Context::Workspace);
        state.popups = vec![Popup::Question(
            crate::popup::MergePrConfirm {
                number: 12,
                title: "Add widget caching".into(),
                base: "main".into(),
                head: "topic".into(),
                method: kika_forge::merge::MergeMethod::Merge,
                head_oid: kika_git::commit_files::CommitId::parse(&"a".repeat(40)).unwrap(),
                repo: kika_forge::ForgeRepo {
                    host: "gitlab.example.com".into(),
                    owner: "acme".into(),
                    name: "widgets".into(),
                },
                host: kika_forge::ForgeHost::parse_gitlab("gitlab.example.com").unwrap(),
                unstable: false,
                words: kika_forge::forge::words(kika_forge::url::ForgeKind::GitLab),
            }
            .into_question(),
        )];
        let rows = draw(&state, 80, 24).1;
        assert!(contains(&rows, " Merge merge request "), "{rows:#?}");
        assert!(
            contains(&rows, "Press y to merge it on GitLab"),
            "{rows:#?}"
        );
        assert!(
            contains(&rows, "Merge !12 Add widget caching into main by merge?"),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_long_create_question_keeps_the_answer_keys_in_sight_on_a_short_screen() {
        for (width, height) in [(80, 8), (40, 8), (24, 6)] {
            let drawn = each_pull_request_popup(width, height);
            let confirm = &drawn.iter().find(|(n, _)| *n == "confirm").unwrap().1;
            assert!(
                contains(confirm, "Press y"),
                "{width}x{height}: {confirm:#?}"
            );
        }
    }

    #[test]
    fn a_narrow_pick_list_cuts_wide_glyphs_whole() {
        let drawn = each_pull_request_popup(20, 5);
        let pick = &drawn.iter().find(|(n, _)| *n == "pick").unwrap().1;
        let row = pick.iter().find(|r| r.contains("upstream")).unwrap();
        assert!(row.contains('…'), "{pick:#?}");
        // Every drawn char of the wide row is one of its own: none split.
        let own = "upstream  組織/日本語のリポジトリ-🦀…│ ";
        assert!(row.chars().all(|c| own.contains(c)), "{row:?}");
    }

    #[test]
    fn a_long_pick_list_scrolls_to_the_cursor_and_marks_hidden_rows() {
        use crate::pick_list::{PickList, PickPurpose};
        let mut state = workspace(&["a.rs"]);
        let rows: Vec<String> = (0..30).map(|i| format!("base-{i:02}")).collect();
        state.popups = vec![Popup::PickList(PickList::new(
            PickPurpose::BaseBranch,
            rows.clone(),
            25,
        ))];
        let (_, drawn) = draw(&state, 80, 10);
        assert!(contains(&drawn, "base-25"), "{drawn:#?}");
        assert!(!contains(&drawn, "base-00"), "{drawn:#?}");
        assert!(contains(&drawn, "▲") && contains(&drawn, "▼"), "{drawn:#?}");
        state.popups = vec![Popup::PickList(PickList::new(
            PickPurpose::BaseBranch,
            rows,
            0,
        ))];
        let (_, drawn) = draw(&state, 80, 10);
        assert!(
            contains(&drawn, "base-00") && !contains(&drawn, "▲"),
            "{drawn:#?}"
        );
        assert!(contains(&drawn, "▼"), "{drawn:#?}");
    }

    #[test]
    fn the_leave_commits_question_names_the_count_and_ids_and_offers_no_enter() {
        use crate::popup::LeaveCommitsConfirm;
        use kika_git::commit_files::CommitId;
        let mut state = workspace(&["a.rs"]);
        state.popups = vec![Popup::Question(
            LeaveCommitsConfirm {
                op: GitOp::CheckoutCommit(CommitId::parse("abc1234").unwrap()),
                head: CommitId::parse("fedcba9").unwrap(),
                head_short_oid: "fedcba9".into(),
                short_oid: "abc1234".into(),
                left: vec!["c1a2b3c".into(), "d4e5f6a".into(), "0b1c2d3".into()],
            }
            .into_question(),
        )];
        let (_, rows) = draw(&state, 100, 12);
        assert!(contains(&rows, " Leave commits behind "), "{rows:#?}");
        assert!(contains(&rows, "leave 3 commits"), "{rows:#?}");
        for id in ["c1a2b3c", "d4e5f6a", "0b1c2d3"] {
            assert!(contains(&rows, id), "{id}: {rows:#?}");
        }
        assert!(
            rows[11].starts_with(" y leave  n stay  ctrl-c quit"),
            "{rows:#?}"
        );
    }

    /// The hard-reset form of the leave-commits question, with eight ids.
    fn hard_reset_leaving_eight() -> AppState {
        use crate::op::ResetMode;
        use crate::popup::LeaveCommitsConfirm;
        use kika_git::commit_files::CommitId;
        let mut state = workspace(&["a.rs"]);
        state.popups = vec![Popup::Question(
            LeaveCommitsConfirm {
                op: GitOp::Reset {
                    to: CommitId::parse("abc1234").unwrap(),
                    mode: ResetMode::Hard,
                },
                head: CommitId::parse("fedcba9").unwrap(),
                head_short_oid: "fedcba9".into(),
                short_oid: "abc1234".into(),
                left: (0..8).map(|i| format!("{i}{i}{i}{i}{i}{i}{i}")).collect(),
            }
            .into_question(),
        )];
        state
    }

    #[test]
    fn the_hard_reset_leave_commits_question_fits_a_normal_terminal() {
        let (_, rows) = draw(&hard_reset_leaving_eight(), 80, 24);
        assert!(contains(&rows, "thrown away"), "{rows:#?}");
        assert!(contains(&rows, "and 3 more"), "{rows:#?}");
        // The keep hint names the key and the old HEAD; wrapping may
        // split it.
        assert!(contains(&rows, "then N to"), "{rows:#?}");
        assert!(contains(&rows, "fedcba9 (y/n)"), "{rows:#?}");
        assert!(!contains(&rows, "git branch"), "{rows:#?}");
    }

    #[test]
    fn a_tiny_terminal_still_shows_the_hard_reset_warning() {
        let (_, rows) = draw(&hard_reset_leaving_eight(), 24, 8);
        assert!(contains(&rows, "uncommitted"), "{rows:#?}");
        assert!(contains(&rows, "thrown away"), "{rows:#?}");
    }

    #[test]
    fn the_drop_stash_question_names_the_entry_and_offers_no_enter() {
        use crate::features::stash::StashRef;
        use kika_git::commit_files::CommitId;
        let mut state = workspace(&["a.rs"]);
        state.popups = vec![Popup::Question(
            crate::features::stash::drop_stash_question(
                "stash@{1}: On main: wip",
                GitOp::StashDrop(StashRef {
                    index: 1,
                    oid: CommitId::parse("abc").unwrap(),
                }),
            ),
        )];
        let (_, rows) = draw(&state, 100, 12);
        assert!(contains(&rows, " Drop stash "), "{rows:#?}");
        assert!(
            contains(
                &rows,
                "Drop stash@{1}: On main: wip? It cannot be recovered"
            ),
            "{rows:#?}"
        );
        assert!(
            rows[11].starts_with(" y drop  n keep  ctrl-c quit"),
            "{rows:#?}"
        );
    }

    #[test]
    fn the_travel_question_shows_every_preview_line_and_offers_no_enter() {
        use crate::features::commits::reflog::{Direction, Expected, Step};
        use kika_git::commit_files::CommitId;
        let mut state = workspace(&["a.rs"]);
        state.popups = vec![Popup::Question(
            crate::features::commits::reflog::travel_question(
                "Undo",
                &[
                    "Undo: commit: second".into(),
                    "Branch main moves from ccccccc to bbbbbbb.".into(),
                    "Files change to match bbbbbbb.".into(),
                    "Commits after bbbbbbb leave main; redo brings them back.".into(),
                ],
                GitOp::Travel {
                    direction: Direction::Undo,
                    step: Step::MoveTo(CommitId::parse("bbb").unwrap()),
                    expect: Expected {
                        head_ref: Some(b"refs/heads/main".to_vec()),
                        oid: CommitId::parse("ccc").unwrap(),
                    },
                },
            ),
        )];
        let (_, rows) = draw(&state, 100, 14);
        assert!(contains(&rows, " Undo "), "{rows:#?}");
        for line in [
            "Undo: commit: second",
            "Branch main moves from ccccccc to bbbbbbb.",
            "Files change to match bbbbbbb.",
            "Commits after bbbbbbb leave main; redo brings them back.",
            "(y/n)",
        ] {
            assert!(contains(&rows, line), "{line:?} in {rows:#?}");
        }
        assert!(
            rows[13].starts_with(" y yes  n no  ctrl-c quit"),
            "{rows:#?}"
        );
    }

    #[test]
    fn the_prompt_shows_the_text_and_the_cursor_after_it() {
        let mut state = workspace(&["a.rs"]);
        state.popups = vec![Popup::Prompt {
            kind: PromptKind::GoToFile,
            input: LineInput {
                text: "src/日本".into(),
                col: 6,
            },
        }];
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| render(&state, frame)).unwrap();
        let rows = buffer_rows(terminal.backend().buffer());
        assert!(contains(&rows, " Go to file "), "{rows:#?}");
        let (x, y) = find(&rows, "src/日本");
        let cursor = terminal.get_cursor_position().unwrap();
        // Two wide glyphs after four narrow chars: eight columns.
        assert_eq!((cursor.x, cursor.y), (x + 8, y));
        assert!(
            rows[23].starts_with(" enter ok  esc cancel  ctrl-c quit"),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_command_prompt_is_titled_by_the_command() {
        use crate::features::custom_commands::{
            CustomCommand, OutputMode, Panel, PendingCommand, test_table,
        };
        let table = test_table(
            Panel::Files,
            &["ctrl-t"],
            &["cp", "{path}", "{prompt1}", "{prompt2}"],
            &["Copy to", "Mode \x1b[2J"],
            OutputMode::None,
        );
        let mut state = workspace(&["a.rs"]);
        state.pending_command = Some(PendingCommand {
            command: CustomCommand::parse(&table, 1).unwrap(),
            target: crate::features::custom_commands::Target {
                path: "a.rs".into(),
                line: None,
                commit: None,
            },
            answers: vec!["x".into()],
        });
        state.popups = vec![Popup::Prompt {
            kind: PromptKind::Command,
            input: LineInput::default(),
        }];
        let (_, rows) = draw(&state, 80, 24);
        assert!(contains(&rows, " Mode \u{FFFD}[2J "), "{rows:#?}");
        assert!(!contains(&rows, "Copy to"), "{rows:#?}");
    }

    #[test]
    fn a_long_prompt_scrolls_to_keep_the_cursor_inside() {
        let text = format!("start{}", "x".repeat(95));
        let mut state = workspace(&["a.rs"]);
        state.popups = vec![Popup::Prompt {
            kind: PromptKind::GoToFile,
            input: LineInput { col: 100, text },
        }];
        let (_, rows) = draw(&state, 80, 24);
        assert!(contains(&rows, "│…xxx"), "{rows:#?}");
        assert!(!contains(&rows, "start"), "{rows:#?}");
    }

    fn reversed(buffer: &Buffer, (x, y): (u16, u16)) -> bool {
        buffer[(x, y)]
            .style()
            .add_modifier
            .contains(Modifier::REVERSED)
    }

    #[test]
    fn only_the_top_menu_marks_its_cursor_row() {
        let mut state = workspace(&["a.rs"]);
        state.popups = vec![
            Popup::Menu {
                id: MenuId::Actions,
                cursor: 0,
            },
            Popup::Menu {
                id: MenuId::Stage,
                cursor: 1,
            },
        ];
        let (buffer, rows) = draw(&state, 80, 24);
        assert!(!reversed(&buffer, find(&rows, "Stage…")), "{rows:#?}");
        assert!(reversed(&buffer, find(&rows, "Everything")), "{rows:#?}");
        assert!(
            !reversed(&buffer, find(&rows, "Selected file")),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_short_screen_scrolls_the_menu_to_its_cursor() {
        let mut state = workspace(&["a.rs"]);
        state.popups = vec![Popup::Menu {
            id: MenuId::Actions,
            cursor: menu::items(MenuId::Actions).len() - 1,
        }];
        // Six rows: the bar leaves five, so three of the items show.
        let (buffer, rows) = draw(&state, 20, 6);
        assert!(reversed(&buffer, find(&rows, "Quit…")), "{rows:#?}");
        assert!(!contains(&rows, "Stage…"), "{rows:#?}");
    }

    #[test]
    fn a_scrolled_menu_marks_the_rows_cut_above_and_below() {
        let mut state = workspace(&["a.rs"]);
        let menu = |cursor| {
            vec![Popup::Menu {
                id: MenuId::Actions,
                cursor,
            }]
        };
        // Three of the items show at a time.
        let last = menu::items(MenuId::Actions).len() - 1;
        state.popups = menu(0);
        let (_, rows) = draw(&state, 30, 6);
        let (_, top) = find(&rows, " Actions ");
        assert!(!rows[usize::from(top)].contains('▲'), "{rows:#?}");
        assert!(rows[usize::from(top) + 4].ends_with("▼┘"), "{rows:#?}");

        state.popups = menu(4);
        let (_, rows) = draw(&state, 30, 6);
        assert!(rows[usize::from(top)].ends_with("▲┐"), "{rows:#?}");
        assert!(rows[usize::from(top) + 4].ends_with("▼┘"), "{rows:#?}");

        state.popups = menu(last);
        let (_, rows) = draw(&state, 30, 6);
        assert!(rows[usize::from(top)].ends_with("▲┐"), "{rows:#?}");
        assert!(!rows[usize::from(top) + 4].contains('▼'), "{rows:#?}");

        // Nothing cut, nothing marked.
        let (_, rows) = draw(&state, 80, 24);
        assert!(!contains(&rows, "▲") && !contains(&rows, "▼"), "{rows:#?}");
    }

    #[test]
    fn a_disabled_item_is_marked_by_more_than_colour() {
        let mut state = workspace(&[]);
        state.popups = vec![Popup::Menu {
            id: MenuId::Actions,
            cursor: 0,
        }];
        let (buffer, rows) = draw(&state, 80, 24);
        let style = buffer[find(&rows, "Commit…")].style();
        assert!(
            style
                .add_modifier
                .contains(Modifier::DIM | Modifier::ITALIC),
            "{style:?}"
        );
        let style = buffer[find(&rows, "Refresh")].style();
        assert!(
            !style
                .add_modifier
                .intersects(Modifier::DIM | Modifier::ITALIC)
        );
    }

    #[test]
    fn a_covered_prompt_does_not_take_the_terminal_cursor() {
        let prompt = Popup::Prompt {
            kind: PromptKind::GoToFile,
            input: LineInput {
                text: "b".into(),
                col: 1,
            },
        };
        let mut state = workspace(&["a.rs"]);
        let cursor_shown = |state: &AppState| {
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            terminal.draw(|frame| render(state, frame)).unwrap();
            terminal.backend().cursor_visible()
        };
        state.popups = vec![prompt.clone()];
        assert!(cursor_shown(&state));
        state.popups = vec![prompt, Popup::Error(error())];
        assert!(!cursor_shown(&state));
    }

    #[test]
    fn a_short_screen_still_shows_some_of_git_message() {
        let mut state = workspace(&["a.rs"]);
        state.popups = vec![Popup::Error(error())];
        // Five rows over the bar: two borders, the hint, and two to share.
        let (_, rows) = draw(&state, 80, 6);
        assert!(contains(&rows, "File exists."), "{rows:#?}");
        assert!(contains(&rows, "Esc: close"), "{rows:#?}");
        let (_, rows) = draw(&state, 80, 5);
        assert!(contains(&rows, "File exists."), "{rows:#?}");
    }

    fn submodules(items: Option<Vec<Submodule>>, error: Option<&str>) -> Popup {
        Popup::Submodules(SubmoduleList {
            list: FilteredList {
                loaded: items.is_some() || error.is_some(),
                rows: items.unwrap_or_default(),
                error: error.map(str::to_string),
                selected: 1,
                ..FilteredList::default()
            },
        })
    }

    fn sub(path: &str, oid: &str, state: SubmoduleState) -> Submodule {
        Submodule {
            path: path.into(),
            oid: oid.into(),
            state,
        }
    }

    #[test]
    fn the_submodule_list_shows_marker_path_short_oid_and_state_with_the_cursor_reversed() {
        let mut state = workspace(&["a.rs"]);
        state.popups = vec![submodules(
            Some(vec![
                sub("libs/new", "1111111aaaa", SubmoduleState::Uninitialized),
                sub("libs/ok", "2222222bbbb", SubmoduleState::UpToDate),
                sub("old one", "3333333cccc", SubmoduleState::OutOfDate),
                sub("both", "4444444dddd", SubmoduleState::Conflicted),
            ]),
            None,
        )];
        let (buffer, rows) = draw(&state, 80, 24);
        assert!(contains(&rows, " Submodules "), "{rows:#?}");
        for row in [
            " - libs/new 1111111 not initialized ",
            "   libs/ok 2222222 up to date ",
            " + old one 3333333 out of date ",
            " U both 4444444 conflicted ",
        ] {
            assert!(contains(&rows, row), "{row:?}: {rows:#?}");
        }
        assert!(!contains(&rows, "1111111a"), "{rows:#?}");
        assert!(reversed(&buffer, find(&rows, "libs/ok")), "{rows:#?}");
        assert!(!reversed(&buffer, find(&rows, "libs/new")), "{rows:#?}");
        assert!(
            rows[23].starts_with(" enter enter  u update  i init  esc close  ctrl-c quit"),
            "{rows:#?}"
        );
    }

    #[test]
    fn the_submodule_list_says_when_it_is_loading_empty_or_failed() {
        let mut state = workspace(&["a.rs"]);
        for (popup, text) in [
            (submodules(None, None), "Loading…"),
            (submodules(Some(vec![]), None), "No submodules"),
            (submodules(None, Some("fatal: no repo")), "fatal: no repo"),
        ] {
            state.popups = vec![popup];
            let (_, rows) = draw(&state, 80, 24);
            assert!(contains(&rows, " Submodules "), "{rows:#?}");
            assert!(contains(&rows, text), "{text}: {rows:#?}");
        }
    }

    #[test]
    fn a_long_submodule_path_loses_its_start_so_oid_and_state_stay_visible() {
        let mut state = workspace(&["a.rs"]);
        let long = "vendor/some/deeply/nested/library/with/a/long/name/core";
        state.popups = vec![submodules(
            Some(vec![
                sub(long, "1111111aaaa", SubmoduleState::OutOfDate),
                sub("short", "2222222bbbb", SubmoduleState::UpToDate),
            ]),
            None,
        )];
        for (width, height) in [(40, 12), (30, 12)] {
            let (buffer, rows) = draw(&state, width, height);
            assert!(contains(&rows, "1111111 out of date │"), "{rows:#?}");
            assert!(contains(&rows, "ore 1111111"), "{rows:#?}");
            assert!(contains(&rows, " + …"), "{rows:#?}");
            assert!(!contains(&rows, "vendor"), "{rows:#?}");
            // The cursor still marks the short row, which is not cut.
            assert!(contains(&rows, " short 2222222 up to date"), "{rows:#?}");
            assert!(reversed(&buffer, find(&rows, "short")), "{rows:#?}");
        }
    }

    #[test]
    fn a_failure_wraps_inside_the_box_without_control_characters() {
        let mut state = workspace(&["a.rs"]);
        let error = "fatal: a rather long first line that cannot fit in thirty columns\n\
                     hint: \x1b[2Jsecond";
        state.popups = vec![submodules(None, Some(error))];
        let (_, rows) = draw(&state, 30, 16);
        assert!(contains(&rows, "fatal: a rather long"), "{rows:#?}");
        assert!(contains(&rows, "thirty columns"), "{rows:#?}");
        assert!(contains(&rows, "hint: \u{FFFD}[2Jsecond"), "{rows:#?}");
        // Every row of the box ends at its border, so nothing was clipped.
        for row in rows
            .iter()
            .filter(|r| r.contains("columns") || r.contains("fatal"))
        {
            assert!(row.trim_end().ends_with('│'), "{row:?}");
        }
    }

    #[test]
    fn a_list_taller_than_the_screen_keeps_the_cursor_in_view() {
        let mut state = workspace(&["a.rs"]);
        let items: Vec<Submodule> = (0..10)
            .map(|i| sub(&format!("sub{i}"), "1234567", SubmoduleState::UpToDate))
            .collect();
        state.popups = vec![Popup::Submodules(SubmoduleList {
            list: FilteredList {
                rows: items,
                selected: 9,
                loaded: true,
                ..FilteredList::default()
            },
        })];
        // Six rows: the bar leaves five, so three of the ten rows show.
        let (buffer, rows) = draw(&state, 40, 6);
        assert!(reversed(&buffer, find(&rows, "sub9")), "{rows:#?}");
        assert!(!contains(&rows, "sub0"), "{rows:#?}");
    }

    #[test]
    fn tiny_terminals_do_not_panic_with_any_popup() {
        let mut state = workspace(&["a.rs"]);
        let popups = [
            Popup::Menu {
                id: MenuId::Actions,
                cursor: 4,
            },
            Popup::Menu {
                id: MenuId::Stage,
                cursor: 1,
            },
            Popup::Question(quit_question()),
            Popup::Prompt {
                kind: PromptKind::GoToFile,
                input: LineInput {
                    text: "日本 a long name".into(),
                    col: 3,
                },
            },
            Popup::Error(error()),
            submodules(
                Some(vec![
                    sub("a", "1234567890", SubmoduleState::UpToDate),
                    sub("日本 long name", "12", SubmoduleState::OutOfDate),
                ]),
                None,
            ),
        ];
        for popup in &popups {
            state.popups = vec![popup.clone()];
            for (w, h) in [(0, 0), (1, 1), (10, 2), (20, 6), (2, 10), (3, 3)] {
                draw(&state, w, h);
            }
        }
        // The whole stack at once, deeper than the screen is tall.
        state.popups = popups.to_vec();
        for (w, h) in [(0, 0), (1, 1), (10, 2), (20, 6), (80, 24)] {
            draw(&state, w, h);
        }
    }

    #[test]
    fn the_welcome_says_where_and_offers_init_then_the_recent_paths() {
        use crate::features::repos::RecentList;

        let mut state = AppState::new(Context::Workspace).with_location(None, "/home/me/x".into());
        state.recent = vec!["/work/alpha".into(), "/work/b\x1b[2Jeta".into()];
        state.popups = vec![Popup::Recent(RecentList::new(true))];

        let (buffer, rows) = draw(&state, 80, 24);

        assert!(contains(&rows, " Recent repositories "), "{rows:#?}");
        assert!(
            contains(&rows, "Not a git repository: /home/me/x"),
            "{rows:#?}"
        );
        assert!(reversed(
            &buffer,
            find(&rows, "Initialise a repository here")
        ));
        assert!(contains(&rows, "/work/alpha"), "{rows:#?}");
        // Drawn safe: the escape byte never reaches the terminal.
        assert!(contains(&rows, "/work/b\u{fffd}[2Jeta"), "{rows:#?}");
        let (_, title) = find(&rows, " Recent repositories ");
        let (_, said) = find(&rows, "Not a git repository");
        let (_, init) = find(&rows, "Initialise");
        let (_, alpha) = find(&rows, "/work/alpha");
        assert!(title < said && said < init && init < alpha, "{rows:#?}");
    }

    #[test]
    fn inside_a_repository_the_list_leaves_it_out_and_an_empty_one_says_so() {
        use crate::features::repos::RecentList;

        let mut state =
            workspace(&["a.rs"]).with_location(Some("/work/a".into()), "/work/a".into());
        state.recent = vec!["/work/a".into()];
        state.popups = vec![Popup::Recent(RecentList::new(false))];

        let (_, rows) = draw(&state, 80, 24);

        assert!(contains(&rows, "No recent repositories"), "{rows:#?}");
        assert!(!contains(&rows, "Not a git repository"), "{rows:#?}");
        assert!(!contains(&rows, "Initialise"), "{rows:#?}");
        assert!(!contains(&rows, "/work/a "), "{rows:#?}");
    }

    #[test]
    fn a_long_launch_path_loses_its_start_but_not_the_words() {
        use crate::features::repos::RecentList;

        let long = format!("/{}/end", "d".repeat(200));
        let mut state = AppState::new(Context::Workspace).with_location(None, long.into());
        state.popups = vec![Popup::Recent(RecentList::new(true))];

        let (_, rows) = draw(&state, 60, 24);

        assert!(contains(&rows, " Not a git repository: …"), "{rows:#?}");
        assert!(contains(&rows, "d/end "), "{rows:#?}");
    }

    fn asked(prompt: &str, typed: &str) -> AppState {
        use crate::popup::CredentialPrompt;
        use crossterm::event::{KeyCode, KeyModifiers};

        let mut state = workspace(&["a.txt"]);
        let mut credential = CredentialPrompt::new(prompt.into(), false);
        for c in typed.chars() {
            credential.input.edit(crate::keymap::KeyPress::new(
                KeyCode::Char(c),
                KeyModifiers::NONE,
            ));
        }
        state.popups = vec![Popup::Credential(credential)];
        state
    }

    #[test]
    fn a_password_is_drawn_as_dots_and_a_username_as_typed() {
        let state = asked("Password for 'https://u@h':", "tok");
        let (_, rows) = draw(&state, 80, 24);
        assert!(contains(&rows, "Password for 'https://u@h':"), "{rows:#?}");
        assert!(contains(&rows, " ••• "), "{rows:#?}");
        assert!(!contains(&rows, "tok"), "{rows:#?}");

        let state = asked("Username for 'https://h':", "ann");
        let (_, rows) = draw(&state, 80, 24);
        assert!(contains(&rows, " ann "), "{rows:#?}");
    }

    #[test]
    fn a_password_popup_on_a_tiny_screen_still_hides_what_was_typed() {
        let typed = "ΩΨЖ";
        let state = asked("Password for 'https://u@h':", typed);
        let (_, rows) = draw(&state, 20, 6);
        for c in typed.chars() {
            assert!(!contains(&rows, &c.to_string()), "{c} shown: {rows:#?}");
        }
        assert!(contains(&rows, "•"), "{rows:#?}");
    }

    #[test]
    fn the_ssh_host_key_question_shows_every_line_and_the_typed_answer() {
        let state = asked(
            "The authenticity of host 'h' can't be established.\n\
             ED25519 key fingerprint is SHA256:abc.\n\
             Are you sure you want to continue connecting (yes/no/[fingerprint])? ",
            "yes",
        );
        let (_, rows) = draw(&state, 80, 24);
        for line in [
            "The authenticity of host 'h' can't be established.",
            "ED25519 key fingerprint is SHA256:abc.",
            "connecting (yes/no/[fingerprint])?",
            " yes ",
        ] {
            assert!(contains(&rows, line), "{line:?} missing: {rows:#?}");
        }
        assert!(!contains(&rows, "•"), "{rows:#?}");
    }

    const HOST_KEY_QUESTION: &str = "The authenticity of host 'h' can't be established.\n\
         Are you sure you want to continue connecting (yes/no/[fingerprint])? ";

    #[test]
    fn a_secret_typed_into_a_host_key_question_is_drawn_as_dots() {
        let state = asked(HOST_KEY_QUESTION, "hunter2");
        let (_, rows) = draw(&state, 80, 24);
        assert!(contains(&rows, " ••••••• "), "{rows:#?}");
        assert!(!contains(&rows, "hunter2"), "{rows:#?}");
    }

    #[test]
    fn a_fingerprint_typed_into_a_host_key_question_is_drawn_as_typed() {
        let fingerprint = "SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2s";
        let state = asked(HOST_KEY_QUESTION, fingerprint);
        let (_, rows) = draw(&state, 80, 24);
        assert!(contains(&rows, fingerprint), "{rows:#?}");
        assert!(!contains(&rows, "•"), "{rows:#?}");
    }

    #[test]
    fn the_host_key_answer_turns_to_dots_once_it_is_no_longer_an_answer() {
        use crate::keymap::KeyPress;
        use crossterm::event::{KeyCode, KeyModifiers};

        let mut state = asked(HOST_KEY_QUESTION, "yes");
        let (_, rows) = draw(&state, 80, 24);
        assert!(contains(&rows, " yes "), "{rows:#?}");

        let Some(Popup::Credential(credential)) = state.popups.last_mut() else {
            panic!("no credential popup");
        };
        credential
            .input
            .edit(KeyPress::new(KeyCode::Char('x'), KeyModifiers::NONE));
        let (_, rows) = draw(&state, 80, 24);
        assert!(contains(&rows, " •••• "), "{rows:#?}");
        assert!(!contains(&rows, "yesx"), "{rows:#?}");
    }

    #[test]
    fn the_answer_line_keeps_the_cursor_after_the_last_dot() {
        let state = asked("Passphrase:", "secret");
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| render(&state, frame)).unwrap();
        let rows = buffer_rows(terminal.backend().buffer());
        let (x, y) = find(&rows, "••••••");
        let cursor = terminal.get_cursor_position().unwrap();
        assert_eq!((cursor.x, cursor.y), (x + 6, y));
    }

    #[test]
    fn ssh_yes_no_question_is_asked_with_y_and_n() {
        use crate::popup::CredentialPrompt;

        let mut state = workspace(&["a.txt"]);
        state.popups = vec![Popup::Credential(CredentialPrompt::new(
            "Allow use of key id_ed25519?".into(),
            true,
        ))];
        let (_, rows) = draw(&state, 80, 24);
        assert!(contains(&rows, "Allow use of key id_ed25519?"), "{rows:#?}");
        assert!(contains(&rows, "(y/n)"), "{rows:#?}");
        let bar = rows.last().unwrap();
        assert!(bar.contains("y yes") && bar.contains("n no"), "{bar:?}");
        assert!(!bar.contains("enter"), "{bar:?}");
    }

    #[test]
    fn the_bisect_reset_question_names_where_it_started() {
        use kika_git::bisect::BisectState;
        use kika_git::repo_state::Operation;
        use kika_git::status::{BranchStatus, Head};
        let mut state = workspace(&[]);
        state.branch = Some(BranchStatus {
            head: Head::Detached("abc".into()),
            upstream: None,
            ahead_behind: None,
            in_progress: vec![Operation::Bisect],
            bisect: Some(BisectState {
                start: "feature".into(),
                ..BisectState::default()
            }),
        });
        state.popups = vec![Popup::Question(state.bisect_reset_question())];
        let (_, rows) = draw(&state, 100, 12);
        assert!(contains(&rows, " Reset bisect "), "{rows:#?}");
        assert!(contains(&rows, "HEAD goes back to feature;"), "{rows:#?}");
    }

    #[test]
    fn the_copy_menu_draws_its_four_items_and_the_bar_says_what_was_copied() {
        let mut state = workspace(&[]);
        state.commits.list.rows = vec![kika_git::log::CommitRow {
            oid: "abc1234def".into(),
            short_oid: "abc1234".into(),
            parents: vec![],
            author: "Ann".into(),
            time: 0,
            subject: "s".into(),
            subject_altered: false,
        }];
        let graph = state.commits.builder.push(&state.commits.list.rows[0]);
        state.commits.graph = vec![graph];
        state.popups = vec![Popup::Menu {
            id: MenuId::Copy,
            cursor: 0,
        }];
        let (_, rows) = draw(&state, 80, 24);
        assert!(contains(&rows, " Copy "), "{rows:#?}");
        for label in ["Full hash", "Short hash", "Subject", "Full message"] {
            assert!(contains(&rows, label), "{label}: {rows:#?}");
        }

        state.popups.clear();
        state.update(crate::intent::AppIntent::Commits(CommitsIntent::Copy(
            CopyIntent::CopyText(crate::clipboard::CopyWhat::Hash),
        )));
        let (_, rows) = draw(&state, 80, 24);
        let bar = rows.last().unwrap();
        assert!(bar.contains("Copied hash: abc1234def"), "{bar:?}");
    }
}
