//! Pull requests from GitHub, read through `gh api`. `gh` holds the login,
//! so Kika never sees a token: it passes none, stores none and draws none.
//! `gh`'s stderr is only matched against fixed markers, never kept, and of
//! the headers only the ETag (validated) and the rate-limit numbers are read.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::json;

use super::cli::CliStatus;
use super::head::Head;
use super::url::pull_url;
use super::{CiState, CreateError, ForgeHost, ForgeRepo, ForgeStatus, PrBadge, PrState};
use kika_git::branches::BranchName;
use kika_git::recording::{CommandEvent, CommandOutcome, CommandSink};
use kika_git::safe_text::terminal_safe;

/// Branches asked about in one call.
pub const BATCH: usize = 50;
/// Branches asked about at all; more are left without a badge.
pub const MAX_BRANCHES: usize = 500;
/// How long one call may take before it counts as offline.
pub const TIMEOUT: Duration = Duration::from_secs(20);
/// How long the output may take to arrive once `gh` has ended.
const DRAIN: Duration = Duration::from_secs(2);
/// The most of `gh`'s output read; a real answer is far smaller.
const MAX_OUTPUT: u64 = 8 * 1024 * 1024;
/// The shortest and longest wait after a rate limit, in seconds: a bad or
/// hostile header can neither hammer GitHub nor silence Kika for long.
pub(crate) const MIN_BACKOFF: i64 = 60;
pub(crate) const MAX_BACKOFF: i64 = 3600;
/// The longest ETag kept or sent.
const MAX_ETAG: usize = 256;
/// The probe's media type header.
const ACCEPT: &str = "Accept: application/vnd.github+json";
/// The most of GitHub's refusal shown, in characters.
pub(super) const MAX_REFUSAL: usize = 400;
/// The longest page URL taken from an answer.
pub(super) const MAX_URL: usize = 2048;

/// Set on every call: no prompt, no pager, no colour, no update check.
pub const GH_ENV: [(&str, &str); 5] = [
    ("GH_PROMPT_DISABLED", "1"),
    ("GH_NO_UPDATE_NOTIFIER", "1"),
    ("GH_SPINNER_DISABLED", "1"),
    ("NO_COLOR", "1"),
    ("GH_PAGER", "cat"),
];

/// Removed from every call, so only the arguments pick the host and no
/// repository is implied. Kika's hidden-mode keys never reach a child.
pub const GH_ENV_REMOVE: [&str; 4] = [
    "GH_HOST",
    "GH_REPO",
    kika_git::askpass::SOCKET_ENV,
    kika_git::rebase_plan::PLAN_ENV,
];

/// What one `gh api` call asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhCall {
    /// A GraphQL query; `body` is the JSON for stdin.
    Graphql { body: Vec<u8> },
    /// A REST read of `path`, made conditional by `etag`: a cheap way to
    /// learn that nothing changed, since GraphQL has no ETags.
    Probe { path: String, etag: Option<String> },
    /// A REST `POST` to `path`; `body` is the JSON for stdin, so nothing
    /// the user typed or a branch is called reaches argv.
    Create { path: String, body: Vec<u8> },
    /// A REST `PUT` to a pull request's merge `path`; `body` is the JSON
    /// for stdin, holding the method and the head commit it must merge.
    Merge { path: String, body: Vec<u8> },
    /// `gh --version`: local, with no host and no stdin, to learn that
    /// `gh` is installed and usable.
    Version,
}

/// One `gh api` call: the host, and what it asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhCommand {
    pub host: String,
    pub kind: GhCall,
    /// Where `gh` runs: Kika's cache directory, never the repository, so
    /// `gh` reads no repository's git config.
    pub cwd: PathBuf,
    pub timeout: Duration,
}

impl GhCommand {
    /// The arguments after `gh`. `--include` puts the status line and
    /// headers on stdout, since `gh`'s exit code cannot tell a 304 from a
    /// failure. Branch names never appear: they travel in the GraphQL body.
    pub fn args(&self) -> Vec<OsString> {
        let host = self.host.as_str();
        let args: Vec<String> = match &self.kind {
            GhCall::Graphql { .. } => ["api", "graphql", "--include", "--hostname", host]
                .into_iter()
                .chain(["--input", "-"])
                .map(String::from)
                .collect(),
            GhCall::Probe { path, etag } => ["api", "--include", "--hostname", host, "-H", ACCEPT]
                .into_iter()
                .map(String::from)
                .chain(
                    etag.iter()
                        .flat_map(|etag| ["-H".to_string(), format!("If-None-Match: {etag}")]),
                )
                .chain([path.clone()])
                .collect(),
            GhCall::Create { path, .. } => [
                "api",
                "--include",
                "--hostname",
                host,
                "--method",
                "POST",
                path,
                "--input",
                "-",
            ]
            .map(String::from)
            .into(),
            GhCall::Merge { path, .. } => [
                "api",
                "--include",
                "--hostname",
                host,
                "--method",
                "PUT",
                path,
                "--input",
                "-",
            ]
            .map(String::from)
            .into(),
            GhCall::Version => vec!["--version".to_string()],
        };
        args.into_iter().map(OsString::from).collect()
    }

    /// The Command log's line: the whole argv, with the ETag's value
    /// replaced by `<etag>`.
    pub fn display_line(&self) -> String {
        match &self.kind {
            GhCall::Graphql { .. } => {
                format!(
                    "gh api graphql --include --hostname {} --input -",
                    self.host
                )
            }
            GhCall::Probe { path, etag } => {
                let condition = match etag {
                    Some(_) => " -H \"If-None-Match: <etag>\"",
                    None => "",
                };
                format!(
                    "gh api --include --hostname {} -H \"{ACCEPT}\"{condition} {path}",
                    self.host
                )
            }
            GhCall::Create { path, .. } => format!(
                "gh api --include --hostname {} --method POST {path} --input -",
                self.host
            ),
            GhCall::Merge { path, .. } => format!(
                "gh api --include --hostname {} --method PUT {path} --input -",
                self.host
            ),
            GhCall::Version => "gh --version".to_string(),
        }
    }

    /// What goes on stdin: the JSON body, or nothing.
    pub fn stdin(&self) -> Vec<u8> {
        match &self.kind {
            GhCall::Graphql { body } | GhCall::Create { body, .. } | GhCall::Merge { body, .. } => {
                body.clone()
            }
            GhCall::Probe { .. } | GhCall::Version => Vec::new(),
        }
    }
}

/// What a finished `gh` left behind.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GhOutput {
    /// `None` when it was killed or ended by a signal.
    pub exit: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// Killed after [`GhCommand::timeout`].
    pub timed_out: bool,
}

/// Runs `gh`; tests swap in [`FakeGh`].
pub trait GhRunner: Send + Sync {
    fn run(&self, cmd: &GhCommand) -> io::Result<GhOutput>;
}

/// The real `gh`.
pub struct CliGh;

impl GhRunner for CliGh {
    fn run(&self, cmd: &GhCommand) -> io::Result<GhOutput> {
        let mut command = kika_git::program::command(OsStr::new("gh"))?;
        command.args(cmd.args());
        for (key, value) in GH_ENV {
            command.env(key, value);
        }
        for key in GH_ENV_REMOVE {
            command.env_remove(key);
        }
        run_piped(command, &cmd.cwd, cmd.stdin(), cmd.timeout)
    }
}

/// Run a forge CLI's `command` in `cwd` with `body` on stdin, killed after
/// `timeout`, and keep at most [`MAX_OUTPUT`] of each output stream.
pub(crate) fn run_piped(
    mut command: std::process::Command,
    cwd: &std::path::Path,
    body: Vec<u8>,
    timeout: Duration,
) -> io::Result<GhOutput> {
    command
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let stdin = child.stdin.take();
    // Dropping the pipe after writing sends EOF. Never joined: a
    // process that stops reading must not hold this call.
    std::thread::spawn(move || {
        if let Some(mut pipe) = stdin {
            let _ = pipe.write_all(&body);
        }
    });
    let reader = |pipe: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(pipe) = pipe {
                let _ = pipe.take(MAX_OUTPUT).read_to_end(&mut bytes);
            }
            let _ = tx.send(bytes);
        });
        rx
    };
    let stdout = reader(child.stdout.take().map(|p| Box::new(p) as _));
    let stderr = reader(child.stderr.take().map(|p| Box::new(p) as _));
    let deadline = Instant::now() + timeout;
    let (status, timed_out) = loop {
        match child.try_wait() {
            Ok(Some(status)) => break (Some(status), false),
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break (None, true);
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    // The CLI has ended, but a process it started could still hold the
    // pipes open; wait for the output only so long, then go without.
    let drained = Instant::now() + DRAIN;
    let collect = |rx: std::sync::mpsc::Receiver<Vec<u8>>| {
        let left = drained.saturating_duration_since(Instant::now());
        rx.recv_timeout(left).unwrap_or_default()
    };
    Ok(GhOutput {
        exit: status.and_then(|s| s.code()),
        stdout: collect(stdout),
        stderr: collect(stderr),
        timed_out,
    })
}

/// What one call came to, read from its status line and headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// 304: nothing changed since the ETag that was sent.
    NotModified,
    /// A 2xx answer, with its ETag when it gave a valid one.
    Body {
        etag: Option<String>,
        body: Vec<u8>,
    },
    /// GitHub refuses more calls until `until` (seconds since the epoch).
    Limited {
        until: i64,
    },
    Failed(ForgeStatus),
}

/// What a fetch found: the badges by local branch, and whether the
/// repository is a fork, which only the GraphQL answer can tell.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Fetched {
    pub badges: HashMap<BranchName, PrBadge>,
    /// GitHub named a `parent` for the repository.
    pub forked: bool,
}

/// Why a fetch gave no badges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Status(ForgeStatus),
    /// Rate limited until this second.
    Limited {
        until: i64,
    },
    /// The forge kind cannot do this yet; nothing was asked.
    Unsupported(crate::forge::Unsupported),
}

/// The `gh` calls of one load: who runs them, where they are logged, the
/// host they go to and the time rate limits are measured from. Only the
/// GitHub forge adapter that
/// [`ForgeContext::forge`](super::task::ForgeContext::forge) hands out
/// holds one, behind the host's allow gate, so its fields stay inside
/// forge.
pub struct Gh<'a> {
    pub(crate) runner: &'a dyn GhRunner,
    pub(crate) sink: &'a CommandSink,
    pub(crate) host: &'a ForgeHost,
    pub(crate) cwd: PathBuf,
    pub(crate) now: i64,
}

impl<'a> Gh<'a> {
    /// A `gh` past the gate, for tests outside forge that need what a
    /// scripted answer parses to. It runs in `/c`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn fixture(
        runner: &'a dyn GhRunner,
        sink: &'a CommandSink,
        host: &'a ForgeHost,
        now: i64,
    ) -> Self {
        Self {
            runner,
            sink,
            host,
            cwd: "/c".into(),
            now,
        }
    }
}

impl Gh<'_> {
    /// Ask whether `repo`'s pull requests changed since `etag`. A 304
    /// does not count against the primary rate limit.
    pub fn probe(&self, repo: &ForgeRepo, etag: Option<&str>) -> Answer {
        let path = format!(
            "repos/{}/{}/pulls?state=all&sort=updated&direction=desc&per_page=1",
            repo.owner, repo.name
        );
        let kind = GhCall::Probe {
            path,
            etag: etag.and_then(valid_etag),
        };
        self.call(
            kind,
            |answer| answer,
            |answer| match answer {
                Answer::NotModified | Answer::Body { .. } => CommandOutcome::Ok,
                Answer::Failed(ForgeStatus::Cli(CliStatus::NotInstalled)) => {
                    CommandOutcome::NotStarted
                }
                _ => CommandOutcome::Failed(None),
            },
        )
    }

    /// The pull request of each of `heads`, from `repo` or its parent,
    /// asked in batches of [`BATCH`], one call after another. The first
    /// failure or rate limit ends it.
    pub fn fetch_badges(&self, repo: &ForgeRepo, heads: &[Head]) -> Result<Fetched, Refusal> {
        let mut fetched = Fetched::default();
        let heads = &heads[..heads.len().min(MAX_BRANCHES)];
        for batch in heads.chunks(BATCH) {
            let kind = GhCall::Graphql {
                body: request_body(repo, batch),
            };
            let result = self.call(
                kind,
                |answer| match answer {
                    Answer::Body { body, .. } => read_answer(&body, batch).map_err(Refusal::Status),
                    Answer::Limited { until } => Err(Refusal::Limited { until }),
                    // A 304 to a call that sent no ETag is not an answer.
                    Answer::NotModified => Err(Refusal::Status(ForgeStatus::Failed)),
                    Answer::Failed(status) => Err(Refusal::Status(status)),
                },
                |result| match result {
                    Ok(_) => CommandOutcome::Ok,
                    Err(Refusal::Status(ForgeStatus::Cli(CliStatus::NotInstalled))) => {
                        CommandOutcome::NotStarted
                    }
                    Err(_) => CommandOutcome::Failed(None),
                },
            );
            let (badges, forked) = result?;
            fetched.forked |= forked;
            for (head, badge) in batch.iter().zip(badges) {
                if let Some(badge) = badge {
                    fetched.badges.insert(head.branch.clone(), badge);
                }
            }
        }
        Ok(fetched)
    }

    /// Open a pull request on `repo` from `head` (`owner:branch` for a
    /// fork) into `base`. Its number and page come back; the page is
    /// rebuilt from `repo` when the answer names one off the host.
    pub fn create_pull(
        &self,
        repo: &ForgeRepo,
        head: &str,
        base: &str,
        title: &str,
    ) -> Result<(u64, String), CreateError> {
        let body = json!({ "title": title, "head": head, "base": base });
        let kind = GhCall::Create {
            path: format!("repos/{}/{}/pulls", repo.owner, repo.name),
            body: serde_json::to_vec(&body).unwrap_or_default(),
        };
        let now = self.now;
        let host = self.host.as_str();
        self.run_logged(
            kind,
            |output| read_created(output, now, host, repo),
            |result| match result {
                Ok(_) => CommandOutcome::Ok,
                Err(CreateError::Status(ForgeStatus::Cli(CliStatus::NotInstalled))) => {
                    CommandOutcome::NotStarted
                }
                Err(_) => CommandOutcome::Failed(None),
            },
        )
    }

    /// Run one call, shown in the Command log under its own id, and read
    /// its answer with `read`.
    pub(super) fn call<T>(
        &self,
        kind: GhCall,
        read: impl FnOnce(Answer) -> T,
        outcome: impl FnOnce(&T) -> CommandOutcome,
    ) -> T {
        let now = self.now;
        self.run_logged(kind, |output| read(classify(output, now)), outcome)
    }

    /// Run one call, shown in the Command log under its own id, and read
    /// what `gh` left with `read`.
    pub(super) fn run_logged<T>(
        &self,
        kind: GhCall,
        read: impl FnOnce(io::Result<GhOutput>) -> T,
        outcome: impl FnOnce(&T) -> CommandOutcome,
    ) -> T {
        let cmd = GhCommand {
            host: self.host.as_str().to_string(),
            kind,
            cwd: self.cwd.clone(),
            timeout: TIMEOUT,
        };
        logged(
            self.sink,
            cmd.display_line(),
            || read(self.runner.run(&cmd)),
            outcome,
        )
    }
}

/// Run `call`, shown in the Command log as `line` under its own id, with
/// the outcome `outcome` reads from its result.
pub(crate) fn logged<T>(
    sink: &CommandSink,
    line: String,
    call: impl FnOnce() -> T,
    outcome: impl FnOnce(&T) -> CommandOutcome,
) -> T {
    let id = sink.next_id();
    sink.send(CommandEvent::Started {
        id,
        line,
        attached: false,
    });
    let start = Instant::now();
    let result = call();
    sink.send(CommandEvent::Finished {
        id,
        outcome: outcome(&result),
        elapsed: start.elapsed(),
    });
    result
}

/// What a create call came to. Rate limits, logins and the network are
/// judged as for any call; a refusal keeps GitHub's message, and only a
/// 201 with a number counts as made.
fn read_created(
    output: io::Result<GhOutput>,
    now: i64,
    host: &str,
    repo: &ForgeRepo,
) -> Result<(u64, String), CreateError> {
    let stdout = output
        .as_ref()
        .map(|o| o.stdout.clone())
        .unwrap_or_default();
    let (status, _, body) = split_include(&stdout);
    match classify(output, now) {
        Answer::Limited { until } => Err(CreateError::RateLimited { until }),
        Answer::Body { .. } if status == Some(201) => {
            let created: Created = serde_json::from_slice(body)
                .map_err(|_| CreateError::Status(ForgeStatus::Failed))?;
            let prefix = format!("https://{host}/");
            let page = created.html_url.filter(|url| {
                url.len() <= MAX_URL
                    && url.to_ascii_lowercase().starts_with(&prefix)
                    && !url.chars().any(|c| c.is_control() || c.is_whitespace())
            });
            let page = page
                .or_else(|| pull_url(repo, created.number))
                .ok_or(CreateError::Status(ForgeStatus::Failed))?;
            Ok((created.number, page))
        }
        Answer::Failed(ForgeStatus::Failed) if status.is_some() => Err(refusal(body).map_or(
            CreateError::Status(ForgeStatus::Failed),
            CreateError::Refused,
        )),
        Answer::Failed(status) => Err(CreateError::Status(status)),
        Answer::Body { .. } | Answer::NotModified => Err(CreateError::Status(ForgeStatus::Failed)),
    }
}

#[derive(Deserialize)]
struct Created {
    number: u64,
    html_url: Option<String>,
}

#[derive(Deserialize)]
struct RestError {
    message: Option<String>,
    #[serde(default)]
    errors: Vec<RestErrorDetail>,
}

#[derive(Deserialize)]
struct RestErrorDetail {
    message: Option<String>,
    field: Option<String>,
    code: Option<String>,
}

/// GitHub's reason for refusing a call, from a REST error body: its
/// message and each detail, terminal-safe and cut to [`MAX_REFUSAL`].
pub(super) fn refusal(body: &[u8]) -> Option<String> {
    let error: RestError = serde_json::from_slice(body).ok()?;
    let details = error.errors.into_iter().filter_map(|detail| {
        detail.message.or_else(|| {
            let parts: Vec<String> = [detail.field, detail.code].into_iter().flatten().collect();
            (!parts.is_empty()).then(|| parts.join(" "))
        })
    });
    let parts: Vec<String> = error.message.into_iter().chain(details).collect();
    if parts.is_empty() {
        return None;
    }
    Some(refusal_text(&parts.join(": ")))
}

/// A forge's reason for a refusal as the user is shown it: terminal-safe
/// and cut to [`MAX_REFUSAL`].
pub(super) fn refusal_text(text: &str) -> String {
    let text = terminal_safe(text.as_bytes());
    match text.char_indices().nth(MAX_REFUSAL) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text,
    }
}

/// Pull requests asked per head ref name: one of another owner's forks
/// may share the name, so the newest few are read and filtered here.
const NODES: usize = 5;

/// The GraphQL query for `count` heads. Fixed text: aliases are `b0…`,
/// and every name is a variable, so nothing from the repository is ever
/// spliced into it. The same aliases are asked of `parent`, which is null
/// unless the repository is a fork, so a fork's pull requests into its
/// upstream are found in the same call.
fn query(count: usize) -> String {
    let mut params = String::from("$owner:String!,$name:String!");
    let mut fields = String::new();
    for i in 0..count {
        params.push_str(&format!(",$b{i}:String!"));
        fields.push_str(&format!(
            " b{i}:pullRequests(headRefName:$b{i},first:{NODES},orderBy:{{field:CREATED_AT,direction:DESC}})\
             {{nodes{{number state isDraft headRepositoryOwner{{login}} \
             commits(last:1){{nodes{{commit{{statusCheckRollup{{state}}}}}}}}}}}}"
        ));
    }
    format!(
        "query({params}){{repository(owner:$owner,name:$name){{{fields} parent{{{fields} }} }}}}"
    )
}

/// `{query, variables}` for stdin. Only the ref names are sent; owners
/// are matched on the answer.
fn request_body(repo: &ForgeRepo, batch: &[Head]) -> Vec<u8> {
    let mut variables = serde_json::Map::new();
    variables.insert("owner".into(), json!(repo.owner));
    variables.insert("name".into(), json!(repo.name));
    for (i, head) in batch.iter().enumerate() {
        variables.insert(format!("b{i}"), json!(head.name));
    }
    serde_json::to_vec(&json!({ "query": query(batch.len()), "variables": variables }))
        .unwrap_or_default()
}

#[derive(Deserialize)]
struct Response {
    data: Option<Data>,
    errors: Option<Vec<GqlError>>,
}

/// Only the errors of an answer, whatever else it holds.
#[derive(Deserialize)]
struct ErrorsOnly {
    errors: Option<Vec<GqlError>>,
}

#[derive(Deserialize)]
struct Data {
    repository: Option<Repository>,
}

/// The aliases `b0…`, and the same aliases on the parent, if any.
#[derive(Deserialize)]
struct Repository {
    parent: Option<HashMap<String, Connection>>,
    #[serde(flatten)]
    aliases: HashMap<String, Connection>,
}

#[derive(Deserialize)]
struct GqlError {
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Deserialize)]
struct Connection {
    nodes: Vec<PrNode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrNode {
    number: u64,
    state: String,
    is_draft: bool,
    /// Null for a deleted fork, which then matches no head.
    #[serde(default)]
    head_repository_owner: Option<Owner>,
    commits: Commits,
}

/// Compared with a head's owner and dropped: never kept or shown.
#[derive(Deserialize)]
struct Owner {
    login: String,
}

#[derive(Deserialize)]
struct Commits {
    nodes: Vec<CommitNode>,
}

#[derive(Deserialize)]
struct CommitNode {
    commit: CommitRollup,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommitRollup {
    status_check_rollup: Option<Rollup>,
}

#[derive(Deserialize)]
struct Rollup {
    state: String,
}

/// Said on stderr when the network is the problem.
pub(crate) const OFFLINE_MARKERS: [&str; 8] = [
    "dial tcp",
    "no such host",
    "connection refused",
    "connection reset",
    "network is unreachable",
    "i/o timeout",
    "tls handshake timeout",
    "deadline exceeded",
];

/// The only headers read; the rest are dropped unseen.
const READ_HEADERS: [&str; 4] = [
    "etag",
    "x-ratelimit-remaining",
    "x-ratelimit-reset",
    "retry-after",
];

/// `--include` output split into its status code, the headers Kika reads
/// (names lower-cased) and the body. Output without a status line is all
/// body.
pub fn split_include(stdout: &[u8]) -> (Option<u16>, Vec<(String, String)>, &[u8]) {
    split_include_reading(stdout, &READ_HEADERS)
}

/// [`split_include`], keeping only the headers named in `read`
/// (lower-case): each forge has its own rate-limit names.
pub(crate) fn split_include_reading<'a>(
    stdout: &'a [u8],
    read: &[&str],
) -> (Option<u16>, Vec<(String, String)>, &'a [u8]) {
    if !stdout.starts_with(b"HTTP/") {
        return (None, Vec::new(), stdout);
    }
    let crlf = find(stdout, b"\r\n\r\n").map(|at| (at, at + 4));
    let lf = find(stdout, b"\n\n").map(|at| (at, at + 2));
    let (head_end, body_start) = match (crlf, lf) {
        (Some(a), Some(b)) => a.min(b),
        (a, b) => a.or(b).unwrap_or((stdout.len(), stdout.len())),
    };
    let head = String::from_utf8_lossy(&stdout[..head_end]);
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok());
    let headers = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            let name = name.trim().to_ascii_lowercase();
            read.contains(&name.as_str())
                .then(|| (name, value.trim().to_string()))
        })
        .collect();
    (status, headers, &stdout[body_start..])
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// `value` when it is an ETag safe to keep and send: ASCII, at most 256
/// bytes, no control characters, `"…"` or `W/"…"` with no quote inside.
pub fn valid_etag(value: &str) -> Option<String> {
    let quoted = value.strip_prefix("W/").unwrap_or(value);
    let inner = quoted.strip_prefix('"')?.strip_suffix('"')?;
    let ok = value.len() <= MAX_ETAG
        && value.is_ascii()
        && !value.bytes().any(|b| b.is_ascii_control())
        && !inner.contains('"');
    ok.then(|| value.to_string())
}

/// What `output` came to, judged by the status line rather than the exit
/// code (`gh` exits non-zero on a 304). Rate limits are checked before a
/// 403 is taken to mean no access. `now` is only used to place `until`.
pub fn classify(output: io::Result<GhOutput>, now: i64) -> Answer {
    let output = match output {
        Ok(output) => output,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Answer::Failed(ForgeStatus::Cli(CliStatus::NotInstalled));
        }
        Err(_) => return Answer::Failed(ForgeStatus::Failed),
    };
    if output.timed_out {
        return Answer::Failed(ForgeStatus::Offline);
    }
    let (status, headers, body) = split_include(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    if output.exit == Some(4) || stderr.contains("http 401") || status == Some(401) {
        return Answer::Failed(ForgeStatus::NotLoggedIn);
    }
    if OFFLINE_MARKERS.iter().any(|m| stderr.contains(m)) {
        return Answer::Failed(ForgeStatus::Offline);
    }
    let header = |name: &str| {
        headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    };
    let number = |name: &str| header(name).and_then(|v| v.parse::<i64>().ok());
    let errors = serde_json::from_slice::<ErrorsOnly>(body)
        .ok()
        .and_then(|e| e.errors)
        .unwrap_or_default();
    let has_error = |kinds: &[&str]| {
        errors
            .iter()
            .any(|e| e.kind.as_deref().is_some_and(|k| kinds.contains(&k)))
    };
    let refused = matches!(status, Some(403 | 429));
    let until = if refused && header("x-ratelimit-remaining") == Some("0") {
        Some(number("x-ratelimit-reset").unwrap_or(now + MIN_BACKOFF))
    } else if let Some(wait) = number("retry-after").filter(|_| refused) {
        Some(now.saturating_add(wait))
    } else if refused && stderr.contains("rate limit") {
        Some(now + MIN_BACKOFF)
    } else if has_error(&["RATE_LIMITED"]) {
        Some(number("x-ratelimit-reset").unwrap_or(now + MIN_BACKOFF))
    } else {
        None
    };
    if let Some(until) = until {
        return Answer::Limited {
            until: until.clamp(now + MIN_BACKOFF, now + MAX_BACKOFF),
        };
    }
    if status == Some(304) {
        return Answer::NotModified;
    }
    if has_error(&["NOT_FOUND", "FORBIDDEN"]) || matches!(status, Some(403 | 404)) {
        return Answer::Failed(ForgeStatus::NoAccess);
    }
    let success = match status {
        Some(code) => (200..300).contains(&code),
        None => output.exit == Some(0),
    };
    if !success {
        return Answer::Failed(ForgeStatus::Failed);
    }
    Answer::Body {
        etag: header("etag").and_then(valid_etag),
        body: body.to_vec(),
    }
}

/// The badge for each of `batch`'s aliases in order, from a GraphQL body,
/// and whether the repository has a parent. An alias's badge is its first
/// pull request whose head owner is the head's, in the repository and
/// else in the parent.
fn read_answer(body: &[u8], batch: &[Head]) -> Result<(Vec<Option<PrBadge>>, bool), ForgeStatus> {
    let response: Response = serde_json::from_slice(body).map_err(|_| ForgeStatus::Failed)?;
    if response.errors.is_some_and(|e| !e.is_empty()) {
        return Err(ForgeStatus::Failed);
    }
    let Some(Data {
        repository: Some(repository),
    }) = response.data
    else {
        return Err(ForgeStatus::Failed);
    };
    let parent = repository.parent.as_ref();
    // By alias index, never by anything the answer names.
    let badges = batch
        .iter()
        .enumerate()
        .map(|(i, head)| {
            let alias = format!("b{i}");
            let pr = head_pr(&repository.aliases, &alias, head)
                .or_else(|| head_pr(parent?, &alias, head))?;
            badge_of(pr)
        })
        .collect();
    Ok((badges, parent.is_some()))
}

/// The first pull request under `alias` opened from `head`'s owner.
fn head_pr<'a>(
    aliases: &'a HashMap<String, Connection>,
    alias: &str,
    head: &Head,
) -> Option<&'a PrNode> {
    aliases.get(alias)?.nodes.iter().find(|pr| {
        pr.head_repository_owner
            .as_ref()
            .is_some_and(|owner| owner.login.eq_ignore_ascii_case(&head.owner))
    })
}

fn badge_of(pr: &PrNode) -> Option<PrBadge> {
    let state = match (pr.state.as_str(), pr.is_draft) {
        ("OPEN", true) => PrState::Draft,
        ("OPEN", false) => PrState::Open,
        ("MERGED", _) => PrState::Merged,
        ("CLOSED", _) => PrState::Closed,
        _ => return None,
    };
    let rollup = pr
        .commits
        .nodes
        .first()
        .and_then(|n| n.commit.status_check_rollup.as_ref());
    let ci = rollup.and_then(|r| match r.state.as_str() {
        "SUCCESS" => Some(CiState::Success),
        "FAILURE" | "ERROR" => Some(CiState::Failure),
        "PENDING" | "EXPECTED" => Some(CiState::Pending),
        _ => None,
    });
    Some(PrBadge {
        number: pr.number,
        state,
        ci,
    })
}

/// A scripted [`GhRunner`]: answers each call with the next reply, keeps
/// every command, and never starts a process.
#[cfg(any(test, feature = "test-support"))]
#[derive(Default)]
pub struct FakeGh {
    replies: std::sync::Mutex<std::collections::VecDeque<Option<GhOutput>>>,
    calls: std::sync::Mutex<Vec<GhCommand>>,
    /// What `gh` would inherit from Kika's environment, such as a token.
    inherited: Vec<(String, String)>,
}

#[cfg(any(test, feature = "test-support"))]
impl FakeGh {
    pub fn new() -> Self {
        Self::default()
    }

    /// Let `gh` see `key=value`, as it would from the user's environment.
    pub fn with_inherited_env(mut self, key: &str, value: &str) -> Self {
        self.inherited.push((key.into(), value.into()));
        self
    }

    /// Whether a call ran with `key=value` in its environment: what it
    /// inherited, less what is removed, plus what is set.
    pub fn saw_env(&self, key: &str, value: &str) -> bool {
        !self.calls().is_empty()
            && !GH_ENV_REMOVE.contains(&key)
            && GH_ENV
                .iter()
                .map(|(k, v)| (*k, *v))
                .chain(self.inherited.iter().map(|(k, v)| (k.as_str(), v.as_str())))
                .any(|pair| pair == (key, value))
    }

    pub fn reply(self, output: GhOutput) -> Self {
        self.replies.lock().unwrap().push_back(Some(output));
        self
    }

    /// The next call fails to start, as with no `gh` installed.
    pub fn missing(self) -> Self {
        self.replies.lock().unwrap().push_back(None);
        self
    }

    pub fn calls(&self) -> Vec<GhCommand> {
        self.calls.lock().unwrap().clone()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl GhRunner for FakeGh {
    fn run(&self, cmd: &GhCommand) -> io::Result<GhOutput> {
        self.calls.lock().unwrap().push(cmd.clone());
        let reply = self.replies.lock().unwrap().pop_front();
        match reply.expect("unexpected gh call") {
            Some(output) => Ok(output),
            None => Err(io::ErrorKind::NotFound.into()),
        }
    }
}

/// What `gh` prints for an `--include` answer `stdout`: it exits 0 on a
/// 2xx status and 1 on any other, a 304 included.
#[cfg(any(test, feature = "test-support"))]
pub fn answer(stdout: &str) -> GhOutput {
    let (status, _, _) = split_include(stdout.as_bytes());
    let ok = status.is_none_or(|code| (200..300).contains(&code));
    GhOutput {
        exit: Some(if ok { 0 } else { 1 }),
        stdout: stdout.as_bytes().to_vec(),
        ..GhOutput::default()
    }
}

/// A failed run with `exit` and `stderr`.
#[cfg(any(test, feature = "test-support"))]
pub fn failure(exit: i32, stderr: &str) -> GhOutput {
    GhOutput {
        exit: Some(exit),
        stderr: stderr.as_bytes().to_vec(),
        ..GhOutput::default()
    }
}

#[cfg(any(test, feature = "test-support"))]
pub mod fixtures {
    pub const STATES: &str = include_str!("fixtures/states_include.http");
    pub const NOT_FOUND: &str = include_str!("fixtures/not_found.json");
    pub const FORBIDDEN: &str = include_str!("fixtures/forbidden.json");
    pub const PROBE_200: &str = include_str!("fixtures/probe_200.http");
    pub const PROBE_304: &str = include_str!("fixtures/probe_304.http");
    pub const LIMIT_PRIMARY: &str = include_str!("fixtures/limit_primary.http");
    pub const LIMIT_SECONDARY: &str = include_str!("fixtures/limit_secondary.http");
    pub const LIMIT_SECONDARY_STDERR: &str = include_str!("fixtures/limit_secondary.stderr");
    pub const GRAPHQL_RATE_LIMITED: &str = include_str!("fixtures/graphql_rate_limited.http");
    pub const SERVER_ERROR: &str = include_str!("fixtures/server_error.http");
    pub const NOT_FOUND_STDERR: &str = include_str!("fixtures/not_found.stderr");
    pub const LOGGED_OUT_STDERR: &str = include_str!("fixtures/logged_out.stderr");
    pub const HTTP_401_STDERR: &str = include_str!("fixtures/http_401.stderr");
    pub const DNS_STDERR: &str = include_str!("fixtures/dns.stderr");
    pub const TIMEOUT_STDERR: &str = include_str!("fixtures/timeout.stderr");
    pub const OTHER_STDERR: &str = include_str!("fixtures/other.stderr");
    pub const CREATE_201: &str = include_str!("fixtures/create_201.http");
    pub const CREATE_422: &str = include_str!("fixtures/create_422.http");
    pub const CREATE_422_STDERR: &str = include_str!("fixtures/create_422.stderr");
    pub const FORK_HEADS: &str = include_str!("fixtures/fork_heads.http");
    pub const FORK_PARENT: &str = include_str!("fixtures/fork_parent.http");
    pub const RENAMED_HEAD: &str = include_str!("fixtures/renamed_head.http");

    /// The token a leaky `gh` holds and prints; it must never leave `gh`.
    pub const SECRET_TOKEN: &str = "ghp_SENTINEL";
    /// The ETag a leaky `gh`'s probe answers with; only the cache keeps it.
    pub const SECRET_ETAG: &str = "W/\"etag_VALUE_9\"";

    /// A `gh` that holds [`SECRET_TOKEN`] and prints it with every answer,
    /// once in a header too, for two refreshes: a 200 probe carrying
    /// [`SECRET_ETAG`] and the badges, then a 401 for both calls.
    pub fn leaky_gh() -> super::FakeGh {
        use super::{GhOutput, answer, failure};
        let noisy = |output: GhOutput| GhOutput {
            stderr: format!(
                "GH_TOKEN={SECRET_TOKEN}\n{}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into_bytes(),
            ..output
        };
        let probe = PROBE_200
            .replace("W/\"5f1e0c2b9a7d\"", SECRET_ETAG)
            .replace(
                "Cache-Control:",
                &format!("Authorization: token {SECRET_TOKEN}\r\nCache-Control:"),
            );
        super::FakeGh::new()
            .with_inherited_env("GH_TOKEN", SECRET_TOKEN)
            .reply(noisy(answer(&probe)))
            .reply(noisy(answer(STATES)))
            .reply(noisy(failure(1, HTTP_401_STDERR)))
            .reply(noisy(failure(1, HTTP_401_STDERR)))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::fixtures::*;
    use super::*;

    /// A fixed clock: 2023-11-14 22:13:20 UTC.
    const NOW: i64 = 1_700_000_000;

    fn host() -> ForgeHost {
        ForgeHost::parse("github.com").unwrap()
    }

    fn repo() -> ForgeRepo {
        ForgeRepo {
            host: "github.com".into(),
            owner: "acme".into(),
            name: "widgets".into(),
        }
    }

    fn names(n: usize) -> Vec<BranchName> {
        (0..n)
            .map(|i| format!("topic-{i}").as_str().into())
            .collect()
    }

    fn gh<'a>(runner: &'a FakeGh, sink: &'a CommandSink, host: &'a ForgeHost) -> Gh<'a> {
        Gh {
            runner,
            sink,
            host,
            cwd: "/cache".into(),
            now: NOW,
        }
    }

    /// Each of `branches` as its own name in the repository's owner.
    fn own(branches: &[BranchName]) -> Vec<Head> {
        super::super::head::heads(branches, &HashMap::new(), &repo())
    }

    fn fetch_heads(runner: &FakeGh, repo: &ForgeRepo, heads: &[Head]) -> Result<Fetched, Refusal> {
        gh(runner, &CommandSink::new(), &host()).fetch_badges(repo, heads)
    }

    fn fetch(
        runner: &FakeGh,
        branches: &[BranchName],
    ) -> Result<HashMap<BranchName, PrBadge>, Refusal> {
        fetch_heads(runner, &repo(), &own(branches)).map(|f| f.badges)
    }

    fn body(cmd: &GhCommand) -> serde_json::Value {
        match &cmd.kind {
            GhCall::Graphql { body } => serde_json::from_slice(body).unwrap(),
            other => panic!("{other:?}"),
        }
    }

    fn classify_ok(output: GhOutput) -> Answer {
        classify(Ok(output), NOW)
    }

    #[test]
    fn the_graphql_argv_is_fixed_and_names_only_the_host() {
        let runner = FakeGh::new().reply(answer(STATES));
        fetch(&runner, &names(1)).unwrap();
        let [cmd] = runner.calls().try_into().unwrap();
        assert_eq!(
            cmd.args(),
            [
                "api",
                "graphql",
                "--include",
                "--hostname",
                "github.com",
                "--input",
                "-"
            ]
            .map(OsString::from)
        );
        assert_eq!(
            cmd.display_line(),
            "gh api graphql --include --hostname github.com --input -"
        );
        assert_eq!(cmd.cwd, PathBuf::from("/cache"));
        assert_eq!(cmd.timeout, Duration::from_secs(20));
    }

    #[test]
    fn the_probe_argv_sends_the_etag_but_the_line_never_shows_it() {
        let runner = FakeGh::new()
            .reply(answer(PROBE_304))
            .reply(answer(PROBE_200));
        let (sink, host) = (CommandSink::new(), host());
        let gh = gh(&runner, &sink, &host);
        assert_eq!(gh.probe(&repo(), Some("\"abc\"")), Answer::NotModified);
        assert!(matches!(gh.probe(&repo(), None), Answer::Body { .. }));
        let [with, without] = runner.calls().try_into().unwrap();
        let path = "repos/acme/widgets/pulls?state=all&sort=updated&direction=desc&per_page=1";
        assert_eq!(
            with.args(),
            [
                "api",
                "--include",
                "--hostname",
                "github.com",
                "-H",
                "Accept: application/vnd.github+json",
                "-H",
                "If-None-Match: \"abc\"",
                path,
            ]
            .map(OsString::from)
        );
        assert_eq!(
            with.display_line(),
            format!(
                "gh api --include --hostname github.com -H \"Accept: application/vnd.github+json\" \
                 -H \"If-None-Match: <etag>\" {path}"
            )
        );
        assert!(!with.display_line().contains("abc"));
        assert_eq!(
            without.args(),
            [
                "api",
                "--include",
                "--hostname",
                "github.com",
                "-H",
                "Accept: application/vnd.github+json",
                path,
            ]
            .map(OsString::from)
        );
        assert!(!without.display_line().contains("If-None-Match"));
    }

    #[test]
    fn an_invalid_etag_is_neither_kept_nor_sent() {
        for good in ["\"abc\"", "W/\"5f1e\"", "\"\""] {
            assert_eq!(valid_etag(good).as_deref(), Some(good));
        }
        let long = format!("\"{}\"", "a".repeat(255));
        for bad in [
            "abc",
            "\"abc",
            "W/abc",
            "\"a\"b\"",
            "\"a\u{7}b\"",
            "\"a\nb\"",
            "\"café\"",
            long.as_str(),
        ] {
            assert_eq!(valid_etag(bad), None, "{bad:?}");
        }
        // Not sent on the probe ...
        let runner = FakeGh::new().reply(answer(PROBE_200));
        let (sink, host) = (CommandSink::new(), host());
        gh(&runner, &sink, &host).probe(&repo(), Some("\"a\nb\""));
        assert!(
            runner.calls()[0]
                .args()
                .iter()
                .all(|a| !a.to_string_lossy().contains("If-None-Match"))
        );
        // ... and not kept from an answer.
        let answer = classify_ok(answer("HTTP/2.0 200 OK\r\nEtag: not-quoted\r\n\r\n[]"));
        assert_eq!(
            answer,
            Answer::Body {
                etag: None,
                body: b"[]".to_vec()
            }
        );
    }

    #[test]
    fn include_output_splits_into_status_read_headers_and_body() {
        let (status, headers, body) = split_include(PROBE_200.as_bytes());
        assert_eq!(status, Some(200));
        assert_eq!(
            headers,
            [
                ("etag".to_string(), "W/\"5f1e0c2b9a7d\"".to_string()),
                ("x-ratelimit-remaining".into(), "4999".into()),
                ("x-ratelimit-reset".into(), "1700003600".into()),
            ]
        );
        assert!(body.starts_with(b"[{\"number\":7"));
        // Plain line ends work too, and output with no status line is
        // all body.
        let (status, headers, body) = split_include(b"HTTP/1.1 304 Not Modified\nETag: \"x\"\n\n");
        assert_eq!(
            (status, headers, body),
            (Some(304), vec![("etag".into(), "\"x\"".into())], &b""[..])
        );
        let (status, headers, body) = split_include(b"{\"a\":\n\n1}");
        assert_eq!(
            (status, headers.len(), body),
            (None, 0, &b"{\"a\":\n\n1}"[..])
        );
    }

    #[test]
    fn rate_limits_are_classified_with_until_clamped() {
        assert_eq!(
            classify_ok(answer(LIMIT_PRIMARY)),
            Answer::Limited {
                until: 1_700_001_800
            }
        );
        let secondary = GhOutput {
            stderr: LIMIT_SECONDARY_STDERR.into(),
            ..answer(LIMIT_SECONDARY)
        };
        assert_eq!(classify_ok(secondary), Answer::Limited { until: NOW + 120 });
        let bare = GhOutput {
            stderr: b"gh: API rate limit exceeded (HTTP 403)".to_vec(),
            ..answer("HTTP/2.0 403 Forbidden\r\n\r\n")
        };
        assert_eq!(classify_ok(bare), Answer::Limited { until: NOW + 60 });
        assert_eq!(
            classify_ok(answer(GRAPHQL_RATE_LIMITED)),
            Answer::Limited {
                until: 1_700_003_600
            }
        );
        let no_reset = GRAPHQL_RATE_LIMITED.replace("X-Ratelimit-Reset: 1700003600\r\n", "");
        assert_eq!(
            classify_ok(answer(&no_reset)),
            Answer::Limited { until: NOW + 60 }
        );
        // Far in the future, in the past, or a huge wait: clamped.
        let far = LIMIT_PRIMARY.replace("1700001800", "1800000000");
        assert_eq!(
            classify_ok(answer(&far)),
            Answer::Limited { until: NOW + 3600 }
        );
        let past = LIMIT_PRIMARY.replace("1700001800", "1");
        assert_eq!(
            classify_ok(answer(&past)),
            Answer::Limited { until: NOW + 60 }
        );
        let huge = LIMIT_SECONDARY.replace("Retry-After: 120", "Retry-After: 9223372036854775807");
        assert_eq!(
            classify_ok(answer(&huge)),
            Answer::Limited { until: NOW + 3600 }
        );
        // A 429 counts as much as a 403.
        let too_many = LIMIT_SECONDARY.replace("403 Forbidden", "429 Too Many Requests");
        assert_eq!(
            classify_ok(answer(&too_many)),
            Answer::Limited { until: NOW + 120 }
        );
    }

    #[test]
    fn a_plain_403_is_still_no_access() {
        let forbidden = FORBIDDEN.replace("HTTP/2.0 200 OK", "HTTP/2.0 403 Forbidden");
        assert!(forbidden.contains("X-Ratelimit-Remaining: 4987"));
        assert_eq!(
            classify_ok(answer(&forbidden)),
            Answer::Failed(ForgeStatus::NoAccess)
        );
        let plain = LIMIT_PRIMARY.replace("X-Ratelimit-Remaining: 0", "X-Ratelimit-Remaining: 9");
        assert_eq!(
            classify_ok(answer(&plain)),
            Answer::Failed(ForgeStatus::NoAccess)
        );
    }

    #[test]
    fn statuses_are_read_from_the_status_line_not_the_exit_code() {
        assert_eq!(classify_ok(answer(PROBE_304)), Answer::NotModified);
        assert_eq!(answer(PROBE_304).exit, Some(1));
        assert_eq!(
            classify_ok(answer(PROBE_200)),
            Answer::Body {
                etag: Some("W/\"5f1e0c2b9a7d\"".into()),
                body: split_include(PROBE_200.as_bytes()).2.to_vec(),
            }
        );
        assert_eq!(
            classify_ok(answer(SERVER_ERROR)),
            Answer::Failed(ForgeStatus::Failed)
        );
        let unauthorized = SERVER_ERROR.replace("502 Bad Gateway", "401 Unauthorized");
        assert_eq!(
            classify_ok(answer(&unauthorized)),
            Answer::Failed(ForgeStatus::NotLoggedIn)
        );
    }

    #[test]
    fn names_travel_as_variables_and_never_in_the_query() {
        let runner = FakeGh::new().reply(answer(STATES));
        let heads = [
            Head {
                branch: "local-only".into(),
                owner: "acme".into(),
                name: "feature/login".into(),
            },
            Head {
                branch: "fix".into(),
                owner: "forker-owner".into(),
                name: "fix\"}{repository".into(),
            },
        ];
        fetch_heads(&runner, &repo(), &heads).unwrap();
        let body = body(&runner.calls()[0]);
        let query = body["query"].as_str().unwrap();
        for word in [
            "feature/login",
            "fix\"",
            "local-only",
            "forker-owner",
            "acme",
            "widgets",
        ] {
            assert!(!query.contains(word), "{word} in {query}");
        }
        // The ref names travel; the head owners and local names do not.
        let vars = &body["variables"];
        assert_eq!(vars["owner"], "acme");
        assert_eq!(vars["name"], "widgets");
        assert_eq!(vars["b0"], "feature/login");
        assert_eq!(vars["b1"], "fix\"}{repository");
        assert_eq!(vars.as_object().unwrap().len(), 4);
        let sent = body.to_string();
        assert!(!sent.contains("forker-owner") && !sent.contains("local-only"));
        let alias =
            "b1:pullRequests(headRefName:$b1,first:5,orderBy:{field:CREATED_AT,direction:DESC})";
        // Each alias is asked of the repository and of its parent.
        assert_eq!(query.matches(alias).count(), 2, "{query}");
        assert!(query.find("parent{").unwrap() > query.find(alias).unwrap());
        assert!(query.contains("headRepositoryOwner{login}"));
        assert!(query.contains("statusCheckRollup{state}"));
        // The text depends on the count alone.
        assert_eq!(query, super::query(2));
    }

    #[test]
    fn a_fork_pr_is_matched_by_its_head_owner_not_by_order() {
        // `origin` is o/r; local `fix` tracks `fork/fix`, and `fork` is me/r.
        let base = ForgeRepo {
            host: "github.com".into(),
            owner: "o".into(),
            name: "r".into(),
        };
        let upstreams = HashMap::from([(
            BranchName::from("fix"),
            kika_git::forge_remote::Upstream {
                remote: b"fork".to_vec(),
                merge: b"refs/heads/fix".to_vec(),
                url: Some(b"git@github.com:me/r.git".to_vec()),
            },
        )]);
        let heads = super::super::head::heads(&["fix".into()], &upstreams, &base);
        let runner = FakeGh::new().reply(answer(FORK_HEADS));
        let fetched = fetch_heads(&runner, &base, &heads).unwrap();
        // The newer PR from `other` is passed over for the one from `me`.
        assert_eq!(
            fetched.badges,
            HashMap::from([(
                BranchName::from("fix"),
                PrBadge {
                    number: 9,
                    state: PrState::Open,
                    ci: Some(CiState::Success),
                }
            )])
        );
        assert!(!fetched.forked);
        let body = body(&runner.calls()[0]);
        assert_eq!(body["variables"]["owner"], "o");
        assert_eq!(body["variables"]["b0"], "fix");
    }

    #[test]
    fn the_parent_answers_when_the_repository_has_no_match() {
        let head = |owner: &str| Head {
            branch: "fix".into(),
            owner: owner.into(),
            name: "fix".into(),
        };
        let badge = |owner: &str| {
            let runner = FakeGh::new().reply(answer(FORK_PARENT));
            let fetched = fetch_heads(&runner, &repo(), &[head(owner)]).unwrap();
            assert!(fetched.forked);
            fetched
                .badges
                .get(&BranchName::from("fix"))
                .map(|b| b.number)
        };
        // Nothing of `me` in the fork; the parent's PR is the one.
        assert_eq!(badge("me"), Some(31));
        // The repository's own match wins over the parent's.
        assert_eq!(badge("someone"), Some(3));
        // A deleted fork's PR (no owner) matches nobody.
        assert_eq!(badge("ghost"), None);
    }

    #[test]
    fn a_missing_or_null_owner_or_parent_matches_nothing() {
        let nodes = |owner: &str| {
            format!(
                "{{\"data\":{{\"repository\":{{\"b0\":{{\"nodes\":[{{\"number\":1,\
                 \"state\":\"OPEN\",\"isDraft\":false,{owner}\"commits\":{{\"nodes\":[]}}}}]}},\
                 \"parent\":null}}}}}}"
            )
        };
        for owner in ["", "\"headRepositoryOwner\":null,"] {
            let runner = FakeGh::new().reply(answer(&nodes(owner)));
            let fetched = fetch_heads(&runner, &repo(), &own(&names(1))).unwrap();
            assert_eq!(fetched, Fetched::default(), "{owner}");
        }
        let runner = FakeGh::new().reply(answer(&nodes(
            "\"headRepositoryOwner\":{\"login\":\"ACME\"},",
        )));
        let fetched = fetch_heads(&runner, &repo(), &own(&names(1))).unwrap();
        assert_eq!(fetched.badges[&names(1)[0]].number, 1);
    }

    #[test]
    fn the_env_disables_prompts_and_holds_no_token() {
        let set: Vec<_> = GH_ENV.iter().map(|(k, _)| *k).collect();
        assert_eq!(
            set,
            [
                "GH_PROMPT_DISABLED",
                "GH_NO_UPDATE_NOTIFIER",
                "GH_SPINNER_DISABLED",
                "NO_COLOR",
                "GH_PAGER"
            ]
        );
        assert!(GH_ENV_REMOVE.contains(&"GH_HOST"));
        assert!(GH_ENV_REMOVE.contains(&"GH_REPO"));
        for (key, value) in GH_ENV {
            assert!(!key.contains("TOKEN") && !value.contains("ghp_"), "{key}");
        }
    }

    #[test]
    fn each_pr_state_and_ci_state_parses_to_its_badge() {
        let runner = FakeGh::new().reply(answer(STATES));
        let branches = names(7);
        let badges = fetch(&runner, &branches).unwrap();
        let badge = |i: usize| badges.get(&branches[i]).copied();
        let pr = |number, state, ci| Some(PrBadge { number, state, ci });
        assert_eq!(badge(0), pr(101, PrState::Open, Some(CiState::Success)));
        assert_eq!(badge(1), pr(102, PrState::Draft, Some(CiState::Pending)));
        assert_eq!(badge(2), pr(103, PrState::Merged, None));
        assert_eq!(badge(3), pr(104, PrState::Closed, Some(CiState::Failure)));
        assert_eq!(badge(4), None);
        assert_eq!(badge(5), pr(105, PrState::Open, Some(CiState::Failure)));
        assert_eq!(badge(6), pr(106, PrState::Open, Some(CiState::Pending)));
        assert_eq!(badges.len(), 6);
    }

    #[test]
    fn many_branches_go_in_batches_of_fifty_up_to_five_hundred() {
        let runner = FakeGh::new()
            .reply(answer(STATES))
            .reply(answer(STATES))
            .reply(answer(STATES));
        let branches = names(120);
        let badges = fetch(&runner, &branches).unwrap();
        let calls = runner.calls();
        // One call per batch, the parent asked in the same call.
        assert_eq!(calls.len(), 3);
        for call in &calls {
            assert!(body(call)["query"].as_str().unwrap().contains("parent{"));
        }
        assert_eq!(body(&calls[1])["variables"]["b0"], "topic-50");
        assert!(body(&calls[2])["variables"].get("b20").is_none());
        // Each batch maps back by its own aliases.
        assert_eq!(badges[&branches[50]].number, 101);
        assert_eq!(badges[&branches[100]].number, 101);

        let mut runner = FakeGh::new();
        for _ in 0..10 {
            runner = runner.reply(answer("{\"data\":{\"repository\":{}}}"));
        }
        fetch(&runner, &names(600)).unwrap();
        assert_eq!(runner.calls().len(), 10);
    }

    #[test]
    fn a_rate_limited_batch_stops_the_fetch() {
        let runner = FakeGh::new()
            .reply(answer(STATES))
            .reply(answer(GRAPHQL_RATE_LIMITED));
        assert_eq!(
            fetch(&runner, &names(120)),
            Err(Refusal::Limited {
                until: 1_700_003_600
            })
        );
        assert_eq!(runner.calls().len(), 2);
    }

    #[test]
    fn each_failure_maps_to_its_status() {
        let cases = [
            (
                FakeGh::new().reply(failure(4, LOGGED_OUT_STDERR)),
                ForgeStatus::NotLoggedIn,
            ),
            (
                FakeGh::new().reply(failure(1, HTTP_401_STDERR)),
                ForgeStatus::NotLoggedIn,
            ),
            (
                FakeGh::new().reply(failure(1, DNS_STDERR)),
                ForgeStatus::Offline,
            ),
            (
                FakeGh::new().reply(failure(1, TIMEOUT_STDERR)),
                ForgeStatus::Offline,
            ),
            (
                FakeGh::new().reply(GhOutput {
                    timed_out: true,
                    ..GhOutput::default()
                }),
                ForgeStatus::Offline,
            ),
            (
                FakeGh::new().reply(GhOutput {
                    stdout: NOT_FOUND.into(),
                    ..failure(1, NOT_FOUND_STDERR)
                }),
                ForgeStatus::NoAccess,
            ),
            (
                FakeGh::new().reply(GhOutput {
                    stdout: FORBIDDEN.into(),
                    ..failure(1, "")
                }),
                ForgeStatus::NoAccess,
            ),
            (
                FakeGh::new().missing(),
                ForgeStatus::Cli(CliStatus::NotInstalled),
            ),
            (
                FakeGh::new().reply(failure(1, OTHER_STDERR)),
                ForgeStatus::Failed,
            ),
            (
                FakeGh::new().reply(answer(SERVER_ERROR)),
                ForgeStatus::Failed,
            ),
            (FakeGh::new().reply(answer("not json")), ForgeStatus::Failed),
            (
                FakeGh::new().reply(answer("{\"data\":null}")),
                ForgeStatus::Failed,
            ),
        ];
        for (runner, want) in cases {
            assert_eq!(fetch(&runner, &names(2)), Err(Refusal::Status(want)));
        }
        // The first failure stops the batches.
        let runner = FakeGh::new().reply(failure(1, DNS_STDERR));
        assert_eq!(
            fetch(&runner, &names(120)),
            Err(Refusal::Status(ForgeStatus::Offline))
        );
        assert_eq!(runner.calls().len(), 1);
    }

    fn create(runner: &FakeGh, head: &str) -> Result<(u64, String), CreateError> {
        gh(runner, &CommandSink::new(), &host()).create_pull(&repo(), head, "main", "Add login")
    }

    #[test]
    fn a_created_pull_request_gives_its_number_and_page() {
        let runner = FakeGh::new().reply(answer(CREATE_201));
        assert_eq!(
            create(&runner, "topic"),
            Ok((42, "https://github.com/acme/widgets/pull/42".into()))
        );
        let [cmd] = runner.calls().try_into().unwrap();
        assert_eq!(
            cmd.args(),
            [
                "api",
                "--include",
                "--hostname",
                "github.com",
                "--method",
                "POST",
                "repos/acme/widgets/pulls",
                "--input",
                "-",
            ]
            .map(OsString::from)
        );
        assert_eq!(
            cmd.display_line(),
            "gh api --include --hostname github.com --method POST repos/acme/widgets/pulls --input -"
        );
        assert_eq!(cmd.cwd, PathBuf::from("/cache"));
        let sent: serde_json::Value = serde_json::from_slice(&cmd.stdin()).unwrap();
        assert_eq!(
            sent,
            json!({ "title": "Add login", "head": "topic", "base": "main" })
        );
    }

    #[test]
    fn names_and_the_title_travel_on_stdin_and_never_in_argv() {
        let runner = FakeGh::new().reply(answer(CREATE_201));
        let (sink, host) = (CommandSink::new(), host());
        let title = "x\"; --method DELETE $(rm -rf ~)";
        gh(&runner, &sink, &host)
            .create_pull(&repo(), "me:feat/--force", "release/1.x", title)
            .unwrap();
        let cmd = &runner.calls()[0];
        let argv: Vec<String> = cmd
            .args()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        for word in ["feat", "release", "DELETE", "rm -rf", "me:"] {
            assert!(argv.iter().all(|a| !a.contains(word)), "{word}: {argv:?}");
            assert!(!cmd.display_line().contains(word), "{word}");
        }
        let sent: serde_json::Value = serde_json::from_slice(&cmd.stdin()).unwrap();
        assert_eq!(sent["head"], "me:feat/--force");
        assert_eq!(sent["base"], "release/1.x");
        assert_eq!(sent["title"], title);
    }

    #[test]
    fn a_refusal_keeps_githubs_message_terminal_safe() {
        let refused = GhOutput {
            stderr: CREATE_422_STDERR.into(),
            ..answer(CREATE_422)
        };
        assert_eq!(refused.exit, Some(1));
        let runner = FakeGh::new().reply(refused);
        assert_eq!(
            create(&runner, "topic"),
            Err(CreateError::Refused(
                "Validation Failed: A pull request already exists for acme:topic.".into()
            ))
        );
        // A detail with no message says its field and code; escapes go.
        let odd = "HTTP/2.0 422 Unprocessable Entity\r\n\r\n\
            {\"message\":\"Bad\\u001b[31m\",\"errors\":[{\"field\":\"base\",\"code\":\"invalid\"}]}";
        let runner = FakeGh::new().reply(answer(odd));
        let Err(CreateError::Refused(text)) = create(&runner, "topic") else {
            panic!()
        };
        assert!(!text.chars().any(char::is_control), "{text:?}");
        assert!(
            text.starts_with("Bad") && text.ends_with(": base invalid"),
            "{text}"
        );
        // No readable reason: a plain failure.
        let runner = FakeGh::new().reply(answer("HTTP/2.0 500 Oops\r\n\r\nnot json"));
        assert_eq!(
            create(&runner, "topic"),
            Err(CreateError::Status(ForgeStatus::Failed))
        );
    }

    #[test]
    fn a_rate_limited_or_failed_create_says_why() {
        let runner = FakeGh::new().reply(answer(LIMIT_PRIMARY));
        assert_eq!(
            create(&runner, "topic"),
            Err(CreateError::RateLimited {
                until: 1_700_001_800
            })
        );
        let cases = [
            (
                FakeGh::new().missing(),
                ForgeStatus::Cli(CliStatus::NotInstalled),
            ),
            (
                FakeGh::new().reply(failure(1, HTTP_401_STDERR)),
                ForgeStatus::NotLoggedIn,
            ),
            (
                FakeGh::new().reply(failure(1, DNS_STDERR)),
                ForgeStatus::Offline,
            ),
            (
                FakeGh::new().reply(GhOutput {
                    stdout: NOT_FOUND.into(),
                    ..failure(1, NOT_FOUND_STDERR)
                }),
                ForgeStatus::NoAccess,
            ),
            // Only a 201 is a pull request made.
            (FakeGh::new().reply(answer(PROBE_200)), ForgeStatus::Failed),
            (
                FakeGh::new().reply(answer("HTTP/2.0 201 Created\r\n\r\n{}")),
                ForgeStatus::Failed,
            ),
        ];
        for (runner, want) in cases {
            assert_eq!(create(&runner, "topic"), Err(CreateError::Status(want)));
        }
    }

    #[test]
    fn a_page_off_the_host_is_replaced_by_the_one_built_here() {
        for page in [
            "https://evil.io/acme/widgets/pull/42",
            "https://github.com.evil.io/x",
            "http://github.com/acme/widgets/pull/42",
            "javascript:alert(1)",
            "https://github.com/a b",
            "https://github.com/a\\u001b]52;c;eA==\\u0007",
        ] {
            let body = CREATE_201.replace("https://github.com/acme/widgets/pull/42", page);
            let runner = FakeGh::new().reply(answer(&body));
            assert_eq!(
                create(&runner, "topic"),
                Ok((42, "https://github.com/acme/widgets/pull/42".into())),
                "{page}"
            );
        }
        let none = CREATE_201.replace(
            "\"html_url\":\"https://github.com/acme/widgets/pull/42\",",
            "",
        );
        let runner = FakeGh::new().reply(answer(&none));
        assert_eq!(
            create(&runner, "topic"),
            Ok((42, "https://github.com/acme/widgets/pull/42".into()))
        );
    }

    #[test]
    fn each_call_is_logged_with_the_fixed_line_and_its_own_id() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = CommandSink::new();
        let keep = Arc::clone(&events);
        sink.connect(move |ev| keep.lock().unwrap().push(ev));
        let taken = sink.next_id();
        let runner = FakeGh::new().reply(answer(STATES)).missing();
        let host = host();
        let result = gh(&runner, &sink, &host).fetch_badges(&repo(), &own(&names(60)));
        assert_eq!(
            result,
            Err(Refusal::Status(ForgeStatus::Cli(CliStatus::NotInstalled)))
        );
        let events = events.lock().unwrap();
        let line = "gh api graphql --include --hostname github.com --input -";
        match events.as_slice() {
            [
                CommandEvent::Started {
                    id: a,
                    line: l1,
                    attached: false,
                },
                CommandEvent::Finished {
                    id: b,
                    outcome: CommandOutcome::Ok,
                    ..
                },
                CommandEvent::Started {
                    id: c,
                    line: l2,
                    attached: false,
                },
                CommandEvent::Finished {
                    id: d,
                    outcome: CommandOutcome::NotStarted,
                    ..
                },
            ] => {
                assert_eq!((l1.as_str(), l2.as_str()), (line, line));
                assert_eq!((a, c), (b, d));
                assert!(a != c && *a != taken && *c != taken);
            }
            other => panic!("{other:?}"),
        }
    }
}
