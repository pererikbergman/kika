//! Merge requests from GitLab, read through `glab api graphql`. `glab`
//! holds the login (its own config, or `GITLAB_TOKEN` in the environment
//! it inherits), so Kika never sees a token: it passes none, stores none
//! and draws none. `glab`'s stderr is only matched against fixed markers,
//! never kept, and of the headers only the rate-limit numbers are read.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use super::github::{
    Answer, BATCH, Fetched, GhOutput, MAX_BACKOFF, MAX_BRANCHES, MIN_BACKOFF, OFFLINE_MARKERS,
    Refusal, TIMEOUT, logged, run_piped, split_include_reading,
};
use super::head::Head;
use super::{CiState, ForgeHost, ForgeRepo, ForgeStatus, PrBadge, PrState};
use kika_git::recording::{CommandOutcome, CommandSink};

/// Set on every call: no prompt, no pager, no colour, no update check,
/// and no usage data sent, which `glab` does by default.
pub const GLAB_ENV: [(&str, &str); 5] = [
    ("NO_COLOR", "1"),
    ("NO_PROMPT", "1"),
    ("GLAB_CHECK_UPDATE", "false"),
    ("GLAB_PAGER", "cat"),
    ("GLAB_SEND_TELEMETRY", "0"),
];

/// Removed from every call, so only the arguments pick the host and no
/// project is implied. Kika's hidden-mode keys never reach a child.
pub const GLAB_ENV_REMOVE: [&str; 8] = [
    "GITLAB_HOST",
    "GL_HOST",
    "GITLAB_URI",
    "GITLAB_API_HOST",
    "GITLAB_URL",
    "GITLAB_REPO",
    kika_git::askpass::SOCKET_ENV,
    kika_git::rebase_plan::PLAN_ENV,
];

/// What one `glab api` call asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlabCall {
    /// A GraphQL query; `body` is the JSON for stdin, so no branch name
    /// or project path ever reaches argv.
    Graphql { body: Vec<u8> },
}

/// One `glab api` call: the host, and what it asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlabCommand {
    pub host: String,
    pub kind: GlabCall,
    /// Where `glab` runs: Kika's cache directory, never the repository,
    /// so `glab` reads no repository's git config.
    pub cwd: PathBuf,
    pub timeout: Duration,
}

impl GlabCommand {
    /// The arguments after `glab`. `--include` puts the status line and
    /// headers on stdout, so a refusal is read from its status, not from
    /// the exit code.
    pub fn args(&self) -> Vec<OsString> {
        match &self.kind {
            GlabCall::Graphql { .. } => [
                "api",
                "graphql",
                "--include",
                "--hostname",
                self.host.as_str(),
                "--input",
                "-",
            ]
            .map(OsString::from)
            .into(),
        }
    }

    /// The Command log's line: the whole argv, which holds no secret.
    pub fn display_line(&self) -> String {
        match &self.kind {
            GlabCall::Graphql { .. } => format!(
                "glab api graphql --include --hostname {} --input -",
                self.host
            ),
        }
    }

    /// What goes on stdin: the JSON body.
    pub fn stdin(&self) -> Vec<u8> {
        match &self.kind {
            GlabCall::Graphql { body } => body.clone(),
        }
    }
}

/// Runs `glab`; tests swap in [`FakeGlab`]. A run leaves what `gh` would:
/// an exit code, both streams and whether it timed out.
pub trait GlabRunner: Send + Sync {
    fn run(&self, cmd: &GlabCommand) -> io::Result<GhOutput>;
}

/// The real `glab`, found as `program`: a bare name looked up on an
/// absolute `PATH` entry, or an absolute path.
pub struct CliGlab {
    program: OsString,
}

impl CliGlab {
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
        }
    }
}

impl GlabRunner for CliGlab {
    fn run(&self, cmd: &GlabCommand) -> io::Result<GhOutput> {
        let mut command = kika_git::program::command(&self.program)?;
        command.args(cmd.args());
        for (key, value) in GLAB_ENV {
            command.env(key, value);
        }
        for key in GLAB_ENV_REMOVE {
            command.env_remove(key);
        }
        run_piped(command, &cmd.cwd, cmd.stdin(), cmd.timeout)
    }
}

/// The `glab` calls of one load: who runs them, where they are logged,
/// the host they go to and the time rate limits are measured from. Only
/// the GitLab forge adapter that
/// [`ForgeContext::forge`](super::task::ForgeContext::forge) hands out
/// holds one, behind the host's allow gate, so its fields stay inside
/// forge.
pub struct Glab<'a> {
    pub(crate) runner: &'a dyn GlabRunner,
    pub(crate) sink: &'a CommandSink,
    pub(crate) host: &'a ForgeHost,
    pub(crate) cwd: PathBuf,
    pub(crate) now: i64,
}

impl Glab<'_> {
    /// The merge request of each of `heads` in `repo`, asked in batches of
    /// [`BATCH`], one call after another, for at most [`MAX_BRANCHES`]
    /// heads. The first failure or rate limit ends it. GitLab has no fork
    /// lookup here, so `forked` is always false.
    pub fn fetch_badges(&self, repo: &ForgeRepo, heads: &[Head]) -> Result<Fetched, Refusal> {
        let mut fetched = Fetched::default();
        let heads = &heads[..heads.len().min(MAX_BRANCHES)];
        for batch in heads.chunks(BATCH) {
            let kind = GlabCall::Graphql {
                body: request_body(repo, batch),
            };
            let badges = self.call(kind, |answer| match answer {
                Answer::Body { body, .. } => read_answer(&body, batch).map_err(Refusal::Status),
                Answer::Limited { until } => Err(Refusal::Limited { until }),
                Answer::NotModified => Err(Refusal::Status(ForgeStatus::Failed)),
                Answer::Failed(status) => Err(Refusal::Status(status)),
            })?;
            for (head, badge) in batch.iter().zip(badges) {
                if let Some(badge) = badge {
                    fetched.badges.insert(head.branch.clone(), badge);
                }
            }
        }
        Ok(fetched)
    }

    /// Run one call, shown in the Command log under its own id, and read
    /// its answer with `read`.
    fn call<T>(
        &self,
        kind: GlabCall,
        read: impl FnOnce(Answer) -> Result<T, Refusal>,
    ) -> Result<T, Refusal> {
        let cmd = GlabCommand {
            host: self.host.as_str().to_string(),
            kind,
            cwd: self.cwd.clone(),
            timeout: TIMEOUT,
        };
        let now = self.now;
        logged(
            self.sink,
            cmd.display_line(),
            || read(classify(self.runner.run(&cmd), now)),
            |result| match result {
                Ok(_) => CommandOutcome::Ok,
                Err(Refusal::Status(ForgeStatus::NoGh)) => CommandOutcome::NotStarted,
                Err(_) => CommandOutcome::Failed(None),
            },
        )
    }
}

/// Merge requests asked per source branch: another namespace's fork may
/// share the name, so the newest few are read and filtered here.
const NODES: usize = 5;

/// The GraphQL query for `count` heads. Fixed text: aliases are `b0…`,
/// and every value is a variable, so nothing from the repository is ever
/// spliced into it.
fn query(count: usize) -> String {
    let mut params = String::from("$path:ID!");
    let mut fields = String::new();
    for i in 0..count {
        params.push_str(&format!(",$b{i}:String!"));
        fields.push_str(&format!(
            " b{i}:mergeRequests(sourceBranches:[$b{i}],sort:CREATED_DESC,first:{NODES})\
             {{nodes{{iid state draft sourceProject{{fullPath}} headPipeline{{status}}}}}}"
        ));
    }
    format!("query({params}){{project(fullPath:$path){{{fields} }}}}")
}

/// `{query, variables}` for stdin. Only the project path and the source
/// branch names are sent; owners are matched on the answer.
fn request_body(repo: &ForgeRepo, batch: &[Head]) -> Vec<u8> {
    let mut variables = serde_json::Map::new();
    variables.insert(
        "path".into(),
        json!(format!("{}/{}", repo.owner, repo.name)),
    );
    for (i, head) in batch.iter().enumerate() {
        variables.insert(format!("b{i}"), json!(head.name));
    }
    serde_json::to_vec(&json!({ "query": query(batch.len()), "variables": variables }))
        .unwrap_or_default()
}

#[derive(Deserialize)]
struct Response {
    data: Option<Data>,
    errors: Option<Vec<serde::de::IgnoredAny>>,
}

#[derive(Deserialize)]
struct Data {
    /// Null when the project does not exist or the login cannot see it.
    project: Option<HashMap<String, Connection>>,
}

#[derive(Deserialize)]
struct Connection {
    nodes: Vec<MrNode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MrNode {
    /// A decimal string in GitLab's schema.
    iid: String,
    state: String,
    draft: bool,
    /// Null for a deleted source project, which then matches no head.
    #[serde(default)]
    source_project: Option<SourceProject>,
    #[serde(default)]
    head_pipeline: Option<Pipeline>,
}

/// Compared with a head's owner and dropped: never kept or shown.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourceProject {
    full_path: String,
}

#[derive(Deserialize)]
struct Pipeline {
    status: String,
}

/// The only headers read; the rest are dropped unseen.
const READ_HEADERS: [&str; 3] = ["ratelimit-remaining", "ratelimit-reset", "retry-after"];

/// What `output` came to, judged by the status line rather than the exit
/// code. Rate limits are checked before a 403 is taken to mean no access,
/// and a 2xx body must be a GraphQL answer naming the project. `now` is
/// only used to place `until`.
pub fn classify(output: io::Result<GhOutput>, now: i64) -> Answer {
    let output = match output {
        Ok(output) => output,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Answer::Failed(ForgeStatus::NoGh),
        Err(_) => return Answer::Failed(ForgeStatus::Failed),
    };
    if output.timed_out {
        return Answer::Failed(ForgeStatus::Offline);
    }
    let (status, headers, body) = split_include_reading(&output.stdout, &READ_HEADERS);
    if status == Some(401) {
        return Answer::Failed(ForgeStatus::NotLoggedIn);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
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
    let spent = status == Some(403) && header("ratelimit-remaining") == Some("0");
    if status == Some(429) || spent {
        let until = number("ratelimit-reset")
            .or_else(|| number("retry-after").map(|wait| now.saturating_add(wait)))
            .unwrap_or(now + MIN_BACKOFF);
        return Answer::Limited {
            until: until.clamp(now + MIN_BACKOFF, now + MAX_BACKOFF),
        };
    }
    if matches!(status, Some(403 | 404)) {
        return Answer::Failed(ForgeStatus::NoAccess);
    }
    let success = match status {
        Some(code) => (200..300).contains(&code),
        None => output.exit == Some(0),
    };
    if !success {
        return Answer::Failed(ForgeStatus::Failed);
    }
    let Ok(response) = serde_json::from_slice::<Response>(body) else {
        return Answer::Failed(ForgeStatus::Failed);
    };
    if response.errors.is_some_and(|e| !e.is_empty()) {
        return Answer::Failed(ForgeStatus::Failed);
    }
    match response.data {
        None => Answer::Failed(ForgeStatus::Failed),
        Some(Data { project: None }) => Answer::Failed(ForgeStatus::NoAccess),
        Some(Data { project: Some(_) }) => Answer::Body {
            etag: None,
            body: body.to_vec(),
        },
    }
}

/// The badge for each of `batch`'s aliases in order, from a GraphQL body
/// [`classify`] accepted.
fn read_answer(body: &[u8], batch: &[Head]) -> Result<Vec<Option<PrBadge>>, ForgeStatus> {
    let response: Response = serde_json::from_slice(body).map_err(|_| ForgeStatus::Failed)?;
    let Some(Data {
        project: Some(aliases),
    }) = response.data
    else {
        return Err(ForgeStatus::Failed);
    };
    // By alias index, never by anything the answer names.
    Ok(batch
        .iter()
        .enumerate()
        .map(|(i, head)| badge_of(head_mr(&aliases, &format!("b{i}"), head)?))
        .collect())
}

/// The first merge request under `alias` whose source project is in
/// `head`'s namespace.
fn head_mr<'a>(
    aliases: &'a HashMap<String, Connection>,
    alias: &str,
    head: &Head,
) -> Option<&'a MrNode> {
    aliases.get(alias)?.nodes.iter().find(|mr| {
        mr.source_project.as_ref().is_some_and(|project| {
            project
                .full_path
                .split_once('/')
                .is_some_and(|(owner, _)| owner.eq_ignore_ascii_case(&head.owner))
        })
    })
}

/// `mr` as a badge, or `None` when its number or state is not one Kika
/// knows. A `MANUAL` pipeline gets no mark: it waits for a person and
/// never finishes on its own.
fn badge_of(mr: &MrNode) -> Option<PrBadge> {
    // Digits only: `parse` alone would take a leading `+`.
    if !mr.iid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let number = mr.iid.parse::<u64>().ok()?;
    let state = match (mr.state.as_str(), mr.draft) {
        ("opened", true) => PrState::Draft,
        ("opened" | "locked", _) => PrState::Open,
        ("merged", _) => PrState::Merged,
        ("closed", _) => PrState::Closed,
        _ => return None,
    };
    let ci = mr
        .head_pipeline
        .as_ref()
        .and_then(|p| match p.status.as_str() {
            "SUCCESS" => Some(CiState::Success),
            "FAILED" | "CANCELED" => Some(CiState::Failure),
            "CREATED"
            | "WAITING_FOR_RESOURCE"
            | "PREPARING"
            | "PENDING"
            | "RUNNING"
            | "SCHEDULED"
            | "WAITING_FOR_CALLBACK"
            | "CANCELING" => Some(CiState::Pending),
            _ => None,
        });
    Some(PrBadge { number, state, ci })
}

/// A scripted [`GlabRunner`]: answers each call with the next reply, keeps
/// every command, and never starts a process.
#[cfg(any(test, feature = "test-support"))]
#[derive(Default)]
pub struct FakeGlab {
    replies: std::sync::Mutex<std::collections::VecDeque<Option<GhOutput>>>,
    calls: std::sync::Mutex<Vec<GlabCommand>>,
    /// What `glab` would inherit from Kika's environment, such as a token.
    inherited: Vec<(String, String)>,
}

#[cfg(any(test, feature = "test-support"))]
impl FakeGlab {
    pub fn new() -> Self {
        Self::default()
    }

    /// Let `glab` see `key=value`, as it would from the user's environment.
    pub fn with_inherited_env(mut self, key: &str, value: &str) -> Self {
        self.inherited.push((key.into(), value.into()));
        self
    }

    /// Whether a call ran with `key=value` in its environment: what it
    /// inherited, less what is removed, plus what is set.
    pub fn saw_env(&self, key: &str, value: &str) -> bool {
        !self.calls().is_empty()
            && !GLAB_ENV_REMOVE.contains(&key)
            && GLAB_ENV
                .iter()
                .map(|(k, v)| (*k, *v))
                .chain(self.inherited.iter().map(|(k, v)| (k.as_str(), v.as_str())))
                .any(|pair| pair == (key, value))
    }

    pub fn reply(self, output: GhOutput) -> Self {
        self.replies.lock().unwrap().push_back(Some(output));
        self
    }

    /// The next call fails to start, as with no `glab` installed.
    pub fn missing(self) -> Self {
        self.replies.lock().unwrap().push_back(None);
        self
    }

    pub fn calls(&self) -> Vec<GlabCommand> {
        self.calls.lock().unwrap().clone()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl GlabRunner for FakeGlab {
    fn run(&self, cmd: &GlabCommand) -> io::Result<GhOutput> {
        self.calls.lock().unwrap().push(cmd.clone());
        let reply = self.replies.lock().unwrap().pop_front();
        match reply.expect("unexpected glab call") {
            Some(output) => Ok(output),
            None => Err(io::ErrorKind::NotFound.into()),
        }
    }
}

/// Hand-written answers in the shape GitLab's GraphQL API gives with
/// `--include`. Build replies with [`github::answer`](super::github::answer).
#[cfg(any(test, feature = "test-support"))]
pub mod fixtures {
    pub const STATES: &str = include_str!("fixtures/gitlab_states.http");
    pub const PROJECT_NULL: &str = include_str!("fixtures/gitlab_project_null.http");
    pub const UNAUTHORIZED_401: &str = include_str!("fixtures/gitlab_unauthorized_401.http");
    pub const RATE_LIMITED_429: &str = include_str!("fixtures/gitlab_rate_limited_429.http");
    pub const SERVER_ERROR: &str = include_str!("fixtures/gitlab_server_error.http");
    pub const GRAPHQL_ERRORS: &str = include_str!("fixtures/gitlab_graphql_errors.http");

    /// The token a leaky `glab` holds and prints; it must never leave `glab`.
    pub const SECRET_TOKEN: &str = "glpat-SENTINEL";

    /// A `glab` that holds [`SECRET_TOKEN`] and prints it with every
    /// answer, on stderr and in a header, for two refreshes: the badges,
    /// then a 401.
    pub fn leaky_glab() -> super::FakeGlab {
        use super::super::github::answer;
        use super::GhOutput;
        let noisy = |output: GhOutput| GhOutput {
            stderr: format!("GITLAB_TOKEN={SECRET_TOKEN}\n").into_bytes(),
            ..output
        };
        let header = format!("PRIVATE-TOKEN: {SECRET_TOKEN}\r\nContent-Type:");
        let states = STATES.replacen("Content-Type:", &header, 1);
        let refused = UNAUTHORIZED_401.replacen("Content-Type:", &header, 1);
        super::FakeGlab::new()
            .with_inherited_env("GITLAB_TOKEN", SECRET_TOKEN)
            .reply(noisy(answer(&states)))
            .reply(noisy(answer(&refused)))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::fixtures::*;
    use super::*;
    use crate::github::{answer, failure};
    use crate::url::ForgeKind;
    use kika_git::branches::BranchName;
    use kika_git::recording::CommandEvent;

    /// A fixed clock: 2023-11-14 22:13:20 UTC.
    const NOW: i64 = 1_700_000_000;

    fn host() -> ForgeHost {
        ForgeHost::parse("gitlab.example.com")
            .unwrap()
            .with_kind(ForgeKind::GitLab)
    }

    fn repo() -> ForgeRepo {
        ForgeRepo {
            host: "gitlab.example.com".into(),
            owner: "acme".into(),
            name: "widgets".into(),
        }
    }

    fn names(n: usize) -> Vec<BranchName> {
        (0..n)
            .map(|i| format!("topic-{i}").as_str().into())
            .collect()
    }

    fn glab<'a>(runner: &'a FakeGlab, sink: &'a CommandSink, host: &'a ForgeHost) -> Glab<'a> {
        Glab {
            runner,
            sink,
            host,
            cwd: "/cache".into(),
            now: NOW,
        }
    }

    fn own(branches: &[BranchName]) -> Vec<Head> {
        super::super::head::heads(branches, &HashMap::new(), &repo())
    }

    fn fetch_heads(runner: &FakeGlab, heads: &[Head]) -> Result<Fetched, Refusal> {
        glab(runner, &CommandSink::new(), &host()).fetch_badges(&repo(), heads)
    }

    fn fetch(
        runner: &FakeGlab,
        branches: &[BranchName],
    ) -> Result<HashMap<BranchName, PrBadge>, Refusal> {
        fetch_heads(runner, &own(branches)).map(|f| f.badges)
    }

    fn body(cmd: &GlabCommand) -> serde_json::Value {
        serde_json::from_slice(&cmd.stdin()).unwrap()
    }

    fn classify_ok(output: GhOutput) -> Answer {
        classify(Ok(output), NOW)
    }

    #[test]
    fn the_graphql_argv_is_fixed_and_names_only_the_host() {
        let runner = FakeGlab::new().reply(answer(STATES));
        let heads = [Head {
            branch: "evil".into(),
            owner: "acme".into(),
            name: "--hostname=evil".into(),
        }];
        fetch_heads(&runner, &heads).unwrap();
        let [cmd] = runner.calls().try_into().unwrap();
        assert_eq!(
            cmd.args(),
            [
                "api",
                "graphql",
                "--include",
                "--hostname",
                "gitlab.example.com",
                "--input",
                "-"
            ]
            .map(OsString::from)
        );
        assert_eq!(
            cmd.display_line(),
            "glab api graphql --include --hostname gitlab.example.com --input -"
        );
        assert_eq!(cmd.cwd, PathBuf::from("/cache"));
        assert_eq!(cmd.timeout, Duration::from_secs(20));
        // The branch travels on stdin only.
        let argv: Vec<String> = cmd
            .args()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(argv.iter().all(|a| !a.contains("evil")), "{argv:?}");
        assert!(!cmd.display_line().contains("evil"));
        assert_eq!(body(&cmd)["variables"]["b0"], "--hostname=evil");
    }

    #[test]
    fn names_travel_as_variables_and_never_in_the_query() {
        let runner = FakeGlab::new().reply(answer(STATES));
        let heads = [
            Head {
                branch: "local-only".into(),
                owner: "acme".into(),
                name: "feature/login".into(),
            },
            Head {
                branch: "fix".into(),
                owner: "forker".into(),
                name: "fix\"]){project".into(),
            },
        ];
        fetch_heads(&runner, &heads).unwrap();
        let body = body(&runner.calls()[0]);
        let query = body["query"].as_str().unwrap();
        for word in [
            "feature/login",
            "fix\"",
            "local-only",
            "forker",
            "acme",
            "widgets",
        ] {
            assert!(!query.contains(word), "{word} in {query}");
        }
        let vars = &body["variables"];
        assert_eq!(vars["path"], "acme/widgets");
        assert_eq!(vars["b0"], "feature/login");
        assert_eq!(vars["b1"], "fix\"]){project");
        assert_eq!(vars.as_object().unwrap().len(), 3);
        let sent = body.to_string();
        assert!(!sent.contains("forker") && !sent.contains("local-only"));
        assert!(
            query.starts_with("query($path:ID!,$b0:String!,$b1:String!){project(fullPath:$path){")
        );
        assert!(query.contains(
            "b1:mergeRequests(sourceBranches:[$b1],sort:CREATED_DESC,first:5)\
             {nodes{iid state draft sourceProject{fullPath} headPipeline{status}}}"
        ));
        assert_eq!(query, super::query(2));
    }

    #[test]
    fn each_mr_state_and_pipeline_maps_to_its_badge() {
        use CiState::{Failure, Pending, Success};
        use PrState::{Closed, Draft, Merged, Open};
        let runner = FakeGlab::new().reply(answer(STATES));
        let branches = names(19);
        let badges = fetch(&runner, &branches).unwrap();
        let pr = |number, state, ci| Some(PrBadge { number, state, ci });
        let want = [
            pr(1, Open, Some(Success)),
            pr(2, Draft, Some(Pending)),
            pr(3, Merged, None),
            pr(4, Closed, Some(Failure)),
            // `locked` shows as open; a cancelled pipeline failed.
            pr(5, Open, Some(Failure)),
            pr(6, Open, Some(Pending)),
            pr(7, Open, Some(Pending)),
            pr(8, Open, Some(Pending)),
            pr(9, Open, Some(Pending)),
            // Scheduled runs on its own; manual waits for a person.
            pr(10, Open, Some(Pending)),
            pr(11, Open, None),
            pr(12, Open, Some(Pending)),
            pr(13, Open, Some(Pending)),
            // Skipped, and a status Kika does not know: no mark.
            pr(14, Open, None),
            pr(15, Open, None),
            // Another namespace's newer MR is passed over for the head's
            // own, its namespace compared without regard to case.
            pr(16, Merged, Some(Success)),
            // Only another namespace (`acmeish` is not `acme`) or a
            // deleted source project: nothing.
            None,
            // A bad iid drops the node.
            None,
            // No MR at all.
            None,
        ];
        for (i, want) in want.iter().enumerate() {
            assert_eq!(badges.get(&branches[i]).copied(), *want, "b{i}");
        }
        assert_eq!(badges.len(), 16);
    }

    #[test]
    fn only_a_plain_decimal_iid_is_a_number() {
        let node = |iid: &str| MrNode {
            iid: iid.into(),
            state: "opened".into(),
            draft: false,
            source_project: None,
            head_pipeline: None,
        };
        assert_eq!(badge_of(&node("42")).map(|b| b.number), Some(42));
        for bad in [
            "",
            "-3",
            "+3",
            "4.0",
            " 4",
            "0x10",
            "99999999999999999999999",
        ] {
            assert_eq!(badge_of(&node(bad)), None, "{bad:?}");
        }
        let odd = MrNode {
            state: "all".into(),
            ..node("1")
        };
        assert_eq!(badge_of(&odd), None);
    }

    #[test]
    fn each_answer_classifies_to_its_status() {
        let failed = |status| Answer::Failed(status);
        let truncated = &STATES[..STATES.len() / 2];
        let not_found = SERVER_ERROR.replace("502 Bad Gateway", "404 Not Found");
        let forbidden = SERVER_ERROR.replace("502 Bad Gateway", "403 Forbidden");
        let cases = [
            (
                Err(io::ErrorKind::NotFound.into()),
                failed(ForgeStatus::NoGh),
            ),
            (
                Err(io::ErrorKind::Other.into()),
                failed(ForgeStatus::Failed),
            ),
            (
                Ok(GhOutput {
                    timed_out: true,
                    ..GhOutput::default()
                }),
                failed(ForgeStatus::Offline),
            ),
            (
                Ok(failure(
                    1,
                    "Post \"https://gitlab.example.com/api/graphql\": dial tcp: lookup gitlab.example.com: no such host",
                )),
                failed(ForgeStatus::Offline),
            ),
            (
                Ok(answer(UNAUTHORIZED_401)),
                failed(ForgeStatus::NotLoggedIn),
            ),
            (Ok(answer(PROJECT_NULL)), failed(ForgeStatus::NoAccess)),
            (Ok(answer(&not_found)), failed(ForgeStatus::NoAccess)),
            (Ok(answer(&forbidden)), failed(ForgeStatus::NoAccess)),
            (Ok(answer(SERVER_ERROR)), failed(ForgeStatus::Failed)),
            (Ok(answer(GRAPHQL_ERRORS)), failed(ForgeStatus::Failed)),
            (Ok(answer(truncated)), failed(ForgeStatus::Failed)),
            (
                Ok(answer("HTTP/2.0 200 OK\r\n\r\n{\"data\":null}")),
                failed(ForgeStatus::Failed),
            ),
            (
                Ok(answer("HTTP/2.0 ???\u{1} not an answer")),
                failed(ForgeStatus::Failed),
            ),
            (
                Ok(failure(1, "glab: something odd")),
                failed(ForgeStatus::Failed),
            ),
        ];
        for (output, want) in cases {
            let shown = format!("{output:?}");
            assert_eq!(classify(output, NOW), want, "{shown}");
        }
        assert!(matches!(
            classify_ok(answer(STATES)),
            Answer::Body { etag: None, .. }
        ));
    }

    /// `answer` with `glab`'s exit 1, as it exits on any GraphQL error.
    fn exit_1(stdout: &str) -> GhOutput {
        GhOutput {
            exit: Some(1),
            ..answer(stdout)
        }
    }

    #[test]
    fn a_200_body_is_read_even_when_glab_exits_non_zero() {
        // GraphQL errors inside a 200: refused, by the body.
        assert_eq!(
            classify_ok(exit_1(GRAPHQL_ERRORS)),
            Answer::Failed(ForgeStatus::Failed)
        );
        // Valid data inside a 200: read, and its badges found.
        assert!(matches!(
            classify_ok(exit_1(STATES)),
            Answer::Body { etag: None, .. }
        ));
        let runner = FakeGlab::new().reply(exit_1(STATES));
        let branches = names(1);
        let badges = fetch(&runner, &branches).unwrap();
        assert_eq!(badges[&branches[0]].number, 1);
        // With no status line, the exit code decides.
        let bare = "{\"data\":{\"project\":{}}}";
        assert_eq!(
            classify_ok(exit_1(bare)),
            Answer::Failed(ForgeStatus::Failed)
        );
        assert!(matches!(classify_ok(answer(bare)), Answer::Body { .. }));
    }

    #[test]
    fn rate_limits_are_classified_with_until_clamped() {
        let limited = |until| Answer::Limited { until };
        assert_eq!(classify_ok(answer(RATE_LIMITED_429)), limited(NOW + 1200));
        // No reset: the wait; neither: a minute.
        let no_reset = RATE_LIMITED_429.replace("Ratelimit-Reset: 1700001200\r\n", "");
        let wait = no_reset.replace("Retry-After: 1200", "Retry-After: 300");
        assert_eq!(classify_ok(answer(&wait)), limited(NOW + 300));
        let bare = no_reset.replace("Retry-After: 1200\r\n", "");
        assert_eq!(classify_ok(answer(&bare)), limited(NOW + 60));
        // Far ahead, in the past, or a huge wait: clamped.
        let far = RATE_LIMITED_429.replace("1700001200", "1800000000");
        assert_eq!(classify_ok(answer(&far)), limited(NOW + 3600));
        let past = RATE_LIMITED_429.replace("1700001200", "1");
        assert_eq!(classify_ok(answer(&past)), limited(NOW + 60));
        let huge = wait.replace("Retry-After: 300", "Retry-After: 9223372036854775807");
        assert_eq!(classify_ok(answer(&huge)), limited(NOW + 3600));
        // A 403 is a limit only when none remain.
        let spent = RATE_LIMITED_429.replace("429 Too Many Requests", "403 Forbidden");
        assert_eq!(classify_ok(answer(&spent)), limited(NOW + 1200));
        let left = spent.replace("Ratelimit-Remaining: 0", "Ratelimit-Remaining: 5");
        assert_eq!(
            classify_ok(answer(&left)),
            Answer::Failed(ForgeStatus::NoAccess)
        );
    }

    #[test]
    fn the_env_disables_prompts_and_holds_no_token() {
        assert_eq!(
            GLAB_ENV,
            [
                ("NO_COLOR", "1"),
                ("NO_PROMPT", "1"),
                ("GLAB_CHECK_UPDATE", "false"),
                ("GLAB_PAGER", "cat"),
                ("GLAB_SEND_TELEMETRY", "0"),
            ]
        );
        for key in [
            "GITLAB_HOST",
            "GL_HOST",
            "GITLAB_URI",
            "GITLAB_API_HOST",
            "GITLAB_URL",
            "GITLAB_REPO",
        ] {
            assert!(GLAB_ENV_REMOVE.contains(&key), "{key}");
        }
        assert!(GLAB_ENV_REMOVE.contains(&kika_git::askpass::SOCKET_ENV));
        assert!(GLAB_ENV_REMOVE.contains(&kika_git::rebase_plan::PLAN_ENV));
        // The token is inherited, never set or removed by Kika.
        assert!(!GLAB_ENV_REMOVE.contains(&"GITLAB_TOKEN"));
        for (key, value) in GLAB_ENV {
            assert!(!key.contains("TOKEN") && !value.contains("glpat"), "{key}");
        }
    }

    #[test]
    fn many_branches_go_in_batches_of_fifty_up_to_five_hundred() {
        let runner = FakeGlab::new()
            .reply(answer(STATES))
            .reply(answer(STATES))
            .reply(answer(STATES));
        let branches = names(120);
        let badges = fetch(&runner, &branches).unwrap();
        let calls = runner.calls();
        assert_eq!(calls.len(), 3);
        assert_eq!(body(&calls[1])["variables"]["b0"], "topic-50");
        assert!(body(&calls[2])["variables"].get("b19").is_some());
        assert!(body(&calls[2])["variables"].get("b20").is_none());
        // Each batch maps back by its own aliases.
        assert_eq!(badges[&branches[50]].number, 1);
        assert_eq!(badges[&branches[100]].number, 1);

        let empty = "HTTP/2.0 200 OK\r\n\r\n{\"data\":{\"project\":{}}}";
        let mut runner = FakeGlab::new();
        for _ in 0..10 {
            runner = runner.reply(answer(empty));
        }
        fetch(&runner, &names(600)).unwrap();
        assert_eq!(runner.calls().len(), 10);
    }

    #[test]
    fn the_first_refusal_stops_the_batches() {
        let runner = FakeGlab::new()
            .reply(answer(STATES))
            .reply(answer(RATE_LIMITED_429));
        assert_eq!(
            fetch(&runner, &names(120)),
            Err(Refusal::Limited { until: NOW + 1200 })
        );
        assert_eq!(runner.calls().len(), 2);
        let runner = FakeGlab::new().reply(answer(PROJECT_NULL));
        assert_eq!(
            fetch(&runner, &names(120)),
            Err(Refusal::Status(ForgeStatus::NoAccess))
        );
        assert_eq!(runner.calls().len(), 1);
        let runner = FakeGlab::new().missing();
        assert_eq!(
            fetch(&runner, &names(1)),
            Err(Refusal::Status(ForgeStatus::NoGh))
        );
    }

    #[test]
    fn each_call_is_logged_with_the_fixed_line_and_its_own_id() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = CommandSink::new();
        let keep = Arc::clone(&events);
        sink.connect(move |ev| keep.lock().unwrap().push(ev));
        let runner = FakeGlab::new().reply(answer(STATES)).missing();
        let host = host();
        let result = glab(&runner, &sink, &host).fetch_badges(&repo(), &own(&names(60)));
        assert_eq!(result, Err(Refusal::Status(ForgeStatus::NoGh)));
        let events = events.lock().unwrap();
        let line = "glab api graphql --include --hostname gitlab.example.com --input -";
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
                assert_ne!(a, c);
            }
            other => panic!("{other:?}"),
        }
    }
}
