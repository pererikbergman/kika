use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;

use crate::config::{self, ConfigPaths};
use crate::diff_stream::DiffStreamer;
use crate::git::GitRunner;
use crate::git::blame::{read_blame, read_commit};
use crate::git::diff::{DiffRequest, load_patch, terminal_safe};
use crate::git::fetch::run_fetch;
use crate::git::head::read_head_message;
use crate::git::ops::{GitOp, run_op};
use crate::git::path::RepoPath;
use crate::git::refs::read_refs;
use crate::git::repo_root::init_repo;
use crate::git::status::read_status;
use crate::git::submodule::read_submodules;
use crate::handoff::HandOff;
use crate::intent::AppIntent;
use crate::recent;

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
    /// Read `HEAD`'s message to amend it; always answered by one
    /// `HeadMessageLoaded`.
    LoadHeadMessage {
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
    /// List the submodules; always answered by one `SubmodulesLoaded`.
    LoadSubmodules {
        tag: Tag,
    },
    /// Point git at the repository `nested` names: each path is a
    /// submodule of the one before, starting from the top-level `root`,
    /// which an empty list means. Runs before the next effect starts.
    SetRepo {
        root: PathBuf,
        nested: Vec<RepoPath>,
    },
    /// Read the recent-repositories list; answered by `RecentLoaded`
    /// with the same `seq`.
    LoadRecent {
        seq: u64,
    },
    /// Put `path` first in the recent list; answered by `RecentLoaded`
    /// with the new list and the same `seq`.
    RecordRecent {
        path: PathBuf,
        seq: u64,
    },
    /// Make the runner's directory a repository; always answered by one
    /// `RepoInitialised`.
    InitRepo {
        tag: Tag,
    },
    /// Blame `path` in the worktree; always answered by one `BlameLoaded`.
    LoadBlame {
        tag: Tag,
        path: RepoPath,
    },
    /// Read commit `oid`'s message and diff; always answered by one
    /// `CommitLoaded`.
    LoadCommit {
        tag: Tag,
        oid: String,
    },
    /// Hand the terminal to another process. Run by the loop, which owns
    /// the terminal, never by [`Runtime`].
    HandOff(HandOff),
    /// Read the refs snapshot; always answered by one `RefsLoaded`.
    SnapshotRefs {
        tag: Tag,
    },
    /// Fetch every remote; always answered by one `FetchFinished`.
    Fetch {
        tag: Tag,
    },
    /// Read the global config file and that of the repository whose top
    /// level is `repo`; answered by one `ConfigLoaded` for the same `repo`.
    LoadConfig {
        repo: Option<PathBuf>,
    },
}

/// How long a diff may take before "Loading…" is shown.
pub const DIFF_LOADING_DELAY: Duration = Duration::from_millis(150);

/// Executes effects off the UI loop and reports each result as an intent.
/// Makes no decisions: staleness and bookkeeping belong to the reducer.
pub struct Runtime {
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    diff: DiffStreamer,
    /// The global config file `LoadConfig` reads; none unless
    /// `with_global_config` names one, so only `main` reads the user's.
    global_config: Option<PathBuf>,
    /// The recent-repositories file. `None` keeps the list empty and
    /// unwritten, so only `main` touches the user's real one.
    recent_store: Option<PathBuf>,
}

impl Runtime {
    pub fn new(runner: Arc<dyn GitRunner>, tx: UnboundedSender<AppIntent>) -> Self {
        Self {
            diff: DiffStreamer::new(Arc::clone(&runner), tx.clone()),
            runner,
            tx,
            global_config: None,
            recent_store: None,
        }
    }

    pub fn with_recent_store(self, recent_store: Option<PathBuf>) -> Self {
        Self {
            recent_store,
            ..self
        }
    }

    pub fn with_global_config(self, global_config: Option<PathBuf>) -> Self {
        Self {
            global_config,
            ..self
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
            Effect::LoadHeadMessage { tag } => {
                tokio::spawn(async move {
                    // A panic still reports back, or the busy counter would stay raised.
                    let result =
                        tokio::task::spawn_blocking(move || read_head_message(runner.as_ref()))
                            .await
                            .unwrap_or_else(|e| {
                                Err(terminal_safe(format!("git log crashed: {e}").as_bytes()))
                            });
                    let _ = tx.send(AppIntent::HeadMessageLoaded { tag, result });
                });
            }
            Effect::LoadSubmodules { tag } => {
                tokio::spawn(async move {
                    // A panic still reports back, or the busy counter would stay raised.
                    let result =
                        tokio::task::spawn_blocking(move || read_submodules(runner.as_ref()))
                            .await
                            .unwrap_or_else(|e| {
                                Err(terminal_safe(
                                    format!("git submodule crashed: {e}").as_bytes(),
                                ))
                            });
                    let _ = tx.send(AppIntent::SubmodulesLoaded { tag, result });
                });
            }
            // Synchronous, so no later effect runs in the old repository.
            // A diff still streaming belongs to the old one.
            Effect::SetRepo { root, nested } => {
                self.diff.cancel();
                let dir = nested
                    .iter()
                    .fold(root, |dir, path| dir.join(path.as_os_str()));
                self.runner.set_workdir(dir);
            }
            Effect::LoadRecent { seq } => self.recent(seq, None),
            Effect::RecordRecent { path, seq } => self.recent(seq, Some(path)),
            Effect::InitRepo { tag } => {
                tokio::spawn(async move {
                    // A panic still reports back, or the busy counter would stay raised.
                    let result = tokio::task::spawn_blocking(move || init_repo(runner.as_ref()))
                        .await
                        .unwrap_or_else(|e| {
                            Err(terminal_safe(format!("git init crashed: {e}").as_bytes()))
                        });
                    let _ = tx.send(AppIntent::RepoInitialised { tag, result });
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
            Effect::LoadBlame { tag, path } => {
                tokio::spawn(async move {
                    // A panic still reports back, or the busy counter would stay raised.
                    let result =
                        tokio::task::spawn_blocking(move || read_blame(runner.as_ref(), &path))
                            .await
                            .unwrap_or_else(|e| {
                                Err(terminal_safe(format!("git blame crashed: {e}").as_bytes()))
                            });
                    let _ = tx.send(AppIntent::BlameLoaded { tag, result });
                });
            }
            Effect::LoadCommit { tag, oid } => {
                tokio::spawn(async move {
                    // A panic still reports back, or the busy counter would stay raised.
                    let result =
                        tokio::task::spawn_blocking(move || read_commit(runner.as_ref(), &oid))
                            .await
                            .unwrap_or_else(|e| {
                                Err(terminal_safe(format!("git show crashed: {e}").as_bytes()))
                            });
                    let _ = tx.send(AppIntent::CommitLoaded { tag, result });
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
            Effect::SnapshotRefs { tag } => {
                tokio::spawn(async move {
                    // A panic still reports back, or the watch would never poll again.
                    let result = tokio::task::spawn_blocking(move || read_refs(runner.as_ref()))
                        .await
                        .unwrap_or_else(|e| {
                            Err(terminal_safe(
                                format!("git for-each-ref crashed: {e}").as_bytes(),
                            ))
                        });
                    let _ = tx.send(AppIntent::RefsLoaded { tag, result });
                });
            }
            Effect::Fetch { tag } => {
                tokio::spawn(async move {
                    // A panic still reports back, or auto-fetch would stop for good.
                    let result = tokio::task::spawn_blocking(move || run_fetch(runner.as_ref()))
                        .await
                        .unwrap_or_else(|e| {
                            Err(terminal_safe(format!("git fetch crashed: {e}").as_bytes()))
                        });
                    let _ = tx.send(AppIntent::FetchFinished { tag, result });
                });
            }
            Effect::LoadConfig { repo } => {
                let paths = ConfigPaths::new(self.global_config.clone(), repo.as_deref());
                tokio::spawn(async move {
                    // `load` reports its own errors; a panic leaves the keys as they were.
                    if let Ok(loaded) =
                        tokio::task::spawn_blocking(move || config::load(&paths)).await
                    {
                        let _ = tx.send(AppIntent::ConfigLoaded {
                            // Echoed so the reducer can drop a late result.
                            repo,
                            keymap: loaded.keymap,
                            errors: loaded.errors,
                        });
                    }
                });
            }
            // The loop takes these out before spawning; without a terminal
            // there is nothing to hand over, so one that slips through is dropped.
            Effect::HandOff(_) => {}
        }
    }

    /// Load the recent list, recording `repo` first when given, and report
    /// it. A list that cannot be written is still shown as it was read.
    fn recent(&self, seq: u64, repo: Option<PathBuf>) {
        let store = self.recent_store.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let list = tokio::task::spawn_blocking(move || {
                let Some(store) = store else {
                    return Vec::new();
                };
                match repo {
                    Some(repo) => {
                        recent::record(&store, &repo).unwrap_or_else(|_| recent::load(&store))
                    }
                    None => recent::load(&store),
                }
            })
            .await
            .unwrap_or_default();
            let _ = tx.send(AppIntent::RecentLoaded { seq, list });
        });
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
    async fn load_config_reads_the_files_again_each_time() {
        use crate::keymap::{KeyPattern, parse_key, registry};

        let dir = std::env::temp_dir().join(format!("kika-load-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let global = dir.join("config.toml");
        let (tx, mut rx) = mpsc::unbounded_channel();
        // No git calls are expected.
        let runtime =
            Runtime::new(Arc::new(FakeRunner::new()), tx).with_global_config(Some(global.clone()));
        let stage = registry().iter().find(|b| b.id == "files.stage").unwrap();
        let commit = registry().iter().find(|b| b.id == "files.commit").unwrap();
        let mut load = async |text: &str, repo: Option<PathBuf>| -> Vec<KeyPattern> {
            std::fs::write(&global, text).unwrap();
            runtime.spawn(Effect::LoadConfig { repo: repo.clone() });
            match rx.recv().await {
                Some(AppIntent::ConfigLoaded {
                    repo: echoed,
                    keymap,
                    errors,
                }) => {
                    assert_eq!(echoed, repo, "answers for the repository it read");
                    assert_eq!(errors, []);
                    [keymap.keys(stage), keymap.keys(commit)].concat()
                }
                other => panic!("expected ConfigLoaded, got {other:?}"),
            }
        };

        let first = load("[keys]\n\"files.stage\" = [\"x\"]\n", None).await;
        let second = load("[keys]\n\"files.stage\" = [\"y\"]\n", None).await;
        // The repository named in the effect is the one whose file is read.
        let repo = dir.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            repo.join(".kika.toml"),
            "[keys]\n\"files.commit\" = [\"C\"]\n",
        )
        .unwrap();
        let in_repo = load("", Some(repo)).await;
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(first, [parse_key("x").unwrap(), parse_key("c").unwrap()]);
        assert_eq!(second, [parse_key("y").unwrap(), parse_key("c").unwrap()]);
        assert_eq!(in_repo, [stage.keys[0], parse_key("C").unwrap()]);
    }

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

    #[tokio::test]
    async fn load_head_message_reports_back_with_the_same_tag() {
        let runner = Arc::new(FakeRunner::new().expect(
            &["log", "-1", "--format=%B", "HEAD", "--"],
            ok(b"subj\n\nbody\n\n"),
        ));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 1,
            seq: 4,
            user: true,
        };

        runtime.spawn(Effect::LoadHeadMessage { tag });

        assert_eq!(
            rx.recv().await,
            Some(AppIntent::HeadMessageLoaded {
                tag,
                result: Ok("subj\n\nbody".into()),
            })
        );
        runner.assert_done();
    }

    #[tokio::test]
    async fn a_panicking_head_message_load_still_reports_back() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(HostilePanic), tx);
        let tag = Tag {
            generation: 0,
            seq: 5,
            user: true,
        };

        runtime.spawn(Effect::LoadHeadMessage { tag });

        match rx.recv().await {
            Some(AppIntent::HeadMessageLoaded {
                tag: got,
                result: Err(message),
            }) => {
                assert_eq!(got, tag);
                assert!(message.starts_with("git log crashed: "), "{message:?}");
                assert!(!message.contains(char::is_control), "{message:?}");
            }
            other => panic!("expected a failed HeadMessageLoaded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn load_blame_reports_back_with_the_same_tag() {
        let oid = "a".repeat(40);
        let out = format!("{oid} 1 1 1\nauthor Ann\nauthor-time 0\nauthor-tz +0000\n\tx\n");
        let runner = Arc::new(
            FakeRunner::new().expect(&["blame", "--porcelain", "--", "a.rs"], ok(out.as_bytes())),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 1,
            seq: 4,
            user: true,
        };

        runtime.spawn(Effect::LoadBlame {
            tag,
            path: "a.rs".into(),
        });

        match rx.recv().await {
            Some(AppIntent::BlameLoaded {
                tag: got,
                result: Ok(lines),
            }) => {
                assert_eq!(got, tag);
                assert_eq!(lines.len(), 1);
                assert_eq!(lines[0].author, "Ann");
            }
            other => panic!("expected a loaded blame, got {other:?}"),
        }
        runner.assert_done();
    }

    #[tokio::test]
    async fn a_panicking_blame_or_commit_load_still_reports_back() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(HostilePanic), tx);
        let tag = Tag {
            generation: 0,
            seq: 5,
            user: true,
        };

        runtime.spawn(Effect::LoadBlame {
            tag,
            path: "a.rs".into(),
        });
        match rx.recv().await {
            Some(AppIntent::BlameLoaded {
                tag: got,
                result: Err(message),
            }) => {
                assert_eq!(got, tag);
                assert!(message.starts_with("git blame crashed: "), "{message:?}");
                assert!(!message.contains(char::is_control), "{message:?}");
            }
            other => panic!("expected a failed BlameLoaded, got {other:?}"),
        }

        runtime.spawn(Effect::LoadCommit {
            tag,
            oid: "abc".into(),
        });
        match rx.recv().await {
            Some(AppIntent::CommitLoaded {
                tag: got,
                result: Err(message),
            }) => {
                assert_eq!(got, tag);
                assert!(message.starts_with("git show crashed: "), "{message:?}");
                assert!(!message.contains(char::is_control), "{message:?}");
            }
            other => panic!("expected a failed CommitLoaded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn load_commit_reports_back_with_the_same_tag() {
        let runner = Arc::new(FakeRunner::new().expect(
            &[
                "show",
                "--no-color",
                "--no-ext-diff",
                "--pretty=medium",
                "--date=iso",
                "abc",
                "--",
            ],
            ok(b"commit abc\n"),
        ));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 1,
            seq: 6,
            user: true,
        };

        runtime.spawn(Effect::LoadCommit {
            tag,
            oid: "abc".into(),
        });

        match rx.recv().await {
            Some(AppIntent::CommitLoaded {
                tag: got,
                result: Ok(lines),
            }) => {
                assert_eq!(got, tag);
                assert_eq!(lines.len(), 1);
                assert_eq!(lines[0].text, "commit abc");
            }
            other => panic!("expected a loaded commit, got {other:?}"),
        }
        runner.assert_done();
    }

    fn patch_request() -> DiffRequest {
        DiffRequest {
            path: "a.rs".into(),
            orig_path: None,
            side: DiffSide::Index,
            options: Default::default(),
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
    async fn load_submodules_reports_back_with_the_same_tag() {
        use crate::git::submodule::{Submodule, SubmoduleState};

        let runner = Arc::new(
            FakeRunner::new().expect(&["submodule", "status"], ok(b" 1234567 sub (heads/main)\n")),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 1,
            seq: 3,
            user: true,
        };

        runtime.spawn(Effect::LoadSubmodules { tag });

        assert_eq!(
            rx.recv().await,
            Some(AppIntent::SubmodulesLoaded {
                tag,
                result: Ok(vec![Submodule {
                    path: "sub".into(),
                    oid: "1234567".into(),
                    state: SubmoduleState::UpToDate,
                }]),
            })
        );
        runner.assert_done();
    }

    #[tokio::test]
    async fn a_panicking_submodule_load_still_reports_back() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(HostilePanic), tx);
        let tag = Tag {
            generation: 0,
            seq: 6,
            user: true,
        };

        runtime.spawn(Effect::LoadSubmodules { tag });

        match rx.recv().await {
            Some(AppIntent::SubmodulesLoaded {
                tag: got,
                result: Err(message),
            }) => {
                assert_eq!(got, tag);
                assert!(
                    message.starts_with("git submodule crashed: "),
                    "{message:?}"
                );
                assert!(!message.contains(char::is_control), "{message:?}");
            }
            other => panic!("expected a failed SubmodulesLoaded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn set_repo_moves_the_runner_into_nested_submodules_and_back_to_the_root() {
        use std::path::Path;

        use crate::git::CliRunner;

        let runner = Arc::new(CliRunner::new("/r/top"));
        let (tx, _rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);

        runtime.spawn(Effect::SetRepo {
            root: "/r/top".into(),
            nested: vec!["libs/a b".into()],
        });
        assert_eq!(runner.workdir(), Path::new("/r/top/libs/a b"));

        runtime.spawn(Effect::SetRepo {
            root: "/r/top".into(),
            nested: vec!["libs/a b".into(), "inner".into()],
        });
        assert_eq!(runner.workdir(), Path::new("/r/top/libs/a b/inner"));

        runtime.spawn(Effect::SetRepo {
            root: "/r/top".into(),
            nested: vec![],
        });
        assert_eq!(runner.workdir(), Path::new("/r/top"));

        // Another repository altogether.
        runtime.spawn(Effect::SetRepo {
            root: "/other".into(),
            nested: vec!["sub".into()],
        });
        assert_eq!(runner.workdir(), Path::new("/other/sub"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn set_repo_joins_the_raw_bytes_of_a_path() {
        use std::os::unix::ffi::OsStrExt;

        use crate::git::CliRunner;
        use crate::git::path::RepoPath;

        let runner = Arc::new(CliRunner::new("/r"));
        let (tx, _rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);

        runtime.spawn(Effect::SetRepo {
            root: "/r".into(),
            nested: vec![RepoPath::from_bytes(b"x\xff")],
        });
        assert_eq!(runner.workdir().as_os_str().as_bytes(), b"/r/x\xff");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_repo_stops_the_diff_still_streaming() {
        let child = FakeChild::hanging();
        let runner = Arc::new(FakeRunner::new().expect_spawn(
            &["diff", "--no-color", "--no-ext-diff", "-U3", "--", "a.rs"],
            child.clone(),
        ));
        let (tx, _rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        runtime.spawn(Effect::LoadDiff {
            tag: Tag {
                generation: 0,
                seq: 1,
                user: false,
            },
            request: DiffRequest {
                path: "a.rs".into(),
                orig_path: None,
                side: DiffSide::Worktree,
                options: Default::default(),
            },
            want: 100,
        });

        runtime.spawn(Effect::SetRepo {
            root: "/r".into(),
            nested: vec![],
        });

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !child.was_killed() {
            assert!(std::time::Instant::now() < deadline, "never killed");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn without_a_store_the_recent_list_is_empty_and_nothing_is_written() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(FakeRunner::new()), tx);

        runtime.spawn(Effect::RecordRecent {
            path: std::env::temp_dir(),
            seq: 1,
        });
        let empty = |seq| Some(AppIntent::RecentLoaded { seq, list: vec![] });
        assert_eq!(rx.recv().await, empty(1));
        runtime.spawn(Effect::LoadRecent { seq: 2 });
        assert_eq!(rx.recv().await, empty(2));
    }

    #[tokio::test]
    async fn with_a_store_recording_reports_the_new_list_and_loading_reads_it() {
        use crate::git::fake::TempDir;

        let tmp = TempDir::new("store");
        let store = tmp.path().join("kika").join("recent-repos");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime =
            Runtime::new(Arc::new(FakeRunner::new()), tx).with_recent_store(Some(store.clone()));
        let repo = tmp.path().to_path_buf();

        runtime.spawn(Effect::RecordRecent {
            path: repo.clone(),
            seq: 1,
        });
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::RecentLoaded {
                seq: 1,
                list: vec![repo.clone()]
            })
        );
        assert!(store.is_file());
        runtime.spawn(Effect::LoadRecent { seq: 2 });
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::RecentLoaded {
                seq: 2,
                list: vec![repo]
            })
        );
    }

    #[tokio::test]
    async fn init_repo_reports_the_new_toplevel_with_the_same_tag() {
        let runner = Arc::new(
            FakeRunner::new()
                .expect(&["init", "-q"], ok(b""))
                .expect(&["rev-parse", "--show-toplevel"], ok(b"/r/new\n")),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 0,
            seq: 2,
            user: true,
        };

        runtime.spawn(Effect::InitRepo { tag });

        assert_eq!(
            rx.recv().await,
            Some(AppIntent::RepoInitialised {
                tag,
                result: Ok("/r/new".into()),
            })
        );
        runner.assert_done();
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

    fn background(seq: u64) -> Tag {
        Tag {
            generation: 1,
            seq,
            user: false,
        }
    }

    #[tokio::test]
    async fn snapshot_refs_reports_back_with_the_same_tag() {
        let runner = Arc::new(
            FakeRunner::new()
                .expect(
                    &["for-each-ref", "--format=%(objectname) %(refname)"],
                    ok(b"1 refs/heads/main\n"),
                )
                .expect(&["symbolic-ref", "-q", "HEAD"], ok(b"refs/heads/main\n"))
                .expect(&["rev-parse", "-q", "--verify", "HEAD"], ok(b"1\n")),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);

        runtime.spawn(Effect::SnapshotRefs { tag: background(3) });

        match rx.recv().await {
            Some(AppIntent::RefsLoaded { tag, result: Ok(_) }) => assert_eq!(tag, background(3)),
            other => panic!("expected a loaded snapshot, got {other:?}"),
        }
        runner.assert_done();
    }

    #[tokio::test]
    async fn a_panicking_snapshot_still_reports_back() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(HostilePanic), tx);

        runtime.spawn(Effect::SnapshotRefs { tag: background(4) });

        match rx.recv().await {
            Some(AppIntent::RefsLoaded {
                tag,
                result: Err(message),
            }) => {
                assert_eq!(tag, background(4));
                assert!(
                    message.starts_with("git for-each-ref crashed: "),
                    "{message:?}"
                );
                assert!(!message.contains(char::is_control), "{message:?}");
            }
            other => panic!("expected a failed RefsLoaded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_reports_back_with_the_same_tag() {
        let runner = Arc::new(FakeRunner::new().expect(
            &["fetch", "--all", "--quiet", "--no-write-fetch-head"],
            ok(b""),
        ));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);

        runtime.spawn(Effect::Fetch { tag: background(5) });

        assert_eq!(
            rx.recv().await,
            Some(AppIntent::FetchFinished {
                tag: background(5),
                result: Ok(()),
            })
        );
        runner.assert_done();
    }

    #[tokio::test]
    async fn a_panicking_fetch_still_reports_back() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(HostilePanic), tx);

        runtime.spawn(Effect::Fetch { tag: background(6) });

        match rx.recv().await {
            Some(AppIntent::FetchFinished {
                tag,
                result: Err(message),
            }) => {
                assert_eq!(tag, background(6));
                assert!(message.starts_with("git fetch crashed: "), "{message:?}");
                assert!(!message.contains(char::is_control), "{message:?}");
            }
            other => panic!("expected a failed FetchFinished, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn load_diff_streams_through_the_diff_streamer() {
        let runner = Arc::new(FakeRunner::new().expect_spawn(
            &["diff", "--no-color", "--no-ext-diff", "-U3", "--", "a.rs"],
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
                options: Default::default(),
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
