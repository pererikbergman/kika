//! The conflict view's intents. Core wraps them in one variant,
//! `AppIntent::Conflicts`, and hands them to this feature's `update`
//! (see `docs/adr/0002-nested-intents.md`).

use super::Choice;
use crate::effect::Tag;
use crate::git::path::RepoPath;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConflictsIntent {
    /// The conflicted file the view opened on was read: its bytes and its
    /// marker size, or why not. `tag` says which load.
    Loaded {
        tag: Tag,
        path: RepoPath,
        result: Result<(Vec<u8>, usize), String>,
    },
    /// Move the conflict view's cursor to the next or previous block.
    Next,
    Prev,
    /// Keep this side of the selected block, and write the file.
    Pick(Choice),
    /// A resolved file was written: what is now on disk, or why not.
    ResolvedWritten {
        tag: Tag,
        path: RepoPath,
        result: Result<Vec<u8>, String>,
    },
    /// Stage the file the conflict view resolved, as the question asked.
    StageResolved,
    /// Resolve the selected conflicted file in the user's merge tool.
    OpenMergeTool,
}
