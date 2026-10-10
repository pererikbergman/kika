use std::sync::Arc;
use std::time::Duration;

use crossterm::event;
use ratatui::DefaultTerminal;
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::effect::Runtime;
use crate::git::GitRunner;
use crate::intent::AppIntent;
use crate::model::AppState;
use crate::view;

/// The main loop: render, wait for the next intent, update, run its effects.
/// The loop only ever awaits the channel; effects report back as intents.
pub async fn run(
    mut terminal: DefaultTerminal,
    mut state: AppState,
    runner: Arc<dyn GitRunner>,
) -> color_eyre::Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let runtime = Runtime::new(runner, tx.clone());
    spawn_input_listener(tx);

    let size = terminal.size()?;
    boot(&mut state, &runtime, size.width, size.height);

    while !state.should_quit {
        terminal.draw(|frame| view::render(&state, frame))?;
        let Some(intent) = rx.recv().await else { break };
        dispatch(&mut state, &runtime, intent);
    }
    Ok(())
}

/// Size the state to the screen, then start the first status load. Shared
/// with the headless harness so tests start the app exactly as `run` does.
pub fn boot(state: &mut AppState, runtime: &Runtime, width: u16, height: u16) {
    // Know the pane height before the first diff request sizes itself.
    dispatch(state, runtime, AppIntent::Resize { width, height });
    // Start the first load before the first draw, so the frame shows "Loading…"
    // instead of waiting on git.
    dispatch(state, runtime, AppIntent::RefreshStatus);
}

/// Apply one intent and run the effects it asks for.
pub fn dispatch(state: &mut AppState, runtime: &Runtime, intent: AppIntent) {
    for effect in state.update(intent) {
        runtime.spawn(effect);
    }
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
