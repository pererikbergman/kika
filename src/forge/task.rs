//! One `LoadPullRequests` effect, run off the UI loop: find the forge
//! repository, show the cache, then ask the forge when told to.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use super::cache::{self, CacheKey, Cached};
use super::github::{Answer, Gh, GhRunner, MAX_BRANCHES, Refusal};
use super::remote::{matches, parse_remote};
use super::{CiState, ForgeHost, ForgeRepo, ForgeStatus, PrState};
use crate::effect::Tag;
use crate::git::GitRunner;
use crate::git::branches::BranchName;
use crate::git::forge_remote::read_remote_url;
use crate::git::recording::CommandSink;
use crate::intent::AppIntent;

/// What the runtime needs to reach a forge. Only `main` builds one with
/// the real `gh`; without one every load answers `ForgeRepoNone`.
#[derive(Clone)]
pub struct ForgeContext {
    pub gh: Arc<dyn GhRunner>,
    pub sink: CommandSink,
    /// `None` when no cache directory is known: nothing is cached.
    pub cache_dir: Option<PathBuf>,
}

/// One load's inputs, as the effect gave them.
pub struct Load<'a> {
    pub tag: Tag,
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

/// Run `load`, sending each result as it is known: `ForgeRepoNone`, or
/// `PullRequestsCached` and then, on a refresh, `PullRequestsFetched`.
///
/// A refresh first probes, for free, whether the pull requests changed;
/// when they did not and the cache is still good, no GraphQL query runs.
/// A rate-limited host gets no call at all until its limit resets.
pub fn run_load(git: &dyn GitRunner, ctx: &ForgeContext, load: &Load, send: &dyn Fn(AppIntent)) {
    let tag = load.tag;
    // Defence in depth: with no hosts nothing is read, run or written.
    if load.hosts.is_empty() {
        send(AppIntent::ForgeRepoNone { tag });
        return;
    }
    let found = match load.known {
        Some(repo) => Some(repo.clone()),
        None => read_remote_url(git).and_then(|url| parse_remote(&url)),
    }
    .and_then(|repo| Some((matches(&repo, load.hosts)?, repo)));
    let Some((host, repo)) = found else {
        send(AppIntent::ForgeRepoNone { tag });
        return;
    };
    let key = load
        .top
        .as_deref()
        .and_then(|top| CacheKey::new(&repo, top));
    let place = ctx.cache_dir.as_deref().zip(key.as_ref());
    let cached = place.and_then(|(dir, key)| cache::read(dir, key));
    send(AppIntent::PullRequestsCached {
        tag,
        repo: repo.clone(),
        badges: cached.as_ref().map(|c| c.badges.clone()),
    });
    if !load.refresh {
        return;
    }
    let fetched = |result, limited_until| {
        send(AppIntent::PullRequestsFetched {
            tag,
            result,
            limited_until,
        })
    };
    let limited = |until| fetched(Err(ForgeStatus::RateLimited), Some(until));
    let until = load
        .limited
        .iter()
        .find(|(limited, _)| limited == host)
        .map(|(_, until)| *until);
    if let Some(until) = until.filter(|until| *until > load.now) {
        return limited(until);
    }
    // `gh` runs in the cache directory, never the repository, so it reads
    // no repository's git config.
    let cwd = ctx
        .cache_dir
        .clone()
        .filter(|dir| cache::ensure_dir(dir).is_ok())
        .unwrap_or_else(std::env::temp_dir);
    let gh = Gh {
        runner: ctx.gh.as_ref(),
        sink: &ctx.sink,
        host,
        cwd,
        now: load.now,
    };
    let branches = &load.branches[..load.branches.len().min(MAX_BRANCHES)];
    let sent = cached.as_ref().and_then(|c| c.etag.clone());
    let store = |cached: &Cached| {
        if let Some((dir, key)) = place {
            // A cache that cannot be written only costs the next startup.
            let _ = cache::write(dir, key, cached, load.now);
        }
    };
    let etag = match gh.probe(&repo, sent.as_deref()) {
        Answer::Limited { until } => return limited(until),
        Answer::NotModified => match cached.filter(|c| reusable(c, branches)) {
            Some(cached) => {
                store(&cached);
                return fetched(Ok(cached.badges), None);
            }
            None => sent,
        },
        Answer::Body { etag, .. } => etag,
        // No worse than without a probe: ask anyway.
        Answer::Failed(_) => None,
    };
    match gh.fetch_badges(&repo, branches) {
        Ok(badges) => {
            store(&Cached {
                badges: badges.clone(),
                etag,
                queried: branches.to_vec(),
            });
            fetched(Ok(badges), None);
        }
        Err(Refusal::Limited { until }) => limited(until),
        Err(Refusal::Status(status)) => fetched(Err(status), None),
    }
}

/// Whether `cached` still answers for `branches` when the pull requests
/// did not change: the same branches were asked, and no open PR waits on
/// CI, which can finish without changing the pull-request list.
fn reusable(cached: &Cached, branches: &[BranchName]) -> bool {
    let waiting = cached.badges.values().any(|badge| {
        matches!(badge.state, PrState::Open | PrState::Draft)
            && matches!(badge.ci, Some(CiState::Pending | CiState::Failure))
    });
    let set = |names: &[BranchName]| names.iter().cloned().collect::<HashSet<_>>();
    set(&cached.queried) == set(branches) && !waiting
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::path::Path;
    use std::sync::Mutex;

    use super::*;
    use crate::forge::github::fixtures::{
        GRAPHQL_RATE_LIMITED, HTTP_401_STDERR, LIMIT_PRIMARY, PROBE_200, PROBE_304, SERVER_ERROR,
        STATES,
    };
    use crate::forge::github::{FakeGh, GhCall, GhCommand, GhOutput, answer, failure};
    use crate::forge::{ForgeRepo, ForgeStatus, ForgeView, PrBadge, PrState};
    use crate::git::fake::{FakeRunner, TempDir, ok};

    /// A fixed clock: 2023-11-14 22:13:20 UTC.
    const NOW: i64 = 1_700_000_000;
    /// The reset `limit_primary.http` and `graphql_rate_limited.http` name.
    const PRIMARY_RESET: i64 = 1_700_001_800;
    const GRAPHQL_RESET: i64 = 1_700_003_600;

    fn tag() -> Tag {
        Tag {
            generation: 0,
            seq: 3,
            user: false,
        }
    }

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

    fn origin(url: &[u8]) -> FakeRunner {
        let mut out = url.to_vec();
        out.push(0);
        FakeRunner::new()
            .expect(&["remote"], ok(b"origin\n"))
            .expect(&["config", "-z", "--get", "remote.origin.url"], ok(&out))
    }

    fn context(gh: Arc<FakeGh>, cache_dir: Option<PathBuf>) -> ForgeContext {
        ForgeContext {
            gh,
            sink: CommandSink::new(),
            cache_dir,
        }
    }

    /// A refresh of the known repository at [`NOW`], no host limited.
    fn refresh<'a>(hosts: &'a [ForgeHost], branches: &'a [BranchName], top: &Path) -> Load<'a> {
        Load {
            tag: tag(),
            hosts,
            branches,
            refresh: true,
            known: None,
            limited: &[],
            top: Some(top.to_path_buf()),
            now: NOW,
        }
    }

    fn sent(git: &FakeRunner, ctx: &ForgeContext, load: &Load) -> Vec<AppIntent> {
        let sent = Mutex::new(Vec::new());
        run_load(git, ctx, load, &|intent| sent.lock().unwrap().push(intent));
        sent.into_inner().unwrap()
    }

    /// Run `load` for the known repository; what it sent.
    fn run_known(ctx: &ForgeContext, load: Load) -> Vec<AppIntent> {
        let git = FakeRunner::new();
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

    /// The `PullRequestsFetched` that ends `sent`.
    fn fetched(
        sent: &[AppIntent],
    ) -> (
        Result<HashMap<BranchName, PrBadge>, ForgeStatus>,
        Option<i64>,
    ) {
        match sent.last() {
            Some(AppIntent::PullRequestsFetched {
                result,
                limited_until,
                ..
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
        cache::write(&dir, &key(tmp.path()), cached, 1).unwrap();
        dir
    }

    fn green(queried: &[BranchName]) -> Cached {
        Cached {
            badges: HashMap::from([(
                "main".into(),
                badge(7, PrState::Open, Some(CiState::Success)),
            )]),
            etag: Some("\"abc\"".into()),
            queried: queried.to_vec(),
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
        assert_eq!(sent, [AppIntent::ForgeRepoNone { tag: tag() }]);
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
        assert_eq!(
            sent(&git, &ctx, &load),
            [AppIntent::ForgeRepoNone { tag: tag() }]
        );
        assert_eq!(gh.calls(), []);
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

        let git = origin(b"https://GitHub.com/acme/widgets.git");
        let sent = sent(&git, &ctx, &refresh(&hosts, &branches, tmp.path()));
        let [cached, fetched] = sent.try_into().unwrap();
        assert_eq!(
            cached,
            AppIntent::PullRequestsCached {
                tag: tag(),
                repo: shown.clone(),
                badges: None,
            }
        );
        let AppIntent::PullRequestsFetched {
            result: Ok(badges),
            limited_until: None,
            ..
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
            [AppIntent::PullRequestsCached {
                tag: tag(),
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
        let mut state = crate::forge::tests::forge_state(&branches);
        let effects = state.load_pull_requests();
        let Some(crate::effect::Effect::LoadPullRequests { tag: load_tag, .. }) = effects.first()
        else {
            panic!("{effects:?}");
        };
        let sent = run_known(
            &ctx,
            Load {
                tag: *load_tag,
                ..refresh(&hosts, &branches, tmp.path())
            },
        );
        assert_eq!(gh.calls().len(), 1);
        assert_eq!(fetched(&sent), (Ok(green.badges.clone()), None));
        for intent in sent {
            state.update(intent);
        }
        assert_eq!(state.forge.status, ForgeView::Fresh);
        assert_eq!(state.forge.badges, green.badges);
        // The cache is rewritten at this time, the ETag unchanged.
        assert_eq!(cache::read(&dir, &key(tmp.path())), Some(green));
        let text = std::fs::read_to_string(dir.join(key(tmp.path()).file_name())).unwrap();
        assert!(text.contains(&format!("\"fetched_at\":{NOW}")), "{text}");
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
            assert_eq!(stored.queried, branches);
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
        assert_eq!(stored.queried, branches);

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
        let sent = run_known(
            &ctx,
            Load {
                limited: &limited,
                ..refresh(&hosts, &branches, tmp.path())
            },
        );
        assert_eq!(gh.calls(), []);
        assert_eq!(
            sent,
            [
                AppIntent::PullRequestsCached {
                    tag: tag(),
                    repo: repo(),
                    badges: Some(green(&branches).badges),
                },
                AppIntent::PullRequestsFetched {
                    tag: tag(),
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
    /// state, the cache or the screen. The probe's ETag reaches only the
    /// cache, as the validated ETag.
    #[test]
    fn a_token_never_leaves_gh() {
        const SENTINEL: &str = "ghp_SENTINEL";
        const ETAG: &str = "W/\"etag_VALUE_9\"";
        let tmp = TempDir::new("forge-secret");
        let cache_dir = tmp.path().join("forge");
        let noisy = |output: GhOutput| GhOutput {
            stderr: format!(
                "GH_TOKEN={SENTINEL}\n{}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into_bytes(),
            ..output
        };
        let probe = PROBE_200.replace("W/\"5f1e0c2b9a7d\"", ETAG).replace(
            "Cache-Control:",
            &format!("Authorization: token {SENTINEL}\r\nCache-Control:"),
        );
        let gh = Arc::new(
            FakeGh::new()
                .with_inherited_env("GH_TOKEN", SENTINEL)
                .reply(noisy(answer(&probe)))
                .reply(noisy(answer(STATES)))
                .reply(noisy(failure(1, HTTP_401_STDERR)))
                .reply(noisy(failure(1, HTTP_401_STDERR))),
        );
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = CommandSink::new();
        let keep = Arc::clone(&events);
        sink.connect(move |ev| keep.lock().unwrap().push(ev));
        let ctx = ForgeContext {
            gh: gh.clone(),
            sink,
            cache_dir: Some(cache_dir.clone()),
        };
        let branches: Vec<BranchName> = vec!["main".into()];
        let mut state = crate::forge::tests::forge_state(&branches);
        let hosts = hosts();
        for _ in 0..2 {
            state.forge.want_refresh();
            let effects = state.load_pull_requests();
            assert_eq!(effects.len(), 1);
            let load_tag = match &effects[0] {
                crate::effect::Effect::LoadPullRequests { tag, .. } => *tag,
                other => panic!("{other:?}"),
            };
            let git = origin(b"https://x:pw_USERINFO@github.com/acme/widgets");
            let load = Load {
                tag: load_tag,
                ..refresh(&hosts, &branches, tmp.path())
            };
            for intent in sent(&git, &ctx, &load) {
                state.update(intent);
            }
        }
        // The token was there to leak, and the second probe sent the ETag.
        assert!(gh.saw_env("GH_TOKEN", SENTINEL));
        assert_eq!(probe_etag(&gh.calls()[2]).as_deref(), Some(ETAG));
        assert_eq!(
            state.forge.status,
            ForgeView::Problem(ForgeStatus::NotLoggedIn)
        );
        assert!(!state.forge.badges.is_empty());
        let secrets = [SENTINEL, "pw_USERINFO", "etag_VALUE"];
        for event in events.lock().unwrap().iter() {
            let text = format!("{event:?}");
            assert!(secrets.iter().all(|s| !text.contains(s)), "{text}");
        }
        let text = format!("{state:?}");
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
        let rows = crate::branches_view::tests::draw_branches(&state, 80, 12);
        assert!(
            rows.iter().all(|r| secrets.iter().all(|s| !r.contains(s))),
            "{rows:?}"
        );
        assert!(
            rows.iter().any(|r| r.contains("GitHub: not logged in")),
            "{rows:?}"
        );
    }
}
