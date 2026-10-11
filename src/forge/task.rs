//! One `LoadPullRequests` effect, run off the UI loop: find the forge
//! repository, show the cache, then ask the forge when told to.

use std::path::PathBuf;
use std::sync::Arc;

use super::cache::{self, CacheKey};
use super::github::{GhRunner, fetch_badges};
use super::remote::{matches, parse_remote};
use super::{ForgeHost, ForgeRepo};
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
    /// The repository's canonical top level, for the cache key.
    pub top: Option<PathBuf>,
    /// Seconds since the epoch, stored with the cache entry.
    pub now: i64,
}

/// Run `load`, sending each result as it is known: `ForgeRepoNone`, or
/// `PullRequestsCached` and then, on a refresh, `PullRequestsFetched`.
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
    let badges = place.and_then(|(dir, key)| cache::read(dir, key));
    send(AppIntent::PullRequestsCached {
        tag,
        repo: repo.clone(),
        badges,
    });
    if !load.refresh {
        return;
    }
    // `gh` runs in the cache directory, never the repository, so it reads
    // no repository's git config.
    let cwd = ctx
        .cache_dir
        .clone()
        .filter(|dir| cache::ensure_dir(dir).is_ok())
        .unwrap_or_else(std::env::temp_dir);
    let result = fetch_badges(ctx.gh.as_ref(), &ctx.sink, host, &repo, load.branches, cwd);
    if let (Ok(badges), Some((dir, key))) = (&result, place) {
        // A cache that cannot be written only costs the next startup.
        let _ = cache::write(dir, key, badges, load.now);
    }
    send(AppIntent::PullRequestsFetched { tag, result });
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::forge::github::fixtures::{HTTP_401_STDERR, STATES};
    use crate::forge::github::{FakeGh, GhOutput, answer, failure};
    use crate::forge::{ForgeRepo, ForgeStatus, ForgeView, PrState};
    use crate::git::fake::{FakeRunner, TempDir, ok};

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

    fn run(
        git: &FakeRunner,
        ctx: &ForgeContext,
        hosts: &[ForgeHost],
        branches: &[BranchName],
        refresh: bool,
        top: Option<PathBuf>,
    ) -> Vec<AppIntent> {
        run_known(git, ctx, hosts, branches, refresh, top, None)
    }

    fn run_known(
        git: &FakeRunner,
        ctx: &ForgeContext,
        hosts: &[ForgeHost],
        branches: &[BranchName],
        refresh: bool,
        top: Option<PathBuf>,
        known: Option<&ForgeRepo>,
    ) -> Vec<AppIntent> {
        let sent = Mutex::new(Vec::new());
        let load = Load {
            tag: tag(),
            hosts,
            branches,
            refresh,
            known,
            top,
            now: 42,
        };
        run_load(git, ctx, &load, &|intent| sent.lock().unwrap().push(intent));
        sent.into_inner().unwrap()
    }

    #[test]
    fn with_no_hosts_nothing_is_read_run_or_written() {
        let tmp = TempDir::new("forge-off");
        let cache_dir = tmp.path().join("forge");
        let git = FakeRunner::new();
        let gh = Arc::new(FakeGh::new());
        let ctx = context(Arc::clone(&gh), Some(cache_dir.clone()));
        let sent = run(
            &git,
            &ctx,
            &[],
            &["main".into()],
            true,
            Some(tmp.path().into()),
        );
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
        let sent = run(&git, &ctx, &hosts(), &["main".into()], true, None);
        assert_eq!(sent, [AppIntent::ForgeRepoNone { tag: tag() }]);
        assert_eq!(gh.calls(), []);
    }

    #[test]
    fn the_cache_is_sent_first_then_the_fetch_which_is_cached() {
        let tmp = TempDir::new("forge-load");
        let cache_dir = tmp.path().join("forge");
        let gh = Arc::new(FakeGh::new().reply(answer(STATES)));
        let ctx = context(Arc::clone(&gh), Some(cache_dir.clone()));
        let branches: Vec<BranchName> = vec!["main".into()];
        let top = Some(tmp.path().to_path_buf());
        let repo = ForgeRepo {
            host: "GitHub.com".into(),
            owner: "acme".into(),
            name: "widgets".into(),
        };

        let git = origin(b"https://GitHub.com/acme/widgets.git");
        let sent = run(&git, &ctx, &hosts(), &branches, true, top.clone());
        let [cached, fetched] = sent.try_into().unwrap();
        assert_eq!(
            cached,
            AppIntent::PullRequestsCached {
                tag: tag(),
                repo: repo.clone(),
                badges: None,
            }
        );
        let AppIntent::PullRequestsFetched {
            result: Ok(badges), ..
        } = fetched
        else {
            panic!("{fetched:?}");
        };
        assert_eq!(badges[&branches[0]].state, PrState::Open);
        // The configured, lower-case host is the one asked.
        assert_eq!(gh.calls()[0].host, "github.com");
        assert_eq!(gh.calls()[0].cwd, cache_dir);

        // The next load, knowing the repository, reads what that one
        // stored and runs nothing: no git, no gh.
        let git = FakeRunner::new();
        let sent = run_known(&git, &ctx, &hosts(), &branches, false, top, Some(&repo));
        git.assert_done();
        assert_eq!(
            sent,
            [AppIntent::PullRequestsCached {
                tag: tag(),
                repo,
                badges: Some(badges),
            }]
        );
        assert_eq!(gh.calls().len(), 1);
    }

    /// `gh` holds a token and prints it; none of it may reach the log, the
    /// state, the cache or the screen.
    #[test]
    fn a_token_never_leaves_gh() {
        const SENTINEL: &str = "ghp_SENTINEL";
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
        let gh = Arc::new(
            FakeGh::new()
                .with_inherited_env("GH_TOKEN", SENTINEL)
                .reply(noisy(answer(STATES)))
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
        let top = Some(tmp.path().to_path_buf());
        for _ in 0..2 {
            state.forge.want_refresh();
            let effects = state.load_pull_requests();
            assert_eq!(effects.len(), 1);
            let load_tag = match &effects[0] {
                crate::effect::Effect::LoadPullRequests { tag, .. } => *tag,
                other => panic!("{other:?}"),
            };
            let git = origin(b"https://x:pw_USERINFO@github.com/acme/widgets");
            let sent = Mutex::new(Vec::new());
            let load = Load {
                tag: load_tag,
                hosts: &hosts(),
                branches: &branches,
                refresh: true,
                known: None,
                top: top.clone(),
                now: 1,
            };
            run_load(&git, &ctx, &load, &|i| sent.lock().unwrap().push(i));
            for intent in sent.into_inner().unwrap() {
                state.update(intent);
            }
        }
        // The token was there to leak.
        assert!(gh.saw_env("GH_TOKEN", SENTINEL));
        assert_eq!(
            state.forge.status,
            ForgeView::Problem(ForgeStatus::NotLoggedIn)
        );
        assert!(!state.forge.badges.is_empty());
        for event in events.lock().unwrap().iter() {
            let text = format!("{event:?}");
            assert!(
                !text.contains(SENTINEL) && !text.contains("pw_USERINFO"),
                "{text}"
            );
        }
        let text = format!("{state:?}");
        assert!(
            !text.contains(SENTINEL) && !text.contains("pw_USERINFO"),
            "state leaks"
        );
        for entry in std::fs::read_dir(&cache_dir).unwrap() {
            let bytes = std::fs::read(entry.unwrap().path()).unwrap();
            let text = String::from_utf8_lossy(&bytes);
            assert!(
                !text.contains(SENTINEL) && !text.contains("pw_USERINFO"),
                "{text}"
            );
        }
        let rows = crate::branches_view::tests::draw_branches(&state, 80, 12);
        assert!(rows.iter().all(|r| !r.contains(SENTINEL)), "{rows:?}");
        assert!(
            rows.iter().any(|r| r.contains("GitHub: not logged in")),
            "{rows:?}"
        );
    }
}
