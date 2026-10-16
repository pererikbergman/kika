//! Merge requests to review on GitLab: four queues in one GraphQL call,
//! and one merge request's detail, with its pipeline's jobs, in another.
//! They are read into the same types GitHub's answers are, and every
//! string is made terminal-safe while it is parsed. The project path and
//! the number travel as GraphQL variables on stdin; argv is the fixed
//! `glab api graphql` line.

use serde::Deserialize;
use serde_json::json;

use super::github::Refusal;
use super::gitlab::{Glab, GlabCall};
use super::review::{
    Check, CheckStatus, MAX_BODY, MAX_CHECKS, MAX_LABELS, MAX_REVIEWERS, MergeState, Mergeability,
    Nodes, PrDetail, PrSummary, QUEUE_SIZE, Queues, ReviewVerdict, capped_body, data_of, full_oid,
    line, read_with, safe_url,
};
use super::{ForgeRepo, ForgeStatus, PrState};

impl Glab<'_> {
    /// The four queues of `repo`, in one call.
    pub fn fetch_queues(&self, repo: &ForgeRepo) -> Result<Queues, Refusal> {
        let path = project_path(repo);
        let variables = json!({ "path": path, "userPath": path });
        let kind = GlabCall::Graphql {
            body: serde_json::to_vec(&json!({ "query": QUEUES_QUERY, "variables": variables }))
                .unwrap_or_default(),
        };
        self.call(kind, |answer| read_with(answer, read_queues))
    }

    /// Merge request `number` of `repo`; `None` when GitLab has none.
    pub fn fetch_detail(&self, repo: &ForgeRepo, number: u64) -> Result<Option<PrDetail>, Refusal> {
        let kind = GlabCall::Graphql {
            body: mr_request(DETAIL_QUERY, repo, number),
        };
        let host = self.host.as_str().to_ascii_lowercase();
        self.call(kind, |answer| {
            read_with(answer, |body| read_detail(body, number, &host))
        })
    }
}

/// The project's full path, as GitLab names it.
fn project_path(repo: &ForgeRepo) -> String {
    format!("{}/{}", repo.owner, repo.name)
}

/// `{query, variables}` for stdin: the project path, and the merge
/// request's iid, a string in GitLab's schema.
pub(crate) fn mr_request(query: &str, repo: &ForgeRepo, iid: u64) -> Vec<u8> {
    let variables = json!({ "path": project_path(repo), "iid": iid.to_string() });
    serde_json::to_vec(&json!({ "query": query, "variables": variables })).unwrap_or_default()
}

/// The queues query: fixed text, the project path its variable, twice
/// over since GitLab types it `String` on `currentUser` and `ID` on
/// `project`. The three of the login's own are asked on `currentUser`,
/// the open ones on the project; each with the most recently updated
/// first.
const QUEUES_QUERY: &str = "query($path:ID!,$userPath:String!){currentUser{\
assignedMergeRequests(projectPath:$userPath,state:opened,sort:UPDATED_DESC,first:50)\
{nodes{iid title draft updatedAt author{username}}} \
reviewRequestedMergeRequests(projectPath:$userPath,state:opened,sort:UPDATED_DESC,first:50)\
{nodes{iid title draft updatedAt author{username}}} \
authoredMergeRequests(projectPath:$userPath,state:opened,sort:UPDATED_DESC,first:50)\
{nodes{iid title draft updatedAt author{username}}}} \
project(fullPath:$path){mergeRequests(state:opened,sort:UPDATED_DESC,first:50)\
{nodes{iid title draft updatedAt author{username}}}}}";

/// The detail query: fixed text; the project path and iid are variables.
const DETAIL_QUERY: &str = "query($path:ID!,$iid:String!){project(fullPath:$path)\
{mergeRequest(iid:$iid){title description state draft sourceBranch targetBranch \
conflicts detailedMergeStatus author{username} \
labels(first:20){nodes{title}} reviewers(first:20){nodes{username}} \
approvedBy(first:20){nodes{username}} diffRefs{headSha startSha baseSha} \
diffStatsSummary{fileCount} headPipeline{jobs(first:100){nodes{name status webPath}}}}}}";

#[derive(Deserialize)]
pub(crate) struct User {
    pub(crate) username: Option<String>,
}

/// A user's name as drawn: a deleted account has none, and is `ghost`.
pub(crate) fn username(user: Option<User>) -> String {
    let name = user.and_then(|u| u.username);
    line(name.as_deref().unwrap_or("ghost"))
}

/// An iid as a number: decimal digits only, as `parse` alone would take
/// a leading `+`.
fn iid_number(iid: &str) -> Option<u64> {
    iid.bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| iid.parse().ok())
        .flatten()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueuesData {
    /// Null for an anonymous login: its three queues are empty.
    current_user: Option<UserQueues>,
    project: Option<ProjectQueue>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserQueues {
    assigned_merge_requests: Option<Nodes<RowNode>>,
    review_requested_merge_requests: Option<Nodes<RowNode>>,
    authored_merge_requests: Option<Nodes<RowNode>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectQueue {
    merge_requests: Option<Nodes<RowNode>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RowNode {
    iid: Option<String>,
    title: Option<String>,
    #[serde(default)]
    draft: bool,
    updated_at: Option<String>,
    author: Option<User>,
}

fn read_queues(body: &[u8]) -> Result<Queues, ForgeStatus> {
    let data: QueuesData = data_of(body)?;
    let project = data.project.ok_or(ForgeStatus::NoAccess)?;
    let list = |nodes: Option<Nodes<RowNode>>| -> Vec<PrSummary> {
        nodes
            .into_iter()
            .flat_map(Nodes::items)
            .filter_map(|node| {
                Some(PrSummary {
                    number: iid_number(node.iid.as_deref()?)?,
                    title: line(node.title.as_deref().unwrap_or_default()),
                    author: username(node.author),
                    draft: node.draft,
                    updated: line(node.updated_at.as_deref().unwrap_or_default()),
                })
            })
            .take(QUEUE_SIZE)
            .collect()
    };
    let (assigned, requested, authored) = match data.current_user {
        Some(user) => (
            list(user.assigned_merge_requests),
            list(user.review_requested_merge_requests),
            list(user.authored_merge_requests),
        ),
        None => Default::default(),
    };
    Ok(Queues::from_lists([
        assigned,
        requested,
        authored,
        list(project.merge_requests),
    ]))
}

#[derive(Deserialize)]
struct DetailData {
    project: Option<DetailProject>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DetailProject {
    merge_request: Option<MrNode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MrNode {
    title: Option<String>,
    description: Option<String>,
    state: Option<String>,
    #[serde(default)]
    draft: bool,
    source_branch: Option<String>,
    target_branch: Option<String>,
    conflicts: Option<bool>,
    detailed_merge_status: Option<String>,
    author: Option<User>,
    labels: Option<Nodes<Label>>,
    reviewers: Option<Nodes<User>>,
    approved_by: Option<Nodes<User>>,
    diff_refs: Option<DiffRefs>,
    diff_stats_summary: Option<DiffStats>,
    head_pipeline: Option<Pipeline>,
}

#[derive(Deserialize)]
struct Label {
    title: Option<String>,
}

/// The commits a merge request's diff is between.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DiffRefs {
    pub(crate) head_sha: Option<String>,
    /// The target branch's tip when the diff was made.
    pub(crate) start_sha: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiffStats {
    file_count: Option<u64>,
}

#[derive(Deserialize)]
struct Pipeline {
    jobs: Option<Nodes<Job>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Job {
    name: Option<String>,
    status: Option<String>,
    web_path: Option<String>,
}

/// The detail of merge request `number`, from a body [`classify`] passed
/// (so the project is there). `host` places a job's page.
///
/// [`classify`]: super::gitlab::classify
fn read_detail(body: &[u8], number: u64, host: &str) -> Result<Option<PrDetail>, ForgeStatus> {
    let data: DetailData = data_of(body)?;
    let project = data.project.ok_or(ForgeStatus::NoAccess)?;
    let Some(mr) = project.merge_request else {
        return Ok(None);
    };
    let state = match mr.state.as_deref() {
        Some("merged") => PrState::Merged,
        Some("closed") => PrState::Closed,
        _ => PrState::Open,
    };
    let mergeable = match mr.conflicts {
        Some(true) => Mergeability::Conflicting,
        Some(false) => Mergeability::Mergeable,
        None => Mergeability::Unknown,
    };
    let names = |nodes: Option<Nodes<User>>| -> Vec<String> {
        nodes
            .into_iter()
            .flat_map(Nodes::items)
            .filter_map(|u| u.username.map(|n| line(&n)))
            .take(MAX_REVIEWERS)
            .collect()
    };
    let labels = mr
        .labels
        .into_iter()
        .flat_map(Nodes::items)
        .filter_map(|l| l.title.map(|t| line(&t)))
        .take(MAX_LABELS)
        .collect();
    let reviews = names(mr.approved_by)
        .into_iter()
        .map(|who| (who, ReviewVerdict::Approved))
        .collect();
    let checks = mr
        .head_pipeline
        .and_then(|p| p.jobs)
        .into_iter()
        .flat_map(Nodes::items)
        .map(|job| check_of(job, host))
        .take(MAX_CHECKS)
        .collect();
    let (head_oid, base_oid) = match mr.diff_refs {
        Some(refs) => (full_oid(refs.head_sha), full_oid(refs.start_sha)),
        None => (None, None),
    };
    Ok(Some(PrDetail {
        number,
        node_id: None,
        title: line(mr.title.as_deref().unwrap_or_default()),
        author: username(mr.author),
        state,
        draft: mr.draft,
        body: capped_body(mr.description.as_deref().unwrap_or_default(), MAX_BODY),
        labels,
        requested_reviewers: names(mr.reviewers),
        reviews,
        base: line(mr.target_branch.as_deref().unwrap_or_default()),
        head: line(mr.source_branch.as_deref().unwrap_or_default()),
        mergeable,
        merge_state: merge_state_of(mr.detailed_merge_status.as_deref()),
        merge_methods: Vec::new(),
        checks,
        head_oid,
        base_oid,
        viewed: Vec::new(),
        viewed_gap: None,
        files_total: mr
            .diff_stats_summary
            .and_then(|s| s.file_count)
            .unwrap_or(0),
    }))
}

/// What stands between the merge request and a merge, from GitLab's
/// `detailedMergeStatus`. Any value Kika does not know is `Unknown`.
fn merge_state_of(status: Option<&str>) -> MergeState {
    match status {
        Some("MERGEABLE") => MergeState::Clean,
        Some("CONFLICT" | "BROKEN_STATUS") => MergeState::Dirty,
        Some("NEED_REBASE") => MergeState::Behind,
        Some("DRAFT_STATUS") => MergeState::Draft,
        Some(
            "BLOCKED_STATUS"
            | "CI_MUST_PASS"
            | "CI_STILL_RUNNING"
            | "DISCUSSIONS_NOT_RESOLVED"
            | "EXTERNAL_STATUS_CHECKS"
            | "JIRA_ASSOCIATION_MISSING"
            | "LOCKED_LFS_FILES"
            | "LOCKED_PATHS"
            | "NOT_APPROVED"
            | "REQUESTED_CHANGES"
            | "SECURITY_POLICY_VIOLATIONS"
            | "TITLE_NOT_MATCHING"
            | "COMMITS_STATUS",
        ) => MergeState::Blocked,
        _ => MergeState::Unknown,
    }
}

/// A pipeline job as a check, its status read as the badge reads a
/// pipeline's: a manual, skipped or unknown job is never a failure.
fn check_of(job: Job, host: &str) -> Check {
    let status = match job.status.as_deref() {
        Some("SUCCESS") => CheckStatus::Success,
        Some("FAILED" | "CANCELED") => CheckStatus::Failure,
        Some(
            "CREATED"
            | "WAITING_FOR_RESOURCE"
            | "PREPARING"
            | "PENDING"
            | "RUNNING"
            | "SCHEDULED"
            | "WAITING_FOR_CALLBACK"
            | "CANCELING",
        ) => CheckStatus::Pending,
        Some("SKIPPED") => CheckStatus::Skipped,
        _ => CheckStatus::Neutral,
    };
    // Only a path on the host itself: `//elsewhere` would name another.
    let details_url = job
        .web_path
        .filter(|p| p.starts_with('/') && !p.starts_with("//"))
        .and_then(|p| safe_url(&format!("https://{host}{p}")));
    Check {
        name: line(job.name.as_deref().unwrap_or_default()),
        status,
        details_url,
    }
}

#[cfg(any(test, feature = "test-support"))]
pub mod fixtures {
    use crate::review::fixtures::HOSTILE;

    pub const QUEUES: &str = include_str!("fixtures/gitlab_review_queues.http");
    pub const DETAIL: &str = include_str!("fixtures/gitlab_review_detail.http");
    pub const DETAIL_MISSING: &str = include_str!("fixtures/gitlab_review_detail_missing.http");

    /// [`DETAIL`] with `HOSTILE` in every string the GitHub one has it in.
    pub fn hostile_detail() -> String {
        let quoted = serde_json::to_string(HOSTILE).unwrap();
        DETAIL
            .replace("\"Add widget caching\"", &quoted)
            .replace("\"octocat\"", &quoted)
            .replace("\"enhancement\"", &quoted)
            .replace("\"build\"", &quoted)
            .replace("\"main\"", &quoted)
            .replace("## Summary", quoted.trim_matches('"'))
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::fixtures::*;
    use super::*;
    use crate::ForgeHost;
    use crate::github::answer;
    use crate::gitlab::fixtures::{
        GRAPHQL_ERRORS, PROJECT_NULL, RATE_LIMITED_429, UNAUTHORIZED_401,
    };
    use crate::gitlab::{FakeGlab, GlabCommand};
    use crate::review::QueueKind;
    use crate::url::ForgeKind;
    use kika_git::commit_files::CommitId;
    use kika_git::recording::CommandSink;

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

    pub(crate) fn glab<'a>(
        runner: &'a FakeGlab,
        sink: &'a CommandSink,
        host: &'a ForgeHost,
    ) -> Glab<'a> {
        Glab {
            runner,
            sink,
            host,
            cwd: "/cache".into(),
            now: NOW,
        }
    }

    fn queues(runner: &FakeGlab) -> Result<Queues, Refusal> {
        glab(runner, &CommandSink::new(), &host()).fetch_queues(&repo())
    }

    fn detail(runner: &FakeGlab, number: u64) -> Result<Option<PrDetail>, Refusal> {
        glab(runner, &CommandSink::new(), &host()).fetch_detail(&repo(), number)
    }

    fn sent(cmd: &GlabCommand) -> serde_json::Value {
        serde_json::from_slice(&cmd.stdin()).unwrap()
    }

    fn numbers(list: &[PrSummary]) -> Vec<u64> {
        list.iter().map(|p| p.number).collect()
    }

    fn argv(cmd: &GlabCommand) -> Vec<String> {
        cmd.args()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn the_queues_are_one_call_with_the_path_on_stdin_only() {
        let runner = FakeGlab::new().reply(answer(QUEUES));
        queues(&runner).unwrap();
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
        assert!(argv(&cmd).iter().all(|a| !a.contains("acme")));
        let body = sent(&cmd);
        assert_eq!(
            body["variables"],
            json!({ "path": "acme/widgets", "userPath": "acme/widgets" })
        );
        let query = body["query"].as_str().unwrap();
        assert_eq!(query, QUEUES_QUERY);
        for word in ["acme", "widgets"] {
            assert!(!query.contains(word), "{word} in {query}");
        }
        for field in [
            "assignedMergeRequests(projectPath:$userPath,state:opened,sort:UPDATED_DESC,first:50)",
            "reviewRequestedMergeRequests(projectPath:$userPath,state:opened,sort:UPDATED_DESC,first:50)",
            "authoredMergeRequests(projectPath:$userPath,state:opened,sort:UPDATED_DESC,first:50)",
            "project(fullPath:$path){mergeRequests(state:opened,sort:UPDATED_DESC,first:50)",
        ] {
            assert!(query.contains(field), "{field} not asked: {query}");
            let row = "{nodes{iid title draft updatedAt author{username}}}";
            assert!(query.contains(&format!("{field}{row}")), "{field}");
        }
        assert!(QUEUES_QUERY.contains(&format!("first:{QUEUE_SIZE}")));
    }

    #[test]
    fn each_queue_holds_its_own_merge_requests_in_order() {
        let runner = FakeGlab::new().reply(answer(QUEUES));
        let queues = queues(&runner).unwrap();
        assert_eq!(numbers(queues.get(QueueKind::Assigned)), [12, 7]);
        assert_eq!(numbers(queues.get(QueueKind::ReviewRequested)), [15, 12]);
        assert_eq!(numbers(queues.get(QueueKind::Authored)), Vec::<u64>::new());
        assert_eq!(numbers(queues.get(QueueKind::AllOpen)), [15, 12, 9, 7]);
        assert_eq!(
            queues.get(QueueKind::AllOpen)[0],
            PrSummary {
                number: 15,
                title: "Draft: new gadget API".into(),
                author: "alice".into(),
                draft: true,
                updated: "2023-11-14T21:00:00Z".into(),
            }
        );
    }

    #[test]
    fn a_row_with_a_bad_iid_is_dropped_and_no_user_leaves_three_queues_empty() {
        let odd = QUEUES
            .replacen("\"iid\":\"12\"", "\"iid\":\"+12\"", 1)
            .replace("\"author\":{\"username\":\"hubot\"}", "\"author\":null");
        let runner = FakeGlab::new().reply(answer(&odd));
        let got = queues(&runner).unwrap();
        assert_eq!(numbers(got.get(QueueKind::Assigned)), [7]);
        assert_eq!(got.get(QueueKind::Assigned)[0].author, "ghost");

        let start = QUEUES.find("{\"assignedMergeRequests\"").unwrap();
        let end = QUEUES.find(",\"project\"").unwrap();
        let anonymous = format!("{}null{}", &QUEUES[..start], &QUEUES[end..]);
        let runner = FakeGlab::new().reply(answer(&anonymous));
        let got = queues(&runner).unwrap();
        for kind in [
            QueueKind::Assigned,
            QueueKind::ReviewRequested,
            QueueKind::Authored,
        ] {
            assert!(got.get(kind).is_empty(), "{kind:?}");
        }
        assert_eq!(numbers(got.get(QueueKind::AllOpen)), [15, 12, 9, 7]);
    }

    #[test]
    fn the_detail_parses_its_state_people_branches_oids_and_jobs() {
        let runner = FakeGlab::new().reply(answer(DETAIL));
        let pr = detail(&runner, 42).unwrap().unwrap();
        let [cmd] = runner.calls().try_into().unwrap();
        let body = sent(&cmd);
        assert_eq!(
            body["variables"],
            json!({ "path": "acme/widgets", "iid": "42" })
        );
        assert_eq!(body["query"], DETAIL_QUERY);
        assert!(argv(&cmd).iter().all(|a| !a.contains("acme") && a != "42"));

        assert_eq!(pr.number, 42);
        assert_eq!(pr.node_id, None);
        assert_eq!(pr.title, "Add widget caching");
        assert_eq!(pr.author, "octocat");
        assert_eq!((pr.state, pr.draft), (PrState::Open, false));
        assert_eq!(pr.labels, ["enhancement", "needs-review"]);
        assert_eq!(pr.requested_reviewers, ["hubot", "carol"]);
        assert_eq!(pr.reviews, [("alice".to_string(), ReviewVerdict::Approved)]);
        assert_eq!(
            (pr.base.as_str(), pr.head.as_str()),
            ("main", "feature/cache")
        );
        assert_eq!(pr.head_oid, CommitId::parse(&"1".repeat(40)));
        // The target's tip, not GitLab's merge base.
        assert_eq!(pr.base_oid, CommitId::parse(&"2".repeat(40)));
        assert_eq!(pr.files_total, 3);
        assert_eq!(pr.mergeable, Mergeability::Mergeable);
        assert_eq!(pr.merge_state, MergeState::Clean);
        assert_eq!(pr.merge_methods, []);
        assert_eq!((pr.viewed.len(), pr.viewed_gap), (0, None));
        assert!(pr.body.starts_with("## Summary\n\n"), "{:?}", pr.body);
        assert!(!pr.body.contains('\r'));
        let job = |name: &str, status, n: u32| Check {
            name: name.into(),
            status,
            details_url: Some(format!(
                "https://gitlab.example.com/acme/widgets/-/jobs/{n}"
            )),
        };
        assert_eq!(
            pr.checks,
            [
                job("build", CheckStatus::Failure, 1),
                job("lint", CheckStatus::Success, 2),
                job("test", CheckStatus::Pending, 3),
                // Waiting for a person, skipped, or unknown: no failure.
                job("deploy", CheckStatus::Neutral, 4),
                job("docs", CheckStatus::Skipped, 5),
                job("odd", CheckStatus::Neutral, 6),
                job("retry", CheckStatus::Failure, 7),
            ]
        );
    }

    #[test]
    fn each_state_and_merge_status_maps_to_its_own() {
        for (text, draft, want) in [
            ("opened", true, PrState::Open),
            ("locked", false, PrState::Open),
            ("merged", false, PrState::Merged),
            ("closed", false, PrState::Closed),
        ] {
            let other = DETAIL
                .replace("\"state\":\"opened\"", &format!("\"state\":\"{text}\""))
                .replace("\"draft\":false", &format!("\"draft\":{draft}"));
            let pr = detail(&FakeGlab::new().reply(answer(&other)), 42)
                .unwrap()
                .unwrap();
            assert_eq!((pr.state, pr.draft), (want, draft), "{text}");
        }
        for (text, want) in [
            ("CONFLICT", MergeState::Dirty),
            ("NEED_REBASE", MergeState::Behind),
            ("DRAFT_STATUS", MergeState::Draft),
            ("NOT_APPROVED", MergeState::Blocked),
            ("CI_STILL_RUNNING", MergeState::Blocked),
            ("DISCUSSIONS_NOT_RESOLVED", MergeState::Blocked),
            ("CHECKING", MergeState::Unknown),
            ("SOMETHING_NEW", MergeState::Unknown),
        ] {
            let other = DETAIL.replace("\"MERGEABLE\"", &format!("\"{text}\""));
            let pr = detail(&FakeGlab::new().reply(answer(&other)), 42)
                .unwrap()
                .unwrap();
            assert_eq!(pr.merge_state, want, "{text}");
        }
        let conflicting = DETAIL.replace("\"conflicts\":false", "\"conflicts\":true");
        let pr = detail(&FakeGlab::new().reply(answer(&conflicting)), 42)
            .unwrap()
            .unwrap();
        assert_eq!(pr.mergeable, Mergeability::Conflicting);
        let bare = DETAIL
            .replace("\"conflicts\":false,", "")
            .replace("\"headPipeline\":{", "\"headPipeline\":null,\"x\":{")
            .replace(&"1".repeat(40), "1111");
        let pr = detail(&FakeGlab::new().reply(answer(&bare)), 42)
            .unwrap()
            .unwrap();
        assert_eq!(pr.mergeable, Mergeability::Unknown);
        assert_eq!(pr.checks, []);
        assert_eq!(pr.head_oid, None);
    }

    #[test]
    fn a_missing_merge_request_is_none_and_refusals_are_the_badges_ones() {
        assert_eq!(
            detail(&FakeGlab::new().reply(answer(DETAIL_MISSING)), 999),
            Ok(None)
        );
        for (fixture, want) in [
            (PROJECT_NULL, Refusal::Status(ForgeStatus::NoAccess)),
            (UNAUTHORIZED_401, Refusal::Status(ForgeStatus::NotLoggedIn)),
            (GRAPHQL_ERRORS, Refusal::Status(ForgeStatus::Failed)),
            (
                RATE_LIMITED_429,
                Refusal::Limited {
                    until: 1_700_001_200,
                },
            ),
        ] {
            assert_eq!(
                detail(&FakeGlab::new().reply(answer(fixture)), 42),
                Err(want)
            );
            assert_eq!(queues(&FakeGlab::new().reply(answer(fixture))), Err(want));
        }
        // GraphQL errors inside a 200 that `glab` exits 1 on.
        let exit_1 = crate::github::GhOutput {
            exit: Some(1),
            ..answer(GRAPHQL_ERRORS)
        };
        assert_eq!(
            queues(&FakeGlab::new().reply(exit_1)),
            Err(Refusal::Status(ForgeStatus::Failed))
        );
        assert_eq!(
            queues(&FakeGlab::new().missing()),
            Err(Refusal::Status(ForgeStatus::NoGh))
        );
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
    fn hostile_text_comes_out_terminal_safe_and_job_pages_stay_on_the_host() {
        let pr = detail(&FakeGlab::new().reply(answer(&hostile_detail())), 42)
            .unwrap()
            .unwrap();
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
        assert!(pr.body.contains("red"));

        for bad in [
            "//evil.example.com/x",
            "https://evil.example.com/x",
            "/a b",
            "/\\u001b]52",
        ] {
            let other = DETAIL.replace("/acme/widgets/-/jobs/1", bad);
            let pr = detail(&FakeGlab::new().reply(answer(&other)), 42)
                .unwrap()
                .unwrap();
            assert_eq!(pr.checks[0].details_url, None, "{bad}");
        }

        let hostile = serde_json::to_string(crate::review::fixtures::HOSTILE).unwrap();
        let queue = QUEUES.replace("\"Cache widget lookups\"", &hostile);
        let got = queues(&FakeGlab::new().reply(answer(&queue))).unwrap();
        no_controls(&got.get(QueueKind::Assigned)[0].title);
        assert!(got.get(QueueKind::Assigned)[0].title.contains("red"));
    }
}
