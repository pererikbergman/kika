//! The I/O behind the Submodules feature's effects: `Runtime::spawn` calls
//! these with its runner and sender.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use crate::effect::Tag;
use crate::git::GitRunner;
use crate::git::diff::terminal_safe;
use crate::git::submodule::read_submodules;
use crate::intent::AppIntent;

/// List the submodules and send them back as `SubmodulesLoaded`.
pub(crate) fn load(runner: Arc<dyn GitRunner>, tx: UnboundedSender<AppIntent>, tag: Tag) {
    tokio::spawn(async move {
        // A panic still reports back, or the busy counter would stay raised.
        let result = tokio::task::spawn_blocking(move || read_submodules(runner.as_ref()))
            .await
            .unwrap_or_else(|e| {
                Err(terminal_safe(
                    format!("git submodule crashed: {e}").as_bytes(),
                ))
            });
        let _ = tx.send(AppIntent::SubmodulesLoaded { tag, result });
    });
}
