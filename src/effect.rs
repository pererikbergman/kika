use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;

use crate::ansi;
use crate::clipboard::{self, ClipboardSink, TerminalSink};
use crate::commits;
use crate::config::{self, ConfigPaths};
use crate::diff_mark::DiffEnd;
use crate::diff_stream::DiffStreamer;
use crate::diff_tool;
use crate::external_diff::{ExternalRenderer, RendererKind, discover, read_diff, run_renderer};
use crate::features::branches::tags;
use crate::features::commits::copy;
use crate::features::custom_commands::OutputMode;
use crate::features::{
    blame, conflicts, custom_commands, pull_requests, repos, submodules, sync, worktrees,
};
use crate::forge::merge::MergeMethod;
use crate::forge::review::{self, ReviewError, ReviewLoad};
use crate::forge::task::ForgeContext;
use crate::forge::{CreateError, ForgeStatus};
use crate::forge::{ForgeHost, ForgeRepo};
use crate::git::GitRunner;
use crate::git::askpass::Secret;
use crate::git::branch_tips::read_tips;
use crate::git::branches::BranchName;
use crate::git::branches::{branch_log, list_branches};
use crate::git::comment_prefix::read_comment_prefix;
use crate::git::commit_files::{CommitId, CommitRange, commit_files};
use crate::git::diff::{DiffRequest, EmojiZwj, load_patch, sanitize, terminal_safe};
use crate::git::fixup_base::find_fixup_base;
use crate::git::head::read_head_message;
use crate::git::left_behind::read_left_behind;
use crate::git::log::{LogQuery, branch_commit, log_page, markers};
use crate::git::path::RepoPath;
use crate::git::pr_objects::{self, CommitsHere};
use crate::git::rebase_plan::{parse_todo, validate_edit};
use crate::git::recording::{CommandEvent, CommandOutcome, CommandSink};
use crate::git::reflog::read_reflog;
use crate::git::refs::read_refs;
use crate::git::remote::{AskpassEnv, RemoteOp};
use crate::git::remotes::{RemoteName, list_remotes};
use crate::git::repo_state::{git_dir_command, git_dir_from};
use crate::git::stash::{StashEntry, list_stashes, stash_files};
use crate::git::status::read_status;
use crate::gitignore::{self, IgnoreAdded};
use crate::handoff::HandOff;
use crate::intent::AppIntent;
use crate::op::{GitOp, run_op};
use crate::prompts::read_prompt_facts;
use crate::rebase_todo::{TodoFile, nothing_done, serialize};
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
    /// Find the commits the staged lines come from; always answered by
    /// one `FixupBaseFound`.
    FindFixupBase {
        tag: Tag,
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
        /// The installed git reads `core.commentString` (2.45).
        string_too: bool,
    },
    /// Check `content` as an edit of `expected`, both read with the
    /// `comment` prefix `expected` was loaded with, then write it over the
    /// todo list at `path` only while it still holds `expected`; always
    /// answered by one `RebaseTodoWritten`.
    WriteRebaseTodo {
        tag: Tag,
        path: PathBuf,
        expected: Vec<u8>,
        content: Vec<u8>,
        comment: Vec<u8>,
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
    /// Start the browser the global config names: `argv` is its argv with
    /// the URL last, run without a shell and never waited on here. Only
    /// a failure to start answers, with `OpenUrlFailed`.
    OpenUrl {
        argv: Vec<OsString>,
    },
    /// Open a pull request on `repo` through `gh`, on the configured
    /// `host`; always answered by one `PullRequestCreated`.
    CreatePullRequest {
        tag: Tag,
        host: ForgeHost,
        repo: ForgeRepo,
        /// The branch to merge, `owner:branch` when it is in a fork.
        head: String,
        base: String,
        title: String,
    },
    /// Merge pull request `number` of `repo` by `method` through `gh`, on
    /// the configured `host`, only while its head is still `head_oid`;
    /// always answered by one `PullRequestMerged`.
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
    /// passed. Always answered by one `ReviewQueuesLoaded`.
    LoadReviewQueues {
        tag: Tag,
        hosts: Vec<ForgeHost>,
        repo: Option<ForgeRepo>,
        limited: Vec<(ForgeHost, i64)>,
    },
    /// Read pull request `number`, found as for `LoadReviewQueues`. Always
    /// answered by one `PrDetailLoaded`.
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
    /// `ReviewObjectsChecked`.
    CheckReviewCommits {
        tag: Tag,
        head: CommitId,
        base: CommitId,
        hosts: Vec<ForgeHost>,
        repo: ForgeRepo,
    },
    /// Fetch `oids` from `remote` by id, with git's and ssh's questions
    /// sent as `CredentialAsked`. Always answered by one
    /// `ReviewHeadFetched`.
    FetchReviewCommits {
        tag: Tag,
        remote: RemoteName,
        oids: Vec<CommitId>,
    },
    /// The merge base of `base` and `head`, and the files the pull request
    /// changes from it. Always answered by one `ReviewFilesLoaded`.
    LoadReviewFiles {
        tag: Tag,
        head: CommitId,
        base: CommitId,
    },
}

/// The todo list of the rebase in progress and its `done` list, from
/// `<git dir>/rebase-merge/`, with the comment prefix git reads them by.
/// `None` when there is no todo file: a rebase that is not interactive,
/// or one between steps. A prefix that cannot be read still loads the
/// list, to be shown read-only with why.
fn read_rebase_todo(runner: &dyn GitRunner, string_too: bool) -> Result<Option<TodoFile>, String> {
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
    let comment = read_comment_prefix(runner, string_too);
    Ok(Some(TodoFile {
        path,
        todo,
        done,
        comment,
    }))
}

/// Write an edited todo list, once it is sure to be git's todo file and a
/// safe edit of what was read, both read with `comment`, the prefix the
/// list was loaded with. The model checked the same; this is the check
/// that counts.
fn write_rebase_todo(
    path: &Path,
    expected: &[u8],
    content: &[u8],
    comment: &[u8],
) -> Result<(), String> {
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
    // A list whose prefix could not be read is never edited.
    if comment.is_empty() {
        return Err("No comment prefix to read the todo list with".into());
    }
    let before = parse_todo(expected, comment)?;
    let after = parse_todo(content, comment)?;
    // Kika writes no comments or blank lines, so every line of `content`
    // must be a command git runs; one the prefix hides would be skipped.
    if serialize(&after) != content {
        return Err("A line of the edit would be read as a comment".into());
    }
    let done_empty = dir
        .and_then(|dir| worktree_file::read_file(&dir.join("done"), MAX_TODO).ok())
        .is_some_and(|done| nothing_done(&done, comment));
    validate_edit(&before, &after, done_empty)?;
    worktree_file::replace_file(path, expected, content)
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
                        crate::external_diff::kill_group(&mut child);
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
                    crate::external_diff::kill_group(&mut child);
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

/// Where a create or merge runs `gh`: as for loads, the cache directory,
/// never the repository, so `gh` reads no repository's git config.
pub(crate) fn gh_cwd(forge: &ForgeContext) -> PathBuf {
    forge
        .cache_dir
        .clone()
        .filter(|dir| crate::forge::cache::ensure_dir(dir).is_ok())
        .unwrap_or_else(std::env::temp_dir)
}

/// Seconds since the epoch, which relative commit dates count from.
pub(crate) fn unix_now() -> i64 {
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
            Effect::LoadSubmodules { tag } => submodules::effects::load(runner, tx, tag),
            Effect::LoadWorktrees { tag } => worktrees::effects::load(runner, tx, tag),
            Effect::AddWorktree { tag, path, branch } => {
                worktrees::effects::add(runner, tx, tag, path, branch)
            }
            Effect::RemoveWorktree { tag, path } => {
                worktrees::effects::remove(runner, tx, tag, path)
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
                blame::effects::load(runner, tx, tag, path, self.emoji_zwj)
            }
            Effect::FindFixupBase { tag } => {
                tokio::spawn(async move {
                    // A panic still reports back, or the busy counter would stay raised.
                    let result =
                        tokio::task::spawn_blocking(move || find_fixup_base(runner.as_ref()))
                            .await
                            .unwrap_or_else(|e| {
                                Err(terminal_safe(
                                    format!("Finding the fixup base crashed: {e}").as_bytes(),
                                ))
                            });
                    let _ = tx.send(AppIntent::FixupBaseFound { tag, result });
                });
            }
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
            Effect::LoadRebaseTodo { tag, string_too } => {
                tokio::spawn(async move {
                    // A panic still reports back, or the panel would wait forever.
                    let result = tokio::task::spawn_blocking(move || {
                        read_rebase_todo(runner.as_ref(), string_too)
                    })
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
                comment,
            } => {
                tokio::spawn(async move {
                    // A panic still reports back, or the todo would stay locked.
                    let result = tokio::task::spawn_blocking(move || {
                        write_rebase_todo(&path, &expected, &content, &comment)
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
            Effect::Fetch { tag } => sync::effects::fetch(runner, tx, tag),
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
            } => {
                // No forge client, or a repository `[forge.allow]` leaves
                // out (the model never asks for one): nothing may reach
                // the network.
                let Some(forge) = self.forge.clone().filter(|_| host.allows(&repo)) else {
                    let _ = tx.send(AppIntent::PullRequestMerged {
                        tag,
                        number,
                        result: Err(CreateError::Status(ForgeStatus::Failed)),
                    });
                    return;
                };
                tokio::spawn(async move {
                    let result = tokio::task::spawn_blocking(move || {
                        let cwd = gh_cwd(&forge);
                        let gh = crate::forge::github::Gh {
                            runner: forge.gh.as_ref(),
                            sink: &forge.sink,
                            host: &host,
                            cwd,
                            now: unix_now(),
                        };
                        gh.merge_pull(&repo, number, method, &head_oid)
                    })
                    .await
                    // A panic still answers, or the merge would stay in flight.
                    .unwrap_or(Err(CreateError::Status(ForgeStatus::Failed)));
                    let _ = tx.send(AppIntent::PullRequestMerged {
                        tag,
                        number,
                        result,
                    });
                });
            }
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
            } => {
                let forge = self.forge.clone();
                tokio::spawn(async move {
                    let answer = tokio::task::spawn_blocking(move || {
                        let Some(forge) = forge else {
                            // No forge client: nothing may reach the network.
                            return (None, Err(ReviewError::NoForgeHost));
                        };
                        let load = ReviewLoad {
                            hosts: &hosts,
                            known: repo.as_ref(),
                            limited: &limited,
                            now: unix_now(),
                        };
                        review::run(runner.as_ref(), &forge, &load, |gh, repo| {
                            gh.fetch_queues(repo)
                        })
                    })
                    .await
                    // A panic still answers, or the load would stay in flight.
                    .unwrap_or((None, Err(ReviewError::Status(ForgeStatus::Failed))));
                    let (repo, result) = answer;
                    let _ = tx.send(AppIntent::ReviewQueuesLoaded { tag, repo, result });
                });
            }
            Effect::LoadPrDetail {
                tag,
                hosts,
                repo,
                number,
                limited,
            } => {
                let forge = self.forge.clone();
                tokio::spawn(async move {
                    let answer = tokio::task::spawn_blocking(move || {
                        let Some(forge) = forge else {
                            return (None, Err(ReviewError::NoForgeHost));
                        };
                        let load = ReviewLoad {
                            hosts: &hosts,
                            known: repo.as_ref(),
                            limited: &limited,
                            now: unix_now(),
                        };
                        review::run(runner.as_ref(), &forge, &load, |gh, repo| {
                            gh.fetch_detail(repo, number)
                        })
                    })
                    .await
                    .unwrap_or((None, Err(ReviewError::Status(ForgeStatus::Failed))));
                    let (repo, result) = answer;
                    let _ = tx.send(AppIntent::PrDetailLoaded {
                        tag,
                        number,
                        repo,
                        result,
                    });
                });
            }
            Effect::CheckReviewCommits {
                tag,
                head,
                base,
                hosts,
                repo,
            } => {
                tokio::spawn(async move {
                    // A panic still answers, or the files screen would wait forever.
                    let result = tokio::task::spawn_blocking(move || {
                        let missing =
                            pr_objects::missing_commits(runner.as_ref(), &[&head, &base])?;
                        if missing.is_empty() {
                            return Ok(CommitsHere::All);
                        }
                        Ok(CommitsHere::Missing {
                            oids: missing,
                            remote: review::fetch_remote(runner.as_ref(), &hosts, &repo),
                        })
                    })
                    .await
                    .unwrap_or_else(|e| {
                        Err(terminal_safe(
                            format!("git cat-file crashed: {e}").as_bytes(),
                        ))
                    });
                    let _ = tx.send(AppIntent::ReviewObjectsChecked { tag, result });
                });
            }
            Effect::FetchReviewCommits { tag, remote, oids } => {
                let slot = Arc::clone(&self.credential);
                tokio::spawn(async move {
                    let bridge_tx = tx.clone();
                    // A panic still answers, or `op_seq` would block every later op.
                    let result = tokio::task::spawn_blocking(move || {
                        let bridge = Bridge::open(tag, bridge_tx, slot);
                        pr_objects::fetch_commits(
                            runner.as_ref(),
                            &remote,
                            &oids,
                            bridge.env.as_ref(),
                        )
                    })
                    .await
                    .unwrap_or_else(|e| Err(terminal_safe(format!("git crashed: {e}").as_bytes())));
                    let _ = tx.send(AppIntent::ReviewHeadFetched { tag, result });
                });
            }
            Effect::LoadReviewFiles { tag, head, base } => {
                tokio::spawn(async move {
                    let result = tokio::task::spawn_blocking(move || {
                        pr_objects::pr_files(runner.as_ref(), &base, &head)
                    })
                    .await
                    .unwrap_or_else(|e| {
                        Err(terminal_safe(
                            format!("git diff-tree crashed: {e}").as_bytes(),
                        ))
                    });
                    let _ = tx.send(AppIntent::ReviewFilesLoaded { tag, result });
                });
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
    use std::io;

    use tokio::sync::mpsc;

    use super::*;
    #[cfg(unix)]
    use crate::features::custom_commands::CommandFailure;
    use crate::git::remote::RemoteFailure;
    use crate::test_support::forge_tag;

    #[tokio::test]
    async fn review_loads_answer_once_and_reach_nobody_without_a_forge_client() {
        use crate::forge::github::{FakeGh, answer};
        use crate::forge::review::fixtures::DETAIL;
        use crate::git::recording::CommandSink;

        let github = vec![ForgeHost::parse("github.com").unwrap()];
        let repo = ForgeRepo {
            host: "github.com".into(),
            owner: "acme".into(),
            name: "widgets".into(),
        };
        let git = Arc::new(FakeRunner::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let bare = Runtime::new(git.clone(), tx);
        bare.spawn(Effect::LoadReviewQueues {
            tag: forge_tag(),
            hosts: github.clone(),
            repo: Some(repo.clone()),
            limited: vec![],
        });
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::ReviewQueuesLoaded {
                tag: forge_tag(),
                repo: None,
                result: Err(ReviewError::NoForgeHost),
            })
        );

        let gh = Arc::new(FakeGh::new().reply(answer(DETAIL)));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(git.clone(), tx).with_forge(Some(ForgeContext {
            gh: gh.clone(),
            sink: CommandSink::new(),
            cache_dir: None,
        }));
        runtime.spawn(Effect::LoadPrDetail {
            tag: forge_tag(),
            hosts: github,
            repo: Some(repo.clone()),
            number: 42,
            limited: vec![],
        });
        match rx.recv().await {
            Some(AppIntent::PrDetailLoaded {
                number: 42,
                repo: Some(found),
                result: Ok(Some(detail)),
                ..
            }) => {
                assert_eq!(found, repo);
                assert_eq!(detail.number, 42);
            }
            other => panic!("{other:?}"),
        }
        git.assert_done();
        assert_eq!(gh.calls().len(), 1);
    }

    #[tokio::test]
    async fn a_merge_runs_gh_in_the_cache_dir_and_answers_once() {
        use crate::forge::github::{FakeGh, GhCall, answer};

        let tmp = crate::git::fake::TempDir::new("forge-effect-merge");
        let cache_dir = tmp.path().join("forge");
        let git = Arc::new(FakeRunner::new());
        let gh = Arc::new(FakeGh::new().reply(answer(
            "HTTP/2.0 200 OK\r\n\r\n{\"merged\":true,\"message\":\"Pull Request successfully merged\"}",
        )));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(git.clone(), tx).with_forge(Some(ForgeContext {
            gh: gh.clone(),
            sink: CommandSink::new(),
            cache_dir: Some(cache_dir.clone()),
        }));
        let merge = Effect::MergePullRequest {
            tag: forge_tag(),
            host: ForgeHost::parse("github.com").unwrap(),
            repo: ForgeRepo {
                host: "github.com".into(),
                owner: "acme".into(),
                name: "widgets".into(),
            },
            number: 42,
            method: MergeMethod::Rebase,
            head_oid: CommitId::parse(&"1".repeat(40)).unwrap(),
        };
        runtime.spawn(merge.clone());
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::PullRequestMerged {
                tag: forge_tag(),
                number: 42,
                result: Ok(()),
            })
        );
        git.assert_done();
        let [call] = gh.calls().try_into().unwrap();
        assert_eq!(call.cwd, cache_dir);
        assert!(matches!(
            &call.kind,
            GhCall::Merge { path, .. } if path == "repos/acme/widgets/pulls/42/merge"
        ));

        // Without a forge client, nothing is asked.
        let (tx, mut rx) = mpsc::unbounded_channel();
        Runtime::new(git, tx).spawn(merge);
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::PullRequestMerged {
                tag: forge_tag(),
                number: 42,
                result: Err(CreateError::Status(ForgeStatus::Failed)),
            })
        );
        assert_eq!(gh.calls().len(), 1);
    }

    #[tokio::test]
    async fn a_merge_on_a_repository_the_allow_list_leaves_out_runs_no_gh() {
        use crate::forge::allow::AllowRule;
        use crate::forge::github::FakeGh;

        let git = Arc::new(FakeRunner::new());
        let gh = Arc::new(FakeGh::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(git.clone(), tx).with_forge(Some(ForgeContext {
            gh: gh.clone(),
            sink: CommandSink::new(),
            cache_dir: None,
        }));
        runtime.spawn(Effect::MergePullRequest {
            tag: forge_tag(),
            host: ForgeHost::parse("github.com")
                .unwrap()
                .with_allow(vec![AllowRule::parse("other").unwrap()]),
            repo: ForgeRepo {
                host: "github.com".into(),
                owner: "acme".into(),
                name: "widgets".into(),
            },
            number: 42,
            method: MergeMethod::Merge,
            head_oid: CommitId::parse(&"1".repeat(40)).unwrap(),
        });
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::PullRequestMerged {
                tag: forge_tag(),
                number: 42,
                result: Err(CreateError::Status(ForgeStatus::Failed)),
            })
        );
        git.assert_done();
        assert_eq!(gh.calls(), []);
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
            fixup: None,
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
                facts: Some(crate::prompts::PromptFacts {
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
            assert!(
                write_rebase_todo(&path, TODO, bad, b"#").is_err(),
                "{bad:?}"
            );
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
            assert!(
                write_rebase_todo(&path, &listed, edit, b"#").is_err(),
                "{odd:?}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), listed);
        }
        // Git stops at a NUL; a list holding one is not edited.
        let nul = b"pick 1111 one\nupdate-ref refs/heads/x\npick 2222 two\0\n";
        std::fs::write(&path, nul).unwrap();
        let moved = b"pick 1111 one\nupdate-ref refs/heads/x\npick 2222 two\n";
        assert!(write_rebase_todo(&path, nul, moved, b"#").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), nul);
        std::fs::write(&path, TODO).unwrap();
        let other = tmp.path().join("git-rebase-todo");
        std::fs::write(&other, TODO).unwrap();
        let edit = b"pick 1111 one\nbreak\nupdate-ref refs/heads/x\ndrop 2222 two\n";
        assert!(write_rebase_todo(&other, TODO, edit, b"#").is_err());
        assert_eq!(
            // The same commands, but not the bytes on disk.
            write_rebase_todo(&path, &TODO[..TODO.len() - 7], edit, b"#"),
            Err(crate::worktree_file::CHANGED_ON_DISK.to_string())
        );
        assert_eq!(write_rebase_todo(&path, TODO, edit, b"#"), Ok(()));
        assert_eq!(std::fs::read(&path).unwrap(), edit);
    }

    #[test]
    fn a_todo_write_refuses_a_first_fold_only_when_nothing_is_done() {
        let tmp = crate::git::fake::TempDir::new("effect-todo");
        let todo = b"pick 1111 one\npick 2222 two\n";
        let path = rebase_dir(&tmp, todo, b"");
        let fold = b"fixup 1111 one\npick 2222 two\n";
        assert!(write_rebase_todo(&path, todo, fold, b"#").is_err());
        std::fs::write(path.with_file_name("done"), "pick 0000 z\n").unwrap();
        assert_eq!(write_rebase_todo(&path, todo, fold, b"#"), Ok(()));
    }

    const COMMENT_CONFIG: [&str; 4] = [
        "config",
        "-z",
        "--get-regexp",
        r"^core\.comment(char|string)$",
    ];

    #[test]
    fn the_todo_is_read_from_git_s_dir_with_its_comment_prefix_and_none_without_one() {
        let tmp = crate::git::fake::TempDir::new("effect-todo");
        let mut git_dir = tmp.path().as_os_str().as_encoded_bytes().to_vec();
        git_dir.push(b'\n');
        let runner = FakeRunner::new()
            .expect(&["rev-parse", "--absolute-git-dir"], ok(&git_dir))
            .expect(&["rev-parse", "--absolute-git-dir"], ok(&git_dir))
            .expect(&COMMENT_CONFIG, ok(b"core.commentstring\n//\0"))
            .expect(&["rev-parse", "--absolute-git-dir"], ok(&git_dir))
            .expect(&COMMENT_CONFIG, ok(b"core.commentstring\n//\0"));
        assert_eq!(read_rebase_todo(&runner, true), Ok(None));
        let path = rebase_dir(&tmp, TODO, b"pick 0000 z\n");
        let file = |comment: &[u8]| {
            Ok(Some(TodoFile {
                path: path.clone(),
                todo: TODO.to_vec(),
                done: Some(b"pick 0000 z\n".to_vec()),
                comment: Ok(comment.to_vec()),
            }))
        };
        assert_eq!(read_rebase_todo(&runner, true), file(b"//"));
        // An older git ignores `core.commentString`.
        assert_eq!(read_rebase_todo(&runner, false), file(b"#"));
        runner.assert_done();
    }

    #[test]
    fn a_comment_prefix_that_cannot_be_read_still_loads_the_todo() {
        let tmp = crate::git::fake::TempDir::new("effect-todo");
        let mut git_dir = tmp.path().as_os_str().as_encoded_bytes().to_vec();
        git_dir.push(b'\n');
        let runner = FakeRunner::new()
            .expect(&["rev-parse", "--absolute-git-dir"], ok(&git_dir))
            .expect(&COMMENT_CONFIG, fail("fatal: bad config line 1\n"));
        rebase_dir(&tmp, TODO, b"");
        let file = read_rebase_todo(&runner, true).unwrap().unwrap();
        assert_eq!(file.todo, TODO);
        assert_eq!(file.comment, Err("fatal: bad config line 1".into()));
        runner.assert_done();
    }

    #[test]
    fn a_todo_write_refuses_a_line_the_prefix_would_hide() {
        let tmp = crate::git::fake::TempDir::new("effect-todo");
        let todo = b"b Rebase 1..2 onto 1\npick 1111 one\npick 2222 two\n";
        let path = rebase_dir(&tmp, todo, b"pick 0000 z\n");
        // The break would read as a comment, and git would not stop.
        let stop = b"pick 1111 one\nbreak\npick 2222 two\n";
        assert!(write_rebase_todo(&path, todo, stop, b"b").is_err());
        let moved = b"pick 2222 two\npick 1111 one\n";
        assert!(write_rebase_todo(&path, todo, moved, b"p").is_err());
        // Anything but what Kika serialises is refused, blank lines too.
        let blank = b"pick 2222 two\n\npick 1111 one\n";
        assert!(write_rebase_todo(&path, todo, blank, b"b").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), todo);
    }

    #[test]
    fn a_todo_write_reads_both_lists_with_the_loaded_comment_prefix() {
        let tmp = crate::git::fake::TempDir::new("effect-todo");
        let todo = b"; Rebase 1..2 onto 1\npick 1111 one\npick 2222 two\n; help\n";
        let path = rebase_dir(&tmp, todo, b"pick 0000 z\n");
        let edit = b"pick 2222 two\npick 1111 one\n";
        assert!(write_rebase_todo(&path, todo, edit, b"#").is_err());
        assert!(write_rebase_todo(&path, todo, edit, b"").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), todo);
        assert_eq!(write_rebase_todo(&path, todo, edit, b";"), Ok(()));
        assert_eq!(std::fs::read(&path).unwrap(), edit);
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
            Some(AppIntent::OpenUrlFailed(notice)) => clear(&notice),
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
        crate::external_diff::kill_group(&mut child);
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
