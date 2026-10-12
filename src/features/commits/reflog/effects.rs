//! The I/O behind the reflog's effect: reading `HEAD`'s reflog off the
//! event loop and reporting back with the load's tag.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use super::git::read_reflog;
use crate::effect::Tag;
use crate::git::GitRunner;
use crate::git::diff::terminal_safe;
use crate::intent::AppIntent;

/// Read the reflog and report it as `ReflogLoaded`.
pub(crate) fn load(runner: Arc<dyn GitRunner>, tx: UnboundedSender<AppIntent>, tag: Tag) {
    tokio::spawn(async move {
        // A panic still reports back, or an undo would wait forever.
        let result = tokio::task::spawn_blocking(move || read_reflog(runner.as_ref()))
            .await
            .unwrap_or_else(|e| Err(terminal_safe(format!("git reflog crashed: {e}").as_bytes())));
        let _ = tx.send(AppIntent::ReflogLoaded { tag, result });
    });
}
