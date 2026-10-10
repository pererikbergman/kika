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

    // Know the pane height before the first diff request sizes itself.
    let size = terminal.size()?;
    state.update(AppIntent::Resize {
        width: size.width,
        height: size.height,
    });
    // Start the first load before the first draw, so the frame shows "Loading…"
    // instead of waiting on git.
    for effect in state.update(AppIntent::RefreshStatus) {
        runtime.spawn(effect);
    }

    while !state.should_quit {
        terminal.draw(|frame| view::render(&state, frame))?;
        let Some(intent) = rx.recv().await else { break };
        for effect in state.update(intent) {
            runtime.spawn(effect);
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
