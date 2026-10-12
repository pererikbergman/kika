//! The I/O behind compare's effect: `Runtime::spawn` calls this with its
//! runner and sender.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use crate::diff_mark::DiffEnd;
use crate::effect::Tag;
use crate::git::GitRunner;
use crate::git::commit_files::{CommitId, CommitRange};
use crate::git::diff::terminal_safe;
use crate::git::log::branch_commit;
use crate::intent::AppIntent;

/// Read both ends of a comparison as commits and send the range from
/// `base` to `target` back as `DiffResolved`.
pub(crate) fn resolve(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    base: DiffEnd,
    target: DiffEnd,
) {
    tokio::spawn(async move {
        // A panic still reports back, or the busy counter would stay raised.
        let result = tokio::task::spawn_blocking(move || {
            let base = resolve_end(runner.as_ref(), base)?;
            let target = resolve_end(runner.as_ref(), target)?;
            Ok(CommitRange {
                oid: target,
                parent: Some(base),
            })
        })
        .await
        .unwrap_or_else(|e| {
            Err(terminal_safe(
                format!("git rev-parse crashed: {e}").as_bytes(),
            ))
        });
        let _ = tx.send(AppIntent::DiffResolved { tag, result });
    });
}

/// A comparison's end as a commit: a commit as it is, a branch where it
/// points now.
fn resolve_end(runner: &dyn GitRunner, end: DiffEnd) -> Result<CommitId, String> {
    match end {
        DiffEnd::Commit(id) => Ok(id),
        DiffEnd::Branch(name) => branch_commit(runner, &name),
    }
}
