//! The I/O behind the Branches feature's effects: listing the local
//! branches and reading the selected branch's log off the event loop, each
//! reporting back with the load's tag.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use super::BranchesIntent;
use crate::background;
use crate::effect::Tag;
use crate::intent::AppIntent;
use kika_git::GitRunner;
use kika_git::branches::{branch_log, list_branches};

/// List the local branches and report them as `BranchesIntent::Loaded`.
pub(crate) fn load(runner: Arc<dyn GitRunner>, tx: UnboundedSender<AppIntent>, tag: Tag) {
    background::run(
        tx,
        move || list_branches(runner.as_ref()),
        // A panic still reports back, or the busy counter would stay raised.
        background::crashed("git for-each-ref"),
        move |result| AppIntent::Branches(BranchesIntent::Loaded { tag, result }),
    );
}

/// Read `refname`'s log and report it as `BranchesIntent::LogLoaded`,
/// naming the ref so a log for a branch no longer selected is dropped.
pub(crate) fn load_log(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    refname: Vec<u8>,
) {
    let read = kika_git::path::os_string(&refname);
    background::run(
        tx,
        move || branch_log(runner.as_ref(), &read),
        // A panic still reports back, or the busy counter would stay raised.
        background::crashed("git log"),
        move |result| {
            AppIntent::Branches(BranchesIntent::LogLoaded {
                tag,
                refname,
                result,
            })
        },
    );
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::mpsc;

    use crate::effect::{Effect, Runtime, Tag};
    use crate::features::branches::BranchesIntent;
    use crate::features::branches::remotes::RemotesIntent;
    use crate::features::branches::tags::TagsIntent;
    use crate::intent::AppIntent;
    use crate::test_support::HostilePanic;
    use kika_git::fake::{FakeRunner, ok};

    #[tokio::test]
    async fn branch_loads_report_back_with_the_same_tag_and_name() {
        let runner = Arc::new(
            FakeRunner::new()
                .expect(
                    &[
                        "for-each-ref",
                        "--format=%(refname)%00%(HEAD)%00%(upstream:short)%00\
                         %(upstream:track,nobracket)%00%(committerdate:unix)",
                        "refs/heads",
                    ],
                    ok(b""),
                )
                .expect(
                    &[
                        "log",
                        "-n",
                        "200",
                        "--date=short",
                        "--format=%h%x1f%ad%x1f%an%x1f%s",
                        "refs/heads/main",
                        "--",
                    ],
                    ok(b"abc\x1f2024-01-02\x1fAnn\x1fWork\n"),
                ),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 1,
            seq: 4,
            user: true,
        };

        runtime.spawn(Effect::LoadBranches { tag });
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::Branches(BranchesIntent::Loaded {
                tag,
                result: Ok(vec![]),
            }))
        );
        runtime.spawn(Effect::LoadBranchLog {
            tag,
            refname: b"refs/heads/main".to_vec(),
        });
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::Branches(BranchesIntent::LogLoaded {
                tag,
                refname: b"refs/heads/main".to_vec(),
                result: Ok(vec![kika_git::branches::LogRow {
                    short_oid: "abc".into(),
                    date: "2024-01-02".into(),
                    author: "Ann".into(),
                    subject: "Work".into(),
                }]),
            }))
        );
        runner.assert_done();
    }

    #[tokio::test]
    async fn a_panicking_branch_load_still_reports_back() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(HostilePanic), tx);
        let tag = Tag {
            generation: 0,
            seq: 2,
            user: true,
        };
        runtime.spawn(Effect::LoadBranches { tag });
        assert!(matches!(
            rx.recv().await,
            Some(AppIntent::Branches(BranchesIntent::Loaded {
                result: Err(_),
                ..
            }))
        ));
        runtime.spawn(Effect::LoadBranchLog {
            tag,
            refname: b"refs/heads/x".to_vec(),
        });
        assert!(matches!(
            rx.recv().await,
            Some(AppIntent::Branches(BranchesIntent::LogLoaded {
                result: Err(_),
                ..
            }))
        ));
        runtime.spawn(Effect::LoadRemotes { tag });
        assert!(matches!(
            rx.recv().await,
            Some(AppIntent::Branches(BranchesIntent::Remotes(
                RemotesIntent::Loaded { result: Err(_), .. }
            )))
        ));
        runtime.spawn(Effect::LoadTags { tag });
        assert!(matches!(
            rx.recv().await,
            Some(AppIntent::Branches(BranchesIntent::Tags(
                TagsIntent::Loaded { result: Err(_), .. }
            )))
        ));
    }
}
