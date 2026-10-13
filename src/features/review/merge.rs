//! Merging the open pull request from review mode's detail. Which ways
//! are offered, and why none is, come from the detail GitHub sent; the
//! merge reaches GitHub only after a question that `y` alone answers, and
//! only on a host the global config lists.

use super::state::{NO_FORGE, ReviewScreen};
use crate::confirm_question::{Answer, Then};
use crate::effect::{Effect, Tag};
use crate::forge::merge::{MergeMethod, merge_options};
use crate::forge::remote::allowed;
use crate::forge::review::MergeState;
use crate::forge::{
    CreateError, ForgeHost, ForgeRepo, PrBadge, PrState, REFRESH_LIMITED, same_repo,
};
use crate::keymap::{KeyScope, Reason};
use crate::model::{AppState, Context};
use crate::pick_list::{PickList, PickPurpose};
use crate::popup::{ErrorPopup, MergePrConfirm, Popup};
use crate::refresh::{RefreshPlan, Scope};

const NO_PR_SHOWN: &str = "Open a pull request first";
const MERGING: &str = "A merge is already running";

/// The merge GitHub is asked for, kept until it answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrMerging {
    pub seq: u64,
    pub number: u64,
    pub method: MergeMethod,
    pub host: ForgeHost,
    pub repo: ForgeRepo,
}

/// Why `M` cannot start a merge: only from a loaded pull request's
/// detail screen.
pub(crate) fn merge_disabled(state: &AppState) -> Option<Reason> {
    let shown = matches!(state.context, Context::Review { .. })
        && matches!(
            (state.review.screen, &state.review.detail),
            (ReviewScreen::Detail(n), Some(detail)) if detail.number == n
        );
    (!shown).then_some(NO_PR_SHOWN.into())
}

impl AppState {
    /// The host and repository a merge goes to, or why it may not: the
    /// repository the detail came from, on a configured host whose
    /// `[forge.allow]` names it and that is not rate limited now.
    fn merge_target(&self) -> Result<(ForgeHost, ForgeRepo), Reason> {
        let repo = self
            .forge
            .repo
            .clone()
            .or_else(|| self.review.repo.clone())
            .ok_or(NO_FORGE)?;
        // A repository `[forge.allow]` leaves out is never asked about.
        let host = allowed(&repo, &self.forge.hosts).ok_or(NO_FORGE)?.clone();
        self.merge_allowed(&host)?;
        Ok((host, repo))
    }

    /// `host` may be asked now: it is configured and not rate limited.
    fn merge_allowed(&self, host: &ForgeHost) -> Result<(), Reason> {
        if !self.forge.hosts.contains(host) {
            return Err(NO_FORGE.into());
        }
        let limited = self.forge.limited.get(host.as_str());
        if limited.is_some_and(|until| *until > self.now) {
            return Err(REFRESH_LIMITED.into());
        }
        Ok(())
    }

    /// `M`: say why the open pull request cannot be merged, or ask how
    /// when several ways are allowed, or ask at once when one is.
    pub(crate) fn open_merge_pr(&mut self) {
        if let Some(reason) = merge_disabled(self) {
            self.notice = Some(reason);
            return;
        }
        if self.pr_merge.is_some() {
            self.notice = Some(MERGING.into());
            return;
        }
        let Some(detail) = self.review.detail.clone() else {
            return;
        };
        let methods = match merge_options(&detail) {
            Ok(methods) => methods,
            Err(block) => {
                self.notice = Some(format!("Cannot merge: {}", block.message()).into());
                return;
            }
        };
        let (host, repo) = match self.merge_target() {
            Ok(target) => target,
            Err(reason) => {
                self.notice = Some(reason);
                return;
            }
        };
        let (Some(&first), Some(head_oid)) = (methods.first(), detail.head_oid.clone()) else {
            return;
        };
        let confirm = MergePrConfirm {
            number: detail.number,
            title: detail.title,
            base: detail.base,
            head: detail.head,
            method: first,
            head_oid,
            repo,
            host,
            unstable: detail.merge_state == MergeState::Unstable,
        };
        if methods.len() == 1 {
            self.popups.push(Popup::Question(confirm.into_question()));
            return;
        }
        let rows = methods.iter().map(|m| m.label().to_string()).collect();
        self.review.merge_pick = Some((confirm, methods));
        self.popups.push(Popup::PickList(PickList::new(
            PickPurpose::MergeMethod,
            rows,
            0,
        )));
    }

    /// The method picked from the "Merge method" list: ask about it.
    pub(crate) fn pick_merge_method(&mut self, row: usize) {
        let Some((mut confirm, methods)) = self.review.merge_pick.take() else {
            return;
        };
        if let Some(&method) = methods.get(row) {
            confirm.method = method;
            self.popups.push(Popup::Question(confirm.into_question()));
        }
    }

    /// `y`: ask GitHub to merge what the question described, unless its
    /// host has since left the config or become rate limited.
    pub(crate) fn confirm_merge_pr(&mut self) -> Vec<Effect> {
        let Some(Then::MergePr(confirm)) = self
            .take_question(KeyScope::MergePullRequest, Answer::YOnly)
            .map(|q| q.then)
        else {
            return vec![];
        };
        if self.pr_merge.is_some() {
            self.notice = Some(MERGING.into());
            return vec![];
        }
        if allowed(&confirm.repo, &self.forge.hosts) != Some(&confirm.host) {
            self.notice = Some(NO_FORGE.into());
            return vec![];
        }
        if let Err(reason) = self.merge_allowed(&confirm.host) {
            self.notice = Some(reason);
            return vec![];
        }
        let seq = self.take_seq();
        let tag = self.user_tag(seq);
        self.notice_text = Some(format!("Merging #{}…", confirm.number));
        self.pr_merge = Some(PrMerging {
            seq,
            number: confirm.number,
            method: confirm.method,
            host: confirm.host.clone(),
            repo: confirm.repo.clone(),
        });
        vec![Effect::MergePullRequest {
            tag,
            host: confirm.host,
            repo: confirm.repo,
            number: confirm.number,
            method: confirm.method,
            head_oid: confirm.head_oid,
        }]
    }

    /// GitHub answered a merge. A merged pull request shows as merged at
    /// once, leaves the open queues, and its branch's badge turns merged;
    /// the badges are then asked for again.
    pub(crate) fn pull_request_merged(
        &mut self,
        tag: Tag,
        number: u64,
        result: Result<(), CreateError>,
    ) -> Vec<Effect> {
        self.user_done(tag);
        let Some(merging) = self
            .pr_merge
            .take_if(|m| m.seq == tag.seq && m.number == number)
        else {
            return vec![];
        };
        if tag.generation != self.generation {
            return vec![];
        }
        match result {
            Ok(()) => {
                self.notice_text = Some(format!("Merged #{number} ({})", merging.method.label()));
                if let Some(detail) = self.review.detail.as_mut().filter(|d| d.number == number) {
                    detail.state = PrState::Merged;
                }
                if let Some(queues) = self.review.queues.as_mut() {
                    queues.remove(number);
                    self.review.reconcile();
                }
                self.mark_badge_merged(&merging)
            }
            Err(CreateError::RateLimited { until }) => {
                self.forge.record_rate_limit(&merging.host, until);
                self.notice = Some("GitHub is rate limiting: nothing was merged".into());
                vec![]
            }
            Err(error) => {
                self.push_error(ErrorPopup::new(
                    "Could not merge the pull request",
                    &error.merge_message(),
                    "Press Esc to close",
                ));
                vec![]
            }
        }
    }

    /// Turn the badge of each branch whose pull request was just merged
    /// to merged, kept over loads started before now, then ask the badges
    /// of the repository once more.
    fn mark_badge_merged(&mut self, merging: &PrMerging) -> Vec<Effect> {
        let on_badge_repo = self
            .forge
            .repo
            .as_ref()
            .is_some_and(|r| same_repo(r, &merging.repo));
        if !on_badge_repo {
            return vec![];
        }
        let merged: Vec<_> = self
            .forge
            .badges
            .iter()
            .filter(|(_, badge)| badge.number == merging.number)
            .map(|(branch, badge)| {
                let badge = PrBadge {
                    state: PrState::Merged,
                    ..*badge
                };
                (branch.clone(), badge)
            })
            .collect();
        // Loads from here on are asked after the merge.
        let known_from = self.next_seq;
        for (branch, badge) in merged {
            self.forge.add_created(branch, badge, known_from);
        }
        self.forge.want_refresh();
        self.apply_plan(RefreshPlan::of(&[Scope::PullRequests]))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crossterm::event::{KeyCode, KeyModifiers};

    use super::*;
    use crate::confirm_question::ConfirmQuestion;
    use crate::features::review::state::tests::{fixture_detail, in_queues, press, repo};
    use crate::forge::review::{MergeState, Mergeability, PrDetail, QueueKind};
    use crate::forge::{CiState, ForgeView};
    use crate::git::branches::{Branch, BranchName, Track};
    use crate::intent::AppIntent;
    use crate::keymap::KeyPress;
    use crate::test_support::load_list;

    const NOW: i64 = 1_700_000_000;

    /// Review mode on the fixture's pull request #12 (from the queues),
    /// with `change` made to its detail before it lands.
    fn on_detail(change: impl FnOnce(&mut PrDetail)) -> AppState {
        let mut state = in_queues();
        let effects = press(&mut state, KeyCode::Enter);
        let (tag, number) = crate::features::review::state::tests::detail_load(&effects);
        let mut detail = fixture_detail();
        detail.number = number;
        change(&mut detail);
        state.update(AppIntent::PrDetailLoaded {
            tag,
            number,
            repo: Some(repo()),
            result: Ok(Some(detail)),
        });
        assert_eq!(state.review.detail.as_ref().map(|d| d.number), Some(number));
        state
    }

    fn all_allowed(pr: &mut PrDetail) {
        pr.merge_methods = vec![MergeMethod::Merge, MergeMethod::Squash, MergeMethod::Rebase];
    }

    fn squash_only(pr: &mut PrDetail) {
        pr.merge_methods = vec![MergeMethod::Squash];
    }

    fn shift_m(state: &mut AppState) -> Vec<Effect> {
        state.update(AppIntent::Key(KeyPress::new(
            KeyCode::Char('M'),
            KeyModifiers::SHIFT,
        )))
    }

    fn top_question(state: &AppState) -> &ConfirmQuestion {
        match state.popups.last() {
            Some(Popup::Question(q)) if q.scope == KeyScope::MergePullRequest => q,
            other => panic!("expected the merge question, got {other:?}"),
        }
    }

    fn top_confirm(state: &AppState) -> &MergePrConfirm {
        match &top_question(state).then {
            Then::MergePr(confirm) => confirm,
            other => panic!("expected a merge, got {other:?}"),
        }
    }

    fn merges(effects: &[Effect]) -> Vec<&Effect> {
        effects
            .iter()
            .filter(|e| matches!(e, Effect::MergePullRequest { .. }))
            .collect()
    }

    /// Ask, answer `y`, and return the tag and the effect's fields.
    fn confirmed(state: &mut AppState) -> (Tag, u64, MergeMethod, String) {
        let effects = press(state, KeyCode::Char('y'));
        match effects.as_slice() {
            [
                Effect::MergePullRequest {
                    tag,
                    host,
                    repo: to,
                    number,
                    method,
                    head_oid,
                },
            ] => {
                assert_eq!(host.as_str(), "github.com");
                assert_eq!(*to, repo());
                (*tag, *number, *method, head_oid.as_str().to_string())
            }
            other => panic!("expected one merge, got {other:?}"),
        }
    }

    #[test]
    fn one_allowed_method_asks_at_once_and_only_y_merges() {
        let mut state = on_detail(squash_only);
        assert!(shift_m(&mut state).is_empty());
        let confirm = top_confirm(&state);
        assert_eq!(confirm.method, MergeMethod::Squash);
        assert_eq!(confirm.number, 12);
        assert_eq!(confirm.head_oid.as_str(), "1".repeat(40));
        assert!(!confirm.unstable);
        assert_eq!(state.key_focus(), crate::keymap::KeyScope::MergePullRequest);
        // Enter and the review keys do nothing here.
        for code in [KeyCode::Enter, KeyCode::Char('r'), KeyCode::Char('M')] {
            assert!(merges(&press(&mut state, code)).is_empty(), "{code:?}");
            top_question(&state);
        }
        let busy = state.busy_count();
        let (tag, number, method, head) = confirmed(&mut state);
        assert_eq!((number, method), (12, MergeMethod::Squash));
        assert_eq!(head, "1".repeat(40));
        assert!(tag.user);
        assert_eq!(state.busy_count(), busy + 1);
        assert!(state.popups.is_empty());
        // A second `y` has no question to answer.
        assert!(state.update(AppIntent::ConfirmMergePr).is_empty());
    }

    #[test]
    fn several_methods_are_picked_from_a_list_of_exactly_those() {
        let mut state = on_detail(all_allowed);
        assert!(shift_m(&mut state).is_empty());
        match state.popups.last() {
            Some(Popup::PickList(list)) => {
                assert_eq!(list.purpose, PickPurpose::MergeMethod);
                assert_eq!(list.rows, ["merge commit", "squash", "rebase"]);
            }
            other => panic!("expected a pick list, got {other:?}"),
        }
        assert!(press(&mut state, KeyCode::Char('j')).is_empty());
        assert!(press(&mut state, KeyCode::Char('j')).is_empty());
        // Picking is harmless: it only asks.
        assert!(merges(&press(&mut state, KeyCode::Enter)).is_empty());
        assert_eq!(top_confirm(&state).method, MergeMethod::Rebase);
        let (_, _, method, _) = confirmed(&mut state);
        assert_eq!(method, MergeMethod::Rebase);

        let mut state =
            on_detail(|pr| pr.merge_methods = vec![MergeMethod::Merge, MergeMethod::Squash]);
        shift_m(&mut state);
        assert!(matches!(state.popups.last(), Some(Popup::PickList(l)) if l.rows.len() == 2));
        // Closing the list asks nothing.
        press(&mut state, KeyCode::Esc);
        assert!(state.popups.is_empty());
    }

    /// One change to a detail.
    type Change = fn(&mut PrDetail);

    #[test]
    fn a_pull_request_that_cannot_merge_says_why_and_asks_nothing() {
        let cases: [(Change, &str); 10] = [
            (|pr| pr.state = PrState::Merged, "already merged"),
            (|pr| pr.state = PrState::Closed, "closed"),
            (|pr| pr.draft = true, "draft"),
            (|pr| pr.mergeable = Mergeability::Conflicting, "conflicts"),
            (|pr| pr.merge_state = MergeState::Dirty, "conflicts"),
            (|pr| pr.merge_state = MergeState::Blocked, "required review"),
            (|pr| pr.merge_state = MergeState::Behind, "updated"),
            (|pr| pr.merge_state = MergeState::Unknown, "refresh with r"),
            (|pr| pr.merge_methods.clear(), "no merge method"),
            (|pr| pr.head_oid = None, "no head commit"),
        ];
        for (change, words) in cases {
            let mut state = on_detail(change);
            assert!(shift_m(&mut state).is_empty(), "{words}");
            assert!(state.popups.is_empty(), "{words}");
            let notice = state.notice.as_deref().unwrap_or_default();
            assert!(
                notice.starts_with("Cannot merge: ") && notice.contains(words),
                "{notice:?} for {words}"
            );
        }
    }

    #[test]
    fn m_does_nothing_off_a_loaded_detail() {
        let mut state = in_queues();
        assert!(shift_m(&mut state).is_empty());
        assert!(state.popups.is_empty());
        assert_eq!(state.notice.as_deref(), Some(NO_PR_SHOWN));
        // Opened but not loaded yet.
        let mut state = in_queues();
        press(&mut state, KeyCode::Enter);
        shift_m(&mut state);
        assert!(state.popups.is_empty());
    }

    #[test]
    fn no_merge_reaches_github_for_a_host_off_the_config_or_rate_limited() {
        // Off the config by the time `M` is pressed.
        let mut state = on_detail(squash_only);
        state.forge.hosts.clear();
        assert!(shift_m(&mut state).is_empty());
        assert!(state.popups.is_empty());
        assert_eq!(state.notice.as_deref(), Some(NO_FORGE));
        // Rate limited by the time `M` is pressed.
        let mut state = on_detail(squash_only);
        state.forge.limited.insert("github.com".into(), NOW + 60);
        shift_m(&mut state);
        assert!(state.popups.is_empty());
        assert_eq!(state.notice.as_deref(), Some(REFRESH_LIMITED));
        // ... or between the question and the answer.
        for change in [
            (|s: &mut AppState| s.forge.hosts.clear()) as fn(&mut AppState),
            |s| {
                s.forge.limited.insert("github.com".into(), NOW + 60);
            },
        ] {
            let mut state = on_detail(squash_only);
            shift_m(&mut state);
            change(&mut state);
            let busy = state.busy_count();
            assert!(press(&mut state, KeyCode::Char('y')).is_empty());
            assert!(state.popups.is_empty());
            assert!(state.pr_merge.is_none());
            assert_eq!(state.busy_count(), busy);
        }
        // A limit that has passed no longer stops it.
        let mut state = on_detail(squash_only);
        state.forge.limited.insert("github.com".into(), NOW - 1);
        shift_m(&mut state);
        confirmed(&mut state);
    }

    #[test]
    fn a_repository_the_allow_list_leaves_out_is_never_merged() {
        use crate::forge::allow::AllowRule;
        let other = || vec![AllowRule::parse("other").unwrap()];
        // Left out by the time `M` is pressed: no question.
        let mut state = on_detail(squash_only);
        state.forge.hosts = vec![ForgeHost::parse("github.com").unwrap().with_allow(other())];
        assert!(shift_m(&mut state).is_empty());
        assert!(state.popups.is_empty());
        assert_eq!(state.notice.as_deref(), Some(NO_FORGE));
        assert!(NO_FORGE.contains("[forge.allow]"));
        // Left out between the question and `y`: no merge.
        let mut state = on_detail(squash_only);
        shift_m(&mut state);
        top_question(&state);
        state.forge.hosts = vec![ForgeHost::parse("github.com").unwrap().with_allow(other())];
        assert!(press(&mut state, KeyCode::Char('y')).is_empty());
        assert!(state.pr_merge.is_none());
        assert_eq!(state.notice.as_deref(), Some(NO_FORGE));
        // An allow-list that names it lets it through.
        let mut state = on_detail(squash_only);
        state.forge.hosts = vec![
            ForgeHost::parse("github.com")
                .unwrap()
                .with_allow(vec![AllowRule::parse("acme/widgets").unwrap()]),
        ];
        shift_m(&mut state);
        let effects = press(&mut state, KeyCode::Char('y'));
        assert_eq!(merges(&effects).len(), 1);
    }

    #[test]
    fn n_and_esc_merge_nothing() {
        for code in [KeyCode::Char('n'), KeyCode::Esc] {
            let mut state = on_detail(squash_only);
            shift_m(&mut state);
            assert!(press(&mut state, code).is_empty());
            assert!(state.popups.is_empty());
            assert!(state.pr_merge.is_none());
        }
    }

    #[test]
    fn the_question_warns_when_checks_have_not_passed() {
        let mut state = on_detail(|pr| {
            squash_only(pr);
            pr.merge_state = MergeState::Unstable;
        });
        shift_m(&mut state);
        let question = &top_question(&state).text;
        assert!(question.starts_with("Press y to merge"), "{question}");
        assert!(
            question.contains("Merge #12 Add widget caching into main by squash?"),
            "{question}"
        );
        assert!(
            question.ends_with("Some checks have not passed"),
            "{question}"
        );
        let mut state = on_detail(squash_only);
        shift_m(&mut state);
        assert!(!top_question(&state).text.contains("checks"));
    }

    /// The fixture's workspace with local `topic` showing #12's badge.
    fn with_badge(state: &mut AppState) {
        load_list(
            &mut state.branches.list,
            Ok(vec![Branch {
                name: "topic".into(),
                head: false,
                upstream: Some("origin/topic".into()),
                track: Track::None,
                merged: false,
            }]),
        );
        state.forge.badges.insert(
            "topic".into(),
            PrBadge {
                number: 12,
                state: PrState::Open,
                ci: Some(CiState::Success),
            },
        );
        state.forge.status = ForgeView::Fresh;
    }

    fn badge(state: &AppState) -> PrBadge {
        state.forge.badges[&BranchName::from("topic")]
    }

    #[test]
    fn a_merge_shows_at_once_in_the_detail_queues_and_badge() {
        let mut state = on_detail(squash_only);
        with_badge(&mut state);
        assert!(state.review.rows().iter().any(|pr| pr.number == 12));
        shift_m(&mut state);
        let (tag, number, ..) = confirmed(&mut state);
        let busy = state.busy_count();
        let effects = state.update(AppIntent::PullRequestMerged {
            tag,
            number,
            result: Ok(()),
        });
        assert_eq!(state.busy_count(), busy - 1);
        assert_eq!(state.review.detail.as_ref().unwrap().state, PrState::Merged);
        let queues = state.review.queues.as_ref().unwrap();
        for kind in QueueKind::ALL {
            assert!(
                queues.get(kind).iter().all(|pr| pr.number != 12),
                "{kind:?}"
            );
        }
        assert_eq!(
            badge(&state),
            PrBadge {
                number: 12,
                state: PrState::Merged,
                ci: Some(CiState::Success),
            }
        );
        assert_eq!(state.notice_text.as_deref(), Some("Merged #12 (squash)"));
        // The badges are asked for again, once.
        let loads: Vec<Tag> = effects
            .iter()
            .filter_map(|e| match e {
                Effect::LoadPullRequests { tag, .. } => Some(*tag),
                _ => None,
            })
            .collect();
        assert_eq!(loads.len(), 1, "{effects:?}");
        assert!(merges(&effects).is_empty());
        // The detail line says it can merge no more.
        assert!(merge_disabled(&state).is_none());
        shift_m(&mut state);
        assert!(state.popups.is_empty());
        assert_eq!(
            state.notice.as_deref(),
            Some("Cannot merge: already merged")
        );
    }

    #[test]
    fn a_load_started_before_the_merge_cannot_bring_the_open_badge_back() {
        let mut state = on_detail(squash_only);
        with_badge(&mut state);
        // A badge load is running when the merge is asked for.
        let before = state.load_pull_requests();
        let Some(Effect::LoadPullRequests { tag: old, .. }) = before.first() else {
            panic!("{before:?}");
        };
        let old = *old;
        shift_m(&mut state);
        let (tag, number, ..) = confirmed(&mut state);
        state.update(AppIntent::PullRequestMerged {
            tag,
            number,
            result: Ok(()),
        });
        assert_eq!(badge(&state).state, PrState::Merged);
        let open = HashMap::from([(
            BranchName::from("topic"),
            PrBadge {
                number: 12,
                state: PrState::Open,
                ci: Some(CiState::Success),
            },
        )]);
        state.update(AppIntent::PullRequestsFetched {
            tag: old,
            result: Ok(open),
            limited_until: None,
        });
        assert_eq!(badge(&state).state, PrState::Merged);
        assert_eq!(badge(&state).ci, Some(CiState::Success));

        // The branches panel draws it merged.
        state.context = Context::Workspace;
        state.focus = crate::model::Focus::Branches;
        state.expanded = crate::layout::SidePanel::Branches;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        terminal
            .draw(|frame| crate::view::render(&state, frame))
            .unwrap();
        let rows = crate::view::buffer_rows(terminal.backend().buffer());
        assert!(
            rows.iter()
                .any(|r| r.contains("topic") && r.contains("#12 merged ✓")),
            "{rows:#?}"
        );
    }

    #[test]
    fn the_badges_reload_once_even_with_no_branch_for_the_pull_request() {
        let mut state = on_detail(squash_only);
        shift_m(&mut state);
        let (tag, number, ..) = confirmed(&mut state);
        let effects = state.update(AppIntent::PullRequestMerged {
            tag,
            number,
            result: Ok(()),
        });
        assert!(state.forge.badges.is_empty());
        assert!(matches!(
            effects.as_slice(),
            [Effect::LoadPullRequests { .. }]
        ));
        // Once the badges come from another repository, nothing reloads.
        let mut state = on_detail(squash_only);
        shift_m(&mut state);
        let (tag, number, ..) = confirmed(&mut state);
        state.forge.repo = Some(ForgeRepo {
            name: "other".into(),
            ..repo()
        });
        let effects = state.update(AppIntent::PullRequestMerged {
            tag,
            number,
            result: Ok(()),
        });
        assert_eq!(effects, []);
    }

    #[test]
    fn a_stale_answer_changes_nothing() {
        let mut state = on_detail(squash_only);
        with_badge(&mut state);
        shift_m(&mut state);
        let (tag, number, ..) = confirmed(&mut state);
        let busy = state.busy_count();
        let stale = Tag {
            seq: tag.seq + 100,
            ..tag
        };
        let effects = state.update(AppIntent::PullRequestMerged {
            tag: stale,
            number,
            result: Ok(()),
        });
        assert!(effects.is_empty());
        // Its busy count is still let go.
        assert_eq!(state.busy_count(), busy - 1);
        assert_eq!(state.review.detail.as_ref().unwrap().state, PrState::Open);
        assert_eq!(badge(&state).state, PrState::Open);
        assert!(state.pr_merge.is_some());
        // Another repository's generation is no answer either.
        let other = Tag {
            generation: tag.generation + 1,
            ..tag
        };
        state.update(AppIntent::PullRequestMerged {
            tag: other,
            number,
            result: Ok(()),
        });
        assert_eq!(state.review.detail.as_ref().unwrap().state, PrState::Open);
        assert_eq!(badge(&state).state, PrState::Open);
    }

    #[test]
    fn a_refusal_shows_an_error_and_a_limit_is_remembered() {
        let mut state = on_detail(squash_only);
        shift_m(&mut state);
        let (tag, number, ..) = confirmed(&mut state);
        state.update(AppIntent::PullRequestMerged {
            tag,
            number,
            result: Err(CreateError::Refused("Head branch was modified".into())),
        });
        match state.popups.as_slice() {
            [Popup::Error(error)] => {
                let shown = format!("{error:?}");
                assert!(
                    shown.contains("Could not merge the pull request"),
                    "{shown}"
                );
                assert!(shown.contains("Head branch was modified"), "{shown}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(state.review.detail.as_ref().unwrap().state, PrState::Open);
        assert!(state.pr_merge.is_none());

        let mut state = on_detail(squash_only);
        shift_m(&mut state);
        let (tag, number, ..) = confirmed(&mut state);
        state.update(AppIntent::PullRequestMerged {
            tag,
            number,
            result: Err(CreateError::RateLimited { until: NOW + 600 }),
        });
        assert_eq!(state.forge.limited.get("github.com"), Some(&(NOW + 600)));
        assert!(state.popups.is_empty());
        // The next try is refused before it asks.
        shift_m(&mut state);
        assert!(state.popups.is_empty());
        assert_eq!(state.notice.as_deref(), Some(REFRESH_LIMITED));
    }

    #[test]
    fn only_one_merge_runs_at_a_time() {
        let mut state = on_detail(squash_only);
        shift_m(&mut state);
        confirmed(&mut state);
        shift_m(&mut state);
        assert!(state.popups.is_empty());
        assert_eq!(state.notice.as_deref(), Some(MERGING));
    }
}
