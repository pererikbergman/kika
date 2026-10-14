//! Merging a pull request on GitHub. Which ways are offered, and why a
//! pull request cannot be merged, are read from its detail here, in fixed
//! words; the merge itself is one REST `PUT` whose body names the head
//! commit the user saw, so GitHub refuses rather than merge another one.

use std::io;

use serde::Deserialize;
use serde_json::json;

use super::github::{Answer, Gh, GhCall, GhOutput, classify, refusal, split_include};
use super::remote::segment_ok;
pub use super::review::MergeMethod;
use super::review::{MergeState, Mergeability, PrDetail};
use super::{CreateError, ForgeRepo, ForgeStatus, PrState};
use kika_git::commit_files::CommitId;
use kika_git::recording::CommandOutcome;

/// Why a pull request cannot be merged from here. A fixed set: nothing
/// GitHub says is ever kept in one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeBlock {
    Merged,
    Closed,
    Draft,
    /// The head conflicts with the base.
    Conflicts,
    /// A required review or check is missing.
    Blocked,
    /// The base moved on and the branch must be updated first.
    Behind,
    /// GitHub has not worked it out yet.
    Unknown,
    /// The repository allows no merge method.
    NoMethod,
    /// GitHub sent no head commit to guard the merge with.
    NoHeadId,
}

impl MergeBlock {
    pub fn message(self) -> &'static str {
        match self {
            MergeBlock::Merged => "already merged",
            MergeBlock::Closed => "closed",
            MergeBlock::Draft => "it is a draft",
            MergeBlock::Conflicts => "it conflicts with the base",
            MergeBlock::Blocked => "a required review or check is missing",
            MergeBlock::Behind => "the base moved on; the branch must be updated first",
            MergeBlock::Unknown => "GitHub is still checking; refresh with r",
            MergeBlock::NoMethod => "the repository allows no merge method",
            MergeBlock::NoHeadId => "GitHub sent no head commit id",
        }
    }
}

/// The ways `detail` may be merged now, in [`MergeMethod`] order, or why
/// it may not. Only an open, ready, mergeable pull request whose merge
/// state is clean, or blocked by nothing required, may be.
pub fn merge_options(detail: &PrDetail) -> Result<Vec<MergeMethod>, MergeBlock> {
    match detail.state {
        PrState::Merged => return Err(MergeBlock::Merged),
        PrState::Closed => return Err(MergeBlock::Closed),
        PrState::Open | PrState::Draft => {}
    }
    if detail.draft || detail.state == PrState::Draft {
        return Err(MergeBlock::Draft);
    }
    if detail.mergeable == Mergeability::Conflicting {
        return Err(MergeBlock::Conflicts);
    }
    match detail.merge_state {
        MergeState::Draft => return Err(MergeBlock::Draft),
        MergeState::Dirty => return Err(MergeBlock::Conflicts),
        MergeState::Blocked => return Err(MergeBlock::Blocked),
        MergeState::Behind => return Err(MergeBlock::Behind),
        MergeState::Unknown => return Err(MergeBlock::Unknown),
        MergeState::Clean | MergeState::HasHooks | MergeState::Unstable => {}
    }
    if detail.mergeable != Mergeability::Mergeable {
        return Err(MergeBlock::Unknown);
    }
    if detail.merge_methods.is_empty() {
        return Err(MergeBlock::NoMethod);
    }
    if detail.head_oid.is_none() {
        return Err(MergeBlock::NoHeadId);
    }
    Ok(
        [MergeMethod::Merge, MergeMethod::Squash, MergeMethod::Rebase]
            .into_iter()
            .filter(|m| detail.merge_methods.contains(m))
            .collect(),
    )
}

/// The REST path of pull request `number`'s merge.
fn merge_path(repo: &ForgeRepo, number: u64) -> String {
    format!("repos/{}/{}/pulls/{number}/merge", repo.owner, repo.name)
}

impl Gh<'_> {
    /// Merge pull request `number` of `repo` by `method`, only while its
    /// head is still `head_oid`. GitHub's own commit title and message
    /// apply.
    pub fn merge_pull(
        &self,
        repo: &ForgeRepo,
        number: u64,
        method: MergeMethod,
        head_oid: &CommitId,
    ) -> Result<(), CreateError> {
        // Nothing that could change the path's meaning is ever sent.
        if !segment_ok(&repo.owner) || !segment_ok(&repo.name) || number == 0 {
            return Err(CreateError::Status(ForgeStatus::Failed));
        }
        let body = json!({ "merge_method": method.api_name(), "sha": head_oid.as_str() });
        let kind = GhCall::Merge {
            path: merge_path(repo, number),
            body: serde_json::to_vec(&body).unwrap_or_default(),
        };
        let now = self.now;
        self.run_logged(
            kind,
            |output| read_merged(output, now),
            |result| match result {
                Ok(()) => CommandOutcome::Ok,
                Err(CreateError::Status(ForgeStatus::NoGh)) => CommandOutcome::NotStarted,
                Err(_) => CommandOutcome::Failed(None),
            },
        )
    }
}

#[derive(Deserialize)]
struct Merged {
    #[serde(default)]
    merged: bool,
}

/// What a merge call came to. Rate limits, logins and the network are
/// judged as for any call; only a 200 that says `merged` counts. GitHub's
/// refusals (405 not mergeable, 409 head moved, 422 invalid) keep its
/// message, terminal-safe and cut short.
fn read_merged(output: io::Result<GhOutput>, now: i64) -> Result<(), CreateError> {
    let stdout = output
        .as_ref()
        .map(|o| o.stdout.clone())
        .unwrap_or_default();
    let (status, _, body) = split_include(&stdout);
    match classify(output, now) {
        Answer::Limited { until } => Err(CreateError::RateLimited { until }),
        Answer::Body { .. } if status == Some(200) => {
            match serde_json::from_slice::<Merged>(body) {
                Ok(Merged { merged: true }) => Ok(()),
                _ => Err(CreateError::Status(ForgeStatus::Failed)),
            }
        }
        Answer::Failed(ForgeStatus::Failed) if matches!(status, Some(405 | 409 | 422)) => {
            Err(refusal(body).map_or(
                CreateError::Status(ForgeStatus::Failed),
                CreateError::Refused,
            ))
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
        DNS_STDERR, HTTP_401_STDERR, LIMIT_PRIMARY, NOT_FOUND, NOT_FOUND_STDERR,
    };
    use crate::github::{FakeGh, MAX_REFUSAL, answer, failure};
    use crate::review::fixtures::DETAIL;
    use kika_git::recording::{CommandEvent, CommandSink};

    const NOW: i64 = 1_700_000_000;
    const HEAD: &str = "1111111111111111111111111111111111111111";

    fn host() -> ForgeHost {
        ForgeHost::parse("github.com").unwrap()
    }

    fn repo() -> ForgeRepo {
        ForgeRepo {
            host: "github.com".into(),
            owner: "o".into(),
            name: "n".into(),
        }
    }

    fn head() -> CommitId {
        CommitId::parse(HEAD).unwrap()
    }

    fn merge_with(runner: &FakeGh, sink: &CommandSink) -> Result<(), CreateError> {
        let host = host();
        let gh = Gh {
            runner,
            sink,
            host: &host,
            cwd: "/cache".into(),
            now: NOW,
        };
        gh.merge_pull(&repo(), 7, MergeMethod::Squash, &head())
    }

    fn merge(runner: &FakeGh) -> Result<(), CreateError> {
        merge_with(runner, &CommandSink::new())
    }

    /// The fixture's detail: open, mergeable, clean, merge and squash
    /// allowed.
    fn detail() -> PrDetail {
        let runner = FakeGh::new().reply(answer(DETAIL));
        let (host, sink) = (host(), CommandSink::new());
        let gh = Gh {
            runner: &runner,
            sink: &sink,
            host: &host,
            cwd: "/cache".into(),
            now: NOW,
        };
        gh.fetch_detail(&repo(), 42).unwrap().unwrap()
    }

    #[test]
    fn only_the_allowed_methods_are_offered_in_order() {
        let mut pr = detail();
        assert_eq!(pr.head_oid, Some(head()));
        assert_eq!(
            merge_options(&pr),
            Ok(vec![MergeMethod::Merge, MergeMethod::Squash])
        );
        pr.merge_methods = vec![MergeMethod::Squash];
        assert_eq!(merge_options(&pr), Ok(vec![MergeMethod::Squash]));
        pr.merge_methods = vec![MergeMethod::Rebase, MergeMethod::Merge, MergeMethod::Squash];
        assert_eq!(
            merge_options(&pr),
            Ok(vec![
                MergeMethod::Merge,
                MergeMethod::Squash,
                MergeMethod::Rebase
            ])
        );
        // Checks that are not required do not stop it.
        for state in [MergeState::HasHooks, MergeState::Unstable] {
            pr.merge_state = state;
            assert!(merge_options(&pr).is_ok(), "{state:?}");
        }
    }

    /// One change to a detail.
    type Change = fn(&mut PrDetail);

    #[test]
    fn each_reason_not_to_merge_has_its_block() {
        let with = |change: Change| {
            let mut pr = detail();
            change(&mut pr);
            merge_options(&pr)
        };
        let cases: [(Change, MergeBlock); 11] = [
            (|pr| pr.state = PrState::Merged, MergeBlock::Merged),
            (|pr| pr.state = PrState::Closed, MergeBlock::Closed),
            (|pr| pr.draft = true, MergeBlock::Draft),
            (|pr| pr.merge_state = MergeState::Draft, MergeBlock::Draft),
            (
                |pr| pr.mergeable = Mergeability::Conflicting,
                MergeBlock::Conflicts,
            ),
            (
                |pr| pr.merge_state = MergeState::Dirty,
                MergeBlock::Conflicts,
            ),
            (
                |pr| pr.merge_state = MergeState::Blocked,
                MergeBlock::Blocked,
            ),
            (|pr| pr.merge_state = MergeState::Behind, MergeBlock::Behind),
            (
                |pr| pr.merge_state = MergeState::Unknown,
                MergeBlock::Unknown,
            ),
            (|pr| pr.merge_methods.clear(), MergeBlock::NoMethod),
            (|pr| pr.head_oid = None, MergeBlock::NoHeadId),
        ];
        for (change, block) in cases {
            assert_eq!(with(change), Err(block), "{block:?}");
        }
        assert_eq!(
            with(|pr| pr.mergeable = Mergeability::Unknown),
            Err(MergeBlock::Unknown)
        );
        assert!(MergeBlock::Unknown.message().contains("refresh with r"));
    }

    #[test]
    fn the_argv_is_fixed_and_the_method_and_head_travel_on_stdin() {
        let runner = FakeGh::new().reply(answer(MERGED));
        assert_eq!(merge(&runner), Ok(()));
        let [cmd] = runner.calls().try_into().unwrap();
        assert_eq!(
            cmd.args(),
            [
                "api",
                "--include",
                "--hostname",
                "github.com",
                "--method",
                "PUT",
                "repos/o/n/pulls/7/merge",
                "--input",
                "-"
            ]
            .map(OsString::from)
        );
        assert_eq!(
            cmd.display_line(),
            "gh api --include --hostname github.com --method PUT repos/o/n/pulls/7/merge --input -"
        );
        assert_eq!(cmd.cwd, std::path::PathBuf::from("/cache"));
        let sent: serde_json::Value = serde_json::from_slice(&cmd.stdin()).unwrap();
        assert_eq!(sent, json!({ "merge_method": "squash", "sha": HEAD }));
    }

    #[test]
    fn an_odd_repository_name_or_number_is_never_sent() {
        let runner = FakeGh::new();
        let host = host();
        let sink = CommandSink::new();
        let gh = Gh {
            runner: &runner,
            sink: &sink,
            host: &host,
            cwd: "/cache".into(),
            now: NOW,
        };
        let odd = ForgeRepo {
            name: "../../user".into(),
            ..repo()
        };
        let failed = Err(CreateError::Status(ForgeStatus::Failed));
        assert_eq!(gh.merge_pull(&odd, 7, MergeMethod::Merge, &head()), failed);
        assert_eq!(
            gh.merge_pull(&repo(), 0, MergeMethod::Merge, &head()),
            failed
        );
        assert_eq!(runner.calls(), []);
    }

    const MERGED: &str = "HTTP/2.0 200 OK\r\nContent-Type: application/json\r\n\r\n\
        {\"sha\":\"6dcb09b5b57875f334f61aebed695e2e4193db5e\",\"merged\":true,\"message\":\"Pull Request successfully merged\"}";

    #[test]
    fn only_a_200_that_says_merged_is_a_merge() {
        let runner = FakeGh::new().reply(answer(MERGED));
        assert_eq!(merge(&runner), Ok(()));
        let failed = Err(CreateError::Status(ForgeStatus::Failed));
        for body in [
            "HTTP/2.0 200 OK\r\n\r\n{\"merged\":false}",
            "HTTP/2.0 200 OK\r\n\r\n{}",
            "HTTP/2.0 200 OK\r\n\r\nnot json",
            "HTTP/2.0 201 Created\r\n\r\n{\"merged\":true}",
        ] {
            let runner = FakeGh::new().reply(answer(body));
            assert_eq!(merge(&runner), failed, "{body}");
        }
    }

    #[test]
    fn a_refusal_keeps_githubs_message_terminal_safe_and_short() {
        let long = format!("Pull Request is not mergeable\u{1b}[31m{}", "x".repeat(400));
        let body = serde_json::to_string(&json!({ "message": long })).unwrap();
        let refused = format!("HTTP/2.0 405 Method Not Allowed\r\n\r\n{body}");
        let runner = FakeGh::new().reply(answer(&refused));
        let Err(CreateError::Refused(text)) = merge(&runner) else {
            panic!("expected a refusal")
        };
        assert!(!text.chars().any(char::is_control), "{text:?}");
        assert!(text.starts_with("Pull Request is not mergeable"), "{text}");
        assert_eq!(text.chars().count(), MAX_REFUSAL + 1, "{text}");
        assert!(text.ends_with('…'));

        let moved = "HTTP/2.0 409 Conflict\r\n\r\n\
            {\"message\":\"Head branch was modified. Review and try the merge again.\"}";
        let runner = FakeGh::new().reply(answer(moved));
        assert_eq!(
            merge(&runner),
            Err(CreateError::Refused(
                "Head branch was modified. Review and try the merge again.".into()
            ))
        );
        let invalid =
            "HTTP/2.0 422 Unprocessable Entity\r\n\r\n{\"message\":\"Merge method not allowed\"}";
        let runner = FakeGh::new().reply(answer(invalid));
        assert_eq!(
            merge(&runner),
            Err(CreateError::Refused("Merge method not allowed".into()))
        );
        // Another status, or no readable reason, is a plain failure.
        for other in [
            "HTTP/2.0 500 Oops\r\n\r\n{\"message\":\"boom\"}",
            "HTTP/2.0 405 Method Not Allowed\r\n\r\nnot json",
        ] {
            let runner = FakeGh::new().reply(answer(other));
            assert_eq!(
                merge(&runner),
                Err(CreateError::Status(ForgeStatus::Failed)),
                "{other}"
            );
        }
    }

    #[test]
    fn limits_logins_the_network_and_a_missing_gh_say_why() {
        let runner = FakeGh::new().reply(answer(LIMIT_PRIMARY));
        assert_eq!(
            merge(&runner),
            Err(CreateError::RateLimited {
                until: 1_700_001_800
            })
        );
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
            assert_eq!(merge(&runner), Err(CreateError::Status(want)));
        }
    }

    #[test]
    fn each_merge_is_logged_once_with_the_fixed_line() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = CommandSink::new();
        let keep = Arc::clone(&events);
        sink.connect(move |ev| keep.lock().unwrap().push(ev));
        let runner = FakeGh::new().reply(answer(MERGED)).missing();
        assert_eq!(merge_with(&runner, &sink), Ok(()));
        assert_eq!(
            merge_with(&runner, &sink),
            Err(CreateError::Status(ForgeStatus::NoGh))
        );
        let events = events.lock().unwrap();
        let line =
            "gh api --include --hostname github.com --method PUT repos/o/n/pulls/7/merge --input -";
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
