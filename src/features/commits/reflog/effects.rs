//! The I/O behind the reflog's effect: reading `HEAD`'s reflog off the
//! event loop and reporting back with the load's tag.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use super::git::read_reflog;
use crate::background;
use crate::effect::Tag;
use crate::features::commits::CommitsIntent;
use crate::features::commits::reflog::ReflogIntent;
use crate::git::GitRunner;
use crate::intent::AppIntent;

/// Read the reflog and report it as `ReflogIntent::Loaded`.
pub(crate) fn load(runner: Arc<dyn GitRunner>, tx: UnboundedSender<AppIntent>, tag: Tag) {
    background::run(
        tx,
        move || read_reflog(runner.as_ref()),
        // A panic still reports back, or an undo would wait forever.
        background::crashed("git reflog"),
        move |result| {
            AppIntent::Commits(CommitsIntent::Reflog(ReflogIntent::Loaded { tag, result }))
        },
    );
}
