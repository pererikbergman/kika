//! Pull requests to review on GitHub: four queues in one GraphQL call,
//! and one pull request's detail in another, then one more per later
//! page of its files' viewed marks, up to [`MAX_FILES_PAGES`] in all.
//! Every string GitHub sends is made terminal-safe while it is parsed, so
//! no type here ever holds raw server text. The repository and the number travel as GraphQL
//! variables on stdin; argv is the fixed `gh api graphql` line.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::json;

use super::cli::CliStatus;
use super::client::Shut;
use super::forge::Forge;
use super::github::{Answer, Gh, GhCall, GhOutput, MAX_URL, Refusal, classify, split_include};
use super::remote::{allowed, matches, parse_remote, segment_ok};
use super::task::ForgeContext;
use super::{ForgeHost, ForgeRepo, ForgeStatus, PrState};
use kika_git::GitRunner;
use kika_git::commit_files::CommitId;
use kika_git::diff::{EmojiZwj, sanitize_body};
use kika_git::forge_remote::{ForgeRemote, read_forge_remote};
use kika_git::recording::CommandOutcome;
use kika_git::remotes::RemoteName;
use kika_git::safe_text::sanitize;

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
/// The files one page of a pull request's files asks for; the queries
/// write it as a literal `first:100`.
pub const FILES_PAGE: usize = 100;
/// The most pages of files read for one detail, its own page included,
/// so a load makes at most this many calls.
pub const MAX_FILES_PAGES: usize = 10;
/// The files whose viewed state a detail keeps; past it, unknown.
pub const MAX_FILES: usize = FILES_PAGE * MAX_FILES_PAGES;
/// The longest file path kept from GitHub, in bytes. Longer ones are
/// dropped: no file Git can check out has one.
pub const MAX_PATH: usize = 4096;

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
    /// The queues from their lists, in [`QueueKind::ALL`] order.
    pub(crate) fn from_lists(lists: [Vec<PrSummary>; 4]) -> Self {
        Self(lists)
    }

    pub fn get(&self, kind: QueueKind) -> &[PrSummary] {
        &self.0[kind.index()]
    }

    /// Take pull request `number` out of every queue, as once it is
    /// merged it is open no more.
    pub fn remove(&mut self, number: u64) {
        for list in &mut self.0 {
            list.retain(|pr| pr.number != number);
        }
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

/// What GitHub says stands between the pull request and a merge, from
/// its `mergeStateStatus`. Any value Kika does not know is `Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeState {
    Clean,
    /// Clean, with a commit status or check that is not required.
    HasHooks,
    /// Mergeable, but a check that is not required has not passed.
    Unstable,
    /// The base moved on; the branch must be updated first.
    Behind,
    /// A required review or check is missing.
    Blocked,
    /// The head conflicts with the base.
    Dirty,
    Draft,
    /// GitHub is still working it out.
    Unknown,
}

/// A way the repository lets a pull request be merged, in the order
/// GitHub offers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

impl MergeMethod {
    /// The REST API's `merge_method` value.
    pub fn api_name(self) -> &'static str {
        match self {
            MergeMethod::Merge => "merge",
            MergeMethod::Squash => "squash",
            MergeMethod::Rebase => "rebase",
        }
    }

    /// The words shown for it.
    pub fn label(self) -> &'static str {
        match self {
            MergeMethod::Merge => "merge commit",
            MergeMethod::Squash => "squash",
            MergeMethod::Rebase => "rebase",
        }
    }
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

/// Whether the reviewer marked a file viewed on GitHub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewedState {
    Viewed,
    Unviewed,
    /// Marked viewed, then changed since.
    Dismissed,
}

/// One pull request as review mode shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrDetail {
    pub number: u64,
    /// GitHub's id for the pull request, which marking a file viewed
    /// needs. Kept only when [`node_id_ok`]; never drawn.
    pub node_id: Option<String>,
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
    pub merge_state: MergeState,
    /// The ways the repository allows a merge, in [`MergeMethod`] order.
    pub merge_methods: Vec<MergeMethod>,
    pub checks: Vec<Check>,
    /// The head and base commits; `None` when GitHub sent no hex id.
    pub head_oid: Option<CommitId>,
    pub base_oid: Option<CommitId>,
    /// The viewed state of up to [`MAX_FILES`] files, by path, read a
    /// page at a time. The paths are only ever compared with local ones,
    /// never drawn.
    pub viewed: Vec<(String, ViewedState)>,
    /// Why `viewed` stops short of every file; `None` when every page
    /// was read.
    pub viewed_gap: Option<ViewedGap>,
    /// How many files GitHub says the pull request changes.
    pub files_total: u64,
}

impl PrDetail {
    /// Until when the host is rate limited, when a later page of files
    /// met a limit. The detail itself still answered.
    pub fn viewed_limit(&self) -> Option<i64> {
        match self.viewed_gap {
            Some(ViewedGap::Limited { until }) => Some(until),
            _ => None,
        }
    }
}

/// Why the viewed marks stop before the pull request's last file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewedGap {
    /// GitHub had more files after [`MAX_FILES_PAGES`] pages.
    Capped,
    /// A later page failed, or gave a cursor that was bad or already sent,
    /// or held nothing yet claimed more.
    Failed,
    /// A later page was rate limited until this second; no more were asked.
    Limited { until: i64 },
}

/// Why review mode has nothing to show. A fixed set: nothing `gh` or
/// GitHub says is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewError {
    /// The remote is on no `[forge] github` host; nobody was asked.
    NoForgeHost,
    /// The remote has several URLs, so it names no one repository;
    /// nobody was asked.
    SeveralUrls(RemoteName),
    /// GitHub refuses calls until this second.
    Limited {
        until: i64,
    },
    Status(ForgeStatus),
    /// The forge kind cannot review yet; nobody was asked.
    Unsupported(crate::forge::Unsupported),
}

impl From<Refusal> for ReviewError {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::Limited { until } => ReviewError::Limited { until },
            Refusal::Status(status) => ReviewError::Status(status),
            Refusal::Unsupported(unsupported) => ReviewError::Unsupported(unsupported),
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

    /// Pull request `number` of `repo`; `None` when GitHub has none. Its
    /// viewed marks past the first page are read before it answers.
    pub fn fetch_detail(&self, repo: &ForgeRepo, number: u64) -> Result<Option<PrDetail>, Refusal> {
        // GraphQL numbers are 32-bit; a larger one names no pull request.
        if number > i32::MAX as u64 {
            return Ok(None);
        }
        let kind = GhCall::Graphql {
            body: detail_body(repo, number),
        };
        let now = self.now;
        let first = self.run_logged(
            kind,
            |output| pull_request_answer(output, now, read_detail),
            outcome,
        )?;
        let Some((mut detail, end)) = first else {
            return Ok(None);
        };
        detail.viewed_gap = self.page_viewed(repo, number, &mut detail.viewed, end);
        Ok(Some(detail))
    }
}

/// `read` a GraphQL answer about one pull request. GitHub says NOT_FOUND
/// beside `pullRequest: null`; that is an answer, `None`, not a lack of
/// access.
pub(crate) fn pull_request_answer<T>(
    output: std::io::Result<GhOutput>,
    now: i64,
    read: impl FnOnce(&[u8]) -> Result<Option<T>, ForgeStatus>,
) -> Result<Option<T>, Refusal> {
    let stdout = output
        .as_ref()
        .map(|o| o.stdout.clone())
        .unwrap_or_default();
    let (status, _, body) = split_include(&stdout);
    match classify(output, now) {
        Answer::Failed(ForgeStatus::NoAccess) if status == Some(200) && pull_request_null(body) => {
            Ok(None)
        }
        answer => read_with(answer, read),
    }
}

pub(crate) fn outcome<T>(result: &Result<T, Refusal>) -> CommandOutcome {
    match result {
        Ok(_) => CommandOutcome::Ok,
        Err(Refusal::Status(ForgeStatus::Cli(CliStatus::NotInstalled))) => {
            CommandOutcome::NotStarted
        }
        Err(_) => CommandOutcome::Failed(None),
    }
}

/// `read` the body of a 2xx answer; anything else is why not.
pub(crate) fn read_with<T>(
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
{repository(owner:$owner,name:$name){mergeCommitAllowed squashMergeAllowed rebaseMergeAllowed \
pullRequest(number:$number)\
{id number title body state isDraft mergeable mergeStateStatus baseRefName headRefName headRefOid baseRefOid \
author{login} files(first:100){totalCount pageInfo{hasNextPage endCursor} nodes{path viewerViewedState}} \
labels(first:20){nodes{name}} \
reviewRequests(first:20){nodes{requestedReviewer{...on User{login} ...on Bot{login} \
...on Mannequin{login} ...on Team{slug}}}} \
latestReviews(first:20){nodes{state author{login}}} \
commits(last:1){nodes{commit{statusCheckRollup{contexts(first:100){nodes{__typename \
...on CheckRun{databaseId name status conclusion detailsUrl} \
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
pub(crate) fn data_of<T: for<'de> Deserialize<'de>>(body: &[u8]) -> Result<T, ForgeStatus> {
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

/// A GraphQL connection's nodes; null ones are skipped.
#[derive(Deserialize)]
pub(crate) struct Nodes<T> {
    #[serde(default = "Vec::new")]
    nodes: Vec<Option<T>>,
}

impl<T> Nodes<T> {
    pub(crate) fn items(self) -> impl Iterator<Item = T> {
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
pub(crate) struct Login {
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
    merge_commit_allowed: Option<bool>,
    squash_merge_allowed: Option<bool>,
    rebase_merge_allowed: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrNode {
    id: Option<String>,
    number: u64,
    title: Option<String>,
    body: Option<String>,
    state: Option<String>,
    #[serde(default)]
    is_draft: bool,
    mergeable: Option<String>,
    merge_state_status: Option<String>,
    base_ref_name: Option<String>,
    head_ref_name: Option<String>,
    head_ref_oid: Option<String>,
    base_ref_oid: Option<String>,
    files: Option<FilesNode>,
    author: Option<Login>,
    labels: Option<Nodes<Named>>,
    review_requests: Option<Nodes<ReviewRequest>>,
    latest_reviews: Option<Nodes<ReviewNode>>,
    commits: Option<Nodes<CommitNode>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FilesNode {
    total_count: Option<u64>,
    page_info: Option<PageInfo>,
    #[serde(default = "Vec::new")]
    nodes: Vec<Option<FileNode>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    #[serde(default)]
    has_next_page: bool,
    end_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileNode {
    path: Option<String>,
    viewer_viewed_state: Option<String>,
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
    /// Higher for a newer run of the same check.
    database_id: Option<u64>,
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
                repository: Some(DetailRepo {
                    pull_request: None,
                    ..
                })
            })
        )
    })
}

/// The detail, and where its page of files left the connection.
fn read_detail(body: &[u8]) -> Result<Option<(PrDetail, PageEnd)>, ForgeStatus> {
    let data: DetailData = data_of(body)?;
    let repository = data.repository.ok_or(ForgeStatus::NoAccess)?;
    // A flag GitHub did not send allows nothing.
    let merge_methods = [
        (repository.merge_commit_allowed, MergeMethod::Merge),
        (repository.squash_merge_allowed, MergeMethod::Squash),
        (repository.rebase_merge_allowed, MergeMethod::Rebase),
    ]
    .into_iter()
    .filter_map(|(allowed, method)| (allowed == Some(true)).then_some(method))
    .collect();
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
    let merge_state = match pr.merge_state_status.as_deref() {
        Some("CLEAN") => MergeState::Clean,
        Some("HAS_HOOKS") => MergeState::HasHooks,
        Some("UNSTABLE") => MergeState::Unstable,
        Some("BEHIND") => MergeState::Behind,
        Some("BLOCKED") => MergeState::Blocked,
        Some("DIRTY") => MergeState::Dirty,
        Some("DRAFT") => MergeState::Draft,
        _ => MergeState::Unknown,
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
            let verdict = verdict_of(r.state.as_deref()?)?;
            Some((author_of(r.author), verdict))
        })
        .take(MAX_REVIEWERS)
        .collect();
    let checks = latest_runs(
        pr.commits
            .into_iter()
            .flat_map(Nodes::items)
            .filter_map(|c| c.commit?.status_check_rollup?.contexts)
            .flat_map(Nodes::items),
    )
    .into_iter()
    .take(MAX_CHECKS)
    .collect();
    let files_total = pr.files.as_ref().and_then(|f| f.total_count).unwrap_or(0);
    let mut viewed = Vec::new();
    let end = add_viewed(&mut viewed, pr.files);
    let oid = full_oid;
    let detail = PrDetail {
        node_id: pr.id.filter(|id| node_id_ok(id)),
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
        merge_state,
        merge_methods,
        checks,
        head_oid: oid(pr.head_ref_oid),
        base_oid: oid(pr.base_ref_oid),
        viewed,
        viewed_gap: None,
        files_total,
    };
    Ok(Some((detail, end)))
}

// Paging through a pull request's files for their viewed marks. Each
// later page is one more `gh api graphql` call on the same `Gh`, so it
// stays behind the allow gate and rate-limit check the load passed.

/// One later page of a pull request's files: fixed text; owner, name,
/// number and the cursor to read after are variables.
const FILES_QUERY: &str = "query($owner:String!,$name:String!,$number:Int!,$after:String!)\
{repository(owner:$owner,name:$name){pullRequest(number:$number)\
{files(first:100,after:$after){pageInfo{hasNextPage endCursor} nodes{path viewerViewedState}}}}}";

fn files_body(repo: &ForgeRepo, number: u64, after: &str) -> Vec<u8> {
    let variables = json!({
        "owner": repo.owner,
        "name": repo.name,
        "number": number,
        "after": after,
    });
    serde_json::to_vec(&json!({ "query": FILES_QUERY, "variables": variables })).unwrap_or_default()
}

/// Whether `cursor` is fit to send back as a page cursor: 1 to 256
/// visible ASCII characters. It only ever travels as a JSON variable;
/// the check keeps it bounded.
pub fn cursor_ok(cursor: &str) -> bool {
    (1..=256).contains(&cursor.len()) && cursor.bytes().all(|b| b.is_ascii_graphic())
}

/// Where one page of files left the connection.
#[derive(Debug, Default)]
struct PageEnd {
    /// GitHub says more files follow.
    more: bool,
    cursor: Option<String>,
    /// The page held at least one file.
    held: bool,
}

/// `(path, state)` of one file: a path over [`MAX_PATH`] is none, and a
/// state Kika does not know counts as not viewed.
fn viewed_of(file: FileNode) -> Option<(String, ViewedState)> {
    let path = file.path.filter(|p| p.len() <= MAX_PATH)?;
    let viewed = match file.viewer_viewed_state.as_deref() {
        Some("VIEWED") => ViewedState::Viewed,
        Some("DISMISSED") => ViewedState::Dismissed,
        _ => ViewedState::Unviewed,
    };
    Some((path, viewed))
}

/// Add a page's files to `viewed`, up to [`MAX_FILES`]; a path already
/// there keeps its first state. Where the page ends comes back.
fn add_viewed(viewed: &mut Vec<(String, ViewedState)>, files: Option<FilesNode>) -> PageEnd {
    let Some(files) = files else {
        return PageEnd::default();
    };
    let info = files.page_info;
    let mut end = PageEnd {
        more: info.as_ref().is_some_and(|i| i.has_next_page),
        cursor: info.and_then(|i| i.end_cursor),
        held: false,
    };
    let mut seen: std::collections::HashSet<String> =
        viewed.iter().map(|(p, _)| p.clone()).collect();
    for file in files.nodes.into_iter().flatten() {
        end.held = true;
        if viewed.len() >= MAX_FILES {
            continue;
        }
        if let Some((path, state)) = viewed_of(file)
            && seen.insert(path.clone())
        {
            viewed.push((path, state));
        }
    }
    end
}

/// The cursor of the next page to ask for, `Ok(None)` when no more are
/// wanted, or the gap that stops the paging. `pages` have been read,
/// `full` says [`MAX_FILES`] are kept, and `sent` are the cursors
/// already asked with.
fn next_cursor(
    end: PageEnd,
    pages: usize,
    full: bool,
    sent: &[String],
) -> Result<Option<String>, ViewedGap> {
    if !end.more {
        return Ok(None);
    }
    if pages >= MAX_FILES_PAGES || full {
        return Err(ViewedGap::Capped);
    }
    match end.cursor {
        Some(cursor) if end.held && cursor_ok(&cursor) && !sent.contains(&cursor) => {
            Ok(Some(cursor))
        }
        _ => Err(ViewedGap::Failed),
    }
}

/// The files of a later page, read like the detail's own.
#[derive(Deserialize)]
struct FilesData {
    repository: Option<FilesRepo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FilesRepo {
    pull_request: Option<FilesPr>,
}

#[derive(Deserialize)]
struct FilesPr {
    files: Option<FilesNode>,
}

/// A later page's files; a missing repository, pull request or files
/// connection is a failure, as the detail just named them.
fn read_files(body: &[u8]) -> Result<FilesNode, ForgeStatus> {
    let data: FilesData = data_of(body)?;
    data.repository
        .and_then(|r| r.pull_request)
        .and_then(|p| p.files)
        .ok_or(ForgeStatus::Failed)
}

impl Gh<'_> {
    /// Read the pages after the detail's own into `viewed`, one call each,
    /// until GitHub has no more or [`MAX_FILES_PAGES`] pages are read.
    /// A page that fails ends it and keeps what was read; why it ended
    /// short comes back.
    fn page_viewed(
        &self,
        repo: &ForgeRepo,
        number: u64,
        viewed: &mut Vec<(String, ViewedState)>,
        mut end: PageEnd,
    ) -> Option<ViewedGap> {
        let mut sent: Vec<String> = Vec::new();
        loop {
            let cursor = match next_cursor(end, 1 + sent.len(), viewed.len() >= MAX_FILES, &sent) {
                Ok(Some(cursor)) => cursor,
                Ok(None) => return None,
                Err(gap) => return Some(gap),
            };
            let kind = GhCall::Graphql {
                body: files_body(repo, number, &cursor),
            };
            sent.push(cursor);
            match self.call(kind, |answer| read_with(answer, read_files), outcome) {
                Ok(files) => end = add_viewed(viewed, Some(files)),
                Err(Refusal::Limited { until }) => return Some(ViewedGap::Limited { until }),
                Err(Refusal::Status(_) | Refusal::Unsupported(_)) => {
                    return Some(ViewedGap::Failed);
                }
            }
        }
    }
}

/// A review's state as a verdict; `None` for one Kika does not know.
pub(crate) fn verdict_of(state: &str) -> Option<ReviewVerdict> {
    Some(match state {
        "APPROVED" => ReviewVerdict::Approved,
        "CHANGES_REQUESTED" => ReviewVerdict::ChangesRequested,
        "COMMENTED" => ReviewVerdict::Commented,
        "DISMISSED" => ReviewVerdict::Dismissed,
        "PENDING" => ReviewVerdict::Pending,
        _ => return None,
    })
}

/// A full SHA-1 or SHA-256 id; nothing shorter can name one commit.
pub(crate) fn full_oid(text: Option<String>) -> Option<CommitId> {
    text.filter(|t| matches!(t.len(), 40 | 64))
        .as_deref()
        .and_then(CommitId::parse)
}

/// Whether `id` is fit to send back as a GraphQL node id: 1 to 128 ASCII
/// letters, digits, `_`, `=` or `-`. Anything else is not one GitHub makes.
pub fn node_id_ok(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'=' | b'-'))
}

/// The checks of `nodes`, one row per check run name holding its latest
/// run: the highest database id, the later one on a tie or with no id.
/// The rollup lists every re-run of a job; GitHub already sends only the
/// latest of each commit status, so those pass through as they are.
fn latest_runs(nodes: impl Iterator<Item = CheckNode>) -> Vec<Check> {
    let mut rows: Vec<(Check, Option<u64>)> = Vec::new();
    let mut runs: HashMap<String, usize> = HashMap::new();
    for node in nodes {
        let is_run = node.typename.as_deref() == Some("CheckRun");
        let id = node.database_id;
        let Some(check) = check_of(node) else {
            continue;
        };
        if !is_run {
            rows.push((check, id));
            continue;
        }
        match runs.get(&check.name) {
            // `None` sorts below any id, so a run with no id never wins
            // over one with an id.
            Some(&at) => {
                if id >= rows[at].1 {
                    rows[at] = (check, id);
                }
            }
            None => {
                runs.insert(check.name.clone(), rows.len());
                rows.push((check, id));
            }
        }
    }
    rows.into_iter().map(|(check, _)| check).collect()
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
            c.is_control() || c.is_whitespace() || kika_git::safe_text::is_invisible_or_bidi(c)
        });
    ok.then(|| url.to_string())
}

pub(crate) fn author_of(author: Option<Login>) -> String {
    // A deleted account has no author; GitHub shows it as ghost.
    let login = author.and_then(|a| a.login);
    line(login.as_deref().unwrap_or("ghost"))
}

/// A one-line field: terminal-safe, line breaks included, and cut short.
pub(crate) fn line(text: &str) -> String {
    let cut = match text.char_indices().nth(MAX_FIELD) {
        Some((at, _)) => &text[..at],
        None => text,
    };
    sanitize(cut.as_bytes())
}

/// The description, cut to [`MAX_BODY`] and made terminal-safe a line at
/// a time, so only `\n` separates its lines.
fn body_text(text: &str) -> String {
    capped_body(text, MAX_BODY)
}

/// `text` cut to `max` bytes on a character boundary, then made
/// terminal-safe a line at a time, so only `\n` separates its lines.
pub(crate) fn capped_body(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
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
/// does. `SeveralUrls` when the remote has several URLs, and
/// `NoForgeHost` when it is on no configured host, or that host's
/// allow-list leaves it out.
pub fn resolve(
    git: &dyn GitRunner,
    hosts: &[ForgeHost],
    known: Option<&ForgeRepo>,
) -> Result<(ForgeHost, ForgeRepo), ReviewError> {
    let repo = match known {
        Some(repo) => repo.clone(),
        None => match read_forge_remote(git) {
            Some(ForgeRemote::One { url, .. }) => parse_remote(&url),
            Some(ForgeRemote::Several(name)) => return Err(ReviewError::SeveralUrls(name)),
            None => None,
        }
        .ok_or(ReviewError::NoForgeHost)?,
    };
    let host = allowed(&repo, hosts)
        .ok_or(ReviewError::NoForgeHost)?
        .clone();
    Ok((host, repo))
}

/// Why no remote may be fetched from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoFetchRemote {
    /// No remote names the repository on a configured host.
    NotNamed,
    /// The remote has several URLs: the one checked need not be the one
    /// git fetches from.
    SeveralUrls(RemoteName),
}

/// The remote a pull request's commits may be fetched from: the one
/// whose one URL names `repo` itself, on one of `hosts`. ASCII case is
/// ignored, as the forges ignore it. `NotNamed` when the remote names
/// another repository or host, or has a name git could read as an
/// option; `SeveralUrls` when it has several URLs, even if one of them
/// names `repo`. Then nothing may be fetched.
pub fn fetch_remote(
    git: &dyn GitRunner,
    hosts: &[ForgeHost],
    repo: &ForgeRepo,
) -> Result<RemoteName, NoFetchRemote> {
    allowed(repo, hosts).ok_or(NoFetchRemote::NotNamed)?;
    let (name, url) = match read_forge_remote(git) {
        Some(ForgeRemote::One { name, url }) => (name, url),
        Some(ForgeRemote::Several(name)) => return Err(NoFetchRemote::SeveralUrls(name)),
        None => return Err(NoFetchRemote::NotNamed),
    };
    let named = parse_remote(&url).ok_or(NoFetchRemote::NotNamed)?;
    let same = named.host.eq_ignore_ascii_case(&repo.host)
        && named.owner.eq_ignore_ascii_case(&repo.owner)
        && named.name.eq_ignore_ascii_case(&repo.name);
    if same && matches(&named, hosts).is_some() && !name.0.starts_with(b"-") {
        Ok(name)
    } else {
        Err(NoFetchRemote::NotNamed)
    }
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
    fetch: impl FnOnce(&dyn Forge, &ForgeRepo) -> Result<T, Refusal>,
) -> (Option<ForgeRepo>, Result<T, ReviewError>) {
    // Defence in depth: with no hosts nothing is read or run.
    if load.hosts.is_empty() {
        return (None, Err(ReviewError::NoForgeHost));
    }
    let (host, repo) = match resolve(git, load.hosts, load.known) {
        Ok(found) => found,
        Err(error) => return (None, Err(error)),
    };
    if let Some(until) = limited_until(load.limited, &host, load.now) {
        return (Some(repo), Err(ReviewError::Limited { until }));
    }
    // `resolve` applied the same allow gate; `Outside` is its answer.
    let forge = match ctx.forge(&host, &repo, load.now) {
        Ok(forge) => forge,
        Err(Shut::Outside) => return (None, Err(ReviewError::NoForgeHost)),
        Err(Shut::Cli(status)) => {
            return (
                Some(repo),
                Err(ReviewError::Status(ForgeStatus::Cli(status))),
            );
        }
    };
    let result = fetch(forge.as_ref(), &repo).map_err(ReviewError::from);
    (Some(repo), result)
}

/// The host's limit, when it has not passed by `now`.
pub fn limited_until(limited: &[(ForgeHost, i64)], host: &ForgeHost, now: i64) -> Option<i64> {
    limited
        .iter()
        .find(|(h, _)| h.as_str() == host.as_str())
        .map(|(_, until)| *until)
        .filter(|until| *until > now)
}

#[cfg(any(test, feature = "test-support"))]
pub mod fixtures {
    use super::Queues;

    pub const QUEUES: &str = include_str!("fixtures/review_queues.http");
    pub const DETAIL: &str = include_str!("fixtures/review_detail.http");
    pub const DETAIL_MISSING: &str = include_str!("fixtures/review_detail_missing.http");
    pub const DETAIL_BAD_OID: &str = include_str!("fixtures/review_detail_bad_oid.http");
    /// A later page of files: two of them, and no more after.
    pub const LATER_PAGE: &str = include_str!("fixtures/review_files_page.http");
    /// A later page whose pull request is gone.
    pub const LATER_PAGE_NULL: &str = include_str!("fixtures/review_files_page_null.http");

    /// The path of file `i` in the built pages.
    pub fn page_path(i: usize) -> String {
        format!("f{i:04}")
    }

    /// GitHub's viewed state of file `i` in the built pages: viewed,
    /// dismissed and not viewed in turn.
    pub fn page_state(i: usize) -> &'static str {
        ["VIEWED", "DISMISSED", "UNVIEWED"][i % 3]
    }

    /// The `files` connection of `paths`, with `next` as the cursor of
    /// the page after it, or no page after it.
    fn files_json(
        paths: std::ops::Range<usize>,
        total: Option<usize>,
        next: Option<&str>,
    ) -> String {
        let nodes: Vec<String> = paths
            .map(|i| {
                format!(
                    "{{\"path\":\"{}\",\"viewerViewedState\":\"{}\"}}",
                    page_path(i),
                    page_state(i)
                )
            })
            .collect();
        let info = match next {
            Some(cursor) => format!(
                "{{\"hasNextPage\":true,\"endCursor\":{}}}",
                serde_json::to_string(cursor).unwrap()
            ),
            None => "{\"hasNextPage\":false,\"endCursor\":null}".to_string(),
        };
        let total = total
            .map(|t| format!("\"totalCount\":{t},"))
            .unwrap_or_default();
        format!(
            "\"files\":{{{total}\"pageInfo\":{info},\"nodes\":[{}]}}",
            nodes.join(",")
        )
    }

    /// A later page holding files `paths`, answered as [`LATER_PAGE`] is.
    pub fn files_page(paths: std::ops::Range<usize>, next: Option<&str>) -> String {
        let (head, _) = LATER_PAGE.split_once("\n\n").unwrap();
        format!(
            "{head}\n\n{{\"data\":{{\"repository\":{{\"pullRequest\":{{{}}}}}}}}}\n",
            files_json(paths, None, next)
        )
    }

    /// [`DETAIL`] with files `paths` of `total` in place of its own.
    pub fn detail_with_files(
        paths: std::ops::Range<usize>,
        total: usize,
        next: Option<&str>,
    ) -> String {
        let start = DETAIL.find("\"files\":").unwrap();
        let end = start + DETAIL[start..].find("]},").unwrap() + 2;
        format!(
            "{}{}{}",
            &DETAIL[..start],
            files_json(paths, Some(total), next),
            &DETAIL[end..]
        )
    }

    /// `queues` with pull request `number`'s title replaced everywhere.
    pub fn with_title(queues: Queues, number: u64, title: &str) -> Queues {
        let mut lists = queues.0;
        for list in &mut lists {
            for pr in list.iter_mut().filter(|pr| pr.number == number) {
                pr.title = title.to_string();
            }
        }
        Queues(lists)
    }

    pub const HOSTILE: &str = "x\u{1b}[31mred\u{1b}]52;c;eA==\u{7}\r\u{202e}evil";

    /// A detail answer whose every string is `HOSTILE`, and whose failing
    /// check's page is `url`.
    pub fn hostile_detail(url: &str) -> String {
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
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::{Arc, Mutex};

    use super::fixtures::*;
    use super::*;
    use crate::github::fixtures::{GRAPHQL_RATE_LIMITED, HTTP_401_STDERR};
    use crate::github::{FakeGh, GhCommand, answer, failure};
    use kika_git::recording::{CommandEvent, CommandSink};

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
        assert!(
            query.contains("...on CheckRun{databaseId name status conclusion detailsUrl}"),
            "{query}"
        );

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

    /// [`DETAIL`] with the check nodes `nodes` (a JSON list's items) in
    /// place of its own.
    fn detail_with_checks(nodes: &str) -> String {
        let open = "\"contexts\":{\"nodes\":[";
        let start = DETAIL.find(open).unwrap() + open.len();
        let end = start + DETAIL[start..].find("]}").unwrap();
        format!("{}{nodes}{}", &DETAIL[..start], &DETAIL[end..])
    }

    /// A check run node named `name`, with `id` as its database id when
    /// there is one, finished with `conclusion`. Its page names the id.
    fn check_run(name: &str, id: Option<u64>, conclusion: &str) -> String {
        let page = id.map_or("none".to_string(), |i| i.to_string());
        let id = id
            .map(|i| format!("\"databaseId\":{i},"))
            .unwrap_or_default();
        format!(
            "{{\"__typename\":\"CheckRun\",{id}\"name\":\"{name}\",\"status\":\"COMPLETED\",\
             \"conclusion\":\"{conclusion}\",\"detailsUrl\":\"https://ci.example.com/{name}/{page}\"}}"
        )
    }

    fn checks_of(nodes: &[String]) -> Vec<(String, CheckStatus, Option<String>)> {
        let runner = FakeGh::new().reply(answer(&detail_with_checks(&nodes.join(","))));
        let pr = detail(&runner, 42).unwrap().unwrap();
        pr.checks
            .into_iter()
            .map(|c| (c.name, c.status, c.details_url))
            .collect()
    }

    #[test]
    fn a_re_run_check_shows_once_with_its_latest_run() {
        let checks = checks_of(&[
            check_run("check", Some(10), "FAILURE"),
            check_run("check", Some(12), "SUCCESS"),
            check_run("check", Some(11), "SUCCESS"),
        ]);
        assert_eq!(
            checks,
            [(
                "check".into(),
                CheckStatus::Success,
                Some("https://ci.example.com/check/12".into())
            )]
        );
    }

    #[test]
    fn a_re_run_check_keeps_the_place_it_first_appears_in() {
        let checks = checks_of(&[
            check_run("a", Some(1), "FAILURE"),
            check_run("b", Some(2), "SUCCESS"),
            check_run("a", Some(3), "SUCCESS"),
        ]);
        assert_eq!(
            checks,
            [
                (
                    "a".into(),
                    CheckStatus::Success,
                    Some("https://ci.example.com/a/3".into())
                ),
                (
                    "b".into(),
                    CheckStatus::Success,
                    Some("https://ci.example.com/b/2".into())
                ),
            ]
        );
    }

    #[test]
    fn a_run_with_an_id_beats_one_without_and_without_ids_the_later_wins() {
        let later = checks_of(&[
            check_run("a", None, "FAILURE"),
            check_run("a", None, "SUCCESS"),
        ]);
        assert_eq!(
            later.iter().map(|c| c.1).collect::<Vec<_>>(),
            [CheckStatus::Success]
        );
        let with_id = checks_of(&[
            check_run("a", Some(1), "FAILURE"),
            check_run("a", None, "SUCCESS"),
        ]);
        assert_eq!(
            with_id.iter().map(|c| c.1).collect::<Vec<_>>(),
            [CheckStatus::Failure]
        );
        let tie = checks_of(&[
            check_run("a", Some(5), "FAILURE"),
            check_run("a", Some(5), "SUCCESS"),
        ]);
        assert_eq!(
            tie.iter().map(|c| c.1).collect::<Vec<_>>(),
            [CheckStatus::Success]
        );
    }

    #[test]
    fn a_status_and_a_check_run_of_the_same_name_both_stay() {
        let status = "{\"__typename\":\"StatusContext\",\"context\":\"a\",\"state\":\"FAILURE\",\
                      \"targetUrl\":\"https://ci.example.com/status/a\"}"
            .to_string();
        let checks = checks_of(&[status.clone(), check_run("a", Some(1), "SUCCESS"), status]);
        assert_eq!(
            checks.iter().map(|c| c.1).collect::<Vec<_>>(),
            [
                CheckStatus::Failure,
                CheckStatus::Success,
                CheckStatus::Failure
            ]
        );
    }

    #[test]
    fn the_detail_reads_its_commits_and_the_viewed_state_of_its_files() {
        let runner = FakeGh::new().reply(answer(DETAIL));
        let pr = detail(&runner, 42).unwrap().unwrap();
        let query = sent(&runner.calls()[0])["query"]
            .as_str()
            .unwrap()
            .to_string();
        let cap = format!("files(first:{FILES_PAGE})");
        for field in [
            "{id ",
            "headRefOid",
            "baseRefOid",
            "viewerViewedState",
            &cap,
        ] {
            assert!(query.contains(field), "{field} not asked: {query}");
        }
        assert_eq!(pr.head_oid, CommitId::parse(&"1".repeat(40)));
        assert_eq!(pr.base_oid, CommitId::parse(&"2".repeat(40)));
        assert_eq!(pr.files_total, 3);
        assert_eq!(
            pr.viewed,
            [
                ("src/cache.rs".to_string(), ViewedState::Viewed),
                ("src/lib.rs".to_string(), ViewedState::Dismissed),
                ("docs/café.md".to_string(), ViewedState::Unviewed),
            ]
        );
    }

    #[test]
    fn the_detail_reads_its_merge_state_and_the_allowed_methods() {
        let runner = FakeGh::new().reply(answer(DETAIL));
        let pr = detail(&runner, 42).unwrap().unwrap();
        let query = sent(&runner.calls()[0])["query"]
            .as_str()
            .unwrap()
            .to_string();
        for field in [
            "mergeStateStatus",
            "mergeCommitAllowed",
            "squashMergeAllowed",
            "rebaseMergeAllowed",
        ] {
            assert!(query.contains(field), "{field} not asked: {query}");
        }
        assert_eq!(pr.merge_state, MergeState::Clean);
        assert_eq!(pr.merge_methods, [MergeMethod::Merge, MergeMethod::Squash]);

        for (text, want) in [
            ("HAS_HOOKS", MergeState::HasHooks),
            ("UNSTABLE", MergeState::Unstable),
            ("BEHIND", MergeState::Behind),
            ("BLOCKED", MergeState::Blocked),
            ("DIRTY", MergeState::Dirty),
            ("DRAFT", MergeState::Draft),
            ("UNKNOWN", MergeState::Unknown),
            ("SOMETHING_NEW", MergeState::Unknown),
        ] {
            let other = DETAIL.replace("\"CLEAN\"", &format!("\"{text}\""));
            let runner = FakeGh::new().reply(answer(&other));
            assert_eq!(detail(&runner, 42).unwrap().unwrap().merge_state, want);
        }
        let all = DETAIL.replace(
            "\"rebaseMergeAllowed\":false",
            "\"rebaseMergeAllowed\":true",
        );
        let runner = FakeGh::new().reply(answer(&all));
        assert_eq!(
            detail(&runner, 42).unwrap().unwrap().merge_methods,
            [MergeMethod::Merge, MergeMethod::Squash, MergeMethod::Rebase]
        );
    }

    #[test]
    fn missing_merge_fields_mean_unknown_and_no_method() {
        let bare = DETAIL.replace("\"mergeStateStatus\":\"CLEAN\",", "").replace(
            "\"mergeCommitAllowed\":true,\"squashMergeAllowed\":true,\"rebaseMergeAllowed\":false,",
            "",
        );
        assert!(!bare.contains("Allowed") && !bare.contains("mergeState"));
        let runner = FakeGh::new().reply(answer(&bare));
        let pr = detail(&runner, 42).unwrap().unwrap();
        assert_eq!(pr.merge_state, MergeState::Unknown);
        assert_eq!(pr.merge_methods, []);
        // A null flag allows nothing either.
        let null = DETAIL.replace("\"mergeCommitAllowed\":true", "\"mergeCommitAllowed\":null");
        let runner = FakeGh::new().reply(answer(&null));
        assert_eq!(
            detail(&runner, 42).unwrap().unwrap().merge_methods,
            [MergeMethod::Squash]
        );
    }

    #[test]
    fn a_merged_pull_request_leaves_every_queue() {
        let runner = FakeGh::new().reply(answer(QUEUES));
        let mut queues = queues(&runner).unwrap();
        queues.remove(12);
        assert_eq!(numbers(queues.get(QueueKind::Assigned)), [7]);
        assert_eq!(numbers(queues.get(QueueKind::ReviewRequested)), [15]);
        assert_eq!(numbers(queues.get(QueueKind::AllOpen)), [15, 9, 7]);
    }

    #[test]
    fn the_detail_keeps_its_node_id_only_when_it_is_a_plain_token() {
        let runner = FakeGh::new().reply(answer(DETAIL));
        let pr = detail(&runner, 42).unwrap().unwrap();
        let query = sent(&runner.calls()[0])["query"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(query.contains("pullRequest(number:$number){id "), "{query}");
        assert_eq!(pr.node_id.as_deref(), Some("PR_kwDOA1"));

        let longest = format!("PR_{}", "a".repeat(125));
        let runner = FakeGh::new().reply(answer(&DETAIL.replace("PR_kwDOA1", &longest)));
        assert_eq!(detail(&runner, 42).unwrap().unwrap().node_id, Some(longest));
        let too_long = format!("PR_{}", "a".repeat(126));
        for bad in [
            "PR kwDOA1",
            too_long.as_str(),
            "",
            "PR_\\u001b[31m",
            "PR/../x",
        ] {
            let runner = FakeGh::new().reply(answer(&DETAIL.replace("PR_kwDOA1", bad)));
            let pr = detail(&runner, 42).unwrap().unwrap();
            assert_eq!(pr.node_id, None, "{bad:?}");
        }
        let gone = DETAIL.replace("\"id\":\"PR_kwDOA1\",", "");
        let runner = FakeGh::new().reply(answer(&gone));
        assert_eq!(detail(&runner, 42).unwrap().unwrap().node_id, None);
    }

    #[test]
    fn an_oid_that_is_not_hex_is_none() {
        let runner = FakeGh::new().reply(answer(DETAIL_BAD_OID));
        let pr = detail(&runner, 42).unwrap().unwrap();
        assert_eq!(pr.head_oid, None);
        assert_eq!(pr.base_oid, CommitId::parse(&"2".repeat(40)));
        // Hex, but too short to name one commit.
        let short = DETAIL.replace(&"1".repeat(40), "1111");
        let runner = FakeGh::new().reply(answer(&short));
        assert_eq!(detail(&runner, 42).unwrap().unwrap().head_oid, None);
    }

    #[test]
    fn the_viewed_list_keeps_at_most_its_cap_and_no_overlong_path() {
        let nodes: Vec<String> = (0..MAX_FILES + 20)
            .map(|i| format!("{{\"path\":\"f{i}\",\"viewerViewedState\":\"ODD\"}}"))
            .chain([format!(
                "{{\"path\":\"{}\",\"viewerViewedState\":\"VIEWED\"}}",
                "p".repeat(MAX_PATH + 1)
            )])
            .collect();
        let files = format!(
            "\"files\":{{\"totalCount\":500,\"nodes\":[{}]}}",
            nodes.join(",")
        );
        let start = DETAIL.find("\"files\":").unwrap();
        let end = start + DETAIL[start..].find("]},").unwrap() + 2;
        let big = format!("{}{files}{}", &DETAIL[..start], &DETAIL[end..]);
        let runner = FakeGh::new().reply(answer(&big));
        let pr = detail(&runner, 42).unwrap().unwrap();
        assert_eq!(pr.viewed.len(), MAX_FILES);
        assert_eq!(pr.files_total, 500);
        // An unknown state counts as not viewed.
        assert!(pr.viewed.iter().all(|(_, v)| *v == ViewedState::Unviewed));

        let long = DETAIL.replace("src/cache.rs", &"p".repeat(MAX_PATH + 1));
        let runner = FakeGh::new().reply(answer(&long));
        let pr = detail(&runner, 42).unwrap().unwrap();
        assert_eq!(pr.viewed.len(), 2);
        assert_eq!(pr.viewed[0].0, "src/lib.rs");
    }

    #[test]
    fn a_missing_pull_request_is_none_not_a_failure() {
        let runner = FakeGh::new().reply(answer(DETAIL_MISSING));
        assert_eq!(detail(&runner, 999), Ok(None));
        assert_eq!(runner.calls().len(), 1);
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
        assert_eq!(runner.calls().len(), 1);
        let runner = FakeGh::new().missing();
        assert_eq!(
            queues(&runner),
            Err(Refusal::Status(ForgeStatus::Cli(CliStatus::NotInstalled)))
        );
        let runner = FakeGh::new().reply(answer("{\"data\":null}"));
        assert_eq!(queues(&runner), Err(Refusal::Status(ForgeStatus::Failed)));
    }

    fn no_controls(text: &str) {
        assert!(
            !text
                .chars()
                .any(|c| c.is_control() || kika_git::safe_text::is_invisible_or_bidi(c)),
            "{text:?}"
        );
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

    /// `fetch_detail` of pull request 42 answered by `pages` in turn.
    fn paged(pages: &[String]) -> (FakeGh, Result<Option<PrDetail>, Refusal>) {
        let runner = pages
            .iter()
            .fold(FakeGh::new(), |runner, page| runner.reply(answer(page)));
        let result = detail(&runner, 42);
        (runner, result)
    }

    /// The marks the built pages give files `paths`.
    fn marks_of(paths: std::ops::Range<usize>) -> Vec<(String, ViewedState)> {
        paths
            .map(|i| {
                let state = match page_state(i) {
                    "VIEWED" => ViewedState::Viewed,
                    "DISMISSED" => ViewedState::Dismissed,
                    _ => ViewedState::Unviewed,
                };
                (page_path(i), state)
            })
            .collect()
    }

    /// The detail's 100 files, saying the next page starts after `c1`.
    fn first_page() -> String {
        detail_with_files(0..100, 250, Some("c1"))
    }

    #[test]
    fn both_queries_page_by_files_page_and_ask_where_the_page_ends() {
        let first = format!("first:{FILES_PAGE}");
        assert!(
            DETAIL_QUERY.contains(&format!(
                "files({first}){{totalCount pageInfo{{hasNextPage endCursor}} nodes{{path viewerViewedState}}}}"
            )),
            "{DETAIL_QUERY}"
        );
        assert!(
            FILES_QUERY.contains(&format!(
                "files({first},after:$after){{pageInfo{{hasNextPage endCursor}} nodes{{path viewerViewedState}}}}"
            )),
            "{FILES_QUERY}"
        );
        assert!(
            FILES_QUERY
                .starts_with("query($owner:String!,$name:String!,$number:Int!,$after:String!)")
        );
        assert_eq!(MAX_FILES, FILES_PAGE * MAX_FILES_PAGES);
        assert_eq!(MAX_FILES, 1000);
    }

    #[test]
    fn a_pull_request_past_one_page_has_every_file_marked_in_three_calls() {
        let (runner, result) = paged(&[
            first_page(),
            files_page(100..200, Some("c2")),
            files_page(200..250, None),
        ]);
        let pr = result.unwrap().unwrap();
        assert_eq!(pr.viewed.len(), 250);
        assert_eq!(pr.viewed, marks_of(0..250));
        assert_eq!(pr.viewed[120], (page_path(120), ViewedState::Viewed));
        assert_eq!(pr.viewed[202], (page_path(202), ViewedState::Dismissed));
        assert_eq!(pr.viewed[248], (page_path(248), ViewedState::Unviewed));
        assert_eq!(pr.viewed_gap, None);
        assert_eq!(pr.files_total, 250);
        assert_eq!(runner.calls().len(), 3);
    }

    #[test]
    fn each_later_page_sends_its_cursor_as_a_variable_on_stdin_only() {
        let (runner, result) = paged(&[
            first_page(),
            files_page(100..200, Some("Y3Vyc29yOjIwMA==")),
            files_page(200..250, None),
        ]);
        result.unwrap().unwrap();
        let calls = runner.calls();
        let fixed = "gh api graphql --include --hostname github.com --input -";
        for (cmd, after) in calls[1..].iter().zip(["c1", "Y3Vyc29yOjIwMA=="]) {
            let body = sent(cmd);
            assert_eq!(body["query"], FILES_QUERY);
            let vars = body["variables"].as_object().unwrap();
            let mut keys: Vec<&str> = vars.keys().map(String::as_str).collect();
            keys.sort_unstable();
            assert_eq!(keys, ["after", "name", "number", "owner"]);
            assert_eq!(vars["after"], after);
            assert_eq!(
                (&vars["owner"], &vars["name"], &vars["number"]),
                (&json!("acme"), &json!("widgets"), &json!(42))
            );
            assert_eq!(cmd.display_line(), fixed);
            assert_eq!(cmd.args(), calls[0].args());
            for arg in cmd.args() {
                let arg = arg.to_string_lossy();
                for word in [after, "acme", "widgets"] {
                    assert!(!arg.contains(word), "{word} in {arg}");
                }
            }
        }
    }

    #[test]
    fn the_two_file_fixture_page_adds_its_marks_after_the_details() {
        let more = DETAIL.replace(
            "\"hasNextPage\":false,\"endCursor\":\"Y3Vyc29yOjM=\"",
            "\"hasNextPage\":true,\"endCursor\":\"Y3Vyc29yOjM=\"",
        );
        let (runner, result) = paged(&[more, LATER_PAGE.to_string()]);
        let pr = result.unwrap().unwrap();
        assert_eq!(
            sent(&runner.calls()[1])["variables"]["after"],
            "Y3Vyc29yOjM="
        );
        assert_eq!(
            pr.viewed[3..],
            [
                ("src/later.rs".to_string(), ViewedState::Viewed),
                ("docs/later.md".to_string(), ViewedState::Dismissed),
            ]
        );
        assert_eq!(pr.viewed_gap, None);
    }

    #[test]
    fn paging_stops_at_its_cap_of_pages_and_files() {
        let mut pages = vec![detail_with_files(0..100, 3000, Some("c1"))];
        for page in 1..MAX_FILES_PAGES {
            let next = format!("c{}", page + 1);
            pages.push(files_page(page * 100..(page + 1) * 100, Some(&next)));
        }
        let (runner, result) = paged(&pages);
        let pr = result.unwrap().unwrap();
        assert_eq!(runner.calls().len(), MAX_FILES_PAGES);
        assert_eq!(pr.viewed.len(), MAX_FILES);
        assert_eq!(pr.viewed, marks_of(0..MAX_FILES));
        assert_eq!(pr.viewed_gap, Some(ViewedGap::Capped));
        assert_eq!(pr.files_total, 3000);
    }

    #[test]
    fn a_failed_later_page_keeps_the_marks_read_before_it() {
        let errors = "HTTP/2.0 200 OK\nContent-Type: application/json\n\n\
                      {\"errors\":[{\"type\":\"INTERNAL\",\"message\":\"boom\"}]}";
        let malformed = "HTTP/2.0 200 OK\nContent-Type: application/json\n\n{\"data\":{\"repo";
        let status_500 = "HTTP/2.0 500 Internal Server Error\nContent-Type: application/json\n\n\
                          {\"message\":\"Server Error\"}";
        for third in [
            status_500,
            crate::github::fixtures::SERVER_ERROR,
            errors,
            malformed,
            LATER_PAGE_NULL,
        ] {
            let (runner, result) = paged(&[
                first_page(),
                files_page(100..200, Some("c2")),
                third.to_string(),
            ]);
            let pr = result.unwrap().unwrap_or_else(|| panic!("{third}"));
            assert_eq!(pr.viewed, marks_of(0..200), "{third}");
            assert_eq!(pr.viewed_gap, Some(ViewedGap::Failed), "{third}");
            assert_eq!(runner.calls().len(), 3, "{third}");
        }
    }

    #[test]
    fn a_rate_limited_later_page_stops_the_paging_and_says_until_when() {
        let (runner, result) = paged(&[first_page(), GRAPHQL_RATE_LIMITED.to_string()]);
        let pr = result.unwrap().unwrap();
        assert_eq!(runner.calls().len(), 2);
        assert_eq!(pr.viewed, marks_of(0..100));
        assert_eq!(
            pr.viewed_gap,
            Some(ViewedGap::Limited {
                until: 1_700_003_600
            })
        );
        assert_eq!(pr.viewed_limit(), Some(1_700_003_600));
        assert_eq!(fixture_detail_limit(), None);
    }

    fn fixture_detail_limit() -> Option<i64> {
        let runner = FakeGh::new().reply(answer(DETAIL));
        detail(&runner, 42).unwrap().unwrap().viewed_limit()
    }

    #[test]
    fn a_cursor_unfit_to_send_is_never_sent() {
        let long = "a".repeat(257);
        for bad in ["", long.as_str(), "a b", "a\nb", "a\u{1b}b", "caf\u{e9}"] {
            let (runner, result) = paged(&[detail_with_files(0..100, 250, Some(bad))]);
            let pr = result.unwrap().unwrap();
            assert_eq!(pr.viewed_gap, Some(ViewedGap::Failed), "{bad:?}");
            assert_eq!(pr.viewed.len(), 100);
            assert_eq!(runner.calls().len(), 1, "{bad:?}");
        }
        // A page that claims more but names no cursor stops it too.
        let none = detail_with_files(0..100, 250, Some("c1")).replace("\"c1\"", "null");
        let (runner, result) = paged(&[none]);
        assert_eq!(result.unwrap().unwrap().viewed_gap, Some(ViewedGap::Failed));
        assert_eq!(runner.calls().len(), 1);

        assert!(cursor_ok(&"a".repeat(256)));
        assert!(cursor_ok("Y3Vyc29yOjEwMA=="));
        assert!(!cursor_ok(&long));
        assert!(!cursor_ok("a\tb"));
    }

    #[test]
    fn a_repeated_cursor_or_an_empty_page_claiming_more_stops_the_paging() {
        let (runner, result) = paged(&[first_page(), files_page(100..200, Some("c1"))]);
        let pr = result.unwrap().unwrap();
        assert_eq!(pr.viewed_gap, Some(ViewedGap::Failed));
        assert_eq!(pr.viewed, marks_of(0..200));
        assert_eq!(runner.calls().len(), 2);

        let (runner, result) = paged(&[first_page(), files_page(100..100, Some("c2"))]);
        let pr = result.unwrap().unwrap();
        assert_eq!(pr.viewed_gap, Some(ViewedGap::Failed));
        assert_eq!(pr.viewed.len(), 100);
        assert_eq!(runner.calls().len(), 2);
        // Null nodes alone hold nothing either.
        let nulls =
            files_page(100..100, Some("c2")).replace("\"nodes\":[]", "\"nodes\":[null,null]");
        let (runner, result) = paged(&[first_page(), nulls]);
        assert_eq!(result.unwrap().unwrap().viewed_gap, Some(ViewedGap::Failed));
        assert_eq!(runner.calls().len(), 2);
    }

    #[test]
    fn a_path_repeated_across_pages_keeps_its_first_state_and_bad_nodes_are_skipped() {
        let later = files_page(100..103, None)
            .replace(&page_path(100), &page_path(0))
            .replace(&page_path(101), &"p".repeat(MAX_PATH + 1))
            .replace(
                "\"nodes\":[",
                "\"nodes\":[null,{\"viewerViewedState\":\"VIEWED\"},",
            );
        assert_eq!(page_state(100), "DISMISSED");
        let (_, result) = paged(&[first_page(), later]);
        let pr = result.unwrap().unwrap();
        assert_eq!(pr.viewed.len(), 101);
        assert_eq!(pr.viewed[0], (page_path(0), ViewedState::Viewed));
        assert_eq!(pr.viewed[100..], marks_of(102..103));
        assert_eq!(pr.viewed_gap, None);
    }

    #[test]
    fn queues_step_round_in_both_directions() {
        assert_eq!(QueueKind::Assigned.step(false), QueueKind::ReviewRequested);
        assert_eq!(QueueKind::AllOpen.step(false), QueueKind::Assigned);
        assert_eq!(QueueKind::Assigned.step(true), QueueKind::AllOpen);
    }

    fn origin(url: &[u8]) -> kika_git::fake::FakeRunner {
        let mut out = url.to_vec();
        out.push(0);
        kika_git::fake::FakeRunner::new()
            .expect(&["remote"], kika_git::fake::ok(b"origin\n"))
            .expect(
                &["config", "-z", "--get-all", "remote.origin.url"],
                kika_git::fake::ok(&out),
            )
    }

    fn context(gh: Arc<FakeGh>) -> ForgeContext {
        ForgeContext {
            gh,
            glab: Arc::new(crate::gitlab::FakeGlab::new()),
            sink: CommandSink::new(),
            cache_dir: None,
            cli: crate::cli::CliGate::default(),
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
        let git = kika_git::fake::FakeRunner::new();
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
        let git = kika_git::fake::FakeRunner::new();
        let (_, result) = run(&git, &context(Arc::clone(&gh)), &load, |gh, repo| {
            gh.fetch_detail(repo, 42)
        });
        assert_eq!(result, Err(ReviewError::Limited { until: NOW + 30 }));
        assert_eq!(gh.calls().len(), 1);
    }

    #[test]
    fn a_repository_the_allow_list_leaves_out_asks_nobody() {
        let rule = |text: &str| crate::allow::AllowRule::parse(text).unwrap();
        let other_only = [host().with_allow(vec![rule("other")])];
        let gh = Arc::new(FakeGh::new().reply(answer(QUEUES)));
        let load = ReviewLoad {
            hosts: &other_only,
            known: None,
            limited: &[],
            now: NOW,
        };
        let git = origin(b"git@github.com:acme/widgets.git");
        let (repo, result) = run(&git, &context(Arc::clone(&gh)), &load, |gh, repo| {
            gh.fetch_queues(repo)
        });
        git.assert_done();
        assert_eq!((repo, result), (None, Err(ReviewError::NoForgeHost)));
        // Known from an earlier load makes no difference.
        let known = self::repo();
        let (repo, result) = run(
            &kika_git::fake::FakeRunner::new(),
            &context(Arc::clone(&gh)),
            &ReviewLoad {
                known: Some(&known),
                ..load
            },
            |gh, repo| gh.fetch_queues(repo),
        );
        assert_eq!((repo, result), (None, Err(ReviewError::NoForgeHost)));
        assert_eq!(gh.calls(), []);
        // Nor may its commits be fetched.
        assert_eq!(
            fetch_remote(&kika_git::fake::FakeRunner::new(), &other_only, &known),
            Err(NoFetchRemote::NotNamed)
        );

        // A listed repository is fetched as without a list.
        let listed = [host().with_allow(vec![rule("other"), rule("ACME/Widgets")])];
        let (repo, result) = run(
            &kika_git::fake::FakeRunner::new(),
            &context(Arc::clone(&gh)),
            &ReviewLoad {
                hosts: &listed,
                known: Some(&known),
                ..load
            },
            |gh, repo| gh.fetch_queues(repo),
        );
        assert_eq!(repo, Some(known));
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(gh.calls().len(), 1);
    }

    #[test]
    fn a_rate_limit_is_found_by_host_name_whatever_its_allow_list() {
        let narrowed = host().with_allow(vec![]);
        let limited = [(host(), NOW + 10)];
        assert_eq!(limited_until(&limited, &narrowed, NOW), Some(NOW + 10));
    }

    fn remote_named(name: &str, url: &[u8]) -> kika_git::fake::FakeRunner {
        let mut out = url.to_vec();
        out.push(0);
        kika_git::fake::FakeRunner::new()
            .expect(
                &["remote"],
                kika_git::fake::ok(format!("{name}\n").as_bytes()),
            )
            .expect(
                &["config", "-z", "--get-all", &format!("remote.{name}.url")],
                kika_git::fake::ok(&out),
            )
    }

    #[test]
    fn commits_are_fetched_only_from_the_remote_of_the_reviewed_repository() {
        let hosts = [host()];
        let git = remote_named("upstream", b"git@github.com:acme/widgets.git");
        assert_eq!(
            fetch_remote(&git, &hosts, &repo()),
            Ok(RemoteName::from("upstream"))
        );
        git.assert_done();
        for url in [
            &b"https://github.com/acme/other"[..],
            b"https://github.com.evil.io/acme/widgets",
            b"/srv/git/widgets.git",
        ] {
            let git = remote_named("origin", url);
            assert_eq!(
                fetch_remote(&git, &hosts, &repo()),
                Err(NoFetchRemote::NotNamed),
                "{url:?}"
            );
        }
        // A host no longer configured is asked nothing, not even git.
        let git = kika_git::fake::FakeRunner::new();
        assert_eq!(
            fetch_remote(&git, &[], &repo()),
            Err(NoFetchRemote::NotNamed)
        );
        let other = [ForgeHost::parse("github.example.com").unwrap()];
        assert_eq!(
            fetch_remote(&git, &other, &repo()),
            Err(NoFetchRemote::NotNamed)
        );
        git.assert_done();
        // A name git would read as an option is never handed to it.
        let git = remote_named("-x", b"https://github.com/acme/widgets");
        assert_eq!(
            fetch_remote(&git, &hosts, &repo()),
            Err(NoFetchRemote::NotNamed)
        );
    }

    #[test]
    fn a_remote_differing_only_in_case_is_the_reviewed_repository() {
        let hosts = [host()];
        for url in [
            &b"git@github.com:Acme/Widgets.git"[..],
            b"https://github.com/ACME/widgets",
        ] {
            let git = remote_named("origin", url);
            assert_eq!(
                fetch_remote(&git, &hosts, &repo()),
                Ok(RemoteName::from("origin")),
                "{url:?}"
            );
            git.assert_done();
        }
        let (lab, nested) = nested_lab();
        let git = remote_named("origin", b"https://gitlab.example.com/Acme/SUB/Widgets");
        assert_eq!(
            fetch_remote(&git, &[lab], &nested),
            Ok(RemoteName::from("origin"))
        );
        git.assert_done();
        // Ignoring case loosens nothing else: a name git would read as an
        // option is still refused.
        let git = remote_named("-x", b"https://github.com/Acme/Widgets");
        assert_eq!(
            fetch_remote(&git, &hosts, &repo()),
            Err(NoFetchRemote::NotNamed)
        );
    }

    #[test]
    fn a_remote_differing_only_in_non_ascii_case_is_another_repository() {
        // Only ASCII case is ignored. Today the remote's parser refuses a
        // non-ASCII path outright; either way this stays refused.
        let known = ForgeRepo {
            owner: "ägare".into(),
            ..repo()
        };
        let git = remote_named("origin", "https://github.com/Ägare/widgets".as_bytes());
        assert_eq!(
            fetch_remote(&git, &[host()], &known),
            Err(NoFetchRemote::NotNamed)
        );
    }

    /// `origin` with a mirror URL and the real one, in either order.
    const SEVERAL_URLS: [&[u8]; 2] = [
        b"https://github.com/me/mirror.git\0https://github.com/acme/widgets.git\0",
        b"https://github.com/acme/widgets.git\0https://github.com/me/mirror.git\0",
    ];

    fn several_urls(urls: &[u8]) -> kika_git::fake::FakeRunner {
        kika_git::fake::FakeRunner::new()
            .expect(&["remote"], kika_git::fake::ok(b"origin\n"))
            .expect(
                &["config", "-z", "--get-all", "remote.origin.url"],
                kika_git::fake::ok(urls),
            )
    }

    #[test]
    fn a_remote_with_several_urls_names_no_repository_in_either_order() {
        let gh = Arc::new(FakeGh::new());
        let hosts = [host()];
        let load = ReviewLoad {
            hosts: &hosts,
            known: None,
            limited: &[],
            now: NOW,
        };
        let origin = RemoteName::from("origin");
        for urls in SEVERAL_URLS {
            let git = several_urls(urls);
            assert_eq!(
                resolve(&git, &hosts, None),
                Err(ReviewError::SeveralUrls(origin.clone()))
            );
            git.assert_done();
            let git = several_urls(urls);
            let (repo, result) = run(&git, &context(Arc::clone(&gh)), &load, |gh, repo| {
                gh.fetch_queues(repo)
            });
            git.assert_done();
            assert_eq!(
                (repo, result),
                (None, Err(ReviewError::SeveralUrls(origin.clone())))
            );
        }
        assert_eq!(gh.calls(), []);
    }

    #[test]
    fn commits_are_never_fetched_from_a_remote_with_several_urls() {
        // Even with the real repository known from an earlier load, as
        // when a URL was added during the session.
        let hosts = [host()];
        for urls in SEVERAL_URLS {
            let git = several_urls(urls);
            assert_eq!(
                fetch_remote(&git, &hosts, &repo()),
                Err(NoFetchRemote::SeveralUrls(RemoteName::from("origin")))
            );
            git.assert_done();
        }
    }

    #[test]
    fn a_limit_counts_only_before_its_second() {
        let limited = [(host(), NOW + 10)];
        assert_eq!(limited_until(&limited, &host(), NOW), Some(NOW + 10));
        assert_eq!(limited_until(&limited, &host(), NOW + 10), None);
        let other = ForgeHost::parse("github.example.com").unwrap();
        assert_eq!(limited_until(&limited, &other, NOW), None);
    }

    fn nested_lab() -> (ForgeHost, ForgeRepo) {
        let host = ForgeHost::parse_gitlab("gitlab.example.com").unwrap();
        let repo = ForgeRepo {
            host: "gitlab.example.com".into(),
            owner: "acme/sub".into(),
            name: "widgets".into(),
        };
        (host, repo)
    }

    #[test]
    fn a_nested_gitlab_remote_resolves_and_an_allow_entry_for_its_top_group_covers_it() {
        let (host, nested) = nested_lab();
        let git = origin(b"https://gitlab.example.com/acme/sub/widgets");
        assert_eq!(
            resolve(&git, std::slice::from_ref(&host), None),
            Ok((host.clone(), nested.clone()))
        );
        git.assert_done();

        let rule = |text: &str| crate::allow::AllowRule::parse(text).unwrap();
        let listed = [host.clone().with_allow(vec![rule("acme")])];
        let git = origin(b"https://gitlab.example.com/acme/sub/widgets");
        assert_eq!(
            resolve(&git, &listed, None),
            Ok((listed[0].clone(), nested.clone()))
        );
        let other = [host.with_allow(vec![rule("other")])];
        assert_eq!(
            resolve(&kika_git::fake::FakeRunner::new(), &other, Some(&nested)),
            Err(ReviewError::NoForgeHost)
        );
    }

    #[test]
    fn commits_of_a_nested_gitlab_project_are_fetched_only_from_its_own_remote() {
        let (host, nested) = nested_lab();
        let hosts = [host];
        let git = remote_named("origin", b"https://gitlab.example.com/acme/sub/widgets");
        assert_eq!(
            fetch_remote(&git, &hosts, &nested),
            Ok(RemoteName::from("origin"))
        );
        git.assert_done();
        for url in [
            &b"https://gitlab.example.com/acme/other/widgets"[..],
            b"https://gitlab.example.com/acme/widgets",
            b"https://gitlab.example.com/acme/sub/sub/widgets",
        ] {
            let git = remote_named("origin", url);
            assert_eq!(
                fetch_remote(&git, &hosts, &nested),
                Err(NoFetchRemote::NotNamed),
                "{url:?}"
            );
        }
    }

    #[test]
    fn github_stays_two_parts_a_nested_remote_or_owner_asks_nobody() {
        let git = origin(b"https://github.com/a/b/c");
        assert_eq!(
            resolve(&git, &[host()], None),
            Err(ReviewError::NoForgeHost)
        );

        let runner = FakeGh::new();
        let nested = ForgeRepo {
            owner: "a/b".into(),
            name: "c".into(),
            ..repo()
        };
        let result = gh(&runner, &CommandSink::new(), &host()).fetch_queues(&nested);
        assert_eq!(result, Err(Refusal::Status(ForgeStatus::Failed)));
        assert_eq!(runner.calls(), []);
    }
}
