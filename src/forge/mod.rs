//! Pull requests from a forge, shown beside the local branches. Off unless
//! the global config names the hosts Kika may contact: with none, no
//! effect is ever asked for, so nothing reaches the network.
//!
//! GitHub is reached through the `gh` CLI, never an HTTP client of Kika's
//! own, so the user's login, proxy and TLS settings apply and Kika never
//! holds a token.

pub mod allow;
pub mod cache;
pub mod client;
pub mod github;
pub mod head;
pub mod merge;
pub mod remote;
pub mod review;
pub mod task;
pub mod url;

use std::collections::HashMap;

use crate::effect::{Effect, Tag};
use crate::git::branches::BranchName;
use allow::AllowRule;

/// A host the user lets Kika contact, checked when the config is read:
/// lower-case ASCII DNS labels joined by dots, nothing else. It carries
/// its `[forge.allow]` list, so every path that may call `gh` for a
/// repository on it can ask [`ForgeHost::allows`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeHost {
    name: String,
    /// `None` allows every repository on the host; an empty list none.
    allow: Option<Vec<AllowRule>>,
}

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
        Ok(Self {
            name: text.to_string(),
            allow: None,
        })
    }

    /// The host limited to the repositories `rules` allow.
    pub fn with_allow(self, rules: Vec<AllowRule>) -> Self {
        Self {
            allow: Some(rules),
            ..self
        }
    }

    pub fn as_str(&self) -> &str {
        &self.name
    }

    /// Whether `gh` may be asked about `repo` on this host: always without
    /// an allow-list, else only when one of its rules names `repo`.
    pub fn allows(&self, repo: &ForgeRepo) -> bool {
        self.allow
            .as_ref()
            .is_none_or(|rules| rules.iter().any(|rule| rule.allows(repo)))
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

pub(crate) const REFRESH_LIMITED: &str = "GitHub is rate limited; refresh after the limit resets";

/// GitHub compares owners and names without regard to case.
pub(crate) fn same_repo(a: &ForgeRepo, b: &ForgeRepo) -> bool {
    a.host.eq_ignore_ascii_case(&b.host)
        && a.owner.eq_ignore_ascii_case(&b.owner)
        && a.name.eq_ignore_ascii_case(&b.name)
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

    /// What the error popup says when a merge failed. A timeout may hide
    /// a merge that went through, so it says how to find out.
    pub fn merge_message(&self) -> String {
        match self {
            CreateError::Status(ForgeStatus::Offline) => {
                "GitHub could not be reached, or did not answer in time. If it timed out, \
                 the merge may still have happened: refresh with r."
                    .into()
            }
            other => other.message(),
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

    /// GitHub refuses calls to `host` until `until`: no create, merge or
    /// load asks it before then, and the status line says so when it is
    /// the badges' host.
    pub(crate) fn record_rate_limit(&mut self, host: &ForgeHost, until: i64) {
        let host = host.as_str();
        self.limited.insert(host.to_string(), until);
        let here = self.repo.as_ref();
        if here.is_some_and(|r| r.host.eq_ignore_ascii_case(host)) {
            self.limited_until = Some(until);
            self.status = ForgeView::Problem(ForgeStatus::RateLimited);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn github() -> Vec<ForgeHost> {
        vec![ForgeHost::parse("github.com").unwrap()]
    }

    #[test]
    fn a_merge_timeout_says_the_merge_may_have_happened() {
        let offline = CreateError::Status(ForgeStatus::Offline);
        assert!(
            offline
                .merge_message()
                .contains("merge may still have happened")
        );
        assert!(offline.merge_message().contains("refresh with r"));
        let refused = CreateError::Refused("Head branch was modified".into());
        assert_eq!(refused.merge_message(), "Head branch was modified");
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
