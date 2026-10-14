//! Bisect's `update` handlers: marking the selected commit or the one
//! git checked out, asking before the reset and running it, and saying
//! what a status load found.

use super::BisectIntent;
use crate::bisect;
use crate::confirm_question::{Answer, ConfirmQuestion, Then};
use crate::effect::Effect;
use crate::features::commits::CommitsIntent;
use crate::intent::AppIntent;
use crate::keymap::KeyScope;
use crate::model::{AppState, Context};
use crate::op::GitOp;
use crate::popup::Popup;
use kika_git::bisect::{BisectMark, BisectState};

/// The bisect's half of `update`, which `commits::update` passes
/// `CommitsIntent::Bisect` to. A free function, so its name cannot clash
/// with handlers.
pub(crate) fn update(state: &mut AppState, intent: BisectIntent) -> Vec<Effect> {
    match intent {
        BisectIntent::MarkSelected(mark) => return state.bisect_selected(mark),
        BisectIntent::MarkHead(mark) => return state.bisect_head(mark),
        BisectIntent::AskReset => state.ask_bisect_reset(),
        BisectIntent::ConfirmReset => return state.bisect_reset(),
    }
    Vec::new()
}

/// Bisecting: marks from the Commits and Status panels, the reset, and
/// saying what a status load found.
impl AppState {
    /// Mark the selected commit. With no bisect in progress the mark
    /// starts one; git checks nothing out until it has a good and a bad.
    pub(crate) fn bisect_selected(&mut self, mark: BisectMark) -> Vec<Effect> {
        let disabled = bisect::selected_disabled(mark);
        let Some((commit, _)) = self.commit_for_action(disabled) else {
            return vec![];
        };
        let start = !bisect::bisecting(self);
        self.start_op(Ok(Some(GitOp::BisectMark {
            mark,
            commit,
            start,
        })))
    }

    /// Mark the commit git checked out to test.
    pub(crate) fn bisect_head(&mut self, mark: BisectMark) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        if let Some(reason) = (bisect::head_disabled(mark))(self) {
            self.notice = Some(reason);
            return vec![];
        }
        let Some(commit) = bisect::head_commit(self) else {
            return vec![];
        };
        self.start_op(Ok(Some(GitOp::BisectMark {
            mark,
            commit,
            start: false,
        })))
    }

    /// Ask before resetting: the marks are dropped and the files change.
    pub(crate) fn ask_bisect_reset(&mut self) {
        if self.context != Context::Workspace {
            return;
        }
        match bisect::reset_disabled(self) {
            Some(reason) => self.notice = Some(reason),
            None => {
                let question = self.bisect_reset_question();
                self.popups.push(Popup::Question(question));
            }
        }
    }

    /// "Reset the bisect?", naming where `HEAD` goes back to. Only `y`
    /// answers: the marks are dropped and the files change.
    pub(crate) fn bisect_reset_question(&self) -> ConfirmQuestion {
        ConfirmQuestion {
            scope: KeyScope::AbortConfirm,
            title: "Reset bisect".into(),
            text: bisect::reset_question(bisect::bisect_state(self)),
            answer: Answer::YOnly,
            then: Then::Intent(Box::new(AppIntent::Commits(CommitsIntent::Bisect(
                BisectIntent::ConfirmReset,
            )))),
            range: false,
            head: None,
        }
    }

    pub(crate) fn bisect_reset(&mut self) -> Vec<Effect> {
        if let Some(reason) = bisect::reset_disabled(self) {
            self.notice = Some(reason);
            return vec![];
        }
        self.start_op(Ok(Some(GitOp::BisectReset)))
    }

    /// Say so when this load is the first to show the bisect's outcome;
    /// the same outcome loaded again says nothing.
    pub(crate) fn announce_bisect(&mut self, before: Option<BisectState>) {
        let Some(now) = bisect::bisect_state(self) else {
            return;
        };
        let was_found = before.as_ref().and_then(|b| b.first_bad.as_ref());
        let was_only_skipped = before.as_ref().is_some_and(|b| b.only_skipped);
        if let Some(found) = &now.first_bad {
            if was_found != Some(found) {
                let short = self.commits.short_oid(&found.commit);
                self.notice = Some(bisect::found_notice(&short, &found.subject).into());
            }
        } else if now.only_skipped && !was_only_skipped {
            self.notice = Some(bisect::ONLY_SKIPPED.into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::Tag;
    use crate::intent::AppIntent;
    use crate::menu::MenuId;
    use crate::test_support::*;
    use crossterm::event::KeyCode;
    use kika_git::FileEntry;
    use kika_git::repo_state::Operation;
    use kika_git::status::{BranchStatus, EntryKind, Head, RepoStatus};

    fn status_with(tag: Tag, branch: BranchStatus) -> AppIntent {
        AppIntent::StatusLoaded {
            tag,
            result: Ok(RepoStatus {
                branch,
                files: vec![],
            }),
        }
    }

    #[test]
    fn a_load_that_brings_the_first_bad_commit_announces_it_once() {
        let mut state = testing_c1(&[Operation::Bisect]);
        let found = BisectState {
            bad: 2,
            good: 1,
            first_bad: Some(kika_git::bisect::Found {
                commit: cid("c1"),
                subject: "commit 1".into(),
            }),
            ..BisectState::default()
        };
        let branch = bisect_branch(
            Head::Detached("c1".into()),
            &[Operation::Bisect],
            Some(found),
        );

        let tag = refresh_tag(&mut state);
        state.update(status_with(tag, branch.clone()));
        assert_eq!(
            state.notice.as_deref(),
            Some("Bisect found c1 (a resets): commit 1")
        );

        press_char(&mut state, 'j');
        assert_eq!(state.notice, None);
        let tag = refresh_tag(&mut state);
        state.update(status_with(tag, branch));
        assert_eq!(state.notice, None);
    }

    #[test]
    fn a_load_that_leaves_only_skipped_commits_says_so() {
        let mut state = testing_c1(&[Operation::Bisect]);
        let stuck = BisectState {
            only_skipped: true,
            ..BisectState::default()
        };
        let tag = refresh_tag(&mut state);
        state.update(status_with(
            tag,
            bisect_branch(
                Head::Detached("c1".into()),
                &[Operation::Bisect],
                Some(stuck),
            ),
        ));
        assert_eq!(state.notice, Some(bisect::ONLY_SKIPPED.into()));
    }

    #[test]
    fn shift_b_opens_the_bisect_menu_and_a_first_mark_starts_a_bisect() {
        let mut state = ready_to_bisect();
        assert_eq!(shift(&mut state, 'B'), vec![]);
        assert_eq!(
            state.popups,
            [Popup::Menu {
                id: MenuId::Bisect,
                cursor: 0
            }]
        );
        let (_, op) = op_of(&press(&mut state, KeyCode::Enter));
        assert_eq!(op, bisect_mark(BisectMark::Bad, "c0", true));
        assert_eq!(state.popups, []);
    }

    #[test]
    fn during_a_bisect_marks_do_not_start_another() {
        let mut state = testing_c1(&[Operation::Bisect]);
        let (_, op) = op_of(&press_char(&mut state, 'g'));
        assert_eq!(op, bisect_mark(BisectMark::Good, "c1", false));
        state.free_op();
        let (_, op) = op_of(&press_char(&mut state, 'b'));
        assert_eq!(op, bisect_mark(BisectMark::Bad, "c1", false));
        state.free_op();
        let (_, op) = op_of(&press_char(&mut state, 's'));
        assert_eq!(op, bisect_mark(BisectMark::Skip, "c1", false));
        state.free_op();
        let (_, op) = op_of(&state.update(AppIntent::Commits(CommitsIntent::Bisect(
            BisectIntent::MarkSelected(BisectMark::Good),
        ))));
        assert_eq!(op, bisect_mark(BisectMark::Good, "c0", false));
    }

    #[test]
    fn a_start_is_refused_with_changes_no_commits_or_an_operation() {
        let changed = |index, worktree| FileEntry {
            path: "a.txt".into(),
            orig_path: None,
            index,
            worktree,
            kind: EntryKind::Changed,
        };
        let cases = [
            (vec![changed('M', '.')], on_branch("main"), bisect::DIRTY),
            (vec![changed('.', 'M')], on_branch("main"), bisect::DIRTY),
            (
                vec![],
                bisect_branch(Head::Unborn("main".into()), &[], None),
                "No commits yet",
            ),
            (
                vec![],
                bisect_branch(Head::Branch("main".into()), &[Operation::Merge], None),
                "Finish or abort the operation in progress",
            ),
        ];
        for (files, branch, reason) in cases {
            let mut state = ready_to_bisect();
            state.files.rows = files;
            state.branch = Some(branch);
            assert_eq!(
                state.update(AppIntent::Commits(CommitsIntent::Bisect(
                    BisectIntent::MarkSelected(BisectMark::Bad)
                ))),
                vec![]
            );
            assert_eq!(state.notice, Some(reason.into()));
            assert_eq!(state.held_seq(), None);
        }
        let mut untracked_only = ready_to_bisect();
        untracked_only.files.rows = vec![untracked("new.txt")];
        op_of(
            &untracked_only.update(AppIntent::Commits(CommitsIntent::Bisect(
                BisectIntent::MarkSelected(BisectMark::Good),
            ))),
        );
    }

    #[test]
    fn a_bisect_with_its_own_terms_is_not_marked() {
        let mut state = testing_c1(&[Operation::Bisect]);
        if let Some(bisect) = state.branch.as_mut().and_then(|b| b.bisect.as_mut()) {
            bisect.terms = kika_git::bisect::Terms {
                bad: "new".into(),
                good: "old".into(),
            };
        }
        for c in ['g', 'b', 's'] {
            state.notice = None;
            assert_eq!(press_char(&mut state, c), vec![], "{c}");
            assert_eq!(state.notice, Some(bisect::CUSTOM_TERMS.into()), "{c}");
        }
        assert_eq!(
            state.update(AppIntent::Commits(CommitsIntent::Bisect(
                BisectIntent::MarkSelected(BisectMark::Bad)
            ))),
            vec![]
        );
        assert_eq!(state.held_seq(), None);
        press_char(&mut state, 'a');
        assert_eq!(
            state.popups,
            [Popup::Question(state.bisect_reset_question())]
        );
    }

    #[test]
    fn the_status_panel_marks_only_once_git_checks_out_a_commit() {
        let mut state = testing_c1(&[Operation::Bisect]);
        state.branch.as_mut().unwrap().head = Head::Branch("main".into());
        assert_eq!(press_char(&mut state, 'g'), vec![]);
        assert_eq!(state.notice, Some(bisect::NOT_TESTING.into()));
    }

    #[test]
    fn the_bisect_menu_reset_asks_too() {
        let mut state = testing_c1(&[Operation::Bisect]);
        state.update(AppIntent::OpenMenu(MenuId::Bisect));
        for _ in 0..3 {
            state.update(AppIntent::MenuNext);
        }
        assert_eq!(state.update(AppIntent::MenuSelect), vec![]);
        assert_eq!(
            state.popups,
            [Popup::Question(state.bisect_reset_question())]
        );

        let mut idle = ready_to_bisect();
        idle.update(AppIntent::OpenMenu(MenuId::Bisect));
        for _ in 0..3 {
            idle.update(AppIntent::MenuNext);
        }
        idle.update(AppIntent::MenuSelect);
        assert_eq!(idle.notice, Some(bisect::NO_BISECT.into()));
        assert_eq!(
            idle.update(AppIntent::Commits(CommitsIntent::Bisect(
                BisectIntent::ConfirmReset
            ))),
            vec![]
        );
    }
}
