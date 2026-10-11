use std::collections::VecDeque;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crossterm::event;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{cursor, execute};
use ratatui::DefaultTerminal;
use ratatui::layout::Rect;
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::effect::{Effect, Runtime};
use crate::git::GitRunner;
use crate::handoff::{self, HandOff};
use crate::intent::AppIntent;
use crate::model::AppState;
use crate::view;

/// The main loop: render, wait for the next intent, update, run its effects.
/// The loop only ever awaits the channel; effects report back as intents.
/// Hand-offs are the exception: they run here, on the terminal, in turn.
pub async fn run(
    mut terminal: DefaultTerminal,
    mut state: AppState,
    runner: Arc<dyn GitRunner>,
) -> color_eyre::Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let runtime = Runtime::new(Arc::clone(&runner), tx.clone());
    let input = Arc::new(InputGate::default());
    spawn_input_listener(tx, Arc::clone(&input));

    let size = terminal.size()?;
    boot(&mut state, &runtime, size.width, size.height);

    while !state.should_quit {
        terminal.draw(|frame| view::render(&state, frame))?;
        let Some(intent) = rx.recv().await else { break };
        let mut jobs = VecDeque::from(dispatch(&mut state, &runtime, intent));
        while let Some(job) = jobs.pop_front() {
            // Editors run where git does, so relative names mean the same
            // to both, inside an entered submodule too.
            let workdir = runner.workdir();
            let intent = suspend_and_run(&mut terminal, &input, job, runner.as_ref(), &workdir)?;
            // Resizes while the terminal was away were never seen.
            let size = terminal.size()?;
            let resize = AppIntent::Resize {
                width: size.width,
                height: size.height,
            };
            jobs.extend(dispatch(&mut state, &runtime, resize));
            jobs.extend(dispatch(&mut state, &runtime, intent));
        }
    }
    Ok(())
}

/// Size the state to the screen, then start the first status load. Shared
/// with the headless harness so tests start the app exactly as `run` does.
pub fn boot(state: &mut AppState, runtime: &Runtime, width: u16, height: u16) {
    // Know the pane height before the first diff request sizes itself.
    // Neither intent asks for the terminal, so there is no hand-off to run.
    dispatch(state, runtime, AppIntent::Resize { width, height });
    // Start the first load before the first draw, so the frame shows "Loading…"
    // instead of waiting on git.
    dispatch(state, runtime, AppIntent::RefreshStatus);
}

/// Apply one intent and spawn the effects it asks for, except hand-offs:
/// those need the terminal, which only the caller has, so they are returned
/// for it to run.
pub fn dispatch(state: &mut AppState, runtime: &Runtime, intent: AppIntent) -> Vec<HandOff> {
    let mut jobs = Vec::new();
    for effect in state.update(intent) {
        match effect {
            Effect::HandOff(job) => jobs.push(job),
            effect => runtime.spawn(effect),
        }
    }
    jobs
}

/// Give the terminal to `job` and take it back, fully redrawn. Input is
/// paused throughout so no key meant for the other process is read here.
fn suspend_and_run(
    terminal: &mut DefaultTerminal,
    input: &InputGate,
    job: HandOff,
    runner: &dyn GitRunner,
    workdir: &Path,
) -> io::Result<AppIntent> {
    // Everything below blocks; tell tokio so other tasks move elsewhere.
    tokio::task::block_in_place(|| {
        let _paused = input.pause();
        let intent = {
            let _restore = Suspended::enter()?;
            let _sigint = CatchInterrupts::new();
            handoff::execute(job, runner, workdir)
        };
        // The other process drew over everything; start from a blank
        // screen at the current size. Not `clear`: it asks the terminal
        // where the cursor is, and a terminal that does not answer would
        // end Kika here.
        let size = terminal.size()?;
        terminal.resize(Rect::new(0, 0, size.width, size.height))?;
        Ok(intent)
    })
}

/// The terminal as it was before Kika took it: cooked mode, main screen,
/// cursor shown. Dropping it takes the terminal back, even on a panic.
struct Suspended;

impl Suspended {
    fn enter() -> io::Result<Self> {
        let guard = Suspended;
        terminal::disable_raw_mode()?;
        execute!(io::stdout(), LeaveAlternateScreen, cursor::Show)?;
        Ok(guard)
    }
}

impl Drop for Suspended {
    fn drop(&mut self) {
        // Nothing to do with an error here: the next draw shows what is left.
        let _ = execute!(io::stdout(), EnterAlternateScreen, cursor::Hide);
        let _ = terminal::enable_raw_mode();
    }
}

/// While alive, Ctrl+C in the cooked terminal reaches only the other
/// process: Kika handles SIGINT instead of dying of it. A handler, unlike
/// ignoring the signal, is reset by `exec`, so the editor still gets it.
struct CatchInterrupts {
    #[cfg(unix)]
    _signal: Option<tokio::signal::unix::Signal>,
}

impl CatchInterrupts {
    fn new() -> Self {
        Self {
            #[cfg(unix)]
            _signal: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).ok(),
        }
    }
}

/// Lets the loop stop the input listener: the listener holds the lock
/// around each poll and read, and the loop holds it for a whole hand-off.
/// `wanted` keeps the listener from grabbing the lock straight back while
/// the loop waits for it.
#[derive(Default)]
struct InputGate {
    lock: Mutex<()>,
    wanted: AtomicBool,
}

impl InputGate {
    /// Wait for the read in progress, if any, then keep the listener out
    /// until the guard drops.
    fn pause(&self) -> Paused<'_> {
        self.wanted.store(true, Ordering::SeqCst);
        Paused {
            gate: self,
            _guard: lock(&self.lock),
        }
    }

    /// Held by the listener around one poll and read.
    fn reading(&self) -> MutexGuard<'_, ()> {
        while self.wanted.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(10));
        }
        lock(&self.lock)
    }
}

struct Paused<'a> {
    gate: &'a InputGate,
    _guard: MutexGuard<'a, ()>,
}

impl Drop for Paused<'_> {
    fn drop(&mut self) {
        self.gate.wanted.store(false, Ordering::SeqCst);
    }
}

/// Forward terminal events to the message bus from a dedicated thread,
/// since crossterm's `read` blocks. Reads only through `gate`, so a
/// hand-off can stop them.
fn spawn_input_listener(tx: UnboundedSender<AppIntent>, gate: Arc<InputGate>) {
    std::thread::spawn(move || {
        while !tx.is_closed() {
            let event = {
                let _reading = gate.reading();
                match event::poll(Duration::from_millis(100)) {
                    Ok(true) => event::read(),
                    Ok(false) => continue,
                    Err(e) => Err(e),
                }
            };
            let Ok(event) = event else { break };
            if let Some(intent) = AppIntent::from_event(event)
                && tx.send(intent).is_err()
            {
                break;
            }
        }
    });
}

/// A poisoned lock only means another thread panicked; the gate still works.
fn lock(mutex: &Mutex<()>) -> MutexGuard<'_, ()> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dispatch_returns_hand_offs_instead_of_spawning_them() {
        use crate::git::FileEntry;
        use crate::git::fake::FakeRunner;
        use crate::git::status::EntryKind;
        use crate::model::Context;

        let (tx, mut rx) = mpsc::unbounded_channel();
        // No expectations queued, so any git call would panic.
        let runtime = Runtime::new(Arc::new(FakeRunner::new()), tx);
        let mut state = AppState::new(Context::Workspace);
        state.files = vec![FileEntry {
            path: "a.rs".into(),
            orig_path: None,
            index: '.',
            worktree: 'M',
            kind: EntryKind::Changed,
        }];
        state.selected = Some(0);

        let jobs = dispatch(&mut state, &runtime, AppIntent::OpenInEditor);

        assert_eq!(
            jobs,
            [HandOff::OpenFile {
                path: "a.rs".into(),
                line: None
            }]
        );
        drop(runtime);
        assert_eq!(rx.recv().await, None);
    }

    #[test]
    fn a_paused_gate_keeps_the_reader_out_until_it_is_released() {
        let gate = Arc::new(InputGate::default());
        let paused = gate.pause();
        let reader = {
            let gate = Arc::clone(&gate);
            std::thread::spawn(move || {
                let _reading = gate.reading();
            })
        };
        std::thread::sleep(Duration::from_millis(50));
        assert!(!reader.is_finished());
        drop(paused);
        reader.join().unwrap();
    }

    #[test]
    fn a_busy_reader_cannot_starve_a_pause() {
        let gate = Arc::new(InputGate::default());
        let stop = Arc::new(AtomicBool::new(false));
        let reader = {
            let (gate, stop) = (Arc::clone(&gate), Arc::clone(&stop));
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let _reading = gate.reading();
                    std::thread::sleep(Duration::from_millis(5));
                }
            })
        };
        for _ in 0..20 {
            drop(gate.pause());
        }
        stop.store(true, Ordering::SeqCst);
        reader.join().unwrap();
    }
}
