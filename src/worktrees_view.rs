//! Draws the Worktrees popup. Reads state only.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::git::diff::sanitize;
use crate::git::worktrees::Worktree;
use crate::model::AppState;
use crate::view::{centered, clip, elide_left, width_of, wrap};
use crate::worktree_list::{WorktreeList, is_current};

/// Hex digits of a detached commit, enough to tell commits apart.
const SHORT_OID: usize = 7;

/// One row per worktree: `*` for the one Kika is in, the branch (or what
/// stands for one), the path and its tags. A long path loses its start to
/// `…`. The cursor row is reversed while the list is on top. Until a load
/// lands, and when it fails, that is said instead.
pub fn render_worktrees(
    state: &AppState,
    list: &WorktreeList,
    top: bool,
    frame: &mut Frame,
    area: Rect,
) {
    let title = " Worktrees ";
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
        (Some(items), None) if items.is_empty() => vec![" No worktrees ".into()],
        (Some(items), None) => items
            .iter()
            .map(|w| row(w, is_current(state, w), room))
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
            let text = clip(row, 0, width);
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

/// `<marker> <branch>  <path>  <tags>`, fitted to `room` columns by
/// eliding the path's start.
fn row(worktree: &Worktree, current: bool, room: usize) -> String {
    let marker = if current { '*' } else { ' ' };
    let label = if worktree.bare {
        "(bare)".to_string()
    } else if let Some(branch) = &worktree.branch {
        branch.display()
    } else {
        let oid = worktree.head.as_ref().map_or("", |id| id.as_str());
        let short: String = oid.chars().take(SHORT_OID).collect();
        format!("(detached) {short}").trim_end().to_string()
    };
    let tags: Vec<&str> = [
        (worktree.main, "main"),
        (worktree.locked, "locked"),
        (worktree.prunable, "missing"),
        (worktree.unsure, "?"),
    ]
    .into_iter()
    .filter_map(|(on, tag)| on.then_some(tag))
    .collect();
    let head = format!(" {marker} {label}  ");
    let tail = if tags.is_empty() {
        " ".to_string()
    } else {
        format!("  {} ", tags.join(" "))
    };
    let path_room = room
        .saturating_sub(width_of(&head) + width_of(&tail))
        .max(1);
    format!(
        "{head}{}{tail}",
        elide_left(worktree.path.display(), path_room)
    )
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::git::worktrees::parse_list;
    use crate::model::Context;
    use crate::popup::Popup;
    use crate::view::{buffer_rows, render};

    const OID: &str = "abcdef1234567890abcdef1234567890abcdef12";

    fn state_with(out: &[u8], repo: &str) -> AppState {
        let mut list = WorktreeList::loading(0);
        list.loaded(Ok(parse_list(out, 0)));
        let mut state =
            AppState::new(Context::Workspace).with_location(Some(repo.into()), "/".into());
        state.popups = vec![Popup::Worktrees(list)];
        state
    }

    fn draw(state: &AppState) -> (ratatui::buffer::Buffer, Vec<String>) {
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        terminal.draw(|frame| render(state, frame)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let rows = buffer_rows(&buffer);
        (buffer, rows)
    }

    fn line_with<'a>(rows: &'a [String], needle: &str) -> (usize, &'a String) {
        rows.iter()
            .enumerate()
            .find(|(_, row)| row.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} not on screen: {rows:#?}"))
    }

    #[test]
    fn rows_show_branch_path_tags_and_the_current_marker_with_the_cursor_reversed() {
        let out = format!(
            "worktree /r/main\0HEAD {OID}\0branch refs/heads/main\0\0\
             worktree /r/wt-feature\0HEAD {OID}\0branch refs/heads/feature\0locked why\0\0\
             worktree /r/gone\0HEAD {OID}\0detached\0prunable\0\0"
        );
        let mut state = state_with(out.as_bytes(), "/r/wt-feature");
        if let Some(Popup::Worktrees(list)) = state.popups.last_mut() {
            list.move_cursor(2);
        }
        let (buffer, rows) = draw(&state);
        line_with(&rows, " Worktrees ");
        let (_, main) = line_with(&rows, "/r/main");
        assert!(main.contains("   main  /r/main  main"), "{main}");
        let (_, feature) = line_with(&rows, "/r/wt-feature");
        assert!(
            feature.contains(" * feature  /r/wt-feature  locked"),
            "{feature}"
        );
        let (y, gone) = line_with(&rows, "/r/gone");
        assert!(
            gone.contains("(detached) abcdef1  /r/gone  missing"),
            "{gone}"
        );

        let x = gone.find("(detached)").unwrap();
        let x = gone[..x].chars().count() as u16;
        let cell = &buffer[(x, y as u16)];
        assert!(cell.style().add_modifier.contains(Modifier::REVERSED));
        let (my, main) = line_with(&rows, "/r/main");
        let mx = main[..main.find("/r/main").unwrap()].chars().count() as u16;
        assert!(
            !buffer[(mx, my as u16)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn a_hostile_path_or_branch_draws_no_escape_byte() {
        let out = b"worktree /r/\x1b[31mevil\0branch refs/heads/x\x1b]0;t\x07\0\0";
        let state = state_with(out, "/r");
        let (_, rows) = draw(&state);
        line_with(&rows, "[31mevil");
        assert!(
            rows.iter()
                .all(|row| !row.contains('\x1b') && !row.contains('\x07'))
        );
    }

    #[test]
    fn loading_empty_and_failed_lists_say_so() {
        let mut state = AppState::new(Context::Workspace);
        state.popups = vec![Popup::Worktrees(WorktreeList::loading(0))];
        line_with(&draw(&state).1, "Loading…");
        let mut list = WorktreeList::loading(0);
        list.loaded(Err("fatal: \x1b[2Jbroken".into()));
        state.popups = vec![Popup::Worktrees(list)];
        let (_, rows) = draw(&state);
        line_with(&rows, "broken");
        assert!(rows.iter().all(|row| !row.contains('\x1b')));
    }

    #[test]
    fn a_long_path_loses_its_start_and_the_row_fits_the_screen() {
        let long = "/very/long/".repeat(20) + "tail-dir";
        let out = format!("worktree {long}\0bare\0\0");
        let state = state_with(out.as_bytes(), "/r");
        let (_, rows) = draw(&state);
        let (_, row) = line_with(&rows, "tail-dir");
        assert!(row.contains('…'), "{row}");
        assert!(row.contains("(bare)"), "{row}");
    }
}
