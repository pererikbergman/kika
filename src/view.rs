use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph};

use crate::git::FileEntry;
use crate::model::{AppState, Context};

/// Draw the state. Reads the model only; never mutates it.
pub fn render(state: &AppState, frame: &mut Frame) {
    let [main, status] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());

    match state.context {
        Context::Workspace => {
            frame.render_widget(files_panel(state), main);
            let mut bar = " r refresh  q quit".to_string();
            if state.busy > 0 {
                bar.push_str(" loading…");
            }
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
fn files_panel(state: &AppState) -> Paragraph<'_> {
    let lines: Vec<Line> = if let Some(error) = &state.status_error {
        vec![Line::from(error.as_str())]
    } else if !state.files_loaded {
        vec![Line::from("Loading…")]
    } else if state.files.is_empty() {
        vec![Line::from("No changes")]
    } else {
        state.files.iter().map(file_line).collect()
    };
    Paragraph::new(lines).block(Block::bordered().title(" Files "))
}

fn file_line(entry: &FileEntry) -> Line<'static> {
    let letters = entry.status_letters();
    match &entry.orig_path {
        Some(orig) => Line::from(format!("{letters} {orig} → {}", entry.path)),
        None => Line::from(format!("{letters} {}", entry.path)),
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::git::status::EntryKind;

    const WIDTH: u16 = 40;

    fn draw(state: &AppState) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(WIDTH, 8)).unwrap();
        terminal.draw(|frame| render(state, frame)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..WIDTH)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect()
            })
            .collect()
    }

    fn contains(rows: &[String], needle: &str) -> bool {
        rows.iter().any(|row| row.contains(needle))
    }

    #[test]
    fn review_context_shows_number_in_title() {
        let rows = draw(&AppState::new(Context::Review { number: 123 }));
        assert!(rows[0].contains("Review #123"), "{rows:#?}");
    }

    #[test]
    fn workspace_shows_loading_before_status_arrives() {
        let mut state = AppState::new(Context::Workspace);
        state.busy = 1;
        let rows = draw(&state);
        assert!(rows[0].contains(" Files "), "{rows:#?}");
        assert!(contains(&rows, "│Loading…"), "{rows:#?}");
        assert!(rows.last().unwrap().contains("loading…"), "{rows:#?}");
    }

    #[test]
    fn workspace_lists_files_with_status_letters_and_renames() {
        let mut state = AppState::new(Context::Workspace);
        state.files_loaded = true;
        state.files = vec![
            FileEntry {
                path: "new.txt".into(),
                orig_path: Some("old.txt".into()),
                index: 'R',
                worktree: '.',
                kind: EntryKind::Renamed,
            },
            FileEntry {
                path: "new_file".into(),
                orig_path: None,
                index: '?',
                worktree: '?',
                kind: EntryKind::Untracked,
            },
        ];
        let rows = draw(&state);
        assert!(contains(&rows, "R  old.txt → new.txt"), "{rows:#?}");
        assert!(contains(&rows, "?? new_file"), "{rows:#?}");
        assert!(!contains(&rows, "loading…"), "{rows:#?}");
        assert!(
            rows.last().unwrap().contains("r refresh  q quit"),
            "{rows:#?}"
        );
    }

    #[test]
    fn workspace_says_no_changes_when_clean() {
        let mut state = AppState::new(Context::Workspace);
        state.files_loaded = true;
        assert!(contains(&draw(&state), "No changes"));
    }

    #[test]
    fn workspace_shows_status_error() {
        let mut state = AppState::new(Context::Workspace);
        state.files_loaded = true;
        state.status_error = Some("fatal: not a git repository".into());
        assert!(contains(&draw(&state), "fatal: not a git repository"));
    }
}
