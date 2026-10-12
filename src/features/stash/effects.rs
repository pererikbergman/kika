//! The I/O behind the stash's effects: reading the stash list and one
//! entry's files off the event loop, each reporting back with its load's
//! tag.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use crate::effect::Tag;
use crate::git::GitRunner;
use crate::git::diff::terminal_safe;
use crate::git::stash::{StashEntry, list_stashes, stash_files};
use crate::intent::AppIntent;

/// Read the stash list and report it as `StashesLoaded`.
pub(crate) fn load(runner: Arc<dyn GitRunner>, tx: UnboundedSender<AppIntent>, tag: Tag) {
    tokio::spawn(async move {
        // A panic still reports back, or the panel would wait forever.
        let result = tokio::task::spawn_blocking(move || list_stashes(runner.as_ref()))
            .await
            .unwrap_or_else(|e| {
                Err(terminal_safe(
                    format!("git stash list crashed: {e}").as_bytes(),
                ))
            });
        let _ = tx.send(AppIntent::StashesLoaded { tag, result });
    });
}

/// List `entry`'s files and report them as `StashFilesLoaded`.
pub(crate) fn load_files(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    entry: StashEntry,
) {
    tokio::spawn(async move {
        // A panic still reports back, or the busy counter would stay raised.
        let result = tokio::task::spawn_blocking(move || stash_files(runner.as_ref(), &entry))
            .await
            .unwrap_or_else(|e| {
                Err(terminal_safe(
                    format!("git diff-tree crashed: {e}").as_bytes(),
                ))
            });
        let _ = tx.send(AppIntent::StashFilesLoaded { tag, result });
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
    async fn stash_loads_report_back_with_the_same_tag_even_on_a_panic() {
        use crate::git::commit_files::CommitId;
        let tag = Tag {
            generation: 3,
            seq: 9,
            user: true,
        };
        let runner = Arc::new(
            FakeRunner::new()
                .expect(
                    &["stash", "list", "-z", "--format=%H%x1f%P%x1f%gs"],
                    ok(b"aa\x1fbb cc\x1fOn main: wip\0"),
                )
                .expect(
                    &[
                        "diff-tree",
                        "-r",
                        "-z",
                        "--no-commit-id",
                        "--name-status",
                        "-M",
                        "--root",
                        "bb",
                        "aa",
                        "--",
                    ],
                    ok(b"M\0a.rs\0"),
                ),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);

        runtime.spawn(Effect::LoadStashes { tag });
        let Some(AppIntent::StashesLoaded {
            tag: got,
            result: Ok(entries),
        }) = rx.recv().await
        else {
            panic!("expected StashesLoaded");
        };
        assert_eq!(got, tag);
        assert_eq!(entries[0].oid, CommitId::parse("aa").unwrap());

        runtime.spawn(Effect::LoadStashFiles {
            tag,
            entry: entries[0].clone(),
        });
        let Some(AppIntent::StashFilesLoaded {
            tag: got,
            result: Ok(files),
        }) = rx.recv().await
        else {
            panic!("expected StashFilesLoaded");
        };
        assert_eq!(got, tag);
        assert_eq!(files[0].file.path, "a.rs".into());
        runner.assert_done();

        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(HostilePanic), tx);
        runtime.spawn(Effect::LoadStashes { tag });
        match rx.recv().await {
            Some(AppIntent::StashesLoaded {
                tag: got,
                result: Err(message),
            }) => {
                assert_eq!(got, tag);
                assert!(
                    message.starts_with("git stash list crashed: "),
                    "{message:?}"
                );
            }
            other => panic!("expected a failed StashesLoaded, got {other:?}"),
        }
        runtime.spawn(Effect::LoadStashFiles {
            tag,
            entry: entries[0].clone(),
        });
        match rx.recv().await {
            Some(AppIntent::StashFilesLoaded {
                tag: got,
                result: Err(message),
            }) => {
                assert_eq!(got, tag);
                assert!(!message.contains(char::is_control), "{message:?}");
            }
            other => panic!("expected a failed StashFilesLoaded, got {other:?}"),
        }
    }
}
