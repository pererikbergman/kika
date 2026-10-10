use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph, Wrap};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::git::FileEntry;
use crate::model::{AppState, Context};

/// Draw the state. Reads the model only; never mutates it.
pub fn render(state: &AppState, frame: &mut Frame) {
    let [main, status] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());

    match state.context {
        Context::Workspace => {
            render_files_panel(state, frame, main);
            // The busy indicator goes first so a narrow bar clips the hints, not it.
            let bar = if state.busy > 0 {
                " loading…  r refresh  q quit"
            } else {
                " r refresh  q quit"
            };
            frame.render_widget(Line::from(bar), status);
        }
        Context::Review { number } => {
            frame.render_widget(
                Paragraph::new("").block(Block::bordered().title(format!(" Review #{number} "))),
                main,
            );
            frame.render_widget(Line::from(" q quit"), status);
        }
    }
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
        Paragraph::new(file_lines(&state.files, inner))
    };
    frame.render_widget(panel.block(block), area);
}

/// One line per file that fits in `area`. When some do not, the last row says
/// how many are hidden, so the cut is never silent.
fn file_lines(files: &[FileEntry], area: Rect) -> Vec<Line<'static>> {
    let rows = usize::from(area.height);
    let width = usize::from(area.width);
    if files.len() <= rows {
        return files.iter().map(|f| file_line(f, width)).collect();
    }
    let shown = rows.saturating_sub(1);
    let mut lines: Vec<Line> = files[..shown].iter().map(|f| file_line(f, width)).collect();
    if rows > 0 {
        lines.push(Line::from(format!("+{} more", files.len() - shown)));
    }
    lines
}

/// `XY path`, or `XY orig → path` for renames and copies, fitted to `width`
/// columns. Paths lose their start first, since the file name is at the end;
/// for renames the new path keeps as much room as it can.
fn file_line(entry: &FileEntry, width: usize) -> Line<'static> {
    let letters = entry.status_letters();
    let prefix = letters.width() + 1;
    let text = match &entry.orig_path {
        None => {
            let path = elide_left(&entry.path, width.saturating_sub(prefix));
            format!("{letters} {path}")
        }
        Some(orig) => {
            let room = width.saturating_sub(prefix + " → ".width());
            let orig_room = room
                .saturating_sub(entry.path.width())
                .max(1)
                .min(orig.width());
            let orig = elide_left(orig, orig_room);
            let path = elide_left(&entry.path, room.saturating_sub(orig_room));
            format!("{letters} {orig} → {path}")
        }
    };
    Line::from(text)
}

/// Fit `text` into `max` columns by replacing its start with `…`.
fn elide_left(text: &str, max: usize) -> String {
    if text.width() <= max {
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
            kept += c.width().unwrap_or(0);
            kept < max
        })
        .last()
        .map_or(text.len(), |(i, _)| i);
    format!("…{}", &text[start..])
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use unicode_width::UnicodeWidthStr;

    use super::*;
    use crate::git::status::EntryKind;

    /// Draw into a `width` x `height` buffer and return its rows as text.
    /// The blank cell after a wide glyph is skipped so rows read like the screen.
    fn draw(state: &AppState, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(state, frame)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                let mut row = String::new();
                let mut x = 0;
                while x < buffer.area.width {
                    let symbol = buffer[(x, y)].symbol();
                    row.push_str(symbol);
                    x += symbol.width().max(1) as u16;
                }
                row
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
        assert!(rows[23].contains("r refresh  q quit"), "{rows:#?}");
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
        let rows = draw(&state, 40, 8);
        assert!(contains(&rows, "│fatal: not a git repository"), "{rows:#?}");
        assert!(contains(&rows, "directories): .git"), "{rows:#?}");
        assert!(contains(&rows, "│Press r to retry"), "{rows:#?}");
    }

    #[test]
    fn multi_line_error_keeps_its_lines() {
        let mut state = loaded(vec![]);
        state.status_error = Some("error: first\nhint: second".into());
        let rows = draw(&state, 40, 8);
        assert!(contains(&rows, "│error: first "), "{rows:#?}");
        assert!(contains(&rows, "│hint: second "), "{rows:#?}");
    }

    #[test]
    fn long_path_is_left_elided_to_fit_the_panel() {
        // Inner width 18: " M " leaves 15 columns for the path.
        let state = loaded(vec![file("src/deeply/nested/module/file.rs")]);
        let rows = draw(&state, 20, 6);
        assert_eq!(rows[1], "│ M …module/file.rs│", "{rows:#?}");
    }

    #[test]
    fn long_rename_keeps_the_new_path_and_elides_the_old_one_first() {
        // Inner width 28: "R  " + orig + " → " + path.
        let state = loaded(vec![renamed("src/old/location/a.rs", "src/new/b.rs")]);
        let rows = draw(&state, 30, 6);
        assert_eq!(rows[1], "│R  …tion/a.rs → src/new/b.rs│", "{rows:#?}");
    }

    #[test]
    fn very_long_rename_elides_both_paths() {
        let state = loaded(vec![renamed("old/name.rs", "src/brand/new/location/b.rs")]);
        let rows = draw(&state, 20, 6);
        assert_eq!(rows[1], "│R  … → …ation/b.rs│", "{rows:#?}");
    }

    #[test]
    fn wide_characters_are_measured_by_display_width() {
        // Each CJK glyph is two columns wide; inner width 14 leaves 11 for the path.
        let state = loaded(vec![file("日本語/日本語/長い名前.md")]);
        let rows = draw(&state, 16, 6);
        assert_eq!(rows[1], "│ M …い名前.md │", "{rows:#?}");
        assert!(rows[1].width() <= 16, "{rows:#?}");
    }

    #[test]
    fn overflowing_list_says_how_many_files_are_hidden() {
        // Height 7: one status row and two borders leave 4 rows, so 3 files and "+N more".
        let files = (0..10).map(|i| file(&format!("f{i}"))).collect();
        let rows = draw(&loaded(files), 30, 7);
        assert!(contains(&rows, "│ M f2 "), "{rows:#?}");
        assert!(!contains(&rows, "│ M f3 "), "{rows:#?}");
        assert!(contains(&rows, "│+7 more "), "{rows:#?}");
    }

    #[test]
    fn list_that_fits_exactly_has_no_more_line() {
        let files = (0..4).map(|i| file(&format!("f{i}"))).collect();
        let rows = draw(&loaded(files), 30, 7);
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
}
