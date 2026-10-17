//! Writing to a merge request's discussion on GitLab: a reply to a
//! discussion, resolving or unresolving one, and a top-level comment.
//! Each is one GraphQL mutation of fixed text; the ids and the body
//! travel only as variables on stdin, so argv is the fixed
//! `glab api graphql` line whatever is written.

use serde::Deserialize;
use serde_json::{Value, json};

use super::discussion_write::{DiscussionWrite, Written};
use super::github::refusal_text;
use super::gitlab::Glab;
use super::gitlab_discussion::{gitlab_id_ok, merge_request_id_ok};
use super::pending::MAX_BODY;
use super::{CreateError, ForgeStatus};

/// A note in an existing discussion. GitLab asks for the noteable even
/// then, so the merge request's id goes too.
const REPLY: &str = "mutation($noteable:NoteableID!,$discussion:DiscussionID!,$body:String!)\
{createNote(input:{noteableId:$noteable,discussionId:$discussion,body:$body}){note{id} errors}}";
/// A note that starts its own discussion on the merge request.
const COMMENT: &str = "mutation($noteable:NoteableID!,$body:String!)\
{createNote(input:{noteableId:$noteable,body:$body}){note{id} errors}}";
const RESOLVE: &str = "mutation($id:DiscussionID!,$resolve:Boolean!)\
{discussionToggleResolve(input:{id:$id,resolve:$resolve}){discussion{resolved} errors}}";

/// Whether `id` is a discussion's GitLab global id, as a reply or a
/// resolve names its thread.
fn discussion_id_ok(id: &str) -> bool {
    gitlab_id_ok(id) && id.starts_with("gid://gitlab/Discussion/")
}

fn body_ok(body: &str) -> bool {
    !body.trim().is_empty() && body.len() <= MAX_BODY
}

#[derive(Deserialize)]
struct Response {
    data: Option<serde_json::Map<String, Value>>,
    errors: Option<Vec<Value>>,
}

/// GitLab's own words, terminal-safe and cut short.
fn refused(text: &str) -> CreateError {
    CreateError::Refused(refusal_text(text))
}

impl Glab<'_> {
    /// Send a reply, a resolve or a comment to GitLab, once. Nothing is
    /// sent for an id GitLab would not have made, or for a blank or
    /// over-long body. A whole review is not a note: it answers
    /// `Status(Failed)` here, as `submit_review` sends it.
    pub(crate) fn write_note(&self, write: &DiscussionWrite) -> Result<Written, CreateError> {
        let failed = CreateError::Status(ForgeStatus::Failed);
        let (request, field) = match write {
            DiscussionWrite::Reply {
                thread,
                subject,
                body,
            } if merge_request_id_ok(subject) && discussion_id_ok(thread) && body_ok(body) => (
                json!({
                    "query": REPLY,
                    "variables": { "noteable": subject, "discussion": thread, "body": body },
                }),
                "createNote",
            ),
            DiscussionWrite::Comment { subject, body }
                if merge_request_id_ok(subject) && body_ok(body) =>
            {
                (
                    json!({
                        "query": COMMENT,
                        "variables": { "noteable": subject, "body": body },
                    }),
                    "createNote",
                )
            }
            DiscussionWrite::Resolve { thread, resolved } if discussion_id_ok(thread) => (
                json!({
                    "query": RESOLVE,
                    "variables": { "id": thread, "resolve": resolved },
                }),
                "discussionToggleResolve",
            ),
            _ => return Err(failed),
        };
        let body = self.mutate(serde_json::to_vec(&request).unwrap_or_default())?;
        read_note(&body, field, write)
    }
}

/// What a 2xx answer came to. GraphQL's errors, at the top or in the
/// payload, keep GitLab's first message; otherwise only the answer this
/// write asks for counts.
fn read_note(body: &[u8], field: &str, write: &DiscussionWrite) -> Result<Written, CreateError> {
    let failed = CreateError::Status(ForgeStatus::Failed);
    let response: Response = serde_json::from_slice(body).map_err(|_| failed.clone())?;
    if let Some(errors) = response.errors.filter(|e| !e.is_empty()) {
        return Err(errors
            .first()
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .map_or(failed, refused));
    }
    let payload = response
        .data
        .as_ref()
        .and_then(|data| data.get(field))
        .filter(|p| p.is_object())
        .ok_or_else(|| failed.clone())?;
    let errors = payload.get("errors").and_then(Value::as_array);
    if let Some(first) = errors.and_then(|e| e.first()) {
        return Err(first.as_str().map_or(failed, refused));
    }
    let written = match write {
        DiscussionWrite::Resolve { resolved, .. } => payload
            .get("discussion")
            .and_then(|d| d.get("resolved"))
            .and_then(Value::as_bool)
            // GitLab must report the state that was asked for.
            .filter(|now| now == resolved)
            .map(Written::Resolved),
        _ => payload
            .get("note")
            .and_then(|n| n.get("id"))
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(|_| Written::Posted),
    };
    written.ok_or(failed)
}

/// Hand-written answers in the shape GitLab gives with `--include`.
#[cfg(any(test, feature = "test-support"))]
pub mod fixtures {
    pub const NOTE_CREATED: &str = include_str!("fixtures/gitlab_note_created.http");
    pub const NOTE_ERRORS: &str = include_str!("fixtures/gitlab_note_errors.http");
    pub const RESOLVED: &str = include_str!("fixtures/gitlab_discussion_resolved.http");
    pub const UNRESOLVED: &str = include_str!("fixtures/gitlab_discussion_unresolved.http");
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::fixtures::*;
    use super::*;
    use crate::ForgeHost;
    use crate::github::{MAX_REFUSAL, answer};
    use crate::gitlab::FakeGlab;
    use crate::gitlab::fixtures::{GRAPHQL_ERRORS, RATE_LIMITED_429, UNAUTHORIZED_401};
    use crate::url::ForgeKind;
    use kika_git::recording::CommandSink;

    const NOW: i64 = 1_700_000_000;
    const MR: &str = "gid://gitlab/MergeRequest/4242";
    const THREAD: &str = "gid://gitlab/Discussion/1111111111111111111111111111111111111111";
    const BODY: &str = "Looks \"fine\" --hostname=evil acme/widgets café";

    fn reply() -> DiscussionWrite {
        DiscussionWrite::Reply {
            thread: THREAD.into(),
            subject: MR.into(),
            body: BODY.into(),
        }
    }

    fn comment() -> DiscussionWrite {
        DiscussionWrite::Comment {
            subject: MR.into(),
            body: BODY.into(),
        }
    }

    fn resolve(resolved: bool) -> DiscussionWrite {
        DiscussionWrite::Resolve {
            thread: THREAD.into(),
            resolved,
        }
    }

    fn write(runner: &FakeGlab, w: &DiscussionWrite) -> Result<Written, CreateError> {
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
        .write_note(w)
    }

    fn failed() -> Result<Written, CreateError> {
        Err(CreateError::Status(ForgeStatus::Failed))
    }

    fn ok(json: &str) -> String {
        format!("HTTP/2.0 200 OK\r\nContent-Type: application/json\r\n\r\n{json}")
    }

    #[test]
    fn each_write_is_one_fixed_call_with_its_ids_and_body_on_stdin_only() {
        let cases = [
            (
                reply(),
                NOTE_CREATED,
                Written::Posted,
                REPLY,
                json!({ "noteable": MR, "discussion": THREAD, "body": BODY }),
            ),
            (
                comment(),
                NOTE_CREATED,
                Written::Posted,
                COMMENT,
                json!({ "noteable": MR, "body": BODY }),
            ),
            (
                resolve(true),
                RESOLVED,
                Written::Resolved(true),
                RESOLVE,
                json!({ "id": THREAD, "resolve": true }),
            ),
            (
                resolve(false),
                UNRESOLVED,
                Written::Resolved(false),
                RESOLVE,
                json!({ "id": THREAD, "resolve": false }),
            ),
        ];
        for (w, reply, want, query, variables) in cases {
            let runner = FakeGlab::new().reply(answer(reply));
            assert_eq!(write(&runner, &w), Ok(want), "{w:?}");
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
                .map(OsString::from),
                "{w:?}"
            );
            assert_eq!(cmd.cwd, std::path::PathBuf::from("/cache"));
            let sent: Value = serde_json::from_slice(&cmd.stdin()).unwrap();
            assert_eq!(sent["query"], query, "{w:?}");
            assert_eq!(sent["variables"], variables, "{w:?}");
            let argv = format!("{:?}", cmd.args());
            for secret in [MR, THREAD, "fine", "evil", "café", "1111"] {
                assert!(!argv.contains(secret), "{argv}");
                assert!(!query.contains(secret), "{query}");
            }
        }
        // The comment mutation names no discussion.
        assert!(!COMMENT.contains("discussion"), "{COMMENT}");
    }

    #[test]
    fn a_bad_id_or_a_blank_or_long_body_makes_no_call() {
        let runner = FakeGlab::new();
        let long = "a".repeat(MAX_BODY + 1);
        let note = "gid://gitlab/DiffNote/1";
        let cases = [
            // The subject is not a merge request.
            DiscussionWrite::Reply {
                thread: THREAD.into(),
                subject: "gid://gitlab/Issue/1".into(),
                body: BODY.into(),
            },
            DiscussionWrite::Reply {
                thread: THREAD.into(),
                subject: "PR_kwDOA1".into(),
                body: BODY.into(),
            },
            // The thread is not a discussion.
            DiscussionWrite::Reply {
                thread: note.into(),
                subject: MR.into(),
                body: BODY.into(),
            },
            DiscussionWrite::Reply {
                thread: "gid://gitlab/Discussion/1 --x".into(),
                subject: MR.into(),
                body: BODY.into(),
            },
            DiscussionWrite::Reply {
                thread: THREAD.into(),
                subject: MR.into(),
                body: " \n\t".into(),
            },
            DiscussionWrite::Reply {
                thread: THREAD.into(),
                subject: MR.into(),
                body: long.clone(),
            },
            DiscussionWrite::Comment {
                subject: "gid://gitlab/MergeRequest/x1".into(),
                body: BODY.into(),
            },
            DiscussionWrite::Comment {
                subject: MR.into(),
                body: String::new(),
            },
            DiscussionWrite::Comment {
                subject: MR.into(),
                body: long,
            },
            DiscussionWrite::Resolve {
                thread: note.into(),
                resolved: true,
            },
            DiscussionWrite::Resolve {
                thread: "gid://gitlab/Discussion/\u{1b}[31m".into(),
                resolved: false,
            },
            DiscussionWrite::SubmitReview {
                pull: MR.into(),
                commit: "1".repeat(40),
                event: crate::discussion_write::ReviewEvent::Comment,
                body: BODY.into(),
                threads: vec![],
            },
        ];
        for w in &cases {
            assert_eq!(write(&runner, w), failed(), "{w:?}");
        }
        assert_eq!(runner.calls(), []);
        // The longest body a note holds may still be sent.
        let runner = FakeGlab::new().reply(answer(NOTE_CREATED));
        let longest = DiscussionWrite::Comment {
            subject: MR.into(),
            body: "a".repeat(MAX_BODY),
        };
        assert_eq!(write(&runner, &longest), Ok(Written::Posted));
    }

    #[test]
    fn gitlabs_errors_are_refusals_in_terminal_safe_words() {
        for w in [reply(), comment(), resolve(true)] {
            let runner = FakeGlab::new().reply(answer(&NOTE_ERRORS.replace(
                "createNote",
                match w {
                    DiscussionWrite::Resolve { .. } => "discussionToggleResolve",
                    _ => "createNote",
                },
            )));
            let Err(CreateError::Refused(text)) = write(&runner, &w) else {
                panic!("expected a refusal for {w:?}")
            };
            assert!(!text.chars().any(char::is_control), "{text:?}");
            assert!(text.starts_with("Note "), "{text}");
            assert!(text.contains("can't be blank"), "{text}");
            assert!(!text.contains("Second"), "only the first: {text}");
        }
        // Top-level GraphQL errors keep their first message.
        let runner = FakeGlab::new().reply(answer(GRAPHQL_ERRORS));
        assert_eq!(
            write(&runner, &reply()),
            Err(CreateError::Refused(
                "Field 'mergeRequestz' doesn't exist on type 'Project'".into()
            ))
        );
        // A long one is cut short.
        let long = json!({ "errors": [{ "message": "x".repeat(500) }] });
        let runner = FakeGlab::new().reply(answer(&ok(&long.to_string())));
        let Err(CreateError::Refused(text)) = write(&runner, &comment()) else {
            panic!("expected a refusal")
        };
        assert_eq!(text.chars().count(), MAX_REFUSAL + 1, "{text}");
        // An error with no message says nothing usable.
        let bare = "{\"errors\":[{\"extensions\":{}}]}";
        let runner = FakeGlab::new().reply(answer(&ok(bare)));
        assert_eq!(write(&runner, &comment()), failed());
    }

    #[test]
    fn a_missing_answer_or_the_wrong_state_is_a_failure() {
        let cases = [
            (reply(), "{\"data\":{}}"),
            (reply(), "{\"data\":null}"),
            (reply(), "{\"data\":{\"createNote\":null}}"),
            (
                reply(),
                "{\"data\":{\"createNote\":{\"note\":null,\"errors\":[]}}}",
            ),
            (
                comment(),
                "{\"data\":{\"createNote\":{\"note\":{},\"errors\":[]}}}",
            ),
            (
                comment(),
                "{\"data\":{\"createNote\":{\"note\":{\"id\":\"\"},\"errors\":[]}}}",
            ),
            // The answer to another mutation is not this one's.
            (comment(), RESOLVED),
            (resolve(true), NOTE_CREATED),
            // GitLab says the discussion is not in the state asked for.
            (resolve(true), UNRESOLVED),
            (resolve(false), RESOLVED),
            (
                resolve(true),
                "{\"data\":{\"discussionToggleResolve\":{\"discussion\":{\"resolved\":\"yes\"}}}}",
            ),
            (comment(), "not json"),
        ];
        for (w, body) in cases {
            let reply = match body.starts_with("HTTP/") {
                true => body.to_string(),
                false => ok(body),
            };
            let runner = FakeGlab::new().reply(answer(&reply));
            assert_eq!(write(&runner, &w), failed(), "{body}");
        }
        for reply in [
            "HTTP/2.0 500 Oops\r\n\r\n{}",
            "HTTP/2.0 302 Found\r\n\r\n{\"data\":{\"createNote\":{\"note\":{\"id\":\"gid://gitlab/Note/1\"}}}}",
        ] {
            let runner = FakeGlab::new().reply(answer(reply));
            assert_eq!(write(&runner, &comment()), failed(), "{reply}");
        }
    }

    #[test]
    fn logins_limits_and_the_network_map_as_the_reads_do() {
        for w in [reply(), comment(), resolve(false)] {
            let cases = [
                (
                    FakeGlab::new().reply(answer(UNAUTHORIZED_401)),
                    CreateError::Status(ForgeStatus::NotLoggedIn),
                ),
                (
                    FakeGlab::new().reply(answer(RATE_LIMITED_429)),
                    CreateError::RateLimited { until: NOW + 1200 },
                ),
                (
                    FakeGlab::new().reply(crate::github::failure(
                        1,
                        "Post \"https://gitlab.example.com/api/graphql\": dial tcp: lookup gitlab.example.com: no such host",
                    )),
                    CreateError::Status(ForgeStatus::Offline),
                ),
                (
                    FakeGlab::new().missing(),
                    CreateError::Status(ForgeStatus::Cli(crate::cli::CliStatus::NotInstalled)),
                ),
            ];
            for (runner, want) in cases {
                assert_eq!(write(&runner, &w), Err(want), "{w:?}");
                // Nothing is tried again.
                assert_eq!(runner.calls().len(), 1);
            }
        }
    }
}
