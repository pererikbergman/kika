//! The I/O behind the sync feature's effects: `Runtime::spawn` calls these
//! with its runner and sender.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use super::git::run_fetch;
use crate::effect::{AnswerSlot, Bridge, Tag};
use crate::git::GitRunner;
use crate::git::diff::terminal_safe;
use crate::git::remote::{RemoteFailure, RemoteOp, run_remote};
use crate::intent::AppIntent;

/// Fetch every remote quietly and send the result back as `FetchFinished`.
pub(crate) fn fetch(runner: Arc<dyn GitRunner>, tx: UnboundedSender<AppIntent>, tag: Tag) {
    tokio::spawn(async move {
        // A panic still reports back, or auto-fetch would stop for good.
        let result = tokio::task::spawn_blocking(move || run_fetch(runner.as_ref()))
            .await
            .unwrap_or_else(|e| Err(terminal_safe(format!("git fetch crashed: {e}").as_bytes())));
        let _ = tx.send(AppIntent::FetchFinished { tag, result });
    });
}

/// Run `op` with the askpass bridge open on `slot`, and send the result
/// back as `RemoteFinished`.
pub(crate) fn remote(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    op: RemoteOp,
    slot: AnswerSlot,
) {
    tokio::spawn(async move {
        let bridge_tx = tx.clone();
        // A panic still reports back, or `op_seq` would block every later op.
        let task_op = op.clone();
        let result = tokio::task::spawn_blocking(move || {
            let bridge = Bridge::open(tag, bridge_tx, slot);
            run_remote(runner.as_ref(), &task_op, bridge.env.as_ref())
        })
        .await
        .unwrap_or_else(|e| {
            Err(RemoteFailure::Failed(terminal_safe(
                format!("git crashed: {e}").as_bytes(),
            )))
        });
        let _ = tx.send(AppIntent::RemoteFinished { tag, op, result });
    });
}
