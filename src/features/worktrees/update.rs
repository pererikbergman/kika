//! The Worktrees popup's `update` handlers: opening and reloading the list,
//! switching to a worktree, and adding and removing one.

use std::path::{Path, PathBuf};

use super::list;
use super::{Worktree, WorktreeList};
use crate::confirm_question::{Answer, ConfirmQuestion, Then};
use crate::effect::{Effect, Tag};
use crate::git::path::RepoPath;
use crate::intent::AppIntent;
use crate::keymap::{self, KeyScope};
use crate::model::{AppState, Context};
use crate::popup::{ErrorPopup, LineInput, Popup, PromptKind};

/// The Worktrees popup: listing, switching to, adding and removing worktrees.
impl AppState {
    pub(crate) fn worktree_list_mut(&mut self) -> Option<&mut WorktreeList> {
        self.popups.iter_mut().rev().find_map(|popup| match popup {
            Popup::Worktrees(list) => Some(list),
            _ => None,
        })
    }

    pub(crate) fn move_worktree_cursor(&mut self, delta: isize) {
        if let Some(list) = self.worktree_list_mut() {
            list.list.move_by(delta);
        }
    }

    /// Show the worktrees, loading them afresh.
    pub(crate) fn open_worktrees(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        if let Some(reason) = list::worktrees_disabled(self) {
            self.notice = Some(reason);
            return vec![];
        }
        let seq = self.take_seq();
        self.popups
            .push(Popup::Worktrees(WorktreeList::loading(seq)));
        vec![Effect::LoadWorktrees {
            tag: self.user_tag(seq),
        }]
    }

    /// Load the list again for the open popup; nothing while it is closed.
    fn reload_worktrees(&mut self) -> Vec<Effect> {
        if self.worktree_list().is_none() {
            return vec![];
        }
        let seq = self.take_seq();
        if let Some(list) = self.worktree_list_mut() {
            list.list.begin(seq);
        }
        vec![Effect::LoadWorktrees {
            tag: self.user_tag(seq),
        }]
    }

    pub(crate) fn worktrees_loaded(&mut self, tag: Tag, result: Result<Vec<Worktree>, String>) {
        self.user_done(tag);
        let generation = self.generation;
        if let Some(list) = self.worktree_list_mut() {
            list.list.loaded(tag, generation, result);
        }
    }

    /// Switch to the selected worktree as to a repository from the recent
    /// list, so it is parked, recorded and refused the same way.
    pub(crate) fn open_worktree(&mut self) -> Vec<Effect> {
        if let Some(reason) = list::open_worktree_disabled(self) {
            self.notice = Some(reason);
            return vec![];
        }
        let Some(path) = self
            .worktree_list()
            .and_then(WorktreeList::selected)
            .map(Worktree::path_buf)
        else {
            return vec![];
        };
        self.update(AppIntent::OpenRepo(path))
    }

    /// Ask where to add a worktree for the Branches panel's local branch,
    /// which is kept now so a reload cannot change it.
    pub(crate) fn ask_worktree_path(&mut self) {
        if let Some(reason) = list::add_worktree_disabled(self) {
            self.notice = Some(reason);
            return;
        }
        let Some(branch) = self.branches.selected_branch().map(|b| b.name.clone()) else {
            return;
        };
        let dir = self
            .repo
            .as_deref()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str());
        let text = match (dir, std::str::from_utf8(&branch.0)) {
            (Some(dir), Ok(name)) => format!("../{dir}-{}", name.replace('/', "-")),
            _ => String::new(),
        };
        let Some(list) = self.worktree_list_mut() else {
            return;
        };
        list.pending_branch = Some(branch);
        self.popups.push(Popup::Prompt {
            kind: PromptKind::WorktreePath,
            input: LineInput::with_text(&text),
        });
    }

    /// Add the worktree at the typed path. Empty text or a control
    /// character keeps the prompt open with a notice instead.
    pub(crate) fn add_worktree_at(&mut self, text: String) -> Vec<Effect> {
        if text.trim().is_empty() || text.contains(char::is_control) {
            self.notice = Some(if text.trim().is_empty() {
                "Type a path for the worktree".into()
            } else {
                "The path has a control character".into()
            });
            self.popups.push(Popup::Prompt {
                kind: PromptKind::WorktreePath,
                input: LineInput::with_text(&text),
            });
            return vec![];
        }
        let seq = self.take_seq();
        let Some(list) = self.worktree_list_mut() else {
            return vec![];
        };
        let Some(branch) = list.pending_branch.take() else {
            return vec![];
        };
        list.action_seq = Some(seq);
        vec![Effect::AddWorktree {
            tag: self.user_tag(seq),
            path: text,
            branch,
        }]
    }

    /// Whether an add or remove result is for the open popup's last ask,
    /// in this repository. Lowers busy either way.
    fn worktree_action_current(&mut self, tag: Tag) -> bool {
        self.user_done(tag);
        tag.generation == self.generation
            && self
                .worktree_list()
                .is_some_and(|list| list.action_seq == Some(tag.seq))
    }

    pub(crate) fn worktree_added(
        &mut self,
        tag: Tag,
        result: Result<PathBuf, String>,
    ) -> Vec<Effect> {
        if !self.worktree_action_current(tag) {
            return vec![];
        }
        match result {
            Ok(_) => {
                self.notice = Some("Worktree added; Enter switches to it".into());
                let mut effects = self.reload_worktrees();
                effects.extend(self.reload_branches());
                effects
            }
            Err(message) => {
                self.push_error(ErrorPopup::new(
                    "Could not add worktree",
                    &message,
                    "Close this, fix the cause, then try again.",
                ));
                vec![]
            }
        }
    }

    /// Ask before removing the selected linked worktree, naming it.
    pub(crate) fn ask_remove_worktree(&mut self) {
        if let Some(reason) = list::remove_worktree_disabled(self) {
            self.notice = Some(reason);
            return;
        }
        let Some(path) = self
            .worktree_list()
            .and_then(WorktreeList::selected)
            .map(|w| w.path.clone())
        else {
            return;
        };
        self.popups
            .push(Popup::Question(remove_worktree_question(path)));
    }

    /// Remove the worktree the question named, never forced. While an op
    /// runs the question stays open.
    pub(crate) fn confirm_remove_worktree(&mut self) -> Vec<Effect> {
        if !matches!(
            self.popups.last(),
            Some(Popup::Question(q)) if q.scope == KeyScope::RemoveWorktree
        ) {
            return vec![];
        }
        if self.op_is_running() {
            self.notice = Some(keymap::OP_RUNNING.into());
            return vec![];
        }
        let Some(Then::RemoveWorktree(path)) = self
            .take_question(KeyScope::RemoveWorktree, Answer::YOnly)
            .map(|q| q.then)
        else {
            return vec![];
        };
        let seq = self.take_seq();
        let Some(list) = self.worktree_list_mut() else {
            return vec![];
        };
        list.action_seq = Some(seq);
        vec![Effect::RemoveWorktree {
            tag: self.user_tag(seq),
            path,
        }]
    }

    pub(crate) fn worktree_removed(&mut self, tag: Tag, result: Result<(), String>) -> Vec<Effect> {
        if !self.worktree_action_current(tag) {
            return vec![];
        }
        if let Err(message) = result {
            self.push_error(ErrorPopup::new(
                "Could not remove worktree",
                &message,
                "Close this, fix the cause, then try again.",
            ));
            return vec![];
        }
        self.notice = Some("Worktree removed".into());
        let mut effects = self.reload_worktrees();
        effects.extend(self.reload_branches());
        effects
    }

    /// Drop the branch kept for the path prompt, when that prompt closes
    /// without an answer.
    pub(crate) fn forget_worktree_branch(&mut self) {
        if let Some(list) = self.worktree_list_mut() {
            list.pending_branch = None;
        }
    }
}

/// "Remove this worktree?", carrying the path taken when it was asked, so
/// a reload before the answer cannot change what is removed.
pub(crate) fn remove_worktree_question(path: RepoPath) -> ConfirmQuestion {
    ConfirmQuestion {
        scope: KeyScope::RemoveWorktree,
        title: "Remove worktree".into(),
        text: format!(
            "Remove worktree {}? Git refuses if it has uncommitted or untracked \
             changes. (y/n)",
            path.display()
        ),
        answer: Answer::YOnly,
        then: Then::RemoveWorktree(path),
        range: false,
        head: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::worktrees::git::parse_list;
    use crate::git::branches::BranchName;
    use crate::git::path::RepoPath;
    use crate::keymap::KeyScope;
    use crate::list_filter::{self, InputTarget};
    use crate::menu::MenuId;
    use crate::model::ParentRepo;
    use crate::test_support::*;
    use crossterm::event::KeyCode;

    /// `main` at `/r/a` (on `main`), `topic` at `/r/a-topic`, a missing
    /// detached one at `/r/gone` and one whose path git may have split.
    fn four_worktrees() -> Vec<Worktree> {
        let mut list = parse_list(
            b"worktree /r/a\0branch refs/heads/main\0\0\
              worktree /r/a-topic\0branch refs/heads/topic\0\0\
              worktree /r/gone\0detached\0prunable\0\0\
              worktree /r/odd\0detached\0\0",
            0,
        );
        list[3].unsure = true;
        list
    }

    fn worktrees_tag(effects: &[Effect]) -> Tag {
        match effects {
            [Effect::LoadWorktrees { tag }] => *tag,
            other => panic!("expected one LoadWorktrees, got {other:?}"),
        }
    }

    /// The branches workspace in repository `/r/a`, with `W` pressed and
    /// `items` loaded, cursor at `cursor`.
    fn with_worktrees(items: Vec<Worktree>, cursor: usize) -> AppState {
        let mut state = with_branches();
        state.repo = Some("/r/a".into());
        let tag = worktrees_tag(&press_char(&mut state, 'W'));
        state.update(AppIntent::WorktreesLoaded {
            tag,
            result: Ok(items),
        });
        for _ in 0..cursor {
            press_char(&mut state, 'j');
        }
        state
    }

    fn the_worktree_list(state: &AppState) -> &WorktreeList {
        state.worktree_list().expect("the Worktrees popup")
    }

    #[test]
    fn w_opens_the_worktrees_popup_and_loads_it() {
        let mut state = with_branches();
        state.repo = Some("/r/a".into());
        let busy = state.busy_count();
        let tag = worktrees_tag(&press_char(&mut state, 'W'));
        assert_eq!((tag.generation, tag.user), (state.generation, true));
        assert_eq!(state.busy_count(), busy + 1);
        assert_eq!(state.key_focus(), KeyScope::Worktrees);
        assert_eq!(*the_worktree_list(&state), WorktreeList::loading(tag.seq));
        state.update(AppIntent::WorktreesLoaded {
            tag,
            result: Ok(four_worktrees()),
        });
        assert_eq!(state.busy_count(), busy);
        assert_eq!(the_worktree_list(&state).list.rows, four_worktrees());
    }

    #[test]
    fn the_worktrees_popup_is_disabled_inside_a_submodule_or_outside_a_repository() {
        let mut state = with_branches();
        assert_eq!(press_char(&mut state, 'W'), vec![]);
        assert_eq!(state.notice, Some("Not in a repository".into()));
        state.repo = Some("/r/a".into());
        state.repo_stack.push(ParentRepo {
            path: "sub".into(),
            files: vec![],
            files_loaded: true,
            branch: None,
            selected: None,
        });
        assert_eq!(press_char(&mut state, 'W'), vec![]);
        assert_eq!(state.notice, Some("Leave the submodule first".into()));
        assert!(state.popups.is_empty());
    }

    #[test]
    fn the_actions_menu_opens_the_worktrees_popup() {
        let mut state = with_branches();
        state.repo = Some("/r/a".into());
        state.update(AppIntent::OpenMenu(MenuId::Actions));
        for _ in 0..6 {
            state.update(AppIntent::MenuNext);
        }
        worktrees_tag(&state.update(AppIntent::MenuSelect));
        assert!(matches!(state.popups[..], [Popup::Worktrees(_)]));
    }

    #[test]
    fn enter_on_another_worktree_switches_to_it_as_open_repo_does() {
        let mut state = with_worktrees(four_worktrees(), 1);
        let mut expected = state.clone();
        let effects = press(&mut state, KeyCode::Enter);
        assert_eq!(
            effects,
            expected.update(AppIntent::OpenRepo("/r/a-topic".into()))
        );
        assert!(matches!(
            effects.first(),
            Some(Effect::SetRepo { root, .. }) if root == Path::new("/r/a-topic")
        ));
        assert_eq!(state.repo.as_deref(), Some(Path::new("/r/a-topic")));
    }

    #[test]
    fn enter_on_the_current_a_missing_or_an_unsure_worktree_says_why() {
        for (cursor, reason) in [
            (0, "Already here"),
            (2, "Worktree is missing"),
            (3, "Path cannot be read safely with this Git"),
        ] {
            let mut state = with_worktrees(four_worktrees(), cursor);
            assert_eq!(press(&mut state, KeyCode::Enter), vec![], "{reason}");
            assert_eq!(state.notice, Some(reason.into()));
            assert_eq!(state.repo.as_deref(), Some(Path::new("/r/a")));
        }
        let mut bare = parse_list(b"worktree /r/b.git\0bare\0\0", 0);
        bare[0].main = false;
        let mut state = with_worktrees(bare, 0);
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.notice, Some("Bare repository".into()));
    }

    fn worktree_rows(state: &AppState) -> Vec<usize> {
        the_worktree_list(state).list.visible()
    }

    #[test]
    fn slash_in_the_worktrees_popup_filters_it_as_typed() {
        let mut state = with_worktrees(four_worktrees(), 0);
        assert_eq!(press_char(&mut state, '/'), vec![]);
        assert_eq!(state.key_focus(), KeyScope::Input);
        let input = state.input.as_ref().expect("the input is open");
        assert_eq!(input.target, InputTarget::WorktreesFilter);
        assert_eq!(input.target.prompt(), "/");
        type_text(&mut state, "gone");
        assert_eq!(the_worktree_list(&state).list.filter, "gone");
        assert_eq!(worktree_rows(&state), [2]);
        assert_eq!(
            the_worktree_list(&state).list.selected,
            2,
            "moved to a shown row"
        );
        // Smart case: an uppercase char makes the match exact.
        press(&mut state, KeyCode::Backspace);
        press(&mut state, KeyCode::Backspace);
        type_text(&mut state, "NE");
        assert_eq!(worktree_rows(&state), Vec::<usize>::new());
        assert_eq!(state.key_focus(), KeyScope::Input);
    }

    #[test]
    fn the_worktrees_filter_stops_at_its_limit() {
        let mut state = with_worktrees(four_worktrees(), 0);
        press_char(&mut state, '/');
        for _ in 0..300 {
            press_char(&mut state, 'z');
        }
        assert_eq!(
            the_worktree_list(&state).list.filter.chars().count(),
            list_filter::MAX_INPUT
        );
        assert_eq!(state.notice, Some(list_filter::input_full_notice().into()));
    }

    #[test]
    fn enter_keeps_the_worktrees_filter_and_esc_in_the_input_clears_it() {
        let mut state = with_worktrees(four_worktrees(), 0);
        press_char(&mut state, '/');
        type_text(&mut state, "topic");
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.input, None);
        assert_eq!(state.key_focus(), KeyScope::Worktrees);
        assert_eq!(worktree_rows(&state), [1]);
        assert_eq!(state.repo.as_deref(), Some(Path::new("/r/a")), "no switch");

        // `/` again starts from the kept text; Esc there clears it.
        press_char(&mut state, '/');
        assert_eq!(state.input.as_ref().unwrap().line.text, "topic");
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.input, None);
        assert_eq!(the_worktree_list(&state).list.filter, "");
        assert_eq!(state.key_focus(), KeyScope::Worktrees);
    }

    #[test]
    fn esc_in_the_worktrees_popup_clears_the_filter_then_closes_it() {
        let mut state = with_worktrees(four_worktrees(), 0);
        press_char(&mut state, '/');
        type_text(&mut state, "topic");
        press(&mut state, KeyCode::Enter);
        assert_eq!(press(&mut state, KeyCode::Esc), vec![]);
        assert_eq!(the_worktree_list(&state).list.filter, "");
        assert_eq!(state.key_focus(), KeyScope::Worktrees, "still open");
        assert_eq!(press(&mut state, KeyCode::Esc), vec![]);
        assert!(state.popups.is_empty());
        assert!(!state.should_quit);

        // `q` closes at once, filter or not.
        let mut state = with_worktrees(four_worktrees(), 0);
        press_char(&mut state, '/');
        type_text(&mut state, "topic");
        press(&mut state, KeyCode::Enter);
        press_char(&mut state, 'q');
        assert!(state.popups.is_empty());

        // Opened again, the list starts unfiltered.
        worktrees_tag(&press_char(&mut state, 'W'));
        assert_eq!(the_worktree_list(&state).list.filter, "");
    }

    #[test]
    fn a_filter_hiding_every_worktree_disables_switch_and_remove() {
        let mut state = with_worktrees(four_worktrees(), 1);
        press_char(&mut state, '/');
        type_text(&mut state, "--force \x1b[31m");
        press(&mut state, KeyCode::Enter);
        assert_eq!(worktree_rows(&state), Vec::<usize>::new());
        assert_eq!(the_worktree_list(&state).selected(), None);
        for code in [KeyCode::Char('d'), KeyCode::Enter] {
            assert_eq!(press(&mut state, code), vec![], "{code:?}");
            assert_eq!(state.notice, Some(keymap::FILTER_HIDES_ROW.into()));
            assert_eq!(state.key_focus(), KeyScope::Worktrees);
        }
        assert_eq!(state.repo.as_deref(), Some(Path::new("/r/a")));
    }

    /// The worktrees of [`four_worktrees`] plus one at a path only a
    /// hostile-looking filter picks out, the filter set and accepted.
    fn filtered_to_hostile() -> (AppState, &'static str) {
        let mut items = four_worktrees();
        items.extend(parse_list(
            b"worktree /r/a-b-fo-rce-wt\0branch refs/heads/side\0\0",
            0,
        ));
        items[4].main = false;
        let mut state = with_worktrees(items, 0);
        let filter = "--force wt";
        press_char(&mut state, '/');
        type_text(&mut state, filter);
        press(&mut state, KeyCode::Enter);
        assert_eq!(worktree_rows(&state), [4]);
        (state, filter)
    }

    /// No effect, and so no argv built from one, holds the filter's text.
    fn assert_no_filter_text(effects: &[Effect], filter: &str) {
        let shown = format!("{effects:?}");
        for part in [filter, "--force", "force"] {
            assert!(!shown.contains(part), "{part:?} in {shown}");
        }
    }

    #[test]
    fn the_worktrees_filter_never_reaches_git() {
        let (mut state, filter) = filtered_to_hostile();
        let loaded = RepoPath::from("/r/a-b-fo-rce-wt");
        assert_eq!(the_worktree_list(&state).selected().unwrap().path, loaded);

        let mut expected = state.clone();
        let effects = press(&mut state, KeyCode::Enter);
        assert_eq!(
            effects,
            expected.update(AppIntent::OpenRepo("/r/a-b-fo-rce-wt".into()))
        );
        assert_no_filter_text(&effects, filter);

        let (mut state, filter) = filtered_to_hostile();
        assert_eq!(press_char(&mut state, 'd'), vec![]);
        assert_eq!(state.key_focus(), KeyScope::RemoveWorktree);
        let effects = press_char(&mut state, 'y');
        match &effects[..] {
            [Effect::RemoveWorktree { path, .. }] => assert_eq!(*path, loaded),
            other => panic!("expected one RemoveWorktree, got {other:?}"),
        }
        assert_no_filter_text(&effects, filter);
    }

    /// `a` pressed with the Branches panel on `wip`, which no worktree has.
    fn asking_for_wip() -> AppState {
        let mut state = with_branches();
        state.update(AppIntent::BranchNext);
        state.update(AppIntent::BranchNext);
        state.repo = Some("/r/a".into());
        let tag = worktrees_tag(&press_char(&mut state, 'W'));
        state.update(AppIntent::WorktreesLoaded {
            tag,
            result: Ok(four_worktrees()),
        });
        assert_eq!(press_char(&mut state, 'a'), vec![]);
        state
    }

    #[test]
    fn a_opens_a_prefilled_prompt_for_the_selected_local_branch() {
        let state = asking_for_wip();
        match state.popups.last() {
            Some(Popup::Prompt { kind, input }) => {
                assert_eq!(*kind, PromptKind::WorktreePath);
                assert_eq!(input.text, "../a-wip");
            }
            other => panic!("expected the path prompt, got {other:?}"),
        }
        assert_eq!(
            state.prompt_title(PromptKind::WorktreePath),
            "New worktree for wip"
        );
        assert_eq!(
            the_worktree_list(&state).pending_branch,
            Some(BranchName::from("wip"))
        );
    }

    #[test]
    fn a_slash_in_the_branch_becomes_a_dash_and_non_utf8_leaves_it_empty() {
        let mut state = with_branches();
        state.repo = Some("/r/a".into());
        let tag = branches_tag(&state.update(AppIntent::RefreshStatus));
        state.update(branches_loaded(
            tag,
            vec![
                git_branch("main", true, true),
                git_branch("feat/x", false, true),
            ],
        ));
        state.update(AppIntent::BranchNext);
        let tag = worktrees_tag(&press_char(&mut state, 'W'));
        state.update(AppIntent::WorktreesLoaded {
            tag,
            result: Ok(vec![]),
        });
        press_char(&mut state, 'a');
        assert!(matches!(
            state.popups.last(),
            Some(Popup::Prompt { input, .. }) if input.text == "../a-feat-x"
        ));
        state.update(AppIntent::PopPopup);
        assert_eq!(the_worktree_list(&state).pending_branch, None);

        state.branches.list.rows[1].name = BranchName::from_bytes(b"x\xff");
        press_char(&mut state, 'a');
        assert!(matches!(
            state.popups.last(),
            Some(Popup::Prompt { input, .. }) if input.text.is_empty()
        ));
    }

    #[test]
    fn submitting_adds_for_the_branch_taken_when_a_was_pressed() {
        let mut state = asking_for_wip();
        // A reload moves the selection while the path is typed.
        let tag = branches_tag(&state.update(AppIntent::RefreshStatus));
        state.update(branches_loaded(tag, three_branches()));
        state.branches.list.selected = 0;
        let busy = state.busy_count();
        let effects = state.update(AppIntent::SubmitPrompt);
        match &effects[..] {
            [Effect::AddWorktree { tag, path, branch }] => {
                assert_eq!(path, "../a-wip");
                assert_eq!(*branch, BranchName::from("wip"));
                assert!(tag.user);
                assert_eq!(the_worktree_list(&state).action_seq, Some(tag.seq));
            }
            other => panic!("expected one AddWorktree, got {other:?}"),
        }
        assert_eq!(state.busy_count(), busy + 1);
        assert_eq!(state.key_focus(), KeyScope::Worktrees);
    }

    #[test]
    fn an_empty_path_keeps_the_prompt_open() {
        let mut state = asking_for_wip();
        if let Some(Popup::Prompt { input, .. }) = state.popups.last_mut() {
            *input = LineInput::with_text("  ");
        }
        assert_eq!(state.update(AppIntent::SubmitPrompt), vec![]);
        assert_eq!(state.notice, Some("Type a path for the worktree".into()));
        assert_eq!(state.key_focus(), KeyScope::Prompt);
        assert_eq!(
            state.update(AppIntent::AddWorktreeAt("a\tb".into())),
            vec![]
        );
        assert_eq!(
            state.notice,
            Some("The path has a control character".into())
        );
        assert!(the_worktree_list(&state).pending_branch.is_some());
    }

    #[test]
    fn a_is_disabled_on_another_tab_or_for_a_branch_checked_out_elsewhere() {
        let mut state = with_worktrees(four_worktrees(), 0);
        state.branches.list.selected = 1;
        assert_eq!(press_char(&mut state, 'a'), vec![]);
        assert_eq!(
            state.notice,
            Some("Branch is already checked out in a worktree".into())
        );
        on_remotes_tab(&mut state);
        state.branches.list.selected = 2;
        press_char(&mut state, 'a');
        assert_eq!(
            state.notice,
            Some("Select a local branch in the Branches panel".into())
        );
        assert_eq!(state.key_focus(), KeyScope::Worktrees);
    }

    /// The add submitted from [`asking_for_wip`], and its tag.
    fn adding() -> (AppState, Tag) {
        let mut state = asking_for_wip();
        let tag = match &state.update(AppIntent::SubmitPrompt)[..] {
            [Effect::AddWorktree { tag, .. }] => *tag,
            other => panic!("expected AddWorktree, got {other:?}"),
        };
        (state, tag)
    }

    #[test]
    fn a_finished_add_says_so_and_reloads_the_list_and_branches() {
        let (mut state, tag) = adding();
        let effects = state.update(AppIntent::WorktreeAdded {
            tag,
            result: Ok("/r/a-wip".into()),
        });
        assert_eq!(
            state.notice,
            Some("Worktree added; Enter switches to it".into())
        );
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::LoadWorktrees { .. }))
        );
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::LoadBranches { .. }))
        );

        let (mut state, tag) = adding();
        state.update(AppIntent::WorktreeAdded {
            tag,
            result: Err("fatal: '/r/a-wip' already exists".into()),
        });
        match state.popups.last() {
            Some(Popup::Error(error)) => assert_eq!(error.title, "Could not add worktree"),
            other => panic!("expected the error, got {other:?}"),
        }
    }

    #[test]
    fn d_on_the_main_or_current_worktree_is_disabled() {
        let mut state = with_worktrees(four_worktrees(), 0);
        assert_eq!(press_char(&mut state, 'd'), vec![]);
        assert_eq!(
            state.notice,
            Some("The main worktree cannot be removed".into())
        );

        let mut state = with_worktrees(four_worktrees(), 1);
        state.repo = Some("/r/a-topic".into());
        press_char(&mut state, 'd');
        assert_eq!(
            state.notice,
            Some("Switch to another worktree first".into())
        );

        let mut locked = four_worktrees();
        locked[1].locked = true;
        let mut state = with_worktrees(locked, 1);
        press_char(&mut state, 'd');
        assert_eq!(state.notice, Some("Worktree is locked".into()));

        let mut state = with_worktrees(four_worktrees(), 3);
        press_char(&mut state, 'd');
        assert_eq!(
            state.notice,
            Some("Path cannot be read safely with this Git".into())
        );

        // Read with newlines, a missing worktree cannot be verified either,
        // but being gone is the reason worth showing.
        let mut gone_unsure = four_worktrees();
        gone_unsure[2].unsure = true;
        let mut state = with_worktrees(gone_unsure, 2);
        press_char(&mut state, 'd');
        assert_eq!(state.notice, Some("Worktree is missing".into()));

        let mut state = with_worktrees(four_worktrees(), 1);
        state.hold_op(99);
        press_char(&mut state, 'd');
        assert_eq!(state.notice, Some(keymap::OP_RUNNING.into()));
        assert_eq!(state.key_focus(), KeyScope::Worktrees);
    }

    #[test]
    fn d_asks_and_only_y_removes_the_path_it_asked_about() {
        let mut state = with_worktrees(four_worktrees(), 1);
        assert_eq!(press_char(&mut state, 'd'), vec![]);
        assert_eq!(state.key_focus(), KeyScope::RemoveWorktree);
        match state.popups.last() {
            Some(Popup::Question(q)) => {
                assert_eq!(q.then, Then::RemoveWorktree(RepoPath::from("/r/a-topic")));
                assert!(q.text.starts_with("Remove worktree /r/a-topic?"));
            }
            other => panic!("expected the question, got {other:?}"),
        }
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.key_focus(), KeyScope::RemoveWorktree);

        // A reload that moves the cursor does not change the answer.
        state.update(AppIntent::WorktreeNext);
        match &press_char(&mut state, 'y')[..] {
            [Effect::RemoveWorktree { tag, path }] => {
                assert_eq!(*path, RepoPath::from("/r/a-topic"));
                assert!(tag.user);
            }
            other => panic!("expected one RemoveWorktree, got {other:?}"),
        }
        assert_eq!(state.key_focus(), KeyScope::Worktrees);
    }

    #[test]
    fn y_while_an_op_runs_keeps_the_question_and_says_why() {
        let mut state = with_worktrees(four_worktrees(), 1);
        press_char(&mut state, 'd');
        let asked = state.popups.clone();
        state.hold_op(99);
        assert_eq!(press_char(&mut state, 'y'), vec![]);
        assert_eq!(state.popups, asked);
        assert_eq!(state.notice, Some(keymap::OP_RUNNING.into()));
    }

    #[test]
    fn n_or_esc_closes_the_question_and_removes_nothing() {
        for key in [KeyCode::Char('n'), KeyCode::Esc] {
            let mut state = with_worktrees(four_worktrees(), 1);
            press_char(&mut state, 'd');
            assert_eq!(press(&mut state, key), vec![]);
            assert_eq!(state.key_focus(), KeyScope::Worktrees, "{key:?}");
        }
    }

    #[test]
    fn a_finished_remove_reloads_and_a_failed_one_shows_gits_words() {
        let mut state = with_worktrees(four_worktrees(), 1);
        press_char(&mut state, 'd');
        let tag = match &press_char(&mut state, 'y')[..] {
            [Effect::RemoveWorktree { tag, .. }] => *tag,
            other => panic!("expected RemoveWorktree, got {other:?}"),
        };
        let mut failed = state.clone();
        let effects = state.update(AppIntent::WorktreeRemoved {
            tag,
            result: Ok(()),
        });
        assert_eq!(state.notice, Some("Worktree removed".into()));
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::LoadWorktrees { .. }))
        );
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::LoadBranches { .. }))
        );

        failed.update(AppIntent::WorktreeRemoved {
            tag,
            result: Err("fatal: contains modified or untracked files".into()),
        });
        match failed.popups.last() {
            Some(Popup::Error(error)) => {
                assert_eq!(error.title, "Could not remove worktree");
                assert!(error.message.contains("untracked"));
            }
            other => panic!("expected the error, got {other:?}"),
        }
    }

    #[test]
    fn stale_worktree_results_are_dropped_but_still_lower_busy() {
        // A load for an older seq, or an older generation.
        let mut state = with_branches();
        state.repo = Some("/r/a".into());
        let old = worktrees_tag(&press_char(&mut state, 'W'));
        let new = worktrees_tag(&state.update(AppIntent::OpenWorktrees));
        let busy = state.busy_count();
        state.update(AppIntent::WorktreesLoaded {
            tag: old,
            result: Ok(four_worktrees()),
        });
        let stale = Tag {
            generation: new.generation + 1,
            ..new
        };
        state.update(AppIntent::WorktreesLoaded {
            tag: stale,
            result: Ok(four_worktrees()),
        });
        assert!(!the_worktree_list(&state).list.loaded);
        assert_eq!(state.busy_count(), busy - 2);

        // An add whose popup was closed, and a remove from an older ask.
        let (mut state, tag) = adding();
        state.update(AppIntent::PopPopup);
        let busy = state.busy_count();
        assert_eq!(
            state.update(AppIntent::WorktreeAdded {
                tag,
                result: Err("boom".into()),
            }),
            vec![]
        );
        assert!(state.popups.is_empty());
        assert_eq!(state.busy_count(), busy - 1);

        let mut state = with_worktrees(four_worktrees(), 1);
        press_char(&mut state, 'd');
        let tag = match &press_char(&mut state, 'y')[..] {
            [Effect::RemoveWorktree { tag, .. }] => *tag,
            other => panic!("expected RemoveWorktree, got {other:?}"),
        };
        let old = Tag {
            seq: tag.seq - 1,
            ..tag
        };
        assert_eq!(
            state.update(AppIntent::WorktreeRemoved {
                tag: old,
                result: Err("boom".into()),
            }),
            vec![]
        );
        assert_eq!(state.key_focus(), KeyScope::Worktrees);
        let gone = Tag {
            generation: tag.generation + 1,
            ..tag
        };
        assert_eq!(
            state.update(AppIntent::WorktreeRemoved {
                tag: gone,
                result: Ok(()),
            }),
            vec![]
        );
    }
}
