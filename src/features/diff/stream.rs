//! Streams one diff at a time into the app. Each request kills the previous
//! `git diff`, and the reader only reads as far as the pane has asked for:
//! while it waits, git blocks on the full pipe, which is what keeps a huge
//! diff cheap.

use std::io::{BufRead, BufReader, ErrorKind, Read};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::mpsc::UnboundedSender;

use crate::background;
use crate::effect::Tag;
use crate::git::GitRunner;
use crate::git::diff::{Classifier, DiffChunk, DiffRequest, EmojiZwj, diff_command};
use crate::git::error::GitError;
use crate::git::runner::{ChildHandle, GitCommand};
use crate::intent::AppIntent;

/// Owns the single diff stream for the Diff pane.
pub struct DiffStreamer {
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    current: Mutex<Option<Active>>,
    /// How body lines draw emoji ZWJ sequences, fixed for the session.
    emoji: EmojiZwj,
}

/// The running stream. Dropping `demand` tells its reader it was superseded.
struct Active {
    seq: u64,
    slot: Arc<Slot>,
    demand: Sender<usize>,
}

/// Shared between the canceller and the reader, so a cancel that arrives
/// before the process exists still kills it.
type Slot = Mutex<SlotState>;

#[derive(Default)]
struct SlotState {
    cancelled: bool,
    child: Option<Arc<dyn ChildHandle>>,
}

impl Active {
    /// Stop the stream. Runs on the UI loop and never blocks on git.
    fn cancel(self) {
        drop(self.demand);
        let mut slot = lock(&self.slot);
        slot.cancelled = true;
        if let Some(child) = &slot.child {
            child.kill();
        }
    }
}

impl DiffStreamer {
    pub fn new(runner: Arc<dyn GitRunner>, tx: UnboundedSender<AppIntent>) -> Self {
        Self {
            runner,
            tx,
            current: Mutex::new(None),
            emoji: EmojiZwj::Escape,
        }
    }

    /// Draw emoji ZWJ sequences in every later stream as `emoji` says.
    pub fn set_emoji_zwj(&mut self, emoji: EmojiZwj) {
        self.emoji = emoji;
    }

    /// Replace the current stream with one for `request`, reading `want`
    /// lines before the first pause.
    pub fn start(&self, tag: Tag, request: DiffRequest, want: usize) {
        let (demand, demand_rx) = mpsc::channel();
        let slot = Arc::new(Slot::default());
        let active = Active {
            seq: tag.seq,
            slot: Arc::clone(&slot),
            demand,
        };
        if let Some(previous) = lock(&self.current).replace(active) {
            previous.cancel();
        }
        let runner = Arc::clone(&self.runner);
        let tx = self.tx.clone();
        let emoji = self.emoji;
        let reader_tx = tx.clone();
        background::run_with(
            tx,
            move || {
                stream(
                    runner.as_ref(),
                    &slot,
                    &request,
                    demand_rx,
                    want,
                    emoji,
                    |result| {
                        // A closed channel means the app is quitting.
                        let _ = reader_tx.send(AppIntent::DiffLoaded { tag, result });
                    },
                );
            },
            // A crash still reports back, or the pane would say "Loading…" forever.
            move |reader| {
                reader.err().map(|e| AppIntent::DiffLoaded {
                    tag,
                    result: Err(format!("git diff crashed: {e}")),
                })
            },
        );
    }

    /// Let stream `seq` read on to `want` lines. Ignored if it was superseded.
    pub fn demand(&self, seq: u64, want: usize) {
        if let Some(active) = lock(&self.current).as_ref()
            && active.seq == seq
        {
            let _ = active.demand.send(want);
        }
    }

    /// Stop the current stream, e.g. when there is no file left to show.
    pub fn cancel(&self) {
        if let Some(active) = lock(&self.current).take() {
            active.cancel();
        }
    }
}

impl Drop for DiffStreamer {
    /// On quit, kill a `git diff` still running, or a reader blocked on it
    /// would keep tokio's shutdown waiting.
    fn drop(&mut self) {
        self.cancel();
    }
}

/// The reader's side: spawn, register the child with the slot, then pump.
fn stream(
    runner: &dyn GitRunner,
    slot: &Slot,
    request: &DiffRequest,
    demand: Receiver<usize>,
    want: usize,
    emoji: EmojiZwj,
    mut emit: impl FnMut(Result<DiffChunk, String>),
) {
    let cmd = diff_command(request);
    let child = match runner.spawn(&cmd) {
        Ok(child) => child,
        Err(e) => return emit(Err(format!("git diff: {}", GitError::io(&e)))),
    };
    {
        let mut slot = lock(slot);
        if slot.cancelled {
            child.handle.kill();
            return;
        }
        slot.child = Some(Arc::clone(&child.handle));
    }
    pump(
        &cmd,
        child.stdout,
        child.handle.as_ref(),
        demand,
        want,
        emoji,
        emit,
    );
}

/// Read and classify lines, emitting a chunk each time `want` is reached and
/// then waiting for a larger `want`. A closed `demand` channel means the
/// stream was superseded: the child is killed and nothing more is emitted.
/// `cmd` is what the child runs, for its error.
pub fn pump(
    cmd: &GitCommand,
    stdout: impl Read,
    child: &dyn ChildHandle,
    demand: Receiver<usize>,
    mut want: usize,
    emoji: EmojiZwj,
    mut emit: impl FnMut(Result<DiffChunk, String>),
) {
    let mut reader = BufReader::new(stdout);
    let mut classifier = Classifier::new(emoji);
    let mut batch = Vec::new();
    let mut count = 0;
    let mut buf = Vec::new();
    loop {
        while count >= want {
            emit(Ok(DiffChunk {
                lines: std::mem::take(&mut batch),
                done: false,
            }));
            // Wait for a demand past what we hold; a smaller one asks for nothing new.
            while count >= want {
                let Ok(next) = demand.recv() else {
                    child.kill();
                    return;
                };
                want = demand.try_iter().fold(want.max(next), usize::max);
            }
        }
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(_) => {
                batch.push(classifier.classify(&buf));
                count += 1;
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => {
                child.kill();
                return emit(Err(format!("git diff: {}", GitError::io(&e))));
            }
        }
    }
    // EOF from a kill is not a real end; the stream was superseded.
    if matches!(demand.try_recv(), Err(TryRecvError::Disconnected)) {
        return;
    }
    match child.finish() {
        Ok(output) if !output.success && !output.stderr.is_empty() => {
            emit(Err(GitError::failed(cmd, &output).to_string()))
        }
        // `--no-index` exits 1 when the files differ, so a quiet failure is fine.
        Ok(_) => emit(Ok(DiffChunk {
            lines: batch,
            done: true,
        })),
        Err(e) => emit(Err(format!("git diff: {}", GitError::io(&e)))),
    }
}

/// A poisoned lock only means another thread panicked mid-cancel; the state is still usable.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use tokio::sync::mpsc as tokio_mpsc;

    use super::*;
    use crate::git::diff::{DiffSide, LineKind};
    use crate::git::fake::{FakeChild, FakeRunner, fail};

    /// Run `pump` on a thread over a spawned fake, collecting what it emits.
    struct Pumped {
        child: FakeChild,
        demand: Sender<usize>,
        chunks: Receiver<Result<DiffChunk, String>>,
        thread: thread::JoinHandle<()>,
    }

    fn pump_over(child: FakeChild, want: usize) -> Pumped {
        let runner = FakeRunner::new().expect_spawn(&["x"], child.clone());
        let cmd = GitCommand::new("x").user_env();
        let spawned = runner.spawn(&cmd).unwrap();
        let (demand, demand_rx) = mpsc::channel();
        let (out, chunks) = mpsc::channel();
        let thread = thread::spawn(move || {
            pump(
                &cmd,
                spawned.stdout,
                spawned.handle.as_ref(),
                demand_rx,
                want,
                EmojiZwj::Escape,
                |chunk| out.send(chunk).unwrap(),
            )
        });
        Pumped {
            child,
            demand,
            chunks,
            thread,
        }
    }

    fn next_chunk(p: &Pumped) -> DiffChunk {
        p.chunks
            .recv_timeout(Duration::from_secs(5))
            .expect("a chunk")
            .expect("not an error")
    }

    #[test]
    fn a_large_diff_is_read_only_as_far_as_wanted() {
        let p = pump_over(FakeChild::lines(50_000), 250);

        let first = next_chunk(&p);
        assert_eq!(first.lines.len(), 250);
        assert!(!first.done);
        assert_eq!(first.lines[0].text, "+line 0");
        // Give a runaway reader time to show itself.
        thread::sleep(Duration::from_millis(20));
        assert!(p.child.reads() < 1_000, "read {}", p.child.reads());

        p.demand.send(500).unwrap();
        let second = next_chunk(&p);
        assert_eq!(second.lines.len(), 250);
        assert!(!second.done);
        assert_eq!(second.lines[0].text, "+line 250");
        assert!(p.child.reads() < 1_000, "read {}", p.child.reads());
    }

    #[test]
    fn dropping_the_demand_sender_ends_the_loop_and_kills_the_child() {
        let p = pump_over(FakeChild::lines(50_000), 10);
        next_chunk(&p);

        drop(p.demand);
        p.thread.join().unwrap();

        assert!(p.child.was_killed());
        assert!(p.chunks.try_recv().is_err(), "nothing after superseding");
    }

    #[test]
    fn queued_demands_are_merged_into_the_largest() {
        let p = pump_over(FakeChild::lines(1_000), 10);
        next_chunk(&p);
        p.demand.send(20).unwrap();
        p.demand.send(40).unwrap();
        // The reader may wake for the first send alone; either way it ends at 40.
        let mut total = 10;
        while total < 40 {
            total += next_chunk(&p).lines.len();
        }
        assert_eq!(total, 40);
    }

    #[test]
    fn a_short_diff_arrives_as_one_done_chunk() {
        let p = pump_over(FakeChild::lines(3), 250);
        let chunk = next_chunk(&p);
        assert!(chunk.done);
        assert_eq!(chunk.lines.len(), 3);
        assert_eq!(chunk.lines[0].kind, LineKind::Header);
        p.thread.join().unwrap();
        assert!(!p.child.was_killed());
    }

    #[test]
    fn hitting_want_exactly_at_eof_still_ends_with_done() {
        let p = pump_over(FakeChild::lines(5), 5);
        assert_eq!(next_chunk(&p).lines.len(), 5);
        p.demand.send(10).unwrap();
        let last = next_chunk(&p);
        assert!(last.done);
        assert!(last.lines.is_empty());
    }

    #[test]
    fn failure_with_stderr_is_an_error() {
        let child = FakeChild::lines(0).exits(fail("fatal: bad revision\n"));
        let p = pump_over(child, 250);
        let result = p.chunks.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(result, Err("fatal: bad revision".to_string()));
    }

    #[test]
    fn stderr_is_made_terminal_safe_but_keeps_its_lines() {
        let child = FakeChild::lines(0).exits(fail("fatal: \x1b[2Jbad\tpath\r\nhint: retry\n"));
        let p = pump_over(child, 250);
        let result = p.chunks.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            result,
            Err("fatal: \u{FFFD}[2Jbad    path\nhint: retry".to_string())
        );
    }

    #[test]
    fn a_demand_that_does_not_grow_emits_nothing() {
        let p = pump_over(FakeChild::lines(1_000), 10);
        next_chunk(&p);
        p.demand.send(10).unwrap();
        p.demand.send(5).unwrap();
        thread::sleep(Duration::from_millis(20));
        assert!(p.chunks.try_recv().is_err(), "no empty chunk");

        p.demand.send(15).unwrap();
        assert_eq!(next_chunk(&p).lines.len(), 5);
    }

    #[test]
    fn a_quiet_failure_like_no_index_exit_1_is_still_done() {
        let child = FakeChild::lines(2).exits(fail(""));
        let p = pump_over(child, 250);
        let chunk = next_chunk(&p);
        assert!(chunk.done);
        assert_eq!(chunk.lines.len(), 2);
    }

    #[test]
    fn a_cancel_before_the_spawn_kills_the_child_once_it_exists() {
        let child = FakeChild::hanging();
        let runner = FakeRunner::new().expect_spawn(UNTRACKED_ARGV, child.clone());
        let slot = Slot::default();
        lock(&slot).cancelled = true;
        let (_demand, demand_rx) = mpsc::channel();
        let mut emitted = 0;

        stream(
            &runner,
            &slot,
            &untracked("a"),
            demand_rx,
            250,
            EmojiZwj::Escape,
            |_| emitted += 1,
        );

        assert!(child.was_killed());
        assert_eq!(emitted, 0);
    }

    /// A diff that cannot start keeps its `git diff:` prefix, and the
    /// reason is drawn safely with no credentials.
    #[test]
    fn a_diff_that_cannot_start_says_why_safely() {
        use crate::git::runner::{GitChild, GitOutput};
        struct NoGit;
        impl GitRunner for NoGit {
            fn run(&self, _: &GitCommand) -> std::io::Result<GitOutput> {
                unreachable!("only spawned")
            }
            fn spawn(&self, _: &GitCommand) -> std::io::Result<GitChild> {
                Err(std::io::Error::other("no git at https://u:tok@h/x\x1b[2J"))
            }
        }
        let (_demand, demand_rx) = mpsc::channel();
        let mut emitted = Vec::new();

        stream(
            &NoGit,
            &Slot::default(),
            &untracked("a"),
            demand_rx,
            250,
            EmojiZwj::Escape,
            |chunk| emitted.push(chunk),
        );

        assert_eq!(
            emitted,
            [Err("git diff: no git at https://h/x\u{FFFD}[2J".to_string())]
        );
    }

    const UNTRACKED_ARGV: &[&str] = &[
        "diff",
        "--no-color",
        "--no-ext-diff",
        "-U3",
        "--no-index",
        "--",
        "/dev/null",
        "a",
    ];

    fn untracked(path: &str) -> DiffRequest {
        DiffRequest {
            path: path.into(),
            orig_path: None,
            side: DiffSide::Untracked,
            options: Default::default(),
            commit: None,
        }
    }

    fn tag(seq: u64) -> Tag {
        Tag {
            generation: 0,
            seq,
            user: false,
        }
    }

    async fn until(what: &str, mut condition: impl FnMut() -> bool) {
        for _ in 0..500 {
            if condition() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting until {what}");
    }

    fn worktree_argv(path: &str) -> Vec<&str> {
        vec!["diff", "--no-color", "--no-ext-diff", "-U3", "--", path]
    }

    fn worktree(path: &str) -> DiffRequest {
        DiffRequest {
            path: path.into(),
            orig_path: None,
            side: DiffSide::Worktree,
            options: Default::default(),
            commit: None,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_new_request_kills_the_previous_child_only() {
        let first = FakeChild::hanging();
        let second = FakeChild::lines(3);
        let runner = Arc::new(
            FakeRunner::new()
                .expect_spawn(&worktree_argv("a"), first.clone())
                .expect_spawn(&worktree_argv("b"), second.clone()),
        );
        let (tx, mut rx) = tokio_mpsc::unbounded_channel();
        let streamer = DiffStreamer::new(runner.clone(), tx);

        streamer.start(tag(1), worktree("a"), 250);
        until("the first child is spawned", || first.was_spawned()).await;
        assert!(!first.was_killed());

        streamer.start(tag(2), worktree("b"), 250);
        until("the first child is killed", || first.was_killed()).await;

        match rx.recv().await {
            Some(AppIntent::DiffLoaded {
                tag: got,
                result: Ok(chunk),
            }) => {
                assert_eq!(got, tag(2), "the killed stream emits nothing");
                assert!(chunk.done);
                assert_eq!(chunk.lines.len(), 3);
            }
            other => panic!("expected the second diff, got {other:?}"),
        }
        assert!(!second.was_killed());
        runner.assert_done();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_kills_the_child_whether_or_not_it_was_spawned_yet() {
        let child = FakeChild::hanging();
        let runner = Arc::new(FakeRunner::new().expect_spawn(UNTRACKED_ARGV, child.clone()));
        let (tx, _rx) = tokio_mpsc::unbounded_channel();
        let streamer = DiffStreamer::new(runner, tx);

        streamer.start(tag(1), untracked("a"), 250);
        streamer.cancel();

        until("the child is killed", || child.was_killed()).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn demand_reaches_only_the_current_stream() {
        let child = FakeChild::lines(1_000);
        let runner = Arc::new(FakeRunner::new().expect_spawn(UNTRACKED_ARGV, child.clone()));
        let (tx, mut rx) = tokio_mpsc::unbounded_channel();
        let streamer = DiffStreamer::new(runner, tx);
        streamer.start(tag(7), untracked("a"), 10);

        let lines = |intent| match intent {
            Some(AppIntent::DiffLoaded {
                result: Ok(chunk), ..
            }) => chunk.lines.len(),
            other => panic!("expected a chunk, got {other:?}"),
        };
        assert_eq!(lines(rx.recv().await), 10);

        streamer.demand(6, 500);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(rx.try_recv().is_err(), "a stale seq is ignored");

        streamer.demand(7, 30);
        assert_eq!(lines(rx.recv().await), 20);
        streamer.cancel();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dropping_the_streamer_kills_a_running_diff() {
        let child = FakeChild::hanging();
        let runner = Arc::new(FakeRunner::new().expect_spawn(UNTRACKED_ARGV, child.clone()));
        let (tx, _rx) = tokio_mpsc::unbounded_channel();
        let streamer = DiffStreamer::new(runner, tx);
        streamer.start(tag(1), untracked("a"), 250);
        until("the child is spawned", || child.was_spawned()).await;

        drop(streamer);

        until("the child is killed", || child.was_killed()).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_crashed_reader_still_reports_back() {
        // No spawn queued, so the fake panics inside the reader.
        let (tx, mut rx) = tokio_mpsc::unbounded_channel();
        let streamer = DiffStreamer::new(Arc::new(FakeRunner::new()), tx);

        streamer.start(tag(3), untracked("a"), 250);

        match rx.recv().await {
            Some(AppIntent::DiffLoaded {
                tag: got,
                result: Err(message),
            }) => {
                assert_eq!(got, tag(3));
                assert!(message.contains("git diff crashed"), "{message}");
            }
            other => panic!("expected a failed DiffLoaded, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_commit_files_diff_streams_only_that_files_change_in_that_commit() {
        use crate::git::CliRunner;
        use crate::git::commit_files::{CommitFile, CommitId, CommitRange};
        use crate::git::fake::{TempDir, git};

        let tmp = TempDir::new("commit-file-diff");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(dir.join("b.txt"), "bee\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "first"]);
        std::fs::write(dir.join("a.txt"), "one\nTWO\n").unwrap();
        std::fs::write(dir.join("b.txt"), "BEE\n").unwrap();
        git(dir, &["commit", "-q", "-am", "second"]);
        // The working tree moves on; the commit's diff must not.
        std::fs::write(dir.join("a.txt"), "later\n").unwrap();
        let rev = |name: &str| {
            let out = String::from_utf8(git(dir, &["rev-parse", name])).unwrap();
            CommitId::parse(out.trim()).unwrap()
        };
        let range = CommitRange {
            oid: rev("HEAD"),
            parent: Some(rev("HEAD^")),
        };
        let file = CommitFile {
            status: 'M',
            path: "a.txt".into(),
            orig_path: None,
        };
        let request = DiffRequest::for_commit_file(&range, &file, Default::default());
        let (tx, mut rx) = tokio_mpsc::unbounded_channel();
        let streamer = DiffStreamer::new(Arc::new(CliRunner::isolated(dir)), tx);

        streamer.start(tag(4), request, 250);

        let Some(AppIntent::DiffLoaded {
            result: Ok(chunk), ..
        }) = rx.recv().await
        else {
            panic!("expected a chunk");
        };
        assert!(chunk.done);
        let text: Vec<(LineKind, &str)> = chunk
            .lines
            .iter()
            .map(|l| (l.kind, l.text.as_str()))
            .collect();
        assert!(text.contains(&(LineKind::Removed, "-two")), "{text:?}");
        assert!(text.contains(&(LineKind::Added, "+TWO")), "{text:?}");
        assert!(
            !text
                .iter()
                .any(|(_, t)| t.contains("b.txt") || t.contains("later"))
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_streamer_set_to_keep_draws_emoji_joiners_in_body_lines() {
        use crate::git::CliRunner;
        use crate::git::fake::{TempDir, git};

        let family = "👨\u{200D}👩\u{200D}👧";
        let tmp = TempDir::new("emoji-diff");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        std::fs::write(dir.join("a"), format!("{family}\n")).unwrap();
        let (tx, mut rx) = tokio_mpsc::unbounded_channel();
        let mut streamer = DiffStreamer::new(Arc::new(CliRunner::isolated(dir)), tx);
        streamer.set_emoji_zwj(EmojiZwj::Keep);

        streamer.start(tag(1), untracked("a"), 250);

        let Some(AppIntent::DiffLoaded {
            result: Ok(chunk), ..
        }) = rx.recv().await
        else {
            panic!("expected a chunk");
        };
        let added: Vec<&str> = chunk
            .lines
            .iter()
            .filter(|l| l.kind == LineKind::Added)
            .map(|l| l.text.as_str())
            .collect();
        assert_eq!(added, [format!("+{family}")]);
    }
}
