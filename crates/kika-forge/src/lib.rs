//! Pull requests from a forge, shown beside the local branches. Off unless
//! the global config names the hosts Kika may contact: with none, no
//! effect is ever asked for, so nothing reaches the network.
//!
//! GitHub is reached through the `gh` CLI and GitLab through `glab`, never
//! an HTTP client of Kika's own, so the user's login, proxy and TLS
//! settings apply and Kika never holds a token.
//!
//! Every call goes through the one [`forge::Forge`] interface. Each forge
//! kind has one forge adapter ([`forge::GitHubForge`] wraps `gh`,
//! [`forge::GitLabForge`] wraps `glab`), and a forge host's kind picks it.

pub mod allow;
pub mod anchor;
pub mod cache;
pub mod cli;
pub mod client;
pub mod discussion;
pub mod discussion_write;
pub mod forge;
pub mod github;
mod github_forge;
pub mod gitlab;
pub mod gitlab_discussion;
pub mod gitlab_discussion_write;
mod gitlab_forge;
pub mod gitlab_merge;
pub mod gitlab_review;
pub mod gitlab_submit;
pub mod head;
pub mod merge;
pub mod pending;
pub mod remote;
pub mod review;
pub mod task;
pub mod url;
pub mod viewed;

use std::collections::HashMap;

use allow::AllowRule;
use cli::CliStatus;
use kika_git::branches::BranchName;
use url::ForgeKind;

/// A host the user lets Kika contact, checked when the config is read:
/// lower-case ASCII DNS labels joined by dots, nothing else. It carries
/// its `[forge.allow]` list, so every path that may call `gh` for a
/// repository on it can ask [`ForgeHost::allows`], and its kind, so only
/// the matching forge's CLI is ever asked about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeHost {
    name: String,
    /// `None` allows every repository on the host; an empty list none.
    allow: Option<Vec<AllowRule>>,
    kind: ForgeKind,
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
            kind: ForgeKind::GitHub,
        })
    }

    /// `text` as a GitLab host, checked as [`Self::parse`] checks it: what
    /// the config's `gitlab_ask` list gives, so `glab` is asked about it.
    pub fn parse_gitlab(text: &str) -> Result<Self, String> {
        Ok(Self::parse(text)?.with_kind(ForgeKind::GitLab))
    }

    /// The host as a `kind` forge; [`Self::parse`] makes a GitHub one.
    pub fn with_kind(self, kind: ForgeKind) -> Self {
        Self { kind, ..self }
    }

    /// Which forge the host is, so which CLI may be asked about it.
    pub fn kind(&self) -> ForgeKind {
        self.kind
    }

    /// Whether a repository on this host may sit in nested groups
    /// (`group/sub/project`): GitLab's may, GitHub's are `owner/repo`.
    pub fn nests(&self) -> bool {
        self.kind == ForgeKind::GitLab
    }

    /// `text` as one of this host's `[forge.allow]` entries: a group path
    /// on a host that [`nests`](Self::nests), else an owner or `owner/repo`.
    pub fn allow_rule(&self, text: &str) -> Result<AllowRule, String> {
        if self.nests() {
            AllowRule::parse_nested(text)
        } else {
            AllowRule::parse(text)
        }
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
    /// The GitHub owner, or on GitLab the whole group path (`acme/sub`).
    pub owner: String,
    /// The repository: the path's last part, never holding a `/`.
    pub name: String,
}

/// The review callers' notice; review runs on GitHub only.
pub const REFRESH_LIMITED: &str = "GitHub is rate limited; refresh after the limit resets";

/// Why a refresh of `kind`'s badges is held back: its host is rate limited.
pub fn refresh_limited(kind: ForgeKind) -> String {
    format!(
        "{} is rate limited; refresh after the limit resets",
        kind.name()
    )
}

/// GitHub compares owners and names without regard to case.
pub fn same_repo(a: &ForgeRepo, b: &ForgeRepo) -> bool {
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
    /// The forge's CLI (`gh` or `glab`) is missing, broken or too old;
    /// never `Ready`. No call is made for its forge kind meanwhile.
    Cli(CliStatus),
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
    /// The forge kind cannot do this yet; nothing was asked.
    Unsupported(forge::Unsupported),
}

impl CreateError {
    /// What the error popup says.
    pub fn message(&self) -> String {
        self.message_in(forge::words_or_default(None))
    }

    /// What the error popup says, naming the forge and its CLI in
    /// `words`.
    pub fn message_in(&self, words: &forge::Words) -> String {
        let forge::Words {
            name,
            cli: tool,
            request,
            ..
        } = words;
        match self {
            CreateError::Status(ForgeStatus::Cli(status)) => cli::problem_text(*status),
            CreateError::Status(ForgeStatus::NotLoggedIn) => {
                format!("{tool} is not logged in to this host; run {tool} auth login.")
            }
            CreateError::Status(ForgeStatus::Offline) => format!(
                "{name} could not be reached, or did not answer in time. If it timed out, \
                 the {request} may still have been made: fetch to read the badges again."
            ),
            CreateError::Status(ForgeStatus::NoAccess) => {
                format!("No access to the repository with {tool}'s login.")
            }
            CreateError::Status(ForgeStatus::RateLimited) | CreateError::RateLimited { .. } => {
                format!("{name} is rate limiting this account.")
            }
            CreateError::Status(ForgeStatus::Failed) => format!("{name} gave no usable answer."),
            CreateError::Refused(message) => message.clone(),
            CreateError::Unsupported(unsupported) => unsupported.message(),
        }
    }

    /// What the error popup says when a merge failed, naming the forge in
    /// `words`. A timeout, or an answer that does not say it merged, may
    /// hide a merge that went through, so it says how to find out.
    pub fn merge_message_in(&self, words: &forge::Words) -> String {
        let name = words.name;
        match self {
            CreateError::Status(ForgeStatus::Offline) => format!(
                "{name} could not be reached, or did not answer in time. If it timed out, \
                 the merge may still have happened: refresh with r."
            ),
            CreateError::Status(ForgeStatus::Failed) => format!(
                "{name} gave no usable answer. The merge may still have happened: refresh with r."
            ),
            other => other.message_in(words),
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

/// The number of a pull-request load. Answers to any load but the
/// latest are dropped; the caller decides what a number belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadSeq(pub u64);

/// What one load needs to run, as [`ForgeState::load`] decided it. The
/// caller turns it into its own effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadRequest {
    pub hosts: Vec<ForgeHost>,
    pub branches: Vec<BranchName>,
    /// Ask the forge, not only the cache.
    pub refresh: bool,
    /// The repository an earlier load found; the remote is then not read.
    pub known: Option<ForgeRepo>,
    /// Hosts rate limited until the given second; not asked before it.
    pub limited: Vec<(ForgeHost, i64)>,
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
    /// Each opted-in forge kind's CLI check; `None` until the first one
    /// answers, and until then no refresh asks the forge. Kept across
    /// repositories, as the CLI is the machine's.
    pub cli: Option<HashMap<ForgeKind, CliStatus>>,
    /// A CLI check runs.
    pub checking: bool,
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
            cli: None,
            checking: false,
        }
    }

    pub fn enabled(&self) -> bool {
        !self.hosts.is_empty()
    }

    /// The kind of the host [`Self::repo`] is on, once a load found it:
    /// how its badges and status line are worded.
    pub fn kind(&self) -> Option<ForgeKind> {
        self.kind_of(self.repo.as_ref()?)
    }

    /// The kind of the configured host `repo` is on, if any: for a
    /// repository found by another load than the badges'.
    pub fn kind_of(&self, repo: &ForgeRepo) -> Option<ForgeKind> {
        remote::matches(repo, &self.hosts).map(ForgeHost::kind)
    }

    /// Back to nothing known, for another repository. The hosts, their
    /// rate limits and the CLI checks stay.
    pub fn reset(&mut self) {
        let limited = std::mem::take(&mut self.limited);
        let cli = self.cli.take();
        let checking = self.checking;
        *self = Self::new(std::mem::take(&mut self.hosts));
        self.limited = limited;
        self.cli = cli;
        self.checking = checking;
    }

    /// The CLI check answered: a held-back refresh may now run.
    pub fn cli_checked(&mut self, statuses: impl IntoIterator<Item = (ForgeKind, CliStatus)>) {
        self.cli = Some(statuses.into_iter().collect());
        self.checking = false;
    }

    /// What is wrong with the CLI of the current repository's forge kind,
    /// when its last check was not `Ready`.
    pub fn cli_problem(&self) -> Option<CliStatus> {
        let kind = self.kind()?;
        let status = *self.cli.as_ref()?.get(&kind)?;
        (status != CliStatus::Ready).then_some(status)
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

    /// The load to start as `seq`, or none: when off, or while a call
    /// runs (a wanted refresh then waits for it to end). Until the first
    /// CLI check answers, a load only reads the cache and the wanted
    /// refresh waits for the check.
    pub fn load(&mut self, seq: impl Into<LoadSeq>) -> Option<LoadRequest> {
        // A repository on no configured host has nothing cached either;
        // only a refresh looks at its remote again.
        if !self.enabled() || self.in_flight || (self.unmatched && !self.refresh_wanted) {
            return None;
        }
        let refresh = self.refresh_wanted && self.cli.is_some();
        if refresh {
            self.refresh_wanted = false;
            self.in_flight = true;
            self.status = ForgeView::Updating;
        }
        self.seq = Some(seq.into().0);
        Some(LoadRequest {
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

    fn current(&self, seq: LoadSeq) -> bool {
        self.seq == Some(seq.0)
    }

    /// The remote matched none of the hosts: show nothing, as when off.
    pub fn repo_none(&mut self, seq: impl Into<LoadSeq>) {
        if !self.current(seq.into()) {
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
        seq: impl Into<LoadSeq>,
        repo: ForgeRepo,
        badges: Option<HashMap<BranchName, PrBadge>>,
    ) {
        if !self.current(seq.into()) {
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
        seq: impl Into<LoadSeq>,
        result: Result<HashMap<BranchName, PrBadge>, ForgeStatus>,
        limited_until: Option<i64>,
    ) {
        let seq = seq.into();
        if !self.current(seq) {
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
                    .retain(|_, (_, known_from)| seq.0 < *known_from);
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
    pub fn record_rate_limit(&mut self, host: &ForgeHost, until: i64) {
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
    fn an_error_names_the_forge_and_cli_it_came_from() {
        let gitlab = forge::words(url::ForgeKind::GitLab);
        let say = |e: CreateError| (e.message(), e.message_in(gitlab));
        assert_eq!(
            say(CreateError::Status(ForgeStatus::NotLoggedIn)),
            (
                "gh is not logged in to this host; run gh auth login.".into(),
                "glab is not logged in to this host; run glab auth login.".into()
            )
        );
        assert_eq!(
            say(CreateError::RateLimited { until: 1 }),
            (
                "GitHub is rate limiting this account.".into(),
                "GitLab is rate limiting this account.".into()
            )
        );
        assert_eq!(
            say(CreateError::Status(ForgeStatus::Failed)).1,
            "GitLab gave no usable answer."
        );
        assert_eq!(
            say(CreateError::Status(ForgeStatus::NoAccess)).1,
            "No access to the repository with glab's login."
        );
        assert!(
            say(CreateError::Status(ForgeStatus::Offline))
                .1
                .contains("the merge request may still")
        );
    }

    fn widgets() -> ForgeRepo {
        ForgeRepo {
            host: "github.com".into(),
            owner: "acme".into(),
            name: "widgets".into(),
        }
    }

    #[test]
    fn a_load_asks_for_what_the_state_knows_and_drops_stale_answers() {
        let mut forge = ForgeState::new(github());
        forge.cli_checked([]);
        forge.set_branches([&BranchName::from("main")]);
        forge.limited.insert("github.com".into(), 99);

        // The first load refreshes: the remote is read again, a call runs.
        let request = forge.load(LoadSeq(1)).unwrap();
        assert_eq!(
            request,
            LoadRequest {
                hosts: github(),
                branches: vec!["main".into()],
                refresh: true,
                known: None,
                limited: vec![(github()[0].clone(), 99)],
            }
        );
        assert!(forge.in_flight);
        assert_eq!(forge.status, ForgeView::Updating);
        assert_eq!(forge.seq, Some(1));
        // While it runs, no other load starts.
        assert_eq!(forge.load(LoadSeq(2)), None);
        assert_eq!(forge.seq, Some(1));

        forge.cached(LoadSeq(1), widgets(), None);
        forge.fetched(LoadSeq(1), Ok(HashMap::new()), None);
        assert!(!forge.in_flight);
        assert_eq!(forge.status, ForgeView::Fresh);

        // A later load that is no refresh names the repository found and
        // leaves the status as it is.
        let request = forge.load(LoadSeq(2)).unwrap();
        assert!(!request.refresh);
        assert_eq!(request.known, Some(widgets()));
        assert!(!forge.in_flight);
        assert_eq!(forge.status, ForgeView::Fresh);
        assert_eq!(forge.seq, Some(2));

        // Answers to the older load change nothing.
        let before = forge.clone();
        forge.repo_none(LoadSeq(1));
        forge.cached(
            LoadSeq(1),
            ForgeRepo {
                name: "other".into(),
                ..widgets()
            },
            None,
        );
        forge.fetched(LoadSeq(1), Err(ForgeStatus::Failed), Some(5));
        assert_eq!(forge, before);
    }

    #[test]
    fn until_the_first_cli_check_answers_a_load_only_reads_the_cache() {
        let mut forge = ForgeState::new(github());
        let request = forge.load(LoadSeq(1)).unwrap();
        assert!(!request.refresh);
        assert!(forge.refresh_wanted && !forge.in_flight);
        assert_eq!(forge.status, ForgeView::Off);

        forge.checking = true;
        forge.cli_checked([(ForgeKind::GitHub, CliStatus::NotInstalled)]);
        assert!(!forge.checking);
        // Not known until a repository matched a host.
        assert_eq!(forge.cli_problem(), None);
        forge.repo = Some(widgets());
        assert_eq!(forge.cli_problem(), Some(CliStatus::NotInstalled));
        // The refresh it held back runs now; the client's gate answers it.
        assert!(forge.load(LoadSeq(2)).unwrap().refresh);

        // Another repository keeps the check, as the CLI is the machine's.
        forge.reset();
        assert_eq!(
            forge.cli,
            Some(HashMap::from([(
                ForgeKind::GitHub,
                CliStatus::NotInstalled
            )]))
        );
        forge.cli_checked([(ForgeKind::GitHub, CliStatus::Ready)]);
        forge.repo = Some(widgets());
        assert_eq!(forge.cli_problem(), None);
    }

    #[test]
    fn a_merge_timeout_says_the_merge_may_have_happened() {
        let github = forge::words_or_default(None);
        let gitlab = forge::words(ForgeKind::GitLab);
        let offline = CreateError::Status(ForgeStatus::Offline);
        assert!(
            offline
                .merge_message_in(github)
                .starts_with("GitHub could not be reached")
        );
        let text = offline.merge_message_in(gitlab);
        assert!(text.starts_with("GitLab could not be reached"), "{text}");
        assert!(text.contains("merge may still have happened"), "{text}");
        assert!(text.contains("refresh with r"), "{text}");
        // An answer that does not say it merged.
        let failed = CreateError::Status(ForgeStatus::Failed).merge_message_in(gitlab);
        assert!(
            failed.starts_with("GitLab gave no usable answer"),
            "{failed}"
        );
        assert!(failed.contains("refresh with r"), "{failed}");
        let refused = CreateError::Refused("Head branch was modified".into());
        assert_eq!(refused.merge_message_in(gitlab), "Head branch was modified");
        let logged_out = CreateError::Status(ForgeStatus::NotLoggedIn);
        assert!(logged_out.merge_message_in(gitlab).starts_with("glab "));
    }

    #[test]
    fn only_a_gitlab_host_nests_and_takes_group_paths_as_allow_entries() {
        let lab = ForgeHost::parse_gitlab("gitlab.example.com").unwrap();
        assert!(lab.nests() && !github()[0].nests());
        assert!(lab.allow_rule("acme/sub/x").is_ok());
        assert!(github()[0].allow_rule("acme/sub/x").is_err());
        assert_eq!(github()[0].allow_rule("acme/x"), AllowRule::parse("acme/x"));
        assert!(lab.allow_rule("-").is_err());
        // A nested repository on that host gets its kind.
        let mut forge = ForgeState::new(vec![github()[0].clone(), lab]);
        forge.repo = Some(ForgeRepo {
            host: "gitlab.example.com".into(),
            owner: "acme/sub".into(),
            name: "x".into(),
        });
        assert_eq!(forge.kind(), Some(ForgeKind::GitLab));
    }

    #[test]
    fn the_kind_is_the_matched_hosts_once_a_repository_is_found() {
        let gitlab = ForgeHost::parse("gitlab.example.com")
            .unwrap()
            .with_kind(ForgeKind::GitLab);
        assert_eq!(gitlab.kind(), ForgeKind::GitLab);
        assert_eq!(github()[0].kind(), ForgeKind::GitHub);
        let mut forge = ForgeState::new(vec![github()[0].clone(), gitlab]);
        assert_eq!(forge.kind(), None);
        forge.repo = Some(widgets());
        assert_eq!(forge.kind(), Some(ForgeKind::GitHub));
        forge.repo = Some(ForgeRepo {
            host: "GitLab.example.com".into(),
            ..widgets()
        });
        assert_eq!(forge.kind(), Some(ForgeKind::GitLab));
        forge.repo = Some(ForgeRepo {
            host: "elsewhere.example".into(),
            ..widgets()
        });
        assert_eq!(forge.kind(), None);
    }

    #[test]
    fn a_gitlab_host_parses_as_any_host_and_says_its_kind() {
        let host = ForgeHost::parse_gitlab("gitlab.example.com").unwrap();
        assert_eq!(host.kind(), ForgeKind::GitLab);
        assert_eq!(host.as_str(), "gitlab.example.com");
        assert_eq!(
            ForgeHost::parse_gitlab("GitLab.com"),
            ForgeHost::parse("GitLab.com")
        );
    }

    #[test]
    fn the_refresh_notice_names_the_forge() {
        assert_eq!(refresh_limited(ForgeKind::GitHub), REFRESH_LIMITED);
        assert_eq!(
            refresh_limited(ForgeKind::GitLab),
            "GitLab is rate limited; refresh after the limit resets"
        );
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
