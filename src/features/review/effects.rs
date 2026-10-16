//! The I/O behind the review effects: reading the queues and a pull
//! request through `gh`, checking for and fetching its commits, listing
//! its files and its commits, planning a checkout of its head,
//! merging it, and reading and writing its pending review in the state
//! directory. Each answers with exactly one intent; none blocks the
//! event loop.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc::UnboundedSender;

use super::git::{self as pr_objects, CommitsHere};
use crate::background;
use crate::effect::{AnswerSlot, Bridge, Tag};
use crate::features::review::ReviewIntent;
use crate::handoff::KeptFile;
use crate::intent::AppIntent;
use kika_forge::discussion_write::DiscussionWrite;
use kika_forge::merge::MergeMethod;
use kika_forge::pending::{self, PendingError, PendingKey, PendingReview};
use kika_forge::review::{self, ReviewError, ReviewLoad};
use kika_forge::task::ForgeContext;
use kika_forge::{CreateError, ForgeHost, ForgeRepo, ForgeStatus};
use kika_git::GitRunner;
use kika_git::branches::BranchName;
use kika_git::commit_files::{CommitId, CommitRange};
use kika_git::remotes::RemoteName;
use kika_git::safe_text::terminal_safe;

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

/// Read pull request `number`'s review threads and conversation; always
/// answered by one `ReviewIntent::DiscussionLoaded`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn load_discussion(
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
                gh.fetch_discussion(repo, number)
            })
        },
        // A panic still answers, or the threads would stay loading.
        |_| (None, Err(ReviewError::Status(ForgeStatus::Failed))),
        move |(repo, result)| {
            AppIntent::Review(ReviewIntent::DiscussionLoaded {
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

/// Plan checking out `head` as `branch`, and find the remote a missing
/// head would be fetched from. Local only: nothing is fetched here.
/// Always answered by one `ReviewIntent::CheckoutPlanned`.
pub(crate) fn plan_checkout(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    head: CommitId,
    branch: BranchName,
    hosts: Vec<ForgeHost>,
    repo: ForgeRepo,
) {
    background::run(
        tx,
        move || {
            let mut plan = pr_objects::plan_checkout(runner.as_ref(), &head, &branch)?;
            if !plan.head_here {
                plan.remote = review::fetch_remote(runner.as_ref(), &hosts, &repo);
            }
            Ok(plan)
        },
        // A panic still answers, or the checkout would wait forever.
        background::crashed("git"),
        move |result| AppIntent::Review(ReviewIntent::CheckoutPlanned { tag, result }),
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

/// List the commits of the pull request's `range`; always answered by one
/// `ReviewIntent::CommitsLoaded`.
pub(crate) fn load_commits(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    range: CommitRange,
) {
    background::run(
        tx,
        move || pr_objects::pr_commits(runner.as_ref(), &range),
        background::crashed("git log"),
        move |result| AppIntent::Review(ReviewIntent::CommitsLoaded { tag, result }),
    );
}

/// List the files `range` changes; always answered by one
/// `ReviewIntent::FilesLoaded` carrying the range back.
pub(crate) fn load_range_files(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    range: CommitRange,
) {
    background::run(
        tx,
        move || {
            let rows = pr_objects::range_files(runner.as_ref(), &range)?;
            Ok((range, rows))
        },
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

/// Mark `path` of pull request `number` viewed or not through `gh`;
/// always answered by one `ReviewIntent::ViewedSet`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn set_file_viewed(
    forge: Option<ForgeContext>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    host: ForgeHost,
    repo: ForgeRepo,
    node_id: String,
    number: u64,
    path: String,
    viewed: bool,
) {
    let sent_to = host.clone();
    let answer = move |path: String, result| {
        AppIntent::Review(ReviewIntent::ViewedSet {
            tag,
            host: sent_to.clone(),
            number,
            path,
            viewed,
            result,
        })
    };
    // No forge client: nothing may reach the network. The client itself
    // refuses a repository `[forge.allow]` leaves out.
    let Some(forge) = forge else {
        let _ = tx.send(answer(path, Err(CreateError::Status(ForgeStatus::Failed))));
        return;
    };
    let sent = path.clone();
    background::run(
        tx,
        move || forge.set_file_viewed(&host, &repo, &node_id, &sent, viewed),
        // A panic still answers, or the mark would stay in flight.
        |_| Err(CreateError::Status(ForgeStatus::Failed)),
        move |result| answer(path, result),
    );
}

/// Write `write` to pull request `number`'s discussion through `gh`;
/// always answered by one `ReviewIntent::DiscussionWritten`. It is never
/// tried again: a post that timed out may still have been made.
pub(crate) fn write_discussion(
    forge: Option<ForgeContext>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    host: ForgeHost,
    repo: ForgeRepo,
    number: u64,
    write: DiscussionWrite,
) {
    let sent_to = host.clone();
    let answer = move |write: DiscussionWrite, result| {
        AppIntent::Review(ReviewIntent::DiscussionWritten {
            tag,
            host: sent_to.clone(),
            number,
            write,
            result,
        })
    };
    // No forge client: nothing may reach the network. The client itself
    // refuses a repository `[forge.allow]` leaves out.
    let Some(forge) = forge else {
        let _ = tx.send(answer(write, Err(CreateError::Status(ForgeStatus::Failed))));
        return;
    };
    let sent = write.clone();
    background::run(
        tx,
        move || forge.write_discussion(&host, &repo, &sent),
        // A panic still answers, or the write would stay in flight.
        |_| Err(CreateError::Status(ForgeStatus::Failed)),
        move |result| answer(write, result),
    );
}

/// Read the pending review stored for `key` in `store`; with no store
/// there is none. Local files only. Always answered by one
/// `ReviewIntent::PendingLoaded`.
pub(crate) fn load_pending(
    store: Option<PathBuf>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    key: PendingKey,
) {
    let file = store.as_ref().map(|dir| dir.join(key.file_name()));
    background::run(
        tx,
        move || match store {
            Some(dir) => pending::read(&dir, &key),
            None => Ok(None),
        },
        |_| Err(PendingError::Unreadable("reading it crashed")),
        move |result| AppIntent::Review(ReviewIntent::PendingLoaded { tag, result, file }),
    );
}

/// The newest save written to each pending-review file this run, so a
/// save that lost a race with a later one never puts older comments back.
static WRITTEN: Mutex<Vec<(PathBuf, u64)>> = Mutex::new(Vec::new());

/// Write `review` for `key` in `store`. Saves of one file run one at a
/// time, and one older (by `tag.seq`) than a save already written is
/// skipped. Local files only. Always answered by one
/// `ReviewIntent::PendingSaved`.
pub(crate) fn save_pending(
    store: Option<PathBuf>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    key: PendingKey,
    review: PendingReview,
) {
    background::run(
        tx,
        move || {
            let Some(dir) = store else {
                return Err("No place to keep the pending review".to_string());
            };
            let file = dir.join(key.file_name());
            let mut written = WRITTEN.lock().unwrap_or_else(|e| e.into_inner());
            if written.iter().any(|(f, seq)| *f == file && *seq > tag.seq) {
                return Ok(());
            }
            let result = pending::write(&dir, &key, &review)
                .map_err(|e| terminal_safe(e.to_string().as_bytes()));
            written.retain(|(f, _)| *f != file);
            written.push((file, tag.seq));
            result
        },
        background::crashed("Saving the pending review"),
        move |result| AppIntent::Review(ReviewIntent::PendingSaved { tag, result }),
    );
}

/// Remove the file a comment was written in, with its directory and
/// whatever the editor left beside it. Nobody is told: the comment was
/// already used, and a leftover is a private temp dir.
pub(crate) fn remove_comment_file(tx: UnboundedSender<AppIntent>, file: KeptFile) {
    background::run_with(
        tx,
        move || {
            let _ = std::fs::remove_dir_all(&file.dir);
        },
        |_| None,
    );
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::mpsc;

    use crate::effect::{Effect, Runtime};
    use crate::features::review::ReviewIntent;
    use crate::intent::AppIntent;
    use crate::test_support::forge_tag;
    use kika_forge::merge::MergeMethod;
    use kika_forge::review::ReviewError;
    use kika_forge::task::ForgeContext;
    use kika_forge::{CreateError, ForgeHost, ForgeRepo, ForgeStatus};
    use kika_git::commit_files::CommitId;
    use kika_git::fake::FakeRunner;
    use kika_git::recording::CommandSink;

    #[tokio::test(flavor = "multi_thread")]
    async fn removing_a_comment_file_takes_its_directory_and_tells_nobody() {
        let tmp = kika_git::fake::TempDir::new("comment-file");
        let dir = tmp.path().join("kika-comment-1-0");
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("kika-comment-1-0.md");
        std::fs::write(&path, "text").unwrap();
        std::fs::write(dir.join("kika-comment-1-0.md.swp"), "swap").unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(FakeRunner::new()), tx);

        runtime.spawn(Effect::RemoveCommentFile(crate::handoff::KeptFile {
            dir: dir.clone(),
            path,
        }));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while dir.exists() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(!dir.exists(), "{dir:?} was left behind");
        drop(runtime);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(rx.try_recv().is_err(), "an intent was sent");
    }

    #[tokio::test]
    async fn review_loads_answer_once_and_reach_nobody_without_a_forge_client() {
        use kika_forge::github::{FakeGh, answer};
        use kika_forge::review::fixtures::DETAIL;
        use kika_git::recording::CommandSink;

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
            glab: Arc::new(kika_forge::gitlab::FakeGlab::new()),
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
    async fn a_detail_load_reads_its_later_pages_of_marks_and_answers_once() {
        use kika_forge::allow::AllowRule;
        use kika_forge::github::{FakeGh, answer};
        use kika_forge::review::fixtures::{detail_with_files, files_page};

        let github = vec![ForgeHost::parse("github.com").unwrap()];
        let repo = ForgeRepo {
            host: "github.com".into(),
            owner: "acme".into(),
            name: "widgets".into(),
        };
        let load = |hosts: Vec<ForgeHost>| Effect::LoadPrDetail {
            tag: forge_tag(),
            hosts,
            repo: Some(repo.clone()),
            number: 42,
            limited: vec![],
        };
        let context = |gh: &Arc<FakeGh>| {
            Some(ForgeContext {
                gh: gh.clone(),
                glab: Arc::new(kika_forge::gitlab::FakeGlab::new()),
                sink: CommandSink::new(),
                cache_dir: None,
            })
        };
        let git = Arc::new(FakeRunner::new());
        let gh = Arc::new(
            FakeGh::new()
                .reply(answer(&detail_with_files(0..100, 130, Some("c1"))))
                .reply(answer(&files_page(100..130, None))),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        Runtime::new(git.clone(), tx)
            .with_forge(context(&gh))
            .spawn(load(github.clone()));
        match rx.recv().await {
            Some(AppIntent::Review(ReviewIntent::PrDetailLoaded {
                result: Ok(Some(detail)),
                ..
            })) => {
                assert_eq!(detail.viewed.len(), 130);
                assert_eq!(detail.viewed_gap, None);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(gh.calls().len(), 2);
        assert!(rx.try_recv().is_err(), "one answer only");

        // No forge client, or a repository the allow-list leaves out:
        // nobody is asked, not even for the first page.
        let gh = Arc::new(FakeGh::new());
        let narrowed = vec![
            github[0]
                .clone()
                .with_allow(vec![AllowRule::parse("other").unwrap()]),
        ];
        for (forge, hosts) in [(None, github.clone()), (context(&gh), narrowed)] {
            let (tx, mut rx) = mpsc::unbounded_channel();
            Runtime::new(git.clone(), tx)
                .with_forge(forge)
                .spawn(load(hosts));
            match rx.recv().await {
                Some(AppIntent::Review(ReviewIntent::PrDetailLoaded { result, .. })) => {
                    assert_eq!(result, Err(ReviewError::NoForgeHost));
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(gh.calls(), []);
        git.assert_done();
    }

    #[tokio::test]
    async fn a_discussion_load_answers_once_and_reaches_gh_only_on_an_allowed_host() {
        use kika_forge::allow::AllowRule;
        use kika_forge::discussion::fixtures::DISCUSSION;
        use kika_forge::github::{FakeGh, answer};

        let github = ForgeHost::parse("github.com").unwrap();
        let repo = ForgeRepo {
            host: "github.com".into(),
            owner: "acme".into(),
            name: "widgets".into(),
        };
        let load = |hosts: Vec<ForgeHost>| Effect::LoadPrDiscussion {
            tag: forge_tag(),
            hosts,
            repo: Some(repo.clone()),
            number: 42,
            limited: vec![],
        };
        let gh = Arc::new(FakeGh::new().reply(answer(DISCUSSION)));
        let client = || {
            Some(ForgeContext {
                gh: gh.clone(),
                glab: Arc::new(kika_forge::gitlab::FakeGlab::new()),
                sink: CommandSink::new(),
                cache_dir: None,
            })
        };
        let git = Arc::new(FakeRunner::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        Runtime::new(git.clone(), tx)
            .with_forge(client())
            .spawn(load(vec![github.clone()]));
        match rx.recv().await {
            Some(AppIntent::Review(ReviewIntent::DiscussionLoaded {
                tag,
                number: 42,
                repo: Some(found),
                result: Ok(Some(discussion)),
            })) => {
                assert_eq!(tag, forge_tag());
                assert_eq!(found, repo);
                assert_eq!(discussion.threads.len(), 5);
                assert_eq!(discussion.conversation.len(), 3);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(gh.calls().len(), 1);

        // No forge client, no host, or a host whose allow-list leaves the
        // repository out: nobody is asked.
        let refused = Some(AppIntent::Review(ReviewIntent::DiscussionLoaded {
            tag: forge_tag(),
            number: 42,
            repo: None,
            result: Err(ReviewError::NoForgeHost),
        }));
        let (tx, mut rx) = mpsc::unbounded_channel();
        Runtime::new(git.clone(), tx).spawn(load(vec![github.clone()]));
        assert_eq!(rx.recv().await, refused);
        let left_out = github
            .clone()
            .with_allow(vec![AllowRule::parse("other").unwrap()]);
        for hosts in [vec![], vec![left_out]] {
            let (tx, mut rx) = mpsc::unbounded_channel();
            Runtime::new(git.clone(), tx)
                .with_forge(client())
                .spawn(load(hosts));
            assert_eq!(rx.recv().await, refused);
        }
        assert_eq!(gh.calls().len(), 1);
        git.assert_done();
    }

    #[tokio::test]
    async fn a_merge_runs_gh_in_the_cache_dir_and_answers_once() {
        use kika_forge::github::{FakeGh, GhCall, answer};

        let tmp = kika_git::fake::TempDir::new("forge-effect-merge");
        let cache_dir = tmp.path().join("forge");
        let git = Arc::new(FakeRunner::new());
        let gh = Arc::new(FakeGh::new().reply(answer(
            "HTTP/2.0 200 OK\r\n\r\n{\"merged\":true,\"message\":\"Pull Request successfully merged\"}",
        )));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(git.clone(), tx).with_forge(Some(ForgeContext {
            gh: gh.clone(),
            glab: Arc::new(kika_forge::gitlab::FakeGlab::new()),
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
        use kika_forge::allow::AllowRule;
        use kika_forge::github::FakeGh;

        let git = Arc::new(FakeRunner::new());
        let gh = Arc::new(FakeGh::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(git.clone(), tx).with_forge(Some(ForgeContext {
            gh: gh.clone(),
            glab: Arc::new(kika_forge::gitlab::FakeGlab::new()),
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

    #[tokio::test]
    async fn a_viewed_mark_answers_once_and_reaches_gh_only_through_the_client() {
        use kika_forge::allow::AllowRule;
        use kika_forge::github::{FakeGh, GhCall, answer};

        let git = Arc::new(FakeRunner::new());
        let gh = Arc::new(FakeGh::new().reply(answer(
            "HTTP/2.0 200 OK\r\n\r\n{\"data\":{\"unmarkFileAsViewed\":{\"clientMutationId\":null}}}",
        )));
        let client = || {
            Some(ForgeContext {
                gh: gh.clone(),
                glab: Arc::new(kika_forge::gitlab::FakeGlab::new()),
                sink: CommandSink::new(),
                cache_dir: None,
            })
        };
        let mark = |host: ForgeHost| Effect::SetFileViewed {
            tag: forge_tag(),
            host,
            repo: ForgeRepo {
                host: "github.com".into(),
                owner: "acme".into(),
                name: "widgets".into(),
            },
            node_id: "PR_kwDOA1".into(),
            number: 42,
            path: "src/a.rs".into(),
            viewed: false,
        };
        // The answer names the host the mark was sent to.
        let answered = |host: &ForgeHost, result| {
            Some(AppIntent::Review(ReviewIntent::ViewedSet {
                tag: forge_tag(),
                host: host.clone(),
                number: 42,
                path: "src/a.rs".into(),
                viewed: false,
                result,
            }))
        };
        let github = ForgeHost::parse("github.com").unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        Runtime::new(git.clone(), tx)
            .with_forge(client())
            .spawn(mark(github.clone()));
        assert_eq!(rx.recv().await, answered(&github, Ok(())));
        let [call] = gh.calls().try_into().unwrap();
        let GhCall::Graphql { body } = &call.kind else {
            panic!("{call:?}");
        };
        let sent: serde_json::Value = serde_json::from_slice(body).unwrap();
        assert_eq!(sent["variables"]["path"], "src/a.rs");

        // Without a forge client, or for a repository `[forge.allow]`
        // leaves out, nothing is asked.
        let failed = |host| answered(host, Err(CreateError::Status(ForgeStatus::Failed)));
        let (tx, mut rx) = mpsc::unbounded_channel();
        Runtime::new(git.clone(), tx).spawn(mark(github.clone()));
        assert_eq!(rx.recv().await, failed(&github));
        let left_out = ForgeHost::parse("github.com")
            .unwrap()
            .with_allow(vec![AllowRule::parse("other").unwrap()]);
        let (tx, mut rx) = mpsc::unbounded_channel();
        Runtime::new(git.clone(), tx)
            .with_forge(client())
            .spawn(mark(left_out.clone()));
        assert_eq!(rx.recv().await, failed(&left_out));
        assert_eq!(gh.calls().len(), 1);
        git.assert_done();
    }

    #[tokio::test]
    async fn a_discussion_write_answers_once_and_reaches_gh_only_through_the_client() {
        use kika_forge::allow::AllowRule;
        use kika_forge::discussion_write::{DiscussionWrite, Written};
        use kika_forge::github::{FakeGh, GhCall, answer};

        let git = Arc::new(FakeRunner::new());
        let gh = Arc::new(FakeGh::new().reply(answer(
            "HTTP/2.0 200 OK\r\n\r\n{\"data\":{\"addPullRequestReviewThreadReply\":{\"comment\":{\"id\":\"PRRC_1\"}}}}",
        )));
        let client = || {
            Some(ForgeContext {
                gh: gh.clone(),
                glab: Arc::new(kika_forge::gitlab::FakeGlab::new()),
                sink: CommandSink::new(),
                cache_dir: None,
            })
        };
        let reply = DiscussionWrite::Reply {
            thread: "PRRT_kwDOA1".into(),
            body: "Done".into(),
        };
        let send = |host: ForgeHost| Effect::WriteDiscussion {
            tag: forge_tag(),
            host,
            repo: ForgeRepo {
                host: "github.com".into(),
                owner: "acme".into(),
                name: "widgets".into(),
            },
            number: 42,
            write: reply.clone(),
        };
        // The answer names the host the write was sent to.
        let answered = |host: &ForgeHost, result| {
            Some(AppIntent::Review(ReviewIntent::DiscussionWritten {
                tag: forge_tag(),
                host: host.clone(),
                number: 42,
                write: reply.clone(),
                result,
            }))
        };
        let github = ForgeHost::parse("github.com").unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        Runtime::new(git.clone(), tx)
            .with_forge(client())
            .spawn(send(github.clone()));
        assert_eq!(rx.recv().await, answered(&github, Ok(Written::Posted)));
        let [call] = gh.calls().try_into().unwrap();
        let GhCall::Graphql { body } = &call.kind else {
            panic!("{call:?}");
        };
        let sent: serde_json::Value = serde_json::from_slice(body).unwrap();
        assert_eq!(sent["variables"]["id"], "PRRT_kwDOA1");
        assert_eq!(sent["variables"]["body"], "Done");

        // Without a forge client, or for a repository `[forge.allow]`
        // leaves out, nothing is asked.
        let failed = |host| answered(host, Err(CreateError::Status(ForgeStatus::Failed)));
        let (tx, mut rx) = mpsc::unbounded_channel();
        Runtime::new(git.clone(), tx).spawn(send(github.clone()));
        assert_eq!(rx.recv().await, failed(&github));
        let left_out = ForgeHost::parse("github.com")
            .unwrap()
            .with_allow(vec![AllowRule::parse("other").unwrap()]);
        let (tx, mut rx) = mpsc::unbounded_channel();
        Runtime::new(git.clone(), tx)
            .with_forge(client())
            .spawn(send(left_out.clone()));
        assert_eq!(rx.recv().await, failed(&left_out));
        assert_eq!(gh.calls().len(), 1);
        git.assert_done();
    }
}
