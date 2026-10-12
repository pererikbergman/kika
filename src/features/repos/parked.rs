//! The state a repository's view is parked in while another is open.

use crate::git::FileEntry;
use crate::git::status::BranchStatus;
use crate::model::ParentRepo;

/// What a repository showed when another was opened from the recent list,
/// put back when it is opened again. Only the file list, the selection
/// and which pane had focus: cursors and scroll would be stale by then.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParkedRepo {
    /// The submodules it was inside, as `AppState::repo_stack`.
    pub repo_stack: Vec<ParentRepo>,
    pub files: Vec<FileEntry>,
    pub files_loaded: bool,
    pub branch: Option<BranchStatus>,
    pub selected: Option<usize>,
    /// The Diff pane had focus; Files or a blame count as Files.
    pub diff_focused: bool,
}
