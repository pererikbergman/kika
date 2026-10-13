//! Worktrees: the popup that lists the repository's worktrees and switches
//! to, adds and removes them. The feature owns the popup's state and guards
//! (`list.rs`), its `update` handlers (`update.rs`), its key bindings
//! (`keys.rs`), its render function (`view.rs`), the I/O behind its effects
//! (`effects.rs`) and its git plumbing: reading, adding and removing
//! worktrees (`git.rs`) and marking a new one untrusted (`trust.rs`).
//!
//! Its `pub(crate)` API:
//! - the types [`Worktree`] and [`WorktreeList`], and the guard
//!   [`worktrees_disabled`];
//! - the queries `worktree_list()` and `worktree_list_mut()` on `AppState`;
//! - the handlers in `update.rs`, including `forget_worktree_branch`, which
//!   `PopPopup` calls when the path prompt closes;
//! - the chunks `keys::{WORKTREES_WORKSPACE, WORKTREES_KEYS,
//!   WORKTREES_REMOVE_CONFIRM}`;
//! - `view::render_worktrees`;
//! - `effects::{load, add, remove}`.
//!
//! Kept cross-feature reads, of branches and repos through core for now:
//! - `local_branch_selected()`;
//! - `self.branches.selected_branch()` in `ask_worktree_path`;
//! - `reload_branches()` after an add or a remove;
//! - `repo_stack` in `worktrees_disabled`;
//! - switching to a worktree through `AppIntent::OpenRepo`.
//!
//! The popup's branches in the shared focus, prompt, filter and escape code
//! (`key_focus` and `prompt_title` in `model.rs`; `open_filter`, the filter
//! apply match and `list_escape` in `list_input.rs`) stay in core.

pub(crate) mod effects;
mod git;
pub(crate) mod keys;
mod list;
mod trust;
mod update;
pub(crate) mod view;

pub(crate) use git::Worktree;
pub(crate) use list::{WorktreeList, worktrees_disabled};

use crate::model::AppState;
use crate::popup::Popup;

impl AppState {
    /// The Worktrees popup, wherever it is on the stack.
    pub fn worktree_list(&self) -> Option<&WorktreeList> {
        self.popups.iter().rev().find_map(|popup| match popup {
            Popup::Worktrees(list) => Some(list),
            _ => None,
        })
    }
}
