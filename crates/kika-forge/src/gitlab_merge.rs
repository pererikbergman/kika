//! Merging a merge request on GitLab. The REST merge answers with the
//! merge request itself, so its state says whether the merge happened;
//! the body names the head commit the user saw, so GitLab refuses rather
//! than merge another one. The project's own merge method applies (a
//! merge commit, semi-linear or fast-forward); only squashing is chosen.
//! The REST path needs the project's numeric id, which one GraphQL lookup
//! by path gives first. The path travels as a variable on stdin and the
//! REST path is made of decimal ids, so argv holds no name from the
//! repository.

use serde::Deserialize;
use serde_json::json;

use super::gitlab::{Glab, GlabCall, RestMethod, RestPath, classify_merge};
use super::gitlab_review::project_path;
use super::gitlab_submit::{project_id, refused};
use super::review::{MergeMethod, data_of, read_with};
use super::{CreateError, ForgeRepo, ForgeStatus};
use kika_git::commit_files::CommitId;

/// The project's global id, asked by its full path.
const PROJECT_QUERY: &str = "query($path:ID!){project(fullPath:$path){id}}";

#[derive(Deserialize)]
struct ProjectData {
    project: Option<ProjectNode>,
}

#[derive(Deserialize)]
struct ProjectNode {
    id: Option<String>,
}

#[derive(Deserialize)]
struct Merged {
    state: Option<String>,
}

/// The numeric project id from a body [`classify`](super::gitlab::classify)
/// passed. A null project is no access; an id that is not GitLab's usual
/// shape is no usable answer.
fn read_project(body: &[u8]) -> Result<u64, ForgeStatus> {
    let data: ProjectData = data_of(body)?;
    let project = data.project.ok_or(ForgeStatus::NoAccess)?;
    project
        .id
        .as_deref()
        .and_then(project_id)
        .ok_or(ForgeStatus::Failed)
}

impl Glab<'_> {
    /// Merge merge request `iid` of `repo`, squashed when `method` is
    /// [`MergeMethod::Squash`], only while its head is still `head_oid`.
    /// GitLab's own commit messages apply. Only an answer whose state is
    /// `merged` is a merge; GitLab's refusals keep its message.
    pub(crate) fn merge_request(
        &self,
        repo: &ForgeRepo,
        iid: u64,
        method: MergeMethod,
        head_oid: &CommitId,
    ) -> Result<(), CreateError> {
        if iid == 0 {
            return Err(CreateError::Status(ForgeStatus::Failed));
        }
        let project = self.project_id(repo)?;
        let body = json!({ "sha": head_oid.as_str(), "squash": method == MergeMethod::Squash });
        let answer = self.rest_judged(
            RestMethod::Put,
            RestPath::Merge { project, iid },
            Some(serde_json::to_vec(&body).unwrap_or_default()),
            classify_merge,
        )?;
        match serde_json::from_slice::<Merged>(&answer) {
            Ok(Merged { state: Some(state) }) if state == "merged" => Ok(()),
            _ => Err(CreateError::Status(ForgeStatus::Failed)),
        }
    }

    /// The numeric id of `repo`'s project, which the REST paths take.
    fn project_id(&self, repo: &ForgeRepo) -> Result<u64, CreateError> {
        let request = json!({
            "query": PROJECT_QUERY,
            "variables": { "path": project_path(repo) },
        });
        let kind = GlabCall::Graphql {
            body: serde_json::to_vec(&request).unwrap_or_default(),
        };
        self.call(kind, |answer| read_with(answer, read_project))
            .map_err(refused)
    }
}

/// Hand-written answers in the shape GitLab gives with `--include`.
#[cfg(any(test, feature = "test-support"))]
pub mod fixtures {
    pub const PROJECT_ID: &str = include_str!("fixtures/gitlab_project_id.http");
    pub const MERGED: &str = include_str!("fixtures/gitlab_merged_200.http");
    pub const MERGE_OPENED: &str = include_str!("fixtures/gitlab_merge_opened_200.http");
    pub const MERGE_405: &str = include_str!("fixtures/gitlab_merge_405.http");
    pub const MERGE_409: &str = include_str!("fixtures/gitlab_merge_409.http");
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::fixtures::*;
    use super::*;
    use crate::ForgeHost;
    use crate::cli::CliStatus;
    use crate::github::{MAX_REFUSAL, answer, failure};
    use crate::gitlab::fixtures::{PROJECT_NULL, RATE_LIMITED_429, UNAUTHORIZED_401};
    use crate::gitlab::{FakeGlab, GlabCommand};
    use crate::url::ForgeKind;
    use kika_git::recording::{CommandEvent, CommandOutcome, CommandSink};

    const NOW: i64 = 1_700_000_000;
    const HEAD: &str = "1111111111111111111111111111111111111111";

    fn repo() -> ForgeRepo {
        ForgeRepo {
            host: "gitlab.example.com".into(),
            owner: "acme".into(),
            name: "widgets".into(),
        }
    }

    fn head() -> CommitId {
        CommitId::parse(HEAD).unwrap()
    }

    fn merge_with(
        runner: &FakeGlab,
        sink: &CommandSink,
        repo: &ForgeRepo,
        iid: u64,
        method: MergeMethod,
    ) -> Result<(), CreateError> {
        let host = ForgeHost::parse("gitlab.example.com")
            .unwrap()
            .with_kind(ForgeKind::GitLab);
        Glab {
            runner,
            sink,
            host: &host,
            cwd: "/cache".into(),
            now: NOW,
        }
        .merge_request(repo, iid, method, &head())
    }

    fn merge(runner: &FakeGlab, method: MergeMethod) -> Result<(), CreateError> {
        merge_with(runner, &CommandSink::new(), &repo(), 12, method)
    }

    /// The project lookup, then `merge` as the PUT's answer.
    fn answering(merge: &str) -> FakeGlab {
        FakeGlab::new()
            .reply(answer(PROJECT_ID))
            .reply(answer(merge))
    }

    fn argv(cmd: &GlabCommand) -> Vec<String> {
        cmd.args()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn sent(cmd: &GlabCommand) -> serde_json::Value {
        serde_json::from_slice(&cmd.stdin()).unwrap()
    }

    #[test]
    fn the_project_is_looked_up_then_the_merge_is_put_with_the_sha_on_stdin() {
        for (method, squash) in [(MergeMethod::Squash, true), (MergeMethod::Merge, false)] {
            let runner = answering(MERGED);
            assert_eq!(merge(&runner, method), Ok(()));
            let [lookup, put] = runner.calls().try_into().unwrap();
            assert_eq!(
                argv(&lookup),
                [
                    "api",
                    "graphql",
                    "--include",
                    "--hostname",
                    "gitlab.example.com",
                    "--input",
                    "-"
                ]
            );
            assert_eq!(
                sent(&lookup),
                json!({ "query": PROJECT_QUERY, "variables": { "path": "acme/widgets" } })
            );
            assert_eq!(
                argv(&put),
                [
                    "api",
                    "projects/17/merge_requests/12/merge",
                    "--method",
                    "PUT",
                    "--include",
                    "--hostname",
                    "gitlab.example.com",
                    "--header",
                    "Content-Type: application/json",
                    "--input",
                    "-"
                ]
            );
            assert_eq!(sent(&put), json!({ "sha": HEAD, "squash": squash }));
            // No path, title or sha in argv.
            for cmd in [&lookup, &put] {
                for arg in argv(cmd) {
                    assert!(
                        !arg.contains("acme") && !arg.contains("widget") && !arg.contains(HEAD),
                        "{arg}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_200_that_is_not_merged_is_a_failure() {
        let failed = Err(CreateError::Status(ForgeStatus::Failed));
        assert_eq!(merge(&answering(MERGE_OPENED), MergeMethod::Merge), failed);
        for body in ["{}", "not json", "{\"state\":null}"] {
            let reply = format!("HTTP/2.0 200 OK\r\n\r\n{body}");
            assert_eq!(
                merge(&answering(&reply), MergeMethod::Merge),
                failed,
                "{body}"
            );
        }
    }

    #[test]
    fn gitlabs_refusals_keep_its_message_terminal_safe_and_short() {
        let Err(CreateError::Refused(text)) = merge(&answering(MERGE_409), MergeMethod::Merge)
        else {
            panic!("expected a refusal");
        };
        assert!(
            text.starts_with("SHA does not match HEAD of source branch"),
            "{text}"
        );
        assert!(!text.chars().any(char::is_control), "{text:?}");
        let Err(CreateError::Refused(text)) = merge(&answering(MERGE_405), MergeMethod::Squash)
        else {
            panic!("expected a refusal");
        };
        assert!(text.starts_with("405 Method Not Allowed"), "{text}");
        assert!(
            !text
                .chars()
                .any(|c| c.is_control() || kika_git::safe_text::is_invisible_or_bidi(c)),
            "{text:?}"
        );
        let long = format!(
            "HTTP/2.0 422 Unprocessable Entity\r\n\r\n{{\"message\":\"{}\"}}",
            "x".repeat(500)
        );
        let Err(CreateError::Refused(text)) = merge(&answering(&long), MergeMethod::Merge) else {
            panic!("expected a refusal");
        };
        assert_eq!(text.chars().count(), MAX_REFUSAL + 1);
        let not_acceptable =
            "HTTP/2.0 406 Not Acceptable\r\n\r\n{\"message\":\"Branch cannot be merged\"}";
        assert_eq!(
            merge(&answering(not_acceptable), MergeMethod::Merge),
            Err(CreateError::Refused("Branch cannot be merged".into()))
        );
    }

    #[test]
    fn logins_limits_and_the_network_say_why_as_the_reads_do() {
        let cases = [
            (
                UNAUTHORIZED_401,
                CreateError::Status(ForgeStatus::NotLoggedIn),
            ),
            (
                RATE_LIMITED_429,
                CreateError::RateLimited {
                    until: 1_700_001_200,
                },
            ),
        ];
        for (fixture, want) in cases.clone() {
            // At the lookup: no PUT follows.
            let runner = FakeGlab::new().reply(answer(fixture));
            assert_eq!(merge(&runner, MergeMethod::Merge), Err(want.clone()));
            assert_eq!(runner.calls().len(), 1);
            // At the PUT.
            let runner = answering(fixture);
            assert_eq!(merge(&runner, MergeMethod::Merge), Err(want));
            assert_eq!(runner.calls().len(), 2);
        }
        let offline = failure(1, "dial tcp: lookup gitlab.example.com: no such host");
        let runner = FakeGlab::new()
            .reply(answer(PROJECT_ID))
            .reply(offline.clone());
        assert_eq!(
            merge(&runner, MergeMethod::Merge),
            Err(CreateError::Status(ForgeStatus::Offline))
        );
        let runner = FakeGlab::new().reply(offline);
        assert_eq!(
            merge(&runner, MergeMethod::Merge),
            Err(CreateError::Status(ForgeStatus::Offline))
        );
        assert_eq!(
            merge(&FakeGlab::new().missing(), MergeMethod::Merge),
            Err(CreateError::Status(ForgeStatus::Cli(
                CliStatus::NotInstalled
            )))
        );
    }

    #[test]
    fn no_project_a_bad_project_id_or_iid_0_sends_no_put() {
        let runner = FakeGlab::new().reply(answer(PROJECT_NULL));
        assert_eq!(
            merge(&runner, MergeMethod::Merge),
            Err(CreateError::Status(ForgeStatus::NoAccess))
        );
        assert_eq!(runner.calls().len(), 1);
        for bad in [
            "gid://gitlab/Project/+17",
            "gid://gitlab/Project/17/merge",
            "gid://gitlab/Group/17",
            "gid://gitlab/Project/",
            "17",
        ] {
            let reply = PROJECT_ID.replace("gid://gitlab/Project/17", bad);
            let runner = FakeGlab::new().reply(answer(&reply));
            assert_eq!(
                merge(&runner, MergeMethod::Merge),
                Err(CreateError::Status(ForgeStatus::Failed)),
                "{bad}"
            );
            assert_eq!(runner.calls().len(), 1, "{bad}");
        }
        let runner = FakeGlab::new();
        assert_eq!(
            merge_with(&runner, &CommandSink::new(), &repo(), 0, MergeMethod::Merge),
            Err(CreateError::Status(ForgeStatus::Failed))
        );
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn a_nested_project_is_looked_up_by_its_full_path() {
        let nested = ForgeRepo {
            owner: "acme/sub".into(),
            ..repo()
        };
        let runner = answering(MERGED);
        assert_eq!(
            merge_with(
                &runner,
                &CommandSink::new(),
                &nested,
                12,
                MergeMethod::Merge
            ),
            Ok(())
        );
        assert_eq!(
            sent(&runner.calls()[0])["variables"]["path"],
            "acme/sub/widgets"
        );
    }

    #[test]
    fn both_calls_are_logged_each_under_its_own_id() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = CommandSink::new();
        let keep = Arc::clone(&events);
        sink.connect(move |ev| keep.lock().unwrap().push(ev));
        let runner = answering(MERGED);
        assert_eq!(
            merge_with(&runner, &sink, &repo(), 12, MergeMethod::Squash),
            Ok(())
        );
        let events = events.lock().unwrap();
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
        assert_eq!(
            l1,
            "glab api graphql --include --hostname gitlab.example.com --input -"
        );
        assert_eq!(
            l2,
            "glab api projects/17/merge_requests/12/merge --method PUT --include \
             --hostname gitlab.example.com --header 'Content-Type: application/json' --input -"
        );
        assert_eq!((a, c), (b, d));
        assert_ne!(a, c);
    }
}
