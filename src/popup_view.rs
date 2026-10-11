//! Draws the popup stack, bottom to top, so a submenu sits on its parent.
//! Reads state only.

use std::path::Path;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::git::diff::sanitize;
use crate::menu::{self, MenuId};
use crate::model::AppState;
use crate::popup::{CredentialPrompt, ErrorPopup, LineInput, Popup};
use crate::recent_list::{RecentList, RecentRow};
use crate::submodule_list::SubmoduleList;
use crate::view::{
    COMMIT_WIDTH, centered, clip, elide_left, render_question, tail, width_of, wrap,
};

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
            Popup::Menu { id, cursor } => {
                let cursor = top.then_some(*cursor);
                render_menu(state, *id, cursor, frame, area)
            }
            Popup::Confirm(kind) => render_question(
                &format!(" {} ", kind.title()),
                &state.confirm_question(*kind),
                frame,
                area,
            ),
            Popup::Prompt { kind, input } => {
                render_prompt(&state.prompt_title(*kind), input, top, frame, area)
            }
            Popup::Error(error) => render_error(error, frame, area),
            Popup::Discard(confirm) => {
                let what = confirm.what;
                let text = format!("{}\n{}", what.question(), confirm.path);
                render_question(&format!(" {} ", what.title()), &text, frame, area)
            }
            Popup::DeleteBranch(confirm) => render_question(
                &format!(" {} ", confirm.title()),
                &confirm.question(),
                frame,
                area,
            ),
            Popup::RemoveRemote(confirm) => render_question(
                &format!(" {} ", confirm.title()),
                &confirm.question(),
                frame,
                area,
            ),
            Popup::DeleteTag(confirm) => render_question(
                &format!(" {} ", confirm.title()),
                &confirm.question(),
                frame,
                area,
            ),
            Popup::ResetHard(confirm) => render_question(
                &format!(" {} ", confirm.title()),
                &confirm.question(),
                frame,
                area,
            ),
            Popup::Submodules(list) => render_submodules(list, top, frame, area),
            Popup::Recent(list) => render_recent(state, list, top, frame, area),
            Popup::Credential(prompt) if prompt.confirm => {
                let text = format!("{}\n(y/n)", prompt.prompt);
                render_question(CREDENTIAL_TITLE, &text, frame, area)
            }
            Popup::Credential(prompt) => render_credential(prompt, top, frame, area),
            Popup::Travel(confirm) => render_question(
                &format!(" {} ", confirm.title),
                &confirm.question(),
                frame,
                area,
            ),
        }
    }
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
/// italic, not only grey, and says why after its label.
fn render_menu(state: &AppState, id: MenuId, cursor: Option<usize>, frame: &mut Frame, area: Rect) {
    let title = format!(" {} ", id.title());
    let rows: Vec<(String, bool)> = menu::items(id)
        .iter()
        .map(|item| match (item.disabled)(state) {
            Some(reason) => (format!(" {}  {reason} ", item.label), true),
            None => (format!(" {} ", item.label), false),
        })
        .collect();
    let width = rows
        .iter()
        .map(|(text, _)| width_of(text))
        .chain([width_of(&title)])
        .max()
        .unwrap_or(0);
    let lines: Vec<Line> = rows
        .into_iter()
        .enumerate()
        .map(|(i, (text, disabled))| {
            let pad = " ".repeat(width - width_of(&text));
            let mut style = Style::new();
            if disabled {
                style = style
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM | Modifier::ITALIC);
            }
            if Some(i) == cursor {
                style = style.add_modifier(Modifier::REVERSED);
            }
            Line::styled(format!("{text}{pad}"), style)
        })
        .collect();
    let popup = centered(
        area,
        u16::try_from(width + 2).unwrap_or(u16::MAX),
        u16::try_from(lines.len() + 2).unwrap_or(u16::MAX),
    );
    frame.render_widget(Clear, popup);
    let rows = usize::from(popup.height.saturating_sub(2)).max(1);
    let total = lines.len();
    let first = cursor.map_or(0, |cursor| cursor.saturating_sub(rows - 1));
    // Rows cut off are marked on the border they are hidden behind.
    let mut block = Block::bordered().title(title);
    if first > 0 {
        block = block.title(Line::raw("▲").right_aligned());
    }
    if first + rows < total {
        block = block.title_bottom(Line::raw("▼").right_aligned());
    }
    let first = u16::try_from(first).unwrap_or(u16::MAX);
    frame.render_widget(Paragraph::new(lines).scroll((first, 0)).block(block), popup);
}

/// Hex digits of a commit the list shows, enough to tell commits apart.
const SHORT_OID: usize = 7;

/// One row per submodule: git's marker, the path, the short commit and the
/// marker in words. A path too long for the screen loses its start to `…`,
/// so the commit and the state, which carry the meaning, stay in sight.
/// The cursor row is reversed while the list is on top, and kept in view
/// on a short screen. Until a load lands, and when it fails or finds
/// nothing, that is said instead; a failure wraps to the box.
fn render_submodules(list: &SubmoduleList, top: bool, frame: &mut Frame, area: Rect) {
    let title = " Submodules ";
    // Text columns inside the borders.
    let room = usize::from(area.width.saturating_sub(2));
    let rows: Vec<String> = match (&list.items, &list.error) {
        (_, Some(error)) => {
            let safe: Vec<String> = error.lines().map(|l| sanitize(l.as_bytes())).collect();
            wrap(&safe.join("\n"), room.saturating_sub(2).max(1))
                .into_iter()
                .map(|row| format!(" {row} "))
                .collect()
        }
        (None, None) => vec![" Loading… ".into()],
        (Some(items), None) if items.is_empty() => vec![" No submodules ".into()],
        (Some(items), None) => items
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
            .collect(),
    };
    let cursor = (top && list.selected().is_some()).then_some(list.cursor);
    let width = rows
        .iter()
        .map(|row| width_of(row))
        .chain([width_of(title)])
        .max()
        .unwrap_or(0)
        .min(room);
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let pad = width.saturating_sub(width_of(row));
            let text = format!("{row}{}", " ".repeat(pad));
            let style = if Some(i) == cursor {
                Style::new().add_modifier(Modifier::REVERSED)
            } else {
                Style::new()
            };
            Line::styled(text, style)
        })
        .collect();
    let popup = centered(
        area,
        u16::try_from(width + 2).unwrap_or(u16::MAX),
        u16::try_from(lines.len() + 2).unwrap_or(u16::MAX),
    );
    frame.render_widget(Clear, popup);
    let shown = usize::from(popup.height.saturating_sub(2)).max(1);
    let first = cursor.map_or(0, |cursor| cursor.saturating_sub(shown - 1));
    let first = u16::try_from(first).unwrap_or(u16::MAX);
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((first, 0))
            .block(Block::bordered().title(title)),
        popup,
    );
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
fn render_recent(state: &AppState, list: &RecentList, top: bool, frame: &mut Frame, area: Rect) {
    let title = " Recent repositories ";
    let room = usize::from(area.width.saturating_sub(2));
    let fit = |text: &str| format!(" {} ", elide_left(text, room.saturating_sub(2).max(1)));
    let mut header = Vec::new();
    if state.repo.is_none() {
        // The path gives way, never the words that say what is wrong.
        let said = "Not a git repository: ";
        let path_room = room.saturating_sub(2 + width_of(said)).max(1);
        let path = elide_left(&path_text(&state.launch), path_room);
        header.push(format!(" {said}{path} "));
    }
    let rows = list.rows(&state.recent, state.repo.as_deref());
    let mut lines_text: Vec<String> = header.clone();
    lines_text.extend(rows.iter().map(|row| match row {
        RecentRow::Init => fit("Initialise a repository here"),
        RecentRow::Repo(path) => fit(&path_text(path)),
    }));
    if !rows.iter().any(|row| matches!(row, RecentRow::Repo(_))) {
        lines_text.push(fit("No recent repositories"));
    }
    let cursor = (top && list.cursor < rows.len()).then_some(header.len() + list.cursor);
    let width = lines_text
        .iter()
        .map(|row| width_of(row))
        .chain([width_of(title)])
        .max()
        .unwrap_or(0)
        .min(room);
    let lines: Vec<Line> = lines_text
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let pad = width.saturating_sub(width_of(row));
            let style = if Some(i) == cursor {
                Style::new().add_modifier(Modifier::REVERSED)
            } else {
                Style::new()
            };
            Line::styled(format!("{row}{}", " ".repeat(pad)), style)
        })
        .collect();
    let popup = centered(
        area,
        u16::try_from(width + 2).unwrap_or(u16::MAX),
        u16::try_from(lines.len() + 2).unwrap_or(u16::MAX),
    );
    frame.render_widget(Clear, popup);
    let shown = usize::from(popup.height.saturating_sub(2)).max(1);
    let first = cursor.map_or(0, |cursor| cursor.saturating_sub(shown - 1));
    let first = u16::try_from(first).unwrap_or(u16::MAX);
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((first, 0))
            .block(Block::bordered().title(title)),
        popup,
    );
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
fn render_credential(prompt: &CredentialPrompt, top: bool, frame: &mut Frame, area: Rect) {
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
        Paragraph::new(tail(&rows, usize::from(question.height))),
        question,
    );
    // One column of padding, as the question has.
    let answer = Rect {
        x: answer.x.saturating_add(1),
        width: answer.width.saturating_sub(2),
        ..answer
    };
    let line = prompt.input.line();
    let shown = if prompt.masked {
        "•".repeat(line.text.chars().count())
    } else {
        line.text.clone()
    };
    render_input_line(&shown, line.col, top, frame, answer);
}

/// What failed in the red border's title, git's words below it, then what
/// to do next and how to close. Long output shows its tail. On a short
/// screen the next step gives way first, so git's words always get a row.
fn render_error(error: &ErrorPopup, frame: &mut Frame, area: Rect) {
    let width = COMMIT_WIDTH.min(area.width);
    let text_width = usize::from(width.saturating_sub(2)).max(1);
    let rows = wrap(&error.message, text_width);
    let next = wrap(&error.next, text_width);
    let height = u16::try_from(rows.len() + next.len() + 3).unwrap_or(u16::MAX);
    let popup = centered(area, width, height);
    frame.render_widget(Clear, popup);
    let block = Block::bordered()
        .title(format!(" {} ", error.title))
        .border_style(Style::new().fg(Color::Red));
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
    frame.render_widget(Paragraph::new(tail(&rows, usize::from(text.height))), text);
    let yellow = Style::new().fg(Color::Yellow);
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

    use super::*;
    use crate::git::FileEntry;
    use crate::git::ops::GitOp;
    use crate::git::status::EntryKind;
    use crate::git::submodule::{Submodule, SubmoduleState};
    use crate::model::Context;
    use crate::popup::PromptKind;
    use crate::popup::{ConfirmKind, DeleteBranchConfirm, DiscardConfirm, DiscardWhat};
    use crate::view::{buffer_rows, render};

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
        state.files_loaded = true;
        state.files = files
            .iter()
            .map(|path| FileEntry {
                path: (*path).into(),
                orig_path: None,
                index: '.',
                worktree: 'M',
                kind: EntryKind::Changed,
            })
            .collect();
        state.selected = (!files.is_empty()).then_some(0);
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
    fn the_confirm_asks_and_the_bar_offers_yes_and_no() {
        let mut state = workspace(&["a.rs"]);
        state.popups = vec![Popup::Confirm(ConfirmKind::Quit)];
        let (_, rows) = draw(&state, 80, 12);
        assert!(contains(&rows, " Quit "), "{rows:#?}");
        assert!(contains(&rows, "Quit Kika? (y/n)"), "{rows:#?}");
        assert!(rows[11].starts_with(" y yes  n no"), "{rows:#?}");
    }

    #[test]
    fn the_discard_question_names_the_file_and_only_y_discards() {
        let mut state = workspace(&["src/a.rs"]);
        state.popups = vec![Popup::Discard(DiscardConfirm {
            what: DiscardWhat::File,
            path: "src/a.rs".into(),
            op: GitOp::DiscardWorktree(vec!["src/a.rs".into()]),
        })];
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
        Popup::DeleteBranch(DeleteBranchConfirm {
            name: "topic".into(),
            op: GitOp::DeleteBranch {
                name: "topic".into(),
                force,
            },
        })
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
        state.commits.pending = crate::git::commit_files::CommitId::parse("abcdef0123");
        state.popups = vec![Popup::Confirm(ConfirmKind::DetachedCheckout)];
        let (_, rows) = draw(&state, 100, 12);
        assert!(contains(&rows, " Detached checkout "), "{rows:#?}");
        assert!(
            contains(&rows, "Check out abcdef0 as a detached HEAD?"),
            "{rows:#?}"
        );
    }

    #[test]
    fn the_hard_reset_question_names_the_target_and_offers_no_enter() {
        use crate::git::commit_files::CommitId;
        use crate::git::ops::ResetMode;
        use crate::popup::ResetHardConfirm;
        let mut state = workspace(&["a.rs"]);
        state.popups = vec![Popup::ResetHard(ResetHardConfirm {
            short_oid: "abc1234".into(),
            op: GitOp::Reset {
                to: CommitId::parse("abc1234").unwrap(),
                mode: ResetMode::Hard,
            },
        })];
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
    fn the_travel_question_shows_every_preview_line_and_offers_no_enter() {
        use crate::git::commit_files::CommitId;
        use crate::git::undo::{Direction, Expected, Step};
        use crate::popup::TravelConfirm;
        let mut state = workspace(&["a.rs"]);
        state.popups = vec![Popup::Travel(TravelConfirm {
            title: "Undo",
            preview: vec![
                "Undo: commit: second".into(),
                "Branch main moves from ccccccc to bbbbbbb.".into(),
                "Files change to match bbbbbbb.".into(),
                "Commits after bbbbbbb leave main; redo brings them back.".into(),
            ],
            op: GitOp::Travel {
                direction: Direction::Undo,
                step: Step::MoveTo(CommitId::parse("bbb").unwrap()),
                expect: Expected {
                    head_ref: Some(b"refs/heads/main".to_vec()),
                    oid: CommitId::parse("ccc").unwrap(),
                },
            },
        })];
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
        use crate::custom_command::{CustomCommand, OutputMode, Panel, test_table};
        use crate::model::PendingCommand;
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
            target: crate::custom_command::Target {
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
            cursor: 7,
        }];
        // Six rows: the bar leaves five, so three of the eight items show.
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
        // Three of the eight items show at a time.
        state.popups = menu(0);
        let (_, rows) = draw(&state, 30, 6);
        let (_, top) = find(&rows, " Actions ");
        assert!(!rows[usize::from(top)].contains('▲'), "{rows:#?}");
        assert!(rows[usize::from(top) + 4].ends_with("▼┘"), "{rows:#?}");

        state.popups = menu(4);
        let (_, rows) = draw(&state, 30, 6);
        assert!(rows[usize::from(top)].ends_with("▲┐"), "{rows:#?}");
        assert!(rows[usize::from(top) + 4].ends_with("▼┘"), "{rows:#?}");

        state.popups = menu(7);
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
            seq: 0,
            items,
            error: error.map(str::to_string),
            cursor: 1,
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
            seq: 0,
            items: Some(items),
            error: None,
            cursor: 9,
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
            Popup::Confirm(ConfirmKind::Quit),
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
        use crate::recent_list::RecentList;

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
        use crate::recent_list::RecentList;

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
        use crate::recent_list::RecentList;

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
    }
}
