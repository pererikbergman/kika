use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;

use crate::diff_stream::DiffStreamer;
use crate::git::GitRunner;
use crate::git::diff::{DiffRequest, load_patch, terminal_safe};
use crate::git::ops::{GitOp, run_op};
use crate::git::status::read_status;
use crate::handoff::HandOff;
use crate::intent::AppIntent;

/// Identifies an effect's result so the reducer can drop stale ones.
/// `generation` changes when the repository changes; `seq` orders loads of the
/// same kind; `user` marks effects that count toward the busy indicator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tag {
    pub generation: u64,
    pub seq: u64,
    pub user: bool,
}

/// Work the reducer asks for. Executed by [`Runtime`], never by `update`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    LoadStatus {
        tag: Tag,
    },
    /// Stream the diff for `request`, reading `want` lines before pausing.
    /// Supersedes any diff still streaming.
    LoadDiff {
        tag: Tag,
        request: DiffRequest,
        want: usize,
    },
    /// Let the stream for `seq` read on until it has `want` lines.
    DiffDemand {
        seq: u64,
        want: usize,
    },
    /// Stop the current diff stream and its `git` process.
    CancelDiff,
    /// Report `DiffSlow` after [`DIFF_LOADING_DELAY`], so fast loads never flash "Loading…".
    DiffLoadingDelay {
        tag: Tag,
    },
    /// Stop git commands still running, because Kika is quitting. Answered
    /// by nothing; the stopped operation reports as failed, if anyone listens.
    InterruptGit,
    /// Load the whole diff of one file as a patch; always answered by one
    /// `PatchLoaded`.
    LoadPatch {
        tag: Tag,
        request: DiffRequest,
    },
    /// Run a Git operation; always answered by one `OpFinished`.
    RunOp {
        tag: Tag,
        op: GitOp,
    },
    /// Hand the terminal to another process. Run by the loop, which owns
    /// the terminal, never by [`Runtime`].
    HandOff(HandOff),
}

/// How long a diff may take before "Loading…" is shown.
pub const DIFF_LOADING_DELAY: Duration = Duration::from_millis(150);

/// Executes effects off the UI loop and reports each result as an intent.
/// Makes no decisions: staleness and bookkeeping belong to the reducer.
pub struct Runtime {
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    diff: DiffStreamer,
}

impl Runtime {
    pub fn new(runner: Arc<dyn GitRunner>, tx: UnboundedSender<AppIntent>) -> Self {
        Self {
            diff: DiffStreamer::new(Arc::clone(&runner), tx.clone()),
            runner,
            tx,
        }
    }

    /// Start an effect on tokio's blocking pool and return at once. The
    /// result arrives later on the channel.
    pub fn spawn(&self, effect: Effect) {
        let runner = Arc::clone(&self.runner);
        let tx = self.tx.clone();
        match effect {
            Effect::LoadStatus { tag } => {
                tokio::spawn(async move {
                    // A panic still reports back, or the busy counter would stay raised.
                    let result = tokio::task::spawn_blocking(move || read_status(runner.as_ref()))
                        .await
                        .unwrap_or_else(|e| Err(format!("git status crashed: {e}")));
                    // A closed channel means the app is quitting; nothing to report to.
                    let _ = tx.send(AppIntent::StatusLoaded { tag, result });
                });
            }
            Effect::LoadPatch { tag, request } => {
                tokio::spawn(async move {
                    // A panic still reports back, or the busy counter would stay raised.
                    let result =
                        tokio::task::spawn_blocking(move || load_patch(runner.as_ref(), &request))
                            .await
                            .unwrap_or_else(|e| {
                                Err(terminal_safe(format!("git diff crashed: {e}").as_bytes()))
                            });
                    let _ = tx.send(AppIntent::PatchLoaded { tag, result });
                });
            }
            Effect::LoadDiff { tag, request, want } => self.diff.start(tag, request, want),
            Effect::DiffDemand { seq, want } => self.diff.demand(seq, want),
            Effect::CancelDiff => self.diff.cancel(),
            Effect::DiffLoadingDelay { tag } => {
                tokio::spawn(async move {
                    tokio::time::sleep(DIFF_LOADING_DELAY).await;
                    let _ = tx.send(AppIntent::DiffSlow { tag });
                });
            }
            // Synchronous: the loop exits right after, and the runtime's
            // shutdown must not wait on git.
            Effect::InterruptGit => self.runner.interrupt(),
            Effect::RunOp { tag, op } => {
                tokio::spawn(async move {
                    // A panic still reports back, or `op_seq` would block every later op.
                    let task_op = op.clone();
                    let result =
                        tokio::task::spawn_blocking(move || run_op(runner.as_ref(), &task_op))
                            .await
                            // The panic text may quote a path; it is drawn in the status bar.
                            .unwrap_or_else(|e| {
                                Err(terminal_safe(format!("git crashed: {e}").as_bytes()))
                            });
                    let _ = tx.send(AppIntent::OpFinished { tag, op, result });
                });
            }
            // The loop takes these out before spawning; without a terminal
            // there is nothing to hand over, so one that slips through is dropped.
            Effect::HandOff(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use tokio::sync::mpsc;

    use super::*;
    use crate::git::FileEntry;
    use crate::git::diff::DiffSide;
    use crate::git::fake::{FakeChild, FakeRunner, ok};
    use crate::git::runner::{GitChild, GitCommand, GitOutput};
    use crate::git::status::{BranchStatus, EntryKind, Head, RepoStatus};

    #[tokio::test]
    async fn load_status_reports_back_with_the_same_tag() {
        let runner = Arc::new(
            FakeRunner::new()
                .expect(
                    &[
                        "status",
                        "--porcelain=v2",
                        "--branch",
                        "-z",
                        "--untracked-files=all",
                    ],
                    ok(b"# branch.oid 1234567\0# branch.head main\0? new_file\0"),
                )
                .expect(
                    &["rev-parse", "--absolute-git-dir"],
                    ok(b"/no/such/kika/repo/.git\n"),
                ),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 2,
            seq: 5,
            user: true,
        };

        runtime.spawn(Effect::LoadStatus { tag });

        assert_eq!(
            rx.recv().await,
            Some(AppIntent::StatusLoaded {
                tag,
                result: Ok(RepoStatus {
                    branch: BranchStatus {
                        head: Head::Branch("main".into()),
                        upstream: None,
                        ahead_behind: None,
                        in_progress: vec![],
                    },
                    files: vec![FileEntry {
                        path: "new_file".into(),
                        orig_path: None,
                        index: '?',
                        worktree: '?',
                        kind: EntryKind::Untracked,
                    }],
                }),
            })
        );
        runner.assert_done();
    }

    #[tokio::test]
    async fn a_panicking_effect_still_reports_back_so_busy_can_fall() {
        // No expectations queued, so the fake panics on the call.
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(FakeRunner::new()), tx);
        let tag = Tag {
            generation: 0,
            seq: 1,
            user: true,
        };

        runtime.spawn(Effect::LoadStatus { tag });

        match rx.recv().await {
            Some(AppIntent::StatusLoaded {
                tag: got,
                result: Err(message),
            }) => {
                assert_eq!(got, tag);
                assert!(message.contains("git status"), "{message}");
            }
            other => panic!("expected a failed StatusLoaded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_op_reports_back_with_the_same_tag_and_op() {
        let runner = Arc::new(FakeRunner::new().expect(&["add", "-A"], ok(b"")));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 3,
            seq: 8,
            user: true,
        };

        runtime.spawn(Effect::RunOp {
            tag,
            op: GitOp::StageAll,
        });

        assert_eq!(
            rx.recv().await,
            Some(AppIntent::OpFinished {
                tag,
                op: GitOp::StageAll,
                result: Ok(()),
            })
        );
        runner.assert_done();
    }

    #[tokio::test]
    async fn a_panicking_op_still_reports_back() {
        // No expectations queued, so the fake panics on the call.
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(FakeRunner::new()), tx);
        let tag = Tag {
            generation: 0,
            seq: 2,
            user: true,
        };

        runtime.spawn(Effect::RunOp {
            tag,
            op: GitOp::UnstageAll,
        });

        match rx.recv().await {
            Some(AppIntent::OpFinished {
                tag: got,
                op: GitOp::UnstageAll,
                result: Err(message),
            }) => {
                assert_eq!(got, tag);
                assert!(message.starts_with("git crashed: "), "{message}");
            }
            other => panic!("expected a failed OpFinished, got {other:?}"),
        }
    }

    /// Panics with terminal control bytes, like a panic that quotes a path.
    struct HostilePanic;

    impl GitRunner for HostilePanic {
        fn run(&self, _: &GitCommand) -> io::Result<GitOutput> {
            panic!("bad \x1b[2J name");
        }

        fn spawn(&self, _: &GitCommand) -> io::Result<GitChild> {
            panic!("bad \x1b[2J name");
        }
    }

    #[tokio::test]
    async fn the_panic_text_of_an_op_carries_no_control_characters() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(HostilePanic), tx);
        let tag = Tag {
            generation: 0,
            seq: 3,
            user: true,
        };

        runtime.spawn(Effect::RunOp {
            tag,
            op: GitOp::StageAll,
        });

        match rx.recv().await {
            Some(AppIntent::OpFinished {
                result: Err(message),
                ..
            }) => {
                assert!(message.starts_with("git crashed: "), "{message:?}");
                assert!(message.contains("[2J name"), "{message:?}");
                assert!(!message.contains(char::is_control), "{message:?}");
            }
            other => panic!("expected a failed OpFinished, got {other:?}"),
        }
    }

    fn patch_request() -> DiffRequest {
        DiffRequest {
            path: "a.rs".into(),
            orig_path: None,
            side: DiffSide::Index,
        }
    }

    #[tokio::test]
    async fn load_patch_reports_back_with_the_same_tag() {
        let runner = Arc::new(FakeRunner::new().expect(
            &[
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                "--no-renames",
                "-U3",
                "--src-prefix=a/",
                "--dst-prefix=b/",
                "--cached",
                "--",
                "a.rs",
            ],
            ok(b"diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-a\n+b\n"),
        ));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 1,
            seq: 6,
            user: true,
        };

        runtime.spawn(Effect::LoadPatch {
            tag,
            request: patch_request(),
        });

        match rx.recv().await {
            Some(AppIntent::PatchLoaded {
                tag: got,
                result: Ok(patch),
            }) => {
                assert_eq!(got, tag);
                assert_eq!(patch.hunks.len(), 1);
            }
            other => panic!("expected a loaded patch, got {other:?}"),
        }
        runner.assert_done();
    }

    #[tokio::test]
    async fn a_panicking_patch_load_still_reports_back() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(HostilePanic), tx);
        let tag = Tag {
            generation: 0,
            seq: 7,
            user: true,
        };

        runtime.spawn(Effect::LoadPatch {
            tag,
            request: patch_request(),
        });

        match rx.recv().await {
            Some(AppIntent::PatchLoaded {
                tag: got,
                result: Err(message),
            }) => {
                assert_eq!(got, tag);
                assert!(message.starts_with("git diff crashed: "), "{message:?}");
                assert!(!message.contains(char::is_control), "{message:?}");
            }
            other => panic!("expected a failed PatchLoaded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_hand_off_is_never_run_by_the_runtime() {
        // No expectations queued, so any git call would panic.
        let runner = Arc::new(FakeRunner::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);

        runtime.spawn(Effect::HandOff(crate::handoff::HandOff::OpenFile {
            path: "a.rs".into(),
            line: None,
        }));

        drop(runtime);
        assert_eq!(rx.recv().await, None);
    }

    #[tokio::test]
    async fn interrupt_git_stops_the_runner_before_returning() {
        let runner = Arc::new(FakeRunner::new());
        let (tx, _rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);

        runtime.spawn(Effect::InterruptGit);

        assert_eq!(runner.interrupts(), 1);
    }

    #[tokio::test]
    async fn loading_delay_reports_slow_after_the_delay() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(FakeRunner::new()), tx);
        let tag = Tag {
            generation: 1,
            seq: 4,
            user: false,
        };
        let start = std::time::Instant::now();

        runtime.spawn(Effect::DiffLoadingDelay { tag });

        assert_eq!(rx.recv().await, Some(AppIntent::DiffSlow { tag }));
        assert!(start.elapsed() >= DIFF_LOADING_DELAY);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn load_diff_streams_through_the_diff_streamer() {
        let runner = Arc::new(FakeRunner::new().expect_spawn(
            &["diff", "--no-color", "--no-ext-diff", "--", "a.rs"],
            FakeChild::lines(2),
        ));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 0,
            seq: 9,
            user: false,
        };

        runtime.spawn(Effect::LoadDiff {
            tag,
            request: DiffRequest {
                path: "a.rs".into(),
                orig_path: None,
                side: DiffSide::Worktree,
            },
            want: 100,
        });

        match rx.recv().await {
            Some(AppIntent::DiffLoaded {
                tag: got,
                result: Ok(chunk),
            }) => {
                assert_eq!(got, tag);
                assert_eq!(chunk.lines.len(), 2);
                assert!(chunk.done);
            }
            other => panic!("expected a diff chunk, got {other:?}"),
        }
        runner.assert_done();
    }
}
