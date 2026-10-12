//! The Worktrees popup's state: the list as last loaded, or why it could
//! not be, the cursor, and the branch an add was asked for, plus why its
//! keys cannot run. Pure; the model loads it and moves the cursor.

use crate::branches::BranchesTab;
use crate::git::branches::BranchName;
use crate::git::worktrees::Worktree;
use crate::keymap::OP_RUNNING;
use crate::model::AppState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeList {
    /// The load whose result is awaited; older ones are dropped.
    pub seq: u64,
    /// `None` until the first load lands.
    pub items: Option<Vec<Worktree>>,
    pub error: Option<String>,
    pub cursor: usize,
    /// The branch `a` was pressed on, kept while its path is typed so a
    /// reload that moves the Branches selection cannot change it.
    pub pending_branch: Option<BranchName>,
    /// The add or remove whose result is awaited; older ones are dropped.
    pub action_seq: Option<u64>,
}

impl WorktreeList {
    /// Waiting for load `seq`, with nothing to show yet.
    pub fn loading(seq: u64) -> Self {
        Self {
            seq,
            items: None,
            error: None,
            cursor: 0,
            pending_branch: None,
            action_seq: None,
        }
    }

    /// Take a load's result. The cursor stays where it was, inside the list.
    pub fn loaded(&mut self, result: Result<Vec<Worktree>, String>) {
        match result {
            Ok(items) => {
                self.cursor = self.cursor.min(items.len().saturating_sub(1));
                self.items = Some(items);
                self.error = None;
            }
            Err(message) => {
                self.items = None;
                self.error = Some(message);
            }
        }
    }

    /// Move the cursor by `delta`, stopping at either end.
    pub fn move_cursor(&mut self, delta: isize) {
        let last = self.items.as_ref().map_or(0, |i| i.len().saturating_sub(1));
        self.cursor = self.cursor.saturating_add_signed(delta).min(last);
    }

    /// The worktree under the cursor, once there is one.
    pub fn selected(&self) -> Option<&Worktree> {
        self.items.as_ref()?.get(self.cursor)
    }

    /// Whether a listed worktree has `branch` checked out.
    pub fn has_branch(&self, branch: &BranchName) -> bool {
        self.items
            .iter()
            .flatten()
            .any(|w| w.branch.as_ref() == Some(branch))
    }
}

/// Why the Worktrees popup cannot open: it lists the top-level
/// repository's worktrees, so not from inside a submodule.
pub(crate) fn worktrees_disabled(state: &AppState) -> Option<&'static str> {
    if !state.repo_stack.is_empty() {
        Some("Leave the submodule first")
    } else if state.repo.is_none() {
        Some("Not in a repository")
    } else {
        None
    }
}

/// Whether `worktree` is the one Kika works in now.
pub(crate) fn is_current(state: &AppState, worktree: &Worktree) -> bool {
    state.repo.as_deref() == Some(worktree.path_buf().as_path())
}

const UNSURE: &str = "Path cannot be read safely with this Git";

/// Why Kika cannot switch to the selected worktree.
pub(crate) fn open_worktree_disabled(state: &AppState) -> Option<&'static str> {
    let Some(worktree) = state.worktree_list().and_then(WorktreeList::selected) else {
        return Some("No worktree selected");
    };
    if is_current(state, worktree) {
        Some("Already here")
    } else if worktree.prunable {
        Some("Worktree is missing")
    } else if worktree.bare {
        Some("Bare repository")
    } else if worktree.unsure {
        Some(UNSURE)
    } else {
        None
    }
}

/// Why no worktree can be added for the Branches panel's selection.
pub(crate) fn add_worktree_disabled(state: &AppState) -> Option<&'static str> {
    let branch = (state.branches.tab == BranchesTab::Local)
        .then(|| state.branches.selected_branch())
        .flatten();
    let Some(branch) = branch else {
        return Some("Select a local branch in the Branches panel");
    };
    state
        .worktree_list()
        .is_some_and(|list| list.has_branch(&branch.name))
        .then_some("Branch is already checked out in a worktree")
}

/// Why the selected worktree cannot be removed.
pub(crate) fn remove_worktree_disabled(state: &AppState) -> Option<&'static str> {
    let Some(worktree) = state.worktree_list().and_then(WorktreeList::selected) else {
        return Some("No worktree selected");
    };
    if worktree.main {
        Some("The main worktree cannot be removed")
    } else if is_current(state, worktree) {
        Some("Switch to another worktree first")
    } else if worktree.locked {
        Some("Worktree is locked")
    } else if worktree.unsure {
        Some(UNSURE)
    } else if state.op_seq.is_some() {
        Some(OP_RUNNING)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::worktrees::parse_list;

    fn list(paths: &[&str]) -> Vec<Worktree> {
        let out: String = paths
            .iter()
            .map(|p| format!("worktree {p}\0branch refs/heads/{p}\0\0"))
            .collect();
        parse_list(out.as_bytes(), 0)
    }

    #[test]
    fn the_cursor_stays_inside_the_list() {
        let mut wl = WorktreeList::loading(1);
        wl.move_cursor(1);
        assert_eq!((wl.cursor, wl.selected()), (0, None));
        wl.loaded(Ok(list(&["a", "b"])));
        wl.move_cursor(5);
        assert_eq!(wl.selected().unwrap().path.display(), "b");
        wl.move_cursor(-5);
        assert_eq!(wl.cursor, 0);
    }

    #[test]
    fn a_shorter_reload_pulls_the_cursor_in_and_an_error_replaces_the_list() {
        let mut wl = WorktreeList::loading(1);
        wl.loaded(Ok(list(&["a", "b"])));
        wl.move_cursor(1);
        wl.loaded(Ok(list(&["a"])));
        assert_eq!(wl.cursor, 0);
        wl.loaded(Err("boom".into()));
        assert_eq!(
            (wl.items.clone(), wl.error.as_deref()),
            (None, Some("boom"))
        );
    }

    #[test]
    fn has_branch_looks_through_every_listed_worktree() {
        let mut wl = WorktreeList::loading(1);
        assert!(!wl.has_branch(&"b".into()));
        wl.loaded(Ok(list(&["a", "b"])));
        assert!(wl.has_branch(&"b".into()));
        assert!(!wl.has_branch(&"c".into()));
    }
}
