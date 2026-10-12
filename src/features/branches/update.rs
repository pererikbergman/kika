//! The Branches feature's `update` handlers: loading the branch list and
//! the selected branch's log, moving between rows and tabs, and checking
//! out the selected branch.

use super::BranchesTab;
use super::keys;
use crate::effect::{Effect, Tag};
use crate::git::branches::{Branch, LogRow};
use crate::model::{AppState, Context};
use crate::op::GitOp;

/// The Branches panel: loading the list and the selected branch's log,
/// moving the selection, and checking out the selected branch.
impl AppState {
    /// [`Self::reload_branches`], for a change noticed outside Kika: out
    /// of the busy count, and only one at a time.
    pub(crate) fn background_branches(&mut self) -> Vec<Effect> {
        if self.watch.branches_pending {
            return vec![];
        }
        self.load_branches(false)
    }

    /// Load the local branches and the remotes together: whatever moves
    /// one can move the other. A background remotes load is not counted:
    /// the branch load beside it keeps a second pair from starting.
    pub(crate) fn load_branches(&mut self, user: bool) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        let seq = self.take_seq();
        self.branches.list_seq = Some(seq);
        self.start_branch_load(user);
        let remotes = self.take_seq();
        self.branches.remotes.list_seq = Some(remotes);
        if user {
            self.busy += 1;
        }
        let tags = self.take_seq();
        self.branches.tags.list_seq = Some(tags);
        if user {
            self.busy += 1;
        }
        let tag = |seq| Tag {
            generation: self.generation,
            seq,
            user,
        };
        vec![
            Effect::LoadBranches { tag: tag(seq) },
            Effect::LoadRemotes { tag: tag(remotes) },
            Effect::LoadTags { tag: tag(tags) },
        ]
    }

    /// A user load raises the busy count; a background one the watch's flag.
    fn start_branch_load(&mut self, user: bool) {
        if user {
            self.busy += 1;
        } else {
            self.watch.branches_pending = true;
        }
    }

    /// Every branch load reports back once, stale or not.
    fn end_branch_load(&mut self, user: bool) {
        if user {
            self.busy = self.busy.saturating_sub(1);
        } else {
            self.watch.branches_pending = false;
        }
    }

    /// Show the list, and load the selected branch's log afresh: a
    /// reload means its commits may have moved.
    pub(crate) fn branches_loaded(
        &mut self,
        tag: Tag,
        result: Result<Vec<Branch>, String>,
    ) -> Vec<Effect> {
        self.end_branch_load(tag.user);
        if tag.generation != self.generation || self.branches.list_seq != Some(tag.seq) {
            return vec![];
        }
        self.branches.list_seq = None;
        if let Ok(branches) = &result {
            self.forge.set_branches(branches.iter().map(|b| &b.name));
        }
        let listed = result.is_ok();
        self.branches.loaded(result);
        let mut effects = if self.branches.tab == BranchesTab::Local {
            self.load_branch_log(tag.user)
        } else {
            vec![]
        };
        if listed {
            effects.extend(self.load_pull_requests());
        }
        effects
    }

    /// Start loading the log of the branch selected on the tab shown;
    /// with none selected, or a remote's header row, there is no log to
    /// show. `user` as for the list.
    pub(crate) fn load_branch_log(&mut self, user: bool) -> Vec<Effect> {
        let refname = match self.branches.tab {
            BranchesTab::Local => self.branches.selected_branch().map(|b| b.name.ref_bytes()),
            BranchesTab::Remotes => self
                .branches
                .remotes
                .selected_branch()
                .map(|b| b.refname.clone()),
            BranchesTab::Tags => self
                .branches
                .tags
                .selected_tag()
                .map(|t| t.name.ref_bytes()),
        };
        let Some(refname) = refname else {
            self.branches.log = Default::default();
            return vec![];
        };
        let seq = self.take_seq();
        self.branches.log.begin(seq, refname.clone());
        self.start_branch_load(user);
        let tag = Tag {
            generation: self.generation,
            seq,
            user,
        };
        vec![Effect::LoadBranchLog { tag, refname }]
    }

    pub(crate) fn branch_log_loaded(
        &mut self,
        tag: Tag,
        refname: Vec<u8>,
        result: Result<Vec<LogRow>, String>,
    ) {
        self.end_branch_load(tag.user);
        let log = &mut self.branches.log;
        if tag.generation != self.generation || log.for_ref.as_ref() != Some(&refname) {
            return;
        }
        log.apply(tag.seq, result);
    }

    /// Show the tab `step` gives, and the log of what is selected there.
    pub(crate) fn switch_branches_tab(
        &mut self,
        step: fn(BranchesTab) -> BranchesTab,
    ) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        self.branches.clear_range(self.branches.tab);
        self.branches.tab = step(self.branches.tab);
        self.load_branch_log(true)
    }

    pub(crate) fn move_branch(&mut self, delta: isize) -> Vec<Effect> {
        if self.branches.move_by(delta) {
            self.load_branch_log(true)
        } else {
            vec![]
        }
    }

    pub(crate) fn checkout_branch(&mut self) -> Vec<Effect> {
        match self.branch_for_action(keys::checkout_branch_disabled) {
            Some(name) => self.start_op(Ok(Some(GitOp::Checkout(name)))),
            None => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::effect::Effect;
    use crate::features::branches::BranchesTab;
    use crate::git::branches::LogRow;
    use crate::intent::AppIntent;
    use crate::keymap::{KeyPress, KeyScope};
    use crate::model::{AppState, Context};
    use crate::op::GitOp;
    use crate::popup::{Popup, PromptKind};
    use crate::test_support::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    fn log_row(subject: &str) -> LogRow {
        LogRow {
            short_oid: "abc1234".into(),
            date: "2024-01-01".into(),
            author: "a".into(),
            subject: subject.into(),
        }
    }

    #[test]
    fn the_branches_panel_takes_its_own_keys() {
        let state = with_branches();
        assert_eq!(state.panel_focus(), KeyScope::Branches);
        assert_eq!(state.key_focus(), KeyScope::Branches);
    }

    #[test]
    fn a_reload_keeps_the_selected_branch_by_name() {
        let mut state = with_branches();
        state.update(AppIntent::BranchNext);
        assert_eq!(selected_name(&state), "topic");

        let tag = branches_tag(&state.update(AppIntent::RefreshStatus));
        let mut moved = three_branches();
        moved.swap(1, 2);
        state.update(branches_loaded(tag, moved));

        assert_eq!(selected_name(&state), "topic");
        assert_eq!(state.branches.selected, Some(2));
    }

    #[test]
    fn a_stale_branch_list_or_log_is_dropped() {
        let mut state = with_branches();
        let old = branches_tag(&state.update(AppIntent::RefreshStatus));
        let new = branches_tag(&state.update(AppIntent::RefreshStatus));
        state.update(branches_loaded(old, vec![git_branch("stale", true, true)]));
        assert_eq!(state.branches.branches, three_branches());

        let mut other_repo = new;
        other_repo.generation += 1;
        state.update(branches_loaded(other_repo, vec![]));
        assert_eq!(state.branches.branches, three_branches());

        let (first, refname) = log_load(&state.update(AppIntent::BranchNext)).unwrap();
        let (second, _) = log_load(&state.update(AppIntent::BranchNext)).unwrap();
        state.update(AppIntent::BranchLogLoaded {
            tag: first,
            refname,
            result: Ok(vec![log_row("topic's")]),
        });
        assert!(
            state.branches.log.rows.is_empty(),
            "the log of wip is awaited"
        );
        state.update(AppIntent::BranchLogLoaded {
            tag: second,
            refname: b"refs/heads/wip".to_vec(),
            result: Ok(vec![log_row("wip's")]),
        });
        assert_eq!(state.branches.log.rows, [log_row("wip's")]);
    }

    #[test]
    fn moving_the_selection_loads_the_new_branch_log() {
        let mut state = with_branches();
        let (tag, refname) = log_load(&state.update(AppIntent::BranchNext)).unwrap();
        assert_eq!(refname, b"refs/heads/topic");
        assert!(tag.user);
        assert_eq!(
            state.branches.log.for_ref.as_deref(),
            Some(&b"refs/heads/topic"[..])
        );
        // At the end there is nowhere to go, so nothing loads.
        state.update(AppIntent::BranchNext);
        assert_eq!(state.update(AppIntent::BranchNext), vec![]);
    }

    #[test]
    fn j_and_k_move_the_branch_selection_and_shift_j_scrolls_its_log() {
        let mut state = with_branches();
        press_char(&mut state, 'j');
        assert_eq!(selected_name(&state), "topic");
        let (tag, refname) = log_load(&press_char(&mut state, 'j')).unwrap();
        state.update(AppIntent::BranchLogLoaded {
            tag,
            refname,
            result: Ok(vec![log_row("1"), log_row("2")]),
        });
        state.update(AppIntent::Key(KeyPress::new(
            KeyCode::Char('J'),
            KeyModifiers::SHIFT,
        )));
        assert_eq!(state.branches.log.scroll, 1);
        press_char(&mut state, 'k');
        assert_eq!(selected_name(&state), "topic");
        assert_eq!(state.branches.log.scroll, 0, "another branch's log");
    }

    #[test]
    fn checking_out_runs_the_op_and_its_finish_reloads_status_and_branches() {
        let mut state = with_branches();
        state.update(AppIntent::BranchNext);
        let (tag, op) = op_of(&press_char(&mut state, ' '));
        assert_eq!(op, GitOp::Checkout("topic".into()));

        let effects = state.update(finished(tag, op, Ok(())));

        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::LoadStatus { .. })),
            "{effects:?}"
        );
        branches_tag(&effects);
    }

    #[test]
    fn checking_out_or_deleting_the_current_branch_is_refused() {
        let mut state = with_branches();
        assert_eq!(state.update(AppIntent::CheckoutBranch), vec![]);
        assert_eq!(state.notice, Some("Already on this branch".into()));
        assert_eq!(state.update(AppIntent::DeleteBranch), vec![]);
        assert_eq!(
            state.notice,
            Some("Cannot delete the current branch".into())
        );
        assert_eq!(state.popups, []);
        assert_eq!(state.op_seq, None);
    }

    #[test]
    fn branch_actions_are_refused_while_an_op_runs_or_with_nothing_selected() {
        let mut state = with_branches();
        state.update(AppIntent::BranchNext);
        state.op_seq = Some(99);
        for intent in [
            AppIntent::CheckoutBranch,
            AppIntent::NewBranch,
            AppIntent::RenameBranch,
            AppIntent::DeleteBranch,
        ] {
            state.notice = None;
            assert_eq!(state.update(intent.clone()), vec![], "{intent:?}");
            assert_eq!(
                state.notice,
                Some("An operation is running".into()),
                "{intent:?}"
            );
        }
        assert_eq!(state.popups, []);

        let mut empty = AppState::new(Context::Workspace);
        empty.update(AppIntent::CheckoutBranch);
        assert_eq!(empty.notice, Some("No branch selected".into()));
    }

    #[test]
    fn deleting_a_merged_branch_asks_and_y_deletes_without_force() {
        let mut state = with_branches();
        state.update(AppIntent::BranchNext);
        press_char(&mut state, 'd');
        assert_eq!(state.key_focus(), KeyScope::DeleteBranch);
        // `D` answers only the question about an unmerged branch.
        state.update(AppIntent::Key(KeyPress::new(
            KeyCode::Char('D'),
            KeyModifiers::SHIFT,
        )));
        assert_eq!(state.op_seq, None);
        assert_eq!(state.popups.len(), 1);

        let (_, op) = op_of(&press_char(&mut state, 'y'));

        assert_eq!(
            op,
            GitOp::DeleteBranch {
                name: "topic".into(),
                force: false
            }
        );
        assert_eq!(state.popups, []);
    }

    #[test]
    fn deleting_an_unmerged_branch_ignores_y_and_d_forces() {
        let mut state = with_branches();
        state.update(AppIntent::BranchNext);
        state.update(AppIntent::BranchNext);
        press_char(&mut state, 'd');
        match state.popups.as_slice() {
            [Popup::DeleteBranch(confirm)] => assert!(confirm.force()),
            other => panic!("expected the delete question, got {other:?}"),
        }

        assert_eq!(press_char(&mut state, 'y'), vec![]);
        assert_eq!(state.popups.len(), 1, "y leaves the question open");
        assert_eq!(state.op_seq, None);

        let (_, op) = op_of(&state.update(AppIntent::Key(KeyPress::new(
            KeyCode::Char('D'),
            KeyModifiers::SHIFT,
        ))));
        assert_eq!(
            op,
            GitOp::DeleteBranch {
                name: "wip".into(),
                force: true
            }
        );
    }

    #[test]
    fn n_or_esc_keeps_the_branch() {
        for key in [KeyCode::Char('n'), KeyCode::Esc] {
            let mut state = with_branches();
            state.update(AppIntent::BranchNext);
            press_char(&mut state, 'd');
            assert_eq!(press(&mut state, key), vec![]);
            assert_eq!(state.popups, []);
            assert!(!state.should_quit);
            assert_eq!(state.op_seq, None);
        }
    }

    #[test]
    fn the_create_prompt_uses_the_branch_selected_when_it_opened() {
        let mut state = with_branches();
        state.update(AppIntent::BranchNext);
        press_char(&mut state, 'n');
        assert_eq!(
            state.prompt_title(PromptKind::NewBranch),
            "New branch from topic"
        );
        // A reload while the user types moves the list under the prompt.
        let tag = branches_tag(&state.update(AppIntent::RefreshStatus));
        state.update(branches_loaded(tag, vec![git_branch("main", true, true)]));

        type_text(&mut state, "fresh");
        let (_, op) = op_of(&press(&mut state, KeyCode::Enter));

        assert_eq!(
            op,
            GitOp::CreateBranch {
                name: "fresh".into(),
                from: "topic".into()
            }
        );
        assert_eq!(state.branches.pending, None);
    }

    #[test]
    fn the_rename_prompt_starts_from_the_old_name_and_renames_that_branch() {
        let mut state = with_branches();
        state.update(AppIntent::BranchNext);
        state.update(AppIntent::BranchNext);
        press(&mut state, KeyCode::Char('R'));
        match state.popups.as_slice() {
            [Popup::Prompt { kind, input }] => {
                assert_eq!(*kind, PromptKind::RenameBranch);
                assert_eq!((input.text.as_str(), input.col), ("wip", 3));
            }
            other => panic!("expected the rename prompt, got {other:?}"),
        }
        let tag = branches_tag(&state.update(AppIntent::RefreshStatus));
        state.update(branches_loaded(tag, three_branches()[..2].to_vec()));

        type_text(&mut state, "-2");
        let (_, op) = op_of(&press(&mut state, KeyCode::Enter));

        assert_eq!(
            op,
            GitOp::RenameBranch {
                from: "wip".into(),
                to: "wip-2".into()
            }
        );
    }

    #[test]
    fn an_empty_name_is_refused_and_a_cancelled_prompt_forgets_the_branch() {
        let mut state = with_branches();
        press_char(&mut state, 'n');
        type_text(&mut state, "  ");
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.notice, Some("A branch needs a name".into()));
        assert_eq!(state.op_seq, None);

        press_char(&mut state, 'n');
        assert!(state.branches.pending.is_some());
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.branches.pending, None);
        assert_eq!(
            state.update(AppIntent::CreateBranchNamed("x".into())),
            vec![]
        );
    }

    #[test]
    fn switching_repository_resets_the_panel() {
        let mut state = with_branches().with_location(Some("/r/a".into()), "/r/a".into());
        state.branches.tab = BranchesTab::Remotes;
        let effects = state.update(AppIntent::OpenRepo("/r/b".into()));
        assert!(!state.branches.loaded);
        assert_eq!(state.branches.branches, []);
        assert_eq!(state.branches.log, Default::default());
        assert_eq!(state.branches.tab, BranchesTab::Local);
        assert!(!state.branches.remotes.loaded);
        assert!(
            effects.iter().any(
                |e| matches!(e, Effect::LoadRemotes { tag } if tag.generation == state.generation)
            ),
            "{effects:?}"
        );
    }
}
