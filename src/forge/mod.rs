//! Pull requests from a forge, shown beside the local branches. Off unless
//! the global config names the hosts Kika may contact: with none, no
//! effect is ever asked for, so nothing reaches the network.
//!
//! GitHub is reached through the `gh` CLI, never an HTTP client of Kika's
//! own, so the user's login, proxy and TLS settings apply and Kika never
//! holds a token.

pub mod cache;
pub mod github;
pub mod head;
pub mod remote;
pub mod review;
pub mod task;
pub mod url;

use std::collections::HashMap;

use crate::effect::{Effect, Tag};
use crate::git::branches::BranchName;
use crate::model::pull_request::refresh_disabled;
use crate::model::{AppState, Context};

/// A host the user lets Kika contact, checked when the config is read:
/// lower-case ASCII DNS labels joined by dots, nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeHost(String);

impl ForgeHost {
    /// `text` as a host, or why it is not one. No scheme, port, userinfo,
    /// path, wildcard, trailing dot or non-ASCII byte: anything looser
    /// could match more than the user meant.
    pub fn parse(text: &str) -> Result<Self, String> {
        let bad = |why: &str| Err(format!("{text:?} is not a host name: {why}"));
        if text.is_empty() || text.len() > 253 {
            return bad("it must hold 1 to 253 characters");
        }
        if !text.is_ascii() {
            return bad("write an international name in its xn-- form");
        }
        if text.bytes().any(|b| b.is_ascii_uppercase()) {
            return bad("use lower case");
        }
        let label_ok = |label: &str| {
            (1..=63).contains(&label.len())
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        };
        if !text.split('.').all(label_ok) {
            return bad("only letters, digits, - and dots between labels");
        }
        Ok(Self(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The forge repository a remote points at. Only these three parts of
/// the URL are ever kept: never its userinfo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeRepo {
    pub host: String,
    pub owner: String,
    pub name: String,
}

/// What a pull request shows beside its branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrBadge {
    pub number: u64,
    pub state: PrState,
    /// `None` when the PR's head has no checks.
    pub ci: Option<CiState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    Open,
    Draft,
    Merged,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiState {
    Success,
    Failure,
    Pending,
}

/// Why pull requests could not be loaded. A fixed set, so nothing the
/// forge or `gh` says is ever kept or drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgeStatus {
    NoGh,
    NotLoggedIn,
    Offline,
    NoAccess,
    Failed,
    /// GitHub refuses calls for now; none is made until the limit resets.
    RateLimited,
}

/// Why a pull request was not created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateError {
    Status(ForgeStatus),
    /// GitHub refuses calls until this second; none is made before it.
    RateLimited {
        until: i64,
    },
    /// GitHub answered with a refusal, such as a 422, in its own words:
    /// terminal-safe and cut short.
    Refused(String),
}

impl CreateError {
    /// What the error popup says.
    pub fn message(&self) -> String {
        match self {
            CreateError::Status(ForgeStatus::NoGh) => "The gh command is not installed.".into(),
            CreateError::Status(ForgeStatus::NotLoggedIn) => {
                "gh is not logged in to this host; run gh auth login.".into()
            }
            CreateError::Status(ForgeStatus::Offline) => {
                "GitHub could not be reached, or did not answer in time. If it timed out, \
                 the pull request may still have been made: fetch to read the badges again."
                    .into()
            }
            CreateError::Status(ForgeStatus::NoAccess) => {
                "No access to the repository with gh's login.".into()
            }
            CreateError::Status(ForgeStatus::RateLimited) | CreateError::RateLimited { .. } => {
                "GitHub is rate limiting this account.".into()
            }
            CreateError::Status(ForgeStatus::Failed) => "GitHub gave no usable answer.".into(),
            CreateError::Refused(message) => message.clone(),
        }
    }
}

/// What the Branches panel says about the forge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgeView {
    /// Off, or not known yet: nothing is said.
    Off,
    /// Badges come from the cache and no refresh runs.
    Cached,
    Updating,
    Fresh,
    Problem(ForgeStatus),
}

/// Pull requests by local branch, for the repository open now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeState {
    /// From the global config at startup; empty means off.
    pub hosts: Vec<ForgeHost>,
    /// Known once the remote matched one of `hosts`.
    pub repo: Option<ForgeRepo>,
    pub badges: HashMap<BranchName, PrBadge>,
    pub status: ForgeView,
    /// A `gh` call runs. Only one at a time.
    pub in_flight: bool,
    /// The next load asks the forge, not only the cache: the first one for
    /// a repository, and the one after a user fetch, pull or push.
    pub refresh_wanted: bool,
    /// The latest load asked for; answers to older ones are dropped.
    pub seq: Option<u64>,
    /// The local branches last listed, UTF-8 names only.
    pub branches: Vec<BranchName>,
    /// The last load found the remote on no configured host.
    pub unmatched: bool,
    /// Per configured host, the second its rate limit resets. Kept across
    /// repositories, since the limit is the account's, and in memory only.
    pub limited: HashMap<String, i64>,
    /// When the current repository's host may be asked again, for the
    /// status line.
    pub limited_until: Option<i64>,
    /// Pull requests Kika created, shown at once and kept over the cache
    /// and over answers to loads started before them; each with the first
    /// load sequence number that knows of it.
    pub created: HashMap<BranchName, (PrBadge, u64)>,
}

impl Default for ForgeState {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl ForgeState {
    pub fn new(hosts: Vec<ForgeHost>) -> Self {
        Self {
            hosts,
            repo: None,
            badges: HashMap::new(),
            status: ForgeView::Off,
            in_flight: false,
            refresh_wanted: true,
            seq: None,
            branches: Vec::new(),
            unmatched: false,
            limited: HashMap::new(),
            limited_until: None,
            created: HashMap::new(),
        }
    }

    pub fn enabled(&self) -> bool {
        !self.hosts.is_empty()
    }

    /// Back to nothing known, for another repository. The hosts and
    /// their rate limits stay.
    pub fn reset(&mut self) {
        let limited = std::mem::take(&mut self.limited);
        *self = Self::new(std::mem::take(&mut self.hosts));
        self.limited = limited;
    }

    /// Remember the listed branches; names that are not UTF-8 cannot be
    /// sent to the forge and get no badge.
    pub fn set_branches<'a>(&mut self, names: impl IntoIterator<Item = &'a BranchName>) {
        self.branches = names
            .into_iter()
            .filter(|name| std::str::from_utf8(&name.0).is_ok())
            .cloned()
            .collect();
    }

    /// The load to start under `tag`, or none: when off, or while a call
    /// runs (a wanted refresh then waits for it to end).
    pub fn load(&mut self, tag: Tag) -> Option<Effect> {
        // A repository on no configured host has nothing cached either;
        // only a refresh looks at its remote again.
        if !self.enabled() || self.in_flight || (self.unmatched && !self.refresh_wanted) {
            return None;
        }
        let refresh = std::mem::take(&mut self.refresh_wanted);
        if refresh {
            self.in_flight = true;
            self.status = ForgeView::Updating;
        }
        self.seq = Some(tag.seq);
        Some(Effect::LoadPullRequests {
            tag,
            hosts: self.hosts.clone(),
            branches: self.branches.clone(),
            refresh,
            // A refresh reads the remote again, in case its URL changed.
            known: self.repo.clone().filter(|_| !refresh),
            limited: self
                .hosts
                .iter()
                .filter_map(|host| Some((host.clone(), *self.limited.get(host.as_str())?)))
                .collect(),
        })
    }

    fn current(&self, tag: Tag) -> bool {
        self.seq == Some(tag.seq)
    }

    /// The remote matched none of the hosts: show nothing, as when off.
    pub fn repo_none(&mut self, tag: Tag) {
        if !self.current(tag) {
            return;
        }
        self.repo = None;
        self.badges.clear();
        self.status = ForgeView::Off;
        self.in_flight = false;
        self.unmatched = true;
    }

    /// The cache's badges, shown at once. A miss keeps what is shown.
    pub fn cached(
        &mut self,
        tag: Tag,
        repo: ForgeRepo,
        badges: Option<HashMap<BranchName, PrBadge>>,
    ) {
        if !self.current(tag) {
            return;
        }
        self.repo = Some(repo);
        self.unmatched = false;
        if let Some(badges) = badges {
            self.badges = badges;
            self.show_created();
        }
        if !self.in_flight && self.status == ForgeView::Off {
            self.status = ForgeView::Cached;
        }
    }

    /// The forge answered. A failure keeps the badges shown and only says
    /// why in the status line: it never interrupts local work. A rate
    /// limit is remembered for the host until `limited_until`; an answer
    /// clears it.
    pub fn fetched(
        &mut self,
        tag: Tag,
        result: Result<HashMap<BranchName, PrBadge>, ForgeStatus>,
        limited_until: Option<i64>,
    ) {
        if !self.current(tag) {
            return;
        }
        self.in_flight = false;
        let host = self.repo.as_ref().map(|r| r.host.to_ascii_lowercase());
        self.limited_until = limited_until;
        match result {
            Ok(badges) => {
                self.badges = badges;
                // A load started after the create answers for it.
                self.created
                    .retain(|_, (_, known_from)| tag.seq < *known_from);
                self.show_created();
                self.status = ForgeView::Fresh;
                if let Some(host) = host {
                    self.limited.remove(&host);
                }
            }
            Err(status) => {
                self.status = ForgeView::Problem(status);
                if let (Some(host), Some(until)) = (host, limited_until) {
                    self.limited.insert(host, until);
                }
            }
        }
    }

    /// Show the pull request just created for `branch` at once. Loads
    /// numbered `known_from` or later answer for it; older ones and the
    /// cache do not know it yet, so it is kept over what they say.
    pub fn add_created(&mut self, branch: BranchName, badge: PrBadge, known_from: u64) {
        self.badges.insert(branch.clone(), badge);
        self.created.insert(branch, (badge, known_from));
    }

    fn show_created(&mut self) {
        for (branch, (badge, _)) in &self.created {
            self.badges.insert(branch.clone(), *badge);
        }
    }

    /// Ask the forge again on the next load.
    pub fn want_refresh(&mut self) {
        if self.enabled() {
            self.refresh_wanted = true;
        }
    }
}

/// The forge's part of `update`.
impl AppState {
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
        self.forge.load(tag).into_iter().collect()
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
pub(crate) mod tests {
    use super::*;
    use crate::git::FileEntry;
    use crate::git::branches::{Branch, Track};
    use crate::git::remote::RemoteOp;
    use crate::git::status::{BranchStatus, EntryKind, Head};
    use crate::intent::AppIntent;
    use crate::layout::SidePanel;
    use crate::model::{CommitFlow, Context, Focus};

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

    /// A workspace on github.com with `branches` listed, the Branches
    /// panel shown, and no load started.
    pub(crate) fn forge_state(branches: &[BranchName]) -> AppState {
        let mut state = AppState::new(Context::Workspace).with_forge_hosts(github());
        let listed = branches
            .iter()
            .map(|b| branch(&String::from_utf8_lossy(&b.0)))
            .collect();
        state.branches.loaded(Ok(listed));
        state.forge.set_branches(branches);
        state.focus = Focus::Branches;
        state.expanded = SidePanel::Branches;
        state.files_loaded = true;
        state
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
        effects.extend(state.update(AppIntent::BranchesLoaded {
            tag,
            result: Ok(names.iter().map(|n| branch(n)).collect()),
        }));
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
        let finished = state.update(AppIntent::RemoteFinished {
            tag,
            op,
            result: Ok(()),
        });
        let tag = branches_tag(&finished);
        effects.extend(finished);
        effects.extend(state.update(AppIntent::BranchesLoaded {
            tag,
            result: Ok(vec![branch("main")]),
        }));
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
        let tmp = crate::git::fake::TempDir::new("forge-default");
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
        effects.extend(remote_ok(&mut state, AppIntent::Fetch, RemoteOp::Fetch));
        let push = RemoteOp::Push {
            force_with_lease: false,
        };
        effects.extend(remote_ok(&mut state, AppIntent::Push, push));
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

        state.update(AppIntent::PullRequestsFetched {
            tag: first,
            result: Ok(HashMap::new()),
            limited_until: None,
        });
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
        let effects = remote_ok(&mut state, AppIntent::Push, push);
        assert_eq!(loads(&effects), []);
        let effects = state.update(AppIntent::PullRequestsFetched {
            tag: first,
            result: Ok(HashMap::new()),
            limited_until: None,
        });
        let [(second, true)] = loads(&effects)[..] else {
            panic!("{effects:?}")
        };
        state.update(AppIntent::PullRequestsFetched {
            tag: second,
            result: Ok(HashMap::new()),
            limited_until: None,
        });

        for (intent, op) in [
            (AppIntent::Fetch, RemoteOp::Fetch),
            (AppIntent::Pull, RemoteOp::Pull),
        ] {
            let effects = remote_ok(&mut state, intent, op);
            let [(tag, true)] = loads(&effects)[..] else {
                panic!("{effects:?}")
            };
            state.update(AppIntent::PullRequestsFetched {
                tag,
                result: Ok(HashMap::new()),
                limited_until: None,
            });
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
        state.update(AppIntent::PullRequestsFetched {
            tag: old,
            result: Ok(HashMap::from([("main".into(), badge(9))])),
            limited_until: None,
        });
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
        state.update(AppIntent::PullRequestsCached {
            tag,
            repo: repo(),
            badges: None,
        });
        state.update(AppIntent::PullRequestsFetched {
            tag,
            result: Ok(HashMap::new()),
            limited_until: None,
        });
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
        let effects = remote_ok(&mut state, AppIntent::Fetch, RemoteOp::Fetch);
        let (tag, _) = loads(&effects)[0];
        assert_eq!(known(&effects), Some((None, true)));

        // Once the remote matched no host, reloads start nothing, until a
        // user fetch asks again.
        state.update(AppIntent::ForgeRepoNone { tag });
        assert_eq!(loads(&list_branches(&mut state, &["main"])), []);
        let effects = remote_ok(&mut state, AppIntent::Fetch, RemoteOp::Fetch);
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
        state.update(AppIntent::PullRequestsCached {
            tag,
            repo: ForgeRepo {
                host: "GitHub.com".into(),
                ..repo()
            },
            badges: Some(shown.clone()),
        });
        let effects = state.update(AppIntent::PullRequestsFetched {
            tag,
            result: Err(ForgeStatus::RateLimited),
            limited_until: Some(T),
        });
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
        assert_eq!(state.op_error, None);
        assert!(state.popups.is_empty());

        // A user fetch hands the limit to the task, which decides by the clock.
        let effects = remote_ok(&mut state, AppIntent::Fetch, RemoteOp::Fetch);
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
        state.update(AppIntent::PullRequestsCached {
            tag: tag2,
            repo: repo(),
            badges: None,
        });
        state.update(AppIntent::PullRequestsFetched {
            tag: tag2,
            result: Ok(HashMap::new()),
            limited_until: None,
        });
        assert!(state.forge.limited.is_empty());
        assert_eq!(state.forge.limited_until, None);
        let effects = remote_ok(&mut state, AppIntent::Fetch, RemoteOp::Fetch);
        assert_eq!(limited_of(&effects), []);
    }

    #[test]
    fn a_remote_on_no_configured_host_shows_nothing() {
        let mut state = AppState::new(Context::Workspace).with_forge_hosts(github());
        let [(tag, true)] = loads(&list_branches(&mut state, &["main"]))[..] else {
            panic!()
        };
        state.update(AppIntent::ForgeRepoNone { tag });
        assert_eq!(state.forge.status, ForgeView::Off);
        assert!(!state.forge.in_flight);
        let rows = crate::branches_view::tests::draw_branches(&state, 80, 12);
        assert!(rows.iter().all(|r| !r.contains("GitHub")), "{rows:?}");
    }

    #[test]
    fn cached_badges_draw_while_the_refresh_still_runs() {
        let names: Vec<BranchName> = vec!["main".into(), "topic".into()];
        let mut state = forge_state(&names);
        let [(tag, true)] = loads(&state.load_pull_requests())[..] else {
            panic!()
        };
        state.update(AppIntent::PullRequestsCached {
            tag,
            repo: repo(),
            badges: Some(HashMap::from([("topic".into(), badge(12))])),
        });
        assert_eq!(state.forge.status, ForgeView::Updating);
        let rows = crate::branches_view::tests::draw_branches(&state, 80, 12);
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
        state.update(AppIntent::PullRequestsCached {
            tag,
            repo: repo(),
            badges: Some(HashMap::from([("main".into(), badge(3))])),
        });
        let effects = state.update(AppIntent::PullRequestsFetched {
            tag,
            result: Err(ForgeStatus::Offline),
            limited_until: None,
        });
        assert_eq!(effects, []);
        assert_eq!(state.forge.badges[&BranchName::from("main")], badge(3));
        assert_eq!(state.forge.status, ForgeView::Problem(ForgeStatus::Offline));
        assert_eq!(state.op_error, None);
        assert!(state.popups.is_empty());
        let rows = crate::branches_view::tests::draw_branches(&state, 80, 12);
        assert!(
            rows.iter().any(|r| r.contains("GitHub: offline · cached")),
            "{rows:#?}"
        );

        // Staging and committing go on as ever.
        state.files = vec![FileEntry {
            path: "a.rs".into(),
            orig_path: None,
            index: '?',
            worktree: '?',
            kind: EntryKind::Untracked,
        }];
        state.selected = Some(0);
        state.focus = Focus::Files;
        state.expanded = SidePanel::Files;
        let staged = state.update(AppIntent::ToggleStage);
        assert!(
            staged.iter().any(|e| matches!(e, Effect::RunOp { .. })),
            "{staged:?}"
        );
        state.op_seq = None;
        state.update(AppIntent::OpenCommit);
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
        state.update(AppIntent::PullRequestsCached {
            tag: old,
            repo: repo(),
            badges: Some(HashMap::from([("main".into(), badge(1))])),
        });
        state.update(AppIntent::ForgeRepoNone { tag: old });
        state.update(AppIntent::PullRequestsFetched {
            tag: old,
            result: Err(ForgeStatus::Failed),
            limited_until: None,
        });
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
        let effects = state.update(AppIntent::RefreshPullRequests);
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
        let effects = state.update(AppIntent::PullRequestsFetched {
            tag,
            result: Ok(HashMap::new()),
            limited_until: None,
        });
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
        assert_eq!(off.update(AppIntent::RefreshPullRequests), []);

        let mut unknown = loaded();
        unknown.forge.repo = None;
        assert_eq!(press_refresh(&mut unknown), []);
        assert_eq!(
            unknown.notice.as_deref(),
            Some("The remote is on no forge host in the global config")
        );

        let mut unmatched = loaded();
        let [(tag, true)] = loads(&unmatched.update(AppIntent::RefreshPullRequests))[..] else {
            panic!()
        };
        unmatched.update(AppIntent::ForgeRepoNone { tag });
        assert!(unmatched.forge.unmatched);
        assert_eq!(unmatched.update(AppIntent::RefreshPullRequests), []);
        assert_eq!(
            unmatched.notice.as_deref(),
            Some("The remote is on no forge host in the global config")
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
    fn the_refresh_key_does_nothing_outside_the_workspace() {
        let mut state = loaded();
        state.context = Context::Review { number: None };
        assert_eq!(state.update(AppIntent::RefreshPullRequests), []);
        assert!(!state.forge.in_flight);
    }

    #[test]
    fn names_that_are_not_utf8_are_never_sent() {
        let mut forge = ForgeState::new(github());
        let odd = BranchName::from_bytes(b"caf\xe9");
        forge.set_branches([&BranchName::from("main"), &odd]);
        assert_eq!(forge.branches, [BranchName::from("main")]);
    }

    #[test]
    fn a_host_is_lower_case_ascii_labels_and_nothing_else() {
        for good in [
            "github.com",
            "github.example.com",
            "xn--bcher-kva.example",
            "localhost",
        ] {
            assert_eq!(ForgeHost::parse(good).unwrap().as_str(), good);
        }
        for bad in [
            "",
            "GitHub.com",
            "github.com:443",
            "user@github.com",
            "https://github.com",
            "*.github.com",
            "github.com.",
            "github.com/o",
            "gіthub.com",
            ".github.com",
            "git..hub.com",
            "-github.com",
            "git_hub.com",
        ] {
            assert!(ForgeHost::parse(bad).is_err(), "{bad:?}");
        }
    }
}
