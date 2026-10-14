//! Checking out the open pull request's head as the local branch
//! `pr/<number>` from its detail, then going to the workspace to build,
//! run or edit it. `b` plans from local git only; a question that `y`
//! alone answers says what will happen; only after `y`, and only when the
//! head is missing, is it fetched by id from the repository's own fetch
//! remote. A branch that has diverged from the head, or that another
//! worktree has checked out, is never moved.

use super::files::{NO_FETCH_REMOTE, STILL_MISSING};
use super::git::{BranchAt, CheckoutPlan, pr_branch};
use super::merge::merge_disabled;
use super::state::NO_FORGE;
use crate::confirm_question::{Answer, ConfirmQuestion, Then};
use crate::effect::{Effect, Tag};
use crate::keymap::{KeyScope, OP_RUNNING, Reason, in_progress_disabled};
use crate::model::AppState;
use crate::op::GitOp;
use crate::popup::Popup;
use kika_git::branches::BranchName;
use kika_git::commit_files::CommitId;
use kika_git::repo_state::{Operation, resumable};

const NO_HEAD: &str = "GitHub gave no head commit for this pull request";
const PLANNING: &str = "The checkout is already being prepared";
const IN_PROGRESS: &str = "Finish or abort the operation in progress";

/// Where a checkout is on its way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckoutStage {
    /// Local git is read; after a fetch, read again.
    Planning,
    /// The question is open.
    Asking,
    /// The head is being fetched, after `y`.
    Fetching,
}

/// A checkout of pull request `number`'s `head` as `branch`, from `b`
/// until its op starts or it is given up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrCheckout {
    pub number: u64,
    pub head: CommitId,
    /// Always `pr/<number>`.
    pub branch: BranchName,
    /// The plan the question was asked from, once there is one; a plan
    /// read after the fetch must match it.
    pub plan: Option<CheckoutPlan>,
    pub stage: CheckoutStage,
    /// The load or fetch in flight; any other answer is dropped.
    pub seq: u64,
    /// The repository it was asked in.
    pub generation: u64,
}

/// Why `b` cannot check out the open pull request: no loaded detail, no
/// head commit, no known repository, git busy, or an operation in
/// progress.
pub(crate) fn pr_checkout_disabled(state: &AppState) -> Option<Reason> {
    if let Some(reason) = merge_disabled(state) {
        return Some(reason);
    }
    if state.review.detail.as_ref()?.head_oid.is_none() {
        return Some(NO_HEAD.into());
    }
    if state.review_repo().is_none() {
        return Some(NO_FORGE.into());
    }
    if state.op_is_running() {
        return Some(OP_RUNNING.into());
    }
    // A question closed with n or Esc leaves nothing in flight.
    let in_flight = state
        .review
        .checkout
        .as_ref()
        .is_some_and(|c| c.generation == state.generation && c.stage != CheckoutStage::Asking);
    if in_flight {
        return Some(PLANNING.into());
    }
    in_progress_disabled(state)
}

/// The first 7 digits of `id`, as the question shows it.
fn short(id: &CommitId) -> &str {
    let id = id.as_str();
    &id[..id.len().min(7)]
}

/// The op a plan allows, or why there is none.
fn op_for(number: u64, head: &CommitId, plan: &CheckoutPlan) -> Result<GitOp, Reason> {
    let branch = pr_branch(number);
    let name = branch.display();
    if !plan.in_progress.is_empty() {
        let only_bisect =
            resumable(&plan.in_progress).is_none() && plan.in_progress.contains(&Operation::Bisect);
        return Err(if only_bisect {
            crate::bisect::RESET_FIRST.into()
        } else {
            IN_PROGRESS.into()
        });
    }
    let old = match &plan.at {
        BranchAt::Absent => None,
        BranchAt::Same if plan.current => {
            return Err(format!("Already on {name} at {}", short(head)).into());
        }
        BranchAt::Same => Some(head.clone()),
        BranchAt::FastForward(old) => Some(old.clone()),
        BranchAt::Diverged => {
            return Err(format!(
                "{name} has commits not in the pull request; rename or delete it first"
            )
            .into());
        }
        BranchAt::OtherWorktree => {
            return Err(format!("{name} is checked out in another worktree").into());
        }
    };
    if !plan.head_here && plan.remote.is_none() {
        return Err(NO_FETCH_REMOTE.into());
    }
    Ok(GitOp::CheckoutPrBranch {
        number,
        branch,
        head: head.clone(),
        old,
        current: plan.current,
    })
}

/// "Check out pull request #N?", with how to answer first, so a short
/// screen cuts the details rather than the answer.
fn question(number: u64, head: &CommitId, plan: &CheckoutPlan, op: GitOp) -> ConfirmQuestion {
    let name = pr_branch(number).display();
    let what = match &plan.at {
        BranchAt::FastForward(old) if plan.current => {
            format!(
                "Fast-forward {name} from {} to {}?",
                short(old),
                short(head)
            )
        }
        BranchAt::FastForward(old) => format!(
            "Fast-forward {name} from {} to {} and switch to it?",
            short(old),
            short(head)
        ),
        BranchAt::Same => format!("Switch to {name} (already at {})?", short(head)),
        _ => format!(
            "Check out pull request #{number} as new branch {name} at {}?",
            short(head)
        ),
    };
    let fetch = match (&plan.remote, plan.head_here) {
        (Some(remote), false) => format!(" Fetches it from {} first.", remote.display()),
        _ => String::new(),
    };
    ConfirmQuestion {
        scope: KeyScope::CheckoutPullRequest,
        title: "Check out pull request".into(),
        text: format!("Press y to check it out, n or Esc to cancel\n{what}{fetch}"),
        answer: Answer::YOnly,
        then: Then::Op(op),
        range: false,
        head: None,
    }
}

impl AppState {
    /// `b`: read from local git what checking out the open pull request
    /// would do. Nothing is fetched, and nothing changes, before `y`.
    pub(crate) fn open_pr_checkout(&mut self) -> Vec<Effect> {
        if let Some(reason) = pr_checkout_disabled(self) {
            self.notice = Some(reason);
            return vec![];
        }
        let (Some(detail), Some(repo)) = (&self.review.detail, self.review_repo()) else {
            return vec![];
        };
        let (number, Some(head)) = (detail.number, detail.head_oid.clone()) else {
            return vec![];
        };
        let seq = self.take_seq();
        self.review.checkout = Some(PrCheckout {
            number,
            head,
            branch: pr_branch(number),
            plan: None,
            stage: CheckoutStage::Planning,
            seq,
            generation: self.generation,
        });
        self.plan_pr_checkout(seq, repo)
    }

    fn plan_pr_checkout(&mut self, seq: u64, repo: kika_forge::ForgeRepo) -> Vec<Effect> {
        let tag = self.user_tag(seq);
        let Some(checkout) = &self.review.checkout else {
            return vec![];
        };
        vec![Effect::PlanPrCheckout {
            tag,
            head: checkout.head.clone(),
            branch: checkout.branch.clone(),
            hosts: self.forge.hosts.clone(),
            repo,
        }]
    }

    /// The checkout waiting at `stage` for `tag`'s answer, if this is it.
    fn checkout_for(&self, tag: Tag, stage: CheckoutStage) -> bool {
        tag.generation == self.generation
            && self
                .review
                .checkout
                .as_ref()
                .is_some_and(|c| c.seq == tag.seq && c.stage == stage)
    }

    /// The plan came: ask, or say why not. A plan read after the fetch
    /// goes on only if it matches the one the question was asked from.
    pub(crate) fn pr_checkout_planned(
        &mut self,
        tag: Tag,
        result: Result<CheckoutPlan, String>,
    ) -> Vec<Effect> {
        self.user_done(tag);
        if !self.checkout_for(tag, CheckoutStage::Planning) {
            return vec![];
        }
        let Some(checkout) = self.review.checkout.take() else {
            return vec![];
        };
        let plan = match result {
            Ok(plan) => plan,
            Err(message) => {
                self.notice = Some(message.into());
                return vec![];
            }
        };
        // After the fetch the head must be here, whatever else the plan says.
        if checkout.plan.is_some() && !plan.head_here {
            self.notice = Some(STILL_MISSING.into());
            return vec![];
        }
        let op = match op_for(checkout.number, &checkout.head, &plan) {
            Ok(op) => op,
            Err(reason) => {
                self.notice = Some(reason);
                return vec![];
            }
        };
        let Some(confirmed) = &checkout.plan else {
            self.popups.push(Popup::Question(question(
                checkout.number,
                &checkout.head,
                &plan,
                op,
            )));
            self.review.checkout = Some(PrCheckout {
                plan: Some(plan),
                stage: CheckoutStage::Asking,
                ..checkout
            });
            return vec![];
        };
        if plan.at != confirmed.at || plan.current != confirmed.current {
            let name = checkout.branch.display();
            self.notice = Some(format!("{name} changed; press b again").into());
            return vec![];
        }
        self.start_pr_checkout(op)
    }

    /// `y`: check the op is still allowed and the repository the same,
    /// then start it, or fetch the missing head first.
    pub(crate) fn confirm_pr_checkout(&mut self) -> Vec<Effect> {
        let asked = self
            .review
            .checkout
            .take_if(|c| c.stage == CheckoutStage::Asking);
        let Some(Then::Op(op)) = self
            .take_question(KeyScope::CheckoutPullRequest, Answer::YOnly)
            .map(|q| q.then)
        else {
            return vec![];
        };
        let Some(checkout) = asked.filter(|c| c.generation == self.generation) else {
            return vec![];
        };
        let Some(plan) = &checkout.plan else {
            return vec![];
        };
        if plan.head_here {
            return self.start_pr_checkout(op);
        }
        let Some(remote) = plan.remote.clone() else {
            return vec![];
        };
        // A user effect holding the git slot, like the files screen's fetch.
        let Some(tag) = self.take_op() else {
            self.notice = Some(OP_RUNNING.into());
            return vec![];
        };
        self.notice = Some(
            format!(
                "Fetching #{}'s head from {}…",
                checkout.number,
                remote.display()
            )
            .into(),
        );
        let head = checkout.head.clone();
        self.review.checkout = Some(PrCheckout {
            stage: CheckoutStage::Fetching,
            seq: tag.seq,
            ..checkout
        });
        vec![Effect::FetchReviewCommits {
            tag,
            remote,
            oids: vec![head],
        }]
    }

    /// Whether `tag` is the checkout's fetch, which `review_head_fetched`
    /// hands here.
    pub(super) fn checkout_fetch(&self, tag: Tag) -> bool {
        self.review
            .checkout
            .as_ref()
            .is_some_and(|c| c.seq == tag.seq && c.stage == CheckoutStage::Fetching)
    }

    /// The fetch ended: plan again, or stay in review and say why not.
    pub(crate) fn pr_checkout_fetched(
        &mut self,
        tag: Tag,
        result: Result<(), String>,
    ) -> Vec<Effect> {
        let held = self.holds_op(tag);
        self.release_op(tag);
        if held {
            // Git has gone; a question still open has nobody to answer.
            self.close_credential();
        }
        if !self.checkout_for(tag, CheckoutStage::Fetching) {
            return vec![];
        }
        if let Err(message) = result {
            self.review.checkout = None;
            self.notice = Some(message.into());
            return vec![];
        }
        let Some(repo) = self.review_repo() else {
            self.review.checkout = None;
            self.notice = Some(NO_FORGE.into());
            return vec![];
        };
        let seq = self.take_seq();
        if let Some(checkout) = &mut self.review.checkout {
            checkout.stage = CheckoutStage::Planning;
            checkout.seq = seq;
        }
        self.plan_pr_checkout(seq, repo)
    }

    /// Go to the workspace and start `op` there, so its result shows in
    /// the workspace's bar like any checkout's.
    fn start_pr_checkout(&mut self, op: GitOp) -> Vec<Effect> {
        let mut effects = self.leave_review();
        effects.extend(self.run_op_where_it_belongs(op));
        effects
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;

    use super::*;
    use crate::features::review::ReviewIntent;
    use crate::features::review::state::tests::{fixture_detail, opened_on, press, repo};
    use crate::intent::AppIntent;
    use crate::model::Context;
    use crate::test_support::{start_bisecting, start_merging};
    use kika_forge::review::PrDetail;
    use kika_git::remotes::RemoteName;

    const HEAD: &str = "1111111111111111111111111111111111111111";
    const OLD: &str = "2222222222222222222222222222222222222222";

    fn id(hex: &str) -> CommitId {
        CommitId::parse(hex).unwrap()
    }

    fn on_detail(change: impl FnOnce(&mut PrDetail)) -> AppState {
        let mut detail = fixture_detail();
        detail.number = 12;
        change(&mut detail);
        let state = opened_on(detail);
        assert!(matches!(
            state.review.screen,
            crate::features::review::ReviewScreen::Detail(12)
        ));
        state
    }

    fn here(at: BranchAt, current: bool) -> CheckoutPlan {
        CheckoutPlan {
            head_here: true,
            remote: None,
            at,
            current,
            in_progress: vec![],
        }
    }

    fn missing(at: BranchAt, remote: Option<&str>) -> CheckoutPlan {
        CheckoutPlan {
            head_here: false,
            remote: remote.map(RemoteName::from),
            ..here(at, false)
        }
    }

    /// `b`, and the one plan it asks for.
    fn press_b(state: &mut AppState) -> Tag {
        let effects = press(state, KeyCode::Char('b'));
        match effects.as_slice() {
            [
                Effect::PlanPrCheckout {
                    tag,
                    head,
                    branch,
                    hosts,
                    repo: to,
                },
            ] => {
                assert_eq!(head.as_str(), HEAD);
                assert_eq!(*branch, BranchName::from("pr/12"));
                assert_eq!(hosts, &state.forge.hosts);
                assert_eq!(*to, repo());
                *tag
            }
            other => panic!("expected one plan, got {other:?}"),
        }
    }

    fn planned(
        state: &mut AppState,
        tag: Tag,
        result: Result<CheckoutPlan, String>,
    ) -> Vec<Effect> {
        state.update(AppIntent::Review(ReviewIntent::CheckoutPlanned {
            tag,
            result,
        }))
    }

    /// `b` answered with `plan`.
    fn asked(plan: CheckoutPlan) -> AppState {
        let mut state = on_detail(|_| {});
        let tag = press_b(&mut state);
        assert_eq!(planned(&mut state, tag, Ok(plan)), []);
        state
    }

    fn question_text(state: &AppState) -> &str {
        match state.popups.last() {
            Some(Popup::Question(q)) if q.scope == KeyScope::CheckoutPullRequest => {
                assert_eq!(q.answer, Answer::YOnly);
                &q.text
            }
            other => panic!("expected the checkout question, got {other:?}"),
        }
    }

    fn no_git(effects: &[Effect]) {
        assert!(
            !effects.iter().any(|e| matches!(
                e,
                Effect::RunOp { .. } | Effect::HandOff(_) | Effect::FetchReviewCommits { .. }
            )),
            "{effects:?}"
        );
    }

    #[test]
    fn b_on_a_loaded_detail_only_plans() {
        let mut state = on_detail(|_| {});
        let busy = state.busy_count();
        let tag = press_b(&mut state);
        assert!(tag.user);
        assert_eq!(state.busy_count(), busy + 1);
        let checkout = state.review.checkout.as_ref().unwrap();
        assert_eq!(checkout.stage, CheckoutStage::Planning);
        assert!(state.popups.is_empty());
        // A second `b` waits for the first.
        assert_eq!(press(&mut state, KeyCode::Char('b')), []);
        assert_eq!(state.notice.as_deref(), Some(PLANNING));
        planned(&mut state, tag, Ok(here(BranchAt::Absent, false)));
        assert_eq!(state.busy_count(), busy);
    }

    /// One change to the state before `b`.
    type Change = fn(&mut AppState);

    #[test]
    fn b_refuses_without_a_head_a_repository_a_free_slot_or_with_an_operation() {
        let cases: [(Change, &str); 5] = [
            (
                |s| {
                    if let Some(detail) = s.review.detail.as_mut() {
                        detail.head_oid = None;
                    }
                },
                NO_HEAD,
            ),
            (
                |s| {
                    s.forge.repo = None;
                    s.review.repo = None;
                },
                NO_FORGE,
            ),
            (
                |s| {
                    s.take_op();
                },
                OP_RUNNING,
            ),
            (start_merging, IN_PROGRESS),
            (start_bisecting, crate::bisect::RESET_FIRST),
        ];
        for (change, notice) in cases {
            let mut state = on_detail(|_| {});
            change(&mut state);
            assert_eq!(press(&mut state, KeyCode::Char('b')), [], "{notice}");
            assert_eq!(state.notice.as_deref(), Some(notice));
            assert!(state.review.checkout.is_none());
        }
        // Off a loaded detail: the queue, or the detail still loading.
        let mut state = crate::features::review::state::tests::in_queues();
        assert_eq!(press(&mut state, KeyCode::Char('b')), []);
        assert_eq!(state.notice.as_deref(), Some("Open a pull request first"));
        press(&mut state, KeyCode::Enter);
        assert_eq!(press(&mut state, KeyCode::Char('b')), []);
        assert!(state.review.checkout.is_none());
    }

    #[test]
    fn each_plan_asks_its_own_question_or_says_why_not() {
        let asks = [
            (
                here(BranchAt::Absent, false),
                "Check out pull request #12 as new branch pr/12 at 1111111?",
            ),
            (
                here(BranchAt::FastForward(id(OLD)), false),
                "Fast-forward pr/12 from 2222222 to 1111111 and switch to it?",
            ),
            (
                here(BranchAt::FastForward(id(OLD)), true),
                "Fast-forward pr/12 from 2222222 to 1111111?",
            ),
            (
                here(BranchAt::Same, false),
                "Switch to pr/12 (already at 1111111)?",
            ),
            (
                missing(BranchAt::Absent, Some("origin")),
                "Check out pull request #12 as new branch pr/12 at 1111111? Fetches it from origin first.",
            ),
        ];
        for (plan, words) in asks {
            let state = asked(plan.clone());
            let text = question_text(&state);
            assert!(
                text.starts_with("Press y to check it out, n or Esc to cancel\n"),
                "{text}"
            );
            assert!(text.ends_with(words), "{text:?} for {plan:?}");
            let checkout = state.review.checkout.as_ref().unwrap();
            assert_eq!(checkout.stage, CheckoutStage::Asking);
            assert_eq!(checkout.plan.as_ref(), Some(&plan));
        }
        let refusals = [
            (
                here(BranchAt::Diverged, false),
                "pr/12 has commits not in the pull request; rename or delete it first",
            ),
            (
                here(BranchAt::OtherWorktree, false),
                "pr/12 is checked out in another worktree",
            ),
            (missing(BranchAt::Absent, None), NO_FETCH_REMOTE),
            (here(BranchAt::Same, true), "Already on pr/12 at 1111111"),
            (
                CheckoutPlan {
                    in_progress: vec![Operation::Rebase],
                    ..here(BranchAt::Absent, false)
                },
                IN_PROGRESS,
            ),
        ];
        for (plan, notice) in refusals {
            let state = asked(plan);
            assert!(state.popups.is_empty(), "{notice}");
            assert_eq!(state.notice.as_deref(), Some(notice));
            assert!(state.review.checkout.is_none());
        }
        // A failed read says why, made safe for the bar.
        let mut state = on_detail(|_| {});
        let tag = press_b(&mut state);
        planned(&mut state, tag, Err("fatal: bad\u{1b}[31m".into()));
        let notice = state.notice.as_deref().unwrap();
        assert!(
            notice.starts_with("fatal: bad") && !notice.contains('\u{1b}'),
            "{notice:?}"
        );
        assert!(state.popups.is_empty());
    }

    #[test]
    fn a_stale_plan_asks_nothing() {
        let mut state = on_detail(|_| {});
        let tag = press_b(&mut state);
        let other = Tag {
            generation: tag.generation + 1,
            ..tag
        };
        planned(&mut state, other, Ok(here(BranchAt::Absent, false)));
        let later = Tag {
            seq: tag.seq + 1,
            ..tag
        };
        planned(&mut state, later, Ok(here(BranchAt::Absent, false)));
        assert!(state.popups.is_empty());
        planned(&mut state, tag, Ok(here(BranchAt::Absent, false)));
        question_text(&state);
    }

    #[test]
    fn y_with_the_head_here_starts_the_planned_op_in_the_workspace() {
        let cases = [
            (here(BranchAt::Absent, false), None),
            (here(BranchAt::Same, false), Some(HEAD)),
            (here(BranchAt::FastForward(id(OLD)), false), Some(OLD)),
            (here(BranchAt::FastForward(id(OLD)), true), Some(OLD)),
        ];
        for (plan, old) in cases {
            let mut state = asked(plan.clone());
            let effects = press(&mut state, KeyCode::Char('y'));
            let runs: Vec<&GitOp> = effects
                .iter()
                .filter_map(|e| match e {
                    Effect::RunOp { op, .. } => Some(op),
                    _ => None,
                })
                .collect();
            assert_eq!(
                runs,
                [&GitOp::CheckoutPrBranch {
                    number: 12,
                    branch: "pr/12".into(),
                    head: id(HEAD),
                    old: old.map(id),
                    current: plan.current,
                }],
                "{plan:?}"
            );
            assert_eq!(state.context, Context::Workspace);
            assert!(state.op_is_running());
            assert!(state.popups.is_empty());
            assert!(state.review.checkout.is_none());
        }
    }

    #[test]
    fn y_does_nothing_in_another_repository_or_once_an_operation_started() {
        let changes: [fn(&mut AppState); 3] =
            [|s| s.generation += 1, start_bisecting, start_merging];
        for change in changes {
            let mut state = asked(here(BranchAt::Absent, false));
            change(&mut state);
            let effects = press(&mut state, KeyCode::Char('y'));
            assert_eq!(effects, []);
            assert!(matches!(state.context, Context::Review { .. }));
            assert!(!state.op_is_running());
            assert!(state.review.checkout.is_none());
        }
    }

    #[test]
    fn n_esc_and_enter_check_out_nothing() {
        for code in [KeyCode::Char('n'), KeyCode::Esc] {
            let mut state = asked(here(BranchAt::Absent, false));
            assert_eq!(press(&mut state, code), []);
            assert!(state.popups.is_empty());
            assert!(matches!(state.context, Context::Review { .. }));
            // `b` asks again.
            press_b(&mut state);
        }
        let mut state = asked(here(BranchAt::Absent, false));
        no_git(&press(&mut state, KeyCode::Enter));
        question_text(&state);
    }

    /// `y` on a missing head: the one fetch it starts.
    fn fetch_after_y(state: &mut AppState) -> Tag {
        let effects = press(state, KeyCode::Char('y'));
        match effects.as_slice() {
            [Effect::FetchReviewCommits { tag, remote, oids }] => {
                assert_eq!(*remote, RemoteName::from("origin"));
                assert_eq!(oids, &[id(HEAD)]);
                assert!(tag.user);
                *tag
            }
            other => panic!("expected one fetch, got {other:?}"),
        }
    }

    fn fetched(state: &mut AppState, tag: Tag, result: Result<(), String>) -> Vec<Effect> {
        state.update(AppIntent::Review(ReviewIntent::HeadFetched { tag, result }))
    }

    fn replan(effects: &[Effect]) -> Tag {
        match effects {
            [Effect::PlanPrCheckout { tag, .. }] => *tag,
            other => panic!("expected the plan again, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_head_is_fetched_only_after_y_then_planned_again() {
        let ff = BranchAt::FastForward(id(OLD));
        let mut state = asked(missing(ff.clone(), Some("origin")));
        let tag = fetch_after_y(&mut state);
        assert!(state.op_is_running());
        assert!(matches!(state.context, Context::Review { .. }));
        let tag = replan(&fetched(&mut state, tag, Ok(())));
        assert!(!state.op_is_running());
        let effects = planned(&mut state, tag, Ok(here(ff.clone(), false)));
        let (_, op) = crate::test_support::op_of(
            &effects
                .into_iter()
                .filter(|e| matches!(e, Effect::RunOp { .. }))
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            op,
            GitOp::CheckoutPrBranch {
                number: 12,
                branch: "pr/12".into(),
                head: id(HEAD),
                old: Some(id(OLD)),
                current: false,
            }
        );
        assert_eq!(state.context, Context::Workspace);
    }

    #[test]
    fn a_plan_that_changed_over_the_fetch_starts_nothing() {
        for (after, notice) in [
            (
                here(BranchAt::Diverged, false),
                "pr/12 has commits not in the pull request; rename or delete it first",
            ),
            (
                here(BranchAt::FastForward(id(OLD)), false),
                "pr/12 changed; press b again",
            ),
            (here(BranchAt::Absent, true), "pr/12 changed; press b again"),
            (missing(BranchAt::Absent, Some("origin")), STILL_MISSING),
            (missing(BranchAt::Absent, None), STILL_MISSING),
        ] {
            let mut state = asked(missing(BranchAt::Absent, Some("origin")));
            let tag = fetch_after_y(&mut state);
            let tag = replan(&fetched(&mut state, tag, Ok(())));
            no_git(&planned(&mut state, tag, Ok(after)));
            assert_eq!(state.notice.as_deref(), Some(notice));
            assert!(matches!(state.context, Context::Review { .. }));
            assert!(state.review.checkout.is_none());
        }
    }

    #[test]
    fn a_failed_fetch_stays_in_review_with_a_safe_notice() {
        let mut state = asked(missing(BranchAt::Absent, Some("origin")));
        let busy = state.busy_count();
        let tag = fetch_after_y(&mut state);
        let effects = fetched(&mut state, tag, Err("fatal: no\u{7}pe".into()));
        assert_eq!(effects, []);
        assert!(!state.op_is_running());
        assert_eq!(state.busy_count(), busy);
        assert!(matches!(state.context, Context::Review { .. }));
        let notice = state.notice.as_deref().unwrap();
        assert!(
            notice.starts_with("fatal: no") && !notice.contains('\u{7}'),
            "{notice:?}"
        );
        assert!(state.review.checkout.is_none());
    }

    #[test]
    fn the_question_draws_its_text_in_a_popup() {
        let state = asked(missing(BranchAt::Absent, Some("origin")));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        terminal
            .draw(|frame| crate::view::render(&state, frame))
            .unwrap();
        let rows = crate::view::buffer_rows(terminal.backend().buffer());
        let screen = rows.join("\n");
        for words in [
            "Check out pull request",
            "Press y to check it out, n or Esc to cancel",
            "new branch pr/12 at 1111111?",
            "Fetches it from origin first.",
        ] {
            assert!(screen.contains(words), "{words:?} in {rows:#?}");
        }
    }

    fn draw(state: &AppState, width: u16, height: u16) -> Vec<String> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| crate::view::render(state, frame))
            .unwrap();
        crate::view::buffer_rows(terminal.backend().buffer())
    }

    #[test]
    fn the_longest_question_keeps_its_answer_line_at_80_by_24() {
        let state = asked(missing(BranchAt::FastForward(id(OLD)), Some("origin")));
        assert!(question_text(&state).ends_with(
            "Fast-forward pr/12 from 2222222 to 1111111 and switch to it? Fetches it from origin first."
        ));
        let rows = draw(&state, 80, 24);
        let screen = rows.join("\n");
        assert!(
            screen.contains("Press y to check it out, n or Esc to cancel"),
            "{rows:#?}"
        );
        assert!(screen.contains("Fast-forward pr/12"), "{rows:#?}");
        for row in &rows {
            assert!(crate::view::width_of(row) <= 80, "{row:?}");
        }
    }

    #[test]
    fn the_detail_bar_offers_b_checkout() {
        let state = on_detail(|_| {});
        let rows = draw(&state, 120, 24);
        let bar = rows.last().unwrap();
        assert!(bar.contains("M merge  b checkout"), "{bar:?}");
        // At 80 columns the bar elides its middle and keeps help and quit.
        let rows = draw(&state, 80, 24);
        let bar = rows.last().unwrap();
        assert!(crate::view::width_of(bar) <= 80, "{bar:?}");
        assert!(bar.contains("? keys  q quit"), "{bar:?}");
        // While refused, as on the queue, the hint stays and says why on a press.
        let mut queue = crate::features::review::state::tests::in_queues();
        assert!(draw(&queue, 120, 24).last().unwrap().contains("b checkout"));
        press(&mut queue, KeyCode::Char('b'));
        assert_eq!(queue.notice.as_deref(), Some("Open a pull request first"));
    }
}
