//! The I/O behind the stash's effects: reading the stash list and one
//! entry's files off the event loop, each reporting back with its load's
//! tag.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use super::git::{StashEntry, list_stashes, stash_files};
use crate::background;
use crate::effect::Tag;
use crate::features::stash::StashIntent;
use crate::intent::AppIntent;
use kika_git::GitRunner;

/// Read the stash list and report it as `StashIntent::Loaded`.
pub(crate) fn load(runner: Arc<dyn GitRunner>, tx: UnboundedSender<AppIntent>, tag: Tag) {
    background::run(
        tx,
        move || list_stashes(runner.as_ref()),
        // A panic still reports back, or the panel would wait forever.
        background::crashed("git stash list"),
        move |result| AppIntent::Stash(StashIntent::Loaded { tag, result }),
    );
}

/// List `entry`'s files and report them as `StashIntent::FilesLoaded`.
pub(crate) fn load_files(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    entry: StashEntry,
) {
    background::run(
        tx,
        move || stash_files(runner.as_ref(), &entry),
        // A panic still reports back, or the busy counter would stay raised.
        background::crashed("git diff-tree"),
        move |result| AppIntent::Stash(StashIntent::FilesLoaded { tag, result }),
    );
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::mpsc;

    use crate::effect::{Effect, Runtime, Tag};
    use crate::features::stash::StashIntent;
    use crate::intent::AppIntent;
    use crate::test_support::HostilePanic;
    use kika_git::fake::{FakeRunner, ok};

    #[tokio::test]
    async fn stash_loads_report_back_with_the_same_tag_even_on_a_panic() {
        use kika_git::commit_files::CommitId;
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
        let Some(AppIntent::Stash(StashIntent::Loaded {
            tag: got,
            result: Ok(entries),
        })) = rx.recv().await
        else {
            panic!("expected StashesLoaded");
        };
        assert_eq!(got, tag);
        assert_eq!(entries[0].oid, CommitId::parse("aa").unwrap());

        runtime.spawn(Effect::LoadStashFiles {
            tag,
            entry: entries[0].clone(),
        });
        let Some(AppIntent::Stash(StashIntent::FilesLoaded {
            tag: got,
            result: Ok(files),
        })) = rx.recv().await
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
            Some(AppIntent::Stash(StashIntent::Loaded {
                tag: got,
                result: Err(message),
            })) => {
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
            Some(AppIntent::Stash(StashIntent::FilesLoaded {
                tag: got,
                result: Err(message),
            })) => {
                assert_eq!(got, tag);
                assert!(!message.contains(char::is_control), "{message:?}");
            }
            other => panic!("expected a failed StashFilesLoaded, got {other:?}"),
        }
    }
}
