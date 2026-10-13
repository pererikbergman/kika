//! Acting on the selected commit in the Commits panel: checking it out
//! as a detached `HEAD`, reverting it and resetting the branch to it.
//! Each reads the commit through [`AppState::commit_for_action`], which
//! the bisect and copy children also use, so only a hex id reaches argv.

use super::keys;
use crate::confirm_question::{Answer, ConfirmQuestion, Then};
use crate::effect::Effect;
use crate::git::commit_files::CommitId;
use crate::keymap::{KeyScope, Reason};
use crate::model::{AppState, Context};
use crate::op::{GitOp, ResetMode};

/// The selected commit's actions: checkout, revert and reset.
impl AppState {
    /// The selected commit's id and short id, or why no action may run on
    /// it now, said in the bar. Only an id of hex digits reaches argv.
    pub(crate) fn commit_for_action(
        &mut self,
        disabled: fn(&AppState) -> Option<Reason>,
    ) -> Option<(CommitId, String)> {
        if self.context != Context::Workspace {
            return None;
        }
        if let Some(reason) = disabled(self) {
            self.notice = Some(reason);
            return None;
        }
        let row = self.commits.rows.get(self.commits.selected)?;
        match CommitId::parse(&row.oid) {
            Some(id) => Some((id, row.short_oid.clone())),
            None => {
                self.notice = Some("Not a commit id git can use".into());
                None
            }
        }
    }

    /// Ask before leaving the branch: commits made on a detached `HEAD`
    /// belong to no branch. From a detached `HEAD`, look first for the
    /// commits the move would leave behind.
    pub(crate) fn ask_checkout_commit(&mut self) -> Vec<Effect> {
        match self.commit_for_action(keys::checkout_commit_disabled) {
            Some((id, _)) => self.move_head(GitOp::CheckoutCommit(id.clone()), id),
            None => vec![],
        }
    }

    /// Revert the selected commit on the terminal, so git's editor and the
    /// commit hooks work as on the command line.
    pub(crate) fn revert_commit(&mut self) -> Vec<Effect> {
        match self.commit_for_action(keys::revert_disabled) {
            Some((id, _)) => self.run_op_where_it_belongs(GitOp::Revert(id)),
            None => vec![],
        }
    }

    /// Reset to the selected commit. Soft and mixed keep the work; hard
    /// throws it away, so it asks first. From a detached `HEAD`, look
    /// first for the commits the reset would leave behind.
    pub(crate) fn reset_to(&mut self, mode: ResetMode) -> Vec<Effect> {
        let Some((to, _)) = self.commit_for_action(keys::reset_disabled) else {
            return vec![];
        };
        self.move_head(
            GitOp::Reset {
                to: to.clone(),
                mode,
            },
            to,
        )
    }
}

/// "Reset hard and lose every uncommitted change?", answered only by `y`.
/// `op` is the planned reset; `head` the detached `HEAD` it was asked from,
/// `None` on a branch.
pub(crate) fn reset_hard_question(
    short_oid: String,
    op: GitOp,
    head: Option<CommitId>,
) -> ConfirmQuestion {
    ConfirmQuestion {
        scope: KeyScope::ResetHard,
        title: "Reset hard".into(),
        text: format!(
            "Reset to {short_oid} and throw away every uncommitted change, staged and \
             unstaged? Commits after it leave the branch. (y/n)"
        ),
        answer: Answer::YOnly,
        then: Then::Op(op),
        range: false,
        head,
    }
}

/// "Check out this commit as a detached HEAD?" Git refuses to overwrite
/// local changes, so nothing is lost and Enter may answer too (owner
/// decision #227). `short_oid` names `id` as the log shows it. It has
/// never re-checked `HEAD`, so `head` is `None`.
pub(crate) fn checkout_question(short_oid: String, id: CommitId) -> ConfirmQuestion {
    ConfirmQuestion {
        scope: KeyScope::Confirm,
        title: "Detached checkout".into(),
        text: format!(
            "Check out {short_oid} as a detached HEAD? You will be on no branch; \
             new commits there belong to no branch until you create one. (y/n)"
        ),
        answer: Answer::YOrEnter,
        then: Then::Op(GitOp::CheckoutCommit(id)),
        range: false,
        head: None,
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;

    use super::*;
    use crate::git::repo_state::Operation;
    use crate::intent::AppIntent;
    use crate::keymap;
    use crate::menu::MenuId;
    use crate::popup::{ConfirmKind, Popup};
    use crate::test_support::{attached_of, cid, on_branch, on_commit, op_of, press, press_char};

    #[test]
    fn space_asks_before_a_detached_checkout_and_y_checks_out_the_commit() {
        let mut state = on_commit();
        assert_eq!(press_char(&mut state, ' '), vec![]);
        let [Popup::Question(q)] = state.popups.as_slice() else {
            panic!("expected the checkout question, got {:?}", state.popups);
        };
        assert_eq!(q.title, "Detached checkout");
        assert_eq!(q.answer, Answer::YOrEnter);
        assert_eq!(q.head, None);
        assert!(
            q.text
                .starts_with("Check out c1 as a detached HEAD? You will be on no branch;"),
        );
        assert_eq!(state.key_focus(), KeyScope::Confirm);
        assert_eq!(state.held_seq(), None);

        let (tag, op) = op_of(&press_char(&mut state, 'y'));
        assert_eq!(op, GitOp::CheckoutCommit(cid("c1")));
        assert_eq!(state.held_seq(), Some(tag.seq));
        assert_eq!(state.popups, []);
    }

    #[test]
    fn enter_also_answers_the_detached_checkout() {
        let mut state = on_commit();
        press_char(&mut state, ' ');
        let (_, op) = op_of(&press(&mut state, KeyCode::Enter));
        assert_eq!(op, GitOp::CheckoutCommit(cid("c1")));
        assert_eq!(state.popups, []);
    }

    #[test]
    fn n_or_esc_keeps_the_branch_and_forgets_the_commit() {
        for code in [KeyCode::Char('n'), KeyCode::Esc] {
            let mut state = on_commit();
            press_char(&mut state, ' ');
            assert_eq!(press(&mut state, code), vec![], "{code:?}");
            assert_eq!(state.popups, [], "{code:?}");
            assert_eq!(state.held_seq(), None, "{code:?}");
        }
    }

    #[test]
    fn capital_r_reverts_the_selected_commit_on_the_terminal() {
        let mut state = on_commit();
        let (tag, op) = attached_of(&press_char(&mut state, 'R'));
        assert_eq!(op, GitOp::Revert(cid("c1")));
        assert_eq!(state.held_seq(), Some(tag.seq));
    }

    #[test]
    fn a_revert_is_refused_on_a_merge_while_an_op_runs_or_during_an_operation() {
        let mut merge = on_commit();
        merge.commits.rows[1].parents = vec!["c2".into(), "d0".into()];
        assert_eq!(press_char(&mut merge, 'R'), vec![]);
        assert_eq!(merge.notice, Some("Cannot revert a merge commit".into()));
        assert_eq!(merge.update(AppIntent::RevertCommit), vec![]);
        assert_eq!(merge.held_seq(), None);

        let mut running = on_commit();
        running.hold_op(9);
        assert_eq!(press_char(&mut running, 'R'), vec![]);
        assert_eq!(running.notice, Some(keymap::OP_RUNNING.into()));
        assert_eq!(running.update(AppIntent::RevertCommit), vec![]);

        let mut reverting = on_commit();
        let mut branch = on_branch("main");
        branch.in_progress = vec![Operation::Revert];
        reverting.branch = Some(branch);
        assert_eq!(press_char(&mut reverting, 'R'), vec![]);
        assert_eq!(
            reverting.notice,
            Some("Finish or abort the operation in progress".into())
        );
        assert_eq!(reverting.update(AppIntent::RevertCommit), vec![]);
        assert_eq!(reverting.held_seq(), None);
    }

    #[test]
    fn capital_x_opens_the_reset_menu_and_soft_and_mixed_reset_at_once() {
        for (moves, mode) in [(0, ResetMode::Soft), (1, ResetMode::Mixed)] {
            let mut state = on_commit();
            assert_eq!(press_char(&mut state, 'X'), vec![]);
            assert_eq!(
                state.popups,
                [Popup::Menu {
                    id: MenuId::Reset,
                    cursor: 0
                }]
            );
            for _ in 0..moves {
                press_char(&mut state, 'j');
            }
            let (_, op) = op_of(&press(&mut state, KeyCode::Enter));
            assert_eq!(
                op,
                GitOp::Reset {
                    to: cid("c1"),
                    mode
                }
            );
            assert_eq!(state.popups, []);
        }
    }

    #[test]
    fn a_hard_reset_asks_first_and_only_y_resets() {
        let mut state = on_commit();
        press_char(&mut state, 'X');
        press_char(&mut state, 'j');
        press_char(&mut state, 'j');
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        let hard = GitOp::Reset {
            to: cid("c1"),
            mode: ResetMode::Hard,
        };
        assert_eq!(
            state.popups,
            [Popup::Question(reset_hard_question(
                "c1".into(),
                hard.clone(),
                None
            ))]
        );
        assert_eq!(state.key_focus(), KeyScope::ResetHard);
        assert_eq!(state.held_seq(), None);

        // Enter is not an answer: the question stays and nothing runs.
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.popups.len(), 1);
        assert_eq!(state.held_seq(), None);

        let (_, op) = op_of(&press_char(&mut state, 'y'));
        assert_eq!(op, hard);
        assert_eq!(state.popups, []);
    }

    #[test]
    fn n_or_esc_closes_the_hard_reset_question() {
        for code in [KeyCode::Char('n'), KeyCode::Esc] {
            let mut state = on_commit();
            state.update(AppIntent::ResetTo(ResetMode::Hard));
            assert_eq!(state.popups.len(), 1, "{code:?}");
            assert_eq!(press(&mut state, code), vec![], "{code:?}");
            assert_eq!(state.popups, [], "{code:?}");
            assert_eq!(state.held_seq(), None, "{code:?}");
        }
    }

    #[test]
    fn answering_the_hard_reset_does_nothing_without_the_question() {
        let mut state = on_commit();
        assert_eq!(
            state.answer_question(KeyScope::ResetHard, Answer::YOnly),
            vec![]
        );
        assert_eq!(state.popups, []);
        assert_eq!(state.update(AppIntent::ConfirmResetHard), vec![]);
        state.popups.push(Popup::Confirm(ConfirmKind::Quit));
        assert_eq!(
            state.answer_question(KeyScope::ResetHard, Answer::YOnly),
            vec![]
        );
        assert_eq!(state.update(AppIntent::ConfirmResetHard), vec![]);
        assert_eq!(state.popups, [Popup::Confirm(ConfirmKind::Quit)]);
        assert_eq!(state.held_seq(), None);
    }

    #[test]
    fn a_reset_waits_for_the_operation_in_progress() {
        let mut state = on_commit();
        let mut branch = on_branch("main");
        branch.in_progress = vec![Operation::Merge];
        state.branch = Some(branch);
        assert_eq!(press_char(&mut state, 'X'), vec![]);
        assert_eq!(state.popups, []);
        assert_eq!(
            state.notice,
            Some("Finish or abort the operation in progress".into())
        );
        for mode in [ResetMode::Soft, ResetMode::Hard] {
            assert_eq!(state.update(AppIntent::ResetTo(mode)), vec![]);
        }
        assert_eq!(state.popups, []);
        assert_eq!(state.held_seq(), None);
    }
}
