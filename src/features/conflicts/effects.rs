//! The I/O behind the conflict view's effects: reading a conflicted file
//! with its marker size, and writing a pick, each off the event loop and
//! reporting back with its tag. The fail-closed path and content checks
//! live in core `worktree_file`; these only call them.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use super::git::conflict_marker_size;
use super::state::MAX_CONFLICT_FILE;
use crate::background;
use crate::effect::Tag;
use crate::features::conflicts::ConflictsIntent;
use crate::intent::AppIntent;
use crate::worktree_file;
use kika_git::GitRunner;
use kika_git::path::RepoPath;

/// Read the conflicted file at `path` under `workdir`, which `Runtime`
/// reads before the spawn, and report it as `ConflictsIntent::Loaded`.
pub(crate) fn load(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    workdir: PathBuf,
    tag: Tag,
    path: RepoPath,
) {
    let read = path.clone();
    background::run(
        tx,
        move || {
            let size = conflict_marker_size(runner.as_ref(), &read)?;
            let bytes = worktree_file::read(&workdir, &read, MAX_CONFLICT_FILE)?;
            Ok((bytes, size))
        },
        // A panic still reports back, or the busy counter would stay raised.
        background::crashed("Reading the file"),
        move |result| AppIntent::Conflicts(ConflictsIntent::Loaded { tag, path, result }),
    );
}

/// Replace the file at `path` under `workdir` with `content`, only while
/// it still holds `expected`, and report it as `ConflictsIntent::ResolvedWritten`.
pub(crate) fn write(
    tx: UnboundedSender<AppIntent>,
    workdir: PathBuf,
    tag: Tag,
    path: RepoPath,
    expected: Vec<u8>,
    content: Vec<u8>,
) {
    let target = path.clone();
    background::run(
        tx,
        move || worktree_file::replace(&workdir, &target, &expected, &content).map(|()| content),
        // A panic still reports back, or the op slot would block every later op.
        background::crashed("Writing the file"),
        move |result| AppIntent::Conflicts(ConflictsIntent::ResolvedWritten { tag, path, result }),
    );
}
