//! The operation's `update` handlers: merging the selected branch or
//! rebasing onto it, continuing the merge, rebase, cherry-pick or revert
//! in progress, asking before aborting it, and checking for work since
//! the stop before skipping, asking when there is some.

use super::OperationIntent;
use super::state::StopWorkCheck;
use crate::confirm_question::{Answer, ConfirmQuestion, Then};
use crate::effect::{Effect, Tag};
use crate::git::branches::BranchName;
use crate::git::commit_files::CommitId;
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
        OperationIntent::StopWorkChecked {
            tag,
            operation,
            head,
            result,
        } => state.stop_work_checked(tag, operation, head, result),
        OperationIntent::ConfirmSkip(operation) => state.confirm_skip(operation),
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

    /// Continue the operation in progress.
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

    /// Skip the commit the operation in progress stopped on, after a
    /// check for work done since the stop that would be lost with it.
    /// Nothing runs here: the check's answer skips or asks. One check
    /// runs at a time, so two quick presses never stack two questions.
    pub(crate) fn skip(&mut self) -> Vec<Effect> {
        let Some(operation) = self.operation_for_action(keymap::skip_disabled) else {
            return vec![];
        };
        if self.skip_check.is_some() {
            self.notice = Some("Still checking for changes since the stop".into());
            return vec![];
        }
        let seq = self.take_seq();
        let tag = self.user_tag(seq);
        self.skip_check = Some(StopWorkCheck {
            seq,
            popups: self.popups.clone(),
            focus: self.key_focus(),
        });
        vec![Effect::CheckStopWork {
            tag,
            operation,
            head: self.detached_head(),
        }]
    }

    /// Skip right away when nothing since the stop would be lost, and
    /// ask when something would or the check failed. An answer for an
    /// older check or repository, for a user who has since moved to
    /// another popup or input, for another operation or for a `HEAD` that
    /// has moved, does neither.
    fn stop_work_checked(
        &mut self,
        tag: Tag,
        operation: Operation,
        head: Option<CommitId>,
        result: Result<bool, String>,
    ) -> Vec<Effect> {
        self.user_done(tag);
        // Taken before the generation check, so a stale answer cannot
        // leave the guard stuck.
        let Some(check) = self.skip_check.take_if(|check| check.seq == tag.seq) else {
            return vec![];
        };
        if tag.generation != self.generation {
            return vec![];
        }
        if self.popups != check.popups || self.key_focus() != check.focus {
            self.notice = Some("Not asked: focus moved; try again".into());
            return vec![];
        }
        if !self.still_skipping(operation) || !self.head_still(head.as_ref()) {
            return vec![];
        }
        match result {
            Ok(false) => self.run_op_where_it_belongs(GitOp::Skip(operation)),
            // Fails closed: a check that could not tell asks too.
            Ok(true) | Err(_) => {
                if let Some(question) = skip_question(operation, head) {
                    self.popups.push(Popup::Question(question));
                }
                vec![]
            }
        }
    }

    /// Skip, as the question asked, if `operation` can still be skipped.
    /// The question already checked that `HEAD` has not moved.
    fn confirm_skip(&mut self, operation: Operation) -> Vec<Effect> {
        if !self.still_skipping(operation) {
            return vec![];
        }
        self.run_op_where_it_belongs(GitOp::Skip(operation))
    }

    /// Whether `operation` is still the one in progress and can be
    /// skipped now; says why not in the bar.
    fn still_skipping(&mut self, operation: Operation) -> bool {
        match self.operation_for_action(keymap::skip_disabled) {
            Some(now) if now == operation => true,
            Some(_) => {
                self.notice = Some("The operation in progress changed; try again".into());
                false
            }
            None => false,
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

/// "Skip the commit the rebase stopped on?", naming the operation the
/// check ran for, so the answer skips what the question said. Only `y`
/// answers: the conflict resolution done since the stop is lost. `head`
/// is the detached `HEAD` it is asked from, so a yes runs nothing once
/// that has moved. A merge has nothing to skip and a bisect skips from
/// its own keys, so neither has a question.
pub(crate) fn skip_question(
    operation: Operation,
    head: Option<CommitId>,
) -> Option<ConfirmQuestion> {
    let (title, name) = match operation {
        Operation::Rebase => ("Skip rebase commit", "rebase"),
        Operation::CherryPick => ("Skip cherry-pick commit", "cherry-pick"),
        Operation::Revert => ("Skip revert commit", "revert"),
        Operation::Merge | Operation::Bisect => return None,
    };
    Some(ConfirmQuestion {
        scope: KeyScope::SkipConfirm,
        title: title.into(),
        text: format!(
            "Skip the commit the {name} stopped on? \
             Conflict resolution done since the stop is lost. (y/n)"
        ),
        answer: Answer::YOnly,
        then: Then::Intent(Box::new(AppIntent::Operation(
            OperationIntent::ConfirmSkip(operation),
        ))),
        range: false,
        head,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::status::Head;
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

    // --- Skip, after the check for work since the stop ---

    const SKIPPABLE: [Operation; 3] = [Operation::Rebase, Operation::CherryPick, Operation::Revert];

    /// The one stop-work check in `effects`: its tag, operation and head.
    fn stop_check_of(effects: &[Effect]) -> (Tag, Operation, Option<CommitId>) {
        match effects {
            [
                Effect::CheckStopWork {
                    tag,
                    operation,
                    head,
                },
            ] => (*tag, *operation, head.clone()),
            other => panic!("expected one CheckStopWork, got {other:?}"),
        }
    }

    fn stop_work_checked(
        tag: Tag,
        operation: Operation,
        head: Option<CommitId>,
        result: Result<bool, String>,
    ) -> AppIntent {
        AppIntent::Operation(OperationIntent::StopWorkChecked {
            tag,
            operation,
            head,
            result,
        })
    }

    /// `status_focused` on `op` with `HEAD` detached on `c0`, as a rebase
    /// leaves it.
    fn detached_in(op: Operation) -> AppState {
        let mut state = status_focused(&[op]);
        state.branch.as_mut().unwrap().head = Head::Detached("c0".into());
        state
    }

    /// `detached_in(op)` after `s` and a check that came back with
    /// `result`, and what that returned.
    fn skip_checked(op: Operation, result: Result<bool, String>) -> (AppState, Vec<Effect>) {
        let mut state = detached_in(op);
        let (tag, operation, head) = stop_check_of(&press_char(&mut state, 's'));
        let effects = state.update(stop_work_checked(tag, operation, head, result));
        (state, effects)
    }

    #[test]
    fn s_checks_for_work_since_the_stop_and_runs_nothing_yet() {
        for op in SKIPPABLE {
            let mut state = status_focused(&[op]);
            let busy = state.busy_count();
            let (tag, operation, head) = stop_check_of(&press_char(&mut state, 's'));
            assert_eq!((operation, head), (op, None), "{op:?}");
            assert!(tag.user, "{op:?}");
            assert_eq!(state.busy_count(), busy + 1, "{op:?}");
            assert_eq!(state.held_seq(), None, "{op:?}");
            assert_eq!(state.popups, [], "{op:?}");

            let mut state = detached_in(op);
            let (_, operation, head) = stop_check_of(&state.update(AppIntent::SkipOperation));
            assert_eq!((operation, head), (op, Some(cid("c0"))), "{op:?}");
        }
    }

    #[test]
    fn a_second_s_while_the_check_runs_waits_for_it() {
        let mut state = status_focused(&[Operation::Rebase]);
        let busy = state.busy_count();
        let (tag, operation, head) = stop_check_of(&press_char(&mut state, 's'));
        assert_eq!(press_char(&mut state, 's'), vec![]);
        assert_eq!(
            state.notice,
            Some("Still checking for changes since the stop".into())
        );
        assert_eq!(state.busy_count(), busy + 1);

        state.update(stop_work_checked(tag, operation, head, Ok(true)));
        assert_eq!(state.busy_count(), busy);
        assert_eq!(
            state.popups,
            [Popup::Question(
                skip_question(Operation::Rebase, None).unwrap()
            )]
        );
    }

    #[test]
    fn with_no_work_since_the_stop_s_skips_on_the_terminal() {
        for op in SKIPPABLE {
            let (state, effects) = skip_checked(op, Ok(false));
            let (tag, git_op) = attached_of(&effects);
            assert_eq!(git_op, GitOp::Skip(op), "{op:?}");
            assert_eq!(state.held_seq(), Some(tag.seq), "{op:?}");
            assert_eq!(state.popups, [], "{op:?}");
        }
    }

    #[test]
    fn work_since_the_stop_or_a_failed_check_asks_first() {
        for op in SKIPPABLE {
            for result in [Ok(true), Err("fatal: bad".to_string())] {
                let case = format!("{op:?} {result:?}");
                let (state, effects) = skip_checked(op, result);
                assert_eq!(effects, vec![], "{case}");
                assert_eq!(state.held_seq(), None, "{case}");
                assert_eq!(
                    state.popups,
                    [Popup::Question(skip_question(op, Some(cid("c0"))).unwrap())],
                    "{case}"
                );
                assert_eq!(state.key_focus(), KeyScope::SkipConfirm, "{case}");
            }
        }
    }

    #[test]
    fn only_y_answers_the_skip_question() {
        for op in SKIPPABLE {
            let (mut state, _) = skip_checked(op, Ok(true));
            let asked = state.popups.clone();
            assert_eq!(press(&mut state, KeyCode::Enter), vec![], "{op:?}");
            assert_eq!(state.popups, asked, "{op:?}");
            // Another question's answer leaves it open.
            assert_eq!(
                state.answer_question(KeyScope::AbortConfirm, Answer::YOnly),
                vec![],
                "{op:?}"
            );
            assert_eq!(
                state.answer_question(KeyScope::SkipConfirm, Answer::YOrEnter),
                vec![],
                "{op:?}"
            );
            assert_eq!(state.popups, asked, "{op:?}");
            assert_eq!(state.held_seq(), None, "{op:?}");

            let (tag, git_op) = attached_of(&press_char(&mut state, 'y'));
            assert_eq!(git_op, GitOp::Skip(op), "{op:?}");
            assert_eq!(state.held_seq(), Some(tag.seq), "{op:?}");
            assert_eq!(state.popups, [], "{op:?}");
        }
    }

    #[test]
    fn confirm_yes_from_another_question_does_not_skip() {
        let (mut state, _) = skip_checked(Operation::Rebase, Ok(true));
        state.popups = vec![Popup::Question(abort_question(Operation::Rebase).unwrap())];
        let (_, op) = op_of(&state.update(AppIntent::ConfirmYes));
        assert_eq!(op, GitOp::Abort(Operation::Rebase));
    }

    #[test]
    fn n_or_esc_keeps_the_work_and_ctrl_c_quits_from_the_skip_question() {
        for no in [KeyCode::Char('n'), KeyCode::Esc] {
            let (mut state, _) = skip_checked(Operation::Rebase, Ok(true));
            assert_eq!(press(&mut state, no), vec![], "{no:?}");
            assert_eq!(state.popups, [], "{no:?}");
            assert_eq!(state.held_seq(), None, "{no:?}");
        }

        let (mut state, _) = skip_checked(Operation::Rebase, Ok(true));
        press_with(&mut state, 'c', KeyModifiers::CONTROL);
        assert!(state.should_quit);
        assert_eq!(state.held_seq(), None);
    }

    #[test]
    fn the_bar_offers_y_skip_and_n_keep() {
        let (state, _) = skip_checked(Operation::CherryPick, Ok(true));
        let rows = crate::test_support::screen::draw(&state, 100, 12);
        assert!(
            rows.iter()
                .any(|row| row.contains("  y skip  n keep  ctrl-c quit")),
            "{rows:#?}"
        );
        assert!(
            rows.iter()
                .any(|row| row.contains(" Skip cherry-pick commit ")),
            "{rows:#?}"
        );
    }

    #[test]
    fn the_skip_question_names_the_operation_and_only_y_answers() {
        let ask = skip_question(Operation::Revert, Some(cid("c0"))).unwrap();
        assert_eq!(ask.title, "Skip revert commit");
        assert_eq!(
            ask.text,
            "Skip the commit the revert stopped on? \
             Conflict resolution done since the stop is lost. (y/n)"
        );
        assert_eq!(
            (ask.scope, ask.answer, ask.range, ask.head),
            (KeyScope::SkipConfirm, Answer::YOnly, false, Some(cid("c0")))
        );
        assert_eq!(
            ask.then,
            Then::Intent(Box::new(AppIntent::Operation(
                OperationIntent::ConfirmSkip(Operation::Revert)
            )))
        );
        assert_eq!(
            skip_question(Operation::Rebase, None).unwrap().title,
            "Skip rebase commit"
        );
        assert_eq!(
            skip_question(Operation::CherryPick, None).unwrap().title,
            "Skip cherry-pick commit"
        );
        assert_eq!(skip_question(Operation::Merge, None), None);
        assert_eq!(skip_question(Operation::Bisect, None), None);
    }

    #[test]
    fn a_stale_check_runs_nothing_and_frees_the_next_press() {
        for result in [Ok(false), Ok(true)] {
            // An answer to some other check is ignored.
            let mut state = detached_in(Operation::Rebase);
            let (tag, operation, head) = stop_check_of(&press_char(&mut state, 's'));
            let stale = Tag {
                seq: tag.seq + 1000,
                ..tag
            };
            let effects = state.update(stop_work_checked(
                stale,
                operation,
                head.clone(),
                result.clone(),
            ));
            assert_eq!(effects, vec![]);
            assert_eq!(state.popups, []);
            assert_eq!(press_char(&mut state, 's'), vec![]);

            // An answer for an older repository runs nothing but clears it.
            state.generation += 1;
            let effects = state.update(stop_work_checked(tag, operation, head, result));
            assert_eq!(effects, vec![]);
            assert_eq!(state.popups, []);
            assert_eq!(state.held_seq(), None);
            stop_check_of(&press_char(&mut state, 's'));
        }
    }

    #[test]
    fn an_answer_after_focus_moved_runs_nothing() {
        for result in [Ok(false), Ok(true), Err("fatal: bad".to_string())] {
            let mut state = detached_in(Operation::Rebase);
            let (tag, operation, head) = stop_check_of(&press_char(&mut state, 's'));
            state.push_error(crate::popup::ErrorPopup::new("Elsewhere", "x", "y"));
            let popups = state.popups.clone();
            let effects = state.update(stop_work_checked(tag, operation, head, result));
            assert_eq!(effects, vec![]);
            assert_eq!(state.popups, popups);
            assert_eq!(state.held_seq(), None);
            assert_eq!(
                state.notice,
                Some("Not asked: focus moved; try again".into())
            );
        }
    }

    #[test]
    fn an_answer_once_the_operation_changed_or_an_op_runs_runs_nothing() {
        type Change = fn(&mut AppState);
        let cases: [(&str, Change, &str); 4] = [
            ("op running", |state| state.hold_op(99), keymap::OP_RUNNING),
            (
                "nothing in progress",
                |state| state.branch.as_mut().unwrap().in_progress = vec![],
                "Nothing in progress",
            ),
            (
                "another operation",
                |state| state.branch.as_mut().unwrap().in_progress = vec![Operation::CherryPick],
                "The operation in progress changed; try again",
            ),
            (
                "now a merge",
                |state| state.branch.as_mut().unwrap().in_progress = vec![Operation::Merge],
                "A merge cannot be skipped",
            ),
        ];
        for (name, change, notice) in cases {
            for result in [Ok(false), Ok(true), Err("fatal: bad".to_string())] {
                let case = format!("{name} {result:?}");
                let mut state = detached_in(Operation::Rebase);
                let (tag, operation, head) = stop_check_of(&press_char(&mut state, 's'));
                change(&mut state);
                let held = state.held_seq();
                let effects = state.update(stop_work_checked(tag, operation, head, result));
                assert_eq!(effects, vec![], "{case}");
                assert_eq!(state.popups, [], "{case}");
                assert_eq!(state.held_seq(), held, "{case}");
                assert_eq!(state.notice.as_deref(), Some(notice), "{case}");
            }

            // And at the answer to the question.
            let (mut state, _) = skip_checked(Operation::Rebase, Ok(true));
            change(&mut state);
            let held = state.held_seq();
            assert_eq!(press_char(&mut state, 'y'), vec![], "{name}");
            assert_eq!(state.popups, [], "{name}");
            assert_eq!(state.held_seq(), held, "{name}");
            assert_eq!(state.notice.as_deref(), Some(notice), "{name}");
        }
    }

    #[test]
    fn a_moved_detached_head_runs_nothing() {
        for result in [Ok(false), Ok(true), Err("fatal: bad".to_string())] {
            let mut state = detached_in(Operation::Rebase);
            let (tag, operation, head) = stop_check_of(&press_char(&mut state, 's'));
            state.branch.as_mut().unwrap().head = Head::Detached("c2".into());
            let effects = state.update(stop_work_checked(tag, operation, head, result));
            assert_eq!(effects, vec![]);
            assert_eq!(state.popups, []);
            assert_eq!(state.held_seq(), None);
            assert_eq!(state.notice, Some("HEAD moved; try again".into()));
        }

        let (mut state, _) = skip_checked(Operation::Rebase, Ok(true));
        state.branch.as_mut().unwrap().head = Head::Detached("c2".into());
        assert_eq!(press_char(&mut state, 'y'), vec![]);
        assert_eq!(state.popups, []);
        assert_eq!(state.held_seq(), None);
        assert_eq!(state.notice, Some("HEAD moved; try again".into()));
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
