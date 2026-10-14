//! The I/O behind compare's effect: `Runtime::spawn` calls this with its
//! runner and sender.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use crate::background;
use crate::diff_mark::DiffEnd;
use crate::effect::Tag;
use crate::features::commits::CommitsIntent;
use crate::features::commits::compare::CompareIntent;
use crate::intent::AppIntent;
use kika_git::GitRunner;
use kika_git::commit_files::{CommitId, CommitRange};
use kika_git::log::branch_commit;

/// Read both ends of a comparison as commits and send the range from
/// `base` to `target` back as `CompareIntent::Resolved`.
pub(crate) fn resolve(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    base: DiffEnd,
    target: DiffEnd,
) {
    background::run(
        tx,
        move || {
            let base = resolve_end(runner.as_ref(), base)?;
            let target = resolve_end(runner.as_ref(), target)?;
            Ok(CommitRange {
                oid: target,
                parent: Some(base),
            })
        },
        // A panic still reports back, or the busy counter would stay raised.
        background::crashed("git rev-parse"),
        move |result| {
            AppIntent::Commits(CommitsIntent::Compare(CompareIntent::Resolved {
                tag,
                result,
            }))
        },
    );
}

/// A comparison's end as a commit: a commit as it is, a branch where it
/// points now.
fn resolve_end(runner: &dyn GitRunner, end: DiffEnd) -> Result<CommitId, String> {
    match end {
        DiffEnd::Commit(id) => Ok(id),
        DiffEnd::Branch(name) => branch_commit(runner, &name),
    }
}
