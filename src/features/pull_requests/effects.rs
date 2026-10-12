//! The I/O behind the pull-request effects: loading the branches' badges,
//! creating a pull request through `gh`, and starting the browser on a
//! forge URL. Each answers with an intent; none blocks the event loop.

use std::ffi::OsString;
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use crate::effect::{Tag, gh_cwd, log_program, unix_now};
use crate::forge::task::{ForgeContext, Load, run_load};
use crate::forge::{CreateError, ForgeHost, ForgeRepo, ForgeStatus};
use crate::git::GitRunner;
use crate::git::branches::BranchName;
use crate::git::diff::terminal_safe;
use crate::git::recording::{CommandOutcome, CommandSink};
use crate::intent::AppIntent;

/// Send the cached badges of the remote's forge repository, then, with
/// `refresh`, ask the forge for `branches`. With no `forge` client or no
/// `hosts` it only answers `ForgeRepoNone`.
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
            let _ = tx.send(AppIntent::ForgeRepoNone { tag });
        }
        Some(forge) => {
            tokio::spawn(async move {
                let send = tx.clone();
                let done = tokio::task::spawn_blocking(move || {
                    let load = Load {
                        tag,
                        hosts: &hosts,
                        branches: &branches,
                        refresh,
                        known: known.as_ref(),
                        limited: &limited,
                        top: std::fs::canonicalize(runner.workdir()).ok(),
                        now: unix_now(),
                    };
                    run_load(runner.as_ref(), &forge, &load, &|intent| {
                        let _ = send.send(intent);
                    });
                })
                .await;
                // A panic still ends the call, or no refresh would run again.
                if done.is_err() && refresh {
                    let _ = tx.send(AppIntent::PullRequestsFetched {
                        tag,
                        result: Err(ForgeStatus::Failed),
                        limited_until: None,
                    });
                }
            });
        }
    }
}

/// Open a pull request on `repo` through `gh`; always answered by one
/// `PullRequestCreated`.
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
    // No forge client, or a repository `[forge.allow]` leaves
    // out (the model never asks for one): nothing may reach
    // the network.
    let Some(forge) = forge.filter(|_| host.allows(&repo)) else {
        let _ = tx.send(AppIntent::PullRequestCreated {
            tag,
            result: Err(CreateError::Status(ForgeStatus::Failed)),
        });
        return;
    };
    tokio::spawn(async move {
        let result = tokio::task::spawn_blocking(move || {
            let cwd = gh_cwd(&forge);
            let gh = crate::forge::github::Gh {
                runner: forge.gh.as_ref(),
                sink: &forge.sink,
                host: &host,
                cwd,
                now: unix_now(),
            };
            gh.create_pull(&repo, &head, &base, &title)
        })
        .await
        // A panic still answers, or the create would stay in flight.
        .unwrap_or(Err(CreateError::Status(ForgeStatus::Failed)));
        let _ = tx.send(AppIntent::PullRequestCreated { tag, result });
    });
}

/// Start `argv` (the browser and the URL last) with no shell and nothing
/// to read or write, logged in the Command log. It is waited for on a
/// thread of its own, so a browser that stays open holds nothing up;
/// only a failure to start is reported, as `OpenUrlFailed`.
pub(crate) fn open_url(argv: Vec<OsString>, sink: CommandSink, tx: UnboundedSender<AppIntent>) {
    let finished = log_program(&argv, sink);
    let Some((program, args)) = argv.split_first() else {
        finished(CommandOutcome::NotStarted);
        let _ = tx.send(AppIntent::OpenUrlFailed("No browser is configured".into()));
        return;
    };
    let spawned = crate::program::command(program).and_then(|mut command| {
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
            let _ = tx.send(AppIntent::OpenUrlFailed(format!(
                "Could not start {program}: {e}"
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
    use crate::forge::task::ForgeContext;
    use crate::forge::{CreateError, ForgeHost, ForgeRepo, ForgeStatus};
    use crate::git::fake::FakeRunner;
    use crate::git::recording::{CommandEvent, CommandOutcome, CommandSink};
    use crate::intent::AppIntent;
    use crate::test_support::forge_tag;

    #[tokio::test]
    async fn a_pull_request_load_with_no_hosts_runs_nothing_and_writes_nothing() {
        use crate::forge::github::FakeGh;
        use crate::git::recording::CommandSink;

        let tmp = crate::git::fake::TempDir::new("forge-effect-off");
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
            Some(AppIntent::ForgeRepoNone { tag: forge_tag() })
        );
        // Without a forge client, even named hosts reach nobody.
        let (tx, mut rx) = mpsc::unbounded_channel();
        let bare = Runtime::new(git.clone(), tx);
        bare.spawn(load(vec![ForgeHost::parse("github.com").unwrap()]));
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::ForgeRepoNone { tag: forge_tag() })
        );
        git.assert_done();
        assert_eq!(gh.calls(), []);
        assert!(!cache_dir.exists());
    }

    #[tokio::test]
    async fn a_create_runs_gh_in_the_cache_dir_and_answers_once() {
        use crate::forge::github::fixtures::CREATE_201;
        use crate::forge::github::{FakeGh, GhCall, answer};

        let tmp = crate::git::fake::TempDir::new("forge-effect-create");
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
            Some(AppIntent::PullRequestCreated {
                tag: forge_tag(),
                result: Ok((42, "https://github.com/acme/widgets/pull/42".into())),
            })
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
            Some(AppIntent::PullRequestCreated {
                tag: forge_tag(),
                result: Err(CreateError::Status(ForgeStatus::Failed)),
            })
        );
    }

    #[tokio::test]
    async fn a_create_on_a_repository_the_allow_list_leaves_out_runs_no_gh() {
        use crate::forge::allow::AllowRule;
        use crate::forge::github::FakeGh;

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
            Some(AppIntent::PullRequestCreated {
                tag: forge_tag(),
                result: Err(CreateError::Status(ForgeStatus::Failed)),
            })
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
            Some(AppIntent::OpenUrlFailed(message)) => {
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
