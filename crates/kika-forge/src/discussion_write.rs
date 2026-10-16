//! Writing to a pull request's discussion on GitHub: a reply to a review
//! thread, resolving or unresolving one, a general comment, and a whole
//! review with its line comments, submitted in one step. Each is
//! one GraphQL mutation of fixed text; the ids and the body travel only
//! as variables on stdin, so argv is the fixed `gh api graphql` line
//! whatever is written.

use std::io;

use serde::Deserialize;
use serde_json::{Value, json};

use super::anchor::{GhLine, GhSide};
use super::cli::CliStatus;
use super::github::{Answer, Gh, GhCall, GhOutput, classify, refusal, split_include};
use super::pending::{MAX_BODY, MAX_COMMENTS, PendingComment};
use super::review::node_id_ok;
use super::{CreateError, ForgeStatus};
use kika_git::recording::CommandOutcome;

const REPLY: &str = "mutation($id:ID!,$body:String!)\
{addPullRequestReviewThreadReply(input:{pullRequestReviewThreadId:$id,body:$body}){comment{id}}}";
const RESOLVE: &str =
    "mutation($id:ID!){resolveReviewThread(input:{threadId:$id}){thread{isResolved}}}";
const UNRESOLVE: &str =
    "mutation($id:ID!){unresolveReviewThread(input:{threadId:$id}){thread{isResolved}}}";
const COMMENT: &str = "mutation($id:ID!,$body:String!)\
{addComment(input:{subjectId:$id,body:$body}){commentEdge{node{id}}}}";
// One round trip: GitHub creates the review, its threads and submits it
// together, so no half-made review is left behind.
const SUBMIT: &str = "mutation($id:ID!,$commit:GitObjectID!,$event:PullRequestReviewEvent!,\
$body:String,$threads:[DraftPullRequestReviewThread])\
{addPullRequestReview(input:{pullRequestId:$id,commitOID:$commit,event:$event,\
body:$body,threads:$threads}){pullRequestReview{id state}}}";

/// How a submitted review judges the pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewEvent {
    Approve,
    RequestChanges,
    Comment,
}

impl ReviewEvent {
    /// The name GitHub's `PullRequestReviewEvent` gives it.
    fn github(self) -> &'static str {
        match self {
            ReviewEvent::Approve => "APPROVE",
            ReviewEvent::RequestChanges => "REQUEST_CHANGES",
            ReviewEvent::Comment => "COMMENT",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ReviewEvent::Approve => "Approve",
            ReviewEvent::RequestChanges => "Request changes",
            ReviewEvent::Comment => "Comment",
        }
    }
}

/// One write the user asked for. `thread` is a review thread's node id
/// and `subject` the pull request's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscussionWrite {
    Reply {
        thread: String,
        body: String,
    },
    Resolve {
        thread: String,
        resolved: bool,
    },
    Comment {
        subject: String,
        body: String,
    },
    /// The pending review: `pull` is the pull request's node id, `commit`
    /// the hex head the comments were written against.
    SubmitReview {
        pull: String,
        commit: String,
        event: ReviewEvent,
        body: String,
        threads: Vec<PendingComment>,
    },
}

/// The body rules GitHub has for each kind of review: Approve needs
/// nothing, Request changes a summary, and Comment a summary or a line
/// comment.
pub(crate) fn review_ok(event: ReviewEvent, body: &str, threads: &[PendingComment]) -> bool {
    let has_body = !body.trim().is_empty();
    let parts_ok = body.len() <= MAX_BODY
        && threads.len() <= MAX_COMMENTS
        && threads
            .iter()
            .all(|t| !t.body.trim().is_empty() && t.body.len() <= MAX_BODY);
    let event_ok = match event {
        ReviewEvent::Approve => true,
        ReviewEvent::RequestChanges => has_body,
        ReviewEvent::Comment => has_body || !threads.is_empty(),
    };
    parts_ok && event_ok
}

fn side(side: GhSide) -> &'static str {
    match side {
        GhSide::Left => "LEFT",
        GhSide::Right => "RIGHT",
    }
}

/// One line comment as GitHub's `DraftPullRequestReviewThread`.
fn draft_thread(comment: &PendingComment) -> Value {
    let GhLine { side: end, line } = comment.end;
    let mut thread = json!({
        "path": comment.path,
        "body": comment.body,
        "line": line,
        "side": side(end),
    });
    if let Some(start) = comment.start {
        thread["startLine"] = json!(start.line);
        thread["startSide"] = json!(side(start.side));
    }
    thread
}

/// What GitHub took: a posted reply or comment, or the thread's state
/// after a resolve, as GitHub reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Written {
    Posted,
    Resolved(bool),
    /// A review's comments were published, but the approval asked with
    /// them was not taken: the comments are public, so the pending review
    /// is done with, and the approval can be asked again on its own.
    PostedApprovalFailed,
}

impl DiscussionWrite {
    /// Whether the write may be sent at all: an id GitHub would have made,
    /// and a body that is neither blank nor longer than a comment holds. A
    /// review also needs a hex head, at most a pending review's comments,
    /// and the summary its kind asks for.
    fn sendable(&self) -> bool {
        let body_ok = |body: &str| !body.trim().is_empty() && body.len() <= MAX_BODY;
        match self {
            DiscussionWrite::Reply { thread, body } => node_id_ok(thread) && body_ok(body),
            DiscussionWrite::Resolve { thread, .. } => node_id_ok(thread),
            DiscussionWrite::Comment { subject, body } => node_id_ok(subject) && body_ok(body),
            DiscussionWrite::SubmitReview {
                pull,
                commit,
                event,
                body,
                threads,
            } => {
                let commit_ok = commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_hexdigit());
                node_id_ok(pull) && commit_ok && review_ok(*event, body, threads)
            }
        }
    }

    /// The request body sent on stdin, and the field its answer is under.
    fn request(&self) -> (Value, &'static str) {
        match self {
            DiscussionWrite::Reply { thread, body } => (
                json!({ "query": REPLY, "variables": { "id": thread, "body": body } }),
                "addPullRequestReviewThreadReply",
            ),
            DiscussionWrite::Resolve { thread, resolved } => {
                let (query, field) = match resolved {
                    true => (RESOLVE, "resolveReviewThread"),
                    false => (UNRESOLVE, "unresolveReviewThread"),
                };
                (
                    json!({ "query": query, "variables": { "id": thread } }),
                    field,
                )
            }
            DiscussionWrite::Comment { subject, body } => (
                json!({ "query": COMMENT, "variables": { "id": subject, "body": body } }),
                "addComment",
            ),
            DiscussionWrite::SubmitReview {
                pull,
                commit,
                event,
                body,
                threads,
            } => {
                // No summary is sent as none, not as an empty one.
                let body = Some(body).filter(|b| !b.trim().is_empty());
                let threads: Vec<Value> = threads.iter().map(draft_thread).collect();
                let variables = json!({
                    "id": pull,
                    "commit": commit,
                    "event": event.github(),
                    "body": body,
                    "threads": threads,
                });
                (
                    json!({ "query": SUBMIT, "variables": variables }),
                    "addPullRequestReview",
                )
            }
        }
    }

    /// What the answer object under the mutation's field says, or `None`
    /// when it is not the shape this write asks for.
    fn read_answer(&self, answer: &Value) -> Option<Written> {
        match self {
            DiscussionWrite::Reply { .. } => answer
                .get("comment")
                .filter(|c| c.is_object())
                .map(|_| Written::Posted),
            DiscussionWrite::Comment { .. } => answer
                .get("commentEdge")
                .and_then(|e| e.get("node"))
                .filter(|n| n.is_object())
                .map(|_| Written::Posted),
            DiscussionWrite::SubmitReview { .. } => answer
                .get("pullRequestReview")
                .filter(|r| r.is_object())
                .map(|_| Written::Posted),
            DiscussionWrite::Resolve { resolved, .. } => answer
                .get("thread")
                .and_then(|t| t.get("isResolved"))
                .and_then(Value::as_bool)
                // GitHub must report the state that was asked for.
                .filter(|now| now == resolved)
                .map(Written::Resolved),
        }
    }
}

impl Gh<'_> {
    /// Send `write` to GitHub, once. Nothing is sent for an id GitHub
    /// would not have made, or for a blank or over-long body.
    pub fn write_discussion(&self, write: &DiscussionWrite) -> Result<Written, CreateError> {
        if !write.sendable() {
            return Err(CreateError::Status(ForgeStatus::Failed));
        }
        let (body, field) = write.request();
        let kind = GhCall::Graphql {
            body: serde_json::to_vec(&body).unwrap_or_default(),
        };
        let now = self.now;
        self.run_logged(
            kind,
            |output| read_write(output, now, field, write),
            |result| match result {
                Ok(_) => CommandOutcome::Ok,
                Err(CreateError::Status(ForgeStatus::Cli(CliStatus::NotInstalled))) => {
                    CommandOutcome::NotStarted
                }
                Err(_) => CommandOutcome::Failed(None),
            },
        )
    }
}

#[derive(Deserialize)]
struct Response {
    data: Option<serde_json::Map<String, Value>>,
    errors: Option<Vec<Value>>,
}

/// What a write came to. Rate limits, logins and the network are judged
/// as for any call; only a 200 with no GraphQL errors whose `data` holds
/// the expected answer under `field` counts. GraphQL's errors keep their
/// message, terminal-safe and cut short.
fn read_write(
    output: io::Result<GhOutput>,
    now: i64,
    field: &str,
    write: &DiscussionWrite,
) -> Result<Written, CreateError> {
    let stdout = output
        .as_ref()
        .map(|o| o.stdout.clone())
        .unwrap_or_default();
    let (status, _, body) = split_include(&stdout);
    let failed = CreateError::Status(ForgeStatus::Failed);
    match classify(output, now) {
        Answer::Limited { until } => Err(CreateError::RateLimited { until }),
        Answer::Body { .. } if status == Some(200) => {
            let response: Response = serde_json::from_slice(body).map_err(|_| failed.clone())?;
            if response.errors.is_some_and(|e| !e.is_empty()) {
                return Err(refusal(body).map_or(failed, CreateError::Refused));
            }
            response
                .data
                .as_ref()
                .and_then(|data| data.get(field))
                .and_then(|answer| write.read_answer(answer))
                .ok_or(failed)
        }
        Answer::Failed(status) => Err(CreateError::Status(status)),
        Answer::Body { .. } | Answer::NotModified => Err(failed),
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;
    use crate::ForgeHost;
    use crate::github::fixtures::{
        DNS_STDERR, GRAPHQL_RATE_LIMITED, HTTP_401_STDERR, NOT_FOUND, NOT_FOUND_STDERR,
        TIMEOUT_STDERR,
    };
    use crate::github::{FakeGh, MAX_REFUSAL, answer, failure};
    use kika_git::recording::CommandSink;

    const NOW: i64 = 1_700_000_000;
    const THREAD: &str = "PRRT_kwDOA1";
    const PR: &str = "PR_kwDOA1";
    const BODY: &str = "Looks \"fine\"\nbut café ☕ needs a test";

    fn ok(json: &str) -> String {
        format!("HTTP/2.0 200 OK\r\nContent-Type: application/json\r\n\r\n{json}")
    }

    fn reply() -> DiscussionWrite {
        DiscussionWrite::Reply {
            thread: THREAD.into(),
            body: BODY.into(),
        }
    }

    fn resolve(resolved: bool) -> DiscussionWrite {
        DiscussionWrite::Resolve {
            thread: THREAD.into(),
            resolved,
        }
    }

    fn comment() -> DiscussionWrite {
        DiscussionWrite::Comment {
            subject: PR.into(),
            body: BODY.into(),
        }
    }

    const REPLIED: &str =
        "{\"data\":{\"addPullRequestReviewThreadReply\":{\"comment\":{\"id\":\"PRRC_1\"}}}}";
    const COMMENTED: &str =
        "{\"data\":{\"addComment\":{\"commentEdge\":{\"node\":{\"id\":\"IC_1\"}}}}}";

    fn resolved_answer(field: &str, now: bool) -> String {
        format!("{{\"data\":{{\"{field}\":{{\"thread\":{{\"isResolved\":{now}}}}}}}}}")
    }

    fn write(runner: &FakeGh, w: &DiscussionWrite) -> Result<Written, CreateError> {
        let host = ForgeHost::parse("github.com").unwrap();
        let sink = CommandSink::new();
        let gh = Gh {
            runner,
            sink: &sink,
            host: &host,
            cwd: "/cache".into(),
            now: NOW,
        };
        gh.write_discussion(w)
    }

    fn failed() -> Result<Written, CreateError> {
        Err(CreateError::Status(ForgeStatus::Failed))
    }

    #[test]
    fn each_write_is_one_fixed_call_with_its_ids_and_body_on_stdin_only() {
        let cases = [
            (
                reply(),
                ok(REPLIED),
                Written::Posted,
                "addPullRequestReviewThreadReply",
                json!({ "id": THREAD, "body": BODY }),
            ),
            (
                resolve(true),
                ok(&resolved_answer("resolveReviewThread", true)),
                Written::Resolved(true),
                "resolveReviewThread",
                json!({ "id": THREAD }),
            ),
            (
                resolve(false),
                ok(&resolved_answer("unresolveReviewThread", false)),
                Written::Resolved(false),
                "unresolveReviewThread",
                json!({ "id": THREAD }),
            ),
            (
                comment(),
                ok(COMMENTED),
                Written::Posted,
                "addComment",
                json!({ "id": PR, "body": BODY }),
            ),
        ];
        for (w, reply, want, name, variables) in cases {
            let runner = FakeGh::new().reply(answer(&reply));
            assert_eq!(write(&runner, &w), Ok(want), "{name}");
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
                .map(OsString::from),
                "{name}"
            );
            assert_eq!(cmd.cwd, std::path::PathBuf::from("/cache"));
            let argv = format!("{:?}", cmd.args());
            for secret in [THREAD, PR, "café", "fine"] {
                assert!(!argv.contains(secret), "{name}: {argv}");
            }
            let sent: Value = serde_json::from_slice(&cmd.stdin()).unwrap();
            assert_eq!(sent["variables"], variables, "{name}");
            let query = sent["query"].as_str().unwrap();
            assert!(query.contains(&format!("{{{name}(input:")), "{query}");
            for secret in [THREAD, PR, "café", "fine"] {
                assert!(!query.contains(secret), "{name}: {query}");
            }
        }
    }

    #[test]
    fn a_bad_id_or_a_blank_or_long_body_makes_no_call() {
        let runner = FakeGh::new();
        let long = "a".repeat(MAX_BODY + 1);
        let cases = [
            DiscussionWrite::Reply {
                thread: "PRRT kw".into(),
                body: BODY.into(),
            },
            DiscussionWrite::Reply {
                thread: THREAD.into(),
                body: " \n\t ".into(),
            },
            DiscussionWrite::Reply {
                thread: THREAD.into(),
                body: long.clone(),
            },
            DiscussionWrite::Resolve {
                thread: String::new(),
                resolved: true,
            },
            DiscussionWrite::Resolve {
                thread: "PRRT_\u{1b}[31m".into(),
                resolved: false,
            },
            DiscussionWrite::Comment {
                subject: "PR/1".into(),
                body: BODY.into(),
            },
            DiscussionWrite::Comment {
                subject: PR.into(),
                body: String::new(),
            },
            DiscussionWrite::Comment {
                subject: PR.into(),
                body: long,
            },
        ];
        for w in &cases {
            assert_eq!(write(&runner, w), failed(), "{w:?}");
        }
        assert_eq!(runner.calls(), []);
        // The longest body a comment holds may still be sent.
        let runner = FakeGh::new().reply(answer(&ok(COMMENTED)));
        let longest = DiscussionWrite::Comment {
            subject: PR.into(),
            body: "a".repeat(MAX_BODY),
        };
        assert_eq!(write(&runner, &longest), Ok(Written::Posted));
    }

    #[test]
    fn graphql_errors_a_missing_answer_or_the_wrong_state_are_failures() {
        let cases = [
            (
                reply(),
                "{\"data\":{\"addPullRequestReviewThreadReply\":null},\"errors\":[{\"type\":\"UNPROCESSABLE\"}]}",
            ),
            (reply(), "{\"data\":{}}"),
            (reply(), "{\"data\":null}"),
            (
                reply(),
                "{\"data\":{\"addPullRequestReviewThreadReply\":{\"comment\":null}}}",
            ),
            // The answer to another mutation is not this one's.
            (reply(), COMMENTED),
            (comment(), REPLIED),
            (
                comment(),
                "{\"data\":{\"addComment\":{\"commentEdge\":null}}}",
            ),
            (
                resolve(true),
                "{\"data\":{\"resolveReviewThread\":{\"thread\":{}}}}",
            ),
            (
                resolve(true),
                "{\"data\":{\"resolveReviewThread\":{\"thread\":{\"isResolved\":\"yes\"}}}}",
            ),
            // GitHub says the thread is not in the state asked for.
            (
                resolve(true),
                "{\"data\":{\"resolveReviewThread\":{\"thread\":{\"isResolved\":false}}}}",
            ),
            (
                resolve(false),
                "{\"data\":{\"unresolveReviewThread\":{\"thread\":{\"isResolved\":true}}}}",
            ),
            (
                resolve(false),
                "{\"data\":{\"resolveReviewThread\":{\"thread\":{\"isResolved\":false}}}}",
            ),
            (comment(), "not json"),
        ];
        for (w, body) in cases {
            let runner = FakeGh::new().reply(answer(&ok(body)));
            assert_eq!(write(&runner, &w), failed(), "{body}");
        }
        for reply in [
            "HTTP/2.0 500 Oops\r\n\r\n{}",
            "HTTP/2.0 502 Bad Gateway\r\n\r\nnot json",
            "HTTP/2.0 201 Created\r\n\r\n{\"data\":{\"addComment\":{\"commentEdge\":{\"node\":{}}}}}",
        ] {
            let runner = FakeGh::new().reply(answer(reply));
            assert_eq!(write(&runner, &comment()), failed(), "{reply}");
        }
    }

    #[test]
    fn githubs_reason_is_terminal_safe_and_cut_short() {
        let long = format!("Thread is locked\u{1b}]52;c;eA==\u{7}\r{}", "x".repeat(500));
        let body = json!({
            "data": { "addPullRequestReviewThreadReply": null },
            "errors": [{ "type": "UNPROCESSABLE", "message": long }],
        });
        let runner = FakeGh::new().reply(answer(&ok(&body.to_string())));
        let Err(CreateError::Refused(text)) = write(&runner, &reply()) else {
            panic!("expected a refusal")
        };
        assert!(!text.chars().any(char::is_control), "{text:?}");
        assert!(text.starts_with("Thread is locked"), "{text}");
        assert_eq!(text.chars().count(), MAX_REFUSAL + 1, "{text}");
    }

    const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
    const SUBMITTED: &str = "{\"data\":{\"addPullRequestReview\":\
{\"pullRequestReview\":{\"id\":\"PRR_1\",\"state\":\"APPROVED\"}}}}";

    fn line(side: GhSide, line: u32) -> GhLine {
        GhLine { side, line }
    }

    fn pending(id: u64, start: Option<GhLine>, end: GhLine) -> PendingComment {
        PendingComment {
            id,
            path: "src/café.rs".into(),
            start,
            end,
            body: format!("{BODY} {id}"),
            head: HEAD.into(),
            start_pos: None,
            end_pos: None,
        }
    }

    fn threads() -> Vec<PendingComment> {
        vec![
            pending(1, None, line(GhSide::Right, 4)),
            pending(2, Some(line(GhSide::Left, 10)), line(GhSide::Right, 12)),
        ]
    }

    fn submit(event: ReviewEvent, body: &str, threads: Vec<PendingComment>) -> DiscussionWrite {
        DiscussionWrite::SubmitReview {
            pull: PR.into(),
            commit: HEAD.into(),
            event,
            body: body.into(),
            threads,
        }
    }

    #[test]
    fn each_review_is_one_fixed_mutation_with_everything_on_stdin() {
        let events = [
            (ReviewEvent::Approve, "APPROVE"),
            (ReviewEvent::RequestChanges, "REQUEST_CHANGES"),
            (ReviewEvent::Comment, "COMMENT"),
        ];
        for (event, name) in events {
            let runner = FakeGh::new().reply(answer(&ok(SUBMITTED)));
            let w = submit(event, BODY, threads());
            assert_eq!(write(&runner, &w), Ok(Written::Posted), "{name}");
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
                .map(OsString::from),
                "{name}"
            );
            let sent: Value = serde_json::from_slice(&cmd.stdin()).unwrap();
            let want = json!({
                "id": PR,
                "commit": HEAD,
                "event": name,
                "body": BODY,
                "threads": [
                    {
                        "path": "src/café.rs",
                        "body": format!("{BODY} 1"),
                        "line": 4,
                        "side": "RIGHT",
                    },
                    {
                        "path": "src/café.rs",
                        "body": format!("{BODY} 2"),
                        "line": 12,
                        "side": "RIGHT",
                        "startLine": 10,
                        "startSide": "LEFT",
                    },
                ],
            });
            assert_eq!(sent["variables"], want, "{name}");
            let query = sent["query"].as_str().unwrap();
            assert_eq!(query, SUBMIT);
            assert!(query.contains("{addPullRequestReview(input:"), "{query}");
            let argv = format!("{:?}", cmd.args());
            for secret in [PR, HEAD, "café", "fine"] {
                assert!(!query.contains(secret), "{name}: {query}");
                assert!(!argv.contains(secret), "{name}: {argv}");
            }
        }
        // An approval may carry nothing; no summary goes as none.
        let runner = FakeGh::new().reply(answer(&ok(SUBMITTED)));
        let bare = submit(ReviewEvent::Approve, " \n", vec![]);
        assert_eq!(write(&runner, &bare), Ok(Written::Posted));
        let [cmd] = runner.calls().try_into().unwrap();
        let sent: Value = serde_json::from_slice(&cmd.stdin()).unwrap();
        assert_eq!(sent["variables"]["body"], Value::Null);
        assert_eq!(sent["variables"]["threads"], json!([]));
        // A comment review may be line comments alone.
        let runner = FakeGh::new().reply(answer(&ok(SUBMITTED)));
        let lines_only = submit(ReviewEvent::Comment, "", threads());
        assert_eq!(write(&runner, &lines_only), Ok(Written::Posted));
    }

    #[test]
    fn a_review_github_would_refuse_makes_no_call() {
        let runner = FakeGh::new();
        let long = "a".repeat(MAX_BODY + 1);
        let many: Vec<PendingComment> = (1..=MAX_COMMENTS as u64 + 1)
            .map(|id| pending(id, None, line(GhSide::Right, 1)))
            .collect();
        let blank_thread = vec![PendingComment {
            body: " \t".into(),
            ..pending(1, None, line(GhSide::Right, 1))
        }];
        let long_thread = vec![PendingComment {
            body: long.clone(),
            ..pending(1, None, line(GhSide::Right, 1))
        }];
        let with = |change: fn(&mut DiscussionWrite)| {
            let mut w = submit(ReviewEvent::Comment, BODY, threads());
            change(&mut w);
            w
        };
        let cases = [
            with(|w| {
                if let DiscussionWrite::SubmitReview { pull, .. } = w {
                    *pull = "PR kw".into();
                }
            }),
            with(|w| {
                if let DiscussionWrite::SubmitReview { commit, .. } = w {
                    *commit = "z".repeat(40);
                }
            }),
            with(|w| {
                if let DiscussionWrite::SubmitReview { commit, .. } = w {
                    *commit = "abc123".into();
                }
            }),
            submit(ReviewEvent::RequestChanges, " \n", threads()),
            submit(ReviewEvent::Comment, "", vec![]),
            submit(ReviewEvent::Approve, &long, vec![]),
            submit(ReviewEvent::Comment, BODY, many),
            submit(ReviewEvent::Comment, BODY, blank_thread),
            submit(ReviewEvent::Approve, "", long_thread),
        ];
        for w in &cases {
            assert_eq!(write(&runner, w), failed(), "{w:?}");
        }
        assert_eq!(runner.calls(), []);
    }

    #[test]
    fn a_review_answer_without_the_review_or_with_errors_is_not_posted() {
        let cases = [
            "{\"data\":{\"addPullRequestReview\":null},\"errors\":[{\"type\":\"UNPROCESSABLE\"}]}",
            "{\"data\":{\"addPullRequestReview\":{\"pullRequestReview\":null}}}",
            "{\"data\":{}}",
            COMMENTED,
        ];
        for body in cases {
            let runner = FakeGh::new().reply(answer(&ok(body)));
            let w = submit(ReviewEvent::Approve, "", threads());
            assert_eq!(write(&runner, &w), failed(), "{body}");
        }
        let refused = json!({
            "data": { "addPullRequestReview": null },
            "errors": [{ "message": "Can not approve your own pull request" }],
        });
        let runner = FakeGh::new().reply(answer(&ok(&refused.to_string())));
        let w = submit(ReviewEvent::Approve, "", threads());
        assert_eq!(
            write(&runner, &w),
            Err(CreateError::Refused(
                "Can not approve your own pull request".into()
            ))
        );
    }

    #[test]
    fn limits_logins_the_network_and_a_missing_gh_say_why() {
        for w in [
            reply(),
            resolve(true),
            comment(),
            submit(ReviewEvent::RequestChanges, BODY, threads()),
        ] {
            let runner = FakeGh::new().reply(answer(GRAPHQL_RATE_LIMITED));
            assert!(
                matches!(write(&runner, &w), Err(CreateError::RateLimited { .. })),
                "{w:?}"
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
                    FakeGh::new().reply(failure(1, TIMEOUT_STDERR)),
                    ForgeStatus::Offline,
                ),
                (
                    FakeGh::new().reply(GhOutput {
                        stdout: NOT_FOUND.into(),
                        ..failure(1, NOT_FOUND_STDERR)
                    }),
                    ForgeStatus::NoAccess,
                ),
            ];
            for (runner, want) in cases {
                assert_eq!(write(&runner, &w), Err(CreateError::Status(want)), "{w:?}");
                // Nothing is tried again.
                assert!(runner.calls().len() <= 1);
            }
        }
    }
}
