//! The I/O behind copy's effect: `Runtime::spawn` calls this with its
//! runner and sender.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use crate::effect::Tag;
use crate::git::GitRunner;
use crate::git::commit_files::CommitId;
use crate::git::diff::terminal_safe;
use crate::git::head::read_commit_message;
use crate::intent::AppIntent;

/// Read commit `oid`'s full message and send it back as
/// `CommitMessageLoaded`.
pub(crate) fn load_message(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    oid: CommitId,
) {
    tokio::spawn(async move {
        let read = oid.clone();
        // A panic still reports back, or the busy counter would stay raised.
        let result =
            tokio::task::spawn_blocking(move || read_commit_message(runner.as_ref(), &read))
                .await
                .unwrap_or_else(|e| Err(terminal_safe(format!("git log crashed: {e}").as_bytes())));
        let _ = tx.send(AppIntent::CommitMessageLoaded { tag, oid, result });
    });
}
