//! The Worktrees popup's state: the list (a `FilteredList`, which owns its
//! rows, error, cursor, filter and load in flight) and the branch an add
//! was asked for, plus why its keys cannot run. Pure; the model loads it
//! and moves the cursor.

use super::git::Worktree;
use crate::filtered_list::{FilteredList, Row};
use crate::keymap::{FILTER_HIDES_ROW, OP_RUNNING, Reason};
use crate::model::AppState;
use kika_git::branches::BranchName;
use kika_git::path::RepoPath;

/// Hex digits of a detached commit, enough to tell commits apart.
const SHORT_OID: usize = 7;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeList {
    /// The worktrees, with the cursor, filter and load in flight. A
    /// reload keeps the cursor at its place, clamped.
    pub list: FilteredList<Worktree>,
    /// The branch `a` was pressed on, kept while its path is typed so a
    /// reload that moves the Branches selection cannot change it.
    pub pending_branch: Option<BranchName>,
    /// The add or remove whose result is awaited; older ones are dropped.
    pub action_seq: Option<u64>,
}

impl Row for Worktree {
    type Key = RepoPath;
    const FOLLOW_ROW: bool = false;

    fn key(&self) -> RepoPath {
        self.path.clone()
    }

    /// Its label and its path, as drawn.
    fn filter_text(&self) -> String {
        format!("{} {}", label(self), self.path.display())
    }
}

impl WorktreeList {
    /// Waiting for load `seq`, with nothing to show yet.
    pub fn loading(seq: u64) -> Self {
        let mut list = FilteredList::default();
        list.begin(seq);
        Self {
            list,
            pending_branch: None,
            action_seq: None,
        }
    }

    /// The worktree under the cursor, once there is one and the filter
    /// shows it, so no action reaches a row that is not drawn.
    pub fn selected(&self) -> Option<&Worktree> {
        if self.list.selected_hidden() {
            return None;
        }
        self.list.selected_row()
    }

    /// Whether a listed worktree has `branch` checked out.
    pub fn has_branch(&self, branch: &BranchName) -> bool {
        self.list
            .rows
            .iter()
            .any(|w| w.branch.as_ref() == Some(branch))
    }
}

/// What a row says for its worktree: `(bare)`, the branch, or
/// `(detached)` and the commit's short id. Terminal-safe. The view draws
/// it and the filter matches it, so the two cannot drift apart.
pub(crate) fn label(worktree: &Worktree) -> String {
    if worktree.bare {
        "(bare)".to_string()
    } else if let Some(branch) = &worktree.branch {
        branch.display()
    } else {
        let oid = worktree.head.as_ref().map_or("", |id| id.as_str());
        let short: String = oid.chars().take(SHORT_OID).collect();
        format!("(detached) {short}").trim_end().to_string()
    }
}

/// Why the Worktrees popup cannot open: it lists the top-level
/// repository's worktrees, so not from inside a submodule.
pub(crate) fn worktrees_disabled(state: &AppState) -> Option<Reason> {
    if !state.repo_stack.is_empty() {
        Some("Leave the submodule first".into())
    } else if state.repo.is_none() {
        Some("Not in a repository".into())
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
pub(crate) fn open_worktree_disabled(state: &AppState) -> Option<Reason> {
    if state
        .worktree_list()
        .is_some_and(|list| list.list.selected_hidden())
    {
        return Some(FILTER_HIDES_ROW.into());
    }
    let Some(worktree) = state.worktree_list().and_then(WorktreeList::selected) else {
        return Some("No worktree selected".into());
    };
    if is_current(state, worktree) {
        Some("Already here".into())
    } else if worktree.prunable {
        Some("Worktree is missing".into())
    } else if worktree.bare {
        Some("Bare repository".into())
    } else if worktree.unsure {
        Some(UNSURE.into())
    } else {
        None
    }
}

/// Why no worktree can be added for the Branches panel's selection.
pub(crate) fn add_worktree_disabled(state: &AppState) -> Option<Reason> {
    let branch = state.local_branch_selected();
    let Some(branch) = branch else {
        return Some("Select a local branch in the Branches panel".into());
    };
    state
        .worktree_list()
        .is_some_and(|list| list.has_branch(&branch.name))
        .then_some("Branch is already checked out in a worktree".into())
}

/// Why the selected worktree cannot be removed.
pub(crate) fn remove_worktree_disabled(state: &AppState) -> Option<Reason> {
    if state
        .worktree_list()
        .is_some_and(|list| list.list.selected_hidden())
    {
        return Some(FILTER_HIDES_ROW.into());
    }
    let Some(worktree) = state.worktree_list().and_then(WorktreeList::selected) else {
        return Some("No worktree selected".into());
    };
    if worktree.main {
        Some("The main worktree cannot be removed".into())
    } else if is_current(state, worktree) {
        Some("Switch to another worktree first".into())
    } else if worktree.locked {
        Some("Worktree is locked".into())
    } else if worktree.unsure && worktree.prunable {
        // A missing directory cannot be verified; being gone is the reason.
        Some("Worktree is missing".into())
    } else if worktree.unsure {
        Some(UNSURE.into())
    } else if state.op_is_running() {
        Some(OP_RUNNING.into())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::worktrees::git::parse_list;
    use crate::test_support::load_list;

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
        wl.list.move_by(1);
        assert_eq!((wl.list.selected, wl.selected()), (0, None));
        load_list(&mut wl.list, Ok(list(&["a", "b"])));
        wl.list.move_by(5);
        assert_eq!(wl.selected().unwrap().path.display(), "b");
        wl.list.move_by(-5);
        assert_eq!(wl.list.selected, 0);
    }

    #[test]
    fn a_shorter_reload_pulls_the_cursor_in_and_an_error_replaces_the_list() {
        let mut wl = WorktreeList::loading(1);
        load_list(&mut wl.list, Ok(list(&["a", "b"])));
        wl.list.move_by(1);
        load_list(&mut wl.list, Ok(list(&["a"])));
        assert_eq!(wl.list.selected, 0);
        load_list(&mut wl.list, Err("boom".into()));
        assert_eq!(
            (wl.list.rows.clone(), wl.list.error.as_deref()),
            (vec![], Some("boom"))
        );
    }

    #[test]
    fn has_branch_looks_through_every_listed_worktree() {
        let mut wl = WorktreeList::loading(1);
        assert!(!wl.has_branch(&"b".into()));
        load_list(&mut wl.list, Ok(list(&["a", "b"])));
        assert!(wl.has_branch(&"b".into()));
        assert!(!wl.has_branch(&"c".into()));
    }

    const OID: &str = "abcdef1234567890abcdef1234567890abcdef12";

    /// `main` at `/r/main`, `topic` at `/r/wt-1`, `hotfix` at `/r/wt-2`, then two
    /// detached ones at `/r/d1` and `/r/d2`.
    fn mixed() -> WorktreeList {
        let out = format!(
            "worktree /r/main\0HEAD {OID}\0branch refs/heads/main\0\0\
             worktree /r/wt-1\0HEAD {OID}\0branch refs/heads/topic\0\0\
             worktree /r/wt-2\0HEAD {OID}\0branch refs/heads/hotfix\0\0\
             worktree /r/d1\0HEAD {OID}\0detached\0\0\
             worktree /r/d2\0HEAD {OID}\0detached\0\0"
        );
        let mut wl = WorktreeList::loading(1);
        load_list(&mut wl.list, Ok(parse_list(out.as_bytes(), 0)));
        wl
    }

    fn filtered(filter: &str) -> WorktreeList {
        let mut wl = mixed();
        wl.list.set_filter(filter.into());
        wl
    }

    #[test]
    fn labels_name_the_branch_bare_or_detached_commit() {
        let wl = mixed();
        let items = &wl.list.rows;
        assert_eq!(label(&items[1]), "topic");
        assert_eq!(label(&items[3]), "(detached) abcdef1");
        let bare = parse_list(b"worktree /r/b.git\0bare\0\0", 0);
        assert_eq!(label(&bare[0]), "(bare)");
        let headless = parse_list(b"worktree /r/x\0detached\0\0", 0);
        assert_eq!(label(&headless[0]), "(detached)");
    }

    #[test]
    fn an_empty_filter_shows_every_row_and_none_before_a_load() {
        assert_eq!(WorktreeList::loading(1).list.visible(), Vec::<usize>::new());
        assert_eq!(mixed().list.visible(), [0, 1, 2, 3, 4]);
        assert!(!mixed().list.selected_hidden());
    }

    #[test]
    fn the_filter_matches_paths_branch_labels_and_detached_rows() {
        assert_eq!(filtered("wt-2").list.visible(), [2]);
        assert_eq!(filtered("topic").list.visible(), [1]);
        assert_eq!(filtered("hotfix").list.visible(), [2]);
        assert_eq!(filtered("detached").list.visible(), [3, 4]);
        assert_eq!(filtered("main").list.visible(), [0]);
        // Smart case, as every list filter.
        assert_eq!(filtered("WT").list.visible(), Vec::<usize>::new());
        assert_eq!(filtered("DETACHED").list.visible(), Vec::<usize>::new());
    }

    #[test]
    fn the_cursor_skips_hidden_rows_and_stops_at_the_ends() {
        let mut wl = filtered("detached");
        assert_eq!(wl.list.selected, 3, "moved off the hidden row it was on");
        wl.list.move_by(1);
        assert_eq!(wl.list.selected, 4);
        wl.list.move_by(1);
        assert_eq!(wl.list.selected, 4);
        wl.list.move_by(-5);
        assert_eq!(wl.list.selected, 3);
        assert_eq!(wl.selected().unwrap().path.display(), "/r/d1");
    }

    #[test]
    fn nothing_matching_selects_nothing_and_keeps_the_cursor() {
        let mut wl = mixed();
        wl.list.move_by(2);
        wl.list.set_filter("zzz".into());
        assert_eq!(wl.list.selected, 2);
        assert!(wl.list.selected_hidden());
        assert_eq!(wl.selected(), None);
        wl.list.move_by(1);
        assert_eq!(wl.list.selected, 2);
        wl.list.filter.clear();
        assert_eq!(wl.selected().unwrap().path.display(), "/r/wt-2");
    }

    #[test]
    fn a_reload_that_hides_the_cursor_row_moves_it_to_the_first_shown() {
        let mut wl = filtered("wt");
        wl.list.move_by(1);
        assert_eq!(wl.list.selected, 2);
        // `/r/wt-2` is gone; `/r/wt-1` stays at row 1.
        let out = format!(
            "worktree /r/main\0HEAD {OID}\0branch refs/heads/main\0\0\
             worktree /r/wt-1\0HEAD {OID}\0branch refs/heads/topic\0\0\
             worktree /r/d1\0HEAD {OID}\0detached\0\0"
        );
        load_list(&mut wl.list, Ok(parse_list(out.as_bytes(), 0)));
        assert_eq!(wl.list.selected, 1);
        assert_eq!(wl.selected().unwrap().path.display(), "/r/wt-1");
    }

    #[test]
    fn a_new_list_starts_unfiltered() {
        assert_eq!(WorktreeList::loading(7).list.filter, "");
    }
}
