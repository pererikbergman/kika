//! Pull requests from GitHub, read through `gh api graphql`. `gh` holds
//! the login, so Kika never sees a token: it passes none, stores none and
//! draws none. `gh`'s stderr is only matched against fixed markers, never
//! kept.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::json;

use super::{CiState, ForgeHost, ForgeRepo, ForgeStatus, PrBadge, PrState};
use crate::git::branches::BranchName;
use crate::git::recording::{CommandEvent, CommandOutcome, CommandSink};

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
    crate::git::askpass::SOCKET_ENV,
    crate::sequence_editor::PLAN_ENV,
];

/// One `gh api graphql` call: the host, and the JSON body for stdin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhCommand {
    pub host: String,
    pub body: Vec<u8>,
    /// Where `gh` runs: Kika's cache directory, never the repository, so
    /// `gh` reads no repository's git config.
    pub cwd: PathBuf,
    pub timeout: Duration,
}

impl GhCommand {
    /// The arguments after `gh`. Nothing from the repository is in them:
    /// owner, name and branches travel in the body as variables.
    pub fn args(&self) -> Vec<OsString> {
        ["api", "graphql", "--hostname", &self.host, "--input", "-"]
            .map(OsString::from)
            .into()
    }

    /// The Command log's line, which is also the whole argv.
    pub fn display_line(&self) -> String {
        format!("gh api graphql --hostname {} --input -", self.host)
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
        let mut command = Command::new("gh");
        command
            .args(cmd.args())
            .current_dir(&cmd.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in GH_ENV {
            command.env(key, value);
        }
        for key in GH_ENV_REMOVE {
            command.env_remove(key);
        }
        let mut child = command.spawn()?;
        let stdin = child.stdin.take();
        let body = cmd.body.clone();
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
        let deadline = Instant::now() + cmd.timeout;
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
        // `gh` has ended, but a process it started could still hold the
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
}

/// The pull request of each of `branches` in `repo`, asked in batches of
/// [`BATCH`], one call after another, each one shown in the Command log.
/// The first failure ends it.
pub fn fetch_badges(
    gh: &dyn GhRunner,
    sink: &CommandSink,
    host: &ForgeHost,
    repo: &ForgeRepo,
    branches: &[BranchName],
    cwd: PathBuf,
) -> Result<HashMap<BranchName, PrBadge>, ForgeStatus> {
    let mut badges = HashMap::new();
    let branches = &branches[..branches.len().min(MAX_BRANCHES)];
    for batch in branches.chunks(BATCH) {
        let cmd = GhCommand {
            host: host.as_str().to_string(),
            body: request_body(repo, batch),
            cwd: cwd.clone(),
            timeout: TIMEOUT,
        };
        let id = sink.next_id();
        sink.send(CommandEvent::Started {
            id,
            line: cmd.display_line(),
            attached: false,
        });
        let start = Instant::now();
        let output = gh.run(&cmd);
        let result = read_answer(output, batch.len());
        sink.send(CommandEvent::Finished {
            id,
            outcome: match &result {
                Ok(_) => CommandOutcome::Ok,
                Err(ForgeStatus::NoGh) => CommandOutcome::NotStarted,
                Err(_) => CommandOutcome::Failed(None),
            },
            elapsed: start.elapsed(),
        });
        for (branch, badge) in batch.iter().zip(result?) {
            if let Some(badge) = badge {
                badges.insert(branch.clone(), badge);
            }
        }
    }
    Ok(badges)
}

/// The GraphQL query for `count` branches. Fixed text: aliases are `b0…`,
/// and every name is a variable, so nothing from the repository is ever
/// spliced into it.
fn query(count: usize) -> String {
    let mut params = String::from("$owner:String!,$name:String!");
    let mut fields = String::new();
    for i in 0..count {
        params.push_str(&format!(",$b{i}:String!"));
        fields.push_str(&format!(
            " b{i}:pullRequests(headRefName:$b{i},first:1,orderBy:{{field:CREATED_AT,direction:DESC}})\
             {{nodes{{number state isDraft commits(last:1){{nodes{{commit{{statusCheckRollup{{state}}}}}}}}}}}}"
        ));
    }
    format!("query({params}){{repository(owner:$owner,name:$name){{{fields} }}}}")
}

/// `{query, variables}` for stdin.
fn request_body(repo: &ForgeRepo, batch: &[BranchName]) -> Vec<u8> {
    let mut variables = serde_json::Map::new();
    variables.insert("owner".into(), json!(repo.owner));
    variables.insert("name".into(), json!(repo.name));
    for (i, branch) in batch.iter().enumerate() {
        // Only UTF-8 names reach here; see `ForgeState::set_branches`.
        let name = String::from_utf8_lossy(&branch.0);
        variables.insert(format!("b{i}"), json!(name));
    }
    serde_json::to_vec(&json!({ "query": query(batch.len()), "variables": variables }))
        .unwrap_or_default()
}

#[derive(Deserialize)]
struct Response {
    data: Option<Data>,
    errors: Option<Vec<GqlError>>,
}

#[derive(Deserialize)]
struct Data {
    repository: Option<HashMap<String, Connection>>,
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
    commits: Commits,
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
const OFFLINE_MARKERS: [&str; 8] = [
    "dial tcp",
    "no such host",
    "connection refused",
    "connection reset",
    "network is unreachable",
    "i/o timeout",
    "tls handshake timeout",
    "deadline exceeded",
];

/// The badge for each of `count` aliases in order, or why there are none.
fn read_answer(
    output: io::Result<GhOutput>,
    count: usize,
) -> Result<Vec<Option<PrBadge>>, ForgeStatus> {
    let output = match output {
        Ok(output) => output,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(ForgeStatus::NoGh),
        Err(_) => return Err(ForgeStatus::Failed),
    };
    if output.timed_out {
        return Err(ForgeStatus::Offline);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    if output.exit == Some(4) || stderr.contains("http 401") {
        return Err(ForgeStatus::NotLoggedIn);
    }
    if OFFLINE_MARKERS.iter().any(|m| stderr.contains(m)) {
        return Err(ForgeStatus::Offline);
    }
    let response: Response =
        serde_json::from_slice(&output.stdout).map_err(|_| ForgeStatus::Failed)?;
    let denied = response
        .errors
        .iter()
        .flatten()
        .any(|e| matches!(e.kind.as_deref(), Some("NOT_FOUND" | "FORBIDDEN")));
    if denied {
        return Err(ForgeStatus::NoAccess);
    }
    let repository = match (output.exit, response.data) {
        (
            Some(0),
            Some(Data {
                repository: Some(repository),
            }),
        ) => repository,
        _ => return Err(ForgeStatus::Failed),
    };
    // By alias index, never by anything the answer names.
    Ok((0..count)
        .map(|i| {
            let pr = repository.get(&format!("b{i}"))?.nodes.first()?;
            badge_of(pr)
        })
        .collect())
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
#[cfg(test)]
pub struct FakeGh {
    replies: std::sync::Mutex<std::collections::VecDeque<Option<GhOutput>>>,
    calls: std::sync::Mutex<Vec<GhCommand>>,
    /// What `gh` would inherit from Kika's environment, such as a token.
    inherited: Vec<(String, String)>,
}

#[cfg(test)]
impl FakeGh {
    pub fn new() -> Self {
        Self {
            replies: Default::default(),
            calls: Default::default(),
            inherited: Vec::new(),
        }
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

#[cfg(test)]
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

/// A successful answer with `stdout`.
#[cfg(test)]
pub fn answer(stdout: &str) -> GhOutput {
    GhOutput {
        exit: Some(0),
        stdout: stdout.as_bytes().to_vec(),
        ..GhOutput::default()
    }
}

/// A failed run with `exit` and `stderr`.
#[cfg(test)]
pub fn failure(exit: i32, stderr: &str) -> GhOutput {
    GhOutput {
        exit: Some(exit),
        stderr: stderr.as_bytes().to_vec(),
        ..GhOutput::default()
    }
}

#[cfg(test)]
pub mod fixtures {
    pub const STATES: &str = include_str!("fixtures/states.json");
    pub const NOT_FOUND: &str = include_str!("fixtures/not_found.json");
    pub const FORBIDDEN: &str = include_str!("fixtures/forbidden.json");
    pub const NOT_FOUND_STDERR: &str = include_str!("fixtures/not_found.stderr");
    pub const LOGGED_OUT_STDERR: &str = include_str!("fixtures/logged_out.stderr");
    pub const HTTP_401_STDERR: &str = include_str!("fixtures/http_401.stderr");
    pub const DNS_STDERR: &str = include_str!("fixtures/dns.stderr");
    pub const TIMEOUT_STDERR: &str = include_str!("fixtures/timeout.stderr");
    pub const OTHER_STDERR: &str = include_str!("fixtures/other.stderr");
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::fixtures::*;
    use super::*;

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

    fn fetch(
        gh: &FakeGh,
        branches: &[BranchName],
    ) -> Result<HashMap<BranchName, PrBadge>, ForgeStatus> {
        fetch_badges(
            gh,
            &CommandSink::new(),
            &host(),
            &repo(),
            branches,
            "/cache".into(),
        )
    }

    fn body(cmd: &GhCommand) -> serde_json::Value {
        serde_json::from_slice(&cmd.body).unwrap()
    }

    #[test]
    fn the_argv_is_fixed_and_names_only_the_host() {
        let gh = FakeGh::new().reply(answer(STATES));
        fetch(&gh, &names(1)).unwrap();
        let [cmd] = gh.calls().try_into().unwrap();
        assert_eq!(
            cmd.args(),
            ["api", "graphql", "--hostname", "github.com", "--input", "-"].map(OsString::from)
        );
        assert_eq!(
            cmd.display_line(),
            "gh api graphql --hostname github.com --input -"
        );
        assert_eq!(cmd.cwd, PathBuf::from("/cache"));
        assert_eq!(cmd.timeout, Duration::from_secs(20));
    }

    #[test]
    fn names_travel_as_variables_and_never_in_the_query() {
        let gh = FakeGh::new().reply(answer(STATES));
        let branches: Vec<BranchName> = vec!["feature/login".into(), "fix\"}{repository".into()];
        fetch(&gh, &branches).unwrap();
        let body = body(&gh.calls()[0]);
        let query = body["query"].as_str().unwrap();
        for word in ["feature/login", "fix\"", "acme", "widgets"] {
            assert!(!query.contains(word), "{word} in {query}");
        }
        let vars = &body["variables"];
        assert_eq!(vars["owner"], "acme");
        assert_eq!(vars["name"], "widgets");
        assert_eq!(vars["b0"], "feature/login");
        assert_eq!(vars["b1"], "fix\"}{repository");
        assert!(query.contains(
            "b1:pullRequests(headRefName:$b1,first:1,orderBy:{field:CREATED_AT,direction:DESC})"
        ));
        assert!(query.contains("statusCheckRollup{state}"));
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
        let gh = FakeGh::new().reply(answer(STATES));
        let branches = names(7);
        let badges = fetch(&gh, &branches).unwrap();
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
        let gh = FakeGh::new()
            .reply(answer(STATES))
            .reply(answer(STATES))
            .reply(answer(STATES));
        let branches = names(120);
        let badges = fetch(&gh, &branches).unwrap();
        let calls = gh.calls();
        assert_eq!(calls.len(), 3);
        assert_eq!(body(&calls[1])["variables"]["b0"], "topic-50");
        assert!(body(&calls[2])["variables"].get("b20").is_none());
        // Each batch maps back by its own aliases.
        assert_eq!(badges[&branches[50]].number, 101);
        assert_eq!(badges[&branches[100]].number, 101);

        let mut gh = FakeGh::new();
        for _ in 0..10 {
            gh = gh.reply(answer("{\"data\":{\"repository\":{}}}"));
        }
        fetch(&gh, &names(600)).unwrap();
        assert_eq!(gh.calls().len(), 10);
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
            (FakeGh::new().missing(), ForgeStatus::NoGh),
            (
                FakeGh::new().reply(failure(1, OTHER_STDERR)),
                ForgeStatus::Failed,
            ),
            (FakeGh::new().reply(answer("not json")), ForgeStatus::Failed),
            (
                FakeGh::new().reply(answer("{\"data\":null}")),
                ForgeStatus::Failed,
            ),
        ];
        for (gh, want) in cases {
            assert_eq!(fetch(&gh, &names(2)), Err(want));
        }
        // The first failure stops the batches.
        let gh = FakeGh::new().reply(failure(1, DNS_STDERR));
        assert_eq!(fetch(&gh, &names(120)), Err(ForgeStatus::Offline));
        assert_eq!(gh.calls().len(), 1);
    }

    #[test]
    fn each_call_is_logged_with_the_fixed_line_and_its_own_id() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = CommandSink::new();
        let keep = Arc::clone(&events);
        sink.connect(move |ev| keep.lock().unwrap().push(ev));
        let taken = sink.next_id();
        let gh = FakeGh::new().reply(answer(STATES)).missing();
        let result = fetch_badges(&gh, &sink, &host(), &repo(), &names(60), "/c".into());
        assert_eq!(result, Err(ForgeStatus::NoGh));
        let events = events.lock().unwrap();
        let line = "gh api graphql --hostname github.com --input -";
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
