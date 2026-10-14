//! One pull-request load, run off the UI loop: find the forge repository,
//! report the cache, then ask the forge when told to. Each step is
//! reported as a [`LoadEvent`]; the caller decides what it becomes.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use super::cache::{self, CacheKey, Cached};
use super::github::{Answer, GhRunner, MAX_BRANCHES, Refusal};
use super::gitlab::GlabRunner;
use super::head::{Head, heads};
use super::remote::{allowed, parse_remote};
use super::url::ForgeKind;
use super::{CiState, ForgeHost, ForgeRepo, ForgeStatus, PrBadge, PrState};
use kika_git::GitRunner;
use kika_git::branches::BranchName;
use kika_git::forge_remote::{read_remote_url, read_upstreams};
use kika_git::recording::CommandSink;

/// What the runtime needs to reach a forge. Only `main` builds one with
/// the real `gh` and `glab`; without one every load answers that no
/// repository was found.
#[derive(Clone)]
pub struct ForgeContext {
    pub gh: Arc<dyn GhRunner>,
    /// Only asked about a repository on a GitLab-kind host.
    pub glab: Arc<dyn GlabRunner>,
    pub sink: CommandSink,
    /// `None` when no cache directory is known: nothing is cached.
    pub cache_dir: Option<PathBuf>,
}

/// What one load found, in the order it is found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadEvent {
    /// The remote is on no configured host, or its host's allow-list
    /// leaves it out: nothing is shown.
    RepoNone,
    /// The repository, and its cached badges when the cache has them.
    Cached {
        repo: ForgeRepo,
        badges: Option<HashMap<BranchName, PrBadge>>,
    },
    /// The forge's answer to a refresh; with a rate limit, the second it
    /// resets.
    Fetched {
        result: Result<HashMap<BranchName, PrBadge>, ForgeStatus>,
        limited_until: Option<i64>,
    },
}

/// One load's inputs, as the caller gave them.
pub struct Load<'a> {
    pub hosts: &'a [ForgeHost],
    pub branches: &'a [BranchName],
    pub refresh: bool,
    /// The repository an earlier load found; the remote is then not read.
    pub known: Option<&'a ForgeRepo>,
    /// Hosts rate limited until the given second; not asked before it.
    pub limited: &'a [(ForgeHost, i64)],
    /// The repository's canonical top level, for the cache key.
    pub top: Option<PathBuf>,
    /// Seconds since the epoch, stored with the cache entry and compared
    /// with `limited`.
    pub now: i64,
}

/// Run `load`, sending each event as it is known: `RepoNone`, or
/// `Cached` and then, on a refresh, `Fetched`.
///
/// The host's kind picks the CLI: `gh` for GitHub, `glab` for GitLab.
/// On GitHub a refresh first probes, for free, whether the pull requests
/// changed; when they did not and the cache is still good, no GraphQL
/// query runs. A fork skips the probe: its own list says nothing of its
/// parent's pull requests. GitLab has no probe. A rate-limited host gets
/// no call at all until its limit resets. Each branch is asked about by its head, read from local git.
/// A repository its host's allow-list leaves out answers `RepoNone`.
pub fn run_load(git: &dyn GitRunner, ctx: &ForgeContext, load: &Load, send: &dyn Fn(LoadEvent)) {
    // Defence in depth: with no hosts nothing is read, run or written.
    if load.hosts.is_empty() {
        send(LoadEvent::RepoNone);
        return;
    }
    let found = match load.known {
        Some(repo) => Some(repo.clone()),
        None => read_remote_url(git).and_then(|url| parse_remote(&url)),
    }
    // A repository `[forge.allow]` leaves out is as if on no host: its
    // cache is not read either, so nothing about it is shown.
    .and_then(|repo| Some((allowed(&repo, load.hosts)?, repo)));
    let Some((host, repo)) = found else {
        send(LoadEvent::RepoNone);
        return;
    };
    let key = load
        .top
        .as_deref()
        .and_then(|top| CacheKey::new(&repo, top));
    let place = ctx.cache_dir.as_deref().zip(key.as_ref());
    let cached = place.and_then(|(dir, key)| cache::read(dir, key));
    send(LoadEvent::Cached {
        repo: repo.clone(),
        badges: cached.as_ref().map(|c| c.badges.clone()),
    });
    if !load.refresh {
        return;
    }
    let fetched = |result, limited_until| {
        send(LoadEvent::Fetched {
            result,
            limited_until,
        })
    };
    let limited = |until| fetched(Err(ForgeStatus::RateLimited), Some(until));
    let until = load
        .limited
        .iter()
        .find(|(limited, _)| limited.as_str() == host.as_str())
        .map(|(_, until)| *until);
    if let Some(until) = until.filter(|until| *until > load.now) {
        return limited(until);
    }
    let branches = &load.branches[..load.branches.len().min(MAX_BRANCHES)];
    // The client applies the same allow gate as above, so a `None` is the
    // allow-list miss's answer should the two ever disagree.
    let (found, etag, heads) = match host.kind() {
        ForgeKind::GitHub => {
            let Some(gh) = ctx.gh(host, &repo, load.now) else {
                return send(LoadEvent::RepoNone);
            };
            let heads = heads(branches, &read_upstreams(git), &repo);
            let sent = cached.as_ref().and_then(|c| c.etag.clone());
            let forked = cached.as_ref().is_some_and(|c| c.forked);
            let etag = if forked {
                sent
            } else {
                match gh.probe(&repo, sent.as_deref()) {
                    Answer::Limited { until } => return limited(until),
                    // The cache is left as it is: its `fetched_at` stays the
                    // time of the last GraphQL answer, which bounds how long
                    // 304s may reuse it.
                    Answer::NotModified => match cached.filter(|c| reusable(c, &heads, load.now)) {
                        Some(cached) => return fetched(Ok(cached.badges), None),
                        None => sent,
                    },
                    Answer::Body { etag, .. } => etag,
                    // No worse than without a probe: ask anyway.
                    Answer::Failed(_) => None,
                }
            };
            (gh.fetch_badges(&repo, &heads), etag, heads)
        }
        // GitLab has no probe: each refresh asks GraphQL.
        ForgeKind::GitLab => {
            let Some(glab) = ctx.glab(host, &repo, load.now) else {
                return send(LoadEvent::RepoNone);
            };
            let heads = heads(branches, &read_upstreams(git), &repo);
            (glab.fetch_badges(&repo, &heads), None, heads)
        }
    };
    match found {
        Ok(found) => {
            if let Some((dir, key)) = place {
                let fresh = Cached {
                    badges: found.badges.clone(),
                    etag,
                    queried: heads,
                    forked: found.forked,
                    fetched_at: load.now,
                };
                // A cache that cannot be written only costs the next startup.
                let _ = cache::write(dir, key, &fresh);
            }
            fetched(Ok(found.badges), None);
        }
        Err(Refusal::Limited { until }) => limited(until),
        Err(Refusal::Status(status)) => fetched(Err(status), None),
    }
}

/// How long, in seconds, a GraphQL answer may be reused on 304s alone.
/// CI can finish or re-run without changing the pull-request list, so a
/// green badge is asked about again at least this often.
const MAX_REUSE: i64 = 15 * 60;

/// Whether `cached` still answers for `heads` at `now` when the pull
/// requests did not change: the same heads were asked, less than
/// [`MAX_REUSE`] ago, and no open PR waits on CI, which can finish
/// without changing the pull-request list.
fn reusable(cached: &Cached, heads: &[Head], now: i64) -> bool {
    let waiting = cached.badges.values().any(|badge| {
        matches!(badge.state, PrState::Open | PrState::Draft)
            && matches!(badge.ci, Some(CiState::Pending | CiState::Failure))
    });
    let set = |heads: &[Head]| heads.iter().cloned().collect::<HashSet<_>>();
    // A time ahead of the clock is not trusted either.
    let young = (0..MAX_REUSE).contains(&(now - cached.fetched_at));
    set(&cached.queried) == set(heads) && young && !waiting
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::path::Path;
    use std::sync::Mutex;

    use super::*;
    use crate::github::fixtures::{
        FORK_PARENT, GRAPHQL_RATE_LIMITED, HTTP_401_STDERR, LIMIT_PRIMARY, PROBE_200, PROBE_304,
        RENAMED_HEAD, SERVER_ERROR, STATES,
    };
    use crate::github::{FakeGh, GhCall, GhCommand, answer, failure};
    use crate::{ForgeRepo, ForgeState, ForgeStatus, ForgeView, LoadSeq, PrBadge, PrState};
    use kika_git::fake::{FakeRunner, TempDir, ok};

    /// A fixed clock: 2023-11-14 22:13:20 UTC.
    const NOW: i64 = 1_700_000_000;
    /// The reset `limit_primary.http` and `graphql_rate_limited.http` name.
    const PRIMARY_RESET: i64 = 1_700_001_800;
    const GRAPHQL_RESET: i64 = 1_700_003_600;

    fn hosts() -> Vec<ForgeHost> {
        vec![ForgeHost::parse("github.com").unwrap()]
    }

    fn repo() -> ForgeRepo {
        ForgeRepo {
            host: "github.com".into(),
            owner: "acme".into(),
            name: "widgets".into(),
        }
    }

    use kika_git::forge_remote::UPSTREAMS_ARGV as UPSTREAMS;

    fn origin(url: &[u8]) -> FakeRunner {
        let mut out = url.to_vec();
        out.push(0);
        FakeRunner::new()
            .expect(&["remote"], ok(b"origin\n"))
            .expect(&["config", "-z", "--get", "remote.origin.url"], ok(&out))
    }

    /// `origin` at `url`, read again on a refresh, when no branch has an
    /// upstream.
    fn origin_refresh(url: &[u8]) -> FakeRunner {
        origin(url).expect(&UPSTREAMS, ok(b""))
    }

    /// Each of `branches` as its own name in [`repo`]'s owner.
    fn own(branches: &[BranchName]) -> Vec<Head> {
        heads(branches, &std::collections::HashMap::new(), &repo())
    }

    fn context(gh: Arc<FakeGh>, cache_dir: Option<PathBuf>) -> ForgeContext {
        ForgeContext {
            gh,
            glab: Arc::new(crate::gitlab::FakeGlab::new()),
            sink: CommandSink::new(),
            cache_dir,
        }
    }

    /// A refresh of the known repository at [`NOW`], no host limited.
    fn refresh<'a>(hosts: &'a [ForgeHost], branches: &'a [BranchName], top: &Path) -> Load<'a> {
        Load {
            hosts,
            branches,
            refresh: true,
            known: None,
            limited: &[],
            top: Some(top.to_path_buf()),
            now: NOW,
        }
    }

    fn sent(git: &FakeRunner, ctx: &ForgeContext, load: &Load) -> Vec<LoadEvent> {
        let sent = Mutex::new(Vec::new());
        run_load(git, ctx, load, &|event| sent.lock().unwrap().push(event));
        sent.into_inner().unwrap()
    }

    /// Show `events` of load `seq` in `forge`, as the model does.
    fn apply(forge: &mut ForgeState, seq: LoadSeq, events: Vec<LoadEvent>) {
        for event in events {
            match event {
                LoadEvent::RepoNone => forge.repo_none(seq),
                LoadEvent::Cached { repo, badges } => forge.cached(seq, repo, badges),
                LoadEvent::Fetched {
                    result,
                    limited_until,
                } => forge.fetched(seq, result, limited_until),
            }
        }
    }

    /// Run `load` for the known repository, no branch with an upstream;
    /// what it sent.
    fn run_known(ctx: &ForgeContext, load: Load) -> Vec<LoadEvent> {
        let git = match load.refresh {
            true => FakeRunner::new().expect(&UPSTREAMS, ok(b"")),
            false => FakeRunner::new(),
        };
        let repo = repo();
        let sent = sent(
            &git,
            ctx,
            &Load {
                known: Some(&repo),
                ..load
            },
        );
        git.assert_done();
        sent
    }

    /// The `Fetched` that ends `sent`.
    fn fetched(
        sent: &[LoadEvent],
    ) -> (
        Result<HashMap<BranchName, PrBadge>, ForgeStatus>,
        Option<i64>,
    ) {
        match sent.last() {
            Some(LoadEvent::Fetched {
                result,
                limited_until,
            }) => (result.clone(), *limited_until),
            other => panic!("{other:?}"),
        }
    }

    fn key(top: &Path) -> CacheKey {
        CacheKey::new(&repo(), top).unwrap()
    }

    fn badge(number: u64, state: PrState, ci: Option<CiState>) -> PrBadge {
        PrBadge { number, state, ci }
    }

    /// A cache dir under `tmp` holding `cached` for the repository at `tmp`.
    fn seed(tmp: &TempDir, cached: &Cached) -> PathBuf {
        let dir = tmp.path().join("forge");
        cache::write(&dir, &key(tmp.path()), cached).unwrap();
        dir
    }

    /// One open PR with green CI, asked about a minute ago.
    fn green(queried: &[BranchName]) -> Cached {
        Cached {
            badges: HashMap::from([(
                "main".into(),
                badge(7, PrState::Open, Some(CiState::Success)),
            )]),
            etag: Some("\"abc\"".into()),
            queried: own(queried),
            forked: false,
            fetched_at: NOW - 60,
        }
    }

    fn probe_etag(cmd: &GhCommand) -> Option<String> {
        match &cmd.kind {
            GhCall::Probe { etag, .. } => etag.clone(),
            other => panic!("not a probe: {other:?}"),
        }
    }

    fn is_graphql(cmd: &GhCommand) -> bool {
        matches!(cmd.kind, GhCall::Graphql { .. })
    }

    #[test]
    fn with_no_hosts_nothing_is_read_run_or_written() {
        let tmp = TempDir::new("forge-off");
        let cache_dir = tmp.path().join("forge");
        let git = FakeRunner::new();
        let gh = Arc::new(FakeGh::new());
        let ctx = context(Arc::clone(&gh), Some(cache_dir.clone()));
        let branches = ["main".into()];
        let sent = sent(&git, &ctx, &refresh(&[], &branches, tmp.path()));
        assert_eq!(sent, [LoadEvent::RepoNone]);
        git.assert_done();
        assert_eq!(gh.calls(), []);
        assert!(!cache_dir.exists());
    }

    #[test]
    fn a_remote_on_another_host_is_none_and_asks_nobody() {
        let git = origin(b"https://github.com.evil.io/o/r");
        let gh = Arc::new(FakeGh::new());
        let ctx = context(Arc::clone(&gh), None);
        let (hosts, branches) = (hosts(), ["main".into()]);
        let load = Load {
            top: None,
            ..refresh(&hosts, &branches, Path::new("/r"))
        };
        assert_eq!(sent(&git, &ctx, &load), [LoadEvent::RepoNone]);
        assert_eq!(gh.calls(), []);
    }

    /// github.com, narrowed by `[forge.allow]` to the owner `acme`.
    fn acme_only() -> Vec<ForgeHost> {
        let rule = crate::allow::AllowRule::parse("acme").unwrap();
        vec![
            ForgeHost::parse("github.com")
                .unwrap()
                .with_allow(vec![rule]),
        ]
    }

    #[test]
    fn a_clone_the_allow_list_leaves_out_reads_and_asks_nothing() {
        let tmp = TempDir::new("forge-unlisted");
        let other = ForgeRepo {
            owner: "other".into(),
            name: "repo".into(),
            ..repo()
        };
        let dir = tmp.path().join("forge");
        let key = CacheKey::new(&other, tmp.path()).unwrap();
        cache::write(&dir, &key, &green(&["main".into()])).unwrap();
        let entries = |dir: &Path| {
            let mut files: Vec<(PathBuf, Vec<u8>)> = std::fs::read_dir(dir)
                .unwrap()
                .map(|e| e.unwrap().path())
                .map(|p| (p.clone(), std::fs::read(&p).unwrap()))
                .collect();
            files.sort();
            files
        };
        let before = entries(&dir);
        let gh = Arc::new(FakeGh::new());
        let ctx = context(Arc::clone(&gh), Some(dir.clone()));
        let (hosts, branches) = (acme_only(), ["main".into()]);

        // Read from the remote: only the URL is read, nothing else.
        let git = origin(b"https://github.com/other/repo");
        let sent = sent(&git, &ctx, &refresh(&hosts, &branches, tmp.path()));
        git.assert_done();
        assert_eq!(sent, [LoadEvent::RepoNone]);

        // Known from an earlier load: still nothing.
        let git = FakeRunner::new();
        for refresh_too in [true, false] {
            let load = Load {
                known: Some(&other),
                refresh: refresh_too,
                ..refresh(&hosts, &branches, tmp.path())
            };
            let sent = self::sent(&git, &ctx, &load);
            assert_eq!(sent, [LoadEvent::RepoNone]);
        }
        git.assert_done();
        assert_eq!(gh.calls(), []);
        assert_eq!(entries(&dir), before);
    }

    #[test]
    fn a_clone_the_allow_list_names_loads_as_without_one() {
        let tmp = TempDir::new("forge-listed");
        let gh = Arc::new(FakeGh::new().reply(answer(PROBE_200)).reply(answer(STATES)));
        let ctx = context(Arc::clone(&gh), Some(tmp.path().join("forge")));
        let (hosts, branches) = (acme_only(), ["main".into()]);
        let git = origin_refresh(b"https://github.com/acme/widgets.git");
        let sent = sent(&git, &ctx, &refresh(&hosts, &branches, tmp.path()));
        git.assert_done();
        assert!(
            matches!(&sent[0], LoadEvent::Cached { repo: r, .. } if *r == repo()),
            "{sent:?}"
        );
        assert!(matches!(fetched(&sent), (Ok(_), None)), "{sent:?}");
        assert_eq!(gh.calls().len(), 2);
    }

    #[test]
    fn the_cache_is_sent_first_then_the_fetch_which_is_cached() {
        let tmp = TempDir::new("forge-load");
        let cache_dir = tmp.path().join("forge");
        let gh = Arc::new(FakeGh::new().reply(answer(PROBE_200)).reply(answer(STATES)));
        let ctx = context(Arc::clone(&gh), Some(cache_dir.clone()));
        let (hosts, branches): (_, Vec<BranchName>) = (hosts(), vec!["main".into()]);
        let shown = ForgeRepo {
            host: "GitHub.com".into(),
            ..repo()
        };

        let git = origin_refresh(b"https://GitHub.com/acme/widgets.git");
        let sent = sent(&git, &ctx, &refresh(&hosts, &branches, tmp.path()));
        let [cached, fetched] = sent.try_into().unwrap();
        assert_eq!(
            cached,
            LoadEvent::Cached {
                repo: shown.clone(),
                badges: None,
            }
        );
        let LoadEvent::Fetched {
            result: Ok(badges),
            limited_until: None,
        } = fetched
        else {
            panic!("{fetched:?}");
        };
        assert_eq!(badges[&branches[0]].state, PrState::Open);
        // The configured, lower-case host is the one asked.
        assert!(gh.calls().iter().all(|c| c.host == "github.com"));
        assert_eq!(gh.calls()[0].cwd, cache_dir);

        // The next load, knowing the repository, reads what that one
        // stored and runs nothing: no git, no gh.
        let git = FakeRunner::new();
        let load = Load {
            refresh: false,
            known: Some(&shown),
            ..refresh(&hosts, &branches, tmp.path())
        };
        let sent = self::sent(&git, &ctx, &load);
        git.assert_done();
        assert_eq!(
            sent,
            [LoadEvent::Cached {
                repo: shown,
                badges: Some(badges),
            }]
        );
        assert_eq!(gh.calls().len(), 2);
    }

    #[test]
    fn the_probe_is_sent_with_the_cached_etag_and_without_one_when_none() {
        let tmp = TempDir::new("forge-probe");
        let (hosts, branches): (_, Vec<BranchName>) = (hosts(), vec!["main".into()]);
        let dir = seed(&tmp, &green(&branches));
        let gh = Arc::new(FakeGh::new().reply(answer(PROBE_304)));
        let ctx = context(Arc::clone(&gh), Some(dir));
        run_known(&ctx, refresh(&hosts, &branches, tmp.path()));
        let [probe] = gh.calls().try_into().unwrap();
        assert_eq!(
            probe.args(),
            [
                "api",
                "--include",
                "--hostname",
                "github.com",
                "-H",
                "Accept: application/vnd.github+json",
                "-H",
                "If-None-Match: \"abc\"",
                "repos/acme/widgets/pulls?state=all&sort=updated&direction=desc&per_page=1",
            ]
            .map(OsString::from)
        );

        // No cache, no ETag: the probe is unconditional, and every call
        // names `--include` and the configured host.
        let gh = Arc::new(FakeGh::new().reply(answer(PROBE_200)).reply(answer(STATES)));
        let ctx = context(Arc::clone(&gh), None);
        run_known(&ctx, refresh(&hosts, &branches, tmp.path()));
        let calls = gh.calls();
        assert_eq!(probe_etag(&calls[0]), None);
        for call in &calls {
            let args: Vec<String> = call
                .args()
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            assert!(
                args.iter().all(|a| !a.contains("If-None-Match")),
                "{args:?}"
            );
            assert!(args.contains(&"--include".to_string()), "{args:?}");
            assert!(args.windows(2).any(|w| w == ["--hostname", "github.com"]));
        }
    }

    #[test]
    fn a_304_keeps_the_cached_badges_and_asks_nothing_more() {
        let tmp = TempDir::new("forge-304");
        let (hosts, branches): (_, Vec<BranchName>) = (hosts(), vec!["main".into(), "x".into()]);
        let mut green = green(&branches);
        // A merged one with failed CI does not wait on anything.
        green.badges.insert(
            "x".into(),
            badge(8, PrState::Merged, Some(CiState::Failure)),
        );
        let dir = seed(&tmp, &green);
        let gh = Arc::new(FakeGh::new().reply(answer(PROBE_304)));
        let ctx = context(Arc::clone(&gh), Some(dir.clone()));
        let mut forge = ForgeState::new(hosts.clone());
        forge.set_branches(&branches);
        let seq = LoadSeq(3);
        assert!(forge.load(seq).is_some_and(|request| request.refresh));
        let sent = run_known(&ctx, refresh(&hosts, &branches, tmp.path()));
        assert_eq!(gh.calls().len(), 1);
        assert_eq!(fetched(&sent), (Ok(green.badges.clone()), None));
        apply(&mut forge, seq, sent);
        assert_eq!(forge.status, ForgeView::Fresh);
        assert_eq!(forge.badges, green.badges);
        // The cache is left alone: `fetched_at` stays the last GraphQL
        // answer's time, so 304s cannot keep it young.
        let file = dir.join(key(tmp.path()).file_name());
        let before = std::fs::read(&file).unwrap();
        assert_eq!(cache::read(&dir, &key(tmp.path())), Some(green));
        assert_eq!(std::fs::read(&file).unwrap(), before);
    }

    #[test]
    fn a_304_on_an_old_answer_asks_graphql_again() {
        let tmp = TempDir::new("forge-304-old");
        let (hosts, branches): (_, Vec<BranchName>) = (hosts(), vec!["main".into()]);
        let old = Cached {
            fetched_at: NOW - 16 * 60,
            ..green(&branches)
        };
        let dir = seed(&tmp, &old);
        let gh = Arc::new(FakeGh::new().reply(answer(PROBE_304)).reply(answer(STATES)));
        let ctx = context(Arc::clone(&gh), Some(dir.clone()));
        let sent = run_known(&ctx, refresh(&hosts, &branches, tmp.path()));
        let calls = gh.calls();
        assert_eq!(calls.len(), 2);
        assert!(is_graphql(&calls[1]));
        let (Ok(badges), None) = fetched(&sent) else {
            panic!("{sent:?}");
        };
        assert_eq!(badges[&branches[0]].number, 101);
        let stored = cache::read(&dir, &key(tmp.path())).unwrap();
        assert_eq!(stored.fetched_at, NOW);
        assert_eq!(stored.etag.as_deref(), Some("\"abc\""));

        // Just inside the bound, the 304 alone still answers.
        let tmp = TempDir::new("forge-304-young");
        let young = Cached {
            fetched_at: NOW - 15 * 60 + 1,
            ..green(&branches)
        };
        let dir = seed(&tmp, &young);
        let gh = Arc::new(FakeGh::new().reply(answer(PROBE_304)));
        let ctx = context(Arc::clone(&gh), Some(dir));
        run_known(&ctx, refresh(&hosts, &branches, tmp.path()));
        assert_eq!(gh.calls().len(), 1);
    }

    #[test]
    fn a_304_still_fetches_for_other_branches_or_waiting_ci() {
        let (hosts, branches): (_, Vec<BranchName>) = (hosts(), vec!["main".into()]);
        let other = green(&["main".into(), "gone".into()]);
        let mut pending = green(&branches);
        pending.badges.insert(
            "main".into(),
            badge(7, PrState::Draft, Some(CiState::Pending)),
        );
        let mut failing = green(&branches);
        failing.badges.insert(
            "main".into(),
            badge(7, PrState::Open, Some(CiState::Failure)),
        );
        for seeded in [other, pending, failing] {
            let tmp = TempDir::new("forge-304-stale");
            let dir = seed(&tmp, &seeded);
            let gh = Arc::new(FakeGh::new().reply(answer(PROBE_304)).reply(answer(STATES)));
            let ctx = context(Arc::clone(&gh), Some(dir.clone()));
            let sent = run_known(&ctx, refresh(&hosts, &branches, tmp.path()));
            let calls = gh.calls();
            assert_eq!(calls.len(), 2, "{seeded:?}");
            assert!(is_graphql(&calls[1]));
            let (Ok(badges), None) = fetched(&sent) else {
                panic!("{sent:?}");
            };
            assert_eq!(badges[&branches[0]].number, 101);
            // The ETag that was sent is kept, with the branches now asked.
            let stored = cache::read(&dir, &key(tmp.path())).unwrap();
            assert_eq!(stored.etag.as_deref(), Some("\"abc\""));
            assert_eq!(stored.queried, own(&branches));
            assert_eq!(stored.badges, badges);
        }
    }

    #[test]
    fn a_200_stores_the_new_etag_which_the_next_refresh_sends() {
        let tmp = TempDir::new("forge-200");
        let (hosts, branches): (_, Vec<BranchName>) = (hosts(), vec!["main".into(), "b".into()]);
        let dir = seed(&tmp, &green(&branches));
        let gh = Arc::new(
            FakeGh::new()
                .reply(answer(PROBE_200))
                .reply(answer(STATES))
                .reply(answer(PROBE_304)),
        );
        let ctx = context(Arc::clone(&gh), Some(dir.clone()));
        run_known(&ctx, refresh(&hosts, &branches, tmp.path()));
        assert!(is_graphql(&gh.calls()[1]));
        let stored = cache::read(&dir, &key(tmp.path())).unwrap();
        assert_eq!(stored.etag.as_deref(), Some("W/\"5f1e0c2b9a7d\""));
        assert_eq!(stored.queried, own(&branches));

        // STATES gave `b` a pending draft, so this 304 asks again; the
        // probe carried the new ETag.
        let gh2 = Arc::new(FakeGh::new().reply(answer(PROBE_304)).reply(answer(STATES)));
        let ctx = context(Arc::clone(&gh2), Some(dir));
        run_known(&ctx, refresh(&hosts, &branches, tmp.path()));
        assert_eq!(
            probe_etag(&gh2.calls()[0]).as_deref(),
            Some("W/\"5f1e0c2b9a7d\"")
        );
    }

    #[test]
    fn a_probe_failure_falls_back_to_graphql() {
        let (hosts, branches): (_, Vec<BranchName>) = (hosts(), vec!["main".into()]);
        let probes = [
            answer(SERVER_ERROR),
            answer("HTTP/2.0 ???\u{1} not an answer"),
            failure(1, "gh: something odd"),
        ];
        for probe in probes {
            let tmp = TempDir::new("forge-probe-fail");
            let dir = seed(&tmp, &green(&branches));
            let gh = Arc::new(FakeGh::new().reply(probe.clone()).reply(answer(STATES)));
            let ctx = context(Arc::clone(&gh), Some(dir.clone()));
            let sent = run_known(&ctx, refresh(&hosts, &branches, tmp.path()));
            assert_eq!(gh.calls().len(), 2, "{probe:?}");
            assert!(matches!(fetched(&sent), (Ok(_), None)), "{sent:?}");
            assert_eq!(cache::read(&dir, &key(tmp.path())).unwrap().etag, None);
        }
        // GraphQL's own result decides the status.
        let tmp = TempDir::new("forge-probe-fail");
        let gh = Arc::new(
            FakeGh::new()
                .reply(answer(SERVER_ERROR))
                .reply(failure(1, HTTP_401_STDERR)),
        );
        let ctx = context(Arc::clone(&gh), None);
        let sent = run_known(&ctx, refresh(&hosts, &branches, tmp.path()));
        assert_eq!(fetched(&sent), (Err(ForgeStatus::NotLoggedIn), None));
    }

    #[test]
    fn a_rate_limited_probe_or_batch_stops_and_leaves_the_cache() {
        let (hosts, few): (_, Vec<BranchName>) = (hosts(), vec!["main".into()]);
        let many: Vec<BranchName> = (0..120).map(|i| format!("t{i}").as_str().into()).collect();
        let cases = [
            (answer(LIMIT_PRIMARY), None, &few, 1, PRIMARY_RESET),
            (
                answer(PROBE_200),
                Some(answer(GRAPHQL_RATE_LIMITED)),
                &many,
                2,
                GRAPHQL_RESET,
            ),
        ];
        for (probe, batch, branches, calls, until) in cases {
            let tmp = TempDir::new("forge-limited");
            let dir = seed(&tmp, &green(&few));
            let file = dir.join(key(tmp.path()).file_name());
            let before = std::fs::read(&file).unwrap();
            let mut gh = FakeGh::new().reply(probe);
            if let Some(batch) = batch {
                gh = gh.reply(batch);
            }
            let gh = Arc::new(gh);
            let ctx = context(Arc::clone(&gh), Some(dir));
            let sent = run_known(&ctx, refresh(&hosts, branches, tmp.path()));
            assert_eq!(gh.calls().len(), calls);
            assert_eq!(fetched(&sent), (Err(ForgeStatus::RateLimited), Some(until)));
            assert_eq!(std::fs::read(&file).unwrap(), before);
        }
    }

    #[test]
    fn a_limited_host_gets_no_call_until_its_reset() {
        let tmp = TempDir::new("forge-backoff");
        let (hosts, branches): (_, Vec<BranchName>) = (hosts(), vec!["main".into()]);
        let dir = seed(&tmp, &green(&branches));
        let limited = [(hosts[0].clone(), NOW + 10)];
        let gh = Arc::new(FakeGh::new());
        let ctx = context(Arc::clone(&gh), Some(dir.clone()));
        // Not even git is asked about upstreams.
        let (git, known) = (FakeRunner::new(), repo());
        let sent = self::sent(
            &git,
            &ctx,
            &Load {
                limited: &limited,
                known: Some(&known),
                ..refresh(&hosts, &branches, tmp.path())
            },
        );
        git.assert_done();
        assert_eq!(gh.calls(), []);
        assert_eq!(
            sent,
            [
                LoadEvent::Cached {
                    repo: repo(),
                    badges: Some(green(&branches).badges),
                },
                LoadEvent::Fetched {
                    result: Err(ForgeStatus::RateLimited),
                    limited_until: Some(NOW + 10),
                },
            ]
        );

        // At the reset, and for another host, `gh` is asked again.
        let other = [(ForgeHost::parse("github.example.com").unwrap(), NOW + 10)];
        for (limited, now) in [(&limited, NOW + 10), (&other, NOW)] {
            let gh = Arc::new(FakeGh::new().reply(answer(PROBE_304)));
            let ctx = context(Arc::clone(&gh), Some(dir.clone()));
            let sent = run_known(
                &ctx,
                Load {
                    limited,
                    now,
                    ..refresh(&hosts, &branches, tmp.path())
                },
            );
            assert_eq!(gh.calls().len(), 1);
            assert!(matches!(fetched(&sent), (Ok(_), None)));
        }
    }

    /// `gh` holds a token and prints it; none of it may reach the log, the
    /// forge state or the cache. The probe's ETag reaches only the cache,
    /// as the validated ETag. The app state and the screen are checked by
    /// the pull_requests effects tests.
    #[test]
    fn a_token_never_leaves_gh() {
        use crate::github::fixtures::{SECRET_ETAG as ETAG, SECRET_TOKEN as SENTINEL, leaky_gh};
        let tmp = TempDir::new("forge-secret");
        let cache_dir = tmp.path().join("forge");
        let gh = Arc::new(leaky_gh());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = CommandSink::new();
        let keep = Arc::clone(&events);
        sink.connect(move |ev| keep.lock().unwrap().push(ev));
        let ctx = ForgeContext {
            gh: gh.clone(),
            glab: Arc::new(crate::gitlab::FakeGlab::new()),
            sink,
            cache_dir: Some(cache_dir.clone()),
        };
        let branches: Vec<BranchName> = vec!["main".into()];
        let hosts = hosts();
        let mut forge = ForgeState::new(hosts.clone());
        forge.set_branches(&branches);
        for seq in [LoadSeq(1), LoadSeq(2)] {
            forge.want_refresh();
            assert!(forge.load(seq).is_some_and(|request| request.refresh));
            let git = origin_refresh(b"https://x:pw_USERINFO@github.com/acme/widgets");
            let load = refresh(&hosts, &branches, tmp.path());
            apply(&mut forge, seq, sent(&git, &ctx, &load));
        }
        // The token was there to leak, and the second probe sent the ETag.
        assert!(gh.saw_env("GH_TOKEN", SENTINEL));
        assert_eq!(probe_etag(&gh.calls()[2]).as_deref(), Some(ETAG));
        assert_eq!(forge.status, ForgeView::Problem(ForgeStatus::NotLoggedIn));
        assert!(!forge.badges.is_empty());
        let secrets = [SENTINEL, "pw_USERINFO", "etag_VALUE"];
        for event in events.lock().unwrap().iter() {
            let text = format!("{event:?}");
            assert!(secrets.iter().all(|s| !text.contains(s)), "{text}");
        }
        let text = format!("{forge:?}");
        assert!(secrets.iter().all(|s| !text.contains(s)), "state leaks");
        for entry in std::fs::read_dir(&cache_dir).unwrap() {
            let bytes = std::fs::read(entry.unwrap().path()).unwrap();
            let text = String::from_utf8_lossy(&bytes);
            assert!(
                !text.contains(SENTINEL) && !text.contains("pw_USERINFO"),
                "{text}"
            );
            assert_eq!(text.matches("etag_VALUE").count(), 1, "{text}");
            assert!(text.contains(r#""etag":"W/\"etag_VALUE_9\"""#), "{text}");
        }
    }

    #[test]
    fn a_renamed_upstream_is_asked_by_its_name_and_badged_by_the_local_one() {
        let tmp = TempDir::new("forge-renamed");
        let git = origin(b"git@github.com:acme/widgets.git")
            .expect(
                &UPSTREAMS,
                ok(b"refs/heads/wip\0origin\0refs/heads/feature-x\n"),
            )
            .expect(&["remote"], ok(b"origin\n"))
            .expect(
                &["config", "-z", "--get", "remote.origin.url"],
                ok(b"git@github.com:acme/widgets.git\0"),
            );
        let gh = Arc::new(
            FakeGh::new()
                .reply(answer(PROBE_200))
                .reply(answer(RENAMED_HEAD)),
        );
        let ctx = context(Arc::clone(&gh), None);
        let (hosts, branches): (_, Vec<BranchName>) = (hosts(), vec!["wip".into()]);
        let sent = sent(&git, &ctx, &refresh(&hosts, &branches, tmp.path()));
        git.assert_done();
        let calls = gh.calls();
        let GhCall::Graphql { body } = &calls[1].kind else {
            panic!("{calls:?}");
        };
        let body: serde_json::Value = serde_json::from_slice(body).unwrap();
        assert_eq!(body["variables"]["b0"], "feature-x");
        assert!(!body.to_string().contains("wip"));
        let (Ok(badges), None) = fetched(&sent) else {
            panic!("{sent:?}");
        };
        assert_eq!(
            badges,
            HashMap::from([(
                BranchName::from("wip"),
                badge(44, PrState::Open, Some(CiState::Success))
            )])
        );
    }

    #[test]
    fn a_fork_finds_its_parents_pr_and_then_skips_the_probe() {
        let tmp = TempDir::new("forge-fork");
        let cache_dir = tmp.path().join("forge");
        let fork = ForgeRepo {
            host: "github.com".into(),
            owner: "me".into(),
            name: "r".into(),
        };
        let gh = Arc::new(
            FakeGh::new()
                .reply(answer(PROBE_200))
                .reply(answer(FORK_PARENT))
                .reply(answer(FORK_PARENT)),
        );
        let ctx = context(Arc::clone(&gh), Some(cache_dir.clone()));
        let (hosts, branches): (_, Vec<BranchName>) = (hosts(), vec!["fix".into()]);
        let want = HashMap::from([(
            BranchName::from("fix"),
            badge(31, PrState::Draft, Some(CiState::Success)),
        )]);

        // `origin` is the fork, me/r: no PR of `me` in it, one in the parent.
        let git = origin_refresh(b"https://github.com/me/r");
        let sent = self::sent(&git, &ctx, &refresh(&hosts, &branches, tmp.path()));
        git.assert_done();
        assert_eq!(fetched(&sent), (Ok(want.clone()), None));
        assert_eq!(gh.calls().len(), 2);
        let key = CacheKey::new(&fork, tmp.path()).unwrap();
        let stored = cache::read(&cache_dir, &key).unwrap();
        assert!(stored.forked);

        // The next refresh asks GraphQL straight away: exactly one call.
        let git = FakeRunner::new().expect(&UPSTREAMS, ok(b""));
        let load = Load {
            known: Some(&fork),
            ..refresh(&hosts, &branches, tmp.path())
        };
        let sent = self::sent(&git, &ctx, &load);
        git.assert_done();
        assert_eq!(fetched(&sent), (Ok(want), None));
        let calls = gh.calls();
        assert_eq!(calls.len(), 3);
        assert!(is_graphql(&calls[2]));
    }

    #[test]
    fn a_changed_head_set_is_not_reusable() {
        let branches: Vec<BranchName> = vec!["main".into()];
        let cached = green(&branches);
        assert!(reusable(&cached, &own(&branches), NOW));
        let renamed = Head {
            name: "other".into(),
            ..own(&branches)[0].clone()
        };
        let forked = Head {
            owner: "me".into(),
            ..own(&branches)[0].clone()
        };
        for head in [renamed, forked] {
            assert!(
                !reusable(&cached, std::slice::from_ref(&head), NOW),
                "{head:?}"
            );
        }
    }

    mod gitlab {
        use super::*;
        use crate::gitlab::FakeGlab;
        use crate::gitlab::fixtures::{RATE_LIMITED_429, STATES, UNAUTHORIZED_401};
        use crate::url::ForgeKind;

        fn hosts() -> Vec<ForgeHost> {
            vec![
                ForgeHost::parse("gitlab.example.com")
                    .unwrap()
                    .with_kind(ForgeKind::GitLab),
            ]
        }

        fn repo() -> ForgeRepo {
            ForgeRepo {
                host: "gitlab.example.com".into(),
                owner: "acme".into(),
                name: "widgets".into(),
            }
        }

        const URL: &[u8] = b"https://gitlab.example.com/acme/widgets.git";

        /// A context whose `gh` must never be asked.
        fn context(glab: &Arc<FakeGlab>, gh: &Arc<FakeGh>, dir: Option<PathBuf>) -> ForgeContext {
            ForgeContext {
                gh: gh.clone(),
                glab: glab.clone(),
                sink: CommandSink::new(),
                cache_dir: dir,
            }
        }

        #[test]
        fn a_refresh_sends_the_cache_then_the_fetch_which_is_cached() {
            let tmp = TempDir::new("forge-gitlab-load");
            let dir = tmp.path().join("forge");
            let glab = Arc::new(FakeGlab::new().reply(answer(STATES)));
            let gh = Arc::new(FakeGh::new());
            let ctx = context(&glab, &gh, Some(dir.clone()));
            let (hosts, branches): (_, Vec<BranchName>) =
                (hosts(), vec!["topic-0".into(), "topic-1".into()]);

            let git = origin_refresh(URL);
            let sent = sent(&git, &ctx, &refresh(&hosts, &branches, tmp.path()));
            git.assert_done();
            let want = HashMap::from([
                (
                    branches[0].clone(),
                    badge(1, PrState::Open, Some(CiState::Success)),
                ),
                (
                    branches[1].clone(),
                    badge(2, PrState::Draft, Some(CiState::Pending)),
                ),
            ]);
            assert_eq!(
                sent,
                [
                    LoadEvent::Cached {
                        repo: repo(),
                        badges: None,
                    },
                    LoadEvent::Fetched {
                        result: Ok(want.clone()),
                        limited_until: None,
                    },
                ]
            );
            let [call] = glab.calls().try_into().unwrap();
            assert_eq!(
                (call.host.as_str(), call.cwd.as_path()),
                ("gitlab.example.com", dir.as_path())
            );
            assert_eq!(gh.calls(), []);
            let key = CacheKey::new(&repo(), tmp.path()).unwrap();
            assert_eq!(
                cache::read(&dir, &key),
                Some(Cached {
                    badges: want.clone(),
                    etag: None,
                    queried: heads(&branches, &HashMap::new(), &repo()),
                    forked: false,
                    fetched_at: NOW,
                })
            );

            // Without a refresh, the cache answers and nothing runs.
            let (git, known) = (FakeRunner::new(), repo());
            let load = Load {
                refresh: false,
                known: Some(&known),
                ..refresh(&hosts, &branches, tmp.path())
            };
            let sent = self::sent(&git, &ctx, &load);
            git.assert_done();
            assert_eq!(
                sent,
                [LoadEvent::Cached {
                    repo: repo(),
                    badges: Some(want),
                }]
            );
            assert_eq!(glab.calls().len(), 1);
            assert_eq!(gh.calls(), []);
        }

        #[test]
        fn a_refusal_or_rate_limit_says_why_and_leaves_the_cache() {
            let branches: Vec<BranchName> = vec!["topic-0".into()];
            let cases = [
                (
                    answer(UNAUTHORIZED_401),
                    Err(ForgeStatus::NotLoggedIn),
                    None,
                ),
                (
                    answer(RATE_LIMITED_429),
                    Err(ForgeStatus::RateLimited),
                    Some(NOW + 1200),
                ),
            ];
            for (reply, result, limited_until) in cases {
                let tmp = TempDir::new("forge-gitlab-refused");
                let glab = Arc::new(FakeGlab::new().reply(reply));
                let gh = Arc::new(FakeGh::new());
                let dir = tmp.path().join("forge");
                let ctx = context(&glab, &gh, Some(dir.clone()));
                let (git, hosts) = (origin_refresh(URL), hosts());
                let sent = sent(&git, &ctx, &refresh(&hosts, &branches, tmp.path()));
                assert_eq!(
                    sent.last(),
                    Some(&LoadEvent::Fetched {
                        result,
                        limited_until
                    })
                );
                assert!(!dir.exists() || std::fs::read_dir(&dir).unwrap().next().is_none());
                assert_eq!(glab.calls().len(), 1);
            }
        }

        #[test]
        fn a_limited_host_gets_no_call_and_no_refresh_none() {
            let tmp = TempDir::new("forge-gitlab-limited");
            let (hosts, branches): (_, Vec<BranchName>) = (hosts(), vec!["main".into()]);
            let glab = Arc::new(FakeGlab::new());
            let gh = Arc::new(FakeGh::new());
            let ctx = context(&glab, &gh, None);
            let limited = [(hosts[0].clone(), NOW + 10)];
            let known = repo();
            for load in [
                Load {
                    limited: &limited,
                    known: Some(&known),
                    ..refresh(&hosts, &branches, tmp.path())
                },
                Load {
                    refresh: false,
                    known: Some(&known),
                    ..refresh(&hosts, &branches, tmp.path())
                },
            ] {
                let git = FakeRunner::new();
                let sent = sent(&git, &ctx, &load);
                git.assert_done();
                assert!(matches!(sent[0], LoadEvent::Cached { .. }), "{sent:?}");
                if load.refresh {
                    assert_eq!(
                        sent[1],
                        LoadEvent::Fetched {
                            result: Err(ForgeStatus::RateLimited),
                            limited_until: Some(NOW + 10),
                        }
                    );
                } else {
                    assert_eq!(sent.len(), 1);
                }
            }
            assert_eq!(glab.calls(), []);
            assert_eq!(gh.calls(), []);
        }

        #[test]
        fn with_no_hosts_nothing_is_read_run_or_written() {
            let tmp = TempDir::new("forge-gitlab-off");
            let dir = tmp.path().join("forge");
            let glab = Arc::new(FakeGlab::new());
            let gh = Arc::new(FakeGh::new());
            let ctx = context(&glab, &gh, Some(dir.clone()));
            let git = FakeRunner::new();
            let branches = ["main".into()];
            let known = repo();
            let load = Load {
                known: Some(&known),
                ..refresh(&[], &branches, tmp.path())
            };
            assert_eq!(sent(&git, &ctx, &load), [LoadEvent::RepoNone]);
            git.assert_done();
            assert_eq!(glab.calls(), []);
            assert!(!dir.exists());
        }

        #[test]
        fn a_gitlab_remote_on_a_github_kind_host_never_reaches_glab() {
            let glab = Arc::new(FakeGlab::new());
            let gh = Arc::new(
                FakeGh::new()
                    .reply(answer(crate::github::fixtures::PROBE_200))
                    .reply(answer(crate::github::fixtures::STATES)),
            );
            let ctx = context(&glab, &gh, None);
            let hub = vec![ForgeHost::parse("gitlab.example.com").unwrap()];
            let branches: Vec<BranchName> = vec!["main".into()];
            let git = origin_refresh(URL);
            sent(&git, &ctx, &refresh(&hub, &branches, Path::new("/r")));
            assert_eq!(glab.calls(), []);
            assert_eq!(gh.calls().len(), 2);
        }

        /// `glab` holds a token and prints it, on stderr and in a header;
        /// none of it may reach the events, the log, the cache or what
        /// Kika passes to `glab`.
        #[test]
        fn a_token_never_leaves_glab() {
            use crate::gitlab::fixtures::{SECRET_TOKEN as SENTINEL, leaky_glab};
            use crate::gitlab::{GLAB_ENV, GLAB_ENV_REMOVE};
            let tmp = TempDir::new("forge-gitlab-secret");
            let dir = tmp.path().join("forge");
            let glab = Arc::new(leaky_glab());
            let gh = Arc::new(FakeGh::new());
            let log = Arc::new(Mutex::new(Vec::new()));
            let sink = CommandSink::new();
            let keep = Arc::clone(&log);
            sink.connect(move |ev| keep.lock().unwrap().push(ev));
            let ctx = ForgeContext {
                sink,
                ..context(&glab, &gh, Some(dir.clone()))
            };
            let branches: Vec<BranchName> = vec!["topic-0".into()];
            let hosts = hosts();
            let mut forge = ForgeState::new(hosts.clone());
            forge.set_branches(&branches);
            let mut events = Vec::new();
            for seq in [LoadSeq(1), LoadSeq(2)] {
                forge.want_refresh();
                assert!(forge.load(seq).is_some_and(|request| request.refresh));
                let git = origin_refresh(b"https://x:pw_USERINFO@gitlab.example.com/acme/widgets");
                let sent = sent(&git, &ctx, &refresh(&hosts, &branches, tmp.path()));
                events.extend(sent.clone());
                apply(&mut forge, seq, sent);
            }
            // The token was there to leak.
            assert!(glab.saw_env("GITLAB_TOKEN", SENTINEL));
            assert_eq!(forge.status, ForgeView::Problem(ForgeStatus::NotLoggedIn));
            assert!(!forge.badges.is_empty());
            assert_eq!(forge.kind(), Some(ForgeKind::GitLab));
            let secrets = [SENTINEL, "glpat", "pw_USERINFO"];
            let clean = |text: &str| secrets.iter().all(|s| !text.contains(s));
            for event in &events {
                assert!(clean(&format!("{event:?}")), "{event:?}");
            }
            for event in log.lock().unwrap().iter() {
                assert!(clean(&format!("{event:?}")), "{event:?}");
            }
            assert!(clean(&format!("{forge:?}")), "state leaks");
            for entry in std::fs::read_dir(&dir).unwrap() {
                let bytes = std::fs::read(entry.unwrap().path()).unwrap();
                assert!(clean(&String::from_utf8_lossy(&bytes)));
            }
            let calls = glab.calls();
            assert_eq!(calls.len(), 2);
            for call in &calls {
                let argv = format!("{:?} {}", call.args(), call.display_line());
                assert!(clean(&argv), "{argv}");
                assert!(clean(&String::from_utf8_lossy(&call.stdin())));
            }
            let env = format!("{GLAB_ENV:?} {GLAB_ENV_REMOVE:?}");
            assert!(clean(&env), "{env}");
        }
    }
}
