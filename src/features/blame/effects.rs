//! The I/O behind the Blame feature's effects: reading a file's blame and
//! a line's commit off the event loop, each reporting back with its tag.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use crate::effect::Tag;
use crate::git::GitRunner;
use crate::git::blame::{read_blame, read_commit};
use crate::git::diff::{EmojiZwj, terminal_safe};
use crate::git::path::RepoPath;
use crate::intent::AppIntent;

/// Blame `path` and report it as `BlameLoaded`.
pub(crate) fn load(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    path: RepoPath,
    emoji: EmojiZwj,
) {
    tokio::spawn(async move {
        // A panic still reports back, or the busy counter would stay raised.
        let result = tokio::task::spawn_blocking(move || read_blame(runner.as_ref(), &path, emoji))
            .await
            .unwrap_or_else(|e| Err(terminal_safe(format!("git blame crashed: {e}").as_bytes())));
        let _ = tx.send(AppIntent::BlameLoaded { tag, result });
    });
}

/// Read commit `oid` and report it as `CommitLoaded`.
pub(crate) fn load_commit(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    oid: String,
    emoji: EmojiZwj,
) {
    tokio::spawn(async move {
        // A panic still reports back, or the busy counter would stay raised.
        let result = tokio::task::spawn_blocking(move || read_commit(runner.as_ref(), &oid, emoji))
            .await
            .unwrap_or_else(|e| Err(terminal_safe(format!("git show crashed: {e}").as_bytes())));
        let _ = tx.send(AppIntent::CommitLoaded { tag, result });
    });
}
