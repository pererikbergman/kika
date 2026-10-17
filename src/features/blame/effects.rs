//! The I/O behind the Blame feature's effects: reading a file's blame and
//! a line's commit off the event loop, each reporting back with its tag.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use crate::background;
use crate::effect::Tag;
use crate::features::blame::BlameIntent;
use crate::intent::AppIntent;
use kika_git::GitRunner;
use kika_git::blame::{Blamed, read_blame, read_commit};
use kika_git::diff::EmojiZwj;
use kika_git::path::RepoPath;

/// Blame `path` and report it as `BlameIntent::Loaded`, or as
/// `BlameIntent::Binary` when there is no text to show.
pub(crate) fn load(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    path: RepoPath,
    emoji: EmojiZwj,
) {
    background::run(
        tx,
        move || read_blame(runner.as_ref(), &path, emoji),
        // A panic still reports back, or the busy counter would stay raised.
        background::crashed("git blame"),
        move |result| {
            AppIntent::Blame(match result {
                Ok(Blamed::Lines(lines)) => BlameIntent::Loaded {
                    tag,
                    result: Ok(lines),
                },
                Ok(Blamed::Binary) => BlameIntent::Binary { tag },
                Err(e) => BlameIntent::Loaded {
                    tag,
                    result: Err(e),
                },
            })
        },
    );
}

/// Read commit `oid` and report it as `BlameIntent::CommitLoaded`.
pub(crate) fn load_commit(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    oid: String,
    emoji: EmojiZwj,
) {
    background::run(
        tx,
        move || read_commit(runner.as_ref(), &oid, emoji),
        // A panic still reports back, or the busy counter would stay raised.
        background::crashed("git show"),
        move |result| AppIntent::Blame(BlameIntent::CommitLoaded { tag, result }),
    );
}
