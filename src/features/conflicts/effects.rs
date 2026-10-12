//! The I/O behind the conflict view's effects: reading a conflicted file
//! with its marker size, and writing a pick, each off the event loop and
//! reporting back with its tag. The fail-closed path and content checks
//! live in core `worktree_file`; these only call them.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use super::git::conflict_marker_size;
use super::state::MAX_CONFLICT_FILE;
use crate::effect::Tag;
use crate::git::GitRunner;
use crate::git::diff::terminal_safe;
use crate::git::path::RepoPath;
use crate::intent::AppIntent;
use crate::worktree_file;

/// Read the conflicted file at `path` under `workdir`, which `Runtime`
/// reads before the spawn, and report it as `ConflictLoaded`.
pub(crate) fn load(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    workdir: PathBuf,
    tag: Tag,
    path: RepoPath,
) {
    tokio::spawn(async move {
        let read = path.clone();
        // A panic still reports back, or the busy counter would stay raised.
        let result = tokio::task::spawn_blocking(move || {
            let size = conflict_marker_size(runner.as_ref(), &read)?;
            let bytes = worktree_file::read(&workdir, &read, MAX_CONFLICT_FILE)?;
            Ok((bytes, size))
        })
        .await
        .unwrap_or_else(|e| {
            Err(terminal_safe(
                format!("Reading the file crashed: {e}").as_bytes(),
            ))
        });
        let _ = tx.send(AppIntent::ConflictLoaded { tag, path, result });
    });
}

/// Replace the file at `path` under `workdir` with `content`, only while
/// it still holds `expected`, and report it as `ResolvedWritten`.
pub(crate) fn write(
    tx: UnboundedSender<AppIntent>,
    workdir: PathBuf,
    tag: Tag,
    path: RepoPath,
    expected: Vec<u8>,
    content: Vec<u8>,
) {
    tokio::spawn(async move {
        let target = path.clone();
        // A panic still reports back, or `op_seq` would block every later op.
        let result = tokio::task::spawn_blocking(move || {
            worktree_file::replace(&workdir, &target, &expected, &content).map(|()| content)
        })
        .await
        .unwrap_or_else(|e| {
            Err(terminal_safe(
                format!("Writing the file crashed: {e}").as_bytes(),
            ))
        });
        let _ = tx.send(AppIntent::ResolvedWritten { tag, path, result });
    });
}
