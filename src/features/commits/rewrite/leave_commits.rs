//! Checking out or resetting to a commit from a detached `HEAD`: before
//! `HEAD` moves, find the commits nothing else reaches and ask before
//! leaving them to the reflog.

use crate::effect::{Effect, Tag};
use crate::git::commit_files::CommitId;
use crate::git::status::Head;
use crate::keymap::KeyScope;
use crate::model::AppState;
use crate::op::{GitOp, ResetMode};
use crate::popup::{ConfirmKind, ErrorPopup, LeaveCommitsConfirm, Popup, ResetHardConfirm};

/// The left-behind check in flight, and where the user was when it
/// started. Its answer only asks if they are still there, so a key meant
/// for something else cannot answer the question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LeftBehindCheck {
    pub seq: u64,
    pub popups: Vec<Popup>,
    pub focus: KeyScope,
}

impl AppState {
    /// `HEAD`'s commit when it is detached on an id git can use.
    fn detached_head(&self) -> Option<CommitId> {
        match &self.branch.as_ref()?.head {
            Head::Detached(oid) => CommitId::parse(oid),
            _ => None,
        }
    }

    /// Plan `op`, which moves `HEAD` to `target`. On a detached `HEAD`,
    /// first look for the commits it would leave behind; anywhere else, go
    /// on as without the check. One check runs at a time, so two quick
    /// presses never stack two questions.
    pub(crate) fn move_head(&mut self, op: GitOp, target: CommitId) -> Vec<Effect> {
        let Some(head) = self.detached_head() else {
            return self.move_head_now(op, None);
        };
        if self.left_behind_check.is_some() {
            self.notice = Some("Still checking for commits left behind".into());
            return vec![];
        }
        let seq = self.take_seq();
        let tag = self.user_tag(seq);
        self.left_behind_check = Some(LeftBehindCheck {
            seq,
            popups: self.popups.clone(),
            focus: self.key_focus(),
        });
        vec![Effect::CheckLeftBehind {
            tag,
            op,
            head,
            target,
        }]
    }

    /// What `op` does once nothing would be left behind: a checkout and a
    /// hard reset still ask their own question, soft and mixed run. `head`
    /// is the detached `HEAD` the hard reset is asked from.
    fn move_head_now(&mut self, op: GitOp, head: Option<CommitId>) -> Vec<Effect> {
        match op {
            GitOp::CheckoutCommit(id) => {
                self.commits.pending = Some(id);
                self.popups
                    .push(Popup::Confirm(ConfirmKind::DetachedCheckout));
                vec![]
            }
            GitOp::Reset {
                ref to,
                mode: ResetMode::Hard,
            } => {
                let short_oid = self.commits.short_oid(to);
                self.popups.push(Popup::ResetHard(ResetHardConfirm {
                    short_oid,
                    op,
                    head,
                }));
                vec![]
            }
            op => self.start_op(Ok(Some(op))),
        }
    }

    /// Ask before leaving commits behind, or go on if there are none. A
    /// result for a `HEAD` that has since moved, for a user who has since
    /// moved to another popup or input, or a failed read, runs nothing.
    pub(crate) fn left_behind_checked(
        &mut self,
        tag: Tag,
        op: GitOp,
        head: CommitId,
        result: Result<Vec<CommitId>, String>,
    ) -> Vec<Effect> {
        if tag.user {
            self.busy = self.busy.saturating_sub(1);
        }
        // Taken before the generation check, so a stale answer cannot
        // leave the guard stuck.
        let Some(check) = self.left_behind_check.take_if(|check| check.seq == tag.seq) else {
            return vec![];
        };
        if tag.generation != self.generation {
            return vec![];
        }
        if !self.head_still(Some(&head)) {
            return vec![];
        }
        if self.popups != check.popups || self.key_focus() != check.focus {
            self.notice = Some("Not asked: focus moved; try again".into());
            return vec![];
        }
        let left = match result {
            Ok(left) => left,
            Err(message) => {
                self.push_error(ErrorPopup::new(
                    "Could not check for commits left behind",
                    &message,
                    "Close this and press the key again to retry; HEAD did not move.",
                ));
                return vec![];
            }
        };
        if left.is_empty() {
            return self.move_head_now(op, Some(head));
        }
        let target = match &op {
            GitOp::CheckoutCommit(id) | GitOp::Reset { to: id, .. } => id,
            _ => return vec![],
        };
        let short_oid = self.commits.short_oid(target);
        let left = left.iter().map(|id| self.commits.short_oid(id)).collect();
        let head_short_oid = self.commits.short_oid(&head);
        self.popups.push(Popup::LeaveCommits(LeaveCommitsConfirm {
            op,
            head,
            head_short_oid,
            short_oid,
            left,
        }));
        vec![]
    }

    /// Run the op the open question carries, and only if one is open, its
    /// guard still allows it and `HEAD` is still where it was asked from.
    pub(crate) fn confirm_leave_commits(&mut self) -> Vec<Effect> {
        if !matches!(self.popups.last(), Some(Popup::LeaveCommits(_))) {
            return vec![];
        }
        let Some(Popup::LeaveCommits(confirm)) = self.popups.pop() else {
            return vec![];
        };
        if self.confirm_refused(&confirm.op) {
            return vec![];
        }
        if !self.head_still(Some(&confirm.head)) {
            return vec![];
        }
        self.start_op(Ok(Some(confirm.op)))
    }

    /// Whether `HEAD` is still the detached `head` a question was asked
    /// from; `None` asks nothing. Says so when it has moved.
    pub(crate) fn head_still(&mut self, head: Option<&CommitId>) -> bool {
        let Some(head) = head else {
            return true;
        };
        if self.detached_head().as_ref() == Some(head) {
            return true;
        }
        self.notice = Some("HEAD moved; try again".into());
        false
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;

    use crate::effect::Tag;
    use crate::git::status::Head;
    use crate::intent::AppIntent;
    use crate::keymap::KeyScope;
    use crate::op::ResetMode;
    use crate::popup::{ConfirmKind, ErrorPopup, LeaveCommitsConfirm, Popup, ResetHardConfirm};
    use crate::test_support::{
        check_of, checked, cid, detached_on_commit, ids, moves, on_branch, on_commit, op_of, press,
        press_char, reset_op,
    };

    #[test]
    fn a_detached_head_checks_for_commits_left_behind_before_anything_else() {
        for (intent, planned) in moves() {
            let mut state = detached_on_commit();
            let busy = state.busy;
            let (tag, op) = check_of(&state.update(intent.clone()));
            assert_eq!(op, planned, "{intent:?}");
            assert!(tag.user, "{intent:?}");
            assert_eq!(state.busy, busy + 1, "{intent:?}");
            assert_eq!(state.popups, [], "{intent:?}");
            assert_eq!(state.commits.pending, None, "{intent:?}");
            assert_eq!(state.op_seq, None, "{intent:?}");
        }
    }

    #[test]
    fn on_a_branch_nothing_is_checked_first() {
        let mut state = on_commit();
        state.branch = Some(on_branch("main"));
        assert_eq!(state.update(AppIntent::CheckoutCommit), vec![]);
        assert_eq!(
            state.popups,
            [Popup::Confirm(ConfirmKind::DetachedCheckout)]
        );

        let mut state = on_commit();
        state.branch = Some(on_branch("main"));
        let (_, op) = op_of(&state.update(AppIntent::ResetTo(ResetMode::Mixed)));
        assert_eq!(op, reset_op(ResetMode::Mixed));
    }

    #[test]
    fn with_nothing_left_behind_each_move_goes_on_as_before() {
        let (state, effects) = checked(AppIntent::CheckoutCommit, Ok(vec![]));
        assert_eq!(effects, vec![]);
        assert_eq!(
            state.popups,
            [Popup::Confirm(ConfirmKind::DetachedCheckout)]
        );
        assert_eq!(state.commits.pending, Some(cid("c1")));

        for mode in [ResetMode::Soft, ResetMode::Mixed] {
            let (state, effects) = checked(AppIntent::ResetTo(mode), Ok(vec![]));
            let (tag, op) = op_of(&effects);
            assert_eq!(op, reset_op(mode));
            assert_eq!(state.op_seq, Some(tag.seq));
        }

        let (state, effects) = checked(AppIntent::ResetTo(ResetMode::Hard), Ok(vec![]));
        assert_eq!(effects, vec![]);
        assert_eq!(
            state.popups,
            [Popup::ResetHard(ResetHardConfirm {
                short_oid: "c1".into(),
                op: reset_op(ResetMode::Hard),
                head: Some(cid("c0")),
            })]
        );
    }

    #[test]
    fn commits_left_behind_open_the_question_and_run_nothing() {
        for (intent, planned) in moves() {
            let (state, effects) = checked(intent.clone(), Ok(ids(3)));
            assert_eq!(effects, vec![], "{intent:?}");
            assert_eq!(state.busy, detached_on_commit().busy, "{intent:?}");
            assert_eq!(state.op_seq, None, "{intent:?}");
            assert_eq!(
                state.popups,
                [Popup::LeaveCommits(LeaveCommitsConfirm {
                    op: planned,
                    head: cid("c0"),
                    head_short_oid: "c0".into(),
                    short_oid: "c1".into(),
                    left: vec!["a0".into(), "a1".into(), "a2".into()],
                })],
                "{intent:?}"
            );
            assert_eq!(state.key_focus(), KeyScope::LeaveCommits);
            let Some(Popup::LeaveCommits(ask)) = state.popups.last() else {
                unreachable!()
            };
            let question = ask.question();
            assert!(question.contains("3 commits"), "{question}");
            assert!(question.contains("(a0, a1, a2)"), "{question}");
        }

        let (state, _) = checked(AppIntent::CheckoutCommit, Ok(ids(7)));
        let Some(Popup::LeaveCommits(ask)) = state.popups.last() else {
            panic!("{:?}", state.popups)
        };
        let question = ask.question();
        assert!(
            question.contains("(a0, a1, a2, a3, a4 and 2 more)"),
            "{question}"
        );
        assert!(!question.contains("a5"), "{question}");
    }

    #[test]
    fn only_y_leaves_the_commits_behind_and_enter_is_no_answer() {
        for (intent, planned) in moves() {
            let (mut state, _) = checked(intent.clone(), Ok(ids(2)));
            assert_eq!(press(&mut state, KeyCode::Enter), vec![], "{intent:?}");
            assert_eq!(state.update(AppIntent::ConfirmYes), vec![], "{intent:?}");
            assert_eq!(state.popups.len(), 1, "{intent:?}");
            assert_eq!(state.op_seq, None, "{intent:?}");

            let (tag, op) = op_of(&press_char(&mut state, 'y'));
            assert_eq!(op, planned, "{intent:?}");
            assert_eq!(state.op_seq, Some(tag.seq), "{intent:?}");
            assert_eq!(state.popups, [], "{intent:?}");
        }

        for code in [KeyCode::Char('n'), KeyCode::Esc] {
            let (mut state, _) = checked(AppIntent::CheckoutCommit, Ok(ids(2)));
            assert_eq!(press(&mut state, code), vec![], "{code:?}");
            assert_eq!(state.popups, [], "{code:?}");
            assert_eq!(state.op_seq, None, "{code:?}");
        }
    }

    #[test]
    fn confirm_leave_commits_does_nothing_without_the_question() {
        let mut state = detached_on_commit();
        assert_eq!(state.update(AppIntent::ConfirmLeaveCommits), vec![]);
        state.popups.push(Popup::Confirm(ConfirmKind::Quit));
        assert_eq!(state.update(AppIntent::ConfirmLeaveCommits), vec![]);
        assert_eq!(state.popups, [Popup::Confirm(ConfirmKind::Quit)]);
        assert_eq!(state.op_seq, None);
    }

    #[test]
    fn a_check_for_a_head_that_moved_runs_nothing() {
        for result in [Ok(vec![]), Ok(ids(2))] {
            let mut state = detached_on_commit();
            let (tag, op) = check_of(&state.update(AppIntent::CheckoutCommit));
            state.branch.as_mut().unwrap().head = Head::Detached("c2".into());
            let effects = state.update(AppIntent::LeftBehindChecked {
                tag,
                op,
                head: cid("c0"),
                result,
            });
            assert_eq!(effects, vec![]);
            assert_eq!(state.popups, []);
            assert_eq!(state.notice, Some("HEAD moved; try again".into()));
            assert_eq!(state.op_seq, None);
            assert_eq!(state.commits.pending, None);
        }

        // Back on a branch is a move too.
        let mut state = detached_on_commit();
        let (tag, op) = check_of(&state.update(AppIntent::CheckoutCommit));
        state.branch = Some(on_branch("main"));
        state.update(AppIntent::LeftBehindChecked {
            tag,
            op,
            head: cid("c0"),
            result: Ok(vec![]),
        });
        assert_eq!(state.popups, []);
    }

    #[test]
    fn a_failed_check_shows_why_and_runs_nothing() {
        for (intent, _) in moves() {
            let (state, effects) = checked(intent.clone(), Err("fatal: bad".into()));
            assert_eq!(effects, vec![], "{intent:?}");
            assert_eq!(state.op_seq, None, "{intent:?}");
            let [Popup::Error(error)] = state.popups.as_slice() else {
                panic!("{intent:?}: {:?}", state.popups)
            };
            assert_eq!(error.title, "Could not check for commits left behind");
            assert!(error.message.contains("fatal: bad"));
            assert!(error.next.contains("retry"), "{}", error.next);
        }
    }

    #[test]
    fn a_second_press_while_a_check_runs_asks_only_once() {
        for (intent, _) in moves() {
            let mut state = detached_on_commit();
            let busy = state.busy;
            let (tag, op) = check_of(&state.update(intent.clone()));
            assert_eq!(state.update(intent.clone()), vec![], "{intent:?}");
            assert_eq!(state.busy, busy + 1, "{intent:?}");
            assert_eq!(
                state.notice,
                Some("Still checking for commits left behind".into()),
                "{intent:?}"
            );
            state.update(AppIntent::LeftBehindChecked {
                tag,
                op,
                head: cid("c0"),
                result: Ok(ids(2)),
            });
            assert!(
                matches!(state.popups.as_slice(), [Popup::LeaveCommits(_)]),
                "{intent:?}: {:?}",
                state.popups
            );
            assert_eq!(state.busy, busy, "{intent:?}");
        }
    }

    #[test]
    fn once_a_check_is_answered_the_next_press_checks_again() {
        // After the question, and after the error.
        for result in [Ok(ids(2)), Err("fatal: bad".to_string())] {
            let (mut state, _) = checked(AppIntent::CheckoutCommit, result.clone());
            state.popups.clear();
            check_of(&state.update(AppIntent::CheckoutCommit));
        }

        // An answer for an older repository clears it too.
        let mut state = detached_on_commit();
        let (tag, op) = check_of(&state.update(AppIntent::CheckoutCommit));
        state.generation += 1;
        state.update(AppIntent::LeftBehindChecked {
            tag,
            op: op.clone(),
            head: cid("c0"),
            result: Ok(ids(2)),
        });
        assert_eq!(state.popups, []);
        check_of(&state.update(AppIntent::CheckoutCommit));

        // An answer to some other check does not.
        let stale = Tag {
            seq: tag.seq + 1000,
            ..tag
        };
        state.update(AppIntent::LeftBehindChecked {
            tag: stale,
            op,
            head: cid("c0"),
            result: Ok(ids(2)),
        });
        assert_eq!(state.popups, []);
        assert_eq!(state.update(AppIntent::CheckoutCommit), vec![]);
    }

    #[test]
    fn an_answer_after_focus_moved_asks_nothing() {
        for name in ["error", "filter"] {
            for (intent, _) in moves() {
                for result in [Ok(ids(2)), Ok(vec![]), Err("fatal: bad".to_string())] {
                    let mut state = detached_on_commit();
                    let (tag, op) = check_of(&state.update(intent.clone()));
                    if name == "error" {
                        state.push_error(ErrorPopup::new("Elsewhere", "x", "y"));
                    } else {
                        press_char(&mut state, '/');
                        assert_eq!(state.key_focus(), KeyScope::Input);
                    }
                    let popups = state.popups.clone();
                    let effects = state.update(AppIntent::LeftBehindChecked {
                        tag,
                        op,
                        head: cid("c0"),
                        result,
                    });
                    let case = format!("{name} {intent:?}");
                    assert_eq!(effects, vec![], "{case}");
                    assert_eq!(state.popups, popups, "{case}");
                    assert_eq!(state.commits.pending, None, "{case}");
                    assert_eq!(state.op_seq, None, "{case}");
                    assert_eq!(
                        state.notice,
                        Some("Not asked: focus moved; try again".into()),
                        "{case}"
                    );
                    assert_eq!(state.update(AppIntent::ConfirmLeaveCommits), vec![]);
                    assert_eq!(state.update(AppIntent::ConfirmResetHard), vec![]);
                    assert_eq!(state.op_seq, None, "{case}");
                }
            }
        }
    }

    #[test]
    fn y_runs_nothing_once_head_has_moved() {
        for (intent, planned) in moves() {
            let (mut state, _) = checked(intent.clone(), Ok(ids(2)));
            let mut moved = state.clone();
            moved.branch.as_mut().unwrap().head = Head::Detached("c2".into());
            assert_eq!(
                moved.update(AppIntent::ConfirmLeaveCommits),
                vec![],
                "{intent:?}"
            );
            assert_eq!(moved.popups, [], "{intent:?}");
            assert_eq!(moved.op_seq, None, "{intent:?}");
            assert_eq!(moved.notice, Some("HEAD moved; try again".into()));

            let (_, op) = op_of(&state.update(AppIntent::ConfirmLeaveCommits));
            assert_eq!(op, planned, "{intent:?}");
        }
    }

    #[test]
    fn the_hard_reset_question_runs_nothing_once_head_has_moved() {
        let (mut state, _) = checked(AppIntent::ResetTo(ResetMode::Hard), Ok(vec![]));
        let mut moved = state.clone();
        moved.branch.as_mut().unwrap().head = Head::Detached("c2".into());
        assert_eq!(moved.update(AppIntent::ConfirmResetHard), vec![]);
        assert_eq!(moved.popups, []);
        assert_eq!(moved.op_seq, None);
        assert_eq!(moved.notice, Some("HEAD moved; try again".into()));

        // Back on a branch is a move too.
        let mut on_main = state.clone();
        on_main.branch = Some(on_branch("main"));
        assert_eq!(on_main.update(AppIntent::ConfirmResetHard), vec![]);
        assert_eq!(on_main.op_seq, None);

        let (_, op) = op_of(&state.update(AppIntent::ConfirmResetHard));
        assert_eq!(op, reset_op(ResetMode::Hard));
    }
}
