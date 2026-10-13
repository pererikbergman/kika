//! Squash, fixup and drop over a range of the Commits log, in one
//! interactive rebase. Dropping several commits asks first, and the answer
//! drops only the commits it asked about.

use super::{range_log_plan, range_rewrite_disabled};
use crate::confirm_question::{Answer, ConfirmQuestion, Then};
use crate::effect::Effect;
use crate::features::commits::CommitsIntent;
use crate::features::commits::rewrite::RewriteIntent;
use crate::git::commit_files::CommitId;
use crate::git::rebase_plan::{Edit, RebasePlan, TodoAction};
use crate::intent::AppIntent;
use crate::keymap::{self, KeyScope, Reason};
use crate::model::{AppState, Context};
use crate::op::GitOp;
use crate::popup::Popup;

/// Why a confirmed drop did not run: the range is not the one asked about.
pub(crate) const SELECTION_CHANGED: &str = "The selection changed; nothing was dropped";

/// "Drop these commits?", carrying the full ids of the log range when
/// asked, newest first. It throws away several commits' changes, so only
/// `y` answers, and the answer drops only if the range still holds them.
/// Only the count is shown: subjects and the filter are not drawn here.
pub(crate) fn drop_range_question(ids: Vec<CommitId>) -> ConfirmQuestion {
    ConfirmQuestion {
        scope: KeyScope::DropRangeConfirm,
        title: "Drop commits".into(),
        text: format!(
            "Drop {} commits and their changes from the history? (y/n)",
            ids.len()
        ),
        answer: Answer::YOnly,
        then: Then::Intent(Box::new(AppIntent::Commits(CommitsIntent::Rewrite(
            RewriteIntent::ConfirmDropRange(ids),
        )))),
        range: false,
        head: None,
    }
}

impl AppState {
    /// Rewrite every commit of the log range with `edit`. A fold runs at
    /// once, as a single-row one does; dropping two or more commits asks
    /// first.
    pub(crate) fn rewrite_range(&mut self, edit: Edit) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        let plan = match self.range_plan(edit) {
            Ok(plan) => plan,
            Err(reason) => {
                self.notice = Some(reason);
                return vec![];
            }
        };
        let ids = dropped(&plan);
        if edit == Edit::Drop && ids.len() > 1 {
            self.popups.push(Popup::Question(drop_range_question(ids)));
            return vec![];
        }
        self.commits.list.range_anchor = None;
        self.run_op_where_it_belongs(GitOp::InteractiveRebase(plan))
    }

    /// Drop the range, as the question asked. The range must still hold
    /// exactly `ids`, and the plan is made again from the log as it is now
    /// and runs only if it drops exactly them, so a reload or a moved
    /// cursor while asking drops nothing.
    pub(crate) fn confirm_drop_range(&mut self, ids: Vec<CommitId>) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        if self.range_ids() != ids {
            self.notice = Some(SELECTION_CHANGED.into());
            return vec![];
        }
        let plan = match self.range_plan(Edit::Drop) {
            Ok(plan) => plan,
            Err(reason) => {
                self.notice = Some(reason);
                return vec![];
            }
        };
        if dropped(&plan) != ids {
            self.notice = Some(SELECTION_CHANGED.into());
            return vec![];
        }
        self.commits.list.range_anchor = None;
        self.run_op_where_it_belongs(GitOp::InteractiveRebase(plan))
    }

    /// The commits the range covers now, newest first; empty without one.
    fn range_ids(&self) -> Vec<CommitId> {
        let rows = &self.commits.list.rows;
        let ids = self
            .commits
            .list
            .range()
            .into_iter()
            .map(|i| CommitId::parse(&rows[i].oid));
        ids.collect::<Option<_>>().unwrap_or_default()
    }

    /// The plan for `edit` over the range, once nothing stops it.
    fn range_plan(&self, edit: Edit) -> Result<RebasePlan, Reason> {
        match keymap::list_guard(self, true).or_else(|| range_rewrite_disabled(self, edit)) {
            Some(reason) => Err(reason),
            None => range_log_plan(self, edit),
        }
    }
}

/// The full ids `plan` drops, newest first, as the log shows them.
fn dropped(plan: &RebasePlan) -> Vec<CommitId> {
    plan.steps
        .iter()
        .rev()
        .filter(|step| step.action == TodoAction::Drop)
        .map(|step| step.commit.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyModifiers};

    use super::*;
    use crate::git::log::CommitRow;
    use crate::git::rebase_plan::{RANGE_GAP, Step};
    use crate::handoff::HandOff;
    use crate::intent::AppIntent;
    use crate::keymap::{KeyPress, KeyScope, OP_RUNNING};
    use crate::layout::SidePanel;
    use crate::model::Focus;
    use crate::op::op_command;

    fn press(state: &mut AppState, code: KeyCode) -> Vec<Effect> {
        state.update(AppIntent::Key(KeyPress::new(code, KeyModifiers::NONE)))
    }

    /// Each char pressed in turn; every effect in order.
    fn keys(state: &mut AppState, text: &str) -> Vec<Effect> {
        text.chars()
            .flat_map(|c| press(state, KeyCode::Char(c)))
            .collect()
    }

    /// The full id of row `i`.
    fn oid(i: usize) -> String {
        format!("{i:040x}")
    }

    fn id(i: usize) -> CommitId {
        CommitId::parse(&oid(i)).unwrap()
    }

    fn row(i: usize) -> CommitRow {
        CommitRow {
            oid: oid(i),
            short_oid: oid(i)[33..].to_string(),
            parents: vec![oid(i + 1)],
            author: "Ann".into(),
            time: 0,
            subject: format!("commit {i}"),
            subject_altered: false,
        }
    }

    /// Rows 0..5 on one line, all loaded, the cursor on row `at` of the
    /// focused Commits panel. No other branch and an unknown git, so no
    /// `--update-refs`.
    fn on_log(at: usize) -> AppState {
        let mut state = AppState::new(Context::Workspace);
        state.focus = Focus::Commits;
        state.expanded = SidePanel::Commits;
        state.commits.list.rows = (0..5).map(row).collect();
        state.commits.list.loaded = true;
        state.commits.done = true;
        state.commits.tips = Some(vec![]);
        state.commits.list.selected = at;
        assert_eq!(state.key_focus(), KeyScope::Commits);
        state
    }

    /// The one rebase `effects` start, handed to the terminal.
    fn rebase_of(effects: &[Effect]) -> RebasePlan {
        match effects {
            [
                Effect::HandOff(HandOff::Attached {
                    op: GitOp::InteractiveRebase(plan),
                    ..
                }),
            ] => plan.clone(),
            other => panic!("expected one interactive rebase, got {other:?}"),
        }
    }

    fn step(action: TodoAction, i: usize) -> Step {
        Step {
            action,
            commit: id(i),
            break_after: false,
        }
    }

    #[test]
    fn s_and_f_fold_the_range_into_the_commit_below_in_one_rebase() {
        use TodoAction::Pick;
        for (key, action) in [("s", TodoAction::Squash), ("F", TodoAction::Fixup)] {
            let mut state = on_log(1);
            assert_eq!(keys(&mut state, "Vj"), vec![]);
            assert_eq!(state.commits.list.range(), [1, 2]);
            let plan = rebase_of(&keys(&mut state, key));
            assert_eq!(plan.base, id(4), "{key}");
            assert_eq!(
                plan.steps,
                [
                    step(Pick, 3),
                    step(action, 2),
                    step(action, 1),
                    step(Pick, 0)
                ],
                "{key}"
            );
            assert_eq!(state.commits.list.range_anchor, None, "{key}");
            assert!(state.op_is_running(), "{key}");
            assert_eq!(state.notice, None, "{key}");
        }
    }

    #[test]
    fn a_one_row_range_rewrites_as_the_key_does_without_one() {
        for key in ["s", "F", "d"] {
            let single = rebase_of(&keys(&mut on_log(1), key));
            let mut state = on_log(1);
            keys(&mut state, "V");
            assert_eq!(rebase_of(&keys(&mut state, key)), single, "{key}");
            assert_eq!(state.popups, [], "{key}");
            assert_eq!(state.commits.list.range_anchor, None, "{key}");
        }
    }

    /// `on_log(0)` with rows 0..=2 selected and `d` pressed.
    fn asked_to_drop_three() -> AppState {
        let mut state = on_log(0);
        assert_eq!(keys(&mut state, "Vjjd"), vec![]);
        state
    }

    #[test]
    fn d_over_three_commits_asks_and_only_y_drops_them() {
        let mut state = asked_to_drop_three();
        let question = drop_range_question(vec![id(0), id(1), id(2)]);
        assert_eq!(state.popups, [Popup::Question(question.clone())]);
        assert_eq!(state.held_seq(), None);
        assert_eq!(state.key_focus(), KeyScope::DropRangeConfirm);
        assert_eq!(question.title, "Drop commits");
        assert!(question.text.contains("Drop 3 commits"));

        // Enter is too easy to press by habit: it answers nothing.
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.popups, [Popup::Question(question.clone())]);
        assert_eq!(keys(&mut state, "n"), vec![]);
        assert_eq!((state.popups.len(), state.held_seq()), (0, None));
        assert_eq!(press(&mut state, KeyCode::Char('d')), vec![]);
        assert_eq!(press(&mut state, KeyCode::Esc), vec![]);
        assert_eq!((state.popups.len(), state.held_seq()), (0, None));
        assert!(
            state.commits.list.range_anchor.is_some(),
            "kept for another try"
        );

        keys(&mut state, "d");
        let plan = rebase_of(&keys(&mut state, "y"));
        assert_eq!(plan.base, id(3));
        let drop = TodoAction::Drop;
        assert_eq!(plan.steps, [step(drop, 2), step(drop, 1), step(drop, 0)]);
        assert_eq!(state.popups, []);
        assert_eq!(state.commits.list.range_anchor, None);
    }

    #[test]
    fn a_range_that_changed_while_asking_drops_nothing() {
        // A new commit came in on top: the range now covers other rows.
        let mut moved = asked_to_drop_three();
        moved.commits.list.rows.insert(0, row(9));
        moved.commits.list.rows[0].parents = vec![oid(0)];
        // The history was rewritten: none of the commits are there.
        let mut gone = asked_to_drop_three();
        gone.commits.list.rows = (10..15).map(row).collect();
        // The cursor moved under the question.
        let mut shrunk = asked_to_drop_three();
        shrunk.commits.list.selected = 1;
        for mut state in [moved, gone, shrunk] {
            assert_eq!(keys(&mut state, "y"), vec![]);
            assert_eq!(state.notice, Some(SELECTION_CHANGED.into()));
            assert_eq!(state.held_seq(), None);
            assert_eq!(state.popups, []);
        }

        // The same commits, but an op started meanwhile: still nothing.
        let mut busy = asked_to_drop_three();
        busy.hold_op(7);
        assert_eq!(keys(&mut busy, "y"), vec![]);
        assert_eq!(busy.notice, Some(OP_RUNNING.into()));
        assert_eq!(busy.held_seq(), Some(7));

        // Sent without the question, ids that are not the range do nothing.
        let mut state = on_log(0);
        keys(&mut state, "Vjj");
        assert_eq!(
            state.update(AppIntent::Commits(CommitsIntent::Rewrite(
                RewriteIntent::ConfirmDropRange(vec![id(0), id(1)])
            ))),
            vec![]
        );
        assert_eq!(state.notice, Some(SELECTION_CHANGED.into()));
        assert_eq!(state.held_seq(), None);
    }

    #[test]
    fn a_range_with_hidden_commits_inside_is_not_rewritten() {
        let mut state = on_log(0);
        // Rows 0 and 2 have a `0` in their text; row 1 does not.
        state.commits.list.filter = "0".into();
        state.commits.list.rows[1].short_oid = "zzzzzzz".into();
        state.commits.list.rows[1].subject = "hidden".into();
        state.commits.list.rows[2].short_oid = "zzzzzzz".into();
        state.commits.list.rows[2].subject = "commit 0 again".into();
        assert_eq!(state.commits.list.visible(), [0, 2, 3, 4]);
        keys(&mut state, "Vj");
        assert_eq!(state.commits.list.range(), [0, 2]);
        for key in ["s", "F", "d"] {
            let mut state = state.clone();
            assert_eq!(keys(&mut state, key), vec![], "{key}");
            assert_eq!(state.notice, Some(RANGE_GAP.into()), "{key}");
            assert_eq!((state.popups.len(), state.held_seq()), (0, None), "{key}");
            assert!(state.commits.list.range_anchor.is_some(), "{key}");
        }
    }

    #[test]
    fn the_filter_text_never_reaches_the_rebase() {
        let filter = "a\x1b[31m;rm";
        let mut state = on_log(1);
        for row in &mut state.commits.list.rows {
            row.subject = format!("x {filter} y");
        }
        state.commits.list.filter = filter.into();
        assert_eq!(state.commits.list.visible(), [0, 1, 2, 3, 4]);
        keys(&mut state, "Vj");
        let plan = rebase_of(&keys(&mut state, "s"));
        let full_hex = |id: &CommitId| {
            id.as_str().len() == 40 && id.as_str().bytes().all(|b| b.is_ascii_hexdigit())
        };
        assert!(full_hex(&plan.base));
        assert!(plan.steps.iter().all(|step| full_hex(&step.commit)));

        let cmd = op_command(&GitOp::InteractiveRebase(plan));
        let env = cmd.envs().iter().map(|(_, v)| v);
        for arg in cmd.args().iter().chain(env) {
            let arg = arg.to_str().unwrap();
            assert!(!arg.contains(filter), "{arg:?}");
            assert!(!arg.contains('\x1b') && !arg.contains(';'), "{arg:?}");
        }
    }

    #[test]
    fn reword_moves_and_the_other_rewrites_still_refuse_a_range() {
        use crate::git::rebase_plan::Direction;
        let intents = [
            AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::Edit(Edit::Reword))),
            AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::Edit(Edit::Stop))),
            AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::Edit(Edit::Pick))),
            AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::Move(Direction::Up))),
            AppIntent::Commits(CommitsIntent::Rewrite(RewriteIntent::Move(Direction::Down))),
        ];
        for intent in intents {
            let mut state = on_log(1);
            keys(&mut state, "Vj");
            assert_eq!(state.update(intent.clone()), vec![], "{intent:?}");
            assert_eq!(
                state.notice,
                Some(keymap::CLEAR_RANGE_FIRST.into()),
                "{intent:?}"
            );
            assert_eq!(state.held_seq(), None, "{intent:?}");
        }
    }
}
