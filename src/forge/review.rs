//! Pull requests to review on GitHub: four queues in one GraphQL call,
//! and one pull request's detail in another. Every string GitHub sends is
//! made terminal-safe while it is parsed, so no type here ever holds raw
//! server text. The repository and the number travel as GraphQL
//! variables on stdin; argv is the fixed `gh api graphql` line.

use serde::Deserialize;
use serde_json::json;

use super::github::{Answer, Gh, GhCall, MAX_URL, Refusal, classify, split_include};
use super::remote::{matches, parse_remote, segment_ok};
use super::task::ForgeContext;
use super::{ForgeHost, ForgeRepo, ForgeStatus, PrState};
use crate::git::GitRunner;
use crate::git::diff::{EmojiZwj, sanitize, sanitize_body};
use crate::git::forge_remote::read_remote_url;
use crate::git::recording::CommandOutcome;

/// Pull requests asked for per queue; there is no paging past it.
pub const QUEUE_SIZE: usize = 50;
/// The most checks, labels and reviewers kept from a detail.
pub const MAX_CHECKS: usize = 100;
pub const MAX_LABELS: usize = 20;
pub const MAX_REVIEWERS: usize = 20;
/// The most of a description read, in bytes.
pub const MAX_BODY: usize = 64 * 1024;
/// The most of a one-line field kept, in characters.
const MAX_FIELD: usize = 512;

/// The four lists of pull requests review mode shows, all limited to the
/// repository of the remote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueKind {
    Assigned,
    ReviewRequested,
    Authored,
    AllOpen,
}

impl QueueKind {
    pub const ALL: [QueueKind; 4] = [
        QueueKind::Assigned,
        QueueKind::ReviewRequested,
        QueueKind::Authored,
        QueueKind::AllOpen,
    ];

    pub fn title(self) -> &'static str {
        match self {
            QueueKind::Assigned => "Assigned",
            QueueKind::ReviewRequested => "Review requested",
            QueueKind::Authored => "Authored",
            QueueKind::AllOpen => "All open",
        }
    }

    /// What narrows the repository's open pull requests to this queue.
    fn qualifier(self) -> Option<&'static str> {
        match self {
            QueueKind::Assigned => Some("assignee:@me"),
            QueueKind::ReviewRequested => Some("review-requested:@me"),
            QueueKind::Authored => Some("author:@me"),
            QueueKind::AllOpen => None,
        }
    }

    fn index(self) -> usize {
        match self {
            QueueKind::Assigned => 0,
            QueueKind::ReviewRequested => 1,
            QueueKind::Authored => 2,
            QueueKind::AllOpen => 3,
        }
    }

    /// The next queue, or the previous one with `back`, wrapping around.
    pub fn step(self, back: bool) -> QueueKind {
        let by = if back { 3 } else { 1 };
        QueueKind::ALL[(self.index() + by) % 4]
    }
}

/// One row of a queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrSummary {
    pub number: u64,
    pub title: String,
    pub author: String,
    pub draft: bool,
    /// When it last changed, as GitHub wrote it.
    pub updated: String,
}

/// Each queue's pull requests, in GitHub's order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Queues([Vec<PrSummary>; 4]);

impl Queues {
    pub fn get(&self, kind: QueueKind) -> &[PrSummary] {
        &self.0[kind.index()]
    }
}

/// What a reviewer last said about the pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewVerdict {
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
    Pending,
}

/// Whether GitHub can merge the head into the base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mergeability {
    Mergeable,
    Conflicting,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Pending,
    Success,
    Failure,
    Neutral,
    Skipped,
}

/// One CI check on the head commit: a check run or a commit status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub status: CheckStatus,
    /// An `https` page about the check, safe to hand to a browser.
    pub details_url: Option<String>,
}

/// One pull request as review mode shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrDetail {
    pub number: u64,
    pub title: String,
    pub author: String,
    /// Open, merged or closed; a draft is `Open` with `draft` set.
    pub state: PrState,
    pub draft: bool,
    /// The description, line by line made terminal-safe, joined by `\n`.
    pub body: String,
    pub labels: Vec<String>,
    pub requested_reviewers: Vec<String>,
    pub reviews: Vec<(String, ReviewVerdict)>,
    pub base: String,
    pub head: String,
    pub mergeable: Mergeability,
    pub checks: Vec<Check>,
}

/// Why review mode has nothing to show. A fixed set: nothing `gh` or
/// GitHub says is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewError {
    /// The remote is on no `[forge] github` host; nobody was asked.
    NoForgeHost,
    /// GitHub refuses calls until this second.
    Limited {
        until: i64,
    },
    Status(ForgeStatus),
}

impl From<Refusal> for ReviewError {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::Limited { until } => ReviewError::Limited { until },
            Refusal::Status(status) => ReviewError::Status(status),
        }
    }
}

impl Gh<'_> {
    /// The four queues of `repo`, in one call.
    pub fn fetch_queues(&self, repo: &ForgeRepo) -> Result<Queues, Refusal> {
        if !segment_ok(&repo.owner) || !segment_ok(&repo.name) {
            return Err(Refusal::Status(ForgeStatus::Failed));
        }
        let kind = GhCall::Graphql {
            body: queues_body(repo),
        };
        self.call(kind, |answer| read_with(answer, read_queues), outcome)
    }

    /// Pull request `number` of `repo`; `None` when GitHub has none.
    pub fn fetch_detail(&self, repo: &ForgeRepo, number: u64) -> Result<Option<PrDetail>, Refusal> {
        // GraphQL numbers are 32-bit; a larger one names no pull request.
        if number > i32::MAX as u64 {
            return Ok(None);
        }
        let kind = GhCall::Graphql {
            body: detail_body(repo, number),
        };
        let now = self.now;
        self.run_logged(
            kind,
            |output| {
                let stdout = output
                    .as_ref()
                    .map(|o| o.stdout.clone())
                    .unwrap_or_default();
                let (status, _, body) = split_include(&stdout);
                match classify(output, now) {
                    // GitHub says NOT_FOUND beside `pullRequest: null`; that
                    // is an answer, not a lack of access.
                    Answer::Failed(ForgeStatus::NoAccess)
                        if status == Some(200) && pull_request_null(body) =>
                    {
                        Ok(None)
                    }
                    answer => read_with(answer, read_detail),
                }
            },
            outcome,
        )
    }
}

fn outcome<T>(result: &Result<T, Refusal>) -> CommandOutcome {
    match result {
        Ok(_) => CommandOutcome::Ok,
        Err(Refusal::Status(ForgeStatus::NoGh)) => CommandOutcome::NotStarted,
        Err(_) => CommandOutcome::Failed(None),
    }
}

/// `read` the body of a 2xx answer; anything else is why not.
fn read_with<T>(
    answer: Answer,
    read: impl FnOnce(&[u8]) -> Result<T, ForgeStatus>,
) -> Result<T, Refusal> {
    match answer {
        Answer::Body { body, .. } => read(&body).map_err(Refusal::Status),
        Answer::Limited { until } => Err(Refusal::Limited { until }),
        // A 304 to a call that sent no ETag is not an answer.
        Answer::NotModified => Err(Refusal::Status(ForgeStatus::Failed)),
        Answer::Failed(status) => Err(Refusal::Status(status)),
    }
}

/// The search string of `kind` in `repo`.
fn search(repo: &ForgeRepo, kind: QueueKind) -> String {
    let base = format!("is:pr is:open repo:{}/{}", repo.owner, repo.name);
    match kind.qualifier() {
        Some(qualifier) => format!("{base} {qualifier}"),
        None => base,
    }
}

/// The queues query: fixed text, aliases `q0…q3` in [`QueueKind::ALL`]
/// order, each search string a variable.
fn queues_query() -> String {
    let params: Vec<String> = (0..4).map(|i| format!("$q{i}:String!")).collect();
    let fields: String = (0..4)
        .map(|i| {
            format!(
                " q{i}:search(query:$q{i},type:ISSUE,first:{QUEUE_SIZE})\
                 {{nodes{{...on PullRequest{{number title isDraft updatedAt author{{login}}}}}}}}"
            )
        })
        .collect();
    format!("query({}){{{fields} }}", params.join(","))
}

fn queues_body(repo: &ForgeRepo) -> Vec<u8> {
    let mut variables = serde_json::Map::new();
    for (i, kind) in QueueKind::ALL.into_iter().enumerate() {
        variables.insert(format!("q{i}"), json!(search(repo, kind)));
    }
    serde_json::to_vec(&json!({ "query": queues_query(), "variables": variables }))
        .unwrap_or_default()
}

/// The detail query: fixed text; owner, name and number are variables.
const DETAIL_QUERY: &str = "query($owner:String!,$name:String!,$number:Int!)\
{repository(owner:$owner,name:$name){pullRequest(number:$number)\
{number title body state isDraft mergeable baseRefName headRefName author{login} \
labels(first:20){nodes{name}} \
reviewRequests(first:20){nodes{requestedReviewer{...on User{login} ...on Bot{login} \
...on Mannequin{login} ...on Team{slug}}}} \
latestReviews(first:20){nodes{state author{login}}} \
commits(last:1){nodes{commit{statusCheckRollup{contexts(first:100){nodes{__typename \
...on CheckRun{name status conclusion detailsUrl} \
...on StatusContext{context state targetUrl}}}}}}}}}}";

fn detail_body(repo: &ForgeRepo, number: u64) -> Vec<u8> {
    let variables = json!({ "owner": repo.owner, "name": repo.name, "number": number });
    serde_json::to_vec(&json!({ "query": DETAIL_QUERY, "variables": variables }))
        .unwrap_or_default()
}

#[derive(Deserialize)]
struct Response<T> {
    data: Option<T>,
    errors: Option<Vec<serde_json::Value>>,
}

/// The data of a GraphQL answer with no errors.
fn data_of<T: for<'de> Deserialize<'de>>(body: &[u8]) -> Result<T, ForgeStatus> {
    let response: Response<T> = serde_json::from_slice(body).map_err(|_| ForgeStatus::Failed)?;
    if response.errors.is_some_and(|e| !e.is_empty()) {
        return Err(ForgeStatus::Failed);
    }
    response.data.ok_or(ForgeStatus::Failed)
}

#[derive(Deserialize)]
struct QueuesData {
    q0: Option<Nodes<SearchNode>>,
    q1: Option<Nodes<SearchNode>>,
    q2: Option<Nodes<SearchNode>>,
    q3: Option<Nodes<SearchNode>>,
}

#[derive(Deserialize)]
struct Nodes<T> {
    #[serde(default = "Vec::new")]
    nodes: Vec<Option<T>>,
}

impl<T> Nodes<T> {
    fn items(self) -> impl Iterator<Item = T> {
        self.nodes.into_iter().flatten()
    }
}

/// A search hit; one that is not a pull request has no fields.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchNode {
    number: Option<u64>,
    title: Option<String>,
    #[serde(default)]
    is_draft: bool,
    updated_at: Option<String>,
    author: Option<Login>,
}

#[derive(Deserialize)]
struct Login {
    login: Option<String>,
}

fn read_queues(body: &[u8]) -> Result<Queues, ForgeStatus> {
    let data: QueuesData = data_of(body)?;
    let list = |nodes: Option<Nodes<SearchNode>>| -> Vec<PrSummary> {
        nodes
            .into_iter()
            .flat_map(Nodes::items)
            .filter_map(|node| {
                Some(PrSummary {
                    number: node.number?,
                    title: line(node.title.as_deref().unwrap_or_default()),
                    author: author_of(node.author),
                    draft: node.is_draft,
                    updated: line(node.updated_at.as_deref().unwrap_or_default()),
                })
            })
            .take(QUEUE_SIZE)
            .collect()
    };
    Ok(Queues([
        list(data.q0),
        list(data.q1),
        list(data.q2),
        list(data.q3),
    ]))
}

#[derive(Deserialize)]
struct DetailData {
    repository: Option<DetailRepo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DetailRepo {
    pull_request: Option<PrNode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrNode {
    number: u64,
    title: Option<String>,
    body: Option<String>,
    state: Option<String>,
    #[serde(default)]
    is_draft: bool,
    mergeable: Option<String>,
    base_ref_name: Option<String>,
    head_ref_name: Option<String>,
    author: Option<Login>,
    labels: Option<Nodes<Named>>,
    review_requests: Option<Nodes<ReviewRequest>>,
    latest_reviews: Option<Nodes<ReviewNode>>,
    commits: Option<Nodes<CommitNode>>,
}

#[derive(Deserialize)]
struct Named {
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewRequest {
    requested_reviewer: Option<Reviewer>,
}

/// A user, bot or mannequin has a login; a team has a slug.
#[derive(Deserialize)]
struct Reviewer {
    login: Option<String>,
    slug: Option<String>,
}

#[derive(Deserialize)]
struct ReviewNode {
    state: Option<String>,
    author: Option<Login>,
}

#[derive(Deserialize)]
struct CommitNode {
    commit: Option<CommitRollup>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommitRollup {
    status_check_rollup: Option<Rollup>,
}

#[derive(Deserialize)]
struct Rollup {
    contexts: Option<Nodes<CheckNode>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CheckNode {
    #[serde(rename = "__typename")]
    typename: Option<String>,
    // CheckRun
    name: Option<String>,
    status: Option<String>,
    conclusion: Option<String>,
    details_url: Option<String>,
    // StatusContext
    context: Option<String>,
    state: Option<String>,
    target_url: Option<String>,
}

/// Whether `body` is an answer saying the repository has no such pull
/// request: `repository` present, `pullRequest` null.
fn pull_request_null(body: &[u8]) -> bool {
    #[derive(Deserialize)]
    struct Lenient {
        data: Option<DetailData>,
    }
    serde_json::from_slice::<Lenient>(body).is_ok_and(|r| {
        matches!(
            r.data,
            Some(DetailData {
                repository: Some(DetailRepo { pull_request: None })
            })
        )
    })
}

fn read_detail(body: &[u8]) -> Result<Option<PrDetail>, ForgeStatus> {
    let data: DetailData = data_of(body)?;
    let repository = data.repository.ok_or(ForgeStatus::NoAccess)?;
    let Some(pr) = repository.pull_request else {
        return Ok(None);
    };
    let state = match pr.state.as_deref() {
        Some("MERGED") => PrState::Merged,
        Some("CLOSED") => PrState::Closed,
        _ => PrState::Open,
    };
    let mergeable = match pr.mergeable.as_deref() {
        Some("MERGEABLE") => Mergeability::Mergeable,
        Some("CONFLICTING") => Mergeability::Conflicting,
        _ => Mergeability::Unknown,
    };
    let labels = pr
        .labels
        .into_iter()
        .flat_map(Nodes::items)
        .filter_map(|l| l.name.map(|n| line(&n)))
        .take(MAX_LABELS)
        .collect();
    let requested_reviewers = pr
        .review_requests
        .into_iter()
        .flat_map(Nodes::items)
        .filter_map(|r| {
            let reviewer = r.requested_reviewer?;
            reviewer.login.or(reviewer.slug).map(|n| line(&n))
        })
        .take(MAX_REVIEWERS)
        .collect();
    let reviews = pr
        .latest_reviews
        .into_iter()
        .flat_map(Nodes::items)
        .filter_map(|r| {
            let verdict = match r.state.as_deref()? {
                "APPROVED" => ReviewVerdict::Approved,
                "CHANGES_REQUESTED" => ReviewVerdict::ChangesRequested,
                "COMMENTED" => ReviewVerdict::Commented,
                "DISMISSED" => ReviewVerdict::Dismissed,
                "PENDING" => ReviewVerdict::Pending,
                _ => return None,
            };
            Some((author_of(r.author), verdict))
        })
        .take(MAX_REVIEWERS)
        .collect();
    let checks = pr
        .commits
        .into_iter()
        .flat_map(Nodes::items)
        .filter_map(|c| c.commit?.status_check_rollup?.contexts)
        .flat_map(Nodes::items)
        .filter_map(check_of)
        .take(MAX_CHECKS)
        .collect();
    Ok(Some(PrDetail {
        number: pr.number,
        title: line(pr.title.as_deref().unwrap_or_default()),
        author: author_of(pr.author),
        state,
        draft: pr.is_draft,
        body: body_text(pr.body.as_deref().unwrap_or_default()),
        labels,
        requested_reviewers,
        reviews,
        base: line(pr.base_ref_name.as_deref().unwrap_or_default()),
        head: line(pr.head_ref_name.as_deref().unwrap_or_default()),
        mergeable,
        checks,
    }))
}

/// A check run or a commit status as one check; anything else is none.
fn check_of(node: CheckNode) -> Option<Check> {
    let (name, status, url) = match node.typename.as_deref()? {
        "CheckRun" => {
            let status = match (node.status.as_deref(), node.conclusion.as_deref()) {
                (Some("COMPLETED"), Some("SUCCESS")) => CheckStatus::Success,
                (Some("COMPLETED"), Some("NEUTRAL" | "STALE")) => CheckStatus::Neutral,
                (Some("COMPLETED"), Some("SKIPPED")) => CheckStatus::Skipped,
                (Some("COMPLETED"), _) => CheckStatus::Failure,
                _ => CheckStatus::Pending,
            };
            (node.name, status, node.details_url)
        }
        "StatusContext" => {
            let status = match node.state.as_deref() {
                Some("SUCCESS") => CheckStatus::Success,
                Some("FAILURE" | "ERROR") => CheckStatus::Failure,
                _ => CheckStatus::Pending,
            };
            (node.context, status, node.target_url)
        }
        _ => return None,
    };
    Some(Check {
        name: line(name.as_deref().unwrap_or_default()),
        status,
        details_url: url.and_then(|u| safe_url(&u)),
    })
}

/// `url` when a browser may be handed it: `https://`, at most
/// [`MAX_URL`] long, with no control or space character.
pub fn safe_url(url: &str) -> Option<String> {
    let ok = url.len() <= MAX_URL
        && url.len() > "https://".len()
        && url
            .get(..8)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
        && !url.chars().any(|c| {
            c.is_control() || c.is_whitespace() || crate::git::diff::is_invisible_or_bidi(c)
        });
    ok.then(|| url.to_string())
}

fn author_of(author: Option<Login>) -> String {
    // A deleted account has no author; GitHub shows it as ghost.
    let login = author.and_then(|a| a.login);
    line(login.as_deref().unwrap_or("ghost"))
}

/// A one-line field: terminal-safe, line breaks included, and cut short.
fn line(text: &str) -> String {
    let cut = match text.char_indices().nth(MAX_FIELD) {
        Some((at, _)) => &text[..at],
        None => text,
    };
    sanitize(cut.as_bytes())
}

/// The description, cut to [`MAX_BODY`] and made terminal-safe a line at
/// a time, so only `\n` separates its lines.
fn body_text(text: &str) -> String {
    let mut end = text.len().min(MAX_BODY);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end]
        .split('\n')
        .map(|l| {
            sanitize_body(
                l.strip_suffix('\r').unwrap_or(l).as_bytes(),
                EmojiZwj::Escape,
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The configured host and repository the remote is on: `known` when an
/// earlier load found it, else read from the remote as the badge load
/// does. `None` when it is on no configured host.
pub fn resolve(
    git: &dyn GitRunner,
    hosts: &[ForgeHost],
    known: Option<&ForgeRepo>,
) -> Option<(ForgeHost, ForgeRepo)> {
    let repo = match known {
        Some(repo) => repo.clone(),
        None => read_remote_url(git).and_then(|url| parse_remote(&url))?,
    };
    let host = matches(&repo, hosts)?.clone();
    Some((host, repo))
}

/// One review load's inputs, as the effect gave them.
pub struct ReviewLoad<'a> {
    pub hosts: &'a [ForgeHost],
    /// The repository an earlier load found; the remote is then not read.
    pub known: Option<&'a ForgeRepo>,
    /// Hosts rate limited until the given second; not asked before it.
    pub limited: &'a [(ForgeHost, i64)],
    pub now: i64,
}

/// Find the repository, then `fetch` from it unless its host is rate
/// limited. Off the UI loop. The repository found comes back whatever the
/// fetch came to; with none, nobody was asked.
pub fn run<T>(
    git: &dyn GitRunner,
    ctx: &ForgeContext,
    load: &ReviewLoad,
    fetch: impl FnOnce(&Gh, &ForgeRepo) -> Result<T, Refusal>,
) -> (Option<ForgeRepo>, Result<T, ReviewError>) {
    // Defence in depth: with no hosts nothing is read or run.
    if load.hosts.is_empty() {
        return (None, Err(ReviewError::NoForgeHost));
    }
    let Some((host, repo)) = resolve(git, load.hosts, load.known) else {
        return (None, Err(ReviewError::NoForgeHost));
    };
    if let Some(until) = limited_until(load.limited, &host, load.now) {
        return (Some(repo), Err(ReviewError::Limited { until }));
    }
    // As for badge loads: `gh` runs in the cache directory, never the
    // repository, so it reads no repository's git config.
    let cwd = ctx
        .cache_dir
        .clone()
        .filter(|dir| super::cache::ensure_dir(dir).is_ok())
        .unwrap_or_else(std::env::temp_dir);
    let gh = Gh {
        runner: ctx.gh.as_ref(),
        sink: &ctx.sink,
        host: &host,
        cwd,
        now: load.now,
    };
    let result = fetch(&gh, &repo).map_err(ReviewError::from);
    (Some(repo), result)
}

/// The host's limit, when it has not passed by `now`.
pub fn limited_until(limited: &[(ForgeHost, i64)], host: &ForgeHost, now: i64) -> Option<i64> {
    limited
        .iter()
        .find(|(h, _)| h == host)
        .map(|(_, until)| *until)
        .filter(|until| *until > now)
}

#[cfg(test)]
pub mod fixtures {
    pub const QUEUES: &str = include_str!("fixtures/review_queues.http");
    pub const DETAIL: &str = include_str!("fixtures/review_detail.http");
    pub const DETAIL_MISSING: &str = include_str!("fixtures/review_detail_missing.http");
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::{Arc, Mutex};

    use super::fixtures::*;
    use super::*;
    use crate::forge::github::fixtures::{GRAPHQL_RATE_LIMITED, HTTP_401_STDERR};
    use crate::forge::github::{FakeGh, GhCommand, answer, failure};
    use crate::git::recording::{CommandEvent, CommandSink};

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

    fn gh<'a>(runner: &'a FakeGh, sink: &'a CommandSink, host: &'a ForgeHost) -> Gh<'a> {
        Gh {
            runner,
            sink,
            host,
            cwd: "/cache".into(),
            now: NOW,
        }
    }

    fn queues(runner: &FakeGh) -> Result<Queues, Refusal> {
        gh(runner, &CommandSink::new(), &host()).fetch_queues(&repo())
    }

    fn detail(runner: &FakeGh, number: u64) -> Result<Option<PrDetail>, Refusal> {
        gh(runner, &CommandSink::new(), &host()).fetch_detail(&repo(), number)
    }

    fn sent(cmd: &GhCommand) -> serde_json::Value {
        serde_json::from_slice(&cmd.stdin()).unwrap()
    }

    fn numbers(list: &[PrSummary]) -> Vec<u64> {
        list.iter().map(|p| p.number).collect()
    }

    #[test]
    fn the_queues_body_holds_the_four_searches_as_variables_only() {
        let runner = FakeGh::new().reply(answer(QUEUES));
        queues(&runner).unwrap();
        let [cmd] = runner.calls().try_into().unwrap();
        let body = sent(&cmd);
        let query = body["query"].as_str().unwrap();
        let vars = body["variables"].as_object().unwrap();
        let mut searches: Vec<&str> = vars.values().map(|v| v.as_str().unwrap()).collect();
        searches.sort_unstable();
        assert_eq!(
            searches,
            [
                "is:pr is:open repo:acme/widgets",
                "is:pr is:open repo:acme/widgets assignee:@me",
                "is:pr is:open repo:acme/widgets author:@me",
                "is:pr is:open repo:acme/widgets review-requested:@me",
            ]
        );
        for word in ["acme", "widgets", "@me", "is:pr"] {
            assert!(!query.contains(word), "{word} in {query}");
        }
        assert_eq!(
            cmd.display_line(),
            "gh api graphql --include --hostname github.com --input -"
        );
        let argv: Vec<String> = cmd
            .args()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(argv.iter().all(|a| !a.contains("acme")), "{argv:?}");
    }

    #[test]
    fn each_queue_holds_its_own_pull_requests_in_order() {
        let runner = FakeGh::new().reply(answer(QUEUES));
        let queues = queues(&runner).unwrap();
        assert_eq!(numbers(queues.get(QueueKind::Assigned)), [12, 7]);
        assert_eq!(numbers(queues.get(QueueKind::ReviewRequested)), [15, 12]);
        assert_eq!(numbers(queues.get(QueueKind::Authored)), Vec::<u64>::new());
        assert_eq!(numbers(queues.get(QueueKind::AllOpen)), [15, 12, 9, 7]);
        let draft = &queues.get(QueueKind::AllOpen)[0];
        assert_eq!(
            *draft,
            PrSummary {
                number: 15,
                title: "Draft: new gadget API".into(),
                author: "alice".into(),
                draft: true,
                updated: "2023-11-14T21:00:00Z".into(),
            }
        );
        assert!(!queues.get(QueueKind::Assigned)[0].draft);
    }

    #[test]
    fn a_repository_name_unfit_for_a_search_is_never_sent() {
        let runner = FakeGh::new();
        let odd = ForgeRepo {
            name: "w is:closed".into(),
            ..repo()
        };
        let result = gh(&runner, &CommandSink::new(), &host()).fetch_queues(&odd);
        assert_eq!(result, Err(Refusal::Status(ForgeStatus::Failed)));
        assert_eq!(runner.calls(), []);
    }

    #[test]
    fn the_detail_parses_with_its_checks_reviewers_and_branches() {
        let runner = FakeGh::new().reply(answer(DETAIL));
        let pr = detail(&runner, 42).unwrap().unwrap();
        let body = sent(&runner.calls()[0]);
        assert_eq!(body["variables"]["owner"], "acme");
        assert_eq!(body["variables"]["name"], "widgets");
        assert_eq!(body["variables"]["number"], 42);
        let query = body["query"].as_str().unwrap();
        assert!(!query.contains("acme") && !query.contains("42"), "{query}");

        assert_eq!(pr.number, 42);
        assert_eq!(pr.title, "Add widget caching");
        assert_eq!(pr.author, "octocat");
        assert_eq!((pr.state, pr.draft), (PrState::Open, false));
        assert_eq!(pr.labels, ["enhancement", "needs-review"]);
        assert_eq!(pr.requested_reviewers, ["hubot", "core"]);
        assert_eq!(
            pr.reviews,
            [
                ("alice".to_string(), ReviewVerdict::Approved),
                ("bob".to_string(), ReviewVerdict::ChangesRequested),
            ]
        );
        assert_eq!(
            (pr.base.as_str(), pr.head.as_str()),
            ("main", "feature/cache")
        );
        assert_eq!(pr.mergeable, Mergeability::Mergeable);
        assert_eq!(
            pr.checks,
            [
                Check {
                    name: "build".into(),
                    status: CheckStatus::Failure,
                    details_url: Some("https://ci.example.com/build/1".into()),
                },
                Check {
                    name: "ci/lint".into(),
                    status: CheckStatus::Success,
                    details_url: Some("https://ci.example.com/lint/1".into()),
                },
                Check {
                    name: "test".into(),
                    status: CheckStatus::Pending,
                    details_url: Some("https://ci.example.com/test/1".into()),
                },
            ]
        );
        assert!(pr.body.starts_with("## Summary\n\n"), "{:?}", pr.body);
        assert!(!pr.body.contains('\r'));
    }

    #[test]
    fn a_missing_pull_request_is_none_not_a_failure() {
        let runner = FakeGh::new().reply(answer(DETAIL_MISSING));
        assert_eq!(detail(&runner, 999), Ok(None));
        // A number GraphQL cannot hold asks nobody.
        let runner = FakeGh::new();
        assert_eq!(detail(&runner, u64::from(u32::MAX)), Ok(None));
        assert_eq!(runner.calls(), []);
        // A missing repository is still no access.
        let gone = DETAIL_MISSING.replace("{\"pullRequest\":null}", "null");
        let runner = FakeGh::new().reply(answer(&gone));
        assert_eq!(
            detail(&runner, 999),
            Err(Refusal::Status(ForgeStatus::NoAccess))
        );
    }

    #[test]
    fn failures_and_limits_say_why() {
        let runner = FakeGh::new().reply(answer(GRAPHQL_RATE_LIMITED));
        assert_eq!(
            queues(&runner),
            Err(Refusal::Limited {
                until: 1_700_003_600
            })
        );
        let runner = FakeGh::new().reply(failure(1, HTTP_401_STDERR));
        assert_eq!(
            detail(&runner, 1),
            Err(Refusal::Status(ForgeStatus::NotLoggedIn))
        );
        let runner = FakeGh::new().missing();
        assert_eq!(queues(&runner), Err(Refusal::Status(ForgeStatus::NoGh)));
        let runner = FakeGh::new().reply(answer("{\"data\":null}"));
        assert_eq!(queues(&runner), Err(Refusal::Status(ForgeStatus::Failed)));
    }

    /// `queues` with pull request `number`'s title replaced everywhere.
    pub(crate) fn with_title(queues: Queues, number: u64, title: &str) -> Queues {
        let mut lists = queues.0;
        for list in &mut lists {
            for pr in list.iter_mut().filter(|pr| pr.number == number) {
                pr.title = title.to_string();
            }
        }
        Queues(lists)
    }

    const HOSTILE: &str = "x\u{1b}[31mred\u{1b}]52;c;eA==\u{7}\r\u{202e}evil";

    fn no_controls(text: &str) {
        assert!(
            !text
                .chars()
                .any(|c| c.is_control() || crate::git::diff::is_invisible_or_bidi(c)),
            "{text:?}"
        );
    }

    /// A detail answer whose every string is `HOSTILE`, and whose failing
    /// check's page is `url`.
    pub(crate) fn hostile_detail(url: &str) -> String {
        let hostile = |s: &str| serde_json::to_string(s).unwrap();
        let h = hostile(HOSTILE);
        DETAIL
            .replace("\"Add widget caching\"", &h)
            .replace("\"octocat\"", &h)
            .replace("\"enhancement\"", &h)
            .replace("\"build\"", &h)
            .replace("\"main\"", &h)
            .replace(
                "## Summary",
                serde_json::to_string(HOSTILE).unwrap().trim_matches('"'),
            )
            .replace("https://ci.example.com/build/1", url)
    }

    #[test]
    fn server_text_is_terminal_safe_and_bad_urls_are_dropped() {
        for url in [
            "javascript:alert(1)",
            "http://ci.example.com/build/1",
            "https://ci.example.com/a b",
            "https://ci.example.com/\\u001b]52",
            "https://",
        ] {
            let runner = FakeGh::new().reply(answer(&hostile_detail(url)));
            let pr = detail(&runner, 42).unwrap().unwrap();
            for text in [
                &pr.title,
                &pr.author,
                &pr.labels[0],
                &pr.checks[0].name,
                &pr.base,
            ] {
                no_controls(text);
                assert!(text.contains("red"), "{text:?}");
            }
            for body_line in pr.body.lines() {
                no_controls(body_line);
            }
            assert_eq!(pr.checks[0].details_url, None, "{url}");
        }
        let long = format!("https://ci.example.com/{}", "a".repeat(MAX_URL));
        assert_eq!(safe_url(&long), None);
        assert!(safe_url("HTTPS://ci.example.com/x").is_some());

        let hostile_queue = QUEUES.replace(
            "\"Cache widget lookups\"",
            &serde_json::to_string(HOSTILE).unwrap(),
        );
        let runner = FakeGh::new().reply(answer(&hostile_queue));
        let queues = queues(&runner).unwrap();
        no_controls(&queues.get(QueueKind::Assigned)[0].title);
    }

    #[test]
    fn caps_hold_on_a_huge_answer() {
        let big = DETAIL.replace("## Summary", &"x".repeat(MAX_BODY + 10));
        let runner = FakeGh::new().reply(answer(&big));
        let pr = detail(&runner, 42).unwrap().unwrap();
        assert!(pr.body.len() <= MAX_BODY);
        let long_title = DETAIL.replace("Add widget caching", &"t".repeat(2000));
        let runner = FakeGh::new().reply(answer(&long_title));
        let pr = detail(&runner, 42).unwrap().unwrap();
        assert_eq!(pr.title.chars().count(), MAX_FIELD);
    }

    #[test]
    fn each_fetch_logs_one_started_and_finished_pair_with_the_fixed_line() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = CommandSink::new();
        let keep = Arc::clone(&events);
        sink.connect(move |ev| keep.lock().unwrap().push(ev));
        let runner = FakeGh::new().reply(answer(QUEUES)).reply(answer(DETAIL));
        let host = host();
        let gh = gh(&runner, &sink, &host);
        gh.fetch_queues(&repo()).unwrap();
        gh.fetch_detail(&repo(), 42).unwrap();
        let events = events.lock().unwrap();
        let line = "gh api graphql --include --hostname github.com --input -";
        let [
            CommandEvent::Started {
                id: a, line: l1, ..
            },
            CommandEvent::Finished {
                id: b,
                outcome: CommandOutcome::Ok,
                ..
            },
            CommandEvent::Started {
                id: c, line: l2, ..
            },
            CommandEvent::Finished {
                id: d,
                outcome: CommandOutcome::Ok,
                ..
            },
        ] = events.as_slice()
        else {
            panic!("{events:?}");
        };
        assert_eq!((l1.as_str(), l2.as_str()), (line, line));
        assert_eq!((a, c), (b, d));
        assert_ne!(a, c);
    }

    #[test]
    fn queues_step_round_in_both_directions() {
        assert_eq!(QueueKind::Assigned.step(false), QueueKind::ReviewRequested);
        assert_eq!(QueueKind::AllOpen.step(false), QueueKind::Assigned);
        assert_eq!(QueueKind::Assigned.step(true), QueueKind::AllOpen);
    }

    fn origin(url: &[u8]) -> crate::git::fake::FakeRunner {
        let mut out = url.to_vec();
        out.push(0);
        crate::git::fake::FakeRunner::new()
            .expect(&["remote"], crate::git::fake::ok(b"origin\n"))
            .expect(
                &["config", "-z", "--get", "remote.origin.url"],
                crate::git::fake::ok(&out),
            )
    }

    fn context(gh: Arc<FakeGh>) -> ForgeContext {
        ForgeContext {
            gh,
            sink: CommandSink::new(),
            cache_dir: None,
        }
    }

    #[test]
    fn a_remote_on_no_configured_host_asks_nobody() {
        let gh = Arc::new(FakeGh::new());
        let hosts = [host()];
        let load = ReviewLoad {
            hosts: &hosts,
            known: None,
            limited: &[],
            now: NOW,
        };
        let git = origin(b"https://github.com.evil.io/acme/widgets");
        let (repo, result) = run(&git, &context(Arc::clone(&gh)), &load, |gh, repo| {
            gh.fetch_queues(repo)
        });
        assert_eq!((repo, result), (None, Err(ReviewError::NoForgeHost)));
        // With no hosts at all, not even the remote is read.
        let git = crate::git::fake::FakeRunner::new();
        let none = ReviewLoad { hosts: &[], ..load };
        let (_, result) = run(&git, &context(Arc::clone(&gh)), &none, |gh, repo| {
            gh.fetch_queues(repo)
        });
        assert_eq!(result.unwrap_err(), ReviewError::NoForgeHost);
        git.assert_done();
        assert_eq!(gh.calls(), []);
    }

    #[test]
    fn the_remote_is_read_once_unknown_and_a_limited_host_gets_no_call() {
        let gh = Arc::new(FakeGh::new().reply(answer(DETAIL)));
        let hosts = [host()];
        let load = ReviewLoad {
            hosts: &hosts,
            known: None,
            limited: &[],
            now: NOW,
        };
        let git = origin(b"git@github.com:acme/widgets.git");
        let (repo, result) = run(&git, &context(Arc::clone(&gh)), &load, |gh, repo| {
            gh.fetch_detail(repo, 42)
        });
        git.assert_done();
        assert_eq!(repo, Some(self::repo()));
        assert_eq!(result.unwrap().unwrap().number, 42);
        assert_eq!(gh.calls()[0].host, "github.com");

        let limited = [(host(), NOW + 30)];
        let known = self::repo();
        let load = ReviewLoad {
            known: Some(&known),
            limited: &limited,
            ..load
        };
        let git = crate::git::fake::FakeRunner::new();
        let (_, result) = run(&git, &context(Arc::clone(&gh)), &load, |gh, repo| {
            gh.fetch_detail(repo, 42)
        });
        assert_eq!(result, Err(ReviewError::Limited { until: NOW + 30 }));
        assert_eq!(gh.calls().len(), 1);
    }

    #[test]
    fn a_limit_counts_only_before_its_second() {
        let limited = [(host(), NOW + 10)];
        assert_eq!(limited_until(&limited, &host(), NOW), Some(NOW + 10));
        assert_eq!(limited_until(&limited, &host(), NOW + 10), None);
        let other = ForgeHost::parse("github.example.com").unwrap();
        assert_eq!(limited_until(&limited, &other, NOW), None);
    }
}
