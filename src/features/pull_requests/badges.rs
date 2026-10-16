//! The per-branch pull-request badges: loading them, refreshing them on
//! request, and taking the forge's answer. Only this side depends on forge,
//! never the other way round.

use std::collections::HashMap;

use super::create::refresh_disabled;
use crate::effect::{Effect, Tag};
use crate::model::{AppState, Context};
use kika_forge::{ForgeHost, ForgeState, ForgeStatus, LoadRequest, PrBadge};
use kika_git::branches::BranchName;

/// The effect that runs `request` and answers under `tag`.
fn load_effect(tag: Tag, request: LoadRequest) -> Effect {
    let LoadRequest {
        hosts,
        branches,
        refresh,
        known,
        limited,
    } = request;
    Effect::LoadPullRequests {
        tag,
        hosts,
        branches,
        refresh,
        known,
        limited,
    }
}

/// The forge's part of `update`.
impl AppState {
    /// The forge hosts the global config lets Kika contact; none is off.
    pub fn with_forge_hosts(self, hosts: Vec<ForgeHost>) -> Self {
        Self {
            forge: ForgeState::new(hosts),
            ..self
        }
    }

    /// Start a pull-request load when the forge is on and no call runs.
    pub(crate) fn load_pull_requests(&mut self) -> Vec<Effect> {
        if !self.forge.enabled() || self.forge.in_flight {
            return vec![];
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        let tag = Tag {
            generation: self.generation,
            seq,
            user: false,
        };
        self.forge
            .load(tag)
            .map(|request| load_effect(tag, request))
            .into_iter()
            .collect()
    }

    /// Ask the forge again because the user pressed for it: the same
    /// refresh a fetch starts, refused with a notice when it may not run.
    pub(crate) fn refresh_pull_requests(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        if let Some(reason) = refresh_disabled(self) {
            self.notice = Some(reason);
            return vec![];
        }
        self.forge.want_refresh();
        self.load_pull_requests()
    }

    /// Show the answer, then run a refresh that waited for this call.
    pub(crate) fn pull_requests_fetched(
        &mut self,
        tag: Tag,
        result: Result<HashMap<BranchName, PrBadge>, ForgeStatus>,
        limited_until: Option<i64>,
    ) -> Vec<Effect> {
        if tag.generation != self.generation {
            return vec![];
        }
        self.forge.fetched(tag, result, limited_until);
        if self.forge.refresh_wanted {
            self.load_pull_requests()
        } else {
            vec![]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::pull_requests::PullRequestsIntent;
    use crate::intent::AppIntent;
    use crate::layout::SidePanel;
    use crate::model::{CommitFlow, Context, Focus};
    use crate::test_support::{
        branches_loaded, forge_state, open_commit, remote_finished, remote_intent, toggle_stage,
    };
    use kika_forge::{CiState, ForgeHost, ForgeRepo, ForgeState, ForgeView, PrState};
    use kika_git::FileEntry;
    use kika_git::branches::{Branch, Track};
    use kika_git::remote::RemoteOp;
    use kika_git::status::{BranchStatus, EntryKind, Head};

    fn github() -> Vec<ForgeHost> {
        vec![ForgeHost::parse("github.com").unwrap()]
    }

    fn branch(name: &str) -> Branch {
        Branch {
            name: name.into(),
            head: name == "main",
            upstream: None,
            track: Track::None,
            merged: true,
        }
    }

    fn repo() -> ForgeRepo {
        ForgeRepo {
            host: "github.com".into(),
            owner: "acme".into(),
            name: "widgets".into(),
        }
    }

    fn badge(number: u64) -> PrBadge {
        PrBadge {
            number,
            state: PrState::Open,
            ci: Some(CiState::Success),
        }
    }

    fn loads(effects: &[Effect]) -> Vec<(Tag, bool)> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::LoadPullRequests { tag, refresh, .. } => Some((*tag, *refresh)),
                _ => None,
            })
            .collect()
    }

    /// The tag of the `LoadBranches` in `effects`.
    fn branches_tag(effects: &[Effect]) -> Tag {
        effects
            .iter()
            .find_map(|e| match e {
                Effect::LoadBranches { tag } => Some(*tag),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no LoadBranches in {effects:?}"))
    }

    /// Reload the branches and answer with `names`; every effect seen.
    fn list_branches(state: &mut AppState, names: &[&str]) -> Vec<Effect> {
        let mut effects = state.update(AppIntent::RefreshStatus);
        let tag = branches_tag(&effects);
        effects.extend(state.update(branches_loaded(
            tag,
            names.iter().map(|n| branch(n)).collect(),
        )));
        effects
    }

    /// Run `op` from its intent to a successful end, and answer the branch
    /// reload that follows; every effect seen.
    fn remote_ok(state: &mut AppState, intent: AppIntent, op: RemoteOp) -> Vec<Effect> {
        state.branch = Some(BranchStatus {
            head: Head::Branch("main".into()),
            upstream: None,
            ahead_behind: None,
            in_progress: vec![],
            bisect: None,
        });
        let mut effects = state.update(intent);
        let tag = effects
            .iter()
            .find_map(|e| match e {
                Effect::Remote { tag, .. } => Some(*tag),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no Remote in {effects:?}"));
        let finished = state.update(remote_finished(tag, op, Ok(())));
        let tag = branches_tag(&finished);
        effects.extend(finished);
        effects.extend(state.update(branches_loaded(tag, vec![branch("main")])));
        effects
    }

    /// A workspace in a repository, so remote operations may run.
    fn in_repo(hosts: Vec<ForgeHost>) -> AppState {
        AppState::new(Context::Workspace)
            .with_location(Some("/r".into()), "/r".into())
            .with_forge_hosts(hosts)
    }

    #[test]
    fn off_by_default_no_load_is_ever_asked_for() {
        let tmp = kika_git::fake::TempDir::new("forge-default");
        let global = tmp.path().join("config.toml");
        std::fs::write(&global, "[watch]\nrefs = 3\n").unwrap();
        let loaded = crate::config::load(&crate::config::ConfigPaths::new(Some(global), None));
        assert_eq!(loaded.errors, []);
        assert_eq!(loaded.forge_hosts, []);

        let mut state = in_repo(loaded.forge_hosts);
        let mut effects = state.update(AppIntent::ConfigLoaded {
            repo: None,
            keymap: loaded.keymap,
            errors: loaded.errors,
        });
        effects.extend(list_branches(&mut state, &["main", "topic"]));
        effects.extend(remote_ok(
            &mut state,
            remote_intent(&RemoteOp::Fetch),
            RemoteOp::Fetch,
        ));
        let push = RemoteOp::Push {
            force_with_lease: false,
        };
        effects.extend(remote_ok(&mut state, remote_intent(&push), push));
        effects.extend(list_branches(&mut state, &["main"]));
        assert_eq!(loads(&effects), []);
        assert_eq!(state.forge.status, ForgeView::Off);
    }

    #[test]
    fn the_first_branch_list_refreshes_and_later_ones_read_the_cache() {
        let mut state = AppState::new(Context::Workspace).with_forge_hosts(github());
        let effects = list_branches(&mut state, &["main", "topic"]);
        let [(first, true)] = loads(&effects)[..] else {
            panic!("{effects:?}");
        };
        match effects
            .iter()
            .find(|e| matches!(e, Effect::LoadPullRequests { .. }))
        {
            Some(Effect::LoadPullRequests {
                hosts, branches, ..
            }) => {
                assert_eq!(*hosts, github());
                assert_eq!(*branches, [BranchName::from("main"), "topic".into()]);
            }
            other => panic!("{other:?}"),
        }
        assert!(state.forge.in_flight);
        // While it runs, nothing else starts.
        assert_eq!(loads(&list_branches(&mut state, &["main"])), []);

        state.update(AppIntent::PullRequests(PullRequestsIntent::Fetched {
            tag: first,
            result: Ok(HashMap::new()),
            limited_until: None,
        }));
        assert!(!state.forge.in_flight);
        assert_eq!(state.forge.status, ForgeView::Fresh);
        let later = loads(&list_branches(&mut state, &["main"]));
        assert!(matches!(later[..], [(_, false)]), "{later:?}");
    }

    #[test]
    fn a_user_fetch_pull_or_push_refreshes_and_one_waits_for_a_call_in_flight() {
        let mut state = in_repo(github());
        let [(first, true)] = loads(&list_branches(&mut state, &["main"]))[..] else {
            panic!()
        };
        // A push ends while the first call still runs: its refresh waits.
        let push = RemoteOp::Push {
            force_with_lease: false,
        };
        let effects = remote_ok(&mut state, remote_intent(&push), push);
        assert_eq!(loads(&effects), []);
        let effects = state.update(AppIntent::PullRequests(PullRequestsIntent::Fetched {
            tag: first,
            result: Ok(HashMap::new()),
            limited_until: None,
        }));
        let [(second, true)] = loads(&effects)[..] else {
            panic!("{effects:?}")
        };
        state.update(AppIntent::PullRequests(PullRequestsIntent::Fetched {
            tag: second,
            result: Ok(HashMap::new()),
            limited_until: None,
        }));

        for (intent, op) in [
            (remote_intent(&RemoteOp::Fetch), RemoteOp::Fetch),
            (remote_intent(&RemoteOp::Pull), RemoteOp::Pull),
        ] {
            let effects = remote_ok(&mut state, intent, op);
            let [(tag, true)] = loads(&effects)[..] else {
                panic!("{effects:?}")
            };
            state.update(AppIntent::PullRequests(PullRequestsIntent::Fetched {
                tag,
                result: Ok(HashMap::new()),
                limited_until: None,
            }));
        }
    }

    #[test]
    fn another_repository_starts_over_with_a_refresh_and_drops_old_answers() {
        let mut state = AppState::new(Context::Workspace).with_forge_hosts(github());
        let [(old, true)] = loads(&list_branches(&mut state, &["main"]))[..] else {
            panic!()
        };
        state.forge.badges.insert("main".into(), badge(1));
        state.generation += 1;
        state.forge.reset();
        assert_eq!(state.forge.hosts, github());
        assert!(state.forge.badges.is_empty() && !state.forge.in_flight);
        state.update(AppIntent::PullRequests(PullRequestsIntent::Fetched {
            tag: old,
            result: Ok(HashMap::from([("main".into(), badge(9))])),
            limited_until: None,
        }));
        assert!(state.forge.badges.is_empty());
        assert!(matches!(
            loads(&list_branches(&mut state, &["main"]))[..],
            [(_, true)]
        ));
    }

    #[test]
    fn a_known_repository_is_reused_and_an_unmatched_one_is_not_looked_at_again() {
        let mut state = in_repo(github());
        let [(tag, true)] = loads(&list_branches(&mut state, &["main"]))[..] else {
            panic!()
        };
        state.update(AppIntent::PullRequests(PullRequestsIntent::Cached {
            tag,
            repo: repo(),
            badges: None,
        }));
        state.update(AppIntent::PullRequests(PullRequestsIntent::Fetched {
            tag,
            result: Ok(HashMap::new()),
            limited_until: None,
        }));
        let known = |effects: &[Effect]| {
            effects.iter().find_map(|e| match e {
                Effect::LoadPullRequests { known, refresh, .. } => Some((known.clone(), *refresh)),
                _ => None,
            })
        };
        // A cache-only reload names the repository, so no remote is read.
        let effects = list_branches(&mut state, &["main"]);
        assert_eq!(known(&effects), Some((Some(repo()), false)));
        // A refresh reads the remote again.
        let effects = remote_ok(&mut state, remote_intent(&RemoteOp::Fetch), RemoteOp::Fetch);
        let (tag, _) = loads(&effects)[0];
        assert_eq!(known(&effects), Some((None, true)));

        // Once the remote matched no host, reloads start nothing, until a
        // user fetch asks again.
        state.update(AppIntent::PullRequests(PullRequestsIntent::RepoNone {
            tag,
        }));
        assert_eq!(loads(&list_branches(&mut state, &["main"])), []);
        let effects = remote_ok(&mut state, remote_intent(&RemoteOp::Fetch), RemoteOp::Fetch);
        assert_eq!(known(&effects), Some((None, true)));
    }

    #[test]
    fn a_rate_limit_is_remembered_for_the_host_until_an_answer_clears_it() {
        const T: i64 = 1_700_000_000;
        let limited_of = |effects: &[Effect]| {
            effects
                .iter()
                .find_map(|e| match e {
                    Effect::LoadPullRequests { limited, .. } => Some(limited.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no load in {effects:?}"))
        };
        let mut state = in_repo(github());
        let effects = list_branches(&mut state, &["main"]);
        assert_eq!(limited_of(&effects), []);
        let [(tag, true)] = loads(&effects)[..] else {
            panic!()
        };
        let shown = HashMap::from([("main".into(), badge(3))]);
        state.update(AppIntent::PullRequests(PullRequestsIntent::Cached {
            tag,
            repo: ForgeRepo {
                host: "GitHub.com".into(),
                ..repo()
            },
            badges: Some(shown.clone()),
        }));
        let effects = state.update(AppIntent::PullRequests(PullRequestsIntent::Fetched {
            tag,
            result: Err(ForgeStatus::RateLimited),
            limited_until: Some(T),
        }));
        assert_eq!(effects, []);
        assert_eq!(
            state.forge.limited,
            HashMap::from([("github.com".into(), T)])
        );
        assert_eq!(state.forge.limited_until, Some(T));
        assert_eq!(state.forge.badges, shown);
        assert_eq!(
            state.forge.status,
            ForgeView::Problem(ForgeStatus::RateLimited)
        );
        assert_eq!(state.op_error(), None);
        assert!(state.popups.is_empty());

        // A user fetch hands the limit to the task, which decides by the clock.
        let effects = remote_ok(&mut state, remote_intent(&RemoteOp::Fetch), RemoteOp::Fetch);
        assert_eq!(limited_of(&effects), [(github()[0].clone(), T)]);

        // Another repository: the limit is the account's, so it stays.
        state.generation += 1;
        state.forge.reset();
        assert_eq!(
            state.forge.limited,
            HashMap::from([("github.com".into(), T)])
        );
        assert_eq!(state.forge.limited_until, None);
        let effects = list_branches(&mut state, &["main"]);
        assert_eq!(limited_of(&effects), [(github()[0].clone(), T)]);
        let [(tag2, true)] = loads(&effects)[..] else {
            panic!("{effects:?}")
        };

        // An answer clears it.
        state.update(AppIntent::PullRequests(PullRequestsIntent::Cached {
            tag: tag2,
            repo: repo(),
            badges: None,
        }));
        state.update(AppIntent::PullRequests(PullRequestsIntent::Fetched {
            tag: tag2,
            result: Ok(HashMap::new()),
            limited_until: None,
        }));
        assert!(state.forge.limited.is_empty());
        assert_eq!(state.forge.limited_until, None);
        let effects = remote_ok(&mut state, remote_intent(&RemoteOp::Fetch), RemoteOp::Fetch);
        assert_eq!(limited_of(&effects), []);
    }

    #[test]
    fn a_remote_on_no_configured_host_shows_nothing() {
        let mut state = AppState::new(Context::Workspace).with_forge_hosts(github());
        let [(tag, true)] = loads(&list_branches(&mut state, &["main"]))[..] else {
            panic!()
        };
        state.update(AppIntent::PullRequests(PullRequestsIntent::RepoNone {
            tag,
        }));
        assert_eq!(state.forge.status, ForgeView::Off);
        assert!(!state.forge.in_flight);
        let rows = crate::test_support::draw_branches(&state, 80, 12);
        assert!(rows.iter().all(|r| !r.contains("GitHub")), "{rows:?}");
    }

    #[test]
    fn cached_badges_draw_while_the_refresh_still_runs() {
        let names: Vec<BranchName> = vec!["main".into(), "topic".into()];
        let mut state = forge_state(&names);
        let [(tag, true)] = loads(&state.load_pull_requests())[..] else {
            panic!()
        };
        state.update(AppIntent::PullRequests(PullRequestsIntent::Cached {
            tag,
            repo: repo(),
            badges: Some(HashMap::from([("topic".into(), badge(12))])),
        }));
        assert_eq!(state.forge.status, ForgeView::Updating);
        let rows = crate::test_support::draw_branches(&state, 80, 12);
        assert!(
            rows.iter().any(|r| r.contains("topic  #12 open ✓")),
            "{rows:#?}"
        );
        assert!(
            rows.iter().any(|r| r.contains("GitHub: updating…")),
            "{rows:#?}"
        );
    }

    #[test]
    fn offline_keeps_the_cached_badges_and_never_blocks_local_work() {
        let names: Vec<BranchName> = vec!["main".into()];
        let mut state = forge_state(&names);
        let [(tag, true)] = loads(&state.load_pull_requests())[..] else {
            panic!()
        };
        state.update(AppIntent::PullRequests(PullRequestsIntent::Cached {
            tag,
            repo: repo(),
            badges: Some(HashMap::from([("main".into(), badge(3))])),
        }));
        let effects = state.update(AppIntent::PullRequests(PullRequestsIntent::Fetched {
            tag,
            result: Err(ForgeStatus::Offline),
            limited_until: None,
        }));
        assert_eq!(effects, []);
        assert_eq!(state.forge.badges[&BranchName::from("main")], badge(3));
        assert_eq!(state.forge.status, ForgeView::Problem(ForgeStatus::Offline));
        assert_eq!(state.op_error(), None);
        assert!(state.popups.is_empty());
        let rows = crate::test_support::draw_branches(&state, 80, 12);
        assert!(
            rows.iter().any(|r| r.contains("GitHub: offline · cached")),
            "{rows:#?}"
        );

        // Staging and committing go on as ever.
        state.files.rows = vec![FileEntry {
            path: "a.rs".into(),
            orig_path: None,
            index: '?',
            worktree: '?',
            kind: EntryKind::Untracked,
        }];
        state.files.selected = 0;
        state.focus = Focus::Files;
        state.expanded = SidePanel::Files;
        let staged = toggle_stage(&mut state);
        assert!(
            staged.iter().any(|e| matches!(e, Effect::RunOp { .. })),
            "{staged:?}"
        );
        state.free_op();
        open_commit(&mut state);
        assert_ne!(state.commit, CommitFlow::Closed);
    }

    #[test]
    fn stale_answers_are_dropped() {
        let mut state = forge_state(&["main".into()]);
        let [(tag, true)] = loads(&state.load_pull_requests())[..] else {
            panic!()
        };
        let old = Tag {
            seq: tag.seq + 100,
            ..tag
        };
        state.update(AppIntent::PullRequests(PullRequestsIntent::Cached {
            tag: old,
            repo: repo(),
            badges: Some(HashMap::from([("main".into(), badge(1))])),
        }));
        state.update(AppIntent::PullRequests(PullRequestsIntent::RepoNone {
            tag: old,
        }));
        state.update(AppIntent::PullRequests(PullRequestsIntent::Fetched {
            tag: old,
            result: Err(ForgeStatus::Failed),
            limited_until: None,
        }));
        assert!(state.forge.in_flight);
        assert_eq!(state.forge.repo, None);
        assert_eq!(state.forge.status, ForgeView::Updating);
    }

    /// A workspace on github.com whose first load is done: the remote
    /// matched, nothing in flight, no refresh waiting.
    fn loaded() -> AppState {
        let mut state = forge_state(&["main".into()]);
        state.forge.repo = Some(repo());
        state.forge.refresh_wanted = false;
        state.forge.status = ForgeView::Fresh;
        state
    }

    fn press_refresh(state: &mut AppState) -> Vec<Effect> {
        let key = crate::keymap::KeyPress::new(
            crossterm::event::KeyCode::Char('B'),
            crossterm::event::KeyModifiers::SHIFT,
        );
        state.update(AppIntent::Key(key))
    }

    #[test]
    fn the_refresh_key_asks_the_forge_again_once() {
        let mut state = loaded();
        let effects = state.update(AppIntent::PullRequests(PullRequestsIntent::Refresh));
        let [(tag, true)] = loads(&effects)[..] else {
            panic!("{effects:?}")
        };
        assert_eq!(effects.len(), 1, "{effects:?}");
        assert!(state.forge.in_flight);
        assert_eq!(state.forge.status, ForgeView::Updating);
        assert_eq!(state.notice, None);

        // Again before the answer: refused, and nothing queued behind it.
        assert_eq!(press_refresh(&mut state), []);
        assert_eq!(
            state.notice.as_deref(),
            Some("Pull requests are already being refreshed")
        );
        assert!(!state.forge.refresh_wanted);
        let effects = state.update(AppIntent::PullRequests(PullRequestsIntent::Fetched {
            tag,
            result: Ok(HashMap::new()),
            limited_until: None,
        }));
        assert_eq!(effects, []);
        assert!(!state.forge.in_flight);
    }

    #[test]
    fn the_refresh_key_is_refused_with_no_hosts_or_no_matched_remote() {
        let mut off = loaded();
        off.forge = ForgeState::new(vec![]);
        assert_eq!(press_refresh(&mut off), []);
        assert_eq!(
            off.notice.as_deref(),
            Some("Name GitHub hosts under [forge] in the global config")
        );
        assert_eq!(
            off.update(AppIntent::PullRequests(PullRequestsIntent::Refresh)),
            []
        );

        let mut unknown = loaded();
        unknown.forge.repo = None;
        assert_eq!(press_refresh(&mut unknown), []);
        assert_eq!(
            unknown.notice.as_deref(),
            Some("The remote is on no forge host in the global config, or not in [forge.allow]")
        );

        let mut unmatched = loaded();
        let [(tag, true)] =
            loads(&unmatched.update(AppIntent::PullRequests(PullRequestsIntent::Refresh)))[..]
        else {
            panic!()
        };
        unmatched.update(AppIntent::PullRequests(PullRequestsIntent::RepoNone {
            tag,
        }));
        assert!(unmatched.forge.unmatched);
        assert_eq!(
            unmatched.update(AppIntent::PullRequests(PullRequestsIntent::Refresh)),
            []
        );
        assert_eq!(
            unmatched.notice.as_deref(),
            Some("The remote is on no forge host in the global config, or not in [forge.allow]")
        );
    }

    #[test]
    fn the_refresh_key_waits_out_a_rate_limit_and_runs_once_it_has_passed() {
        const T: i64 = 1_700_000_000;
        let mut state = loaded();
        state.forge.limited.insert("github.com".into(), T);
        state.forge.repo = Some(ForgeRepo {
            host: "GitHub.com".into(),
            ..repo()
        });
        state.now = T - 1;
        assert_eq!(press_refresh(&mut state), []);
        assert_eq!(
            state.notice.as_deref(),
            Some("GitHub is rate limited; refresh after the limit resets")
        );
        assert!(!state.forge.in_flight);

        state.now = T;
        state.notice = None;
        let effects = press_refresh(&mut state);
        assert!(matches!(loads(&effects)[..], [(_, true)]), "{effects:?}");
        assert_eq!(state.notice, None);
    }

    #[test]
    fn a_passed_rate_limit_waits_for_the_user_to_ask_again() {
        const T: i64 = 1_700_000_000;
        let limited = || {
            let mut state = loaded();
            state.forge.limited.insert("github.com".into(), T);
            state.forge.limited_until = Some(T);
            state.forge.status = ForgeView::Problem(ForgeStatus::RateLimited);
            state.now = T + 60;
            state
        };
        let refreshes = |effects: &[Effect]| loads(effects).iter().filter(|(_, r)| *r).count();

        // A branch-list reload only reads the cache.
        let mut state = limited();
        let effects = list_branches(&mut state, &["main"]);
        assert!(matches!(loads(&effects)[..], [(_, false)]), "{effects:?}");
        assert_eq!(refreshes(&effects), 0, "{effects:?}");
        assert_eq!(
            state.forge.status,
            ForgeView::Problem(ForgeStatus::RateLimited)
        );

        // So does a refs poll.
        let mut state = limited();
        let effects = state.update(AppIntent::Watch(crate::watch::WatchEvent::PollRefs));
        assert_eq!(refreshes(&effects), 0, "{effects:?}");
        assert_eq!(
            state.forge.status,
            ForgeView::Problem(ForgeStatus::RateLimited)
        );

        // Asking again does call the forge.
        let mut state = limited();
        let effects = state.update(AppIntent::PullRequests(PullRequestsIntent::Refresh));
        assert_eq!(refreshes(&effects), 1, "{effects:?}");
    }

    fn gitlab() -> Vec<ForgeHost> {
        vec![ForgeHost::parse_gitlab("gitlab.example.com").unwrap()]
    }

    /// The hosts of each load in `effects`, with whether it refreshes.
    fn load_hosts(effects: &[Effect]) -> Vec<(Vec<ForgeHost>, bool)> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::LoadPullRequests { hosts, refresh, .. } => Some((hosts.clone(), *refresh)),
                _ => None,
            })
            .collect()
    }

    /// Answer the load in flight, so the next one may start.
    fn answer(state: &mut AppState, effects: &[Effect]) {
        let [(tag, _)] = loads(effects)[..] else {
            panic!("{effects:?}")
        };
        state.update(AppIntent::PullRequests(PullRequestsIntent::Fetched {
            tag,
            result: Ok(HashMap::new()),
            limited_until: None,
        }));
    }

    #[test]
    fn a_gitlab_host_loads_as_a_github_one_does() {
        let mut state = in_repo(gitlab());
        // Opening the repository.
        let effects = list_branches(&mut state, &["main"]);
        assert_eq!(load_hosts(&effects), [(gitlab(), true)]);
        answer(&mut state, &effects);
        state.forge.repo = Some(ForgeRepo {
            host: "gitlab.example.com".into(),
            ..repo()
        });
        // `B`, from the Branches panel.
        state.focus = Focus::Branches;
        state.files.loaded = true;
        let effects = press_refresh(&mut state);
        assert_eq!(load_hosts(&effects), [(gitlab(), true)]);
        answer(&mut state, &effects);
        // A finished fetch, pull or push.
        let push = RemoteOp::Push {
            force_with_lease: false,
        };
        for (intent, op) in [
            (remote_intent(&RemoteOp::Fetch), RemoteOp::Fetch),
            (remote_intent(&RemoteOp::Pull), RemoteOp::Pull),
            (remote_intent(&push), push),
        ] {
            let effects = remote_ok(&mut state, intent, op);
            assert_eq!(load_hosts(&effects), [(gitlab(), true)]);
            answer(&mut state, &effects);
        }
    }

    #[test]
    fn the_refresh_key_names_gitlab_when_its_host_is_rate_limited() {
        const T: i64 = 1_700_000_000;
        let mut state = loaded();
        state.forge.hosts = gitlab();
        state.forge.repo = Some(ForgeRepo {
            host: "gitlab.example.com".into(),
            ..repo()
        });
        state.forge.limited.insert("gitlab.example.com".into(), T);
        state.now = T - 1;
        assert_eq!(press_refresh(&mut state), []);
        assert_eq!(
            state.notice.as_deref(),
            Some("GitLab is rate limited; refresh after the limit resets")
        );
    }

    #[test]
    fn the_refresh_key_does_nothing_outside_the_workspace() {
        let mut state = loaded();
        state.context = Context::Review { number: None };
        assert_eq!(
            state.update(AppIntent::PullRequests(PullRequestsIntent::Refresh)),
            []
        );
        assert!(!state.forge.in_flight);
    }
}
