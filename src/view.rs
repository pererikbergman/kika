use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph};

use crate::model::{AppState, Context};

/// Draw the state. Reads the model only; never mutates it.
pub fn render(state: &AppState, frame: &mut Frame) {
    let [main, status] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());

    let title = match state.context {
        Context::Workspace => " Workspace ".to_string(),
        Context::Review { number } => format!(" Review #{number} "),
    };
    frame.render_widget(
        Paragraph::new("").block(Block::bordered().title(title)),
        main,
    );
    frame.render_widget(Line::from(" q quit"), status);
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    #[test]
    fn review_context_shows_number_in_title() {
        let mut terminal = Terminal::new(TestBackend::new(30, 5)).unwrap();
        let state = AppState::new(Context::Review { number: 123 });
        terminal.draw(|frame| render(&state, frame)).unwrap();

        let top_row: String = (0..30)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol().to_string())
            .collect();
        assert!(top_row.contains("Review #123"), "{top_row:?}");
    }
}
