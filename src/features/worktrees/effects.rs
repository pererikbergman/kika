//! The I/O behind the Worktrees popup's effects: listing, adding and
//! removing worktrees off the event loop, each reporting back with its tag.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use super::git::{add_worktree, read_worktrees, remove_worktree};
use crate::effect::Tag;
use crate::git::GitRunner;
use crate::git::branches::BranchName;
use crate::git::diff::terminal_safe;
use crate::git::path::RepoPath;
use crate::intent::AppIntent;

/// List the worktrees and report them as `WorktreesLoaded`.
pub(crate) fn load(runner: Arc<dyn GitRunner>, tx: UnboundedSender<AppIntent>, tag: Tag) {
    tokio::spawn(async move {
        // A panic still reports back, or the busy counter would stay raised.
        let result = tokio::task::spawn_blocking(move || read_worktrees(runner.as_ref()))
            .await
            .unwrap_or_else(|e| {
                Err(terminal_safe(
                    format!("git worktree list crashed: {e}").as_bytes(),
                ))
            });
        let _ = tx.send(AppIntent::WorktreesLoaded { tag, result });
    });
}

/// Add a worktree for `branch` at `path` and report it as `WorktreeAdded`.
pub(crate) fn add(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    path: String,
    branch: BranchName,
) {
    tokio::spawn(async move {
        // A panic still reports back, or the busy counter would stay raised.
        let result =
            tokio::task::spawn_blocking(move || add_worktree(runner.as_ref(), &path, &branch))
                .await
                .unwrap_or_else(|e| {
                    Err(terminal_safe(
                        format!("git worktree add crashed: {e}").as_bytes(),
                    ))
                });
        let _ = tx.send(AppIntent::WorktreeAdded { tag, result });
    });
}

/// Remove the worktree at `path`, never forced, and report it as
/// `WorktreeRemoved`.
pub(crate) fn remove(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    path: RepoPath,
) {
    tokio::spawn(async move {
        // A panic still reports back, or the busy counter would stay raised.
        let result = tokio::task::spawn_blocking(move || remove_worktree(runner.as_ref(), &path))
            .await
            .unwrap_or_else(|e| {
                Err(terminal_safe(
                    format!("git worktree remove crashed: {e}").as_bytes(),
                ))
            });
        let _ = tx.send(AppIntent::WorktreeRemoved { tag, result });
    });
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::mpsc;

    use crate::effect::{Effect, Runtime, Tag};
    use crate::git::fake::{FakeRunner, fail, ok};
    use crate::intent::AppIntent;

    #[tokio::test]
    async fn worktree_effects_report_back_with_the_same_tag() {
        let runner = Arc::new(
            FakeRunner::new()
                .expect(
                    &["worktree", "list", "--porcelain", "-z"],
                    ok(b"worktree /r\0bare\0\0"),
                )
                .expect(
                    &["worktree", "remove", "--", "/r/x"],
                    fail("fatal: '/r/x' is locked\n"),
                ),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 1,
            seq: 4,
            user: true,
        };

        runtime.spawn(Effect::LoadWorktrees { tag });
        match rx.recv().await {
            Some(AppIntent::WorktreesLoaded {
                tag: got,
                result: Ok(list),
            }) => {
                assert_eq!(got, tag);
                assert_eq!(list[0].path.display(), "/r");
            }
            other => panic!("expected WorktreesLoaded, got {other:?}"),
        }
        runtime.spawn(Effect::RemoveWorktree {
            tag,
            path: "/r/x".into(),
        });
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::WorktreeRemoved {
                tag,
                result: Err("fatal: '/r/x' is locked".into()),
            })
        );
        runner.assert_done();
    }

    #[tokio::test]
    async fn a_failing_worktree_add_still_reports_back() {
        // No workdir and no answers: the git-dir read fails, which must
        // still answer once.
        let runner = Arc::new(FakeRunner::new().expect(
            &["rev-parse", "--git-dir", "--git-common-dir"],
            fail("fatal: not a git repository\n"),
        ));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 0,
            seq: 1,
            user: true,
        };
        runtime.spawn(Effect::AddWorktree {
            tag,
            path: "../x".into(),
            branch: "feature".into(),
        });
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::WorktreeAdded {
                tag,
                result: Err("fatal: not a git repository".into()),
            })
        );
        runner.assert_done();
    }
}
