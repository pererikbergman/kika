//! Runs the app headless: the real model, effect runtime and git runner over
//! a [`TestRepo`], drawn on a `TestBackend`, with keys injected the way the
//! terminal would send them.

use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Modifier;
use ratatui::widgets::Block;
use tokio::sync::mpsc::{self, UnboundedReceiver};

use crate::app;
use crate::e2e::repo::TestRepo;
use crate::effect::Runtime;
use crate::git::CliRunner;
use crate::intent::AppIntent;
use crate::model::{AppState, Context};
use crate::view;

/// How long `wait_idle` lets the app go without news before calling it a
/// hang. It bounds a failure; a passing test never waits this long.
const HANG_TIMEOUT: Duration = Duration::from_secs(10);

/// The app in a test. Every step that feeds it input returns only once it
/// is idle, so assertions never race the effects they caused.
pub struct Harness {
    state: AppState,
    rx: UnboundedReceiver<AppIntent>,
    runtime: Runtime,
    terminal: Terminal<TestBackend>,
}

impl Harness {
    /// Start on an 80x24 screen and wait for the first load and diff.
    pub async fn start(repo: &TestRepo) -> Self {
        Self::start_sized(repo, 80, 24).await
    }

    pub async fn start_sized(repo: &TestRepo, width: u16, height: u16) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(CliRunner::new(repo.path())), tx);
        let mut harness = Self {
            state: AppState::new(Context::Workspace),
            rx,
            runtime,
            terminal: Terminal::new(TestBackend::new(width, height)).unwrap(),
        };
        app::boot(&mut harness.state, &harness.runtime, width, height);
        harness.wait_idle().await;
        harness
    }

    pub async fn press(&mut self, code: KeyCode) {
        self.press_with(code, KeyModifiers::NONE).await;
    }

    /// Send a key through the same mapping real input takes, then wait for idle.
    pub async fn press_with(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        let event = Event::Key(KeyEvent::new(code, modifiers));
        if let Some(intent) = AppIntent::from_event(event) {
            app::dispatch(&mut self.state, &self.runtime, intent);
        }
        self.wait_idle().await;
    }

    /// Type `text` key by key, as a terminal sends it: uppercase with
    /// SHIFT, and `\n` as Enter.
    pub async fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            match c {
                '\n' => self.press(KeyCode::Enter).await,
                c if c.is_uppercase() => {
                    self.press_with(KeyCode::Char(c), KeyModifiers::SHIFT).await
                }
                c => self.press(KeyCode::Char(c)).await,
            }
        }
    }

    /// Draw the current state and return the screen, one string per row.
    pub fn screen(&mut self) -> Vec<String> {
        self.draw();
        view::buffer_rows(self.terminal.backend().buffer())
    }

    /// The trimmed text of the highlighted row in the Files panel, if any.
    pub fn selected_line(&mut self) -> Option<String> {
        self.draw();
        let buffer = self.terminal.backend().buffer();
        // The same split `view::render` makes, down to the panel's inside.
        let [main, _] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(buffer.area);
        let [files, _] =
            Layout::horizontal([Constraint::Percentage(30), Constraint::Fill(1)]).areas(main);
        let inner = Block::bordered().inner(files);
        (inner.top()..inner.bottom())
            .find(|&y| {
                buffer[(inner.x, y)]
                    .style()
                    .add_modifier
                    .contains(Modifier::REVERSED)
            })
            .map(|y| {
                view::row_text(buffer, y, inner.left()..inner.right())
                    .trim()
                    .to_string()
            })
    }

    /// Fail with the whole screen when `needle` is not on it.
    pub fn assert_screen_contains(&mut self, needle: &str) {
        let screen = self.screen();
        assert!(
            screen.iter().any(|row| row.contains(needle)),
            "{needle:?} not on screen:\n{}",
            screen.join("\n")
        );
    }

    pub fn state(&self) -> &AppState {
        &self.state
    }

    fn draw(&mut self) {
        let state = &self.state;
        self.terminal
            .draw(|frame| view::render(state, frame))
            .unwrap();
    }

    /// Feed effect results back in until the app is idle. Idle is read from
    /// the state, never guessed from elapsed time.
    async fn wait_idle(&mut self) {
        loop {
            while let Ok(intent) = self.rx.try_recv() {
                app::dispatch(&mut self.state, &self.runtime, intent);
            }
            if is_idle(&self.state) {
                return;
            }
            match tokio::time::timeout(HANG_TIMEOUT, self.rx.recv()).await {
                Ok(Some(intent)) => app::dispatch(&mut self.state, &self.runtime, intent),
                // The runtime holds a sender, so the channel cannot close.
                Ok(None) => unreachable!("the runtime keeps the channel open"),
                Err(_) => self.hang(),
            }
        }
    }

    fn hang(&mut self) -> ! {
        let screen = self.screen().join("\n");
        let state = &self.state;
        let diff = &state.diff;
        panic!(
            "app not idle after {HANG_TIMEOUT:?}: busy {}, op_seq {:?}, diff seq {:?} \
             pending {} done {} requested {} lines {}\n{screen}",
            state.busy,
            state.op_seq,
            diff.seq,
            diff.pending,
            diff.done,
            diff.requested,
            diff.lines.len(),
        );
    }
}

/// Whether nothing more will change without input: no status load or Git
/// operation in flight, and the diff settled. Diff streams stay out of
/// `busy`, so the Diff pane is read directly.
pub(crate) fn is_idle(state: &AppState) -> bool {
    let diff = &state.diff;
    // A stream that has sent all it was asked for is paused until the model
    // demands more, so nothing arrives on its own.
    let diff_settled =
        diff.seq.is_none() || (!diff.pending && (diff.done || diff.lines.len() >= diff.requested));
    state.busy == 0 && state.op_seq.is_none() && diff_settled
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::diff::{DiffLine, LineKind};

    /// A state with a diff stream for request 1 that has asked for 10 lines.
    fn streaming() -> AppState {
        let mut state = AppState::new(Context::Workspace);
        state.diff.seq = Some(1);
        state.diff.requested = 10;
        state
    }

    fn lines(n: usize) -> Vec<DiffLine> {
        let line = DiffLine {
            kind: LineKind::Context,
            text: "x".into(),
        };
        vec![line; n]
    }

    #[test]
    fn a_fresh_state_with_no_diff_requested_is_idle() {
        assert!(is_idle(&AppState::new(Context::Workspace)));
    }

    #[test]
    fn a_status_load_or_op_in_flight_is_not_idle() {
        let mut state = AppState::new(Context::Workspace);
        state.busy = 1;
        assert!(!is_idle(&state));

        let mut state = AppState::new(Context::Workspace);
        state.op_seq = Some(3);
        assert!(!is_idle(&state));
    }

    #[test]
    fn a_diff_waiting_for_its_first_chunk_is_not_idle() {
        let mut state = streaming();
        state.diff.pending = true;
        assert!(!is_idle(&state));
    }

    #[test]
    fn a_stream_short_of_what_it_asked_for_is_not_idle() {
        let mut state = streaming();
        state.diff.lines = lines(4);
        assert!(!is_idle(&state));
    }

    #[test]
    fn a_finished_diff_is_idle() {
        let mut state = streaming();
        state.diff.lines = lines(4);
        state.diff.done = true;
        assert!(is_idle(&state));
    }

    #[test]
    fn a_stream_paused_at_its_demand_is_idle() {
        let mut state = streaming();
        state.diff.lines = lines(10);
        assert!(is_idle(&state));
    }
}
