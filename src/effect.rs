use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;

use crate::background;
use crate::clipboard::{self, ClipboardSink, TerminalSink};
use crate::config::{self, ConfigPaths};
use crate::diff_mark::DiffEnd;
use crate::features::branches::{self, remotes, tags};
use crate::features::commits::reflog;
use crate::features::commits::rewrite;
use crate::features::commits::{self, compare, copy};
use crate::features::custom_commands::OutputMode;
use crate::features::diff::renderers::{self, ExternalRenderer};
use crate::features::diff::{self, DiffStreamer};
use crate::features::stash::StashEntry;
use crate::features::sync::SyncIntent;
use crate::features::{
    blame, commit, conflicts, custom_commands, files, pull_requests, repos, review, stash,
    submodules, sync, worktrees,
};
use crate::forge::merge::MergeMethod;
use crate::forge::task::ForgeContext;
use crate::forge::{ForgeHost, ForgeRepo, LoadSeq};
use crate::git::GitRunner;
use crate::git::askpass::Secret;
use crate::git::branches::BranchName;
use crate::git::commit_files::{CommitId, CommitRange};
use crate::git::diff::{DiffRequest, EmojiZwj};
use crate::git::log::LogQuery;
use crate::git::path::RepoPath;
use crate::git::recording::{CommandEvent, CommandOutcome, CommandSink};
use crate::git::refs::read_refs;
use crate::git::remote::{AskpassEnv, RemoteOp};
use crate::git::remotes::RemoteName;
use crate::git::safe_text::terminal_safe;
use crate::handoff::HandOff;
use crate::intent::AppIntent;
use crate::op::{GitOp, run_op};

/// Identifies an effect's result so the reducer can drop stale ones.
/// `generation` changes when the repository changes; `seq` orders loads of the
/// same kind; `user` marks effects that count toward the busy indicator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tag {
    pub generation: u64,
    pub seq: u64,
    pub user: bool,
}

/// The forge numbers its loads by the tag's `seq`; the generation check
/// stays with the caller.
impl From<Tag> for LoadSeq {
    fn from(tag: Tag) -> Self {
        LoadSeq(tag.seq)
    }
}

/// Work the reducer asks for. Executed by [`Runtime`], never by `update`.
/// Variants are grouped by the code that runs them: core first, then the
/// features in the order of ADR-0001's layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    // --- core: loop ---
    /// Stop git commands still running, because Kika is quitting. Answered
    /// by nothing; the stopped operation reports as failed, if anyone listens.
    InterruptGit,
    /// Hand the terminal to another process. Run by the loop, which owns
    /// the terminal, never by [`Runtime`].
    HandOff(HandOff),
    /// Point git at the repository `nested` names: each path is a
    /// submodule of the one before, starting from the top-level `root`,
    /// which an empty list means. Runs before the next effect starts.
    SetRepo {
        root: PathBuf,
        nested: Vec<RepoPath>,
    },

    // --- core: op slot ---
    /// Run a Git operation; always answered by one `OpFinished`.
    RunOp {
        tag: Tag,
        op: GitOp,
    },

    // --- core: watch and refresh ---
    /// Read the refs snapshot; always answered by one `RefsLoaded`.
    SnapshotRefs {
        tag: Tag,
    },

    // --- core: config ---
    /// Read the global config file and that of the repository whose top
    /// level is `repo`; answered by one `ConfigLoaded` for the same `repo`.
    LoadConfig {
        repo: Option<PathBuf>,
    },

    // --- core: credentials ---
    /// Hand the answer to the question the running remote operation
    /// asked; `None` cancels it. Does nothing when no question waits.
    AnswerCredential(Option<Secret>),

    // --- core: clipboard ---
    /// Put `text` on the system clipboard. Run by the loop, between draws,
    /// never by a task; a failure comes back as `CopyFailed`.
    Clipboard(String),

    // --- files ---
    LoadStatus {
        tag: Tag,
    },
    /// Add `line` to the worktree's top-level `.gitignore`, creating it if
    /// need be; always answered by one `FilesIntent::IgnoreWritten`.
    AppendIgnore {
        tag: Tag,
        line: Vec<u8>,
    },

    // --- diff ---
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
    /// Report `DiffIntent::Slow` after [`DIFF_LOADING_DELAY`], so fast
    /// loads never flash "Loading…".
    DiffLoadingDelay {
        tag: Tag,
    },
    /// Load the whole diff of one file as a patch; always answered by one
    /// `DiffIntent::PatchLoaded`.
    LoadPatch {
        tag: Tag,
        request: DiffRequest,
    },

    // --- diff/renderers ---
    /// Find the configured diff renderers: the global `[diff] pager`, then
    /// git's global pagers, then the global `[diff] tool`. Answered by one
    /// `RenderersIntent::Found`.
    FindRenderers,
    /// Run `renderer` at `cols` columns: a pager on the diff for `request`,
    /// a tool on both sides of its file. Always answered by one
    /// `RenderersIntent::Rendered`.
    RenderDiff {
        tag: Tag,
        request: DiffRequest,
        renderer: ExternalRenderer,
        cols: u16,
    },

    // --- commit ---
    /// Read `HEAD`'s message to amend it; always answered by one
    /// `CommitIntent::HeadMessageLoaded`.
    LoadHeadMessage {
        tag: Tag,
    },
    /// Read what decides whether the commit `op` needs the terminal;
    /// always answered by one `CommitIntent::Probed`.
    ProbeCommit {
        tag: Tag,
        op: GitOp,
    },

    // --- stash ---
    /// Read the stash list; always answered by one `StashIntent::Loaded`.
    LoadStashes {
        tag: Tag,
    },
    /// List the files `entry` holds; always answered by one
    /// `StashIntent::FilesLoaded`.
    LoadStashFiles {
        tag: Tag,
        entry: StashEntry,
    },

    // --- submodules ---
    /// List the submodules; always answered by one `SubmodulesIntent::Loaded`.
    LoadSubmodules {
        tag: Tag,
    },

    // --- worktrees ---
    /// List the worktrees; always answered by one `WorktreesIntent::Loaded`.
    LoadWorktrees {
        tag: Tag,
    },
    /// Check the typed `path`, then add a worktree for `branch` there;
    /// always answered by one `WorktreesIntent::Added`.
    AddWorktree {
        tag: Tag,
        path: String,
        branch: BranchName,
    },
    /// Remove the linked worktree at `path`, never forced; always
    /// answered by one `WorktreesIntent::Removed`.
    RemoveWorktree {
        tag: Tag,
        path: RepoPath,
    },

    // --- repos ---
    /// Read the recent-repositories list; answered by `ReposIntent::Loaded`
    /// with the same `seq`.
    LoadRecent {
        seq: u64,
    },
    /// Put `path` first in the recent list; answered by `ReposIntent::Loaded`
    /// with the new list and the same `seq`.
    RecordRecent {
        path: PathBuf,
        seq: u64,
    },
    /// Make the runner's directory a repository; always answered by one
    /// `ReposIntent::Initialised`.
    InitRepo {
        tag: Tag,
    },

    // --- custom_commands ---
    /// Run the user's command in the background, in the repository's
    /// workdir, with its output captured; always answered by one
    /// `CustomCommandsIntent::Finished`.
    RunCommand {
        title: String,
        argv: Vec<OsString>,
        output: OutputMode,
    },
    /// Stop the command `RunCommand` started, which then answers with
    /// `CommandFailure::Cancelled`. Does nothing once it has finished.
    CancelCommand,

    // --- blame ---
    /// Blame `path` in the worktree; always answered by one `BlameIntent::Loaded`.
    LoadBlame {
        tag: Tag,
        path: RepoPath,
    },
    /// Read commit `oid`'s message and diff; always answered by one
    /// `BlameIntent::CommitLoaded`.
    LoadCommit {
        tag: Tag,
        oid: String,
    },

    // --- conflicts ---
    /// Read conflicted `path` from the worktree and its marker size;
    /// always answered by one `ConflictsIntent::Loaded` for the same `path`.
    LoadConflict {
        tag: Tag,
        path: RepoPath,
    },
    /// Write `content` over `path`, only while it still holds `expected`;
    /// always answered by one `ConflictsIntent::ResolvedWritten` for the same `path`.
    WriteResolved {
        tag: Tag,
        path: RepoPath,
        expected: Vec<u8>,
        content: Vec<u8>,
    },

    // --- sync ---
    /// Fetch every remote; always answered by one `SyncIntent::FetchFinished`.
    Fetch {
        tag: Tag,
    },
    /// Fetch, pull or push, with git's and ssh's questions sent as
    /// `CredentialAsked`; always answered by one `SyncIntent::RemoteFinished`.
    Remote {
        tag: Tag,
        op: RemoteOp,
    },

    // --- pull_requests ---
    /// Find the forge repository of the remote and send its cached badges,
    /// then, with `refresh`, ask the forge for `branches`. Answered by
    /// `PullRequestsIntent::RepoNone`, or `PullRequestsIntent::Cached` and
    /// then, with `refresh`, `PullRequestsIntent::Fetched`. With no `hosts`
    /// it only answers `PullRequestsIntent::RepoNone`.
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
    /// Start the browser the global config names: `argv` is its argv with
    /// the URL last, run without a shell and never waited on here. Only
    /// a failure to start answers, with `PullRequestsIntent::OpenUrlFailed`.
    OpenUrl {
        argv: Vec<OsString>,
    },
    /// Open a pull request on `repo` through `gh`, on the configured
    /// `host`; always answered by one `PullRequestsIntent::Created`.
    CreatePullRequest {
        tag: Tag,
        host: ForgeHost,
        repo: ForgeRepo,
        /// The branch to merge, `owner:branch` when it is in a fork.
        head: String,
        base: String,
        title: String,
    },

    // --- review ---
    /// Merge pull request `number` of `repo` by `method` through `gh`, on
    /// the configured `host`, only while its head is still `head_oid`;
    /// always answered by one `ReviewIntent::Merged`.
    MergePullRequest {
        tag: Tag,
        host: ForgeHost,
        repo: ForgeRepo,
        number: u64,
        method: MergeMethod,
        head_oid: CommitId,
    },
    /// Read the review queues of the remote's repository: `repo` when an
    /// earlier load found it, else the remote is read and matched against
    /// `hosts`. A host in `limited` is not asked before its second has
    /// passed. Always answered by one `ReviewIntent::QueuesLoaded`.
    LoadReviewQueues {
        tag: Tag,
        hosts: Vec<ForgeHost>,
        repo: Option<ForgeRepo>,
        limited: Vec<(ForgeHost, i64)>,
    },
    /// Read pull request `number`, found as for `LoadReviewQueues`. Always
    /// answered by one `ReviewIntent::PrDetailLoaded`.
    LoadPrDetail {
        tag: Tag,
        hosts: Vec<ForgeHost>,
        repo: Option<ForgeRepo>,
        number: u64,
        limited: Vec<(ForgeHost, i64)>,
    },
    /// See whether a pull request's `head` and `base` commits are here;
    /// for missing ones, find the remote of `repo` on one of `hosts` to
    /// fetch them from. Local only. Always answered by one
    /// `ReviewIntent::ObjectsChecked`.
    CheckReviewCommits {
        tag: Tag,
        head: CommitId,
        base: CommitId,
        hosts: Vec<ForgeHost>,
        repo: ForgeRepo,
    },
    /// Fetch `oids` from `remote` by id, with git's and ssh's questions
    /// sent as `CredentialAsked`. Always answered by one
    /// `ReviewIntent::HeadFetched`.
    FetchReviewCommits {
        tag: Tag,
        remote: RemoteName,
        oids: Vec<CommitId>,
    },
    /// The merge base of `base` and `head`, and the files the pull request
    /// changes from it. Always answered by one `ReviewIntent::FilesLoaded`.
    LoadReviewFiles {
        tag: Tag,
        head: CommitId,
        base: CommitId,
    },

    // --- branches ---
    /// List the local branches; always answered by one
    /// `BranchesIntent::Loaded`.
    LoadBranches {
        tag: Tag,
    },
    /// Read the newest commits of the full ref `refname`, local or
    /// remote-tracking; always answered by one `BranchesIntent::LogLoaded`
    /// for the same `refname`.
    LoadBranchLog {
        tag: Tag,
        refname: Vec<u8>,
    },

    // --- branches/remotes ---
    /// List the remotes and their remote-tracking branches; always
    /// answered by one `RemotesIntent::Loaded`.
    LoadRemotes {
        tag: Tag,
    },

    // --- branches/tags ---
    /// List the tags; always answered by one `TagsIntent::Loaded`.
    LoadTags {
        tag: Tag,
    },

    // --- commits ---
    /// Read a page of `HEAD`'s log, from `tip` when set, narrowed to
    /// `query`; always answered by one `CommitsIntent::PageLoaded`.
    LoadCommits {
        tag: Tag,
        tip: Option<String>,
        skip: usize,
        query: LogQuery,
    },
    /// List the files `range` changed; always answered by one
    /// `CommitsIntent::FilesLoaded`.
    LoadCommitFiles {
        tag: Tag,
        range: CommitRange,
    },
    /// Read which commits are unpushed or unmerged; always answered by
    /// one `CommitsIntent::MarkersLoaded`.
    LoadMarkers {
        tag: Tag,
    },
    /// Read where every local branch points; always answered by one
    /// `CommitsIntent::TipsLoaded`.
    LoadBranchTips {
        tag: Tag,
    },

    // --- commits/reflog ---
    /// Read `HEAD`'s reflog and where `HEAD` is; always answered by one
    /// `ReflogIntent::Loaded`.
    LoadReflog {
        tag: Tag,
    },

    // --- commits/rewrite ---
    /// Find the commits the staged lines come from; always answered by
    /// one `RewriteIntent::FixupBaseFound`.
    FindFixupBase {
        tag: Tag,
    },
    /// Read the stopped rebase's todo list and what it has done; always
    /// answered by one `RewriteIntent::TodoLoaded`.
    LoadRebaseTodo {
        tag: Tag,
        /// The installed git reads `core.commentString` (2.45).
        string_too: bool,
    },
    /// Check `content` as an edit of `expected`, both read with the
    /// `comment` prefix `expected` was loaded with, then write it over the
    /// todo list at `path` only while it still holds `expected`; always
    /// answered by one `RewriteIntent::TodoWritten`.
    WriteRebaseTodo {
        tag: Tag,
        path: PathBuf,
        expected: Vec<u8>,
        content: Vec<u8>,
        comment: Vec<u8>,
    },
    /// List the commits only `head` reaches before `op` moves `HEAD` to
    /// `target`; always answered by one `RewriteIntent::LeftBehindChecked`.
    CheckLeftBehind {
        tag: Tag,
        op: GitOp,
        head: CommitId,
        target: CommitId,
    },

    // --- commits/copy ---
    /// Read commit `oid`'s full message to copy it; always answered by
    /// one `CopyIntent::MessageLoaded`.
    LoadCommitMessage {
        tag: Tag,
        oid: CommitId,
    },

    // --- commits/compare ---
    /// Read both ends of a comparison as commits; always answered by one
    /// `CompareIntent::Resolved`.
    ResolveDiff {
        tag: Tag,
        base: DiffEnd,
        target: DiffEnd,
    },
}

/// Log `argv` as started in the Command log, and return what logs it as
/// finished, with its outcome and how long it ran.
pub(crate) fn log_program(
    argv: &[OsString],
    sink: CommandSink,
) -> impl FnOnce(CommandOutcome) + Send + use<> {
    let line = argv
        .iter()
        .map(|arg| terminal_safe(arg.as_encoded_bytes()))
        .collect::<Vec<_>>()
        .join(" ");
    let id = sink.next_id();
    sink.send(CommandEvent::Started {
        id,
        line,
        attached: false,
    });
    let start = std::time::Instant::now();
    move |outcome| {
        sink.send(CommandEvent::Finished {
            id,
            outcome,
            elapsed: start.elapsed(),
        })
    }
}

/// How long a clipboard program may take before it is stopped. Programs
/// that keep owning the selection fork and return well within it.
const CLIPBOARD_TIMEOUT: Duration = Duration::from_secs(5);

/// Copy with `argv`, the global config's clipboard program, given `bytes`
/// on stdin; never in argv or the Command log. Spawning is quick, and
/// writing and waiting run on a worker thread, so a program that does not
/// read or does not exit holds nothing up: past `timeout` it is stopped
/// with everything it started. Any failure comes back as `CopyFailed`.
/// Returns the worker, if the program started, so tests can see it end.
fn run_clipboard_command(
    argv: Vec<OsString>,
    bytes: Vec<u8>,
    sink: CommandSink,
    tx: UnboundedSender<AppIntent>,
    timeout: Duration,
) -> Option<std::thread::JoinHandle<()>> {
    use std::process::Stdio;

    let start = std::time::Instant::now();
    let finished = log_program(&argv, sink);
    // `check_argv` refuses an empty command, so this is only a guard.
    let Some((program, args)) = argv.split_first() else {
        finished(CommandOutcome::NotStarted);
        return None;
    };
    let name = terminal_safe(program.as_encoded_bytes());
    let fail = move |why: String| {
        let _ = tx.send(AppIntent::CopyFailed(copy_failure(&why)));
    };
    let mut command = match crate::program::command(program) {
        Ok(command) => command,
        Err(e) => {
            finished(CommandOutcome::NotStarted);
            fail(format!("Could not start {name}: {e}"));
            return None;
        }
    };
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Its own process group, so a stop reaches whatever it starts.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            finished(CommandOutcome::NotStarted);
            fail(format!("Could not start {name}: {e}"));
            return None;
        }
    };
    let mut writer = child
        .stdin
        .take()
        .map(|stdin| StdinWriter::new(stdin, bytes));
    Some(std::thread::spawn(move || {
        let deadline = start + timeout;
        let mut write = if writer.is_none() {
            Sending::Sent
        } else {
            Sending::Ongoing
        };
        let mut exited = None;
        let status = loop {
            let mut progressed = false;
            if let Some(w) = writer.as_mut() {
                match w.poll() {
                    WritePoll::Pending { progressed: p } => progressed = p,
                    WritePoll::Done(result) => {
                        // Dropping stdin closes our end of the pipe.
                        writer = None;
                        write = result.map_or_else(Sending::Failed, |()| Sending::Sent);
                    }
                }
            }
            if exited.is_none() {
                match child.try_wait() {
                    Ok(status) => exited = status,
                    Err(e) => {
                        crate::features::diff::renderers::external_diff::kill_group(&mut child);
                        break Err(format!("Could not wait for {name}: {e}"));
                    }
                }
            }
            match exited {
                // A failed exit says more than the write, so it need not wait.
                Some(status) if !matches!(write, Sending::Ongoing) || !status.success() => {
                    break Ok(status);
                }
                _ if std::time::Instant::now() < deadline => {
                    if !progressed {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
                // It exited, but something it started holds stdin without
                // reading. Its group is never signalled once the program is
                // reaped, as its id may be reused; closing our end of the
                // pipe is enough to hold nothing here.
                Some(status) => {
                    writer = None;
                    write = Sending::Abandoned;
                    break Ok(status);
                }
                None => {
                    crate::features::diff::renderers::external_diff::kill_group(&mut child);
                    break Err(format!(
                        "{name} did not finish within {}s",
                        timeout.as_secs_f32()
                    ));
                }
            }
        };
        drop(writer);
        match (status, write) {
            (Ok(status), _) if !status.success() => {
                finished(CommandOutcome::Failed(Some(status.to_string())));
                fail(format!("{name} failed: {status}"));
            }
            (Ok(_), Sending::Sent) => finished(CommandOutcome::Ok),
            (Ok(_), Sending::Failed(e)) => {
                finished(CommandOutcome::Failed(None));
                fail(format!("Could not send the text to {name}: {e}"));
            }
            (Ok(_), Sending::Ongoing | Sending::Abandoned) => {
                finished(CommandOutcome::Failed(None));
                fail(format!("Could not send the text to {name}"));
            }
            (Err(why), _) => {
                finished(CommandOutcome::Failed(None));
                fail(why);
            }
        }
    }))
}

/// `why` a copy failed, as the popup says it: one sentence, then where to
/// look, with no doubled period when `why` already ends in one.
fn copy_failure(why: &str) -> String {
    let stop = if why.ends_with('.') { "" } else { "." };
    format!("{why}{stop} Check clipboard.command in the global config.")
}

/// Where sending the text to a clipboard program stands.
enum Sending {
    Ongoing,
    Sent,
    Failed(std::io::Error),
    /// Still unsent at the timeout, after the program had exited.
    Abandoned,
}

/// What one step of sending the text did.
enum WritePoll {
    /// More to send; `progressed` if some bytes went this step.
    Pending { progressed: bool },
    /// All sent, or the write failed and was given up.
    Done(std::io::Result<()>),
}

/// Sends the text to a clipboard program's stdin a step at a time, from
/// the thread that waits for it, so a program (or a helper it started)
/// that holds stdin without reading parks no thread: the pipe is
/// non-blocking and a full one is tried again on the next step.
#[cfg(unix)]
struct StdinWriter {
    stdin: std::process::ChildStdin,
    bytes: Vec<u8>,
    sent: usize,
    /// Set if the pipe could not be made non-blocking; the first step
    /// reports it.
    broken: Option<std::io::Error>,
}

#[cfg(unix)]
impl StdinWriter {
    fn new(stdin: std::process::ChildStdin, bytes: Vec<u8>) -> Self {
        use std::os::fd::AsRawFd;
        let fd = stdin.as_raw_fd();
        // SAFETY: `fd` is the open pipe `stdin` owns; F_GETFL and F_SETFL
        // only read and set its status flags.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        let set = flags != -1
            // SAFETY: as above.
            && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } != -1;
        let broken = (!set).then(std::io::Error::last_os_error);
        Self {
            stdin,
            bytes,
            sent: 0,
            broken,
        }
    }

    fn poll(&mut self) -> WritePoll {
        use std::io::{ErrorKind, Write};
        if let Some(e) = self.broken.take() {
            return WritePoll::Done(Err(e));
        }
        let mut progressed = false;
        while self.sent < self.bytes.len() {
            match self.stdin.write(&self.bytes[self.sent..]) {
                Ok(0) => return WritePoll::Done(Err(ErrorKind::WriteZero.into())),
                Ok(n) => {
                    self.sent += n;
                    progressed = true;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    return WritePoll::Pending { progressed };
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return WritePoll::Done(Err(e)),
            }
        }
        WritePoll::Done(Ok(()))
    }
}

/// Without non-blocking pipes, the text is sent from a thread of its own,
/// deliberately detached: one that never finishes because nothing reads
/// ends when the program's last holder of stdin exits.
#[cfg(not(unix))]
struct StdinWriter {
    wrote: std::sync::mpsc::Receiver<std::io::Result<()>>,
}

#[cfg(not(unix))]
impl StdinWriter {
    fn new(mut stdin: std::process::ChildStdin, bytes: Vec<u8>) -> Self {
        use std::io::Write;
        let (wrote_tx, wrote) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = wrote_tx.send(stdin.write_all(&bytes));
        });
        Self { wrote }
    }

    fn poll(&mut self) -> WritePoll {
        match self.wrote.try_recv() {
            Ok(result) => WritePoll::Done(result),
            Err(std::sync::mpsc::TryRecvError::Empty) => WritePoll::Pending { progressed: false },
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                WritePoll::Done(Err(std::io::ErrorKind::BrokenPipe.into()))
            }
        }
    }
}

/// Seconds since the epoch, which relative commit dates count from.
pub(crate) fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
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
    /// The global config's `[clipboard] command`, read once at startup;
    /// when set it copies instead of OSC 52.
    clipboard_command: Option<Vec<OsString>>,
    /// Where programs Kika starts outside git are logged, as git's are.
    sink: CommandSink,
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
            clipboard_command: None,
            sink: CommandSink::new(),
        }
    }

    /// Log the programs Kika starts outside git, such as the browser, to
    /// `sink`, beside git's own commands.
    pub fn with_command_sink(self, sink: CommandSink) -> Self {
        Self { sink, ..self }
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

    /// Copy with `command`, given the text on stdin, instead of OSC 52.
    pub fn with_clipboard_command(self, clipboard_command: Option<Vec<OsString>>) -> Self {
        Self {
            clipboard_command,
            ..self
        }
    }

    /// Put `text` on the clipboard now, on the caller's thread, so the
    /// bytes never land inside a frame being drawn. With a clipboard
    /// program, only starting it happens here; a failure arrives later as
    /// `CopyFailed`.
    pub fn copy_to_clipboard(&self, text: &str) -> Result<(), String> {
        if let Some(argv) = &self.clipboard_command {
            let bytes = clipboard::command_payload(text)?;
            // Detached: the worker ends by itself, at the latest at the timeout.
            let _ = run_clipboard_command(
                argv.clone(),
                bytes,
                self.sink.clone(),
                self.tx.clone(),
                CLIPBOARD_TIMEOUT,
            );
            return Ok(());
        }
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
            Effect::LoadStatus { tag } => files::effects::load(runner, tx, tag),
            Effect::LoadHeadMessage { tag } => commit::effects::load_head_message(runner, tx, tag),
            Effect::LoadSubmodules { tag } => submodules::effects::load(runner, tx, tag),
            Effect::LoadWorktrees { tag } => worktrees::effects::load(runner, tx, tag),
            Effect::AddWorktree { tag, path, branch } => {
                worktrees::effects::add(runner, tx, tag, path, branch)
            }
            Effect::RemoveWorktree { tag, path } => {
                worktrees::effects::remove(runner, tx, tag, path)
            }
            Effect::LoadBranches { tag } => branches::effects::load(runner, tx, tag),
            Effect::LoadBranchLog { tag, refname } => {
                branches::effects::load_log(runner, tx, tag, refname)
            }
            Effect::LoadRemotes { tag } => remotes::effects::load(runner, tx, tag),
            Effect::LoadTags { tag } => tags::effects::load(runner, tx, tag),
            // Synchronous, so no later effect runs in the old repository.
            // A diff still streaming belongs to the old one.
            Effect::SetRepo { root, nested } => {
                self.diff.cancel();
                let dir = nested
                    .iter()
                    .fold(root, |dir, path| dir.join(path.as_os_str()));
                self.runner.set_workdir(dir);
            }
            Effect::LoadRecent { seq } => repos::effects::load(self.recent_store.clone(), tx, seq),
            Effect::RecordRecent { path, seq } => {
                repos::effects::record(self.recent_store.clone(), tx, seq, path)
            }
            Effect::InitRepo { tag } => repos::effects::init(runner, tx, tag),
            Effect::LoadPatch { tag, request } => {
                diff::effects::load_patch(runner, tx, tag, request)
            }
            Effect::LoadBlame { tag, path } => {
                blame::effects::load(runner, tx, tag, path, self.emoji_zwj)
            }
            Effect::FindFixupBase { tag } => rewrite::effects::find_base(runner, tx, tag),
            Effect::LoadConflict { tag, path } => {
                // Where git runs now, inside an entered submodule too.
                conflicts::effects::load(runner, tx, self.runner.workdir(), tag, path)
            }
            Effect::WriteResolved {
                tag,
                path,
                expected,
                content,
            } => conflicts::effects::write(tx, self.runner.workdir(), tag, path, expected, content),
            Effect::LoadCommit { tag, oid } => {
                blame::effects::load_commit(runner, tx, tag, oid, self.emoji_zwj)
            }
            Effect::FindRenderers => renderers::effects::find(
                runner,
                tx,
                self.diff_pager.clone(),
                self.diff_tool.clone(),
            ),
            Effect::RenderDiff {
                tag,
                request,
                renderer,
                cols,
            } => {
                // Where git runs now, inside an entered submodule too.
                let workdir = self.runner.workdir();
                renderers::effects::render(runner, tx, workdir, tag, request, renderer, cols)
            }
            Effect::LoadDiff { tag, request, want } => self.diff.start(tag, request, want),
            Effect::DiffDemand { seq, want } => self.diff.demand(seq, want),
            Effect::CancelDiff => self.diff.cancel(),
            Effect::DiffLoadingDelay { tag } => diff::effects::loading_delay(tx, tag),
            // Synchronous: the loop exits right after, and the runtime's
            // shutdown must not wait on git.
            Effect::InterruptGit => self.runner.interrupt(),
            Effect::RunOp { tag, op } => {
                let task_op = op.clone();
                background::run(
                    tx,
                    move || run_op(runner.as_ref(), &task_op),
                    // A panic still reports back, or `op_seq` would block every later op.
                    // The panic text may quote a path; it is drawn in the status bar.
                    background::crashed("git"),
                    move |result| AppIntent::OpFinished { tag, op, result },
                );
            }
            Effect::ProbeCommit { tag, op } => commit::effects::probe(runner, tx, tag, op),
            Effect::SnapshotRefs { tag } => background::run(
                tx,
                move || read_refs(runner.as_ref()),
                // A panic still reports back, or the watch would never poll again.
                background::crashed("git for-each-ref"),
                move |result| AppIntent::RefsLoaded { tag, result },
            ),
            Effect::LoadCommits {
                tag,
                tip,
                skip,
                query,
            } => commits::effects::load(runner, tx, tag, tip, skip, query),
            Effect::ResolveDiff { tag, base, target } => {
                compare::effects::resolve(runner, tx, tag, base, target)
            }
            Effect::LoadCommitFiles { tag, range } => {
                commits::effects::load_files(runner, tx, tag, range)
            }
            Effect::LoadStashes { tag } => stash::effects::load(runner, tx, tag),
            Effect::LoadStashFiles { tag, entry } => {
                stash::effects::load_files(runner, tx, tag, entry)
            }
            Effect::LoadMarkers { tag } => commits::effects::load_markers(runner, tx, tag),
            Effect::LoadBranchTips { tag } => commits::effects::load_tips(runner, tx, tag),
            Effect::LoadRebaseTodo { tag, string_too } => {
                rewrite::effects::load_todo(runner, tx, tag, string_too)
            }
            Effect::WriteRebaseTodo {
                tag,
                path,
                expected,
                content,
                comment,
            } => rewrite::effects::write_todo(tx, tag, path, expected, content, comment),
            Effect::LoadReflog { tag } => reflog::effects::load(runner, tx, tag),
            Effect::Fetch { tag } => sync::effects::fetch(runner, tx, tag),
            Effect::LoadConfig { repo } => {
                let paths = ConfigPaths::new(self.global_config.clone(), repo.as_deref());
                background::run_with(
                    tx,
                    move || config::load(&paths),
                    // `load` reports its own errors; a panic leaves the keys as they were.
                    move |outcome| {
                        outcome.ok().map(|loaded| AppIntent::ConfigLoaded {
                            // Echoed so the reducer can drop a late result.
                            repo,
                            keymap: loaded.keymap,
                            errors: loaded.errors,
                        })
                    },
                );
            }
            Effect::RunCommand {
                title,
                argv,
                output,
            } => {
                // Taken now: a `SetRepo` after this must not move the command.
                *lock(&self.command) = Some(custom_commands::effects::run(
                    self.runner.workdir(),
                    tx,
                    title,
                    argv,
                    output,
                ));
            }
            Effect::CancelCommand => custom_commands::effects::cancel(lock(&self.command).take()),
            Effect::Remote { tag, op } => {
                sync::effects::remote(runner, tx, tag, op, Arc::clone(&self.credential))
            }
            Effect::LoadCommitMessage { tag, oid } => {
                copy::effects::load_message(runner, tx, tag, oid)
            }
            Effect::CheckLeftBehind {
                tag,
                op,
                head,
                target,
            } => rewrite::effects::check_left_behind(runner, tx, tag, op, head, target),
            // Where git runs now, inside an entered submodule too.
            Effect::AppendIgnore { tag, line } => {
                files::effects::ignore(self.runner.workdir(), tx, tag, line)
            }
            // The loop takes these out and copies itself, between draws.
            // Without a loop there is no terminal to copy through.
            Effect::Clipboard(_) => {}
            Effect::OpenUrl { argv } => {
                pull_requests::effects::open_url(argv, self.sink.clone(), tx)
            }
            Effect::CreatePullRequest {
                tag,
                host,
                repo,
                head,
                base,
                title,
            } => pull_requests::effects::create(
                self.forge.clone(),
                tx,
                tag,
                host,
                repo,
                head,
                base,
                title,
            ),
            Effect::MergePullRequest {
                tag,
                host,
                repo,
                number,
                method,
                head_oid,
            } => review::effects::merge(
                self.forge.clone(),
                tx,
                tag,
                host,
                repo,
                number,
                method,
                head_oid,
            ),
            Effect::AnswerCredential(answer) => {
                if let Some(answers) = lock(&self.credential).as_ref() {
                    // Gone means the operation ended; nobody is asking.
                    let _ = answers.send(answer);
                }
            }
            // The loop takes these out before spawning; without a terminal
            // there is nothing to hand over, so one that slips through is dropped.
            Effect::HandOff(_) => {}
            Effect::LoadReviewQueues {
                tag,
                hosts,
                repo,
                limited,
            } => review::effects::load_queues(
                self.forge.clone(),
                runner,
                tx,
                tag,
                hosts,
                repo,
                limited,
            ),
            Effect::LoadPrDetail {
                tag,
                hosts,
                repo,
                number,
                limited,
            } => review::effects::load_detail(
                self.forge.clone(),
                runner,
                tx,
                tag,
                hosts,
                repo,
                number,
                limited,
            ),
            Effect::CheckReviewCommits {
                tag,
                head,
                base,
                hosts,
                repo,
            } => review::effects::check_commits(runner, tx, tag, head, base, hosts, repo),
            Effect::FetchReviewCommits { tag, remote, oids } => review::effects::fetch_commits(
                runner,
                tx,
                tag,
                remote,
                oids,
                Arc::clone(&self.credential),
            ),
            Effect::LoadReviewFiles { tag, head, base } => {
                review::effects::load_files(runner, tx, tag, head, base)
            }
            Effect::LoadPullRequests {
                tag,
                hosts,
                branches,
                refresh,
                known,
                limited,
            } => pull_requests::effects::load(
                self.forge.clone(),
                runner,
                tx,
                tag,
                hosts,
                branches,
                refresh,
                known,
                limited,
            ),
        }
    }
}

/// Where a running remote operation waits for the user's answer.
pub(crate) type AnswerSlot = Arc<Mutex<Option<std::sync::mpsc::Sender<Option<Secret>>>>>;

/// The askpass bridge for one remote operation, wired to the loop: each
/// question goes out as an intent, and its answer comes back through the
/// runtime's slot. Without one (not on unix, or it would not start) `env`
/// is `None` and git runs closed to questions.
pub(crate) struct Bridge {
    #[cfg(unix)]
    server: Option<crate::git::askpass::AskpassServer>,
    slot: AnswerSlot,
    pub(crate) env: Option<AskpassEnv>,
}

impl Bridge {
    #[cfg(unix)]
    pub(crate) fn open(tag: Tag, tx: UnboundedSender<AppIntent>, slot: AnswerSlot) -> Self {
        use crate::git::askpass::{AskpassServer, Question};

        let (answer_tx, answer_rx) = std::sync::mpsc::channel();
        *lock(&slot) = Some(answer_tx);
        let answers = Mutex::new(answer_rx);
        let server = std::env::current_exe().ok().and_then(|program| {
            let server = AskpassServer::start(move |question| match question {
                Question::Notice(text) => {
                    let _ = tx.send(AppIntent::Sync(SyncIntent::RemoteNotice { tag, text }));
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
    pub(crate) fn open(_tag: Tag, _tx: UnboundedSender<AppIntent>, slot: AnswerSlot) -> Self {
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
    #[cfg(unix)]
    use std::io;

    use tokio::sync::mpsc;

    use super::*;
    use crate::features::blame::BlameIntent;
    use crate::features::commits::CommitsIntent;
    use crate::features::commits::compare::CompareIntent;
    use crate::features::commits::copy::CopyIntent;
    use crate::features::commits::reflog::ReflogIntent;
    #[cfg(unix)]
    use crate::features::custom_commands::CommandFailure;
    use crate::features::custom_commands::CustomCommandsIntent;
    use crate::features::diff::DiffIntent;
    use crate::features::pull_requests::PullRequestsIntent;
    use crate::features::repos::ReposIntent;
    use crate::features::submodules::SubmodulesIntent;
    use crate::git::remote::RemoteFailure;
    use crate::test_support::{HostilePanic, background, commit_op, forge_tag};

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
                )
                .expect(&crate::git::forge_remote::UPSTREAMS_ARGV, ok(b"")),
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
            Some(AppIntent::PullRequests(PullRequestsIntent::Cached {
                badges: None,
                ..
            }))
        ));
        assert!(matches!(
            rx.recv().await,
            Some(AppIntent::PullRequests(PullRequestsIntent::Fetched {
                result: Ok(_),
                ..
            }))
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
                "git for-each-ref \
                 --format=%(refname)%00%(upstream:remotename)%00%(upstream:remoteref) refs/heads",
                probe_line,
                gh_line,
                gh_line
            ]
        );
        let mut ids: Vec<u64> = started.iter().map(|(id, _)| *id).collect();
        ids.dedup();
        assert_eq!(ids.len(), 6, "{events:?}");
        // One finish per start, with the same id.
        for (id, _) in &started {
            let finished = events
                .iter()
                .filter(|e| matches!(e, CommandEvent::Finished { id: f, .. } if f == id))
                .count();
            assert_eq!(finished, 1, "{id}");
        }
    }
    use crate::git::diff::DiffSide;
    use crate::git::fake::{FakeChild, FakeRunner, fail, ok};
    #[cfg(unix)]
    use crate::git::runner::{GitChild, GitCommand, GitOutput};

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
            Some(AppIntent::Commits(CommitsIntent::Copy(
                CopyIntent::MessageLoaded {
                    tag,
                    oid,
                    result: Ok("subj".into()),
                }
            )))
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
            Some(AppIntent::Blame(BlameIntent::Loaded {
                tag: got,
                result: Ok(lines),
            })) => {
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
            Some(AppIntent::Blame(BlameIntent::Loaded {
                tag: got,
                result: Err(message),
            })) => {
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
            Some(AppIntent::Blame(BlameIntent::CommitLoaded {
                tag: got,
                result: Err(message),
            })) => {
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
            Some(AppIntent::Blame(BlameIntent::CommitLoaded {
                tag: got,
                result: Ok(lines),
            })) => {
                assert_eq!(got, tag);
                assert_eq!(lines.len(), 1);
                assert_eq!(lines[0].text, "commit abc");
            }
            other => panic!("expected a loaded commit, got {other:?}"),
        }
        runner.assert_done();
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
            Some(AppIntent::Submodules(SubmodulesIntent::Loaded {
                tag,
                result: Ok(vec![Submodule {
                    path: "sub".into(),
                    oid: "1234567".into(),
                    state: SubmoduleState::UpToDate,
                }]),
            }))
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
            Some(AppIntent::Submodules(SubmodulesIntent::Loaded {
                tag: got,
                result: Err(message),
            })) => {
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
        let empty = |seq| Some(AppIntent::Repos(ReposIntent::Loaded { seq, list: vec![] }));
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
            Some(AppIntent::Repos(ReposIntent::Loaded {
                seq: 1,
                list: vec![repo.clone()]
            }))
        );
        assert!(store.is_file());
        runtime.spawn(Effect::LoadRecent { seq: 2 });
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::Repos(ReposIntent::Loaded {
                seq: 2,
                list: vec![repo]
            }))
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
            Some(AppIntent::Repos(ReposIntent::Initialised {
                tag,
                result: Ok("/r/new".into()),
            }))
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

        assert_eq!(
            rx.recv().await,
            Some(AppIntent::Diff(DiffIntent::Slow { tag }))
        );
        assert!(start.elapsed() >= DIFF_LOADING_DELAY);
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
            Some(AppIntent::Commits(CommitsIntent::Compare(
                CompareIntent::Resolved {
                    tag: user,
                    result: Ok(CommitRange {
                        oid: target,
                        parent: CommitId::parse("aaa111"),
                    }),
                }
            )))
        );

        // Output that is not hex is never taken for a commit.
        runtime.spawn(Effect::ResolveDiff {
            tag: user,
            base: DiffEnd::Commit(CommitId::parse("aaa111").unwrap()),
            target: DiffEnd::Branch(BranchName::from_bytes(b"topic")),
        });
        match rx.recv().await {
            Some(AppIntent::Commits(CommitsIntent::Compare(CompareIntent::Resolved {
                tag,
                result: Err(message),
            }))) => {
                assert_eq!(tag, user);
                assert!(message.contains("not a commit id"), "{message:?}");
            }
            other => panic!("expected a failed DiffResolved, got {other:?}"),
        }
        runner.assert_done();
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
            Some(AppIntent::Sync(SyncIntent::FetchFinished {
                tag: background(5),
                result: Ok(()),
            }))
        );
        runner.assert_done();
    }

    #[tokio::test]
    async fn a_panicking_fetch_still_reports_back() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(HostilePanic), tx);

        runtime.spawn(Effect::Fetch { tag: background(6) });

        match rx.recv().await {
            Some(AppIntent::Sync(SyncIntent::FetchFinished {
                tag,
                result: Err(message),
            })) => {
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
            Some(AppIntent::Diff(DiffIntent::Loaded {
                tag: got,
                result: Ok(chunk),
            })) => {
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
            Some(AppIntent::CustomCommands(CustomCommandsIntent::Finished {
                title,
                output: OutputMode::Popup,
                result: Err(CommandFailure::Exited(e)),
            })) => {
                assert_eq!(title, "Try");
                assert!(e.contains('3') && e.contains("hi"), "{e}");
            }
            other => panic!("expected a failed CommandFinished, got {other:?}"),
        }

        // `none` drops the output of a success; `popup` keeps it.
        runtime.spawn(run(&["sh", "-c", "printf hi"], OutputMode::None));
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::CustomCommands(CustomCommandsIntent::Finished {
                title: "Try".into(),
                output: OutputMode::None,
                result: Ok(String::new()),
            }))
        );
        runtime.spawn(run(&["sh", "-c", "pwd"], OutputMode::Popup));
        match rx.recv().await {
            Some(AppIntent::CustomCommands(CustomCommandsIntent::Finished {
                result: Ok(cwd),
                ..
            })) => assert_eq!(
                std::fs::canonicalize(cwd).unwrap(),
                std::fs::canonicalize(tmp.path()).unwrap()
            ),
            other => panic!("expected the workdir, got {other:?}"),
        }

        // A program that cannot start still answers, once.
        runtime.spawn(run(&["kika-no-such-program"], OutputMode::None));
        match rx.recv().await {
            Some(AppIntent::CustomCommands(CustomCommandsIntent::Finished {
                result: Err(CommandFailure::Launch(e)),
                ..
            })) => {
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
            Some(AppIntent::CustomCommands(CustomCommandsIntent::Finished {
                title: "Hang".into(),
                output: OutputMode::Popup,
                result: Err(CommandFailure::Cancelled),
            }))
        );
        assert!(started.elapsed() < Duration::from_secs(10));
        // A second cancel finds nothing to stop and sends nothing.
        runtime.spawn(Effect::CancelCommand);
        tokio::task::yield_now().await;
        assert!(rx.try_recv().is_err());
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
                Some(AppIntent::Blame(BlameIntent::Loaded {
                    result: Ok(lines), ..
                })) => assert_eq!(lines[0].text, text, "{emoji:?}"),
                other => panic!("expected a blame, got {other:?}"),
            }
            runtime.spawn(Effect::LoadCommit {
                tag,
                oid: oid.clone(),
            });
            match rx.recv().await {
                Some(AppIntent::Blame(BlameIntent::CommitLoaded {
                    result: Ok(lines), ..
                })) => assert_eq!(lines.last().unwrap().text, format!("+{text}"), "{emoji:?}"),
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
            Some(AppIntent::Sync(SyncIntent::RemoteFinished {
                tag,
                op,
                result: Ok(()),
            }))
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
            Some(AppIntent::Sync(SyncIntent::RemoteFinished {
                tag: got,
                op: RemoteOp::Fetch,
                result: Err(RemoteFailure::Failed(message)),
            })) => {
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
                Some(AppIntent::Sync(SyncIntent::RemoteFinished { result, .. })) => {
                    assert_eq!(result.is_ok(), succeeded, "{result:?}")
                }
                other => panic!("expected RemoteFinished, got {other:?}"),
            }
        }
        // With nothing running, an answer goes nowhere.
        runtime.spawn(Effect::AnswerCredential(None));
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
            Some(AppIntent::Commits(CommitsIntent::Reflog(ReflogIntent::Loaded {
                tag,
                result: Ok(reflog),
            }))) => {
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
            Some(AppIntent::Commits(CommitsIntent::Reflog(ReflogIntent::Loaded {
                tag,
                result: Err(message),
            }))) => {
                assert_eq!(tag, background(9));
                assert!(!message.contains(char::is_control), "{message:?}");
            }
            other => panic!("expected a failed ReflogLoaded, got {other:?}"),
        }
    }

    /// A runtime that copies with `argv`, its log, and where intents land.
    fn clipboard_runtime(
        argv: &[&str],
    ) -> (
        Runtime,
        Arc<crate::clipboard::RecordingSink>,
        std::sync::mpsc::Receiver<CommandEvent>,
        mpsc::UnboundedReceiver<AppIntent>,
    ) {
        let sink = CommandSink::new();
        let (log_tx, log_rx) = std::sync::mpsc::channel();
        sink.connect(move |ev| {
            let _ = log_tx.send(ev);
        });
        let osc = Arc::new(crate::clipboard::RecordingSink::default());
        let (tx, rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(FakeRunner::new()), tx)
            .with_command_sink(sink)
            .with_clipboard(osc.clone())
            .with_clipboard_command(Some(argv.iter().map(OsString::from).collect()));
        (runtime, osc, log_rx, rx)
    }

    /// The next Command log event, waiting at most a few seconds.
    fn next_event(log: &std::sync::mpsc::Receiver<CommandEvent>) -> CommandEvent {
        log.recv_timeout(Duration::from_secs(10))
            .expect("a Command log event")
    }

    #[test]
    fn with_no_clipboard_command_a_copy_is_osc_52() {
        let osc = Arc::new(crate::clipboard::RecordingSink::default());
        let (tx, _rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(FakeRunner::new()), tx).with_clipboard(osc.clone());
        assert_eq!(runtime.copy_to_clipboard("abc"), Ok(()));
        assert_eq!(osc.copies(), [crate::clipboard::osc52("abc").unwrap()]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_clipboard_command_gets_the_cleaned_text_on_stdin_and_not_in_argv() {
        let tmp = crate::git::fake::TempDir::new("clipboard-command");
        let out = tmp.path().join("copied");
        // The test's own fake clipboard program; Kika itself never builds a shell line.
        let script = format!("cat > '{}'", out.display());
        let (runtime, osc, log, mut rx) = clipboard_runtime(&["sh", "-c", &script]);

        assert_eq!(runtime.copy_to_clipboard("hi\x1b"), Ok(()));
        let CommandEvent::Started { id, line, .. } = next_event(&log) else {
            panic!("expected the program to be logged first");
        };
        assert_eq!(line, format!("sh -c {script}"));
        assert!(!line.contains("hi"), "{line}");
        match next_event(&log) {
            CommandEvent::Finished {
                id: done, outcome, ..
            } => {
                assert_eq!(done, id);
                assert_eq!(outcome, CommandOutcome::Ok);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(std::fs::read(&out).unwrap(), b"hi");
        assert!(osc.copies().is_empty(), "the program replaces OSC 52");
        assert!(rx.try_recv().is_err(), "a copy that worked sends nothing");
    }

    /// Hidden: run by `program`'s decoy test, from a directory of decoys
    /// with only relative and empty `PATH` entries.
    #[cfg(unix)]
    #[tokio::test]
    async fn decoy_child_open_url_and_clipboard() {
        if !crate::program::in_decoy_child() {
            return;
        }
        let (tx, mut rx) = mpsc::unbounded_channel();
        let url = vec!["kika-decoy".into(), "https://example.com".into()];
        pull_requests::effects::open_url(url, CommandSink::new(), tx.clone());
        let copy = vec!["kika-decoy".into()];
        run_clipboard_command(
            copy,
            b"x".to_vec(),
            CommandSink::new(),
            tx,
            CLIPBOARD_TIMEOUT,
        );
        let clear = |notice: &str| {
            assert!(notice.contains(crate::program::NOT_FOUND), "{notice}");
            assert!(!notice.chars().any(char::is_control), "{notice:?}");
        };
        match rx.recv().await {
            Some(AppIntent::PullRequests(PullRequestsIntent::OpenUrlFailed(notice))) => {
                clear(&notice)
            }
            other => panic!("{other:?}"),
        }
        match rx.recv().await {
            Some(AppIntent::CopyFailed(notice)) => clear(&notice),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn a_clipboard_command_that_cannot_start_says_so_later() {
        let (runtime, osc, log, mut rx) = clipboard_runtime(&["/nonexistent/kika-clip"]);
        assert_eq!(runtime.copy_to_clipboard("abc"), Ok(()));
        match rx.recv().await {
            Some(AppIntent::CopyFailed(message)) => {
                assert!(
                    message.starts_with("Could not start /nonexistent/kika-clip: "),
                    "{message}"
                );
                assert!(
                    message.ends_with(" Check clipboard.command in the global config."),
                    "{message}"
                );
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(next_event(&log), CommandEvent::Started { .. }));
        assert!(matches!(
            next_event(&log),
            CommandEvent::Finished {
                outcome: CommandOutcome::NotStarted,
                ..
            }
        ));
        assert!(osc.copies().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_clipboard_command_that_fails_names_its_exit_status() {
        let (runtime, osc, _log, mut rx) = clipboard_runtime(&["false"]);
        assert_eq!(runtime.copy_to_clipboard("abc"), Ok(()));
        match rx.recv().await {
            Some(AppIntent::CopyFailed(message)) => {
                assert!(
                    message.starts_with("false failed: exit status: 1"),
                    "{message}"
                );
                assert!(message.ends_with(" Check clipboard.command in the global config."));
            }
            other => panic!("{other:?}"),
        }
        assert!(osc.copies().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_clipboard_command_that_hangs_is_stopped_and_reported() {
        let (sink, log, tx, mut rx) = clipboard_sink();
        let start = std::time::Instant::now();
        // It never reads its input, so a large text would fill the pipe too.
        let worker = run_clipboard_command(
            vec!["sleep".into(), "30".into()],
            vec![b'x'; crate::clipboard::MAX_COPY],
            sink,
            tx,
            Duration::from_millis(200),
        )
        .expect("the program started");
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "it returns at once"
        );
        match rx.recv().await {
            Some(AppIntent::CopyFailed(message)) => {
                assert!(message.starts_with("sleep did not finish"), "{message}");
            }
            other => panic!("{other:?}"),
        }
        assert!(start.elapsed() < Duration::from_secs(10));
        assert!(matches!(next_event(&log), CommandEvent::Started { .. }));
        assert!(matches!(
            next_event(&log),
            CommandEvent::Finished {
                outcome: CommandOutcome::Failed(_),
                ..
            }
        ));
        assert!(
            ends_within(worker, Duration::from_millis(200) + Duration::from_secs(2)),
            "the worker thread is gone"
        );
    }

    /// The pid a test's fake program wrote to `path`.
    #[cfg(unix)]
    fn read_pid(path: &std::path::Path) -> libc::pid_t {
        std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
            .trim()
            .parse()
            .unwrap()
    }

    /// Whether no process, not even an unreaped one, has `pid`.
    #[cfg(unix)]
    fn gone(pid: libc::pid_t) -> bool {
        // SAFETY: signal 0 only checks whether the process exists.
        let result = unsafe { libc::kill(pid, 0) };
        result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }

    /// Wait up to `within` for `pid` to be gone. A killed orphan is reaped
    /// by the system, not by Kika, so it may linger for a moment.
    #[cfg(unix)]
    fn assert_gone_within(pid: libc::pid_t, within: Duration) {
        let deadline = std::time::Instant::now() + within;
        while !gone(pid) {
            assert!(std::time::Instant::now() < deadline, "{pid} still exists");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Whether `worker` has ended within `within`; joined if it has.
    #[cfg(unix)]
    fn ends_within(worker: std::thread::JoinHandle<()>, within: Duration) -> bool {
        let deadline = std::time::Instant::now() + within;
        while !worker.is_finished() {
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        worker.join().expect("the clipboard worker did not panic");
        true
    }

    /// A clipboard program's log and where its intents land.
    #[cfg(unix)]
    fn clipboard_sink() -> (
        CommandSink,
        std::sync::mpsc::Receiver<CommandEvent>,
        mpsc::UnboundedSender<AppIntent>,
        mpsc::UnboundedReceiver<AppIntent>,
    ) {
        let sink = CommandSink::new();
        let (log_tx, log) = std::sync::mpsc::channel();
        sink.connect(move |ev| {
            let _ = log_tx.send(ev);
        });
        let (tx, rx) = mpsc::unbounded_channel();
        (sink, log, tx, rx)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_clipboard_command_that_times_out_is_reaped_with_all_it_started() {
        let tmp = crate::git::fake::TempDir::new("clipboard-group");
        let helper = tmp.path().join("helper");
        let leader = tmp.path().join("leader");
        // The test's own fake clipboard program; Kika itself never builds a shell line.
        let script = format!(
            "sleep 30 & echo $! > '{}'; echo $$ > '{}'; wait",
            helper.display(),
            leader.display()
        );
        let (sink, log, tx, mut rx) = clipboard_sink();
        // Long enough that the shell has surely written both pids first.
        let timeout = Duration::from_secs(1);
        let worker = run_clipboard_command(
            vec!["sh".into(), "-c".into(), script.into()],
            b"abc".to_vec(),
            sink,
            tx,
            timeout,
        )
        .expect("the program started");
        match rx.recv().await {
            Some(AppIntent::CopyFailed(message)) => {
                assert!(message.starts_with("sh did not finish within"), "{message}");
            }
            other => panic!("{other:?}"),
        }
        assert!(
            ends_within(worker, timeout + Duration::from_secs(2)),
            "the worker thread is gone"
        );
        assert!(gone(read_pid(&leader)), "the program is reaped");
        assert_gone_within(read_pid(&helper), Duration::from_secs(2));
        assert!(matches!(next_event(&log), CommandEvent::Started { .. }));
        assert!(matches!(
            next_event(&log),
            CommandEvent::Finished {
                outcome: CommandOutcome::Failed(_),
                ..
            }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn stopping_a_clipboard_program_reaps_it() {
        let mut command = std::process::Command::new("sleep");
        command.arg("30");
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let mut child = command.spawn().unwrap();
        let pid = libc::pid_t::try_from(child.id()).unwrap();
        crate::features::diff::renderers::external_diff::kill_group(&mut child);
        assert!(gone(pid), "{pid} is neither running nor a zombie");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_helper_holding_stdin_open_leaves_no_thread_behind() {
        let tmp = crate::git::fake::TempDir::new("clipboard-stdin");
        let helper = tmp.path().join("helper");
        // The program exits at once; its helper keeps stdin open, unread.
        let script = format!("sleep 30 0<&0 & echo $! > '{}'; exit 0", helper.display());
        let (sink, log, tx, mut rx) = clipboard_sink();
        // Long enough that the shell has surely exited first.
        let timeout = Duration::from_secs(1);
        let worker = run_clipboard_command(
            vec!["sh".into(), "-c".into(), script.into()],
            // More than any pipe buffer holds.
            vec![b'x'; 1024 * 1024],
            sink,
            tx,
            timeout,
        )
        .expect("the program started");
        let message = match rx.recv().await {
            Some(AppIntent::CopyFailed(message)) => message,
            other => panic!("{other:?}"),
        };
        let ended = ends_within(worker, timeout + Duration::from_secs(2));
        let helper = read_pid(&helper);
        // SAFETY: only sends a signal to the test's own helper.
        unsafe { libc::kill(helper, libc::SIGKILL) };
        assert!(ended, "the worker thread is gone");
        assert!(
            message.starts_with("Could not send the text to sh"),
            "{message}"
        );
        assert!(matches!(next_event(&log), CommandEvent::Started { .. }));
        assert!(matches!(
            next_event(&log),
            CommandEvent::Finished {
                outcome: CommandOutcome::Failed(_),
                ..
            }
        ));
    }

    #[test]
    fn a_copy_failure_never_doubles_the_period() {
        let expected = "x. Check clipboard.command in the global config.";
        for why in ["x", "x."] {
            let message = copy_failure(why);
            assert_eq!(message, expected);
            assert!(!message.contains(".."), "{message}");
        }
    }

    #[test]
    fn text_too_long_for_a_clipboard_command_starts_nothing() {
        let (runtime, osc, log, mut rx) = clipboard_runtime(&["/nonexistent/kika-clip"]);
        let long = "x".repeat(crate::clipboard::MAX_COPY + 1);
        assert_eq!(
            runtime.copy_to_clipboard(&long),
            Err(crate::clipboard::TOO_LONG.to_string())
        );
        assert!(log.try_recv().is_err(), "nothing was started");
        assert!(rx.try_recv().is_err());
        assert!(osc.copies().is_empty());
    }
}
