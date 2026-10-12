//! The I/O behind the Branches feature's effects: listing the local
//! branches and reading the selected branch's log off the event loop, each
//! reporting back with the load's tag.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use crate::effect::Tag;
use crate::git::GitRunner;
use crate::git::branches::{branch_log, list_branches};
use crate::git::diff::terminal_safe;
use crate::intent::AppIntent;

/// List the local branches and report them as `BranchesLoaded`.
pub(crate) fn load(runner: Arc<dyn GitRunner>, tx: UnboundedSender<AppIntent>, tag: Tag) {
    tokio::spawn(async move {
        // A panic still reports back, or the busy counter would stay raised.
        let result = tokio::task::spawn_blocking(move || list_branches(runner.as_ref()))
            .await
            .unwrap_or_else(|e| {
                Err(terminal_safe(
                    format!("git for-each-ref crashed: {e}").as_bytes(),
                ))
            });
        let _ = tx.send(AppIntent::BranchesLoaded { tag, result });
    });
}

/// Read `refname`'s log and report it as `BranchLogLoaded`, naming the
/// ref so a log for a branch no longer selected is dropped.
pub(crate) fn load_log(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    refname: Vec<u8>,
) {
    tokio::spawn(async move {
        let read = crate::git::path::os_string(&refname);
        // A panic still reports back, or the busy counter would stay raised.
        let result = tokio::task::spawn_blocking(move || branch_log(runner.as_ref(), &read))
            .await
            .unwrap_or_else(|e| Err(terminal_safe(format!("git log crashed: {e}").as_bytes())));
        let _ = tx.send(AppIntent::BranchLogLoaded {
            tag,
            refname,
            result,
        });
    });
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::mpsc;

    use crate::effect::{Effect, Runtime, Tag};
    use crate::git::fake::{FakeRunner, ok};
    use crate::intent::AppIntent;
    use crate::test_support::HostilePanic;

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
            Some(AppIntent::BranchesLoaded {
                tag,
                result: Ok(vec![]),
            })
        );
        runtime.spawn(Effect::LoadBranchLog {
            tag,
            refname: b"refs/heads/main".to_vec(),
        });
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::BranchLogLoaded {
                tag,
                refname: b"refs/heads/main".to_vec(),
                result: Ok(vec![crate::git::branches::LogRow {
                    short_oid: "abc".into(),
                    date: "2024-01-02".into(),
                    author: "Ann".into(),
                    subject: "Work".into(),
                }]),
            })
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
            Some(AppIntent::BranchesLoaded { result: Err(_), .. })
        ));
        runtime.spawn(Effect::LoadBranchLog {
            tag,
            refname: b"refs/heads/x".to_vec(),
        });
        assert!(matches!(
            rx.recv().await,
            Some(AppIntent::BranchLogLoaded { result: Err(_), .. })
        ));
        runtime.spawn(Effect::LoadRemotes { tag });
        assert!(matches!(
            rx.recv().await,
            Some(AppIntent::RemotesLoaded { result: Err(_), .. })
        ));
        runtime.spawn(Effect::LoadTags { tag });
        assert!(matches!(
            rx.recv().await,
            Some(AppIntent::TagsLoaded { result: Err(_), .. })
        ));
    }
}
