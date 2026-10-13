//! Tells the Command log about every git process Kika starts: one event
//! when it starts and one when it ends. Timing happens here, on the I/O
//! side, so `update` stays pure.

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use super::command_line::display_line;
use super::runner::{AttachedExit, ChildHandle, GitChild, GitCommand, GitOutput, GitRunner};

/// One step in a command's life, as the Command log hears of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandEvent {
    /// Sent before the process starts. `line` is already cleaned of secrets.
    Started {
        id: u64,
        line: String,
        /// Run on the terminal during a hand-off.
        attached: bool,
    },
    /// Sent exactly once per `Started`.
    Finished {
        id: u64,
        outcome: CommandOutcome,
        elapsed: Duration,
    },
}

/// How a command ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandOutcome {
    Ok,
    /// Background runs only know they failed; attached runs carry their
    /// exit, such as `status 1`.
    Failed(Option<String>),
    /// The process could not be started or waited for.
    NotStarted,
    /// A streamed command whose reader gave up before it ended.
    Stopped,
}

type Listener = Box<dyn Fn(CommandEvent) + Send + Sync>;

/// Where events go. Built before the loop exists, and connected by it, so
/// events sent before then are dropped. It hands out the ids too, so every
/// process that reports here, git or not, gets one of its own.
#[derive(Clone, Default)]
pub struct CommandSink(Arc<SinkInner>);

#[derive(Default)]
struct SinkInner {
    listener: OnceLock<Listener>,
    next_id: AtomicU64,
}

impl CommandSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Send every later event to `listener`. Only the first call counts.
    pub fn connect(&self, listener: impl Fn(CommandEvent) + Send + Sync + 'static) {
        let _ = self.0.listener.set(Box::new(listener));
    }

    pub fn send(&self, event: CommandEvent) {
        if let Some(listener) = self.0.listener.get() {
            listener(event);
        }
    }

    /// An id no other command reporting to this sink has.
    pub fn next_id(&self) -> u64 {
        self.0.next_id.fetch_add(1, Ordering::Relaxed)
    }
}

impl fmt::Debug for CommandSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandSink")
            .field("connected", &self.0.listener.get().is_some())
            .finish()
    }
}

/// A [`GitRunner`] that reports each command to a [`CommandSink`] and
/// hands it to `inner` unchanged.
pub struct RecordingRunner {
    inner: Arc<dyn GitRunner>,
    sink: CommandSink,
}

impl RecordingRunner {
    pub fn new(inner: Arc<dyn GitRunner>, sink: CommandSink) -> Self {
        Self { inner, sink }
    }

    /// Announce `cmd` and return its id and start time.
    fn start(&self, cmd: &GitCommand, attached: bool) -> (u64, Instant) {
        let id = self.sink.next_id();
        self.sink.send(CommandEvent::Started {
            id,
            line: display_line(cmd),
            attached,
        });
        (id, Instant::now())
    }

    fn finish(&self, id: u64, start: Instant, outcome: CommandOutcome) {
        self.sink.send(CommandEvent::Finished {
            id,
            outcome,
            elapsed: start.elapsed(),
        });
    }
}

/// What a finished background run says.
fn outcome_of(output: &io::Result<GitOutput>) -> CommandOutcome {
    match output {
        Ok(output) if output.success => CommandOutcome::Ok,
        Ok(_) => CommandOutcome::Failed(None),
        Err(_) => CommandOutcome::NotStarted,
    }
}

impl GitRunner for RecordingRunner {
    fn run(&self, cmd: &GitCommand) -> io::Result<GitOutput> {
        let (id, start) = self.start(cmd, false);
        let output = self.inner.run(cmd);
        self.finish(id, start, outcome_of(&output));
        output
    }

    fn run_attached(&self, cmd: &GitCommand) -> io::Result<AttachedExit> {
        let (id, start) = self.start(cmd, true);
        let exit = self.inner.run_attached(cmd);
        let outcome = match &exit {
            Ok(exit) if exit.success => CommandOutcome::Ok,
            Ok(exit) => CommandOutcome::Failed(Some(exit.status.clone())),
            Err(_) => CommandOutcome::NotStarted,
        };
        self.finish(id, start, outcome);
        exit
    }

    fn spawn(&self, cmd: &GitCommand) -> io::Result<GitChild> {
        let (id, start) = self.start(cmd, false);
        match self.inner.spawn(cmd) {
            Ok(child) => Ok(GitChild {
                stdout: child.stdout,
                handle: Arc::new(RecordingHandle {
                    inner: child.handle,
                    sink: self.sink.clone(),
                    id,
                    start,
                    done: AtomicBool::new(false),
                }),
            }),
            Err(e) => {
                self.finish(id, start, CommandOutcome::NotStarted);
                Err(e)
            }
        }
    }

    fn interrupt(&self) {
        self.inner.interrupt();
    }

    fn workdir(&self) -> PathBuf {
        self.inner.workdir()
    }

    fn set_workdir(&self, dir: PathBuf) {
        self.inner.set_workdir(dir);
    }
}

/// A spawned child's handle that reports its end: on the first `finish`,
/// or as `Stopped` when the last copy is dropped without one.
struct RecordingHandle {
    inner: Arc<dyn ChildHandle>,
    sink: CommandSink,
    id: u64,
    start: Instant,
    done: AtomicBool,
}

impl RecordingHandle {
    /// Send `outcome` unless this id has finished already.
    fn report(&self, outcome: CommandOutcome) {
        if !self.done.swap(true, Ordering::SeqCst) {
            self.sink.send(CommandEvent::Finished {
                id: self.id,
                outcome,
                elapsed: self.start.elapsed(),
            });
        }
    }
}

impl ChildHandle for RecordingHandle {
    fn kill(&self) {
        self.inner.kill();
    }

    fn finish(&self) -> io::Result<GitOutput> {
        let output = self.inner.finish();
        let outcome = match &output {
            Ok(output) if output.success => CommandOutcome::Ok,
            // It started, so a failed wait is a failure, not "not started".
            Ok(_) | Err(_) => CommandOutcome::Failed(None),
        };
        self.report(outcome);
        output
    }
}

impl Drop for RecordingHandle {
    fn drop(&mut self) {
        self.report(CommandOutcome::Stopped);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::sync::Mutex;

    use super::*;
    use crate::git::fake::{FakeChild, FakeRunner, fail, ok};

    /// A sink that keeps every event, and what the inner runner had seen
    /// when each one arrived.
    fn collecting() -> (CommandSink, Arc<Mutex<Vec<CommandEvent>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = CommandSink::new();
        let keep = Arc::clone(&events);
        sink.connect(move |ev| keep.lock().unwrap().push(ev));
        (sink, events)
    }

    fn recording(fake: Arc<FakeRunner>) -> (RecordingRunner, Arc<Mutex<Vec<CommandEvent>>>) {
        let (sink, events) = collecting();
        (RecordingRunner::new(fake, sink), events)
    }

    fn started(id: u64, line: &str, attached: bool) -> CommandEvent {
        CommandEvent::Started {
            id,
            line: line.into(),
            attached,
        }
    }

    /// The events with each elapsed time zeroed, for comparison.
    fn timeless(events: &Mutex<Vec<CommandEvent>>) -> Vec<CommandEvent> {
        events
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .map(|ev| match ev {
                CommandEvent::Finished { id, outcome, .. } => CommandEvent::Finished {
                    id,
                    outcome,
                    elapsed: Duration::ZERO,
                },
                other => other,
            })
            .collect()
    }

    fn finished(id: u64, outcome: CommandOutcome) -> CommandEvent {
        CommandEvent::Finished {
            id,
            outcome,
            elapsed: Duration::ZERO,
        }
    }

    /// Fails every call, as a missing `git` binary would.
    struct Broken;

    impl GitRunner for Broken {
        fn run(&self, _: &GitCommand) -> io::Result<GitOutput> {
            Err(io::ErrorKind::NotFound.into())
        }
        fn spawn(&self, _: &GitCommand) -> io::Result<GitChild> {
            Err(io::ErrorKind::NotFound.into())
        }
    }

    #[test]
    fn a_run_is_announced_before_the_inner_runner_sees_it_and_finished_after() {
        let (sink, events) = collecting();
        let seen = Arc::clone(&events);
        let fake = Arc::new(
            FakeRunner::new()
                .expect(&["status"], ok(b""))
                .expect(&["commit", "-m", "x"], fail("boom")),
        );
        // The inner runner checks the log while it "runs".
        struct Peek(Arc<FakeRunner>, Arc<Mutex<Vec<CommandEvent>>>);
        impl GitRunner for Peek {
            fn run(&self, cmd: &GitCommand) -> io::Result<GitOutput> {
                let events = self.1.lock().unwrap();
                assert!(
                    matches!(events.last(), Some(CommandEvent::Started { .. })),
                    "{events:?}"
                );
                drop(events);
                self.0.run(cmd)
            }
            fn spawn(&self, cmd: &GitCommand) -> io::Result<GitChild> {
                self.0.spawn(cmd)
            }
        }
        let runner = RecordingRunner::new(Arc::new(Peek(Arc::clone(&fake), seen)), sink);

        assert!(
            runner
                .run(&GitCommand::new("status").standard_env())
                .unwrap()
                .success
        );
        let commit = GitCommand::new("commit").user_env().arg("-m").arg("x");
        assert!(!runner.run(&commit).unwrap().success);

        assert_eq!(
            timeless(&events),
            [
                started(0, "git status", false),
                finished(0, CommandOutcome::Ok),
                started(1, "git commit -m x", false),
                finished(1, CommandOutcome::Failed(None)),
            ]
        );
        fake.assert_done();
    }

    #[test]
    fn a_run_that_cannot_start_finishes_as_not_started() {
        let (sink, events) = collecting();
        let runner = RecordingRunner::new(Arc::new(Broken), sink);
        assert!(
            runner
                .run(&GitCommand::new("status").standard_env())
                .is_err()
        );
        assert!(
            runner
                .spawn(&GitCommand::new("log").standard_env())
                .is_err()
        );
        // The default `run_attached` is unsupported.
        assert!(
            runner
                .run_attached(&GitCommand::new("commit").user_env())
                .is_err()
        );
        assert_eq!(
            timeless(&events),
            [
                started(0, "git status", false),
                finished(0, CommandOutcome::NotStarted),
                started(1, "git log", false),
                finished(1, CommandOutcome::NotStarted),
                started(2, "git commit", true),
                finished(2, CommandOutcome::NotStarted),
            ]
        );
    }

    #[test]
    fn an_attached_run_carries_its_exit_status() {
        let fake = Arc::new(
            FakeRunner::new()
                .expect_attached(&["commit"], true)
                .expect_attached(&["rebase", "--continue"], false),
        );
        let (runner, events) = recording(Arc::clone(&fake));
        assert!(
            runner
                .run_attached(&GitCommand::new("commit").user_env())
                .unwrap()
                .success
        );
        let cont = GitCommand::new("rebase").user_env().arg("--continue");
        assert!(!runner.run_attached(&cont).unwrap().success);
        assert_eq!(
            timeless(&events),
            [
                started(0, "git commit", true),
                finished(0, CommandOutcome::Ok),
                started(1, "git rebase --continue", true),
                finished(1, CommandOutcome::Failed(Some("status 1".into()))),
            ]
        );
        fake.assert_done();
    }

    #[test]
    fn a_spawned_child_finishes_once_on_finish() {
        let fake = Arc::new(
            FakeRunner::new()
                .expect_spawn(&["log"], FakeChild::lines(2))
                .expect_spawn(&["diff"], FakeChild::lines(0).exits(fail("bad"))),
        );
        let (runner, events) = recording(Arc::clone(&fake));

        let child = runner
            .spawn(&GitCommand::new("log").standard_env())
            .unwrap();
        assert_eq!(timeless(&events), [started(0, "git log", false)]);
        let mut out = String::new();
        let mut stdout = child.stdout;
        stdout.read_to_string(&mut out).unwrap();
        assert_eq!(out, "+line 0\n+line 1\n");
        assert!(child.handle.finish().unwrap().success);
        // A second finish, and the drop, add nothing.
        child.handle.finish().unwrap();
        drop(child.handle);

        let child = runner
            .spawn(&GitCommand::new("diff").standard_env())
            .unwrap();
        assert!(!child.handle.finish().unwrap().success);
        drop(child);

        assert_eq!(
            timeless(&events),
            [
                started(0, "git log", false),
                finished(0, CommandOutcome::Ok),
                started(1, "git diff", false),
                finished(1, CommandOutcome::Failed(None)),
            ]
        );
        fake.assert_done();
    }

    #[test]
    fn a_spawned_child_dropped_without_finish_is_stopped_once() {
        let probe = FakeChild::hanging();
        let fake = Arc::new(FakeRunner::new().expect_spawn(&["log"], probe.clone()));
        let (runner, events) = recording(Arc::clone(&fake));

        let child = runner
            .spawn(&GitCommand::new("log").standard_env())
            .unwrap();
        let other = Arc::clone(&child.handle);
        child.handle.kill();
        assert!(probe.was_killed());
        drop(child);
        // Still held elsewhere, so not stopped yet.
        assert_eq!(events.lock().unwrap().len(), 1);
        drop(other);

        assert_eq!(
            timeless(&events),
            [
                started(0, "git log", false),
                finished(0, CommandOutcome::Stopped),
            ]
        );
    }

    #[test]
    fn interrupt_and_workdir_reach_the_inner_runner() {
        let fake = Arc::new(FakeRunner::new());
        let (runner, events) = recording(Arc::clone(&fake));
        runner.interrupt();
        assert_eq!(fake.interrupts(), 1);
        assert!(events.lock().unwrap().is_empty());

        let tmp = crate::git::fake::TempDir::new("recording-workdir");
        let cli = Arc::new(crate::git::CliRunner::isolated(tmp.path()));
        let runner = RecordingRunner::new(cli, CommandSink::new());
        assert_eq!(runner.workdir(), tmp.path());
        runner.set_workdir(tmp.path().join("sub"));
        assert_eq!(runner.workdir(), tmp.path().join("sub"));
    }

    #[test]
    fn runners_sharing_a_sink_never_share_an_id() {
        let (sink, events) = collecting();
        let one = RecordingRunner::new(
            Arc::new(FakeRunner::new().expect(&["status"], ok(b""))),
            sink.clone(),
        );
        let two = RecordingRunner::new(
            Arc::new(FakeRunner::new().expect(&["log"], ok(b""))),
            sink.clone(),
        );
        one.run(&GitCommand::new("status").standard_env()).unwrap();
        let other = sink.next_id();
        two.run(&GitCommand::new("log").standard_env()).unwrap();
        assert_eq!(
            timeless(&events),
            [
                started(0, "git status", false),
                finished(0, CommandOutcome::Ok),
                started(2, "git log", false),
                finished(2, CommandOutcome::Ok),
            ]
        );
        assert_eq!(other, 1);
    }

    #[test]
    fn events_before_connect_are_dropped_and_only_the_first_listener_counts() {
        let sink = CommandSink::new();
        sink.send(started(9, "git early", false));
        let events = Arc::new(Mutex::new(Vec::new()));
        let keep = Arc::clone(&events);
        sink.connect(move |ev| keep.lock().unwrap().push(ev));
        sink.connect(|_| panic!("a second listener is ignored"));
        // A clone shares the listener.
        sink.clone().send(started(1, "git status", false));
        assert_eq!(*events.lock().unwrap(), [started(1, "git status", false)]);
        assert!(format!("{sink:?}").contains("connected: true"));
    }

    #[test]
    fn a_real_git_status_is_recorded_with_a_non_zero_time() {
        let tmp = crate::git::fake::TempDir::new("recording-real");
        crate::git::fake::git(tmp.path(), &["init", "-q"]);
        crate::git::fake::git(tmp.path(), &["config", "user.name", "t"]);
        crate::git::fake::git(tmp.path(), &["config", "user.email", "t@t"]);
        let (sink, events) = collecting();
        let runner =
            RecordingRunner::new(Arc::new(crate::git::CliRunner::isolated(tmp.path())), sink);

        assert!(
            runner
                .run(&GitCommand::new("status").standard_env())
                .unwrap()
                .success
        );

        let events = events.lock().unwrap();
        assert_eq!(events[0], started(0, "git status", false));
        match &events[1..] {
            [
                CommandEvent::Finished {
                    id: 0,
                    outcome: CommandOutcome::Ok,
                    elapsed,
                },
            ] => assert!(*elapsed > Duration::ZERO),
            other => panic!("{other:?}"),
        }
    }
}
