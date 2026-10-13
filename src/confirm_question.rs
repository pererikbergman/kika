//! A question asked before a git op that cannot be undone. One place per
//! question holds its words, the keys that answer it and the re-check run
//! when it is answered, so a new question cannot forget the re-check or
//! bind Enter by mistake.

use crossterm::event::{KeyCode, KeyModifiers};

use crate::effect::Effect;
use crate::git::commit_files::CommitId;
use crate::git::status::Head;
use crate::keymap::{KeyPattern, KeyScope};
use crate::model::AppState;
use crate::op::GitOp;
use crate::popup::Popup;

/// The keys that answer yes to a question.
///
/// A destructive question answers only to `y`: Enter is unbound, so a key
/// meant for the panel underneath cannot throw work away. A question that
/// Enter may answer (the detached checkout, owner decision #227) joins
/// later as one more variant with its own key list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Answer {
    YOnly,
}

const Y_ONLY: &[KeyPattern] = &[KeyPattern::exact(KeyCode::Char('y'), KeyModifiers::NONE)];

impl Answer {
    /// The yes keys, for the question's binding.
    pub(crate) const fn keys(self) -> &'static [KeyPattern] {
        match self {
            Answer::YOnly => Y_ONLY,
        }
    }

    /// The hint that ends the question's text.
    fn hint(self) -> &'static str {
        match self {
            Answer::YOnly => "(y/n)",
        }
    }
}

/// One open question. The op is fixed when it is asked, so a reload before
/// the answer cannot change the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfirmQuestion {
    /// The key scope while it is on top; its bindings answer it.
    pub(crate) scope: KeyScope,
    pub(crate) title: &'static str,
    /// The question, without the answer hint.
    pub(crate) text: String,
    pub(crate) answer: Answer,
    pub(crate) op: GitOp,
    /// The detached `HEAD` it was asked from, so yes runs nothing once
    /// `HEAD` has moved. `None` on a branch.
    pub(crate) head: Option<CommitId>,
}

impl ConfirmQuestion {
    /// The words the popup shows, ending with how to answer.
    pub(crate) fn question(&self) -> String {
        format!("{} {}", self.text, self.answer.hint())
    }
}

impl AppState {
    /// Run the op the question on top carries, if it is the one `scope`
    /// answers, its guard still allows it and `HEAD` has not moved since
    /// it was asked. Any answer closes it.
    pub(crate) fn answer_question(&mut self, scope: KeyScope) -> Vec<Effect> {
        if !matches!(self.popups.last(), Some(Popup::Question(q)) if q.scope == scope) {
            return vec![];
        }
        let Some(Popup::Question(q)) = self.popups.pop() else {
            return vec![];
        };
        if self.confirm_refused(&q.op) || !self.head_still(q.head.as_ref()) {
            return vec![];
        }
        self.start_op(Ok(Some(q.op)))
    }

    /// `HEAD`'s commit when it is detached on an id git can use.
    pub(crate) fn detached_head(&self) -> Option<CommitId> {
        match &self.branch.as_ref()?.head {
            Head::Detached(oid) => CommitId::parse(oid),
            _ => None,
        }
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

    use super::*;
    use crate::features::commits::reset_hard_question;
    use crate::git::repo_state::Operation;
    use crate::intent::AppIntent;
    use crate::op::ResetMode;
    use crate::test_support::{
        checked, cid, on_branch, on_commit, op_of, press, press_char, reset_op,
    };

    #[test]
    fn a_destructive_answer_is_y_alone_and_never_enter() {
        let keys = Answer::YOnly.keys();
        assert_eq!(
            keys,
            [KeyPattern::exact(KeyCode::Char('y'), KeyModifiers::NONE)]
        );
        assert!(keys.iter().all(|key| key.code != KeyCode::Enter));
    }

    #[test]
    fn enter_and_confirm_yes_leave_the_hard_reset_question_and_y_runs_it() {
        let mut state = on_commit();
        assert_eq!(state.update(AppIntent::ResetTo(ResetMode::Hard)), vec![]);
        let asked = state.popups.clone();
        assert!(matches!(asked.as_slice(), [Popup::Question(_)]));

        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.popups, asked);
        assert_eq!(state.update(AppIntent::ConfirmYes), vec![]);
        assert_eq!(state.popups, asked);
        assert_eq!(state.held_seq(), None);

        let effects = press_char(&mut state, 'y');
        assert_eq!(effects.len(), 1);
        let (tag, op) = op_of(&effects);
        assert_eq!(op, reset_op(ResetMode::Hard));
        assert_eq!(state.held_seq(), Some(tag.seq));
        assert_eq!(state.popups, []);
    }

    #[test]
    fn the_hard_reset_question_refuses_once_head_has_moved() {
        let detached_elsewhere = |state: &mut AppState| {
            state.branch.as_mut().unwrap().head = Head::Detached("c2".into());
        };
        let onto_a_branch = |state: &mut AppState| state.branch = Some(on_branch("main"));
        for (name, move_head) in [
            ("detached", &detached_elsewhere as &dyn Fn(&mut AppState)),
            ("branch", &onto_a_branch),
        ] {
            let (mut state, _) = checked(AppIntent::ResetTo(ResetMode::Hard), Ok(vec![]));
            let asked_from = match state.popups.as_slice() {
                [Popup::Question(q)] => q.head.clone(),
                popups => panic!("{name}: {popups:?}"),
            };
            assert_eq!(asked_from, Some(cid("c0")), "{name}");
            move_head(&mut state);
            assert_eq!(state.update(AppIntent::ConfirmResetHard), vec![], "{name}");
            assert_eq!(state.popups, [], "{name}");
            assert_eq!(state.notice, Some("HEAD moved; try again".into()), "{name}");
            assert_eq!(state.held_seq(), None, "{name}");
        }

        // Where it was asked from, the answer runs the planned reset.
        let (mut state, _) = checked(AppIntent::ResetTo(ResetMode::Hard), Ok(vec![]));
        let (_, op) = op_of(&state.update(AppIntent::ConfirmResetHard));
        assert_eq!(op, reset_op(ResetMode::Hard));
    }

    #[test]
    fn the_hard_reset_question_waits_for_an_operation_in_progress() {
        for operation in [Operation::Merge, Operation::Rebase] {
            let mut state = on_commit();
            state.update(AppIntent::ResetTo(ResetMode::Hard));
            let mut branch = on_branch("main");
            branch.in_progress = vec![operation];
            state.branch = Some(branch);
            let reason = crate::keymap::in_progress_disabled(&state);
            assert!(reason.is_some(), "{operation:?}");
            assert_eq!(state.update(AppIntent::ConfirmResetHard), vec![]);
            assert_eq!(state.notice, reason, "{operation:?}");
            assert_eq!(state.popups, [], "{operation:?}");
            assert_eq!(state.held_seq(), None, "{operation:?}");
        }
    }

    #[test]
    fn a_question_for_another_scope_is_not_answered() {
        let mut state = on_commit();
        let question = reset_hard_question("c1".into(), reset_op(ResetMode::Hard), None);
        state.popups = vec![Popup::Question(question)];
        let popups = state.popups.clone();
        assert_eq!(state.answer_question(KeyScope::DeleteBranch), vec![]);
        assert_eq!(state.popups, popups);
        assert_eq!(state.held_seq(), None);
    }
}
