//! The I/O behind the pull-request effects: loading the branches' badges,
//! creating a pull request through `gh`, and starting the browser on a
//! forge URL. Each answers with an intent; none blocks the event loop.

use std::ffi::OsString;
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use crate::background;
use crate::effect::{Tag, log_program};
use crate::features::pull_requests::PullRequestsIntent;
use crate::intent::AppIntent;
use kika_forge::task::{ForgeContext, Load, LoadEvent, run_load};
use kika_forge::{CreateError, ForgeHost, ForgeRepo, ForgeStatus};
use kika_git::GitRunner;
use kika_git::branches::BranchName;
use kika_git::recording::{CommandOutcome, CommandSink};
use kika_git::safe_text::terminal_safe;

/// Send the cached badges of the remote's forge repository, then, with
/// `refresh`, ask the forge for `branches`. With no `forge` client or no
/// `hosts` it only answers `PullRequestsIntent::RepoNone`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn load(
    forge: Option<ForgeContext>,
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    hosts: Vec<ForgeHost>,
    branches: Vec<BranchName>,
    refresh: bool,
    known: Option<ForgeRepo>,
    limited: Vec<(ForgeHost, i64)>,
) {
    match forge.filter(|_| !hosts.is_empty()) {
        // Off: nothing is read, run or written.
        None => {
            let _ = tx.send(AppIntent::PullRequests(PullRequestsIntent::RepoNone {
                tag,
            }));
        }
        Some(forge) => {
            let send = tx.clone();
            background::run_with(
                tx,
                move || {
                    let load = Load {
                        hosts: &hosts,
                        branches: &branches,
                        refresh,
                        known: known.as_ref(),
                        limited: &limited,
                        top: std::fs::canonicalize(runner.workdir()).ok(),
                        now: forge.now(),
                    };
                    run_load(runner.as_ref(), &forge, &load, &|event| {
                        let _ = send.send(load_intent(tag, event));
                    });
                },
                // A panic still ends the call, or no refresh would run again.
                move |done| {
                    (done.is_err() && refresh).then_some(AppIntent::PullRequests(
                        PullRequestsIntent::Fetched {
                            tag,
                            result: Err(ForgeStatus::Failed),
                            limited_until: None,
                        },
                    ))
                },
            );
        }
    }
}

/// The intent that reports `event` of the load tagged `tag`.
pub(crate) fn load_intent(tag: Tag, event: LoadEvent) -> AppIntent {
    match event {
        LoadEvent::RepoNone => AppIntent::PullRequests(PullRequestsIntent::RepoNone { tag }),
        LoadEvent::Cached { repo, badges } => {
            AppIntent::PullRequests(PullRequestsIntent::Cached { tag, repo, badges })
        }
        LoadEvent::Fetched {
            result,
            limited_until,
        } => AppIntent::PullRequests(PullRequestsIntent::Fetched {
            tag,
            result,
            limited_until,
        }),
    }
}

/// Open a pull request on `repo` through `gh`; always answered by one
/// `PullRequestsIntent::Created`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn create(
    forge: Option<ForgeContext>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    host: ForgeHost,
    repo: ForgeRepo,
    head: String,
    base: String,
    title: String,
) {
    // No forge client: nothing may reach the network. The client itself
    // refuses a repository `[forge.allow]` leaves out.
    let Some(forge) = forge else {
        let _ = tx.send(AppIntent::PullRequests(PullRequestsIntent::Created {
            tag,
            result: Err(CreateError::Status(ForgeStatus::Failed)),
        }));
        return;
    };
    background::run(
        tx,
        move || forge.create_pull(&host, &repo, &head, &base, &title),
        // A panic still answers, or the create would stay in flight.
        |_| Err(CreateError::Status(ForgeStatus::Failed)),
        move |result| AppIntent::PullRequests(PullRequestsIntent::Created { tag, result }),
    );
}

/// Start `argv` (the browser and the URL last) with no shell and nothing
/// to read or write, logged in the Command log. It is waited for on a
/// thread of its own, so a browser that stays open holds nothing up;
/// only a failure to start is reported, as `PullRequestsIntent::OpenUrlFailed`.
pub(crate) fn open_url(argv: Vec<OsString>, sink: CommandSink, tx: UnboundedSender<AppIntent>) {
    let finished = log_program(&argv, sink);
    let Some((program, args)) = argv.split_first() else {
        finished(CommandOutcome::NotStarted);
        let _ = tx.send(AppIntent::PullRequests(PullRequestsIntent::OpenUrlFailed(
            "No browser is configured".into(),
        )));
        return;
    };
    let spawned = kika_git::program::command(program).and_then(|mut command| {
        command
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
    });
    match spawned {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let outcome = match child.wait() {
                    Ok(status) if status.success() => CommandOutcome::Ok,
                    Ok(status) => CommandOutcome::Failed(Some(status.to_string())),
                    Err(_) => CommandOutcome::NotStarted,
                };
                finished(outcome);
            });
        }
        Err(e) => {
            finished(CommandOutcome::NotStarted);
            let program = terminal_safe(program.as_encoded_bytes());
            let _ = tx.send(AppIntent::PullRequests(PullRequestsIntent::OpenUrlFailed(
                format!("Could not start {program}: {e}"),
            )));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    #[cfg(unix)]
    use std::time::Duration;

    use tokio::sync::mpsc;

    use crate::effect::{Effect, Runtime};
    use crate::features::pull_requests::PullRequestsIntent;
    use crate::intent::AppIntent;
    use crate::test_support::forge_tag;
    use kika_forge::task::ForgeContext;
    use kika_forge::{CreateError, ForgeHost, ForgeRepo, ForgeStatus};
    use kika_git::fake::FakeRunner;
    use kika_git::recording::{CommandEvent, CommandOutcome, CommandSink};

    #[tokio::test]
    async fn a_pull_request_load_with_no_hosts_runs_nothing_and_writes_nothing() {
        use kika_forge::github::FakeGh;
        use kika_git::recording::CommandSink;

        let tmp = kika_git::fake::TempDir::new("forge-effect-off");
        let cache_dir = tmp.path().join("forge");
        let git = Arc::new(FakeRunner::new());
        let gh = Arc::new(FakeGh::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(git.clone(), tx).with_forge(Some(ForgeContext {
            gh: gh.clone(),
            sink: CommandSink::new(),
            cache_dir: Some(cache_dir.clone()),
        }));
        let load = |hosts| Effect::LoadPullRequests {
            tag: forge_tag(),
            hosts,
            branches: vec!["main".into()],
            refresh: true,
            known: None,
            limited: vec![],
        };
        runtime.spawn(load(vec![]));
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::PullRequests(PullRequestsIntent::RepoNone {
                tag: forge_tag()
            }))
        );
        // Without a forge client, even named hosts reach nobody.
        let (tx, mut rx) = mpsc::unbounded_channel();
        let bare = Runtime::new(git.clone(), tx);
        bare.spawn(load(vec![ForgeHost::parse("github.com").unwrap()]));
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::PullRequests(PullRequestsIntent::RepoNone {
                tag: forge_tag()
            }))
        );
        git.assert_done();
        assert_eq!(gh.calls(), []);
        assert!(!cache_dir.exists());
    }

    #[test]
    fn load_intent_carries_the_tag_and_each_event_as_is() {
        use super::load_intent;
        use kika_forge::task::LoadEvent;
        use std::collections::HashMap;

        let repo = ForgeRepo {
            host: "github.com".into(),
            owner: "acme".into(),
            name: "widgets".into(),
        };
        let badges = HashMap::from([(
            "main".into(),
            kika_forge::PrBadge {
                number: 7,
                state: kika_forge::PrState::Open,
                ci: None,
            },
        )]);
        assert_eq!(
            load_intent(forge_tag(), LoadEvent::RepoNone),
            AppIntent::PullRequests(PullRequestsIntent::RepoNone { tag: forge_tag() })
        );
        assert_eq!(
            load_intent(
                forge_tag(),
                LoadEvent::Cached {
                    repo: repo.clone(),
                    badges: Some(badges.clone()),
                }
            ),
            AppIntent::PullRequests(PullRequestsIntent::Cached {
                tag: forge_tag(),
                repo,
                badges: Some(badges.clone()),
            })
        );
        for (result, limited_until) in [
            (Ok(badges), None),
            (Err(ForgeStatus::RateLimited), Some(1_700_000_000)),
        ] {
            assert_eq!(
                load_intent(
                    forge_tag(),
                    LoadEvent::Fetched {
                        result: result.clone(),
                        limited_until,
                    }
                ),
                AppIntent::PullRequests(PullRequestsIntent::Fetched {
                    tag: forge_tag(),
                    result,
                    limited_until,
                })
            );
        }
    }

    /// `gh` holds a token and prints it; none of it may reach the app
    /// state or the screen. The log, the cache and the forge state are
    /// checked by `forge::task`'s own test.
    #[test]
    fn a_token_never_reaches_the_app_state_or_the_screen() {
        use super::load_intent;
        use crate::test_support::{draw_branches, forge_state};
        use kika_forge::github::fixtures::{SECRET_TOKEN as SENTINEL, leaky_gh};
        use kika_forge::task::{Load, run_load};
        use kika_forge::{ForgeStatus, ForgeView};
        use kika_git::branches::BranchName;
        use kika_git::fake::{TempDir, ok};
        use kika_git::forge_remote::UPSTREAMS_ARGV;

        let tmp = TempDir::new("forge-secret-app");
        let gh = Arc::new(leaky_gh());
        let ctx = ForgeContext {
            gh: gh.clone(),
            sink: CommandSink::new(),
            cache_dir: Some(tmp.path().join("forge")),
        };
        let branches: Vec<BranchName> = vec!["main".into()];
        let mut state = forge_state(&branches);
        for _ in 0..2 {
            state.forge.want_refresh();
            let effects = state.load_pull_requests();
            let [
                Effect::LoadPullRequests {
                    tag,
                    hosts,
                    branches,
                    refresh,
                    known,
                    limited,
                },
            ] = effects.as_slice()
            else {
                panic!("{effects:?}");
            };
            let git = FakeRunner::new()
                .expect(&["remote"], ok(b"origin\n"))
                .expect(
                    &["config", "-z", "--get", "remote.origin.url"],
                    ok(b"https://x:pw_USERINFO@github.com/acme/widgets\0"),
                )
                .expect(&UPSTREAMS_ARGV, ok(b""));
            let load = Load {
                hosts,
                branches,
                refresh: *refresh,
                known: known.as_ref(),
                limited,
                top: Some(tmp.path().to_path_buf()),
                now: 1_700_000_000,
            };
            let sent = Mutex::new(Vec::new());
            run_load(&git, &ctx, &load, &|event| {
                sent.lock().unwrap().push(load_intent(*tag, event));
            });
            git.assert_done();
            for intent in sent.into_inner().unwrap() {
                state.update(intent);
            }
        }
        // The token was there to leak, and the second load got as far as
        // GitHub's refusal.
        assert!(gh.saw_env("GH_TOKEN", SENTINEL));
        assert_eq!(gh.calls().len(), 4);
        assert_eq!(
            state.forge.status,
            ForgeView::Problem(ForgeStatus::NotLoggedIn)
        );
        let secrets = [SENTINEL, "pw_USERINFO", "etag_VALUE"];
        let text = format!("{state:?}");
        assert!(secrets.iter().all(|s| !text.contains(s)), "state leaks");
        let rows = draw_branches(&state, 80, 12);
        assert!(
            rows.iter().all(|r| secrets.iter().all(|s| !r.contains(s))),
            "{rows:?}"
        );
        assert!(
            rows.iter().any(|r| r.contains("GitHub: not logged in")),
            "{rows:?}"
        );
    }

    #[tokio::test]
    async fn a_create_runs_gh_in_the_cache_dir_and_answers_once() {
        use kika_forge::github::fixtures::CREATE_201;
        use kika_forge::github::{FakeGh, GhCall, answer};

        let tmp = kika_git::fake::TempDir::new("forge-effect-create");
        let cache_dir = tmp.path().join("forge");
        let git = Arc::new(FakeRunner::new());
        let gh = Arc::new(FakeGh::new().reply(answer(CREATE_201)));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(git.clone(), tx).with_forge(Some(ForgeContext {
            gh: gh.clone(),
            sink: CommandSink::new(),
            cache_dir: Some(cache_dir.clone()),
        }));
        let create = Effect::CreatePullRequest {
            tag: forge_tag(),
            host: ForgeHost::parse("github.com").unwrap(),
            repo: ForgeRepo {
                host: "github.com".into(),
                owner: "acme".into(),
                name: "widgets".into(),
            },
            head: "me:topic".into(),
            base: "main".into(),
            title: "Add login".into(),
        };
        runtime.spawn(create.clone());
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::PullRequests(PullRequestsIntent::Created {
                tag: forge_tag(),
                result: Ok((42, "https://github.com/acme/widgets/pull/42".into())),
            }))
        );
        git.assert_done();
        let [call] = gh.calls().try_into().unwrap();
        assert_eq!(call.cwd, cache_dir);
        assert!(matches!(call.kind, GhCall::Create { .. }));

        // Without a forge client, nothing is asked.
        let (tx, mut rx) = mpsc::unbounded_channel();
        Runtime::new(git, tx).spawn(create);
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::PullRequests(PullRequestsIntent::Created {
                tag: forge_tag(),
                result: Err(CreateError::Status(ForgeStatus::Failed)),
            }))
        );
    }

    #[tokio::test]
    async fn a_create_on_a_repository_the_allow_list_leaves_out_runs_no_gh() {
        use kika_forge::allow::AllowRule;
        use kika_forge::github::FakeGh;

        let git = Arc::new(FakeRunner::new());
        let gh = Arc::new(FakeGh::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(git.clone(), tx).with_forge(Some(ForgeContext {
            gh: gh.clone(),
            sink: CommandSink::new(),
            cache_dir: None,
        }));
        let other = AllowRule::parse("other").unwrap();
        runtime.spawn(Effect::CreatePullRequest {
            tag: forge_tag(),
            host: ForgeHost::parse("github.com")
                .unwrap()
                .with_allow(vec![other]),
            repo: ForgeRepo {
                host: "github.com".into(),
                owner: "acme".into(),
                name: "widgets".into(),
            },
            head: "me:topic".into(),
            base: "main".into(),
            title: "Add login".into(),
        });
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::PullRequests(PullRequestsIntent::Created {
                tag: forge_tag(),
                result: Err(CreateError::Status(ForgeStatus::Failed)),
            }))
        );
        git.assert_done();
        assert_eq!(gh.calls(), []);
    }

    #[tokio::test]
    async fn a_browser_that_cannot_start_says_why_and_is_logged() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = CommandSink::new();
        let keep = Arc::clone(&events);
        sink.connect(move |ev| keep.lock().unwrap().push(ev));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(FakeRunner::new()), tx).with_command_sink(sink.clone());
        let url = "https://github.com/acme/widgets/pull/7";
        runtime.spawn(Effect::OpenUrl {
            argv: vec!["/nonexistent/kika-browser".into(), url.into()],
        });
        match rx.recv().await {
            Some(AppIntent::PullRequests(PullRequestsIntent::OpenUrlFailed(message))) => {
                assert!(
                    message.starts_with("Could not start /nonexistent/kika-browser: "),
                    "{message}"
                );
            }
            other => panic!("{other:?}"),
        }
        let events = events.lock().unwrap();
        match events.as_slice() {
            [
                CommandEvent::Started { id: a, line, .. },
                CommandEvent::Finished {
                    id: b,
                    outcome: CommandOutcome::NotStarted,
                    ..
                },
            ] => {
                assert_eq!(a, b);
                assert_eq!(line, &format!("/nonexistent/kika-browser {url}"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_browser_that_starts_reports_nothing_back() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(FakeRunner::new()), tx);
        // `true` stands in for a browser: it takes the URL and exits.
        runtime.spawn(Effect::OpenUrl {
            argv: vec!["true".into(), "https://github.com/o/r/pull/1".into()],
        });
        let waited = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await;
        assert!(waited.is_err(), "{waited:?}");
    }
}
