use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;

use crate::ansi;
use crate::captured_command::run_captured;
use crate::clipboard::{self, ClipboardSink, TerminalSink};
use crate::commits;
use crate::config::{self, ConfigPaths};
use crate::conflict::MAX_CONFLICT_FILE;
use crate::custom_command::{CommandFailure, OutputMode};
use crate::diff_mark::DiffEnd;
use crate::diff_stream::DiffStreamer;
use crate::diff_tool;
use crate::external_diff::{ExternalRenderer, RendererKind, discover, read_diff, run_renderer};
use crate::forge::ForgeStatus;
use crate::forge::task::{ForgeContext, Load, run_load};
use crate::forge::{ForgeHost, ForgeRepo};
use crate::git::GitRunner;
use crate::git::askpass::Secret;
use crate::git::attr::conflict_marker_size;
use crate::git::blame::{read_blame, read_commit};
use crate::git::branch_tips::read_tips;
use crate::git::branches::BranchName;
use crate::git::branches::{branch_log, list_branches};
use crate::git::commit_files::{CommitId, CommitRange, commit_files};
use crate::git::diff::{DiffRequest, EmojiZwj, load_patch, sanitize, terminal_safe};
use crate::git::fetch::run_fetch;
use crate::git::head::{read_commit_message, read_head_message};
use crate::git::left_behind::read_left_behind;
use crate::git::log::{LogQuery, branch_commit, log_page, markers};
use crate::git::ops::{GitOp, run_op};
use crate::git::path::RepoPath;
use crate::git::prompts::read_prompt_facts;
use crate::git::rebase_plan::{parse_todo, validate_edit};
use crate::git::reflog::read_reflog;
use crate::git::refs::read_refs;
use crate::git::remote::{AskpassEnv, RemoteFailure, RemoteOp, run_remote};
use crate::git::remotes::list_remotes;
use crate::git::repo_root::init_repo;
use crate::git::repo_state::{git_dir_command, git_dir_from};
use crate::git::stash::{StashEntry, list_stashes, stash_files};
use crate::git::status::read_status;
use crate::git::submodule::read_submodules;
use crate::git::tags::list_tags;
use crate::git::worktrees::{add_worktree, read_worktrees, remove_worktree};
use crate::gitignore::{self, IgnoreAdded};
use crate::handoff::HandOff;
use crate::intent::AppIntent;
use crate::rebase_todo::{TodoFile, nothing_done};
use crate::recent;
use crate::sequence_editor::MAX_TODO;
use crate::worktree_file;

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
    /// Read what decides whether the commit `op` needs the terminal;
    /// always answered by one `CommitProbed`.
    ProbeCommit {
        tag: Tag,
        op: GitOp,
    },
    /// List the submodules; always answered by one `SubmodulesLoaded`.
    LoadSubmodules {
        tag: Tag,
    },
    /// List the worktrees; always answered by one `WorktreesLoaded`.
    LoadWorktrees {
        tag: Tag,
    },
    /// Check the typed `path`, then add a worktree for `branch` there;
    /// always answered by one `WorktreeAdded`.
    AddWorktree {
        tag: Tag,
        path: String,
        branch: BranchName,
    },
    /// Remove the linked worktree at `path`, never forced; always
    /// answered by one `WorktreeRemoved`.
    RemoveWorktree {
        tag: Tag,
        path: RepoPath,
    },
    /// Point git at the repository `nested` names: each path is a
    /// submodule of the one before, starting from the top-level `root`,
    /// which an empty list means. Runs before the next effect starts.
    SetRepo {
        root: PathBuf,
        nested: Vec<RepoPath>,
    },
    /// List the local branches; always answered by one `BranchesLoaded`.
    LoadBranches {
        tag: Tag,
    },
    /// Read the newest commits of the full ref `refname`, local or
    /// remote-tracking; always answered by one `BranchLogLoaded` for the
    /// same `refname`.
    LoadBranchLog {
        tag: Tag,
        refname: Vec<u8>,
    },
    /// List the remotes and their remote-tracking branches; always
    /// answered by one `RemotesLoaded`.
    LoadRemotes {
        tag: Tag,
    },
    /// List the tags; always answered by one `TagsLoaded`.
    LoadTags {
        tag: Tag,
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
    /// Read conflicted `path` from the worktree and its marker size;
    /// always answered by one `ConflictLoaded` for the same `path`.
    LoadConflict {
        tag: Tag,
        path: RepoPath,
    },
    /// Write `content` over `path`, only while it still holds `expected`;
    /// always answered by one `ResolvedWritten` for the same `path`.
    WriteResolved {
        tag: Tag,
        path: RepoPath,
        expected: Vec<u8>,
        content: Vec<u8>,
    },
    /// Read commit `oid`'s message and diff; always answered by one
    /// `CommitLoaded`.
    LoadCommit {
        tag: Tag,
        oid: String,
    },
    /// Find the configured diff renderers: the global `[diff] pager`, then
    /// git's global pagers, then the global `[diff] tool`. Answered by one
    /// `RenderersFound`.
    FindRenderers,
    /// Run `renderer` at `cols` columns: a pager on the diff for `request`,
    /// a tool on both sides of its file. Always answered by one
    /// `DiffRendered`.
    RenderDiff {
        tag: Tag,
        request: DiffRequest,
        renderer: ExternalRenderer,
        cols: u16,
    },
    /// Hand the terminal to another process. Run by the loop, which owns
    /// the terminal, never by [`Runtime`].
    HandOff(HandOff),
    /// Read the refs snapshot; always answered by one `RefsLoaded`.
    SnapshotRefs {
        tag: Tag,
    },
    /// Read a page of `HEAD`'s log, from `tip` when set, narrowed to
    /// `query`; always answered by one `CommitsLoaded`.
    LoadCommits {
        tag: Tag,
        tip: Option<String>,
        skip: usize,
        query: LogQuery,
    },
    /// Read both ends of a comparison as commits; always answered by one
    /// `DiffResolved`.
    ResolveDiff {
        tag: Tag,
        base: DiffEnd,
        target: DiffEnd,
    },
    /// List the files `range` changed; always answered by one
    /// `CommitFilesLoaded`.
    LoadCommitFiles {
        tag: Tag,
        range: CommitRange,
    },
    /// Read the stash list; always answered by one `StashesLoaded`.
    LoadStashes {
        tag: Tag,
    },
    /// List the files `entry` holds; always answered by one
    /// `StashFilesLoaded`.
    LoadStashFiles {
        tag: Tag,
        entry: StashEntry,
    },
    /// Read which commits are unpushed or unmerged; always answered by
    /// one `MarkersLoaded`.
    LoadMarkers {
        tag: Tag,
    },
    /// Read where every local branch points; always answered by one
    /// `BranchTipsLoaded`.
    LoadBranchTips {
        tag: Tag,
    },
    /// Read the stopped rebase's todo list and what it has done; always
    /// answered by one `RebaseTodoLoaded`.
    LoadRebaseTodo {
        tag: Tag,
    },
    /// Check `content` as an edit of `expected`, then write it over the
    /// todo list at `path` only while it still holds `expected`; always
    /// answered by one `RebaseTodoWritten`.
    WriteRebaseTodo {
        tag: Tag,
        path: PathBuf,
        expected: Vec<u8>,
        content: Vec<u8>,
    },
    /// Read `HEAD`'s reflog and where `HEAD` is; always answered by one
    /// `ReflogLoaded`.
    LoadReflog {
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
    /// Run the user's command in the background, in the repository's
    /// workdir, with its output captured; always answered by one
    /// `CommandFinished`.
    RunCommand {
        title: String,
        argv: Vec<OsString>,
        output: OutputMode,
    },
    /// Stop the command `RunCommand` started, which then answers with
    /// `CommandFailure::Cancelled`. Does nothing once it has finished.
    CancelCommand,
    /// Fetch, pull or push, with git's and ssh's questions sent as
    /// `CredentialAsked`; always answered by one `RemoteFinished`.
    Remote {
        tag: Tag,
        op: RemoteOp,
    },
    /// Hand the answer to the question the running remote operation
    /// asked; `None` cancels it. Does nothing when no question waits.
    AnswerCredential(Option<Secret>),
    /// Find the forge repository of the remote and send its cached badges,
    /// then, with `refresh`, ask the forge for `branches`. Answered by
    /// `ForgeRepoNone`, or `PullRequestsCached` and then, with `refresh`,
    /// `PullRequestsFetched`. With no `hosts` it only answers `ForgeRepoNone`.
    /// A `known` repository is used as is, without reading the remote.
    /// A host in `limited` is not asked before its second has passed.
    LoadPullRequests {
        tag: Tag,
        hosts: Vec<ForgeHost>,
        branches: Vec<BranchName>,
        refresh: bool,
        known: Option<ForgeRepo>,
        limited: Vec<(ForgeHost, i64)>,
    },
    /// Put `text` on the system clipboard. Run by the loop, between draws,
    /// never by a task; a failure comes back as `CopyFailed`.
    Clipboard(String),
    /// Read commit `oid`'s full message to copy it; always answered by
    /// one `CommitMessageLoaded`.
    LoadCommitMessage {
        tag: Tag,
        oid: CommitId,
    },
    /// List the commits only `head` reaches before `op` moves `HEAD` to
    /// `target`; always answered by one `LeftBehindChecked`.
    CheckLeftBehind {
        tag: Tag,
        op: GitOp,
        head: CommitId,
        target: CommitId,
    },
    /// Add `line` to the worktree's top-level `.gitignore`, creating it if
    /// need be; always answered by one `IgnoreWritten`.
    AppendIgnore {
        tag: Tag,
        line: Vec<u8>,
    },
}

/// The todo list of the rebase in progress and its `done` list, from
/// `<git dir>/rebase-merge/`. `None` when there is no todo file: a rebase
/// that is not interactive, or one between steps.
fn read_rebase_todo(runner: &dyn GitRunner) -> Result<Option<TodoFile>, String> {
    let output = runner
        .run(&git_dir_command())
        .map_err(|e| terminal_safe(e.to_string().as_bytes()))?;
    if !output.success {
        return Err(terminal_safe(&output.stderr));
    }
    let dir = git_dir_from(&output.stdout).join("rebase-merge");
    let path = dir.join("git-rebase-todo");
    if std::fs::symlink_metadata(&path).is_err() {
        return Ok(None);
    }
    let todo = worktree_file::read_file(&path, MAX_TODO)?;
    let done = worktree_file::read_file(&dir.join("done"), MAX_TODO).ok();
    Ok(Some(TodoFile { path, todo, done }))
}

/// Write an edited todo list, once it is sure to be git's todo file and a
/// safe edit of what was read. The model checked the same; this is the
/// check that counts.
fn write_rebase_todo(path: &Path, expected: &[u8], content: &[u8]) -> Result<(), String> {
    let dir = path.parent();
    let is_todo = path
        .file_name()
        .is_some_and(|name| name == "git-rebase-todo")
        && dir
            .and_then(Path::file_name)
            .is_some_and(|name| name == "rebase-merge");
    if !is_todo {
        return Err(format!("Not a rebase todo list: {}", path.display()));
    }
    let before = parse_todo(expected)?;
    let after = parse_todo(content)?;
    let done_empty = dir
        .and_then(|dir| worktree_file::read_file(&dir.join("done"), MAX_TODO).ok())
        .is_some_and(|done| nothing_done(&done));
    validate_edit(&before, &after, done_empty)?;
    worktree_file::replace_file(path, expected, content)
}

/// Seconds since the epoch, which relative commit dates count from.
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// A comparison's end as a commit: a commit as it is, a branch where it
/// points now.
fn resolve_end(runner: &dyn GitRunner, end: DiffEnd) -> Result<CommitId, String> {
    match end {
        DiffEnd::Commit(id) => Ok(id),
        DiffEnd::Branch(name) => branch_commit(runner, &name),
    }
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
    /// The global config's `[diff] pager`, read once at startup.
    diff_pager: Option<String>,
    /// The global config's `[diff] tool`, read once at startup.
    diff_tool: Option<String>,
    /// How diff and blame body lines draw emoji ZWJ sequences, resolved
    /// once at startup for the running terminal.
    emoji_zwj: EmojiZwj,
    /// Stops the last captured command; aborting a finished one is a no-op.
    command: Mutex<Option<tokio::task::AbortHandle>>,
    /// Where the running remote operation waits for an answer.
    credential: AnswerSlot,
    /// How to reach a forge; `None` keeps every forge load local and empty.
    forge: Option<ForgeContext>,
    /// Where copied text goes: the terminal, unless a test records it.
    clipboard: Arc<dyn ClipboardSink>,
}

impl Runtime {
    pub fn new(runner: Arc<dyn GitRunner>, tx: UnboundedSender<AppIntent>) -> Self {
        Self {
            diff: DiffStreamer::new(Arc::clone(&runner), tx.clone()),
            runner,
            tx,
            global_config: None,
            recent_store: None,
            diff_pager: None,
            diff_tool: None,
            emoji_zwj: EmojiZwj::Escape,
            command: Mutex::new(None),
            credential: AnswerSlot::default(),
            forge: None,
            clipboard: Arc::new(TerminalSink),
        }
    }

    /// The forge client, used only for hosts the global config names.
    pub fn with_forge(self, forge: Option<ForgeContext>) -> Self {
        Self { forge, ..self }
    }

    /// Send copies to `clipboard` instead of the terminal.
    // Only the headless harness swaps it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn with_clipboard(self, clipboard: Arc<dyn ClipboardSink>) -> Self {
        Self { clipboard, ..self }
    }

    /// Put `text` on the clipboard now, on the caller's thread, so the
    /// bytes never land inside a frame being drawn.
    pub fn copy_to_clipboard(&self, text: &str) -> Result<(), String> {
        let bytes = clipboard::osc52(text)?;
        self.clipboard
            .copy(&bytes)
            .map_err(|e| format!("Could not write to the terminal: {e}"))
    }

    /// The renderer the global config names, offered first by `FindRenderers`.
    pub fn with_diff_pager(self, diff_pager: Option<String>) -> Self {
        Self { diff_pager, ..self }
    }

    /// The display-only tool the global config names, offered last by
    /// `FindRenderers`.
    pub fn with_diff_tool(self, diff_tool: Option<String>) -> Self {
        Self { diff_tool, ..self }
    }

    /// Draw emoji ZWJ sequences in streamed diffs, blame and blamed
    /// commits as `emoji` says, for the whole session.
    pub fn with_emoji_zwj(mut self, emoji: EmojiZwj) -> Self {
        self.diff.set_emoji_zwj(emoji);
        Self {
            emoji_zwj: emoji,
            ..self
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
            Effect::LoadWorktrees { tag } => {
                tokio::spawn(async move {
                    // A panic still reports back, or the busy counter would stay raised.
                    let result =
                        tokio::task::spawn_blocking(move || read_worktrees(runner.as_ref()))
                            .await
                            .unwrap_or_else(|e| {
                                Err(terminal_safe(
                                    format!("git worktree list crashed: {e}").as_bytes(),
                                ))
                            });
                    let _ = tx.send(AppIntent::WorktreesLoaded { tag, result });
                });
            }
            Effect::AddWorktree { tag, path, branch } => {
                tokio::spawn(async move {
                    // A panic still reports back, or the busy counter would stay raised.
                    let result = tokio::task::spawn_blocking(move || {
                        add_worktree(runner.as_ref(), &path, &branch)
                    })
                    .await
                    .unwrap_or_else(|e| {
                        Err(terminal_safe(
                            format!("git worktree add crashed: {e}").as_bytes(),
                        ))
                    });
                    let _ = tx.send(AppIntent::WorktreeAdded { tag, result });
                });
            }
            Effect::RemoveWorktree { tag, path } => {
                tokio::spawn(async move {
                    // A panic still reports back, or the busy counter would stay raised.
                    let result = tokio::task::spawn_blocking(move || {
                        remove_worktree(runner.as_ref(), &path)
                    })
                    .await
                    .unwrap_or_else(|e| {
                        Err(terminal_safe(
                            format!("git worktree remove crashed: {e}").as_bytes(),
                        ))
                    });
                    let _ = tx.send(AppIntent::WorktreeRemoved { tag, result });
                });
            }
            Effect::LoadBranches { tag } => {
                tokio::spawn(async move {
                    // A panic still reports back, or the busy counter would stay raised.
                    let result =
                        tokio::task::spawn_blocking(move || list_branches(runner.as_ref()))
                            .await
                            .unwrap_or_else(|e| {
                                Err(terminal_safe(
                                    format!("git for-each-ref crashed: {e}").as_bytes(),
                                ))
                            });
                    let _ = tx.send(AppIntent::BranchesLoaded { tag, result });
                });
            }
            Effect::LoadBranchLog { tag, refname } => {
                tokio::spawn(async move {
                    let read = crate::git::path::os_string(&refname);
                    // A panic still reports back, or the busy counter would stay raised.
                    let result =
                        tokio::task::spawn_blocking(move || branch_log(runner.as_ref(), &read))
                            .await
                            .unwrap_or_else(|e| {
                                Err(terminal_safe(format!("git log crashed: {e}").as_bytes()))
                            });
                    let _ = tx.send(AppIntent::BranchLogLoaded {
                        tag,
                        refname,
                        result,
                    });
                });
            }
            Effect::LoadRemotes { tag } => {
                tokio::spawn(async move {
                    // A panic still reports back, or the busy counter would stay raised.
                    let result = tokio::task::spawn_blocking(move || list_remotes(runner.as_ref()))
                        .await
                        .unwrap_or_else(|e| {
                            Err(terminal_safe(
                                format!("git for-each-ref crashed: {e}").as_bytes(),
                            ))
                        });
                    let _ = tx.send(AppIntent::RemotesLoaded { tag, result });
                });
            }
            Effect::LoadTags { tag } => {
                tokio::spawn(async move {
                    // A panic still reports back, or the busy counter would stay raised.
                    let result = tokio::task::spawn_blocking(move || list_tags(runner.as_ref()))
                        .await
                        .unwrap_or_else(|e| {
                            Err(terminal_safe(
                                format!("git for-each-ref crashed: {e}").as_bytes(),
                            ))
                        });
                    let _ = tx.send(AppIntent::TagsLoaded { tag, result });
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
                let emoji = self.emoji_zwj;
                tokio::spawn(async move {
                    // A panic still reports back, or the busy counter would stay raised.
                    let result = tokio::task::spawn_blocking(move || {
                        read_blame(runner.as_ref(), &path, emoji)
                    })
                    .await
                    .unwrap_or_else(|e| {
                        Err(terminal_safe(format!("git blame crashed: {e}").as_bytes()))
                    });
                    let _ = tx.send(AppIntent::BlameLoaded { tag, result });
                });
            }
            Effect::LoadConflict { tag, path } => {
                // Where git runs now, inside an entered submodule too.
                let workdir = self.runner.workdir();
                tokio::spawn(async move {
                    let read = path.clone();
                    // A panic still reports back, or the busy counter would stay raised.
                    let result = tokio::task::spawn_blocking(move || {
                        let size = conflict_marker_size(runner.as_ref(), &read)?;
                        let bytes = worktree_file::read(&workdir, &read, MAX_CONFLICT_FILE)?;
                        Ok((bytes, size))
                    })
                    .await
                    .unwrap_or_else(|e| {
                        Err(terminal_safe(
                            format!("Reading the file crashed: {e}").as_bytes(),
                        ))
                    });
                    let _ = tx.send(AppIntent::ConflictLoaded { tag, path, result });
                });
            }
            Effect::WriteResolved {
                tag,
                path,
                expected,
                content,
            } => {
                let workdir = self.runner.workdir();
                tokio::spawn(async move {
                    let target = path.clone();
                    // A panic still reports back, or `op_seq` would block every later op.
                    let result = tokio::task::spawn_blocking(move || {
                        worktree_file::replace(&workdir, &target, &expected, &content)
                            .map(|()| content)
                    })
                    .await
                    .unwrap_or_else(|e| {
                        Err(terminal_safe(
                            format!("Writing the file crashed: {e}").as_bytes(),
                        ))
                    });
                    let _ = tx.send(AppIntent::ResolvedWritten { tag, path, result });
                });
            }
            Effect::LoadCommit { tag, oid } => {
                let emoji = self.emoji_zwj;
                tokio::spawn(async move {
                    // A panic still reports back, or the busy counter would stay raised.
                    let result = tokio::task::spawn_blocking(move || {
                        read_commit(runner.as_ref(), &oid, emoji)
                    })
                    .await
                    .unwrap_or_else(|e| {
                        Err(terminal_safe(format!("git show crashed: {e}").as_bytes()))
                    });
                    let _ = tx.send(AppIntent::CommitLoaded { tag, result });
                });
            }
            Effect::FindRenderers => {
                let pager = self.diff_pager.clone();
                let tool = self.diff_tool.clone();
                tokio::spawn(async move {
                    // A panic finds nothing; the built-in view still works.
                    let found =
                        tokio::task::spawn_blocking(move || discover(runner.as_ref(), pager, tool))
                            .await
                            .unwrap_or_default();
                    let _ = tx.send(AppIntent::RenderersFound(found));
                });
            }
            Effect::RenderDiff {
                tag,
                request,
                renderer,
                cols,
            } => {
                // Where git runs now, inside an entered submodule too.
                let workdir = self.runner.workdir();
                let label = renderer.label.clone();
                tokio::spawn(async move {
                    // A panic still reports back, or the busy counter would stay raised.
                    let result = tokio::task::spawn_blocking(move || {
                        let out = match renderer.kind {
                            RendererKind::Pager => {
                                let diff = read_diff(runner.as_ref(), &request, &renderer.label)?;
                                run_renderer(&renderer, &workdir, &diff, cols)?
                            }
                            RendererKind::Tool => diff_tool::render(
                                runner.as_ref(),
                                &workdir,
                                &request,
                                &renderer,
                                cols,
                            )?,
                        };
                        Ok(ansi::parse(&out))
                    })
                    .await
                    .unwrap_or_else(|e| {
                        Err(terminal_safe(format!("{label} crashed: {e}").as_bytes()))
                    });
                    let _ = tx.send(AppIntent::DiffRendered { tag, result });
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
            Effect::ProbeCommit { tag, op } => {
                tokio::spawn(async move {
                    // Errors and panics both report `None`, which hands over.
                    let facts =
                        tokio::task::spawn_blocking(move || read_prompt_facts(runner.as_ref()))
                            .await
                            .ok()
                            .and_then(Result::ok);
                    let _ = tx.send(AppIntent::CommitProbed { tag, op, facts });
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
            Effect::LoadCommits {
                tag,
                tip,
                skip,
                query,
            } => {
                tokio::spawn(async move {
                    // A panic still reports back, or the panel would wait forever.
                    let result = tokio::task::spawn_blocking(move || {
                        let page =
                            log_page(runner.as_ref(), tip.as_deref(), skip, commits::PAGE, &query)?;
                        Ok((page, unix_now()))
                    })
                    .await
                    .unwrap_or_else(|e| {
                        Err(terminal_safe(format!("git log crashed: {e}").as_bytes()))
                    });
                    let _ = tx.send(AppIntent::CommitsLoaded { tag, skip, result });
                });
            }
            Effect::ResolveDiff { tag, base, target } => {
                tokio::spawn(async move {
                    // A panic still reports back, or the busy counter would stay raised.
                    let result = tokio::task::spawn_blocking(move || {
                        let base = resolve_end(runner.as_ref(), base)?;
                        let target = resolve_end(runner.as_ref(), target)?;
                        Ok(CommitRange {
                            oid: target,
                            parent: Some(base),
                        })
                    })
                    .await
                    .unwrap_or_else(|e| {
                        Err(terminal_safe(
                            format!("git rev-parse crashed: {e}").as_bytes(),
                        ))
                    });
                    let _ = tx.send(AppIntent::DiffResolved { tag, result });
                });
            }
            Effect::LoadCommitFiles { tag, range } => {
                tokio::spawn(async move {
                    // A panic still reports back, or the busy counter would stay raised.
                    let result =
                        tokio::task::spawn_blocking(move || commit_files(runner.as_ref(), &range))
                            .await
                            .unwrap_or_else(|e| {
                                Err(terminal_safe(
                                    format!("git diff-tree crashed: {e}").as_bytes(),
                                ))
                            });
                    let _ = tx.send(AppIntent::CommitFilesLoaded { tag, result });
                });
            }
            Effect::LoadStashes { tag } => {
                tokio::spawn(async move {
                    // A panic still reports back, or the panel would wait forever.
                    let result = tokio::task::spawn_blocking(move || list_stashes(runner.as_ref()))
                        .await
                        .unwrap_or_else(|e| {
                            Err(terminal_safe(
                                format!("git stash list crashed: {e}").as_bytes(),
                            ))
                        });
                    let _ = tx.send(AppIntent::StashesLoaded { tag, result });
                });
            }
            Effect::LoadStashFiles { tag, entry } => {
                tokio::spawn(async move {
                    // A panic still reports back, or the busy counter would stay raised.
                    let result =
                        tokio::task::spawn_blocking(move || stash_files(runner.as_ref(), &entry))
                            .await
                            .unwrap_or_else(|e| {
                                Err(terminal_safe(
                                    format!("git diff-tree crashed: {e}").as_bytes(),
                                ))
                            });
                    let _ = tx.send(AppIntent::StashFilesLoaded { tag, result });
                });
            }
            Effect::LoadMarkers { tag } => {
                tokio::spawn(async move {
                    // A panic still reports back, or the panel would wait forever.
                    let result = tokio::task::spawn_blocking(move || markers(runner.as_ref()))
                        .await
                        .unwrap_or_else(|e| {
                            Err(terminal_safe(
                                format!("git rev-list crashed: {e}").as_bytes(),
                            ))
                        });
                    let _ = tx.send(AppIntent::MarkersLoaded { tag, result });
                });
            }
            Effect::LoadBranchTips { tag } => {
                tokio::spawn(async move {
                    // A panic still reports back, or the panel would wait forever.
                    let result = tokio::task::spawn_blocking(move || read_tips(runner.as_ref()))
                        .await
                        .unwrap_or_else(|e| {
                            Err(terminal_safe(
                                format!("git for-each-ref crashed: {e}").as_bytes(),
                            ))
                        });
                    let _ = tx.send(AppIntent::BranchTipsLoaded { tag, result });
                });
            }
            Effect::LoadRebaseTodo { tag } => {
                tokio::spawn(async move {
                    // A panic still reports back, or the panel would wait forever.
                    let result =
                        tokio::task::spawn_blocking(move || read_rebase_todo(runner.as_ref()))
                            .await
                            .unwrap_or_else(|e| {
                                Err(terminal_safe(
                                    format!("Reading the todo list crashed: {e}").as_bytes(),
                                ))
                            });
                    let _ = tx.send(AppIntent::RebaseTodoLoaded { tag, result });
                });
            }
            Effect::WriteRebaseTodo {
                tag,
                path,
                expected,
                content,
            } => {
                tokio::spawn(async move {
                    // A panic still reports back, or the todo would stay locked.
                    let result = tokio::task::spawn_blocking(move || {
                        write_rebase_todo(&path, &expected, &content)
                    })
                    .await
                    .unwrap_or_else(|e| {
                        Err(terminal_safe(
                            format!("Writing the todo list crashed: {e}").as_bytes(),
                        ))
                    });
                    let _ = tx.send(AppIntent::RebaseTodoWritten { tag, result });
                });
            }
            Effect::LoadReflog { tag } => {
                tokio::spawn(async move {
                    // A panic still reports back, or an undo would wait forever.
                    let result = tokio::task::spawn_blocking(move || read_reflog(runner.as_ref()))
                        .await
                        .unwrap_or_else(|e| {
                            Err(terminal_safe(format!("git reflog crashed: {e}").as_bytes()))
                        });
                    let _ = tx.send(AppIntent::ReflogLoaded { tag, result });
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
            Effect::RunCommand {
                title,
                argv,
                output,
            } => {
                // Taken now: a `SetRepo` after this must not move the command.
                let workdir = self.runner.workdir();
                let task = tokio::spawn(run_captured(argv, workdir));
                *lock(&self.command) = Some(task.abort_handle());
                tokio::spawn(async move {
                    // A panic or a cancel still reports back, or
                    // `command_running` would stay set.
                    let result = task.await.unwrap_or_else(|e| {
                        Err(if e.is_cancelled() {
                            CommandFailure::Cancelled
                        } else {
                            CommandFailure::Launch(terminal_safe(
                                format!("The command crashed: {e}").as_bytes(),
                            ))
                        })
                    });
                    let result = match output {
                        OutputMode::Popup => result,
                        OutputMode::Terminal | OutputMode::None => result.map(|_| String::new()),
                    };
                    let _ = tx.send(AppIntent::CommandFinished {
                        title,
                        output,
                        result,
                    });
                });
            }
            Effect::CancelCommand => {
                // Dropping the aborted future kills the program.
                if let Some(task) = lock(&self.command).take() {
                    task.abort();
                }
            }
            Effect::Remote { tag, op } => {
                let slot = Arc::clone(&self.credential);
                tokio::spawn(async move {
                    let bridge_tx = tx.clone();
                    // A panic still reports back, or `op_seq` would block every later op.
                    let task_op = op.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        let bridge = Bridge::open(tag, bridge_tx, slot);
                        run_remote(runner.as_ref(), &task_op, bridge.env.as_ref())
                    })
                    .await
                    .unwrap_or_else(|e| {
                        Err(RemoteFailure::Failed(terminal_safe(
                            format!("git crashed: {e}").as_bytes(),
                        )))
                    });
                    let _ = tx.send(AppIntent::RemoteFinished { tag, op, result });
                });
            }
            Effect::LoadCommitMessage { tag, oid } => {
                tokio::spawn(async move {
                    let read = oid.clone();
                    // A panic still reports back, or the busy counter would stay raised.
                    let result = tokio::task::spawn_blocking(move || {
                        read_commit_message(runner.as_ref(), &read)
                    })
                    .await
                    .unwrap_or_else(|e| {
                        Err(terminal_safe(format!("git log crashed: {e}").as_bytes()))
                    });
                    let _ = tx.send(AppIntent::CommitMessageLoaded { tag, oid, result });
                });
            }
            Effect::CheckLeftBehind {
                tag,
                op,
                head,
                target,
            } => {
                tokio::spawn(async move {
                    let from = head.clone();
                    // A panic still reports back, or the busy counter would stay raised.
                    let result = tokio::task::spawn_blocking(move || {
                        read_left_behind(runner.as_ref(), &from, &target)
                    })
                    .await
                    .unwrap_or_else(|e| {
                        Err(terminal_safe(
                            format!("git rev-list crashed: {e}").as_bytes(),
                        ))
                    });
                    let _ = tx.send(AppIntent::LeftBehindChecked {
                        tag,
                        op,
                        head,
                        result,
                    });
                });
            }
            Effect::AppendIgnore { tag, line } => {
                // Where git runs now, inside an entered submodule too.
                let workdir = self.runner.workdir();
                tokio::spawn(async move {
                    // A panic still reports back, or the busy counter would stay raised.
                    let result =
                        tokio::task::spawn_blocking(move || append_ignore(&workdir, &line))
                            .await
                            .unwrap_or_else(|e| {
                                Err(terminal_safe(
                                    format!("Writing .gitignore crashed: {e}").as_bytes(),
                                ))
                            });
                    let _ = tx.send(AppIntent::IgnoreWritten { tag, result });
                });
            }
            // The loop takes these out and copies itself, between draws.
            // Without a loop there is no terminal to copy through.
            Effect::Clipboard(_) => {}
            Effect::AnswerCredential(answer) => {
                if let Some(answers) = lock(&self.credential).as_ref() {
                    // Gone means the operation ended; nobody is asking.
                    let _ = answers.send(answer);
                }
            }
            // The loop takes these out before spawning; without a terminal
            // there is nothing to hand over, so one that slips through is dropped.
            Effect::HandOff(_) => {}
            Effect::LoadPullRequests {
                tag,
                hosts,
                branches,
                refresh,
                known,
                limited,
            } => match self.forge.clone().filter(|_| !hosts.is_empty()) {
                // Off: nothing is read, run or written.
                None => {
                    let _ = tx.send(AppIntent::ForgeRepoNone { tag });
                }
                Some(forge) => {
                    tokio::spawn(async move {
                        let send = tx.clone();
                        let done = tokio::task::spawn_blocking(move || {
                            let load = Load {
                                tag,
                                hosts: &hosts,
                                branches: &branches,
                                refresh,
                                known: known.as_ref(),
                                limited: &limited,
                                top: std::fs::canonicalize(runner.workdir()).ok(),
                                now: unix_now(),
                            };
                            run_load(runner.as_ref(), &forge, &load, &|intent| {
                                let _ = send.send(intent);
                            });
                        })
                        .await;
                        // A panic still ends the call, or no refresh would run again.
                        if done.is_err() && refresh {
                            let _ = tx.send(AppIntent::PullRequestsFetched {
                                tag,
                                result: Err(ForgeStatus::Failed),
                                limited_until: None,
                            });
                        }
                    });
                }
            },
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

/// The largest `.gitignore` read to add a line to it.
const MAX_GITIGNORE: u64 = 1024 * 1024;

/// Add `line` to the `.gitignore` at the top of `workdir`, unless it is a
/// line there already. Nothing but that one file is touched: no git
/// command runs and the index stays as it is.
fn append_ignore(workdir: &std::path::Path, line: &[u8]) -> Result<IgnoreAdded, String> {
    let file = RepoPath::from(".gitignore");
    let added = worktree_file::update(workdir, &file, MAX_GITIGNORE, |old| {
        gitignore::appended(old, line)
    })?;
    Ok(IgnoreAdded {
        pattern: sanitize(line),
        added,
    })
}

/// Where a running remote operation waits for the user's answer.
type AnswerSlot = Arc<Mutex<Option<std::sync::mpsc::Sender<Option<Secret>>>>>;

/// The askpass bridge for one remote operation, wired to the loop: each
/// question goes out as an intent, and its answer comes back through the
/// runtime's slot. Without one (not on unix, or it would not start) `env`
/// is `None` and git runs closed to questions.
struct Bridge {
    #[cfg(unix)]
    server: Option<crate::git::askpass::AskpassServer>,
    slot: AnswerSlot,
    env: Option<AskpassEnv>,
}

impl Bridge {
    #[cfg(unix)]
    fn open(tag: Tag, tx: UnboundedSender<AppIntent>, slot: AnswerSlot) -> Self {
        use crate::git::askpass::{AskpassServer, Question};

        let (answer_tx, answer_rx) = std::sync::mpsc::channel();
        *lock(&slot) = Some(answer_tx);
        let answers = Mutex::new(answer_rx);
        let server = std::env::current_exe().ok().and_then(|program| {
            let server = AskpassServer::start(move |question| match question {
                Question::Notice(text) => {
                    let _ = tx.send(AppIntent::RemoteNotice { tag, text });
                    None
                }
                Question::Ask { prompt, confirm } => {
                    let asked = AppIntent::CredentialAsked {
                        tag,
                        prompt,
                        confirm,
                    };
                    tx.send(asked).ok()?;
                    // An error means the operation ended: cancelled.
                    lock(&answers).recv().ok().flatten()
                }
            })
            .ok()?;
            Some((program, server))
        });
        let env = server.as_ref().map(|(program, server)| AskpassEnv {
            program: program.clone(),
            socket: server.socket().to_path_buf(),
            git_askpass: true,
        });
        Self {
            server: server.map(|(_, server)| server),
            slot,
            env,
        }
    }

    #[cfg(not(unix))]
    fn open(_tag: Tag, _tx: UnboundedSender<AppIntent>, slot: AnswerSlot) -> Self {
        Self { slot, env: None }
    }
}

impl Drop for Bridge {
    /// Hang up on a question still open, then drop the answer channel, so
    /// the server's thread stops waiting too.
    fn drop(&mut self) {
        #[cfg(unix)]
        drop(self.server.take());
        *lock(&self.slot) = None;
    }
}

/// A poisoned lock only means another thread panicked; the value is fine.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use std::io;

    use tokio::sync::mpsc;

    use super::*;

    fn forge_tag() -> Tag {
        Tag {
            generation: 0,
            seq: 11,
            user: false,
        }
    }

    #[tokio::test]
    async fn a_pull_request_load_with_no_hosts_runs_nothing_and_writes_nothing() {
        use crate::forge::github::FakeGh;
        use crate::git::recording::CommandSink;

        let tmp = crate::git::fake::TempDir::new("forge-effect-off");
        let cache_dir = tmp.path().join("forge");
        let git = Arc::new(FakeRunner::new());
        let gh = Arc::new(FakeGh::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(git.clone(), tx).with_forge(Some(ForgeContext {
            gh: gh.clone(),
            sink: CommandSink::new(),
            cache_dir: Some(cache_dir.clone()),
        }));
        let load = |hosts| Effect::LoadPullRequests {
            tag: forge_tag(),
            hosts,
            branches: vec!["main".into()],
            refresh: true,
            known: None,
            limited: vec![],
        };
        runtime.spawn(load(vec![]));
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::ForgeRepoNone { tag: forge_tag() })
        );
        // Without a forge client, even named hosts reach nobody.
        let (tx, mut rx) = mpsc::unbounded_channel();
        let bare = Runtime::new(git.clone(), tx);
        bare.spawn(load(vec![ForgeHost::parse("github.com").unwrap()]));
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::ForgeRepoNone { tag: forge_tag() })
        );
        git.assert_done();
        assert_eq!(gh.calls(), []);
        assert!(!cache_dir.exists());
    }

    #[tokio::test]
    async fn each_gh_call_is_logged_with_its_own_id_beside_git_commands() {
        use crate::forge::github::fixtures::{PROBE_200, STATES};
        use crate::forge::github::{FakeGh, answer};
        use crate::git::recording::{CommandEvent, CommandSink, RecordingRunner};

        let tmp = crate::git::fake::TempDir::new("forge-effect-log");
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = CommandSink::new();
        let keep = Arc::clone(&events);
        sink.connect(move |ev| keep.lock().unwrap().push(ev));
        let fake = Arc::new(
            FakeRunner::new()
                .expect(&["remote"], ok(b"origin\n"))
                .expect(
                    &["config", "-z", "--get", "remote.origin.url"],
                    ok(b"git@github.com:acme/widgets.git\0"),
                ),
        );
        let git = Arc::new(RecordingRunner::new(fake.clone(), sink.clone()));
        let gh = Arc::new(
            FakeGh::new()
                .reply(answer(PROBE_200))
                .reply(answer(STATES))
                .reply(answer(STATES)),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(git, tx).with_forge(Some(ForgeContext {
            gh: gh.clone(),
            sink,
            cache_dir: Some(tmp.path().join("forge")),
        }));
        let branches: Vec<BranchName> = (0..60).map(|i| format!("b{i}").as_str().into()).collect();
        runtime.spawn(Effect::LoadPullRequests {
            tag: forge_tag(),
            hosts: vec![ForgeHost::parse("github.com").unwrap()],
            branches,
            refresh: true,
            known: None,
            limited: vec![],
        });
        assert!(matches!(
            rx.recv().await,
            Some(AppIntent::PullRequestsCached { badges: None, .. })
        ));
        assert!(matches!(
            rx.recv().await,
            Some(AppIntent::PullRequestsFetched { result: Ok(_), .. })
        ));
        fake.assert_done();
        assert_eq!(gh.calls().len(), 3);

        let events = events.lock().unwrap();
        let started: Vec<(u64, &str)> = events
            .iter()
            .filter_map(|e| match e {
                CommandEvent::Started { id, line, .. } => Some((*id, line.as_str())),
                _ => None,
            })
            .collect();
        let probe_line = "gh api --include --hostname github.com \
            -H \"Accept: application/vnd.github+json\" \
            repos/acme/widgets/pulls?state=all&sort=updated&direction=desc&per_page=1";
        let gh_line = "gh api graphql --include --hostname github.com --input -";
        let lines: Vec<&str> = started.iter().map(|(_, line)| *line).collect();
        assert_eq!(
            lines,
            [
                "git remote",
                "git config -z --get remote.origin.url",
                probe_line,
                gh_line,
                gh_line
            ]
        );
        let mut ids: Vec<u64> = started.iter().map(|(id, _)| *id).collect();
        ids.dedup();
        assert_eq!(ids.len(), 5, "{events:?}");
        // One finish per start, with the same id.
        for (id, _) in &started {
            let finished = events
                .iter()
                .filter(|e| matches!(e, CommandEvent::Finished { id: f, .. } if f == id))
                .count();
            assert_eq!(finished, 1, "{id}");
        }
    }
    use crate::git::FileEntry;
    use crate::git::diff::DiffSide;
    use crate::git::fake::{FakeChild, FakeRunner, fail, ok};
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
    async fn the_message_read_is_logged_and_a_clipboard_copy_is_not() {
        use crate::clipboard::RecordingSink;
        use crate::git::recording::{CommandEvent, CommandOutcome, CommandSink, RecordingRunner};

        let oid = CommitId::parse("0123abcd").unwrap();
        let fake = Arc::new(FakeRunner::new().expect(
            &["log", "-1", "--format=%B", "0123abcd", "--"],
            ok(b"subj\n"),
        ));
        let sink = CommandSink::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&events);
        sink.connect(move |event| seen.lock().unwrap().push(event));
        let runner = Arc::new(RecordingRunner::new(fake.clone(), sink));
        let clipboard = Arc::new(RecordingSink::default());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner, tx).with_clipboard(clipboard.clone());

        assert_eq!(runtime.copy_to_clipboard("clip secret"), Ok(()));
        assert_eq!(clipboard.copies().len(), 1);
        assert!(events.lock().unwrap().is_empty(), "a copy runs no command");

        let tag = Tag {
            generation: 0,
            seq: 1,
            user: true,
        };
        runtime.spawn(Effect::LoadCommitMessage {
            tag,
            oid: oid.clone(),
        });
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::CommitMessageLoaded {
                tag,
                oid,
                result: Ok("subj".into()),
            })
        );
        fake.assert_done();
        let events = events.lock().unwrap().clone();
        let [
            CommandEvent::Started { line, .. },
            CommandEvent::Finished { outcome, .. },
        ] = events.as_slice()
        else {
            panic!("expected one started and finished command, got {events:?}");
        };
        assert!(line.ends_with("log -1 --format=%B 0123abcd --"), "{line:?}");
        assert!(!line.contains("clip"), "{line:?}");
        assert_eq!(outcome, &CommandOutcome::Ok);
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
                        bisect: None,
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
    async fn check_left_behind_answers_with_the_op_head_and_ids() {
        let head = CommitId::parse("aa11").unwrap();
        let target = CommitId::parse("bb22").unwrap();
        let op = GitOp::CheckoutCommit(target.clone());
        let runner = Arc::new(FakeRunner::new().expect(
            &[
                "rev-list",
                "aa11",
                "--not",
                "bb22",
                "--branches",
                "--tags",
                "--remotes",
                "--",
            ],
            ok(b"aa11\ncc33\n"),
        ));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 1,
            seq: 4,
            user: true,
        };

        runtime.spawn(Effect::CheckLeftBehind {
            tag,
            op: op.clone(),
            head: head.clone(),
            target,
        });

        assert_eq!(
            rx.recv().await,
            Some(AppIntent::LeftBehindChecked {
                tag,
                op,
                head,
                result: Ok(vec![
                    CommitId::parse("aa11").unwrap(),
                    CommitId::parse("cc33").unwrap()
                ]),
            })
        );
        runner.assert_done();
    }

    #[tokio::test]
    async fn a_panicking_left_behind_check_still_reports_back() {
        // No expectations queued, so the fake panics on the call.
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(FakeRunner::new()), tx);
        let tag = Tag {
            generation: 0,
            seq: 2,
            user: true,
        };
        let id = CommitId::parse("aa").unwrap();
        runtime.spawn(Effect::CheckLeftBehind {
            tag,
            op: GitOp::CheckoutCommit(id.clone()),
            head: id.clone(),
            target: id,
        });
        match rx.recv().await {
            Some(AppIntent::LeftBehindChecked {
                tag: got,
                result: Err(message),
                ..
            }) => {
                assert_eq!(got, tag);
                assert!(message.contains("git rev-list crashed"), "{message}");
            }
            other => panic!("expected a failed LeftBehindChecked, got {other:?}"),
        }
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

    fn commit_op() -> GitOp {
        GitOp::Commit {
            message: Some("m\n".into()),
            stage_all: false,
            amend: false,
            no_verify: false,
        }
    }

    /// A commit run as an effect stays detached: the fake has no attached
    /// run expected, so it would panic on one.
    #[tokio::test]
    async fn a_commit_run_as_an_effect_never_takes_the_terminal() {
        let runner = Arc::new(FakeRunner::new().expect(&["commit", "-F", "-"], ok(b"")));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 1,
            seq: 4,
            user: true,
        };

        runtime.spawn(Effect::RunOp {
            tag,
            op: commit_op(),
        });

        assert_eq!(
            rx.recv().await,
            Some(AppIntent::OpFinished {
                tag,
                op: commit_op(),
                result: Ok(()),
            })
        );
        runner.assert_done();
    }

    #[tokio::test]
    async fn probe_commit_reports_the_facts_with_the_same_tag_and_op() {
        let runner = Arc::new(
            FakeRunner::new()
                .expect(
                    &["config", "--type=bool", "--get", "commit.gpgsign"],
                    ok(b"true\n"),
                )
                .expect(&["rev-parse", "--git-path", "hooks"], ok(b"/nowhere\n")),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 1,
            seq: 6,
            user: true,
        };

        runtime.spawn(Effect::ProbeCommit {
            tag,
            op: commit_op(),
        });

        assert_eq!(
            rx.recv().await,
            Some(AppIntent::CommitProbed {
                tag,
                op: commit_op(),
                facts: Some(crate::git::prompts::PromptFacts {
                    signs: true,
                    hooks: vec![],
                }),
            })
        );
        runner.assert_done();
    }

    #[tokio::test]
    async fn a_failed_or_panicking_probe_reports_no_facts() {
        let tag = Tag {
            generation: 0,
            seq: 1,
            user: true,
        };
        let failing = FakeRunner::new().expect(
            &["config", "--type=bool", "--get", "commit.gpgsign"],
            fail("fatal: bad config\n"),
        );
        // No expectations at all: the fake panics on the first read.
        for runner in [failing, FakeRunner::new()] {
            let (tx, mut rx) = mpsc::unbounded_channel();
            let runtime = Runtime::new(Arc::new(runner), tx);
            runtime.spawn(Effect::ProbeCommit {
                tag,
                op: commit_op(),
            });
            assert_eq!(
                rx.recv().await,
                Some(AppIntent::CommitProbed {
                    tag,
                    op: commit_op(),
                    facts: None,
                })
            );
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
            commit: None,
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
    async fn branch_loads_report_back_with_the_same_tag_and_name() {
        let runner = Arc::new(
            FakeRunner::new()
                .expect(
                    &[
                        "for-each-ref",
                        "--format=%(refname)%00%(HEAD)%00%(upstream:short)%00\
                         %(upstream:track,nobracket)%00%(committerdate:unix)",
                        "refs/heads",
                    ],
                    ok(b""),
                )
                .expect(
                    &[
                        "log",
                        "-n",
                        "200",
                        "--date=short",
                        "--format=%h%x1f%ad%x1f%an%x1f%s",
                        "refs/heads/main",
                        "--",
                    ],
                    ok(b"abc\x1f2024-01-02\x1fAnn\x1fWork\n"),
                ),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 1,
            seq: 4,
            user: true,
        };

        runtime.spawn(Effect::LoadBranches { tag });
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::BranchesLoaded {
                tag,
                result: Ok(vec![]),
            })
        );
        runtime.spawn(Effect::LoadBranchLog {
            tag,
            refname: b"refs/heads/main".to_vec(),
        });
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::BranchLogLoaded {
                tag,
                refname: b"refs/heads/main".to_vec(),
                result: Ok(vec![crate::git::branches::LogRow {
                    short_oid: "abc".into(),
                    date: "2024-01-02".into(),
                    author: "Ann".into(),
                    subject: "Work".into(),
                }]),
            })
        );
        runner.assert_done();
    }

    #[tokio::test]
    async fn a_panicking_branch_load_still_reports_back() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(HostilePanic), tx);
        let tag = Tag {
            generation: 0,
            seq: 2,
            user: true,
        };
        runtime.spawn(Effect::LoadBranches { tag });
        assert!(matches!(
            rx.recv().await,
            Some(AppIntent::BranchesLoaded { result: Err(_), .. })
        ));
        runtime.spawn(Effect::LoadBranchLog {
            tag,
            refname: b"refs/heads/x".to_vec(),
        });
        assert!(matches!(
            rx.recv().await,
            Some(AppIntent::BranchLogLoaded { result: Err(_), .. })
        ));
        runtime.spawn(Effect::LoadRemotes { tag });
        assert!(matches!(
            rx.recv().await,
            Some(AppIntent::RemotesLoaded { result: Err(_), .. })
        ));
        runtime.spawn(Effect::LoadTags { tag });
        assert!(matches!(
            rx.recv().await,
            Some(AppIntent::TagsLoaded { result: Err(_), .. })
        ));
    }

    #[tokio::test]
    async fn a_tags_load_reports_back_with_the_same_tag() {
        let oid = "1".repeat(40);
        let record = format!("refs/tags/v1\0commit\0{oid}\0\0\0\0d\0s\n");
        let list = crate::git::tags::list_command();
        let argv: Vec<String> = list
            .args()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let runner = Arc::new(FakeRunner::new().expect(&argv, ok(record.as_bytes())));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 1,
            seq: 6,
            user: false,
        };

        runtime.spawn(Effect::LoadTags { tag });

        match rx.recv().await {
            Some(AppIntent::TagsLoaded {
                tag: got,
                result: Ok(tags),
            }) => {
                assert_eq!(got, tag);
                assert_eq!(tags.len(), 1);
                assert_eq!(tags[0].name.display(), "v1");
            }
            other => panic!("expected TagsLoaded, got {other:?}"),
        }
        runner.assert_done();
    }

    #[tokio::test]
    async fn a_remotes_load_reports_back_with_the_same_tag() {
        let runner = Arc::new(
            FakeRunner::new()
                .expect(
                    &["config", "-z", "--get-regexp", r"^remote\..*\.url$"],
                    ok(b"remote.origin.url\n/srv/r\0"),
                )
                .expect(
                    &[
                        "for-each-ref",
                        "--format=%(refname)%00%(symref)",
                        "refs/remotes",
                    ],
                    ok(b"refs/remotes/origin/main\0\n"),
                ),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 1,
            seq: 5,
            user: false,
        };

        runtime.spawn(Effect::LoadRemotes { tag });

        match rx.recv().await {
            Some(AppIntent::RemotesLoaded {
                tag: got,
                result: Ok(remotes),
            }) => {
                assert_eq!(got, tag);
                assert_eq!(remotes.len(), 1);
                assert_eq!(remotes[0].branches[0].display(), "origin/main");
            }
            other => panic!("expected RemotesLoaded, got {other:?}"),
        }
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
                commit: None,
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
    async fn worktree_effects_report_back_with_the_same_tag() {
        let runner = Arc::new(
            FakeRunner::new()
                .expect(
                    &["worktree", "list", "--porcelain", "-z"],
                    ok(b"worktree /r\0bare\0\0"),
                )
                .expect(
                    &["worktree", "remove", "--", "/r/x"],
                    fail("fatal: '/r/x' is locked\n"),
                ),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 1,
            seq: 4,
            user: true,
        };

        runtime.spawn(Effect::LoadWorktrees { tag });
        match rx.recv().await {
            Some(AppIntent::WorktreesLoaded {
                tag: got,
                result: Ok(list),
            }) => {
                assert_eq!(got, tag);
                assert_eq!(list[0].path.display(), "/r");
            }
            other => panic!("expected WorktreesLoaded, got {other:?}"),
        }
        runtime.spawn(Effect::RemoveWorktree {
            tag,
            path: "/r/x".into(),
        });
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::WorktreeRemoved {
                tag,
                result: Err("fatal: '/r/x' is locked".into()),
            })
        );
        runner.assert_done();
    }

    #[tokio::test]
    async fn a_failing_worktree_add_still_reports_back() {
        // No workdir and no answers: the git-dir read fails, which must
        // still answer once.
        let runner = Arc::new(FakeRunner::new().expect(
            &["rev-parse", "--git-dir", "--git-common-dir"],
            fail("fatal: not a git repository\n"),
        ));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 0,
            seq: 1,
            user: true,
        };
        runtime.spawn(Effect::AddWorktree {
            tag,
            path: "../x".into(),
            branch: "feature".into(),
        });
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::WorktreeAdded {
                tag,
                result: Err("fatal: not a git repository".into()),
            })
        );
        runner.assert_done();
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
    async fn load_commits_reports_the_page_with_the_same_tag_skip_and_a_time() {
        let runner = Arc::new(FakeRunner::new().expect(
            &[
                "log",
                "--no-show-signature",
                "--no-color",
                "-z",
                "--format=%H%x1f%h%x1f%P%x1f%at%x1f%an%x1f%s",
                "--skip=200",
                "-n",
                "200",
                "abc",
                "--",
            ],
            ok(b"abc1\x1fabc\x1f\x1f7\x1fAnn\x1fsubj\0"),
        ));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);

        runtime.spawn(Effect::LoadCommits {
            tag: background(5),
            tip: Some("abc".into()),
            skip: 200,
            query: LogQuery::default(),
        });

        match rx.recv().await {
            Some(AppIntent::CommitsLoaded {
                tag,
                skip,
                result: Ok((page, now)),
            }) => {
                assert_eq!((tag, skip), (background(5), 200));
                assert_eq!(page.rows[0].subject, "subj");
                assert!(page.done);
                assert!(now > 1_600_000_000, "{now}");
            }
            other => panic!("expected a loaded page, got {other:?}"),
        }
        runner.assert_done();
    }

    #[tokio::test]
    async fn resolve_diff_reads_each_branch_through_its_full_ref() {
        let user = Tag {
            generation: 1,
            seq: 9,
            user: true,
        };
        let runner = Arc::new(
            FakeRunner::new()
                .expect(
                    &["rev-parse", "-q", "--verify", "refs/heads/main^{commit}"],
                    ok(b"aaa111\n"),
                )
                .expect(
                    &["rev-parse", "-q", "--verify", "refs/heads/topic^{commit}"],
                    ok(b"--output=x\n"),
                ),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let target = CommitId::parse("bbb222").unwrap();

        runtime.spawn(Effect::ResolveDiff {
            tag: user,
            base: DiffEnd::Branch(BranchName::from_bytes(b"main")),
            target: DiffEnd::Commit(target.clone()),
        });
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::DiffResolved {
                tag: user,
                result: Ok(CommitRange {
                    oid: target,
                    parent: CommitId::parse("aaa111"),
                }),
            })
        );

        // Output that is not hex is never taken for a commit.
        runtime.spawn(Effect::ResolveDiff {
            tag: user,
            base: DiffEnd::Commit(CommitId::parse("aaa111").unwrap()),
            target: DiffEnd::Branch(BranchName::from_bytes(b"topic")),
        });
        match rx.recv().await {
            Some(AppIntent::DiffResolved {
                tag,
                result: Err(message),
            }) => {
                assert_eq!(tag, user);
                assert!(message.contains("not a commit id"), "{message:?}");
            }
            other => panic!("expected a failed DiffResolved, got {other:?}"),
        }
        runner.assert_done();
    }

    #[tokio::test]
    async fn load_commits_passes_its_query_to_git() {
        let runner = Arc::new(FakeRunner::new().expect(
            &[
                "log",
                "--no-show-signature",
                "--no-color",
                "-z",
                "--format=%H%x1f%h%x1f%P%x1f%at%x1f%an%x1f%s",
                "--skip=0",
                "-n",
                "200",
                "--author=Ann",
                "--fixed-strings",
                "HEAD",
                "--",
            ],
            ok(b""),
        ));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);

        runtime.spawn(Effect::LoadCommits {
            tag: background(5),
            tip: None,
            skip: 0,
            query: LogQuery {
                path: None,
                author: Some("Ann".into()),
            },
        });

        assert!(matches!(
            rx.recv().await,
            Some(AppIntent::CommitsLoaded { result: Ok(_), .. })
        ));
        runner.assert_done();
    }

    #[tokio::test]
    async fn panicking_commit_and_marker_loads_still_report_back() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(HostilePanic), tx);

        runtime.spawn(Effect::LoadCommits {
            tag: background(6),
            tip: None,
            skip: 0,
            query: LogQuery::default(),
        });
        match rx.recv().await {
            Some(AppIntent::CommitsLoaded {
                tag,
                result: Err(message),
                ..
            }) => {
                assert_eq!(tag, background(6));
                assert!(!message.contains(char::is_control), "{message:?}");
            }
            other => panic!("expected a failed CommitsLoaded, got {other:?}"),
        }

        runtime.spawn(Effect::LoadMarkers { tag: background(7) });
        match rx.recv().await {
            Some(AppIntent::MarkersLoaded {
                tag,
                result: Err(message),
            }) => {
                assert_eq!(tag, background(7));
                assert!(!message.contains(char::is_control), "{message:?}");
            }
            other => panic!("expected a failed MarkersLoaded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn stash_loads_report_back_with_the_same_tag_even_on_a_panic() {
        use crate::git::commit_files::CommitId;
        let tag = Tag {
            generation: 3,
            seq: 9,
            user: true,
        };
        let runner = Arc::new(
            FakeRunner::new()
                .expect(
                    &["stash", "list", "-z", "--format=%H%x1f%P%x1f%gs"],
                    ok(b"aa\x1fbb cc\x1fOn main: wip\0"),
                )
                .expect(
                    &[
                        "diff-tree",
                        "-r",
                        "-z",
                        "--no-commit-id",
                        "--name-status",
                        "-M",
                        "--root",
                        "bb",
                        "aa",
                        "--",
                    ],
                    ok(b"M\0a.rs\0"),
                ),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);

        runtime.spawn(Effect::LoadStashes { tag });
        let Some(AppIntent::StashesLoaded {
            tag: got,
            result: Ok(entries),
        }) = rx.recv().await
        else {
            panic!("expected StashesLoaded");
        };
        assert_eq!(got, tag);
        assert_eq!(entries[0].oid, CommitId::parse("aa").unwrap());

        runtime.spawn(Effect::LoadStashFiles {
            tag,
            entry: entries[0].clone(),
        });
        let Some(AppIntent::StashFilesLoaded {
            tag: got,
            result: Ok(files),
        }) = rx.recv().await
        else {
            panic!("expected StashFilesLoaded");
        };
        assert_eq!(got, tag);
        assert_eq!(files[0].file.path, "a.rs".into());
        runner.assert_done();

        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(HostilePanic), tx);
        runtime.spawn(Effect::LoadStashes { tag });
        match rx.recv().await {
            Some(AppIntent::StashesLoaded {
                tag: got,
                result: Err(message),
            }) => {
                assert_eq!(got, tag);
                assert!(
                    message.starts_with("git stash list crashed: "),
                    "{message:?}"
                );
            }
            other => panic!("expected a failed StashesLoaded, got {other:?}"),
        }
        runtime.spawn(Effect::LoadStashFiles {
            tag,
            entry: entries[0].clone(),
        });
        match rx.recv().await {
            Some(AppIntent::StashFilesLoaded {
                tag: got,
                result: Err(message),
            }) => {
                assert_eq!(got, tag);
                assert!(!message.contains(char::is_control), "{message:?}");
            }
            other => panic!("expected a failed StashFilesLoaded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn load_commit_files_reports_back_with_the_same_tag_even_on_a_panic() {
        use crate::git::commit_files::{CommitFile, CommitId};
        let range = CommitRange {
            oid: CommitId::parse("abc").unwrap(),
            parent: None,
        };
        let runner = Arc::new(FakeRunner::new().expect(
            &[
                "diff-tree",
                "-r",
                "-z",
                "--no-commit-id",
                "--name-status",
                "-M",
                "--root",
                "abc",
                "--",
            ],
            ok(b"A\0new\0"),
        ));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 2,
            seq: 8,
            user: true,
        };

        runtime.spawn(Effect::LoadCommitFiles {
            tag,
            range: range.clone(),
        });

        let new = CommitFile {
            status: 'A',
            path: "new".into(),
            orig_path: None,
        };
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::CommitFilesLoaded {
                tag,
                result: Ok(vec![new])
            })
        );
        runner.assert_done();

        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(HostilePanic), tx);
        runtime.spawn(Effect::LoadCommitFiles { tag, range });
        match rx.recv().await {
            Some(AppIntent::CommitFilesLoaded {
                tag: got,
                result: Err(message),
            }) => {
                assert_eq!(got, tag);
                assert!(
                    message.starts_with("git diff-tree crashed: "),
                    "{message:?}"
                );
                assert!(!message.contains(char::is_control), "{message:?}");
            }
            other => panic!("expected a failed CommitFilesLoaded, got {other:?}"),
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
                commit: None,
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

    #[cfg(unix)]
    #[tokio::test]
    async fn run_command_reports_a_failure_with_its_status_and_output() {
        let tmp = crate::git::fake::TempDir::new("effect-command");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runner = Arc::new(crate::git::CliRunner::new(tmp.path()));
        let runtime = Runtime::new(runner, tx);
        let run = |argv: &[&str], output| Effect::RunCommand {
            title: "Try".into(),
            argv: argv.iter().map(OsString::from).collect(),
            output,
        };

        runtime.spawn(run(&["sh", "-c", "printf hi; exit 3"], OutputMode::Popup));
        match rx.recv().await {
            Some(AppIntent::CommandFinished {
                title,
                output: OutputMode::Popup,
                result: Err(CommandFailure::Exited(e)),
            }) => {
                assert_eq!(title, "Try");
                assert!(e.contains('3') && e.contains("hi"), "{e}");
            }
            other => panic!("expected a failed CommandFinished, got {other:?}"),
        }

        // `none` drops the output of a success; `popup` keeps it.
        runtime.spawn(run(&["sh", "-c", "printf hi"], OutputMode::None));
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::CommandFinished {
                title: "Try".into(),
                output: OutputMode::None,
                result: Ok(String::new()),
            })
        );
        runtime.spawn(run(&["sh", "-c", "pwd"], OutputMode::Popup));
        match rx.recv().await {
            Some(AppIntent::CommandFinished {
                result: Ok(cwd), ..
            }) => assert_eq!(
                std::fs::canonicalize(cwd).unwrap(),
                std::fs::canonicalize(tmp.path()).unwrap()
            ),
            other => panic!("expected the workdir, got {other:?}"),
        }

        // A program that cannot start still answers, once.
        runtime.spawn(run(&["kika-no-such-program"], OutputMode::None));
        match rx.recv().await {
            Some(AppIntent::CommandFinished {
                result: Err(CommandFailure::Launch(e)),
                ..
            }) => {
                assert!(e.starts_with("Could not start"), "{e}")
            }
            other => panic!("expected a start failure, got {other:?}"),
        }
        assert!(rx.try_recv().is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelling_stops_a_hung_command_which_answers_once() {
        let tmp = crate::git::fake::TempDir::new("effect-cancel");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runner = Arc::new(crate::git::CliRunner::new(tmp.path()));
        let runtime = Runtime::new(runner, tx);
        let started = std::time::Instant::now();

        runtime.spawn(Effect::RunCommand {
            title: "Hang".into(),
            argv: ["sh", "-c", "sleep 30"].map(OsString::from).to_vec(),
            output: OutputMode::Popup,
        });
        runtime.spawn(Effect::CancelCommand);

        assert_eq!(
            rx.recv().await,
            Some(AppIntent::CommandFinished {
                title: "Hang".into(),
                output: OutputMode::Popup,
                result: Err(CommandFailure::Cancelled),
            })
        );
        assert!(started.elapsed() < Duration::from_secs(10));
        // A second cancel finds nothing to stop and sends nothing.
        runtime.spawn(Effect::CancelCommand);
        tokio::task::yield_now().await;
        assert!(rx.try_recv().is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn render_diff_feeds_the_plain_diff_to_the_renderer_and_parses_it() {
        let runner = Arc::new(FakeRunner::new().expect(
            &["diff", "--no-color", "--no-ext-diff", "-U3", "--", "a.rs"],
            ok(b"\x1b[31m-a\x1b[0m\n+b\n"),
        ));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 1,
            seq: 8,
            user: true,
        };

        runtime.spawn(Effect::RenderDiff {
            tag,
            request: DiffRequest {
                path: "a.rs".into(),
                orig_path: None,
                side: DiffSide::Worktree,
                options: Default::default(),
                commit: None,
            },
            renderer: ExternalRenderer {
                label: "cat".into(),
                argv: vec!["cat".into()],
                kind: crate::external_diff::RendererKind::Pager,
            },
            cols: 40,
        });

        match rx.recv().await {
            Some(AppIntent::DiffRendered {
                tag: got,
                result: Ok(rows),
            }) => {
                assert_eq!(got, tag);
                let texts: Vec<String> = rows.iter().map(|r| r.text()).collect();
                assert_eq!(texts, ["-a", "+b"]);
            }
            other => panic!("expected rendered rows, got {other:?}"),
        }
        runner.assert_done();
    }

    /// Run `RenderDiff` with tool `argv` on `request` and take its rows.
    #[cfg(unix)]
    async fn tool_rows(
        runner: Arc<dyn GitRunner>,
        request: DiffRequest,
        argv: &[&str],
    ) -> Vec<String> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner, tx);
        let tag = Tag {
            generation: 1,
            seq: 3,
            user: true,
        };
        runtime.spawn(Effect::RenderDiff {
            tag,
            request,
            renderer: ExternalRenderer {
                label: argv[0].into(),
                argv: argv.iter().map(OsString::from).collect(),
                kind: crate::external_diff::RendererKind::Tool,
            },
            cols: 40,
        });
        match rx.recv().await {
            Some(AppIntent::DiffRendered {
                tag: got,
                result: Ok(rows),
            }) => {
                assert_eq!(got, tag);
                assert!(rx.try_recv().is_err(), "one answer only");
                rows.iter().map(|r| r.text()).collect()
            }
            other => panic!("expected rendered rows, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn render_diff_runs_a_tool_on_both_sides_of_a_worktree_change() {
        use crate::git::fake::{TempDir, git};
        let tmp = TempDir::new("effect-tool");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        std::fs::write(dir.join("a.txt"), "old \x1b]0;title\x07one\n").unwrap();
        git(dir, &["add", "a.txt"]);
        std::fs::write(dir.join("a.txt"), "new \x1b[2J\x08two\n").unwrap();
        let runner: Arc<dyn GitRunner> = Arc::new(crate::git::CliRunner::isolated(dir));
        let request = DiffRequest {
            path: "a.txt".into(),
            orig_path: None,
            side: DiffSide::Worktree,
            options: Default::default(),
            commit: None,
        };

        let rows = tool_rows(Arc::clone(&runner), request.clone(), &["cat"]).await;
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(
            rows[0].starts_with("old ") && rows[0].ends_with("one"),
            "{rows:?}"
        );
        assert!(
            rows[1].starts_with("new ") && rows[1].ends_with("two"),
            "{rows:?}"
        );
        for row in &rows {
            assert!(!row.chars().any(char::is_control), "{row:?}");
        }

        // The tool's arguments name the temp files, gone once it is done.
        let rows = tool_rows(runner, request, &["echo"]).await;
        let args: Vec<&str> = rows[0].split(' ').collect();
        assert_eq!(args.len(), 3, "{rows:?}");
        assert_eq!(args[0], "--");
        let old = std::path::Path::new(args[1]);
        assert_eq!(old.file_name().unwrap(), "old.txt");
        assert!(!old.parent().unwrap().exists(), "{old:?}");
    }

    #[tokio::test]
    async fn find_renderers_offers_the_configured_pager_then_the_global_git_ones() {
        let runner = Arc::new(
            FakeRunner::new()
                .expect(
                    &["config", "--global", "--get", "pager.diff"],
                    ok(b"delta\n"),
                )
                .expect(&["config", "--global", "--get", "core.pager"], ok(b"")),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx).with_diff_pager(Some("riff".into()));

        runtime.spawn(Effect::FindRenderers);

        match rx.recv().await {
            Some(AppIntent::RenderersFound(found)) => {
                let labels: Vec<&str> = found.iter().map(|r| r.label.as_str()).collect();
                assert_eq!(labels, ["riff", "delta"]);
            }
            other => panic!("expected renderers, got {other:?}"),
        }
        runner.assert_done();
    }

    #[tokio::test]
    async fn the_emoji_policy_reaches_blame_and_the_blamed_commit() {
        let family = "👨\u{200D}👩";
        let oid = "a".repeat(40);
        let blame = format!("{oid} 1 1 1\nauthor Ann\n\t{family}\n");
        let show = format!("commit {oid}\n\ndiff --git a/a b/a\n@@ -1 +1 @@\n+{family}\n");
        for (emoji, text) in [
            (EmojiZwj::Keep, family.to_string()),
            (EmojiZwj::Escape, "👨<U+200D>👩".to_string()),
        ] {
            let runner = Arc::new(
                FakeRunner::new()
                    .expect(&["blame", "--porcelain", "--", "a"], ok(blame.as_bytes()))
                    .expect(
                        &[
                            "show",
                            "--no-color",
                            "--no-ext-diff",
                            "--pretty=medium",
                            "--date=iso",
                            &oid,
                            "--",
                        ],
                        ok(show.as_bytes()),
                    ),
            );
            let (tx, mut rx) = mpsc::unbounded_channel();
            let runtime = Runtime::new(runner.clone(), tx).with_emoji_zwj(emoji);
            let tag = Tag {
                generation: 0,
                seq: 1,
                user: false,
            };

            runtime.spawn(Effect::LoadBlame {
                tag,
                path: "a".into(),
            });
            match rx.recv().await {
                Some(AppIntent::BlameLoaded {
                    result: Ok(lines), ..
                }) => assert_eq!(lines[0].text, text, "{emoji:?}"),
                other => panic!("expected a blame, got {other:?}"),
            }
            runtime.spawn(Effect::LoadCommit {
                tag,
                oid: oid.clone(),
            });
            match rx.recv().await {
                Some(AppIntent::CommitLoaded {
                    result: Ok(lines), ..
                }) => assert_eq!(lines.last().unwrap().text, format!("+{text}"), "{emoji:?}"),
                other => panic!("expected a commit, got {other:?}"),
            }
            runner.assert_done();
        }
    }

    #[tokio::test]
    async fn a_remote_op_reports_back_with_the_same_tag_and_op() {
        // With the bridge (unix), the user's own askpass is looked for first.
        let fake = if cfg!(unix) {
            FakeRunner::new().expect(&["config", "--get", "core.askPass"], fail(""))
        } else {
            FakeRunner::new()
        };
        let runner = Arc::new(fake.expect(&["push"], ok(b"")));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 1,
            seq: 3,
            user: true,
        };
        let op = RemoteOp::Push {
            force_with_lease: false,
        };

        runtime.spawn(Effect::Remote {
            tag,
            op: op.clone(),
        });

        assert_eq!(
            rx.recv().await,
            Some(AppIntent::RemoteFinished {
                tag,
                op,
                result: Ok(()),
            })
        );
        runner.assert_done();
    }

    #[tokio::test]
    async fn a_panicking_remote_op_still_reports_back() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(HostilePanic), tx);
        let tag = Tag {
            generation: 0,
            seq: 4,
            user: true,
        };

        runtime.spawn(Effect::Remote {
            tag,
            op: RemoteOp::Fetch,
        });

        match rx.recv().await {
            Some(AppIntent::RemoteFinished {
                tag: got,
                op: RemoteOp::Fetch,
                result: Err(RemoteFailure::Failed(message)),
            }) => {
                assert_eq!(got, tag);
                assert!(message.starts_with("git crashed: "), "{message:?}");
                assert!(!message.contains(char::is_control), "{message:?}");
            }
            other => panic!("expected a failed RemoteFinished, got {other:?}"),
        }
    }

    /// Stands in for git that needs a password: it runs the askpass
    /// client the env names, as git would, and succeeds only on `tok`.
    #[cfg(unix)]
    struct AskingGit;

    #[cfg(unix)]
    impl GitRunner for AskingGit {
        fn run(&self, cmd: &GitCommand) -> io::Result<GitOutput> {
            if cmd.args()[0] == "config" {
                return Ok(fail(""));
            }
            let socket = cmd
                .envs()
                .iter()
                .find(|(k, _)| k == crate::git::askpass::SOCKET_ENV)
                .map(|(_, v)| PathBuf::from(v))
                .expect("the bridge is in the env");
            let mut out = Vec::new();
            crate::git::askpass::client(
                &socket,
                std::ffi::OsStr::new("Password for 'https://h': "),
                None,
                &mut out,
            );
            Ok(if out == b"tok\n" {
                ok(b"")
            } else {
                fail("fatal: Authentication failed for 'https://h/'")
            })
        }

        fn spawn(&self, _: &GitCommand) -> io::Result<GitChild> {
            unreachable!("remote ops are run, not spawned")
        }
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_question_from_git_is_asked_and_the_answer_reaches_it() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(AskingGit), tx);
        let tag = Tag {
            generation: 0,
            seq: 5,
            user: true,
        };

        for (answer, succeeded) in [(Some(Secret::new("tok")), true), (None, false)] {
            runtime.spawn(Effect::Remote {
                tag,
                op: RemoteOp::Fetch,
            });
            assert_eq!(
                rx.recv().await,
                Some(AppIntent::CredentialAsked {
                    tag,
                    prompt: "Password for 'https://h':".into(),
                    confirm: false,
                })
            );
            runtime.spawn(Effect::AnswerCredential(answer));
            match rx.recv().await {
                Some(AppIntent::RemoteFinished { result, .. }) => {
                    assert_eq!(result.is_ok(), succeeded, "{result:?}")
                }
                other => panic!("expected RemoteFinished, got {other:?}"),
            }
        }
        // With nothing running, an answer goes nowhere.
        runtime.spawn(Effect::AnswerCredential(None));
    }

    /// A `rebase-merge` dir in a temp git dir, with `todo` and `done`.
    fn rebase_dir(tmp: &crate::git::fake::TempDir, todo: &[u8], done: &[u8]) -> PathBuf {
        let dir = tmp.path().join("rebase-merge");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("git-rebase-todo"), todo).unwrap();
        std::fs::write(dir.join("done"), done).unwrap();
        dir.join("git-rebase-todo")
    }

    const TODO: &[u8] = b"pick 1111 one\nupdate-ref refs/heads/x\npick 2222 two\n# help\n";

    #[test]
    fn a_todo_write_checks_the_file_and_the_edit_before_writing() {
        let tmp = crate::git::fake::TempDir::new("effect-todo");
        let path = rebase_dir(&tmp, TODO, b"pick 0000 z\n");
        for bad in [
            &b"pick 1111 one\nupdate-ref refs/heads/x\npick 2222 two\npick 3333 more\n"[..],
            b"pick 2222 two\nupdate-ref refs/heads/x\npick 1111 one\n",
            b"pick 1111 one\nupdate-ref refs/heads/x\npick 2222 two\nexec rm -rf /\n",
            b"pick 1111 one\npick 2222 two\n",
        ] {
            assert!(write_rebase_todo(&path, TODO, bad).is_err(), "{bad:?}");
            assert_eq!(std::fs::read(&path).unwrap(), TODO);
        }
        // A line git rejects is not a comment: a list holding one is not
        // edited, whatever the edit.
        for odd in [&b"\xef\xbb\xbf"[..], b"-", b"\x0b"] {
            let mut listed = TODO.to_vec();
            listed.extend_from_slice(odd);
            listed.extend_from_slice(b"pick 3333 three\n");
            std::fs::write(&path, &listed).unwrap();
            let edit = b"pick 1111 one\nupdate-ref refs/heads/x\ndrop 2222 two\n";
            assert!(write_rebase_todo(&path, &listed, edit).is_err(), "{odd:?}");
            assert_eq!(std::fs::read(&path).unwrap(), listed);
        }
        // Git stops at a NUL; a list holding one is not edited.
        let nul = b"pick 1111 one\nupdate-ref refs/heads/x\npick 2222 two\0\n";
        std::fs::write(&path, nul).unwrap();
        let moved = b"pick 1111 one\nupdate-ref refs/heads/x\npick 2222 two\n";
        assert!(write_rebase_todo(&path, nul, moved).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), nul);
        std::fs::write(&path, TODO).unwrap();
        let other = tmp.path().join("git-rebase-todo");
        std::fs::write(&other, TODO).unwrap();
        let edit = b"pick 1111 one\nbreak\nupdate-ref refs/heads/x\ndrop 2222 two\n";
        assert!(write_rebase_todo(&other, TODO, edit).is_err());
        assert_eq!(
            // The same commands, but not the bytes on disk.
            write_rebase_todo(&path, &TODO[..TODO.len() - 7], edit),
            Err(crate::worktree_file::CHANGED_ON_DISK.to_string())
        );
        assert_eq!(write_rebase_todo(&path, TODO, edit), Ok(()));
        assert_eq!(std::fs::read(&path).unwrap(), edit);
    }

    #[test]
    fn a_todo_write_refuses_a_first_fold_only_when_nothing_is_done() {
        let tmp = crate::git::fake::TempDir::new("effect-todo");
        let todo = b"pick 1111 one\npick 2222 two\n";
        let path = rebase_dir(&tmp, todo, b"");
        let fold = b"fixup 1111 one\npick 2222 two\n";
        assert!(write_rebase_todo(&path, todo, fold).is_err());
        std::fs::write(path.with_file_name("done"), "pick 0000 z\n").unwrap();
        assert_eq!(write_rebase_todo(&path, todo, fold), Ok(()));
    }

    #[test]
    fn the_todo_is_read_from_git_s_dir_and_none_without_one() {
        let tmp = crate::git::fake::TempDir::new("effect-todo");
        let mut git_dir = tmp.path().as_os_str().as_encoded_bytes().to_vec();
        git_dir.push(b'\n');
        let runner = FakeRunner::new()
            .expect(&["rev-parse", "--absolute-git-dir"], ok(&git_dir))
            .expect(&["rev-parse", "--absolute-git-dir"], ok(&git_dir));
        assert_eq!(read_rebase_todo(&runner), Ok(None));
        let path = rebase_dir(&tmp, TODO, b"pick 0000 z\n");
        assert_eq!(
            read_rebase_todo(&runner),
            Ok(Some(TodoFile {
                path,
                todo: TODO.to_vec(),
                done: Some(b"pick 0000 z\n".to_vec()),
            }))
        );
        runner.assert_done();
    }

    #[tokio::test]
    async fn load_reflog_reports_the_reflog_with_the_same_tag_even_after_a_panic() {
        let runner = Arc::new(
            FakeRunner::new()
                .expect(
                    &[
                        "reflog",
                        "show",
                        "-z",
                        "-n",
                        "1000",
                        "--format=%H %T %gs",
                        "HEAD",
                    ],
                    ok(b"abc 123 commit: x\0"),
                )
                .expect(&["rev-parse", "-q", "--verify", "HEAD"], ok(b"abc\n"))
                .expect(&["symbolic-ref", "-q", "HEAD"], fail("")),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);

        runtime.spawn(Effect::LoadReflog { tag: background(8) });
        match rx.recv().await {
            Some(AppIntent::ReflogLoaded {
                tag,
                result: Ok(reflog),
            }) => {
                assert_eq!(tag, background(8));
                assert_eq!(reflog.entries.len(), 1);
                assert_eq!(
                    reflog.head.map(|h| h.as_str().to_string()),
                    Some("abc".into())
                );
            }
            other => panic!("expected a loaded reflog, got {other:?}"),
        }
        runner.assert_done();

        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(HostilePanic), tx);
        runtime.spawn(Effect::LoadReflog { tag: background(9) });
        match rx.recv().await {
            Some(AppIntent::ReflogLoaded {
                tag,
                result: Err(message),
            }) => {
                assert_eq!(tag, background(9));
                assert!(!message.contains(char::is_control), "{message:?}");
            }
            other => panic!("expected a failed ReflogLoaded, got {other:?}"),
        }
    }
}
