//! Marking a pull request's file viewed, or not viewed, on GitHub. One
//! GraphQL mutation of fixed text; the pull request's id and the path
//! travel only as variables on stdin, so argv is the fixed `gh api
//! graphql` line whatever the file is called.

use std::io;

use serde::Deserialize;
use serde_json::json;

use super::github::{Answer, Gh, GhCall, GhOutput, classify, refusal, split_include};
use super::review::{MAX_PATH, node_id_ok};
use super::{CreateError, ForgeStatus};
use kika_git::recording::CommandOutcome;

const MARK: &str = "mutation($id:ID!,$path:String!)\
{markFileAsViewed(input:{pullRequestId:$id,path:$path}){clientMutationId}}";
const UNMARK: &str = "mutation($id:ID!,$path:String!)\
{unmarkFileAsViewed(input:{pullRequestId:$id,path:$path}){clientMutationId}}";

impl Gh<'_> {
    /// Mark `path` of the pull request GitHub knows as `node_id` viewed,
    /// or not viewed. The mark is the user's, for the whole pull request.
    pub fn set_file_viewed(
        &self,
        node_id: &str,
        path: &str,
        viewed: bool,
    ) -> Result<(), CreateError> {
        // Nothing GitHub would not have made, and no path Git could not hold.
        if !node_id_ok(node_id) || path.is_empty() || path.len() > MAX_PATH {
            return Err(CreateError::Status(ForgeStatus::Failed));
        }
        let (query, field) = match viewed {
            true => (MARK, "markFileAsViewed"),
            false => (UNMARK, "unmarkFileAsViewed"),
        };
        let body = json!({ "query": query, "variables": { "id": node_id, "path": path } });
        let kind = GhCall::Graphql {
            body: serde_json::to_vec(&body).unwrap_or_default(),
        };
        let now = self.now;
        self.run_logged(
            kind,
            |output| read_set(output, now, field),
            |result| match result {
                Ok(()) => CommandOutcome::Ok,
                Err(CreateError::Status(ForgeStatus::NoGh)) => CommandOutcome::NotStarted,
                Err(_) => CommandOutcome::Failed(None),
            },
        )
    }
}

#[derive(Deserialize)]
struct Response {
    data: Option<serde_json::Map<String, serde_json::Value>>,
    errors: Option<Vec<serde_json::Value>>,
}

/// What a mark call came to. Rate limits, logins and the network are
/// judged as for any call; only a 200 with no GraphQL errors whose
/// `data` holds `field`'s object counts. GraphQL's errors keep their
/// message, terminal-safe and cut short.
fn read_set(output: io::Result<GhOutput>, now: i64, field: &str) -> Result<(), CreateError> {
    let stdout = output
        .as_ref()
        .map(|o| o.stdout.clone())
        .unwrap_or_default();
    let (status, _, body) = split_include(&stdout);
    match classify(output, now) {
        Answer::Limited { until } => Err(CreateError::RateLimited { until }),
        Answer::Body { .. } if status == Some(200) => {
            let failed = CreateError::Status(ForgeStatus::Failed);
            let response: Response = serde_json::from_slice(body).map_err(|_| failed.clone())?;
            if response.errors.is_some_and(|e| !e.is_empty()) {
                return Err(refusal(body).map_or(failed, CreateError::Refused));
            }
            match response.data.as_ref().and_then(|data| data.get(field)) {
                Some(serde_json::Value::Object(_)) => Ok(()),
                _ => Err(failed),
            }
        }
        Answer::Failed(status) => Err(CreateError::Status(status)),
        Answer::Body { .. } | Answer::NotModified => Err(CreateError::Status(ForgeStatus::Failed)),
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::ForgeHost;
    use crate::github::fixtures::{
        DNS_STDERR, GRAPHQL_RATE_LIMITED, HTTP_401_STDERR, NOT_FOUND, NOT_FOUND_STDERR,
    };
    use crate::github::{FakeGh, MAX_REFUSAL, answer, failure};
    use kika_git::recording::{CommandEvent, CommandSink};

    const NOW: i64 = 1_700_000_000;
    const ID: &str = "PR_kwDOA1";
    const PATH: &str = "src/café \"quoted\".rs";
    const MARKED: &str = "HTTP/2.0 200 OK\r\nContent-Type: application/json\r\n\r\n\
        {\"data\":{\"markFileAsViewed\":{\"clientMutationId\":null}}}";
    const UNMARKED: &str = "HTTP/2.0 200 OK\r\nContent-Type: application/json\r\n\r\n\
        {\"data\":{\"unmarkFileAsViewed\":{\"clientMutationId\":null}}}";

    fn host() -> ForgeHost {
        ForgeHost::parse("github.com").unwrap()
    }

    fn set_with(
        runner: &FakeGh,
        sink: &CommandSink,
        id: &str,
        path: &str,
        viewed: bool,
    ) -> Result<(), CreateError> {
        let host = host();
        let gh = Gh {
            runner,
            sink,
            host: &host,
            cwd: "/cache".into(),
            now: NOW,
        };
        gh.set_file_viewed(id, path, viewed)
    }

    fn set(runner: &FakeGh, viewed: bool) -> Result<(), CreateError> {
        set_with(runner, &CommandSink::new(), ID, PATH, viewed)
    }

    fn failed() -> Result<(), CreateError> {
        Err(CreateError::Status(ForgeStatus::Failed))
    }

    #[test]
    fn one_fixed_call_carries_the_id_and_path_on_stdin_only() {
        for (viewed, reply, name, other) in [
            (true, MARKED, "markFileAsViewed", "{unmarkFileAsViewed("),
            (false, UNMARKED, "unmarkFileAsViewed", "{markFileAsViewed("),
        ] {
            let runner = FakeGh::new().reply(answer(reply));
            assert_eq!(set(&runner, viewed), Ok(()), "{name}");
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
            assert_eq!(cmd.cwd, std::path::PathBuf::from("/cache"));
            let argv = format!("{:?}", cmd.args());
            assert!(!argv.contains(ID) && !argv.contains("café"), "{argv}");
            let sent: serde_json::Value = serde_json::from_slice(&cmd.stdin()).unwrap();
            assert_eq!(sent["variables"], json!({ "id": ID, "path": PATH }));
            let query = sent["query"].as_str().unwrap();
            assert!(query.contains(&format!("{{{name}(input:")), "{query}");
            assert!(!query.contains(other), "{query}");
            assert!(!query.contains(ID) && !query.contains("café"), "{query}");
        }
    }

    #[test]
    fn a_bad_id_or_path_makes_no_call() {
        let runner = FakeGh::new();
        let sink = CommandSink::new();
        let long = "p".repeat(MAX_PATH + 1);
        for (id, path) in [
            ("PR kw", PATH),
            ("", PATH),
            ("PR_\u{1b}[31m", PATH),
            (ID, ""),
            (ID, long.as_str()),
        ] {
            assert_eq!(set_with(&runner, &sink, id, path, true), failed(), "{id:?}");
        }
        assert_eq!(runner.calls(), []);
        // The longest path kept from GitHub may still be sent.
        let runner = FakeGh::new().reply(answer(MARKED));
        let longest = "p".repeat(MAX_PATH);
        assert_eq!(set_with(&runner, &sink, ID, &longest, true), Ok(()));
    }

    #[test]
    fn graphql_errors_or_no_answer_object_are_failures() {
        for body in [
            "{\"data\":{\"markFileAsViewed\":null},\"errors\":[{\"type\":\"UNPROCESSABLE\"}]}",
            "{\"data\":{}}",
            "{\"data\":null}",
            "{\"data\":{\"markFileAsViewed\":null}}",
            "{\"data\":{\"markFileAsViewed\":\"yes\"}}",
            // The answer to the other mutation is not this one's.
            "{\"data\":{\"unmarkFileAsViewed\":{\"clientMutationId\":null}}}",
            "not json",
        ] {
            let reply = format!("HTTP/2.0 200 OK\r\n\r\n{body}");
            let runner = FakeGh::new().reply(answer(&reply));
            assert_eq!(set(&runner, true), failed(), "{body}");
        }
        for reply in [
            "HTTP/2.0 500 Oops\r\n\r\n{}",
            "HTTP/2.0 502 Bad Gateway\r\n\r\nnot json",
            "HTTP/2.0 201 Created\r\n\r\n{\"data\":{\"markFileAsViewed\":{}}}",
        ] {
            let runner = FakeGh::new().reply(answer(reply));
            assert_eq!(set(&runner, true), failed(), "{reply}");
        }
    }

    #[test]
    fn githubs_reason_is_terminal_safe_and_cut_short() {
        let long = format!(
            "Could not resolve\u{1b}]52;c;eA==\u{7}\r{}",
            "x".repeat(500)
        );
        let body = json!({
            "data": { "markFileAsViewed": null },
            "errors": [{ "type": "UNPROCESSABLE", "message": long }],
        });
        let reply = format!("HTTP/2.0 200 OK\r\n\r\n{body}");
        let runner = FakeGh::new().reply(answer(&reply));
        let Err(CreateError::Refused(text)) = set(&runner, true) else {
            panic!("expected a refusal")
        };
        assert!(!text.chars().any(char::is_control), "{text:?}");
        assert!(text.starts_with("Could not resolve"), "{text}");
        assert_eq!(text.chars().count(), MAX_REFUSAL + 1, "{text}");
    }

    #[test]
    fn limits_logins_the_network_and_a_missing_gh_say_why() {
        let runner = FakeGh::new().reply(answer(GRAPHQL_RATE_LIMITED));
        assert!(matches!(
            set(&runner, true),
            Err(CreateError::RateLimited { .. })
        ));
        let cases = [
            (FakeGh::new().missing(), ForgeStatus::NoGh),
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
        ];
        for (runner, want) in cases {
            assert_eq!(set(&runner, true), Err(CreateError::Status(want)));
        }
    }

    #[test]
    fn each_mark_is_logged_once_under_its_own_id() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = CommandSink::new();
        let keep = Arc::clone(&events);
        sink.connect(move |ev| keep.lock().unwrap().push(ev));
        let runner = FakeGh::new().reply(answer(MARKED)).missing();
        assert_eq!(set_with(&runner, &sink, ID, PATH, true), Ok(()));
        assert_eq!(
            set_with(&runner, &sink, ID, PATH, true),
            Err(CreateError::Status(ForgeStatus::NoGh))
        );
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
                outcome: CommandOutcome::NotStarted,
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
}
