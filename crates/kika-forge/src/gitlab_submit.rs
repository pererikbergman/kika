//! Submitting a pending review to GitLab. The pending review stays on
//! disk while it is written, as on GitHub; only a submit makes GitLab
//! draft notes, one per comment and one for the summary, and publishes
//! them together, so the merge request shows them all at once or not at
//! all. An approval follows, held to the head the comments were written
//! on. Bodies travel only on stdin, and every REST path is made of
//! decimal ids, so argv holds no text from the review or the repository.

use serde::Deserialize;
use serde_json::{Value, json};
use sha1::{Digest, Sha1};

use super::anchor::GhSide;
use super::discussion_write::{ReviewEvent, Written, review_ok};
use super::github::Refusal;
use super::gitlab::{Glab, GlabCall, RestMethod, RestPath};
use super::gitlab_discussion::merge_request_id_ok;
use super::pending::{PendingComment, RowPos};
use super::review::{data_of, read_with};
use super::{CreateError, ForgeStatus};

/// Asked by the merge request's global id, so no project path is sent.
const REFS_QUERY: &str = "query($id:MergeRequestID!){mergeRequest(id:$id)\
{iid project{id} diffRefs{baseSha startSha headSha}}}";

pub(crate) const NO_POSITION: &str =
    "A pending comment was written before Kika could post it to GitLab; write it again in P";
const MOVED: &str =
    "The merge request's head moved since the comments were written; refresh with r";
const NO_REQUEST_CHANGES: &str = "GitLab takes an approval or comments, not a request for changes";

/// The three commits GitLab places a diff note against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiffRefs {
    pub(crate) base: String,
    pub(crate) start: String,
    pub(crate) head: String,
}

/// What the submit needs of the merge request: its project and number,
/// for the REST paths, and its diff's commits.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    project: u64,
    iid: u64,
    refs: DiffRefs,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RefsData {
    merge_request: Option<RefsMr>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RefsMr {
    iid: Option<String>,
    project: Option<RefsProject>,
    diff_refs: Option<RefsShas>,
}

#[derive(Deserialize)]
struct RefsProject {
    id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RefsShas {
    base_sha: Option<String>,
    start_sha: Option<String>,
    head_sha: Option<String>,
}

#[derive(Deserialize)]
struct Created {
    id: u64,
}

/// Decimal digits only, as `parse` alone would take a leading `+`.
fn decimal(text: &str) -> Option<u64> {
    text.bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| text.parse().ok())
        .flatten()
}

/// A full commit id in hex, as git names one.
fn sha_ok(sha: &str) -> bool {
    matches!(sha.len(), 40 | 64) && sha.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The target of a refs answer [`classify`](super::gitlab::classify)
/// passed. A null merge request, or ids or commits that are not GitLab's
/// usual shape, are no usable answer.
fn read_target(body: &[u8]) -> Result<Target, ForgeStatus> {
    let data: RefsData = data_of(body)?;
    let failed = ForgeStatus::Failed;
    let mr = data.merge_request.ok_or(failed)?;
    let iid = mr.iid.as_deref().and_then(decimal).ok_or(failed)?;
    let project = mr
        .project
        .and_then(|p| p.id)
        .as_deref()
        .and_then(|id| id.strip_prefix("gid://gitlab/Project/"))
        .and_then(decimal)
        .ok_or(failed)?;
    let shas = mr.diff_refs.ok_or(failed)?;
    let sha = |s: Option<String>| s.filter(|s| sha_ok(s)).ok_or(failed);
    let refs = DiffRefs {
        base: sha(shas.base_sha)?,
        start: sha(shas.start_sha)?,
        head: sha(shas.head_sha)?,
    };
    Ok(Target { project, iid, refs })
}

/// GitLab's name for one side of a line in a range.
fn line_type(pos: RowPos, side: GhSide) -> Value {
    match (pos.both, side) {
        (true, _) => Value::Null,
        (false, GhSide::Right) => json!("new"),
        (false, GhSide::Left) => json!("old"),
    }
}

/// The old and new line GitLab is given for a row: an added row has only
/// its new line, a removed row only its old one, and a context row both.
fn lines(pos: RowPos, side: GhSide) -> (Value, Value) {
    match (pos.both, side) {
        (true, _) => (json!(pos.old), json!(pos.new)),
        (false, GhSide::Right) => (Value::Null, json!(pos.new)),
        (false, GhSide::Left) => (json!(pos.old), Value::Null),
    }
}

/// GitLab's `line_code`: the hex SHA-1 of the path, then both counters.
fn line_code(path: &str, pos: RowPos) -> String {
    let hash = Sha1::digest(path.as_bytes());
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    format!("{hex}_{}_{}", pos.old, pos.new)
}

/// `comment`'s place as a GitLab diff note's `position`; `None` when it
/// was written without the line counters GitLab needs. The path is the
/// new name on both sides.
pub(crate) fn draft_position(comment: &PendingComment, refs: &DiffRefs) -> Option<Value> {
    let end = comment.end_pos?;
    let (old_line, new_line) = lines(end, comment.end.side);
    let mut position = json!({
        "position_type": "text",
        "base_sha": refs.base,
        "start_sha": refs.start,
        "head_sha": refs.head,
        "old_path": comment.path,
        "new_path": comment.path,
    });
    if !old_line.is_null() {
        position["old_line"] = old_line;
    }
    if !new_line.is_null() {
        position["new_line"] = new_line;
    }
    if let Some(start) = comment.start {
        let from = comment.start_pos?;
        let point = |pos: RowPos, side: GhSide| {
            let (old_line, new_line) = lines(pos, side);
            json!({
                "line_code": line_code(&comment.path, pos),
                "type": line_type(pos, side),
                "old_line": old_line,
                "new_line": new_line,
            })
        };
        position["line_range"] = json!({
            "start": point(from, start.side),
            "end": point(end, comment.end.side),
        });
    }
    Some(position)
}

fn refused(refusal: Refusal) -> CreateError {
    match refusal {
        Refusal::Status(status) => CreateError::Status(status),
        Refusal::Limited { until } => CreateError::RateLimited { until },
        Refusal::Unsupported(unsupported) => CreateError::Unsupported(unsupported),
    }
}

fn to_body(value: &Value) -> Option<Vec<u8>> {
    Some(serde_json::to_vec(value).unwrap_or_default())
}

impl Glab<'_> {
    /// Submit a pending review to merge request `pull` (its global id),
    /// written against `commit`. Refused before any call when a comment has
    /// no GitLab position, and before any draft is made when the head moved
    /// or the reviewer has drafts of their own on GitLab, since publishing
    /// would send those too. A failure while drafting or publishing deletes
    /// the drafts this submit made, so nothing shows and the pending review
    /// is kept. An approval GitLab refuses after the comments went out is
    /// [`Written::PostedApprovalFailed`].
    pub(crate) fn submit_review(
        &self,
        pull: &str,
        commit: &str,
        event: ReviewEvent,
        body: &str,
        threads: &[PendingComment],
    ) -> Result<Written, CreateError> {
        let failed = CreateError::Status(ForgeStatus::Failed);
        if !merge_request_id_ok(pull) || !sha_ok(commit) || !review_ok(event, body, threads) {
            return Err(failed);
        }
        if event == ReviewEvent::RequestChanges {
            return Err(CreateError::Refused(NO_REQUEST_CHANGES.into()));
        }
        let placed = |c: &PendingComment| {
            c.end_pos.is_some() && (c.start.is_none() || c.start_pos.is_some())
        };
        if !threads.iter().all(placed) {
            return Err(CreateError::Refused(NO_POSITION.into()));
        }
        let target = self.target(pull)?;
        if !target.refs.head.eq_ignore_ascii_case(commit) {
            return Err(CreateError::Refused(MOVED.into()));
        }
        let Target { project, iid, refs } = target;
        let mut notes: Vec<Value> = Vec::new();
        for comment in threads {
            let position = draft_position(comment, &refs).ok_or_else(|| failed.clone())?;
            notes.push(json!({ "note": comment.body, "position": position }));
        }
        if !body.trim().is_empty() {
            notes.push(json!({ "note": body }));
        }
        // Only a publish would send drafts made elsewhere; an approval
        // alone leaves them be.
        if !notes.is_empty() {
            let drafts = RestPath::DraftNotes { project, iid };
            let theirs = self.rest(RestMethod::Get, drafts, None)?;
            let theirs: Vec<Value> = serde_json::from_slice(&theirs).map_err(|_| failed.clone())?;
            if !theirs.is_empty() {
                return Err(CreateError::Refused(format!(
                    "You have draft notes on !{iid} from elsewhere; publish or delete them on GitLab first"
                )));
            }
        }
        let mut made: Vec<u64> = Vec::new();
        let published = self.publish(project, iid, &notes, &mut made);
        if let Err(error) = published {
            for note in made {
                // Best effort: what is left is only a draft, which shows to
                // no one else.
                let _ = self.rest(
                    RestMethod::Delete,
                    RestPath::DraftNote { project, iid, note },
                    None,
                );
            }
            return Err(error);
        }
        if event != ReviewEvent::Approve {
            return Ok(Written::Posted);
        }
        let approve = RestPath::Approve { project, iid };
        match self.rest(
            RestMethod::Post,
            approve,
            to_body(&json!({ "sha": commit })),
        ) {
            Ok(_) => Ok(Written::Posted),
            Err(_) if !made.is_empty() => Ok(Written::PostedApprovalFailed),
            Err(error) => Err(error),
        }
    }

    /// The project, number and diff commits of merge request `pull`.
    fn target(&self, pull: &str) -> Result<Target, CreateError> {
        let request = json!({ "query": REFS_QUERY, "variables": { "id": pull } });
        let kind = GlabCall::Graphql {
            body: serde_json::to_vec(&request).unwrap_or_default(),
        };
        self.call(kind, |answer| read_with(answer, read_target))
            .map_err(refused)
    }

    /// Make each of `notes` a draft, keeping each one's id in `made`, then
    /// publish them all in one call when there are any.
    fn publish(
        &self,
        project: u64,
        iid: u64,
        notes: &[Value],
        made: &mut Vec<u64>,
    ) -> Result<(), CreateError> {
        let drafts = RestPath::DraftNotes { project, iid };
        for note in notes {
            let answer = self.rest(RestMethod::Post, drafts, to_body(note))?;
            let created: Created = serde_json::from_slice(&answer)
                .map_err(|_| CreateError::Status(ForgeStatus::Failed))?;
            made.push(created.id);
        }
        if made.is_empty() {
            return Ok(());
        }
        self.rest(
            RestMethod::Post,
            RestPath::BulkPublish { project, iid },
            None,
        )
        .map(|_| ())
    }
}

/// Hand-written answers in the shape GitLab gives with `--include`.
#[cfg(any(test, feature = "test-support"))]
pub mod fixtures {
    pub const REFS: &str = include_str!("fixtures/gitlab_submit_refs.http");
    pub const REFS_MOVED: &str = include_str!("fixtures/gitlab_submit_refs_moved.http");
    pub const DRAFTS_EMPTY: &str = include_str!("fixtures/gitlab_draft_notes_empty.http");
    pub const DRAFTS_SOME: &str = include_str!("fixtures/gitlab_draft_notes_some.http");
    pub const DRAFT_CREATED: &str = include_str!("fixtures/gitlab_draft_created_201.http");
    pub const NO_CONTENT: &str = include_str!("fixtures/gitlab_no_content_204.http");
    pub const APPROVED: &str = include_str!("fixtures/gitlab_approved_201.http");
    pub const CONFLICT: &str = include_str!("fixtures/gitlab_conflict_409.http");

    /// [`DRAFT_CREATED`] as draft `id`.
    pub fn created(id: u64) -> String {
        DRAFT_CREATED.replacen("\"id\":101", &format!("\"id\":{id}"), 1)
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use crate::ForgeHost;
    use crate::anchor::{GhLine, row_numbers, row_positions};
    use crate::github::answer;
    use crate::gitlab::fixtures::{RATE_LIMITED_429, UNAUTHORIZED_401};
    use crate::gitlab::{FakeGlab, GlabCommand};
    use crate::url::ForgeKind;
    use kika_git::diff::DiffLine;
    use kika_git::recording::CommandSink;

    const NOW: i64 = 1_700_000_000;
    const PULL: &str = "gid://gitlab/MergeRequest/4242";
    const HEAD: &str = "1111111111111111111111111111111111111111";
    const PATH: &str = "src/cache.rs";
    /// `sha1("src/cache.rs")` in hex.
    const PATH_SHA: &str = "43da4c419a4c25fe29f872dea1052a4686b978e4";
    const SECRET: &str = "Looks \"fine\" --hostname=evil acme/widgets";

    fn rows() -> Vec<DiffLine> {
        let mut classifier = kika_git::diff::Classifier::new(kika_git::diff::EmojiZwj::Escape);
        [
            "diff --git a/src/cache.rs b/src/cache.rs",
            "index 1111111..2222222 100644",
            "--- a/src/cache.rs",
            "+++ b/src/cache.rs",
            "@@ -10,3 +10,3 @@ fn get() {",
            " keep",
            "-old",
            "+new",
            " tail",
        ]
        .iter()
        .map(|t| classifier.classify(t.as_bytes()))
        .collect()
    }

    /// Row `row` of the sample diff as the app keeps it: its line, as the
    /// app names it, and its counters.
    fn at(row: usize) -> (GhLine, RowPos) {
        let lines = rows();
        let (old, new) = row_numbers(&lines)[row];
        let (o, n) = row_positions(&lines)[row].unwrap();
        let line = match new {
            Some(line) => GhLine {
                side: GhSide::Right,
                line,
            },
            None => GhLine {
                side: GhSide::Left,
                line: old.unwrap(),
            },
        };
        let pos = RowPos {
            old: o,
            new: n,
            both: old.is_some() && new.is_some(),
        };
        (line, pos)
    }

    const CONTEXT: usize = 5;
    const REMOVED: usize = 6;
    const ADDED: usize = 7;
    const TAIL: usize = 8;

    fn single(id: u64, row: usize) -> PendingComment {
        let (end, end_pos) = at(row);
        PendingComment {
            id,
            path: PATH.into(),
            start: None,
            end,
            body: format!("{SECRET} {id}"),
            head: HEAD.into(),
            start_pos: None,
            end_pos: Some(end_pos),
        }
    }

    fn range(id: u64, first: usize, last: usize) -> PendingComment {
        let (start, start_pos) = at(first);
        PendingComment {
            start: Some(start),
            start_pos: Some(start_pos),
            ..single(id, last)
        }
    }

    fn refs() -> DiffRefs {
        DiffRefs {
            base: "3".repeat(40),
            start: "2".repeat(40),
            head: HEAD.into(),
        }
    }

    fn base_position() -> Value {
        json!({
            "position_type": "text",
            "base_sha": "3".repeat(40),
            "start_sha": "2".repeat(40),
            "head_sha": HEAD,
            "old_path": PATH,
            "new_path": PATH,
        })
    }

    #[test]
    fn each_kind_of_line_gets_its_own_position() {
        let with = |extra: Value| {
            let mut want = base_position();
            for (k, v) in extra.as_object().unwrap() {
                want[k] = v.clone();
            }
            want
        };
        // An added row: its new line only.
        assert_eq!(
            draft_position(&single(1, ADDED), &refs()),
            Some(with(json!({ "new_line": 11 })))
        );
        // A removed row: its old line only.
        assert_eq!(
            draft_position(&single(1, REMOVED), &refs()),
            Some(with(json!({ "old_line": 11 })))
        );
        // A context row: both.
        assert_eq!(
            draft_position(&single(1, CONTEXT), &refs()),
            Some(with(json!({ "old_line": 10, "new_line": 10 })))
        );
        // A range from a removed row to a context row, each end with its
        // line code from both counters.
        assert_eq!(
            draft_position(&range(1, REMOVED, TAIL), &refs()),
            Some(with(json!({
                "old_line": 12,
                "new_line": 12,
                "line_range": {
                    "start": {
                        "line_code": format!("{PATH_SHA}_11_11"),
                        "type": "old",
                        "old_line": 11,
                        "new_line": null,
                    },
                    "end": {
                        "line_code": format!("{PATH_SHA}_12_12"),
                        "type": null,
                        "old_line": 12,
                        "new_line": 12,
                    },
                },
            })))
        );
        // An added row's old counter is the next old line.
        let added = range(1, ADDED, TAIL);
        let position = draft_position(&added, &refs()).unwrap();
        assert_eq!(
            position["line_range"]["start"],
            json!({
                "line_code": format!("{PATH_SHA}_12_11"),
                "type": "new",
                "old_line": null,
                "new_line": 11,
            })
        );
        // Written before positions were kept: none.
        let old = PendingComment {
            end_pos: None,
            ..single(1, ADDED)
        };
        assert_eq!(draft_position(&old, &refs()), None);
        let half = PendingComment {
            start_pos: None,
            ..range(1, REMOVED, TAIL)
        };
        assert_eq!(draft_position(&half, &refs()), None);
    }

    fn submit(
        runner: &FakeGlab,
        event: ReviewEvent,
        body: &str,
        threads: &[PendingComment],
    ) -> Result<Written, CreateError> {
        let sink = CommandSink::new();
        let host = ForgeHost::parse("gitlab.example.com")
            .unwrap()
            .with_kind(ForgeKind::GitLab);
        Glab {
            runner,
            sink: &sink,
            host: &host,
            cwd: "/cache".into(),
            now: NOW,
        }
        .submit_review(PULL, HEAD, event, body, threads)
    }

    fn argv(cmd: &GlabCommand) -> Vec<String> {
        cmd.args()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// Each call's method and path, or `graphql`.
    fn shape(calls: &[GlabCommand]) -> Vec<String> {
        calls
            .iter()
            .map(|cmd| match &cmd.kind {
                GlabCall::Rest { method, path, .. } => format!("{method:?} {}", path.render()),
                GlabCall::Graphql { .. } => "graphql".into(),
                GlabCall::Version => "version".into(),
            })
            .collect()
    }

    fn sent(cmd: &GlabCommand) -> Value {
        serde_json::from_slice(&cmd.stdin()).unwrap()
    }

    const MR: &str = "projects/17/merge_requests/7";

    fn two_comments() -> Vec<PendingComment> {
        vec![single(1, ADDED), range(2, REMOVED, TAIL)]
    }

    /// A FakeGlab that answers the refs and an empty draft list.
    fn ready() -> FakeGlab {
        FakeGlab::new()
            .reply(answer(REFS))
            .reply(answer(DRAFTS_EMPTY))
    }

    #[test]
    fn a_comment_review_drafts_each_comment_and_the_summary_then_publishes_once() {
        let runner = ready()
            .reply(answer(&created(101)))
            .reply(answer(&created(102)))
            .reply(answer(&created(103)))
            .reply(answer(NO_CONTENT));
        let threads = two_comments();
        assert_eq!(
            submit(&runner, ReviewEvent::Comment, "Summary: café ☕", &threads),
            Ok(Written::Posted)
        );
        let calls = runner.calls();
        assert_eq!(
            shape(&calls),
            [
                "graphql".to_string(),
                format!("Get {MR}/draft_notes"),
                format!("Post {MR}/draft_notes"),
                format!("Post {MR}/draft_notes"),
                format!("Post {MR}/draft_notes"),
                format!("Post {MR}/draft_notes/bulk_publish"),
            ]
        );
        // The refs are asked by the merge request's id, on stdin.
        let refs = sent(&calls[0]);
        assert_eq!(refs["query"], REFS_QUERY);
        assert_eq!(refs["variables"], json!({ "id": PULL }));
        // Each comment with its position, then the summary with none.
        assert_eq!(
            sent(&calls[2]),
            json!({
                "note": threads[0].body,
                "position": draft_position(&threads[0], &super::tests::refs()).unwrap(),
            })
        );
        assert_eq!(sent(&calls[3])["note"], threads[1].body.as_str());
        assert!(sent(&calls[3])["position"]["line_range"].is_object());
        assert_eq!(sent(&calls[4]), json!({ "note": "Summary: café ☕" }));
        // No comment, path or project name in argv.
        for cmd in &calls {
            for arg in argv(cmd) {
                for secret in ["evil", "acme", "widgets", "fine", "café", PATH, PULL] {
                    assert!(!arg.contains(secret), "{arg}");
                }
            }
            assert!(!cmd.display_line().contains("acme"));
        }
    }

    #[test]
    fn an_approval_follows_the_publish_with_the_head_on_stdin() {
        let runner = ready()
            .reply(answer(&created(101)))
            .reply(answer(NO_CONTENT))
            .reply(answer(APPROVED));
        assert_eq!(
            submit(&runner, ReviewEvent::Approve, "", &[single(1, CONTEXT)]),
            Ok(Written::Posted)
        );
        let calls = runner.calls();
        assert_eq!(shape(&calls).last().unwrap(), &format!("Post {MR}/approve"));
        assert_eq!(calls.len(), 5);
        assert_eq!(sent(&calls[4]), json!({ "sha": HEAD }));
        // An approval alone makes no draft, publishes nothing, and so
        // does not mind drafts made elsewhere.
        let runner = FakeGlab::new().reply(answer(REFS)).reply(answer(APPROVED));
        assert_eq!(
            submit(&runner, ReviewEvent::Approve, " ", &[]),
            Ok(Written::Posted)
        );
        assert_eq!(
            shape(&runner.calls()),
            ["graphql".to_string(), format!("Post {MR}/approve")]
        );
    }

    #[test]
    fn a_failed_draft_deletes_the_ones_made_and_answers_the_error() {
        let runner = ready()
            .reply(answer(&created(101)))
            .reply(answer(CONFLICT))
            .reply(answer(NO_CONTENT));
        let got = submit(&runner, ReviewEvent::Comment, "", &two_comments());
        let Err(CreateError::Refused(why)) = got else {
            panic!("{got:?}");
        };
        assert!(why.starts_with("SHA "), "{why}");
        assert!(!why.chars().any(char::is_control), "{why}");
        assert_eq!(
            shape(&runner.calls())[2..],
            [
                format!("Post {MR}/draft_notes"),
                format!("Post {MR}/draft_notes"),
                format!("Delete {MR}/draft_notes/101"),
            ]
        );
    }

    #[test]
    fn a_failed_publish_deletes_every_draft_made() {
        let runner = ready()
            .reply(answer(&created(101)))
            .reply(answer(&created(102)))
            .reply(answer(&created(103)))
            .reply(answer(UNAUTHORIZED_401))
            // A delete that fails too is let go.
            .reply(answer(NO_CONTENT))
            .reply(answer(UNAUTHORIZED_401))
            .reply(answer(NO_CONTENT));
        assert_eq!(
            submit(&runner, ReviewEvent::Approve, "Ship it", &two_comments()),
            Err(CreateError::Status(ForgeStatus::NotLoggedIn))
        );
        assert_eq!(
            shape(&runner.calls())[5..],
            [
                format!("Post {MR}/draft_notes/bulk_publish"),
                format!("Delete {MR}/draft_notes/101"),
                format!("Delete {MR}/draft_notes/102"),
                format!("Delete {MR}/draft_notes/103"),
            ]
        );
    }

    #[test]
    fn an_approval_refused_after_the_publish_is_posted_without_it() {
        let runner = ready()
            .reply(answer(&created(101)))
            .reply(answer(NO_CONTENT))
            .reply(answer(CONFLICT));
        assert_eq!(
            submit(&runner, ReviewEvent::Approve, "", &[single(1, ADDED)]),
            Ok(Written::PostedApprovalFailed)
        );
        assert_eq!(runner.calls().len(), 5);
        // With nothing published, the refusal is the answer.
        let runner = FakeGlab::new().reply(answer(REFS)).reply(answer(CONFLICT));
        let got = submit(&runner, ReviewEvent::Approve, "", &[]);
        assert!(matches!(got, Err(CreateError::Refused(_))), "{got:?}");
    }

    #[test]
    fn each_refusal_comes_before_any_draft() {
        let posts = |runner: &FakeGlab| {
            shape(&runner.calls())
                .iter()
                .filter(|s| s.starts_with("Post"))
                .count()
        };
        // The head moved.
        let runner = FakeGlab::new().reply(answer(REFS_MOVED));
        assert_eq!(
            submit(&runner, ReviewEvent::Comment, "", &two_comments()),
            Err(CreateError::Refused(MOVED.into()))
        );
        assert_eq!(runner.calls().len(), 1);
        // Drafts the reviewer made elsewhere.
        let runner = FakeGlab::new()
            .reply(answer(REFS))
            .reply(answer(DRAFTS_SOME));
        assert_eq!(
            submit(&runner, ReviewEvent::Comment, "", &two_comments()),
            Err(CreateError::Refused(
                "You have draft notes on !7 from elsewhere; publish or delete them on GitLab first"
                    .into()
            ))
        );
        assert_eq!(posts(&runner), 0);
        // A comment written before positions were kept: no call at all.
        let runner = FakeGlab::new();
        let old = PendingComment {
            end_pos: None,
            ..single(1, ADDED)
        };
        assert_eq!(
            submit(&runner, ReviewEvent::Comment, "", &[old]),
            Err(CreateError::Refused(NO_POSITION.into()))
        );
        let half = PendingComment {
            start_pos: None,
            ..range(2, REMOVED, TAIL)
        };
        assert_eq!(
            submit(&runner, ReviewEvent::Comment, "", &[half]),
            Err(CreateError::Refused(NO_POSITION.into()))
        );
        assert_eq!(runner.calls(), []);
        // A bad merge request id, head or review: no call either.
        let sink = CommandSink::new();
        let host = ForgeHost::parse_gitlab("gitlab.example.com").unwrap();
        let glab = Glab {
            runner: &runner,
            sink: &sink,
            host: &host,
            cwd: "/cache".into(),
            now: NOW,
        };
        let failed = Err(CreateError::Status(ForgeStatus::Failed));
        for pull in [
            "PR_kwDOA1",
            "gid://gitlab/Note/1",
            "gid://gitlab/MergeRequest/1 2",
            "",
        ] {
            assert_eq!(
                glab.submit_review(pull, HEAD, ReviewEvent::Approve, "", &[]),
                failed,
                "{pull}"
            );
        }
        assert_eq!(
            glab.submit_review(PULL, "--hostname", ReviewEvent::Approve, "", &[]),
            failed
        );
        assert_eq!(
            glab.submit_review(PULL, HEAD, ReviewEvent::Comment, " ", &[]),
            failed
        );
        assert!(matches!(
            glab.submit_review(PULL, HEAD, ReviewEvent::RequestChanges, "No", &[]),
            Err(CreateError::Refused(_))
        ));
        assert_eq!(runner.calls(), []);
    }

    #[test]
    fn logins_limits_and_the_network_map_as_the_reads_do() {
        let cases = [
            (
                answer(UNAUTHORIZED_401),
                CreateError::Status(ForgeStatus::NotLoggedIn),
            ),
            (
                answer(RATE_LIMITED_429),
                CreateError::RateLimited { until: NOW + 1200 },
            ),
            (
                crate::github::failure(
                    1,
                    "Post \"https://gitlab.example.com/api/graphql\": dial tcp: lookup gitlab.example.com: no such host",
                ),
                CreateError::Status(ForgeStatus::Offline),
            ),
        ];
        for (reply, want) in cases {
            // On the refs query, and on the first draft.
            let runner = FakeGlab::new().reply(reply.clone());
            let got = submit(&runner, ReviewEvent::Comment, "", &two_comments());
            assert_eq!(got, Err(want.clone()));
            let runner = ready().reply(reply);
            let got = submit(&runner, ReviewEvent::Comment, "", &two_comments());
            assert_eq!(got, Err(want));
            assert_eq!(runner.calls().len(), 3);
        }
        // A null merge request, or a project id that is not decimal.
        let null = "HTTP/2.0 200 OK\r\n\r\n{\"data\":{\"mergeRequest\":null}}";
        let odd = REFS.replace("gid://gitlab/Project/17", "gid://gitlab/Project/-17");
        for body in [null.to_string(), odd] {
            let runner = FakeGlab::new().reply(answer(&body));
            assert_eq!(
                submit(&runner, ReviewEvent::Comment, "", &two_comments()),
                Err(CreateError::Status(ForgeStatus::Failed))
            );
        }
    }
}
