use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crossterm::event::{self, DisableFocusChange, EnableFocusChange};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{cursor, execute};
use ratatui::DefaultTerminal;
use ratatui::layout::Rect;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::time::MissedTickBehavior;

use crate::config::Loaded;
use crate::effect::{Effect, Runtime};
use crate::git::GitRunner;
use crate::git::recording::CommandSink;
use crate::handoff::{self, HandOff};
use crate::intent::AppIntent;
use crate::model::AppState;
use crate::view;
use crate::watch::{WatchConfig, WatchEvent};

/// The main loop: render, wait for the next intent, update, run its effects.
/// The loop only ever awaits the channel; effects report back as intents.
/// Hand-offs are the exception: they run here, on the terminal, in turn.
pub async fn run(
    mut terminal: DefaultTerminal,
    mut state: AppState,
    runner: Arc<dyn GitRunner>,
    sink: CommandSink,
    recent_store: Option<PathBuf>,
    global_config: Option<PathBuf>,
    config: Loaded,
) -> color_eyre::Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    // Before anything runs git, so every command reaches the log.
    let log_tx = tx.clone();
    sink.connect(move |event| {
        let _ = log_tx.send(AppIntent::CommandLogged(event));
    });
    let runtime = Runtime::new(Arc::clone(&runner), tx.clone())
        .with_recent_store(recent_store)
        .with_global_config(global_config)
        .with_diff_pager(config.diff_pager.clone());
    let input = Arc::new(InputGate::default());
    // Watch settings apply from startup only: the timers are spawned once.
    spawn_watch_timers(tx.clone(), &config.watch);
    // Before the first hand-off, whose guard keeps this as the action to restore.
    #[cfg(unix)]
    spawn_interrupt_listener(tx.clone())?;
    spawn_input_listener(tx.clone(), Arc::clone(&input));
    let mut spinner = Spinner::default();
    state.watch.fetch_enabled = config.watch.fetch.is_some();
    // Read by `main` for this repository, so it always applies.
    let loaded = AppIntent::ConfigLoaded {
        repo: state.repo.clone(),
        keymap: config.keymap,
        errors: config.errors,
    };
    dispatch(&mut state, &runtime, loaded);

    let size = terminal.size()?;
    boot(&mut state, &runtime, size.width, size.height);

    while !state.should_quit {
        spinner.follow(state.remote.is_some(), &tx);
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

/// Size the state to the screen, then start: the first status load, or
/// the welcome outside a repository. Shared with the headless harness so
/// tests start the app exactly as `run` does.
pub fn boot(state: &mut AppState, runtime: &Runtime, width: u16, height: u16) {
    // Know the pane height before the first diff request sizes itself.
    // Neither intent asks for the terminal, so there is no hand-off to run.
    dispatch(state, runtime, AppIntent::Resize { width, height });
    // Start the first load before the first draw, so the frame shows "Loading…"
    // instead of waiting on git.
    dispatch(state, runtime, AppIntent::Start);
    // Renderers only matter once the user cycles to one, so the first
    // frame never waits on them.
    runtime.spawn(Effect::FindRenderers);
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
            let _sigint = CatchInterrupts::new()?;
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
        // The other process would read focus reports as typed text.
        execute!(
            io::stdout(),
            DisableFocusChange,
            LeaveAlternateScreen,
            cursor::Show
        )?;
        Ok(guard)
    }
}

impl Drop for Suspended {
    fn drop(&mut self) {
        // Nothing to do with an error here: the next draw shows what is left.
        let _ = execute!(
            io::stdout(),
            EnterAlternateScreen,
            cursor::Hide,
            EnableFocusChange
        );
        let _ = terminal::enable_raw_mode();
    }
}

/// For the length of a hand-off, Ctrl+C in the cooked terminal reaches
/// only the other process: Kika's SIGINT goes to a handler that does
/// nothing, so neither Kika nor its interrupt listener sees it. A handler,
/// unlike ignoring the signal, is reset by `exec`, so the editor still gets
/// it. Dropping the guard puts the previous action back exactly.
struct CatchInterrupts {
    #[cfg(unix)]
    previous: libc::sigaction,
}

impl CatchInterrupts {
    #[cfg(unix)]
    fn new() -> io::Result<Self> {
        // SAFETY: both actions are fully initialised, and the handler only
        // returns, which is async-signal-safe.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = ignore_interrupt_handler();
            action.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut action.sa_mask);
            let mut previous: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(libc::SIGINT, &action, &mut previous) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { previous })
        }
    }

    /// Raw mode reads Ctrl+C as a key here, so there is nothing to catch.
    #[cfg(not(unix))]
    fn new() -> io::Result<Self> {
        Ok(Self {})
    }
}

#[cfg(unix)]
impl Drop for CatchInterrupts {
    fn drop(&mut self) {
        // SAFETY: `previous` is the action `sigaction` handed back in `new`.
        // It cannot fail for SIGINT with a valid action; nothing to report.
        unsafe { libc::sigaction(libc::SIGINT, &self.previous, std::ptr::null_mut()) };
    }
}

/// The hand-off's SIGINT handler: catching the signal is the whole job.
#[cfg(unix)]
extern "C" fn ignore_interrupt(_: libc::c_int) {}

/// `ignore_interrupt` in the form `sigaction` stores, so it can be compared.
#[cfg(unix)]
fn ignore_interrupt_handler() -> libc::sighandler_t {
    ignore_interrupt as extern "C" fn(libc::c_int) as libc::sighandler_t
}

/// Turn every SIGINT from outside into a quit, so Kika ends through the
/// loop and the terminal is restored. Registered here, before the task, so
/// a failure reaches the caller instead of leaving SIGINT unhandled.
#[cfg(unix)]
fn spawn_interrupt_listener(tx: UnboundedSender<AppIntent>) -> io::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut interrupts = signal(SignalKind::interrupt())?;
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = tx.closed() => break,
                received = interrupts.recv() => {
                    if received.is_none() || tx.send(AppIntent::Quit).is_err() {
                        break;
                    }
                }
            }
        }
    });
    Ok(())
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

/// How often the spinner of a running fetch, pull or push turns.
pub const SPINNER_TICK: Duration = Duration::from_millis(100);

/// The ticker that turns the spinner, running only while a remote
/// operation does, so an idle Kika wakes for nothing. `update` stays pure:
/// it only counts the ticks.
#[derive(Default)]
struct Spinner {
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Spinner {
    /// Start ticking when `running` and not yet ticking; stop otherwise.
    fn follow(&mut self, running: bool, tx: &UnboundedSender<AppIntent>) {
        match (&self.task, running) {
            (None, true) => {
                let tx = tx.clone();
                self.task = Some(tokio::spawn(async move {
                    let mut ticks = tokio::time::interval(SPINNER_TICK);
                    ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
                    ticks.tick().await;
                    loop {
                        ticks.tick().await;
                        if tx.send(AppIntent::SpinnerTick).is_err() {
                            break;
                        }
                    }
                }));
            }
            (Some(task), false) => {
                task.abort();
                self.task = None;
            }
            _ => {}
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Send a `Watch` intent per interval in `config`, one task each, until
/// the loop goes away. The first tick is skipped: startup loads already.
/// Ticks missed while the loop was busy are dropped, not caught up.
pub fn spawn_watch_timers(tx: UnboundedSender<AppIntent>, config: &WatchConfig) {
    let mut timers = vec![
        (config.refs, WatchEvent::PollRefs),
        (config.files, WatchEvent::PollFiles),
    ];
    if let Some(fetch) = config.fetch {
        timers.push((fetch, WatchEvent::Fetch));
    }
    for (period, event) in timers {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut ticks = tokio::time::interval(period);
            ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
            ticks.tick().await;
            loop {
                tokio::select! {
                    _ = tx.closed() => break,
                    _ = ticks.tick() => {
                        if tx.send(AppIntent::Watch(event)).is_err() {
                            break;
                        }
                    }
                }
            }
        });
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

    /// The watch intents that arrive within `window` of virtual time.
    async fn watch_intents(config: WatchConfig, window: Duration) -> Vec<WatchEvent> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        spawn_watch_timers(tx, &config);
        let mut events = Vec::new();
        let _ = tokio::time::timeout(window, async {
            while let Some(intent) = rx.recv().await {
                match intent {
                    AppIntent::Watch(event) => events.push(event),
                    other => panic!("expected only watch intents, got {other:?}"),
                }
            }
        })
        .await;
        events
    }

    #[tokio::test(start_paused = true)]
    async fn watch_timers_send_each_event_on_its_interval_after_skipping_the_first_tick() {
        let config = WatchConfig {
            refs: Duration::from_millis(10),
            files: Duration::from_millis(25),
            fetch: Some(Duration::from_millis(40)),
        };

        assert_eq!(watch_intents(config, Duration::from_millis(9)).await, []);

        let events = watch_intents(config, Duration::from_millis(45)).await;
        let count = |e| events.iter().filter(|&&x| x == e).count();
        assert_eq!(count(WatchEvent::PollRefs), 4);
        assert_eq!(count(WatchEvent::PollFiles), 1);
        assert_eq!(count(WatchEvent::Fetch), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn without_a_fetch_interval_no_fetch_is_sent() {
        let config = WatchConfig {
            refs: Duration::from_millis(10),
            files: Duration::from_millis(10),
            fetch: None,
        };
        let events = watch_intents(config, Duration::from_secs(5)).await;
        assert!(events.contains(&WatchEvent::PollRefs));
        assert!(events.contains(&WatchEvent::PollFiles));
        assert!(!events.contains(&WatchEvent::Fetch));
    }

    #[tokio::test(start_paused = true)]
    async fn watch_timers_stop_when_the_loop_goes_away() {
        let config = WatchConfig {
            refs: Duration::from_millis(10),
            files: Duration::from_millis(10),
            fetch: Some(Duration::from_secs(300)),
        };
        let (tx, rx) = mpsc::unbounded_channel();
        spawn_watch_timers(tx.clone(), &config);
        drop(rx);
        // Every timer task held a clone; once they have all ended, only ours is left.
        tokio::time::timeout(Duration::from_secs(1), async {
            while tx.strong_count() > 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("every timer task ends");
    }

    #[tokio::test(start_paused = true)]
    async fn the_spinner_ticks_only_while_a_remote_operation_runs() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut spinner = Spinner::default();

        spinner.follow(true, &tx);
        // Following again while running starts no second ticker.
        spinner.follow(true, &tx);
        tokio::time::sleep(SPINNER_TICK * 3 + SPINNER_TICK / 2).await;
        let mut ticks = 0;
        while let Ok(intent) = rx.try_recv() {
            assert_eq!(intent, AppIntent::SpinnerTick);
            ticks += 1;
        }
        assert_eq!(ticks, 3);

        spinner.follow(false, &tx);
        tokio::time::sleep(SPINNER_TICK * 5).await;
        assert!(rx.try_recv().is_err());
    }

    /// Signal actions belong to the whole process; tests that change them
    /// take turns.
    #[cfg(unix)]
    static SIGNALS: Mutex<()> = Mutex::new(());

    /// The current SIGINT action, read without changing it.
    #[cfg(unix)]
    fn sigint_action() -> libc::sigaction {
        // SAFETY: a null new action only reads the current one into `old`.
        unsafe {
            let mut old: libc::sigaction = std::mem::zeroed();
            assert_eq!(libc::sigaction(libc::SIGINT, std::ptr::null(), &mut old), 0);
            old
        }
    }

    /// The parts of an action that decide what a signal does.
    #[cfg(unix)]
    fn action_parts(action: &libc::sigaction) -> (libc::sighandler_t, libc::c_int, Vec<i32>) {
        let mask = (1..32)
            // SAFETY: `sa_mask` is a valid, initialised signal set.
            .filter(|&sig| unsafe { libc::sigismember(&action.sa_mask, sig) } == 1)
            .collect();
        (action.sa_sigaction, action.sa_flags, mask)
    }

    #[cfg(unix)]
    #[test]
    fn sigint_is_held_back_during_a_hand_off_and_quits_after_it() {
        let _turn = lock(&SIGNALS);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (tx, mut rx) = mpsc::unbounded_channel();
            spawn_interrupt_listener(tx).expect("listener installs");
            let before = sigint_action();

            let guard = CatchInterrupts::new().expect("guard installs");
            let during = sigint_action();
            assert_eq!(during.sa_sigaction, ignore_interrupt_handler());
            // SAFETY: SIGINT now runs the guard's no-op handler.
            unsafe { libc::raise(libc::SIGINT) };
            let held = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await;
            assert!(
                held.is_err(),
                "the listener saw {held:?} during the hand-off"
            );

            drop(guard);
            assert_eq!(action_parts(&sigint_action()), action_parts(&before));
            // SAFETY: SIGINT runs the listener's handler again.
            unsafe { libc::raise(libc::SIGINT) };
            let quit = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await;
            assert_eq!(quit, Ok(Some(AppIntent::Quit)));
        });
    }

    #[cfg(unix)]
    #[test]
    fn ctrl_c_during_a_hand_off_still_reaches_the_other_process() {
        use std::os::unix::process::ExitStatusExt;

        let _turn = lock(&SIGNALS);
        let _guard = CatchInterrupts::new().expect("guard installs");
        let status = std::process::Command::new("sh")
            .args(["-c", "kill -INT $$; sleep 1"])
            .status()
            .unwrap();
        assert_eq!(status.signal(), Some(libc::SIGINT));
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
