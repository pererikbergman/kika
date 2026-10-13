//! Draws the Files panel: the working tree's changed files, one row each.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};

use crate::git::FileEntry;
use crate::git::status::EntryKind;
use crate::model::AppState;
use crate::view::{PanelLook, elide_left, filter_title, no_matches, range_row, width_of};

/// The Files panel: an error, a placeholder until the first load, or one
/// line per file. Collapsed, a count stands in for the list.
pub(crate) fn render_files_panel(
    state: &AppState,
    look: &PanelLook,
    expanded: bool,
    frame: &mut Frame,
    area: Rect,
) {
    let count = match state.files.rows.len() {
        1 => "1 file".to_string(),
        n => format!("{n} files"),
    };
    let shown = state.files.visible();
    let range = state.files.range();
    let selected = match state.files.range_anchor {
        Some(_) => format!(" · {} selected", range.len()),
        None => String::new(),
    };
    let title = if !state.files.filter.is_empty() {
        let counts = format!("{} of {}{selected}", shown.len(), state.files.rows.len());
        look.title(&filter_title(look, "Files", &state.files.filter, &counts))
    } else if !selected.is_empty() {
        look.title(&format!("Files{selected}"))
    } else if look.title_only && state.files.loaded && state.files.error.is_none() {
        look.title(&format!("Files · {count}"))
    } else {
        look.title("Files")
    };
    let block = look.block(title);
    let inner = block.inner(area);
    let panel = if let Some(error) = &state.files.error {
        let mut lines: Vec<Line> = error.lines().map(Line::from).collect();
        lines.push(Line::from("Press r to retry"));
        Paragraph::new(lines).wrap(Wrap { trim: false })
    } else if !state.files.loaded {
        Paragraph::new("Loading…")
    } else if state.files.rows.is_empty() {
        Paragraph::new("No changes")
    } else if shown.is_empty() {
        Paragraph::new(no_matches())
    } else if !expanded {
        Paragraph::new(count)
    } else {
        Paragraph::new(file_lines(
            &state.files.rows,
            &shown,
            // Rows are listed here, so the cursor is on one.
            Some(state.files.selected),
            &range,
            inner,
        ))
    };
    frame.render_widget(panel.block(block), area);
}

/// The `shown` files that fit in `area`, scrolled so `selected` is visible.
/// It and the files in `range` are drawn reversed. When files below the
/// window are cut, the last row says how many, so the cut is never silent.
fn file_lines(
    files: &[FileEntry],
    shown: &[usize],
    selected: Option<usize>,
    range: &[usize],
    area: Rect,
) -> Vec<Line<'static>> {
    let rows = usize::from(area.height);
    let width = usize::from(area.width);
    let line = |&i: &usize| range_row(file_line(&files[i], width), i, selected, range, width);
    if shown.len() <= rows {
        return shown.iter().map(line).collect();
    }
    let selected = selected
        .and_then(|s| shown.iter().position(|&i| i == s))
        .unwrap_or(0);
    if rows <= 1 {
        // No room for a "+N more" row; the selection matters more.
        return shown.iter().skip(selected).take(rows).map(line).collect();
    }
    // One row is kept for "+N more" unless the window reaches the end.
    let more = rows - 1;
    let start = selected.saturating_sub(more - 1);
    if start + rows >= shown.len() {
        let start = shown.len() - rows;
        return shown.iter().skip(start).map(line).collect();
    }
    let mut lines: Vec<Line> = shown.iter().skip(start).take(more).map(line).collect();
    lines.push(Line::from(format!("+{} more", shown.len() - start - more)));
    lines
}

/// `XY path`, or `XY orig → path` for renames and copies, fitted to `width`
/// columns. Paths lose their start first, since the file name is at the end;
/// for renames the new path keeps as much room as it can. A conflict reads
/// `! XY path` in red bold, so it stands out without colour too.
fn file_line(entry: &FileEntry, width: usize) -> Line<'static> {
    let conflicted = entry.kind == EntryKind::Conflicted;
    let letters = if conflicted {
        format!("! {}", entry.status_letters())
    } else {
        entry.status_letters()
    };
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
    if conflicted {
        Line::styled(
            text,
            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        )
    } else {
        Line::from(text)
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::{Color, Modifier};

    use crate::git::FileEntry;
    use crate::git::status::EntryKind;
    use crate::test_support::screen::{
        cell_at, contains, draw, draw_buffer, draw_files, file, files_rows, find, loaded, renamed,
        status_focused, thin,
    };
    use crate::view::{buffer_rows, render, width_of};

    #[test]
    fn a_collapsed_files_panel_counts_the_files() {
        let mut state = status_focused(&loaded(vec![file("a.rs"), file("b.rs")]));
        let rows = draw(&state, 80, 24);
        assert!(files_rows(&rows).1.starts_with("│2 files "), "{rows:#?}");
        assert!(!contains(&rows, " M a.rs"), "{rows:#?}");
        state.files.rows.pop();
        let rows = draw(&state, 80, 24);
        assert!(files_rows(&rows).1.starts_with("│1 file "), "{rows:#?}");
        state.files.rows.clear();
        let rows = draw(&state, 80, 24);
        assert!(files_rows(&rows).1.starts_with("│No changes "), "{rows:#?}");
    }

    #[test]
    fn a_conflicted_row_is_marked_and_red_bold() {
        let mut state = loaded(vec![file("a.txt")]);
        state.files.rows.insert(
            0,
            FileEntry {
                path: "both.txt".into(),
                orig_path: None,
                index: 'U',
                worktree: 'U',
                kind: EntryKind::Conflicted,
            },
        );
        state.files.selected = 1;
        let buffer = draw_buffer(&state, 80, 24);
        let rows = thin(buffer_rows(&buffer));
        assert!(contains(&rows, "│! UU both.txt "), "{rows:#?}");
        assert!(contains(&rows, "│ M a.txt "), "{rows:#?}");
        let (x, y) = find(&buffer, "! UU both.txt");
        for x in x..x + 13 {
            let style = buffer[(x, y)].style();
            assert_eq!(style.fg, Some(Color::Red), "{x}");
            assert!(style.add_modifier.contains(Modifier::BOLD), "{x}");
        }
        let (x, y) = find(&buffer, " M a.txt");
        assert_ne!(buffer[(x + 1, y)].style().fg, Some(Color::Red));
    }

    #[test]
    fn workspace_says_no_changes_when_clean() {
        assert!(contains(&draw(&loaded(vec![]), 40, 13), "No changes"));
    }

    #[test]
    fn workspace_shows_status_error_wrapped_with_a_retry_hint() {
        let mut state = loaded(vec![]);
        state.files.error =
            Some("fatal: not a git repository (or any of the parent directories): .git".into());
        let rows = draw_files(&state, 40, 13);
        assert!(contains(&rows, "│fatal: not a git repository"), "{rows:#?}");
        assert!(contains(&rows, "directories): .git"), "{rows:#?}");
        assert!(contains(&rows, "│Press r to retry"), "{rows:#?}");
    }

    #[test]
    fn multi_line_error_keeps_its_lines() {
        let mut state = loaded(vec![]);
        state.files.error = Some("error: first\nhint: second".into());
        let rows = draw_files(&state, 40, 13);
        assert!(contains(&rows, "│error: first "), "{rows:#?}");
        assert!(contains(&rows, "│hint: second "), "{rows:#?}");
    }

    #[test]
    fn long_path_is_left_elided_to_fit_the_panel() {
        // Inner width 18: " M " leaves 15 columns for the path.
        let state = loaded(vec![file("src/deeply/nested/module/file.rs")]);
        let rows = draw_files(&state, 20, 11);
        assert_eq!(rows[2], "│ M …module/file.rs│", "{rows:#?}");
    }

    #[test]
    fn long_rename_keeps_the_new_path_and_elides_the_old_one_first() {
        // Inner width 28: "R  " + orig + " → " + path.
        let state = loaded(vec![renamed("src/old/location/a.rs", "src/new/b.rs")]);
        let rows = draw_files(&state, 30, 11);
        assert_eq!(rows[2], "│R  …tion/a.rs → src/new/b.rs│", "{rows:#?}");
    }

    #[test]
    fn very_long_rename_elides_both_paths() {
        let state = loaded(vec![renamed("old/name.rs", "src/brand/new/location/b.rs")]);
        let rows = draw_files(&state, 20, 11);
        assert_eq!(rows[2], "│R  … → …ation/b.rs│", "{rows:#?}");
    }

    #[test]
    fn wide_characters_are_measured_by_display_width() {
        // Each CJK glyph is two columns wide; inner width 14 leaves 11 for the path.
        let state = loaded(vec![file("日本語/日本語/長い名前.md")]);
        let rows = draw_files(&state, 16, 11);
        assert_eq!(rows[2], "│ M …い名前.md │", "{rows:#?}");
        assert!(width_of(&rows[2]) <= 16, "{rows:#?}");
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
        assert_eq!(rows[2], "│ M …me<U+200B>│", "{rows:#?}");
        assert!(width_of(&rows[2]) <= 16, "{rows:#?}");
    }

    #[test]
    fn overflowing_list_says_how_many_files_are_hidden() {
        // Height 12: the bar, four title rows and two borders leave 5 rows,
        // so 4 files and "+N more".
        let files = (0..10).map(|i| file(&format!("f{i}"))).collect();
        let rows = draw_files(&loaded(files), 30, 12);
        assert!(contains(&rows, "│ M f3 "), "{rows:#?}");
        assert!(!contains(&rows, "│ M f4 "), "{rows:#?}");
        assert!(contains(&rows, "│+6 more "), "{rows:#?}");
    }

    #[test]
    fn list_that_fits_exactly_has_no_more_line() {
        let files = (0..4).map(|i| file(&format!("f{i}"))).collect();
        let rows = draw_files(&loaded(files), 30, 12);
        assert!(contains(&rows, "│ M f3 "), "{rows:#?}");
        assert!(!contains(&rows, "more"), "{rows:#?}");
    }

    #[test]
    fn the_selected_file_row_is_reversed_across_the_panel() {
        let mut state = loaded(vec![file("a.rs"), file("b.rs")]);
        state.files.selected = 1;
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
        // Height 12: 5 rows, so 4 files plus "+N more" at a time.
        let mut state = loaded((0..10).map(|i| file(&format!("f{i}"))).collect());
        state.files.selected = 5;
        let rows = draw_files(&state, 30, 12);
        assert!(contains(&rows, "│ M f5 "), "{rows:#?}");
        assert!(!contains(&rows, "│ M f1 "), "{rows:#?}");
        assert!(contains(&rows, "│+4 more "), "{rows:#?}");

        // Near the end the window shows the last files and no "more" row.
        state.files.selected = 8;
        let rows = draw_files(&state, 30, 12);
        for name in ["f6", "f7", "f8", "f9"] {
            assert!(contains(&rows, &format!("│ M {name} ")), "{rows:#?}");
        }
        assert!(!contains(&rows, "more"), "{rows:#?}");
    }

    #[test]
    fn a_one_row_list_shows_the_selection() {
        // Height 4: three rows over the bar, all Files', one inside its border.
        let mut state = loaded((0..10).map(|i| file(&format!("f{i}"))).collect());
        state.files.selected = 4;
        let rows = draw_files(&state, 30, 4);
        assert!(contains(&rows, "│ M f4 "), "{rows:#?}");
    }

    #[test]
    fn a_filtered_files_title_counts_what_shows() {
        let files: Vec<FileEntry> = (0..7)
            .map(|i| file(&format!("a{i}.rs")))
            .chain((0..3).map(|i| file(&format!("b{i}.md"))))
            .collect();
        let mut state = loaded(files);
        state.files.selected = 7;
        state.files.filter = "md".into();
        let rows = draw(&state, 100, 30);
        assert!(contains(&rows, "Files · /md 3 of 10"), "{rows:#?}");
        assert!(contains(&rows, "b2.md"), "{rows:#?}");
        assert!(!contains(&rows, "a0.rs"), "{rows:#?}");

        state.files.filter = "zzz".into();
        let rows = draw(&state, 100, 30);
        assert!(contains(&rows, "No matches"), "{rows:#?}");
    }

    #[test]
    fn range_rows_are_drawn_like_the_selected_row() {
        let mut state = loaded(vec![file("a.rs"), file("b.rs"), file("c.rs")]);
        state.files.selected = 1;
        state.files.range_anchor = Some("c.rs".into());
        let buffer = draw_buffer(&state, 100, 20);
        let reversed = |needle| {
            cell_at(&buffer, needle)
                .modifier
                .contains(Modifier::REVERSED)
        };
        assert!(reversed("b.rs"), "the cursor row");
        assert!(reversed("c.rs"), "the anchor row");
        assert!(!reversed("a.rs"));
        assert!(contains(&buffer_rows(&buffer), "Files · 2 selected"));
        // The cursor, the end that moves, stands out from the rest.
        let bold = |needle| cell_at(&buffer, needle).modifier.contains(Modifier::BOLD);
        assert!(bold("b.rs"), "the cursor row");
        assert!(!bold("c.rs"), "the anchor row");
    }
}
