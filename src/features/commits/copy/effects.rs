//! The I/O behind copy's effect: `Runtime::spawn` calls this with its
//! runner and sender.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use crate::background;
use crate::effect::Tag;
use crate::git::GitRunner;
use crate::git::commit_files::CommitId;
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
    let read = oid.clone();
    background::run(
        tx,
        move || read_commit_message(runner.as_ref(), &read),
        // A panic still reports back, or the busy counter would stay raised.
        background::crashed("git log"),
        move |result| AppIntent::CommitMessageLoaded { tag, oid, result },
    );
}
