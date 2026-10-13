//! The Submodules feature's `update` handlers: loading and moving the
//! popup's list, running init and update on the selected submodule, and
//! entering and leaving a submodule.

use super::keys::{enter_submodule_disabled, leave_submodule_disabled};
use super::list::SubmoduleList;
use crate::effect::{Effect, Tag};
use crate::git::submodule::Submodule;
use crate::model::{AppState, Context, ParentRepo};
use crate::op::GitOp;
use crate::popup::Popup;

/// The Submodules popup and entering and leaving a submodule.
impl AppState {
    /// The Submodules popup, wherever it is on the stack.
    fn submodule_list(&mut self) -> Option<&mut SubmoduleList> {
        self.popups.iter_mut().rev().find_map(|popup| match popup {
            Popup::Submodules(list) => Some(list),
            _ => None,
        })
    }

    pub(crate) fn move_submodule_cursor(&mut self, delta: isize) {
        if let Some(list) = self.submodule_list() {
            list.list.move_by(delta);
        }
    }

    /// Show the submodules, loading them afresh.
    pub(crate) fn open_submodules(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        let seq = self.take_seq();
        self.popups
            .push(Popup::Submodules(SubmoduleList::loading(seq)));
        self.load_submodules(seq)
    }

    /// Load the list again for the open popup, keeping what it shows
    /// until the result lands. Nothing while the popup is closed.
    pub(crate) fn reload_submodules(&mut self) -> Vec<Effect> {
        if self.submodule_list().is_none() {
            return vec![];
        }
        let seq = self.take_seq();
        if let Some(list) = self.submodule_list() {
            list.list.begin(seq);
        }
        self.load_submodules(seq)
    }

    fn load_submodules(&mut self, seq: u64) -> Vec<Effect> {
        vec![Effect::LoadSubmodules {
            tag: self.user_tag(seq),
        }]
    }

    pub(crate) fn submodules_loaded(&mut self, tag: Tag, result: Result<Vec<Submodule>, String>) {
        self.user_done(tag);
        let generation = self.generation;
        if let Some(list) = self.submodule_list() {
            list.list.loaded(tag, generation, result);
        }
    }

    /// Initialize the submodule under the popup's cursor.
    pub(crate) fn init_submodule(&mut self) -> Vec<Effect> {
        let op = self
            .selected_submodule()
            .map(|s| GitOp::SubmoduleInit(s.path.clone()));
        self.start_op(Ok(op))
    }

    /// Update the submodule under the popup's cursor.
    pub(crate) fn update_submodule(&mut self) -> Vec<Effect> {
        let op = self
            .selected_submodule()
            .map(|s| GitOp::SubmoduleUpdate(s.path.clone()));
        self.start_op(Ok(op))
    }

    /// Work in the selected submodule as if Kika had started there. This
    /// repository's list and selection are kept for the way back.
    pub(crate) fn enter_submodule(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        if let Some(reason) = enter_submodule_disabled(self) {
            self.notice = Some(reason);
            return vec![];
        }
        let Some(path) = self.selected_submodule().map(|s| s.path.clone()) else {
            return vec![];
        };
        // A loaded status, which listing submodules needs, means a repository.
        let Some(root) = self.repo.clone() else {
            return vec![];
        };
        self.repo_stack.push(ParentRepo {
            path,
            files: std::mem::take(&mut self.files),
            files_loaded: self.files_loaded,
            branch: self.branch.take(),
            selected: self.selected.take(),
        });
        self.switch_repo();
        self.files_loaded = false;
        let mut effects = vec![self.set_repo(&root)];
        effects.extend(self.reload_repo());
        effects
    }

    /// Back to the repository this one was entered from, showing what it
    /// showed then while a fresh status load catches up.
    pub(crate) fn leave_submodule(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        if let Some(reason) = leave_submodule_disabled(self) {
            self.notice = Some(reason);
            return vec![];
        }
        let Some(root) = self.repo.clone() else {
            return vec![];
        };
        let Some(parent) = self.repo_stack.pop() else {
            return vec![];
        };
        self.switch_repo();
        self.files = parent.files;
        self.files_loaded = parent.files_loaded;
        self.branch = parent.branch;
        let mut effects = vec![self.set_repo(&root)];
        if let Some(index) = parent.selected.filter(|&i| i < self.files.len()) {
            effects.extend(self.select(index));
        }
        effects.extend(self.reload_repo());
        effects
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff_pane::DiffPane;
    use crate::git::path::RepoPath;
    use crate::git::submodule::SubmoduleState;
    use crate::intent::AppIntent;
    use crate::keymap::KeyScope;
    use crate::menu::MenuId;
    use crate::model::Focus;
    use crate::refresh::Selection;
    use crate::test_support::*;
    use crossterm::event::KeyCode;
    use std::path::Path;

    fn submodule_list(state: &AppState) -> &SubmoduleList {
        match state.popups.last() {
            Some(Popup::Submodules(list)) => list,
            other => panic!("expected the Submodules popup, got {other:?}"),
        }
    }

    #[test]
    fn s_opens_the_submodules_popup_and_loads_it_once() {
        let (mut state, _) = with_files(three_files(), 24);
        let busy = state.busy_count();
        let tag = submodules_tag(&press_char(&mut state, 'S'));
        assert!(tag.user);
        assert_eq!(tag.generation, state.generation);
        assert_eq!(state.busy_count(), busy + 1);
        assert_eq!(state.key_focus(), KeyScope::Submodules);
        assert_eq!(*submodule_list(&state), SubmoduleList::loading(tag.seq));

        let subs = vec![submodule("sub", SubmoduleState::UpToDate)];
        state.update(AppIntent::SubmodulesLoaded {
            tag,
            result: Ok(subs.clone()),
        });
        assert_eq!(submodule_list(&state).list.rows, subs);
        assert_eq!(state.busy_count(), busy);
    }

    #[test]
    fn the_actions_menu_opens_the_submodules_popup() {
        let (mut state, _) = with_files(three_files(), 24);
        state.update(AppIntent::OpenMenu(MenuId::Actions));
        for _ in 0..5 {
            state.update(AppIntent::MenuNext);
        }
        submodules_tag(&state.update(AppIntent::MenuSelect));
        assert!(matches!(state.popups[..], [Popup::Submodules(_)]));
    }

    #[test]
    fn a_stale_or_unwanted_submodule_load_is_dropped_but_still_lowers_busy() {
        let (mut state, _) = with_files(three_files(), 24);
        let old = submodules_tag(&press_char(&mut state, 'S'));
        assert_eq!(state.busy_count(), 1);
        let subs = vec![submodule("sub", SubmoduleState::UpToDate)];
        let stale_seq = Tag {
            seq: old.seq + 100,
            ..old
        };
        state.update(AppIntent::SubmodulesLoaded {
            tag: stale_seq,
            result: Ok(subs.clone()),
        });
        let stale_generation = Tag {
            generation: old.generation + 1,
            ..old
        };
        state.update(AppIntent::SubmodulesLoaded {
            tag: stale_generation,
            result: Ok(subs.clone()),
        });
        assert!(!submodule_list(&state).list.loaded);
        assert_eq!(state.busy_count(), 0);

        // Closed before the answer: nothing comes back.
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.popups, []);
        state.update(AppIntent::SubmodulesLoaded {
            tag: old,
            result: Ok(subs),
        });
        assert_eq!(state.popups, []);
        assert!(!state.should_quit);
    }

    #[test]
    fn j_and_k_move_the_submodule_cursor_within_bounds() {
        let mut state = with_submodules(vec![
            submodule("a", SubmoduleState::UpToDate),
            submodule("b", SubmoduleState::OutOfDate),
        ]);
        press_char(&mut state, 'k');
        assert_eq!(submodule_list(&state).list.selected, 0);
        for _ in 0..3 {
            press_char(&mut state, 'j');
        }
        assert_eq!(submodule_list(&state).list.selected, 1);
        press(&mut state, KeyCode::Up);
        assert_eq!(submodule_list(&state).list.selected, 0);
        press(&mut state, KeyCode::Down);
        assert_eq!(submodule_list(&state).list.selected, 1);
        // The file selection did not move.
        assert_eq!(state.selected, Some(1));
    }

    #[test]
    fn i_and_u_run_one_op_on_the_selected_submodule() {
        let mut state = with_submodules(vec![
            submodule("a", SubmoduleState::Uninitialized),
            submodule("libs/b", SubmoduleState::Uninitialized),
        ]);
        press_char(&mut state, 'j');
        let (tag, op) = op_of(&press_char(&mut state, 'i'));
        assert_eq!(op, GitOp::SubmoduleInit("libs/b".into()));

        // One at a time.
        assert_eq!(press_char(&mut state, 'u'), vec![]);
        assert_eq!(state.notice, Some("An operation is running".into()));
        state.update(AppIntent::OpFinished {
            tag,
            op,
            result: Ok(()),
        });

        let (_, op) = op_of(&press_char(&mut state, 'u'));
        assert_eq!(op, GitOp::SubmoduleUpdate("libs/b".into()));
    }

    #[test]
    fn a_finished_submodule_op_reloads_the_files_and_the_open_list() {
        let mut state = with_submodules(vec![submodule("a", SubmoduleState::Uninitialized)]);
        let (tag, op) = op_of(&press_char(&mut state, 'u'));
        let effects = state.update(AppIntent::OpFinished {
            tag,
            op: op.clone(),
            result: Ok(()),
        });
        let seq = match effects.as_slice() {
            [Effect::LoadStatus { .. }, Effect::LoadSubmodules { tag }] => tag.seq,
            other => panic!("expected a status and a submodule load, got {other:?}"),
        };
        let list = submodule_list(&state);
        assert_eq!(list.list.seq, Some(seq));
        // What it showed stays until the reload lands.
        assert!(list.list.loaded);

        // Closed: only the files reload.
        let (tag, op) = op_of(&state.update(AppIntent::UpdateSubmodule));
        state.popups.clear();
        let effects = state.update(AppIntent::OpFinished {
            tag,
            op,
            result: Ok(()),
        });
        assert!(
            matches!(effects.as_slice(), [Effect::LoadStatus { .. }]),
            "{effects:?}"
        );
    }

    /// The submodule path of a `SetRepo` from the top level [`TOP`].
    fn set_repo(effect: &Effect) -> &[RepoPath] {
        match effect {
            Effect::SetRepo { root, nested } => {
                assert_eq!(root, Path::new(TOP));
                nested
            }
            other => panic!("expected SetRepo, got {other:?}"),
        }
    }

    #[test]
    fn enter_on_a_checked_out_submodule_switches_to_it() {
        let mut state = with_submodules(vec![
            submodule("a", SubmoduleState::Uninitialized),
            submodule("sub", SubmoduleState::UpToDate),
        ]);
        let generation = state.generation;
        let files = state.files.clone();
        let branch = state.branch.clone();
        press_char(&mut state, 'j');

        let effects = press(&mut state, KeyCode::Enter);

        assert_eq!(
            state.repo_stack,
            [ParentRepo {
                path: "sub".into(),
                files,
                files_loaded: true,
                branch,
                selected: Some(1),
            }]
        );
        assert_eq!(state.generation, generation + 1);
        assert_eq!(state.files, []);
        assert!(!state.files_loaded);
        assert_eq!(state.branch, None);
        assert_eq!(state.selected, None);
        assert_eq!(state.popups, []);
        assert_eq!(state.focus, Focus::Files);
        assert_eq!(state.diff, DiffPane::default());
        match effects.as_slice() {
            [
                set,
                Effect::LoadStatus { tag },
                Effect::LoadBranches { .. },
                Effect::LoadRemotes { .. },
                Effect::LoadTags { .. },
                Effect::LoadCommits { skip: 0, .. },
                Effect::LoadMarkers { .. },
                Effect::LoadBranchTips { .. },
                Effect::LoadStashes { .. },
            ] => {
                assert_eq!(set_repo(set), [RepoPath::from("sub")]);
                assert_eq!(tag.generation, state.generation);
            }
            other => panic!("expected SetRepo then LoadStatus, got {other:?}"),
        }
    }

    #[test]
    fn enter_on_an_uninitialized_submodule_only_says_why() {
        let mut state = with_submodules(vec![submodule("a", SubmoduleState::Uninitialized)]);
        let before = state.clone();
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.notice, Some("Update the submodule first".into()));
        assert_eq!(
            AppState {
                notice: None,
                ..state
            },
            before
        );
    }

    #[test]
    fn enter_on_a_conflicted_submodule_says_why_and_stays_in_the_parent() {
        let mut state = with_submodules(vec![submodule("a", SubmoduleState::Conflicted)]);
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(
            state.notice,
            Some("Resolve the submodule conflict first".into())
        );
        assert_eq!(state.repo_stack, []);
        assert!(matches!(state.popups[..], [Popup::Submodules(_)]));
    }

    #[test]
    fn enter_waits_for_a_running_op() {
        let mut state = with_submodules(vec![submodule("a", SubmoduleState::UpToDate)]);
        state.hold_op(99);
        assert_eq!(state.update(AppIntent::EnterSubmodule), vec![]);
        assert_eq!(
            state.notice,
            Some("Wait for the operation to finish".into())
        );
        assert_eq!(state.repo_stack, []);
    }

    #[test]
    fn backspace_restores_the_parent_exactly_and_reloads_it() {
        let mut state = with_submodules(vec![submodule("sub", SubmoduleState::OutOfDate)]);
        let files = state.files.clone();
        let branch = state.branch.clone();
        press(&mut state, KeyCode::Enter);
        let tag = refresh_tag(&mut state);
        state.update(loaded(tag, &["inner.txt"]));
        let generation = state.generation;

        let effects = press(&mut state, KeyCode::Backspace);

        assert_eq!(state.repo_stack, []);
        assert_eq!(state.generation, generation + 1);
        assert_eq!(state.files, files);
        assert_eq!(state.selected, Some(1));
        assert_eq!(state.branch, branch);
        assert!(state.files_loaded);
        match effects.as_slice() {
            [
                set,
                Effect::LoadDiff { request, tag, .. },
                Effect::DiffLoadingDelay { .. },
                Effect::LoadStatus { tag: status },
                Effect::LoadBranches { .. },
                Effect::LoadRemotes { .. },
                Effect::LoadTags { .. },
                Effect::LoadCommits { skip: 0, .. },
                Effect::LoadMarkers { .. },
                Effect::LoadBranchTips { .. },
                Effect::LoadStashes { .. },
            ] => {
                assert_eq!(set_repo(set), []);
                assert_eq!(request.path, "b.rs".into());
                assert_eq!(tag.generation, state.generation);
                assert_eq!(status.generation, state.generation);
            }
            other => panic!("expected SetRepo, the diff, then status, got {other:?}"),
        }
        assert_eq!(state.status_selection, Selection::KeepPath);
    }

    #[test]
    fn nested_submodules_stack_their_paths() {
        let mut state = with_submodules(vec![submodule("outer", SubmoduleState::UpToDate)]);
        press(&mut state, KeyCode::Enter);
        let tag = submodules_tag(&state.update(AppIntent::OpenSubmodules));
        state.update(AppIntent::SubmodulesLoaded {
            tag,
            result: Ok(vec![submodule("inner", SubmoduleState::UpToDate)]),
        });
        let effects = press(&mut state, KeyCode::Enter);
        assert_eq!(
            set_repo(&effects[0]),
            [RepoPath::from("outer"), RepoPath::from("inner")]
        );
        let effects = press(&mut state, KeyCode::Backspace);
        assert_eq!(set_repo(&effects[0]), [RepoPath::from("outer")]);
    }

    #[test]
    fn backspace_outside_a_submodule_is_disabled() {
        let (mut state, _) = with_files(three_files(), 24);
        let before = state.clone();
        assert_eq!(press(&mut state, KeyCode::Backspace), vec![]);
        assert_eq!(state.notice, Some("Not in a submodule".into()));
        assert_eq!(
            AppState {
                notice: None,
                ..state
            },
            before
        );
    }

    #[test]
    fn results_for_the_parent_are_dropped_inside_the_submodule() {
        let mut state = with_submodules(vec![submodule("sub", SubmoduleState::UpToDate)]);
        let parent = refresh_tag(&mut state);
        press(&mut state, KeyCode::Enter);
        state.update(loaded(parent, &["parent.txt"]));
        assert_eq!(state.files, []);
        assert!(!state.files_loaded);
    }
}
