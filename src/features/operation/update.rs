//! The operation's `update` handlers: merging the selected branch or
//! rebasing onto it, and continuing, skipping or asking before aborting
//! the merge, rebase, cherry-pick or revert in progress.

use super::OperationIntent;
use crate::confirm_question::{Answer, ConfirmQuestion, Then};
use crate::effect::Effect;
use crate::git::branches::BranchName;
use crate::git::repo_state::Operation;
use crate::intent::AppIntent;
use crate::keymap::{self, KeyScope, Reason};
use crate::model::{AppState, Context};
use crate::op::GitOp;
use crate::popup::Popup;

/// The operation's half of `update`: core's one `AppIntent::Operation` arm
/// lands here. A free function, so its name cannot clash with handlers.
pub(crate) fn update(state: &mut AppState, intent: OperationIntent) -> Vec<Effect> {
    match intent {
        OperationIntent::Merge(strategy) => {
            state.branch_op(|branch| GitOp::Merge { branch, strategy })
        }
        OperationIntent::RebaseOnto => state.branch_op(|onto| GitOp::Rebase { onto }),
        OperationIntent::ConfirmAbort(operation) => {
            state.start_op(Ok(Some(GitOp::Abort(operation))))
        }
    }
}

/// Merge, rebase onto, and the operation in progress.
impl AppState {
    /// Merge the selected branch, or rebase onto it: the op `make` builds
    /// from its name, run on the terminal when it may make commits.
    pub(crate) fn branch_op(&mut self, make: impl FnOnce(BranchName) -> GitOp) -> Vec<Effect> {
        match self.branch_for_action(super::keys::merge_disabled) {
            Some(name) => self.run_op_where_it_belongs(make(name)),
            None => vec![],
        }
    }

    /// The merge, rebase or cherry-pick in progress, or why none can be
    /// acted on now, said in the bar.
    fn operation_for_action(
        &mut self,
        disabled: fn(&AppState) -> Option<Reason>,
    ) -> Option<Operation> {
        if self.context != Context::Workspace {
            return None;
        }
        if let Some(reason) = disabled(self) {
            self.notice = Some(reason);
            return None;
        }
        keymap::resumable_operation(self)
    }

    /// Continue or skip the operation in progress.
    pub(crate) fn resume(
        &mut self,
        disabled: fn(&AppState) -> Option<Reason>,
        make: fn(Operation) -> GitOp,
    ) -> Vec<Effect> {
        match self.operation_for_action(disabled) {
            Some(operation) => self.run_op_where_it_belongs(make(operation)),
            None => vec![],
        }
    }

    /// Ask before aborting: resolved conflicts are thrown away. The
    /// question names the operation, and the answer aborts that one.
    pub(crate) fn ask_abort(&mut self) {
        if let Some(question) = self
            .operation_for_action(keymap::resume_disabled)
            .and_then(abort_question)
        {
            self.popups.push(Popup::Question(question));
        }
    }
}

/// "Abort the merge?", naming the operation found when asked, so the
/// answer aborts what the question said. Only `y` answers: resolved
/// conflicts are lost. A bisect is not aborted here but reset, from the
/// bisect menu, so it has no question.
pub(crate) fn abort_question(operation: Operation) -> Option<ConfirmQuestion> {
    let (title, name) = match operation {
        Operation::Merge => ("Abort merge", "merge"),
        Operation::Rebase => ("Abort rebase", "rebase"),
        Operation::CherryPick => ("Abort cherry-pick", "cherry-pick"),
        Operation::Revert => ("Abort revert", "revert"),
        Operation::Bisect => return None,
    };
    Some(ConfirmQuestion {
        scope: KeyScope::AbortConfirm,
        title: title.into(),
        text: format!(
            "Abort the {name} and return to where it started? \
             Resolved conflicts are lost. (y/n)"
        ),
        answer: Answer::YOnly,
        then: Then::Intent(Box::new(AppIntent::Operation(
            OperationIntent::ConfirmAbort(operation),
        ))),
        range: false,
        head: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent::AppIntent;
    use crate::keymap::KeyScope;
    use crate::menu::MenuId;
    use crate::op::MergeStrategy;
    use crate::test_support::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    // --- Merge, rebase and the operation in progress ---

    #[test]
    fn m_on_another_branch_opens_the_merge_menu() {
        let mut state = in_progress(&[]);
        next_branch(&mut state);
        assert_eq!(press_char(&mut state, 'm'), vec![]);
        assert_eq!(
            state.popups,
            [Popup::Menu {
                id: MenuId::Merge,
                cursor: 0
            }]
        );
    }

    #[test]
    fn fast_forward_and_squash_run_detached_on_the_selected_branch() {
        for strategy in [MergeStrategy::FastForwardOnly, MergeStrategy::Squash] {
            let mut state = in_progress(&[]);
            next_branch(&mut state);
            let (_, op) =
                op_of(&state.update(AppIntent::Operation(OperationIntent::Merge(strategy))));
            assert_eq!(
                op,
                GitOp::Merge {
                    branch: "topic".into(),
                    strategy
                }
            );
        }
    }

    #[test]
    fn a_merge_commit_and_a_rebase_run_on_the_terminal() {
        let mut state = in_progress(&[]);
        next_branch(&mut state);
        let (tag, op) = attached_of(&state.update(AppIntent::Operation(OperationIntent::Merge(
            MergeStrategy::NoFastForward,
        ))));
        assert_eq!(
            op,
            GitOp::Merge {
                branch: "topic".into(),
                strategy: MergeStrategy::NoFastForward
            }
        );
        assert_eq!(state.held_seq(), Some(tag.seq));
        state.update(finished(tag, op, Ok(())));

        let (_, op) = attached_of(&state.update(AppIntent::Operation(OperationIntent::RebaseOnto)));
        assert_eq!(
            op,
            GitOp::Rebase {
                onto: "topic".into()
            }
        );
    }

    #[test]
    fn the_merge_menu_runs_the_chosen_item() {
        let mut state = in_progress(&[]);
        next_branch(&mut state);
        press_char(&mut state, 'm');
        press_char(&mut state, 'j');
        let (_, op) = attached_of(&press(&mut state, KeyCode::Enter));
        assert!(matches!(
            op,
            GitOp::Merge {
                strategy: MergeStrategy::NoFastForward,
                ..
            }
        ));
        assert_eq!(state.popups, []);
    }

    #[test]
    fn merging_the_current_branch_or_during_an_operation_is_refused() {
        let mut state = in_progress(&[]);
        assert_eq!(press_char(&mut state, 'm'), vec![]);
        assert_eq!(state.notice, Some("Already on this branch".into()));
        assert_eq!(state.popups, []);
        assert_eq!(
            state.update(AppIntent::Operation(OperationIntent::Merge(
                MergeStrategy::Squash
            ))),
            vec![]
        );
        assert_eq!(
            state.update(AppIntent::Operation(OperationIntent::RebaseOnto)),
            vec![]
        );
        assert_eq!(state.notice, Some("Already on this branch".into()));

        let mut merging = in_progress(&[Operation::Merge]);
        next_branch(&mut merging);
        assert_eq!(press_char(&mut merging, 'm'), vec![]);
        assert_eq!(
            merging.notice,
            Some("Finish or abort the operation in progress".into())
        );
        assert_eq!(merging.popups, []);
        assert_eq!(
            merging.update(AppIntent::Operation(OperationIntent::RebaseOnto)),
            vec![]
        );
        assert_eq!(merging.held_seq(), None);
    }

    #[test]
    fn c_continues_a_merge_on_the_terminal_and_s_cannot_skip_it() {
        let mut state = status_focused(&[Operation::Merge]);
        let (tag, op) = attached_of(&press_char(&mut state, 'c'));
        assert_eq!(op, GitOp::Continue(Operation::Merge));
        state.update(finished(tag, op, Ok(())));

        assert_eq!(press_char(&mut state, 's'), vec![]);
        assert_eq!(state.notice, Some("A merge cannot be skipped".into()));
        assert_eq!(state.update(AppIntent::SkipOperation), vec![]);
        assert_eq!(state.held_seq(), None);
    }

    #[test]
    fn s_skips_a_rebase_commit_on_the_terminal() {
        let mut state = status_focused(&[Operation::Rebase]);
        let (_, op) = attached_of(&press_char(&mut state, 's'));
        assert_eq!(op, GitOp::Skip(Operation::Rebase));
    }

    #[test]
    fn a_asks_first_and_y_aborts_detached() {
        let mut state = status_focused(&[Operation::Merge]);
        assert_eq!(press_char(&mut state, 'a'), vec![]);
        assert_eq!(
            state.popups,
            [Popup::Question(abort_question(Operation::Merge).unwrap())]
        );
        assert_eq!(state.held_seq(), None);

        let (_, op) = op_of(&press_char(&mut state, 'y'));
        assert_eq!(op, GitOp::Abort(Operation::Merge));
        assert_eq!(state.popups, []);
    }

    #[test]
    fn n_keeps_the_operation() {
        let mut state = status_focused(&[Operation::Merge]);
        press_char(&mut state, 'a');
        assert_eq!(press_char(&mut state, 'n'), vec![]);
        assert_eq!(state.popups, []);
        assert_eq!(state.held_seq(), None);
    }

    #[test]
    fn enter_does_not_answer_the_abort_question() {
        let mut state = status_focused(&[Operation::Merge]);
        press_char(&mut state, 'a');
        let question = [Popup::Question(abort_question(Operation::Merge).unwrap())];
        assert_eq!(state.popups, question);
        assert_eq!(state.key_focus(), KeyScope::AbortConfirm);

        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.held_seq(), None);
        assert_eq!(state.popups, question);
    }

    #[test]
    fn y_aborts_every_operation_that_can_be_aborted() {
        for op in [
            Operation::Merge,
            Operation::Rebase,
            Operation::CherryPick,
            Operation::Revert,
        ] {
            let mut state = status_focused(&[op]);
            assert_eq!(press_char(&mut state, 'a'), vec![], "{op:?}");
            assert_eq!(state.key_focus(), KeyScope::AbortConfirm, "{op:?}");
            let (_, git_op) = op_of(&press_char(&mut state, 'y'));
            assert_eq!(git_op, GitOp::Abort(op));
            assert_eq!(state.popups, [], "{op:?}");
        }
    }

    #[test]
    fn n_or_esc_keeps_the_operation_and_ctrl_c_quits_from_the_abort_question() {
        for no in [KeyCode::Char('n'), KeyCode::Esc] {
            let mut state = status_focused(&[Operation::Rebase]);
            press_char(&mut state, 'a');
            assert_eq!(press(&mut state, no), vec![], "{no:?}");
            assert_eq!(state.popups, [], "{no:?}");
            assert_eq!(state.held_seq(), None, "{no:?}");
        }

        let mut state = status_focused(&[Operation::Rebase]);
        press_char(&mut state, 'a');
        press_with(&mut state, 'c', KeyModifiers::CONTROL);
        assert!(state.should_quit);
        assert_eq!(state.held_seq(), None);
    }

    #[test]
    fn with_nothing_in_progress_the_status_keys_are_disabled() {
        let mut state = status_focused(&[]);
        for c in ['c', 's', 'a'] {
            state.notice = None;
            assert_eq!(press_char(&mut state, c), vec![], "{c}");
            assert_eq!(state.notice, Some("Nothing in progress".into()), "{c}");
        }
        for intent in [
            AppIntent::ContinueOperation,
            AppIntent::SkipOperation,
            AppIntent::AbortOperation,
        ] {
            assert_eq!(state.update(intent), vec![]);
        }
        assert_eq!(state.popups, []);
        assert_eq!(state.held_seq(), None);
    }

    #[test]
    fn the_status_keys_wait_for_a_running_op() {
        let mut state = status_focused(&[Operation::Rebase]);
        state.hold_op(9);
        for c in ['c', 's', 'a'] {
            state.notice = None;
            assert_eq!(press_char(&mut state, c), vec![], "{c}");
            assert_eq!(state.notice, Some(keymap::OP_RUNNING.into()), "{c}");
        }
        assert_eq!(state.popups, []);
    }
}
