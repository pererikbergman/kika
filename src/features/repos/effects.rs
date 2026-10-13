//! The I/O behind the repos effects: reading and recording the recent
//! list, and `git init` in the launch directory. `Runtime::spawn` calls
//! these from its `LoadRecent`, `RecordRecent` and `InitRepo` arms.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use super::recent;
use crate::background;
use crate::effect::Tag;
use crate::features::repos::ReposIntent;
use crate::git::GitRunner;
use crate::git::repo_root::init_repo;
use crate::intent::AppIntent;

/// Load the recent list from `store` and report it.
pub(crate) fn load(store: Option<PathBuf>, tx: UnboundedSender<AppIntent>, seq: u64) {
    report(store, tx, seq, None);
}

/// Put `path` first in the recent list at `store` and report the list.
pub(crate) fn record(
    store: Option<PathBuf>,
    tx: UnboundedSender<AppIntent>,
    seq: u64,
    path: PathBuf,
) {
    report(store, tx, seq, Some(path));
}

/// Load the recent list, recording `repo` first when given, and report
/// it. A list that cannot be written is still shown as it was read.
fn report(store: Option<PathBuf>, tx: UnboundedSender<AppIntent>, seq: u64, repo: Option<PathBuf>) {
    background::run(
        tx,
        move || {
            let Some(store) = store else {
                return Vec::new();
            };
            match repo {
                Some(repo) => {
                    recent::record(&store, &repo).unwrap_or_else(|_| recent::load(&store))
                }
                None => recent::load(&store),
            }
        },
        |_| Vec::new(),
        move |list| AppIntent::Repos(ReposIntent::Loaded { seq, list }),
    );
}

/// Make the launch directory a repository and report its top level.
pub(crate) fn init(runner: Arc<dyn GitRunner>, tx: UnboundedSender<AppIntent>, tag: Tag) {
    background::run(
        tx,
        move || init_repo(runner.as_ref()),
        // A panic still reports back, or the busy counter would stay raised.
        background::crashed("git init"),
        move |result| AppIntent::Repos(ReposIntent::Initialised { tag, result }),
    );
}
