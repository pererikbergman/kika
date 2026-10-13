//! The I/O behind the sync feature's effects: `Runtime::spawn` calls these
//! with its runner and sender.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use super::git::run_fetch;
use crate::background;
use crate::effect::{AnswerSlot, Bridge, Tag};
use crate::git::GitRunner;
use crate::git::diff::terminal_safe;
use crate::git::remote::{RemoteFailure, RemoteOp, run_remote};
use crate::intent::AppIntent;

/// Fetch every remote quietly and send the result back as `FetchFinished`.
pub(crate) fn fetch(runner: Arc<dyn GitRunner>, tx: UnboundedSender<AppIntent>, tag: Tag) {
    background::run(
        tx,
        move || run_fetch(runner.as_ref()),
        // A panic still reports back, or auto-fetch would stop for good.
        background::crashed("git fetch"),
        move |result| AppIntent::FetchFinished { tag, result },
    );
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
    let bridge_tx = tx.clone();
    let task_op = op.clone();
    background::run(
        tx,
        move || {
            let bridge = Bridge::open(tag, bridge_tx, slot);
            run_remote(runner.as_ref(), &task_op, bridge.env.as_ref())
        },
        // A panic still reports back, or the op slot would block every later op.
        |e| {
            Err(RemoteFailure::Failed(terminal_safe(
                format!("git crashed: {e}").as_bytes(),
            )))
        },
        move |result| AppIntent::RemoteFinished { tag, op, result },
    );
}
