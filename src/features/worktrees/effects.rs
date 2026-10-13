//! The I/O behind the Worktrees popup's effects: listing, adding and
//! removing worktrees off the event loop, each reporting back with its tag.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use super::git::{add_worktree, read_worktrees, remove_worktree};
use crate::background;
use crate::effect::Tag;
use crate::features::worktrees::WorktreesIntent;
use crate::git::GitRunner;
use crate::git::branches::BranchName;
use crate::git::path::RepoPath;
use crate::intent::AppIntent;

/// List the worktrees and report them as `WorktreesIntent::Loaded`.
pub(crate) fn load(runner: Arc<dyn GitRunner>, tx: UnboundedSender<AppIntent>, tag: Tag) {
    background::run(
        tx,
        move || read_worktrees(runner.as_ref()),
        // A panic still reports back, or the busy counter would stay raised.
        background::crashed("git worktree list"),
        move |result| AppIntent::Worktrees(WorktreesIntent::Loaded { tag, result }),
    );
}

/// Add a worktree for `branch` at `path` and report it as `WorktreesIntent::Added`.
pub(crate) fn add(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    path: String,
    branch: BranchName,
) {
    background::run(
        tx,
        move || add_worktree(runner.as_ref(), &path, &branch),
        // A panic still reports back, or the busy counter would stay raised.
        background::crashed("git worktree add"),
        move |result| AppIntent::Worktrees(WorktreesIntent::Added { tag, result }),
    );
}

/// Remove the worktree at `path`, never forced, and report it as
/// `WorktreesIntent::Removed`.
pub(crate) fn remove(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    path: RepoPath,
) {
    background::run(
        tx,
        move || remove_worktree(runner.as_ref(), &path),
        // A panic still reports back, or the busy counter would stay raised.
        background::crashed("git worktree remove"),
        move |result| AppIntent::Worktrees(WorktreesIntent::Removed { tag, result }),
    );
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::mpsc;

    use crate::effect::{Effect, Runtime, Tag};
    use crate::features::worktrees::WorktreesIntent;
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
            Some(AppIntent::Worktrees(WorktreesIntent::Loaded {
                tag: got,
                result: Ok(list),
            })) => {
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
            Some(AppIntent::Worktrees(WorktreesIntent::Removed {
                tag,
                result: Err("fatal: '/r/x' is locked".into()),
            }))
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
            Some(AppIntent::Worktrees(WorktreesIntent::Added {
                tag,
                result: Err("fatal: not a git repository".into()),
            }))
        );
        runner.assert_done();
    }
}
