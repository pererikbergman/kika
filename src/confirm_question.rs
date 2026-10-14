//! A yes/no question, most often asked before a git op that cannot be
//! undone. One place per question holds its words, the keys that answer
//! it and what a yes does: an op re-checked when it is answered, or an
//! intent whose handler checks again. So a new question cannot forget the
//! re-check or bind Enter by mistake.

use std::borrow::Cow;

use crossterm::event::{KeyCode, KeyModifiers};

use crate::effect::Effect;
use crate::intent::AppIntent;
use crate::keymap::{KeyPattern, KeyScope};
use crate::model::AppState;
use crate::op::GitOp;
use crate::popup::{MergePrConfirm, Popup};
use kika_git::commit_files::CommitId;
use kika_git::path::RepoPath;
use kika_git::status::Head;

/// The keys that answer yes to a question.
///
/// A destructive question answers only to its own deliberate key: `y`, or
/// `D` for a force delete. Enter is unbound for them, so a key meant for
/// the panel underneath cannot throw work away. Only the detached
/// checkout lets Enter answer too (owner decision #227): git refuses to
/// overwrite local changes there, so nothing is lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Answer {
    YOnly,
    /// A force delete, which may lose commits: only `D` answers.
    Force,
    /// `y` or Enter.
    YOrEnter,
}

const Y_ONLY: &[KeyPattern] = &[KeyPattern::exact(KeyCode::Char('y'), KeyModifiers::NONE)];
const FORCE: &[KeyPattern] = &[KeyPattern::any(KeyCode::Char('D'))];
const Y_OR_ENTER: &[KeyPattern] = &[
    KeyPattern::exact(KeyCode::Char('y'), KeyModifiers::NONE),
    KeyPattern::any(KeyCode::Enter),
];

impl Answer {
    /// The yes keys, for the question's binding.
    pub(crate) const fn keys(self) -> &'static [KeyPattern] {
        match self {
            Answer::YOnly => Y_ONLY,
            Answer::Force => FORCE,
            Answer::YOrEnter => Y_OR_ENTER,
        }
    }
}

/// What a yes does, fixed when the question is asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Then {
    Op(GitOp),
    /// Remove this linked worktree; it runs as its own effect, not an op.
    RemoveWorktree(RepoPath),
    /// Ask GitHub to merge the pull request as described.
    MergePr(Box<MergePrConfirm>),
    /// Send this intent: its handler does the work and its own checks.
    Intent(Box<AppIntent>),
}

/// One open question. What yes does is fixed when it is asked, so a
/// reload before the answer cannot change the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfirmQuestion {
    /// The key scope while it is on top; its bindings answer it.
    pub(crate) scope: KeyScope,
    pub(crate) title: Cow<'static, str>,
    /// The words the popup shows, including how to answer, which is not
    /// always at the end.
    pub(crate) text: String,
    pub(crate) answer: Answer,
    pub(crate) then: Then,
    /// Asked over a list's range: a yes forgets that range.
    pub(crate) range: bool,
    /// The detached `HEAD` it was asked from, so yes runs nothing once
    /// `HEAD` has moved. `None` on a branch, or when `HEAD` is not checked.
    pub(crate) head: Option<CommitId>,
}

impl AppState {
    /// Close the question on top and hand it back, if `scope` and
    /// `answer` are the ones it was asked with, its guard still allows
    /// its op and `HEAD` has not moved since it was asked. Any other
    /// question, or none, stays as it was.
    pub(crate) fn take_question(
        &mut self,
        scope: KeyScope,
        answer: Answer,
    ) -> Option<ConfirmQuestion> {
        if !matches!(
            self.popups.last(),
            Some(Popup::Question(q)) if q.scope == scope && q.answer == answer
        ) {
            return None;
        }
        let Some(Popup::Question(q)) = self.popups.pop() else {
            return None;
        };
        if let Then::Op(op) = &q.then
            && self.confirm_refused(op)
        {
            return None;
        }
        if !self.head_still(q.head.as_ref()) {
            return None;
        }
        Some(q)
    }

    /// Run the op or send the intent the question on top carries, if
    /// [`Self::take_question`] hands it over. Only for questions whose
    /// yes is an op or an intent.
    pub(crate) fn answer_question(&mut self, scope: KeyScope, answer: Answer) -> Vec<Effect> {
        match self.take_question(scope, answer).map(|q| q.then) {
            Some(Then::Op(op)) => self.start_op(Ok(Some(op))),
            Some(Then::Intent(intent)) => self.update(*intent),
            _ => vec![],
        }
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

/// "Quit Kika?", which `y` or Enter answers: nothing is lost by quitting.
pub(crate) fn quit_question() -> ConfirmQuestion {
    ConfirmQuestion {
        scope: KeyScope::Confirm,
        title: "Quit".into(),
        text: "Quit Kika? (y/n)".into(),
        answer: Answer::YOrEnter,
        then: Then::Intent(Box::new(AppIntent::Quit)),
        range: false,
        head: None,
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;

    use super::*;
    use crate::features::commits::CommitsIntent;
    use crate::features::commits::reset_hard_question;
    use crate::intent::AppIntent;
    use crate::op::ResetMode;
    use crate::test_support::{
        checked, cid, on_branch, on_commit, op_of, press, press_char, reset_op,
    };
    use kika_git::repo_state::Operation;

    #[test]
    fn a_destructive_answer_is_y_or_d_alone_and_only_the_checkout_takes_enter() {
        let y = KeyPattern::exact(KeyCode::Char('y'), KeyModifiers::NONE);
        assert_eq!(Answer::YOnly.keys(), [y]);
        assert_eq!(Answer::Force.keys(), [KeyPattern::any(KeyCode::Char('D'))]);
        assert_eq!(
            Answer::YOrEnter.keys(),
            [y, KeyPattern::any(KeyCode::Enter)]
        );
        for answer in [Answer::YOnly, Answer::Force, Answer::YOrEnter] {
            let enter = answer.keys().iter().any(|key| key.code == KeyCode::Enter);
            assert_eq!(enter, answer == Answer::YOrEnter, "{answer:?}");
        }
    }

    /// Each question's yes binding, with the answer its question is asked
    /// with.
    const YES_BINDINGS: [(&str, KeyScope, Answer); 20] = [
        (
            "discard_confirm.yes",
            KeyScope::DiscardConfirm,
            Answer::YOnly,
        ),
        ("delete_branch.yes", KeyScope::DeleteBranch, Answer::YOnly),
        ("delete_branch.force", KeyScope::DeleteBranch, Answer::Force),
        ("remove_remote.yes", KeyScope::RemoveRemote, Answer::YOnly),
        ("delete_tag.yes", KeyScope::DeleteTag, Answer::YOnly),
        ("delete_range.yes", KeyScope::DeleteRange, Answer::YOnly),
        ("drop_stash.yes", KeyScope::DropStash, Answer::YOnly),
        ("leave_commits.yes", KeyScope::LeaveCommits, Answer::YOnly),
        (
            "remove_worktree.yes",
            KeyScope::RemoveWorktree,
            Answer::YOnly,
        ),
        ("travel.yes", KeyScope::Travel, Answer::YOnly),
        ("merge_pr.yes", KeyScope::MergePullRequest, Answer::YOnly),
        (
            "checkout_pr.yes",
            KeyScope::CheckoutPullRequest,
            Answer::YOnly,
        ),
        ("reset_hard.yes", KeyScope::ResetHard, Answer::YOnly),
        ("confirm.yes", KeyScope::Confirm, Answer::YOrEnter),
        ("abort_confirm.yes", KeyScope::AbortConfirm, Answer::YOnly),
        ("skip_confirm.yes", KeyScope::SkipConfirm, Answer::YOnly),
        (
            "autosquash_confirm.yes",
            KeyScope::AutosquashConfirm,
            Answer::YOnly,
        ),
        ("patch_confirm.yes", KeyScope::PatchConfirm, Answer::YOnly),
        (
            "drop_range_confirm.yes",
            KeyScope::DropRangeConfirm,
            Answer::YOnly,
        ),
        (
            "create_pull_request.yes",
            KeyScope::CreatePullRequest,
            Answer::YOnly,
        ),
    ];

    #[test]
    fn every_question_binding_answers_with_its_question_keys_and_is_repo_locked() {
        use crate::keymap::{REPO_LOCKED, registry};
        for (id, scope, answer) in YES_BINDINGS {
            let binding = registry().iter().find(|b| b.id == id).expect(id);
            assert_eq!(binding.scope, scope, "{id}");
            assert_eq!(binding.keys, answer.keys(), "{id}");
            assert!(REPO_LOCKED.contains(&id), "{id}");
        }
        // No other binding in a destructive question's scope takes Enter.
        for binding in registry() {
            let destructive = YES_BINDINGS
                .iter()
                .any(|(_, scope, answer)| *scope == binding.scope && *answer != Answer::YOrEnter);
            let enter = binding.keys.iter().any(|key| key.code == KeyCode::Enter);
            assert!(!(destructive && enter), "{}", binding.id);
        }
    }

    #[test]
    fn enter_and_confirm_yes_leave_the_hard_reset_question_and_y_runs_it() {
        let mut state = on_commit();
        assert_eq!(
            state.update(AppIntent::Commits(CommitsIntent::ResetTo(ResetMode::Hard))),
            vec![]
        );
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
            let (mut state, _) = checked(
                AppIntent::Commits(CommitsIntent::ResetTo(ResetMode::Hard)),
                Ok(vec![]),
            );
            let asked_from = match state.popups.as_slice() {
                [Popup::Question(q)] => q.head.clone(),
                popups => panic!("{name}: {popups:?}"),
            };
            assert_eq!(asked_from, Some(cid("c0")), "{name}");
            move_head(&mut state);
            assert_eq!(
                state.update(AppIntent::Commits(CommitsIntent::ConfirmResetHard)),
                vec![],
                "{name}"
            );
            assert_eq!(state.popups, [], "{name}");
            assert_eq!(state.notice, Some("HEAD moved; try again".into()), "{name}");
            assert_eq!(state.held_seq(), None, "{name}");
        }

        // Where it was asked from, the answer runs the planned reset.
        let (mut state, _) = checked(
            AppIntent::Commits(CommitsIntent::ResetTo(ResetMode::Hard)),
            Ok(vec![]),
        );
        let (_, op) = op_of(&state.update(AppIntent::Commits(CommitsIntent::ConfirmResetHard)));
        assert_eq!(op, reset_op(ResetMode::Hard));
    }

    #[test]
    fn the_detached_checkout_question_refuses_once_head_has_moved() {
        let detached_elsewhere = |state: &mut AppState| {
            state.branch.as_mut().unwrap().head = Head::Detached("c2".into());
        };
        let onto_a_branch = |state: &mut AppState| state.branch = Some(on_branch("main"));
        let by_y = |state: &mut AppState| press_char(state, 'y');
        let by_enter = |state: &mut AppState| press(state, KeyCode::Enter);
        let by_intent = |state: &mut AppState| state.update(AppIntent::ConfirmYes);
        type Yes<'a> = &'a dyn Fn(&mut AppState) -> Vec<Effect>;
        for (name, move_head) in [
            ("detached", &detached_elsewhere as &dyn Fn(&mut AppState)),
            ("branch", &onto_a_branch),
        ] {
            for (how, answer) in [
                ("y", &by_y as Yes),
                ("enter", &by_enter),
                ("confirm_yes", &by_intent),
            ] {
                let (mut state, _) =
                    checked(AppIntent::Commits(CommitsIntent::Checkout), Ok(vec![]));
                let asked_from = match state.popups.as_slice() {
                    [Popup::Question(q)] => q.head.clone(),
                    popups => panic!("{name}/{how}: {popups:?}"),
                };
                assert_eq!(asked_from, Some(cid("c0")), "{name}/{how}");
                move_head(&mut state);
                assert_eq!(answer(&mut state), vec![], "{name}/{how}");
                assert_eq!(state.popups, [], "{name}/{how}");
                assert_eq!(
                    state.notice,
                    Some("HEAD moved; try again".into()),
                    "{name}/{how}"
                );
                assert_eq!(state.held_seq(), None, "{name}/{how}");
            }
        }

        // Where it was asked from, the answer checks out the planned commit.
        let (mut state, _) = checked(AppIntent::Commits(CommitsIntent::Checkout), Ok(vec![]));
        let effects = press_char(&mut state, 'y');
        let (tag, op) = op_of(&effects);
        assert_eq!(op, GitOp::CheckoutCommit(cid("c1")));
        assert_eq!(state.held_seq(), Some(tag.seq));
        assert_eq!(state.popups, []);
    }

    #[test]
    fn the_hard_reset_question_waits_for_an_operation_in_progress() {
        for operation in [Operation::Merge, Operation::Rebase] {
            let mut state = on_commit();
            state.update(AppIntent::Commits(CommitsIntent::ResetTo(ResetMode::Hard)));
            let mut branch = on_branch("main");
            branch.in_progress = vec![operation];
            state.branch = Some(branch);
            let reason = crate::keymap::in_progress_disabled(&state);
            assert!(reason.is_some(), "{operation:?}");
            assert_eq!(
                state.update(AppIntent::Commits(CommitsIntent::ConfirmResetHard)),
                vec![]
            );
            assert_eq!(state.notice, reason, "{operation:?}");
            assert_eq!(state.popups, [], "{operation:?}");
            assert_eq!(state.held_seq(), None, "{operation:?}");
        }
    }

    /// One of each question that only `y` (or `D`) answers, as asked.
    fn destructive_questions() -> Vec<ConfirmQuestion> {
        use crate::features::{branches, commits, stash};
        use crate::popup::{DeleteRangeConfirm, DiscardWhat, LeaveCommitsConfirm, RangeKind};
        use kika_forge::merge::MergeMethod;
        use kika_forge::{ForgeHost, ForgeRepo};
        let branch = |force| GitOp::DeleteBranch {
            name: "t".into(),
            force,
        };
        let tag = GitOp::DeleteTag {
            name: "v1".into(),
            oid: "1".repeat(40),
        };
        let travel = GitOp::Travel {
            direction: commits::reflog::Direction::Undo,
            step: commits::reflog::Step::MoveTo(cid("c1")),
            expect: commits::reflog::Expected {
                head_ref: None,
                oid: cid("c0"),
            },
        };
        let stash_ref = stash::StashRef {
            index: 0,
            oid: cid("c1"),
        };
        vec![
            DiscardWhat::File.into_question("a.rs".into(), GitOp::DiscardWorktree(vec![])),
            branches::delete_branch_question("t".into(), branch(false)),
            branches::delete_branch_question("t".into(), branch(true)),
            branches::remotes::remove_remote_question("o".into(), GitOp::RemoveRemote("o".into())),
            branches::tags::delete_tag_question("v1".into(), tag),
            DeleteRangeConfirm {
                kind: RangeKind::Tags,
                names: vec!["v1".into()],
                skipped: vec![],
                op: GitOp::DeleteTags(vec![]),
            }
            .into_question(),
            stash::drop_stash_question("stash@{0}: x", GitOp::StashDrop(stash_ref)),
            LeaveCommitsConfirm {
                op: GitOp::CheckoutCommit(cid("c1")),
                head: cid("c0"),
                head_short_oid: "c0".into(),
                short_oid: "c1".into(),
                left: vec!["a0".into()],
            }
            .into_question(),
            ConfirmQuestion {
                scope: KeyScope::RemoveWorktree,
                title: "Remove worktree".into(),
                text: "Remove worktree /r/w? (y/n)".into(),
                answer: Answer::YOnly,
                then: Then::RemoveWorktree("/r/w".into()),
                range: false,
                head: None,
            },
            commits::reflog::travel_question("Undo", &["Undo: x".into()], travel),
            crate::popup::MergePrConfirm {
                number: 7,
                title: "t".into(),
                base: "main".into(),
                head: "topic".into(),
                method: MergeMethod::Squash,
                head_oid: cid("c1"),
                repo: ForgeRepo {
                    host: "github.com".into(),
                    owner: "acme".into(),
                    name: "widgets".into(),
                },
                host: ForgeHost::parse("github.com").unwrap(),
                unstable: false,
            }
            .into_question(),
            reset_hard_question("c1".into(), reset_op(ResetMode::Hard), None),
            crate::features::operation::skip_question(Operation::Rebase, None).unwrap(),
            crate::features::operation::skip_question_unchecked(Operation::CherryPick, None)
                .unwrap(),
        ]
    }

    /// Whether `scope`'s yes binding sends `ConfirmYes`, which then
    /// answers its question: the discard and the skip question.
    fn yes_is_confirm_yes(scope: KeyScope) -> bool {
        crate::keymap::registry().iter().any(|b| {
            b.scope == scope && b.intent == AppIntent::ConfirmYes && b.id.ends_with(".yes")
        })
    }

    #[test]
    fn enter_never_answers_a_destructive_question_and_confirm_yes_only_a_discard_or_skip() {
        // Only these two destructive questions take `ConfirmYes`; any other
        // that starts to would drop its assertion below, so it must be
        // added here on purpose.
        let mut confirm_yes: Vec<KeyScope> = destructive_questions()
            .iter()
            .map(|q| q.scope)
            .filter(|scope| yes_is_confirm_yes(*scope))
            .collect();
        confirm_yes.dedup();
        assert_eq!(
            confirm_yes,
            [KeyScope::DiscardConfirm, KeyScope::SkipConfirm]
        );
        for question in destructive_questions() {
            let name = question.title.clone();
            assert_ne!(question.answer, Answer::YOrEnter, "{name}");
            let mut state = on_commit();
            state.popups = vec![Popup::Question(question)];
            let asked = state.popups.clone();
            assert_eq!(state.key_focus(), asked_scope(&asked), "{name}");
            assert_eq!(press(&mut state, KeyCode::Enter), vec![], "{name}");
            assert_eq!(state.popups, asked, "{name}");
            if !yes_is_confirm_yes(asked_scope(&asked)) {
                assert_eq!(state.update(AppIntent::ConfirmYes), vec![], "{name}");
                assert_eq!(state.popups, asked, "{name}");
            }
            assert_eq!(state.held_seq(), None, "{name}");
        }
    }

    fn asked_scope(popups: &[Popup]) -> KeyScope {
        match popups {
            [Popup::Question(q)] => q.scope,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_question_for_another_scope_is_not_answered() {
        let mut state = on_commit();
        let question = reset_hard_question("c1".into(), reset_op(ResetMode::Hard), None);
        state.popups = vec![Popup::Question(question)];
        let popups = state.popups.clone();
        assert_eq!(
            state.answer_question(KeyScope::DeleteBranch, Answer::YOnly),
            vec![]
        );
        assert_eq!(state.popups, popups);
        assert_eq!(state.held_seq(), None);
    }

    #[test]
    fn take_question_hands_over_only_the_question_asked_with_that_scope_and_answer() {
        let menu = || Popup::Menu {
            id: crate::menu::MenuId::Bisect,
            cursor: 0,
        };
        let reset = || reset_hard_question("c1".into(), reset_op(ResetMode::Hard), None);
        let cases = [
            ("none", vec![], KeyScope::ResetHard, Answer::YOnly),
            (
                "another popup",
                vec![menu()],
                KeyScope::ResetHard,
                Answer::YOnly,
            ),
            (
                "below another popup",
                vec![Popup::Question(reset()), menu()],
                KeyScope::ResetHard,
                Answer::YOnly,
            ),
            (
                "another scope",
                vec![Popup::Question(reset())],
                KeyScope::DeleteBranch,
                Answer::YOnly,
            ),
            (
                "another answer",
                vec![Popup::Question(reset())],
                KeyScope::ResetHard,
                Answer::YOrEnter,
            ),
        ];
        for (name, popups, scope, answer) in cases {
            let mut state = on_commit();
            state.popups = popups.clone();
            assert_eq!(state.take_question(scope, answer), None, "{name}");
            assert_eq!(state.popups, popups, "{name}");
            assert_eq!(state.notice, None, "{name}");
        }

        let mut state = on_commit();
        state.popups = vec![Popup::Question(reset())];
        assert_eq!(
            state.take_question(KeyScope::ResetHard, Answer::YOnly),
            Some(reset())
        );
        assert_eq!(state.popups, []);
    }
}
