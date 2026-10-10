use std::time::Duration;

use crossterm::event;
use ratatui::DefaultTerminal;
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::intent::AppIntent;
use crate::model::AppState;
use crate::view;

/// The main loop: render, wait for the next intent, update, repeat.
pub async fn run(mut terminal: DefaultTerminal, mut state: AppState) -> color_eyre::Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    spawn_input_listener(tx);

    while !state.should_quit {
        terminal.draw(|frame| view::render(&state, frame))?;
        match rx.recv().await {
            Some(intent) => state.update(intent),
            None => break,
        }
    }
    Ok(())
}

/// Forward terminal events to the message bus from a dedicated thread,
/// since crossterm's `read` blocks.
fn spawn_input_listener(tx: UnboundedSender<AppIntent>) {
    std::thread::spawn(move || {
        while !tx.is_closed() {
            match event::poll(Duration::from_millis(100)) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(_) => break,
            }
            let Ok(event) = event::read() else { break };
            if let Some(intent) = AppIntent::from_event(event)
                && tx.send(intent).is_err()
            {
                break;
            }
        }
    });
}
