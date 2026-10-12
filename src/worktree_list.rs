//! The Worktrees popup's state: the list as last loaded, or why it could
//! not be, the cursor, the filter, and the branch an add was asked for,
//! plus why its keys cannot run. Pure; the model loads it and moves the
//! cursor.

use crate::branches::BranchesTab;
use crate::git::branches::BranchName;
use crate::git::worktrees::Worktree;
use crate::keymap::{FILTER_HIDES_ROW, OP_RUNNING, Reason};
use crate::list_filter;
use crate::model::AppState;

/// Hex digits of a detached commit, enough to tell commits apart.
const SHORT_OID: usize = 7;

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
    /// Narrows the rows shown. Only ever matched, never passed to git.
    pub filter: String,
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
            filter: String::new(),
        }
    }

    /// Take a load's result. The cursor stays where it was, inside the list
    /// and on a row the filter shows.
    pub fn loaded(&mut self, result: Result<Vec<Worktree>, String>) {
        match result {
            Ok(items) => {
                self.cursor = self.cursor.min(items.len().saturating_sub(1));
                self.items = Some(items);
                self.error = None;
                self.reconcile();
            }
            Err(message) => {
                self.items = None;
                self.error = Some(message);
            }
        }
    }

    /// Move the cursor by `delta` among the rows shown, stopping at either
    /// end.
    pub fn move_cursor(&mut self, delta: isize) {
        if let Some(next) = list_filter::step(&self.visible(), self.cursor, delta) {
            self.cursor = next;
        }
    }

    /// The worktree under the cursor, once there is one and the filter
    /// shows it, so no action reaches a row that is not drawn.
    pub fn selected(&self) -> Option<&Worktree> {
        if self.selected_hidden() {
            return None;
        }
        self.items.as_ref()?.get(self.cursor)
    }

    /// The indexes of the rows the filter shows, in list order.
    pub fn visible(&self) -> Vec<usize> {
        let Some(items) = &self.items else {
            return Vec::new();
        };
        if self.filter.is_empty() {
            return (0..items.len()).collect();
        }
        let texts: Vec<String> = items.iter().map(filter_text).collect();
        list_filter::visible(&self.filter, texts.iter().map(String::as_str))
    }

    /// The filter hides the cursor's row, which happens only when it hides
    /// every row.
    pub fn selected_hidden(&self) -> bool {
        !self.filter.is_empty()
            && self
                .items
                .as_ref()
                .and_then(|items| items.get(self.cursor))
                .is_some_and(|w| !list_filter::matches(&self.filter, &filter_text(w)))
    }

    /// Put a cursor the filter hides on the first row shown; leave it be
    /// when no row is.
    pub fn reconcile(&mut self) {
        if self.selected_hidden()
            && let Some(&first) = self.visible().first()
        {
            self.cursor = first;
        }
    }

    /// Whether a listed worktree has `branch` checked out.
    pub fn has_branch(&self, branch: &BranchName) -> bool {
        self.items
            .iter()
            .flatten()
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

/// The text the filter matches a row by: its label and its path, as drawn.
fn filter_text(worktree: &Worktree) -> String {
    format!("{} {}", label(worktree), worktree.path.display())
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
        .is_some_and(WorktreeList::selected_hidden)
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
    let branch = (state.branches.tab == BranchesTab::Local)
        .then(|| state.branches.selected_branch())
        .flatten();
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
        .is_some_and(WorktreeList::selected_hidden)
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
    } else if state.op_seq.is_some() {
        Some(OP_RUNNING.into())
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
        wl.loaded(Ok(parse_list(out.as_bytes(), 0)));
        wl
    }

    fn filtered(filter: &str) -> WorktreeList {
        let mut wl = mixed();
        wl.filter = filter.into();
        wl.reconcile();
        wl
    }

    #[test]
    fn labels_name_the_branch_bare_or_detached_commit() {
        let wl = mixed();
        let items = wl.items.as_ref().unwrap();
        assert_eq!(label(&items[1]), "topic");
        assert_eq!(label(&items[3]), "(detached) abcdef1");
        let bare = parse_list(b"worktree /r/b.git\0bare\0\0", 0);
        assert_eq!(label(&bare[0]), "(bare)");
        let headless = parse_list(b"worktree /r/x\0detached\0\0", 0);
        assert_eq!(label(&headless[0]), "(detached)");
    }

    #[test]
    fn an_empty_filter_shows_every_row_and_none_before_a_load() {
        assert_eq!(WorktreeList::loading(1).visible(), Vec::<usize>::new());
        assert_eq!(mixed().visible(), [0, 1, 2, 3, 4]);
        assert!(!mixed().selected_hidden());
    }

    #[test]
    fn the_filter_matches_paths_branch_labels_and_detached_rows() {
        assert_eq!(filtered("wt-2").visible(), [2]);
        assert_eq!(filtered("topic").visible(), [1]);
        assert_eq!(filtered("hotfix").visible(), [2]);
        assert_eq!(filtered("detached").visible(), [3, 4]);
        assert_eq!(filtered("main").visible(), [0]);
        // Smart case, as every list filter.
        assert_eq!(filtered("WT").visible(), Vec::<usize>::new());
        assert_eq!(filtered("DETACHED").visible(), Vec::<usize>::new());
    }

    #[test]
    fn the_cursor_skips_hidden_rows_and_stops_at_the_ends() {
        let mut wl = filtered("detached");
        assert_eq!(wl.cursor, 3, "moved off the hidden row it was on");
        wl.move_cursor(1);
        assert_eq!(wl.cursor, 4);
        wl.move_cursor(1);
        assert_eq!(wl.cursor, 4);
        wl.move_cursor(-5);
        assert_eq!(wl.cursor, 3);
        assert_eq!(wl.selected().unwrap().path.display(), "/r/d1");
    }

    #[test]
    fn nothing_matching_selects_nothing_and_keeps_the_cursor() {
        let mut wl = mixed();
        wl.move_cursor(2);
        wl.filter = "zzz".into();
        wl.reconcile();
        assert_eq!(wl.cursor, 2);
        assert!(wl.selected_hidden());
        assert_eq!(wl.selected(), None);
        wl.move_cursor(1);
        assert_eq!(wl.cursor, 2);
        wl.filter.clear();
        assert_eq!(wl.selected().unwrap().path.display(), "/r/wt-2");
    }

    #[test]
    fn a_reload_that_hides_the_cursor_row_moves_it_to_the_first_shown() {
        let mut wl = filtered("wt");
        wl.move_cursor(1);
        assert_eq!(wl.cursor, 2);
        // `/r/wt-2` is gone; `/r/wt-1` stays at row 1.
        let out = format!(
            "worktree /r/main\0HEAD {OID}\0branch refs/heads/main\0\0\
             worktree /r/wt-1\0HEAD {OID}\0branch refs/heads/topic\0\0\
             worktree /r/d1\0HEAD {OID}\0detached\0\0"
        );
        wl.loaded(Ok(parse_list(out.as_bytes(), 0)));
        assert_eq!(wl.cursor, 1);
        assert_eq!(wl.selected().unwrap().path.display(), "/r/wt-1");
    }

    #[test]
    fn a_new_list_starts_unfiltered() {
        assert_eq!(WorktreeList::loading(7).filter, "");
    }
}
