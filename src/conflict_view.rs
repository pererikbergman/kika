//! Draws the conflict view in the Diff pane's place: the file's text with
//! each block's sides coloured, and the selected block's first row
//! reversed so it reads without colour.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};

use crate::conflict::{ConflictView, Row, RowKind};
use crate::git::diff::sanitize_body;
use crate::keymap::{KeyPattern, Keymap, registry};
use crate::view::{PanelLook, clip, reversed};

/// What to do when a file cannot be resolved here, with the keys that do
/// it from where the hint is shown: the merge tool from this view, the
/// editor from the file list, since `o` here picks a side.
pub fn elsewhere(keymap: &Keymap) -> String {
    let key = |id: &str| {
        registry()
            .iter()
            .find(|b| b.id == id)
            .and_then(|binding| keymap.keys(binding).first())
            .map(KeyPattern::label)
    };
    let tool = match key("conflict.mergetool") {
        Some(key) => format!("Press {key} to resolve it in your merge tool"),
        None => "Resolve it in your merge tool".to_string(),
    };
    let editor = match (key("conflict.back"), key("files.open")) {
        (Some(back), Some(open)) => format!(", or {back} then {open} to edit it in your editor."),
        _ => ", or edit it in your editor from the file list.".to_string(),
    };
    tool + &editor
}

pub fn render(
    view: &ConflictView,
    keymap: &Keymap,
    look: &PanelLook,
    frame: &mut Frame,
    area: Rect,
) {
    let path = view.path.display();
    let block = match view.loaded {
        Some(_) => {
            let head = format!("Conflict {}/{} ·", view.block + 1, view.block_count());
            look.block(look.path_title(&head, path, ""))
        }
        None => look.block(look.path_title("Conflict ·", path, "")),
    };
    let inner = block.inner(area);
    let (rows, width) = (usize::from(inner.height), usize::from(inner.width));
    let pane = if let Some(error) = &view.error {
        let mut lines: Vec<Line> = error.lines().map(Line::from).collect();
        lines.push(Line::from(""));
        lines.push(Line::from(elsewhere(keymap)));
        Paragraph::new(lines).wrap(Wrap { trim: false })
    } else if view.loaded.is_none() {
        Paragraph::new("Loading…")
    } else {
        let lines = view
            .rows()
            .iter()
            .skip(view.scroll)
            .take(rows)
            .map(|row| styled_row(view, row, width))
            .collect::<Vec<_>>();
        Paragraph::new(lines)
    };
    frame.render_widget(pane.block(block), area);
}

/// One row, coloured by what it is. File text is made safe to draw first.
fn styled_row(view: &ConflictView, row: &Row<'_>, width: usize) -> Line<'static> {
    let block = row
        .block
        .and_then(|i| view.loaded.as_ref()?.file.blocks().nth(i));
    let label = |text: &str, label: &str| {
        if label.is_empty() {
            text.to_string()
        } else {
            format!("{text} {label}")
        }
    };
    let (text, style) = match row.kind {
        RowKind::Text => (sanitize_body(row.line), Style::new()),
        RowKind::Ours => (sanitize_body(row.line), Style::new().fg(Color::Green)),
        RowKind::Base => (
            sanitize_body(row.line),
            Style::new().add_modifier(Modifier::DIM),
        ),
        RowKind::Theirs => (sanitize_body(row.line), Style::new().fg(Color::Cyan)),
        RowKind::OursMarker => (
            label("<<<<<<<", block.map_or("", |b| &b.ours_label)),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        RowKind::BaseMarker => (
            "||||||| base".to_string(),
            Style::new().add_modifier(Modifier::DIM),
        ),
        RowKind::Separator => ("=======".to_string(), Style::new()),
        RowKind::TheirsMarker => (
            label(">>>>>>>", block.map_or("", |b| &b.theirs_label)),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        RowKind::Picked(choice) => (
            format!("✓ {}", choice.name()),
            Style::new().add_modifier(Modifier::BOLD),
        ),
    };
    let line = Line::styled(clip(&text, 0, width), style);
    if row.kind.is_header() && row.block == Some(view.block) {
        reversed(line, width)
    } else {
        line
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::conflict::{Choice, ConflictView};
    use crate::model::{AppState, Context, Focus};
    use crate::view::{buffer_rows, render};

    const TWO: &[u8] = b"top\n<<<<<<< HEAD\nours\n||||||| base\nbase\n=======\ntheirs\n>>>>>>> feature\nmiddle\n<<<<<<< HEAD\nours 2\n=======\ntheirs 2\n>>>>>>> feature\n";

    fn state(view: ConflictView) -> AppState {
        let mut state = AppState::new(Context::Workspace);
        state.files_loaded = true;
        state.focus = Focus::Conflict(view);
        state
    }

    fn draw(state: &AppState) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| render(state, frame)).unwrap();
        terminal.backend().buffer().clone()
    }

    /// The row holding `needle`, and the column it starts at.
    fn find(rows: &[String], needle: &str) -> (usize, usize) {
        rows.iter()
            .enumerate()
            .find_map(|(y, row)| {
                row.find(needle)
                    .map(|byte| (y, row[..byte].chars().count()))
            })
            .unwrap_or_else(|| panic!("{needle:?} not on screen:\n{}", rows.join("\n")))
    }

    #[test]
    fn blocks_show_their_markers_and_coloured_sides_until_picked() {
        let mut view = ConflictView::new("a.txt".into(), 1);
        view.loaded(Ok((TWO.to_vec(), 7)), 20);
        view.choices[1] = Some(Choice::Theirs);
        let buffer = draw(&state(view));
        let rows = buffer_rows(&buffer);
        let screen = rows.join("\n");
        assert!(screen.contains("Conflict 1/2 · a.txt"), "{screen}");

        let (y, x) = find(&rows, "<<<<<<< HEAD");
        let header = buffer[(x as u16, y as u16)].style();
        assert!(
            header
                .add_modifier
                .contains(ratatui::style::Modifier::REVERSED)
        );
        let (y, x) = find(&rows, "ours ");
        assert_eq!(
            buffer[(x as u16, y as u16)].style().fg,
            Some(ratatui::style::Color::Green)
        );
        let (y, x) = find(&rows, "||||||| base");
        let (base_y, _) = find(&rows, "┃base ");
        assert_eq!(base_y, y + 1);
        assert!(
            buffer[(x as u16, base_y as u16)]
                .style()
                .add_modifier
                .contains(ratatui::style::Modifier::DIM)
        );
        find(&rows, "=======");
        let (y, x) = find(&rows, "theirs ");
        assert_eq!(
            buffer[(x as u16, y as u16)].style().fg,
            Some(ratatui::style::Color::Cyan)
        );
        find(&rows, ">>>>>>> feature");

        // The picked block shows only the side kept.
        let (picked, _) = find(&rows, "✓ theirs");
        let (kept, _) = find(&rows, "theirs 2");
        assert_eq!(kept, picked + 1);
        assert!(!screen.contains("ours 2"), "{screen}");
    }

    #[test]
    fn an_unreadable_file_says_why_and_where_to_resolve_it() {
        let mut view = ConflictView::new("a.txt".into(), 1);
        view.loaded(Ok((b"no markers\n".to_vec(), 7)), 20);
        let rows = buffer_rows(&draw(&state(view)));
        let screen = rows.join("\n");
        assert!(
            screen.contains("No conflict markers were found"),
            "{screen}"
        );
        assert!(
            screen.contains("Press M to resolve it in your merge"),
            "{screen}"
        );
    }

    #[test]
    fn the_hint_names_the_keys_that_work_where_it_is_shown() {
        use crate::keymap::{Keymap, remap};
        assert_eq!(
            super::elsewhere(&Keymap::default()),
            "Press M to resolve it in your merge tool, or esc then o to edit it in your editor."
        );
        let remapped = remap(&[("conflict.mergetool", &["T"]), ("files.open", &["e"])]);
        assert_eq!(
            super::elsewhere(&remapped),
            "Press T to resolve it in your merge tool, or esc then e to edit it in your editor."
        );
        let unbound = remap(&[("conflict.mergetool", &[]), ("files.open", &[])]);
        assert_eq!(
            super::elsewhere(&unbound),
            "Resolve it in your merge tool, or edit it in your editor from the file list."
        );
    }

    #[test]
    fn file_text_never_reaches_the_terminal_raw() {
        let mut view = ConflictView::new("a.txt".into(), 1);
        let bytes = b"\x1b[2J\n<<<<<<< a\x1b]0;x\x07\nours\x1b[31m\n=======\n>>>>>>> b\n";
        view.loaded(Ok((bytes.to_vec(), 7)), 20);
        let rows = buffer_rows(&draw(&state(view)));
        assert!(rows.iter().all(|row| !row.contains('\x1b')), "{rows:#?}");
        find(&rows, "\u{FFFD}[2J");
    }
}
