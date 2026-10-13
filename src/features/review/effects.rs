//! The I/O behind the review effects: reading the queues and a pull
//! request through `gh`, checking for and fetching its commits, listing
//! its files, and merging it. Each answers with exactly one intent; none
//! blocks the event loop.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use super::git::{self as pr_objects, CommitsHere};
use crate::background;
use crate::effect::{AnswerSlot, Bridge, Tag};
use crate::features::review::ReviewIntent;
use crate::forge::merge::MergeMethod;
use crate::forge::review::{self, ReviewError, ReviewLoad};
use crate::forge::task::ForgeContext;
use crate::forge::{CreateError, ForgeHost, ForgeRepo, ForgeStatus};
use crate::git::GitRunner;
use crate::git::commit_files::CommitId;
use crate::git::remotes::RemoteName;
use crate::intent::AppIntent;

/// Read the review queues; always answered by one `ReviewIntent::QueuesLoaded`.
pub(crate) fn load_queues(
    forge: Option<ForgeContext>,
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    hosts: Vec<ForgeHost>,
    repo: Option<ForgeRepo>,
    limited: Vec<(ForgeHost, i64)>,
) {
    background::run(
        tx,
        move || {
            let Some(forge) = forge else {
                // No forge client: nothing may reach the network.
                return (None, Err(ReviewError::NoForgeHost));
            };
            let load = ReviewLoad {
                hosts: &hosts,
                known: repo.as_ref(),
                limited: &limited,
                now: forge.now(),
            };
            review::run(runner.as_ref(), &forge, &load, |gh, repo| {
                gh.fetch_queues(repo)
            })
        },
        // A panic still answers, or the load would stay in flight.
        |_| (None, Err(ReviewError::Status(ForgeStatus::Failed))),
        move |(repo, result)| AppIntent::Review(ReviewIntent::QueuesLoaded { tag, repo, result }),
    );
}

/// Read pull request `number`; always answered by one `ReviewIntent::PrDetailLoaded`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn load_detail(
    forge: Option<ForgeContext>,
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    hosts: Vec<ForgeHost>,
    repo: Option<ForgeRepo>,
    number: u64,
    limited: Vec<(ForgeHost, i64)>,
) {
    background::run(
        tx,
        move || {
            let Some(forge) = forge else {
                return (None, Err(ReviewError::NoForgeHost));
            };
            let load = ReviewLoad {
                hosts: &hosts,
                known: repo.as_ref(),
                limited: &limited,
                now: forge.now(),
            };
            review::run(runner.as_ref(), &forge, &load, |gh, repo| {
                gh.fetch_detail(repo, number)
            })
        },
        |_| (None, Err(ReviewError::Status(ForgeStatus::Failed))),
        move |(repo, result)| {
            AppIntent::Review(ReviewIntent::PrDetailLoaded {
                tag,
                number,
                repo,
                result,
            })
        },
    );
}

/// See whether `head` and `base` are here, and where to fetch the missing
/// ones from. Local only; always answered by one `ReviewIntent::ObjectsChecked`.
pub(crate) fn check_commits(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    head: CommitId,
    base: CommitId,
    hosts: Vec<ForgeHost>,
    repo: ForgeRepo,
) {
    background::run(
        tx,
        move || {
            let missing = pr_objects::missing_commits(runner.as_ref(), &[&head, &base])?;
            if missing.is_empty() {
                return Ok(CommitsHere::All);
            }
            Ok(CommitsHere::Missing {
                oids: missing,
                remote: review::fetch_remote(runner.as_ref(), &hosts, &repo),
            })
        },
        // A panic still answers, or the files screen would wait forever.
        background::crashed("git cat-file"),
        move |result| AppIntent::Review(ReviewIntent::ObjectsChecked { tag, result }),
    );
}

/// Fetch `oids` from `remote`, with git's questions asked through the
/// core credential `slot`; always answered by one `ReviewIntent::HeadFetched`.
pub(crate) fn fetch_commits(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    remote: RemoteName,
    oids: Vec<CommitId>,
    slot: AnswerSlot,
) {
    let bridge_tx = tx.clone();
    background::run(
        tx,
        move || {
            let bridge = Bridge::open(tag, bridge_tx, slot);
            pr_objects::fetch_commits(runner.as_ref(), &remote, &oids, bridge.env.as_ref())
        },
        // A panic still answers, or `op_seq` would block every later op.
        background::crashed("git"),
        move |result| AppIntent::Review(ReviewIntent::HeadFetched { tag, result }),
    );
}

/// List the files the pull request changes; always answered by one
/// `ReviewIntent::FilesLoaded`.
pub(crate) fn load_files(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    head: CommitId,
    base: CommitId,
) {
    background::run(
        tx,
        move || pr_objects::pr_files(runner.as_ref(), &base, &head),
        background::crashed("git diff-tree"),
        move |result| AppIntent::Review(ReviewIntent::FilesLoaded { tag, result }),
    );
}

/// Merge pull request `number` through `gh`; always answered by one
/// `ReviewIntent::Merged`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn merge(
    forge: Option<ForgeContext>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    host: ForgeHost,
    repo: ForgeRepo,
    number: u64,
    method: MergeMethod,
    head_oid: CommitId,
) {
    // No forge client: nothing may reach the network. The client itself
    // refuses a repository `[forge.allow]` leaves out.
    let Some(forge) = forge else {
        let _ = tx.send(AppIntent::Review(ReviewIntent::Merged {
            tag,
            number,
            result: Err(CreateError::Status(ForgeStatus::Failed)),
        }));
        return;
    };
    background::run(
        tx,
        move || forge.merge_pull(&host, &repo, number, method, &head_oid),
        // A panic still answers, or the merge would stay in flight.
        |_| Err(CreateError::Status(ForgeStatus::Failed)),
        move |result| {
            AppIntent::Review(ReviewIntent::Merged {
                tag,
                number,
                result,
            })
        },
    );
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::mpsc;

    use crate::effect::{Effect, Runtime};
    use crate::features::review::ReviewIntent;
    use crate::forge::merge::MergeMethod;
    use crate::forge::review::ReviewError;
    use crate::forge::task::ForgeContext;
    use crate::forge::{CreateError, ForgeHost, ForgeRepo, ForgeStatus};
    use crate::git::commit_files::CommitId;
    use crate::git::fake::FakeRunner;
    use crate::git::recording::CommandSink;
    use crate::intent::AppIntent;
    use crate::test_support::forge_tag;

    #[tokio::test]
    async fn review_loads_answer_once_and_reach_nobody_without_a_forge_client() {
        use crate::forge::github::{FakeGh, answer};
        use crate::forge::review::fixtures::DETAIL;
        use crate::git::recording::CommandSink;

        let github = vec![ForgeHost::parse("github.com").unwrap()];
        let repo = ForgeRepo {
            host: "github.com".into(),
            owner: "acme".into(),
            name: "widgets".into(),
        };
        let git = Arc::new(FakeRunner::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let bare = Runtime::new(git.clone(), tx);
        bare.spawn(Effect::LoadReviewQueues {
            tag: forge_tag(),
            hosts: github.clone(),
            repo: Some(repo.clone()),
            limited: vec![],
        });
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::Review(ReviewIntent::QueuesLoaded {
                tag: forge_tag(),
                repo: None,
                result: Err(ReviewError::NoForgeHost),
            }))
        );

        let gh = Arc::new(FakeGh::new().reply(answer(DETAIL)));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(git.clone(), tx).with_forge(Some(ForgeContext {
            gh: gh.clone(),
            sink: CommandSink::new(),
            cache_dir: None,
        }));
        runtime.spawn(Effect::LoadPrDetail {
            tag: forge_tag(),
            hosts: github,
            repo: Some(repo.clone()),
            number: 42,
            limited: vec![],
        });
        match rx.recv().await {
            Some(AppIntent::Review(ReviewIntent::PrDetailLoaded {
                number: 42,
                repo: Some(found),
                result: Ok(Some(detail)),
                ..
            })) => {
                assert_eq!(found, repo);
                assert_eq!(detail.number, 42);
            }
            other => panic!("{other:?}"),
        }
        git.assert_done();
        assert_eq!(gh.calls().len(), 1);
    }

    #[tokio::test]
    async fn a_merge_runs_gh_in_the_cache_dir_and_answers_once() {
        use crate::forge::github::{FakeGh, GhCall, answer};

        let tmp = crate::git::fake::TempDir::new("forge-effect-merge");
        let cache_dir = tmp.path().join("forge");
        let git = Arc::new(FakeRunner::new());
        let gh = Arc::new(FakeGh::new().reply(answer(
            "HTTP/2.0 200 OK\r\n\r\n{\"merged\":true,\"message\":\"Pull Request successfully merged\"}",
        )));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(git.clone(), tx).with_forge(Some(ForgeContext {
            gh: gh.clone(),
            sink: CommandSink::new(),
            cache_dir: Some(cache_dir.clone()),
        }));
        let merge = Effect::MergePullRequest {
            tag: forge_tag(),
            host: ForgeHost::parse("github.com").unwrap(),
            repo: ForgeRepo {
                host: "github.com".into(),
                owner: "acme".into(),
                name: "widgets".into(),
            },
            number: 42,
            method: MergeMethod::Rebase,
            head_oid: CommitId::parse(&"1".repeat(40)).unwrap(),
        };
        runtime.spawn(merge.clone());
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::Review(ReviewIntent::Merged {
                tag: forge_tag(),
                number: 42,
                result: Ok(()),
            }))
        );
        git.assert_done();
        let [call] = gh.calls().try_into().unwrap();
        assert_eq!(call.cwd, cache_dir);
        assert!(matches!(
            &call.kind,
            GhCall::Merge { path, .. } if path == "repos/acme/widgets/pulls/42/merge"
        ));

        // Without a forge client, nothing is asked.
        let (tx, mut rx) = mpsc::unbounded_channel();
        Runtime::new(git, tx).spawn(merge);
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::Review(ReviewIntent::Merged {
                tag: forge_tag(),
                number: 42,
                result: Err(CreateError::Status(ForgeStatus::Failed)),
            }))
        );
        assert_eq!(gh.calls().len(), 1);
    }

    #[tokio::test]
    async fn a_merge_on_a_repository_the_allow_list_leaves_out_runs_no_gh() {
        use crate::forge::allow::AllowRule;
        use crate::forge::github::FakeGh;

        let git = Arc::new(FakeRunner::new());
        let gh = Arc::new(FakeGh::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(git.clone(), tx).with_forge(Some(ForgeContext {
            gh: gh.clone(),
            sink: CommandSink::new(),
            cache_dir: None,
        }));
        runtime.spawn(Effect::MergePullRequest {
            tag: forge_tag(),
            host: ForgeHost::parse("github.com")
                .unwrap()
                .with_allow(vec![AllowRule::parse("other").unwrap()]),
            repo: ForgeRepo {
                host: "github.com".into(),
                owner: "acme".into(),
                name: "widgets".into(),
            },
            number: 42,
            method: MergeMethod::Merge,
            head_oid: CommitId::parse(&"1".repeat(40)).unwrap(),
        });
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::Review(ReviewIntent::Merged {
                tag: forge_tag(),
                number: 42,
                result: Err(CreateError::Status(ForgeStatus::Failed)),
            }))
        );
        git.assert_done();
        assert_eq!(gh.calls(), []);
    }
}
