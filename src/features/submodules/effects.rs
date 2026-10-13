//! The I/O behind the Submodules feature's effects: `Runtime::spawn` calls
//! these with its runner and sender.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use crate::background;
use crate::effect::Tag;
use crate::features::submodules::SubmodulesIntent;
use crate::git::GitRunner;
use crate::git::submodule::read_submodules;
use crate::intent::AppIntent;

/// List the submodules and send them back as `SubmodulesLoaded`.
pub(crate) fn load(runner: Arc<dyn GitRunner>, tx: UnboundedSender<AppIntent>, tag: Tag) {
    background::run(
        tx,
        move || read_submodules(runner.as_ref()),
        // A panic still reports back, or the busy counter would stay raised.
        background::crashed("git submodule"),
        move |result| AppIntent::Submodules(SubmodulesIntent::Loaded { tag, result }),
    );
}
