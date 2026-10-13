//! The I/O behind the Files panel's effect: reading the working tree's
//! status off the event loop and reporting back with the load's tag.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use crate::effect::Tag;
use crate::git::GitRunner;
use crate::git::status::read_status;
use crate::intent::AppIntent;

/// Read the status and report it as `StatusLoaded`.
pub(crate) fn load(runner: Arc<dyn GitRunner>, tx: UnboundedSender<AppIntent>, tag: Tag) {
    tokio::spawn(async move {
        // A panic still reports back, or the busy counter would stay raised.
        let result = tokio::task::spawn_blocking(move || read_status(runner.as_ref()))
            .await
            .unwrap_or_else(|e| Err(format!("git status crashed: {e}")));
        // A closed channel means the app is quitting; nothing to report to.
        let _ = tx.send(AppIntent::StatusLoaded { tag, result });
    });
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::mpsc;

    use crate::effect::{Effect, Runtime, Tag};
    use crate::git::FileEntry;
    use crate::git::fake::{FakeRunner, ok};
    use crate::git::status::{BranchStatus, EntryKind, Head, RepoStatus};
    use crate::intent::AppIntent;

    #[tokio::test]
    async fn load_status_reports_back_with_the_same_tag() {
        let runner = Arc::new(
            FakeRunner::new()
                .expect(
                    &[
                        "status",
                        "--porcelain=v2",
                        "--branch",
                        "-z",
                        "--untracked-files=all",
                    ],
                    ok(b"# branch.oid 1234567\0# branch.head main\0? new_file\0"),
                )
                .expect(
                    &["rev-parse", "--absolute-git-dir"],
                    ok(b"/no/such/kika/repo/.git\n"),
                ),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 2,
            seq: 5,
            user: true,
        };

        runtime.spawn(Effect::LoadStatus { tag });

        assert_eq!(
            rx.recv().await,
            Some(AppIntent::StatusLoaded {
                tag,
                result: Ok(RepoStatus {
                    branch: BranchStatus {
                        head: Head::Branch("main".into()),
                        upstream: None,
                        ahead_behind: None,
                        in_progress: vec![],
                        bisect: None,
                    },
                    files: vec![FileEntry {
                        path: "new_file".into(),
                        orig_path: None,
                        index: '?',
                        worktree: '?',
                        kind: EntryKind::Untracked,
                    }],
                }),
            })
        );
        runner.assert_done();
    }
}
