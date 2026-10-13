//! The I/O behind the Commits panel's effects: a page of the log, the
//! merged/pushed markers, the branch tips and a commit's files, each read
//! off the event loop and reported back with the load's tag.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use super::PAGE;
use super::git::read_tips;
use crate::effect::{Tag, unix_now};
use crate::git::GitRunner;
use crate::git::commit_files::{CommitRange, commit_files};
use crate::git::diff::terminal_safe;
use crate::git::log::{LogQuery, log_page, markers};
use crate::intent::AppIntent;

/// Read a page of `HEAD`'s log, from `tip` when set, narrowed to `query`,
/// and report it as `CommitsLoaded`.
pub(crate) fn load(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    tip: Option<String>,
    skip: usize,
    query: LogQuery,
) {
    tokio::spawn(async move {
        // A panic still reports back, or the panel would wait forever.
        let result = tokio::task::spawn_blocking(move || {
            let page = log_page(runner.as_ref(), tip.as_deref(), skip, PAGE, &query)?;
            Ok((page, unix_now()))
        })
        .await
        .unwrap_or_else(|e| Err(terminal_safe(format!("git log crashed: {e}").as_bytes())));
        let _ = tx.send(AppIntent::CommitsLoaded { tag, skip, result });
    });
}

/// Read which commits are unpushed or unmerged and report them as
/// `MarkersLoaded`.
pub(crate) fn load_markers(runner: Arc<dyn GitRunner>, tx: UnboundedSender<AppIntent>, tag: Tag) {
    tokio::spawn(async move {
        // A panic still reports back, or the panel would wait forever.
        let result = tokio::task::spawn_blocking(move || markers(runner.as_ref()))
            .await
            .unwrap_or_else(|e| {
                Err(terminal_safe(
                    format!("git rev-list crashed: {e}").as_bytes(),
                ))
            });
        let _ = tx.send(AppIntent::MarkersLoaded { tag, result });
    });
}

/// Read where every local branch points and report it as
/// `BranchTipsLoaded`.
pub(crate) fn load_tips(runner: Arc<dyn GitRunner>, tx: UnboundedSender<AppIntent>, tag: Tag) {
    tokio::spawn(async move {
        // A panic still reports back, or the panel would wait forever.
        let result = tokio::task::spawn_blocking(move || read_tips(runner.as_ref()))
            .await
            .unwrap_or_else(|e| {
                Err(terminal_safe(
                    format!("git for-each-ref crashed: {e}").as_bytes(),
                ))
            });
        let _ = tx.send(AppIntent::BranchTipsLoaded { tag, result });
    });
}

/// List the files `range` changed and report them as `CommitFilesLoaded`.
pub(crate) fn load_files(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    range: CommitRange,
) {
    tokio::spawn(async move {
        // A panic still reports back, or the busy counter would stay raised.
        let result = tokio::task::spawn_blocking(move || commit_files(runner.as_ref(), &range))
            .await
            .unwrap_or_else(|e| {
                Err(terminal_safe(
                    format!("git diff-tree crashed: {e}").as_bytes(),
                ))
            });
        let _ = tx.send(AppIntent::CommitFilesLoaded { tag, result });
    });
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::mpsc;

    use crate::effect::{Effect, Runtime, Tag};
    use crate::git::commit_files::CommitRange;
    use crate::git::fake::{FakeRunner, ok};
    use crate::git::log::LogQuery;
    use crate::intent::AppIntent;
    use crate::test_support::{HostilePanic, background};

    #[tokio::test]
    async fn load_commits_reports_the_page_with_the_same_tag_skip_and_a_time() {
        let runner = Arc::new(FakeRunner::new().expect(
            &[
                "log",
                "--no-show-signature",
                "--no-color",
                "-z",
                "--format=%H%x1f%h%x1f%P%x1f%at%x1f%an%x1f%s",
                "--skip=200",
                "-n",
                "200",
                "abc",
                "--",
            ],
            ok(b"abc1\x1fabc\x1f\x1f7\x1fAnn\x1fsubj\0"),
        ));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);

        runtime.spawn(Effect::LoadCommits {
            tag: background(5),
            tip: Some("abc".into()),
            skip: 200,
            query: LogQuery::default(),
        });

        match rx.recv().await {
            Some(AppIntent::CommitsLoaded {
                tag,
                skip,
                result: Ok((page, now)),
            }) => {
                assert_eq!((tag, skip), (background(5), 200));
                assert_eq!(page.rows[0].subject, "subj");
                assert!(page.done);
                assert!(now > 1_600_000_000, "{now}");
            }
            other => panic!("expected a loaded page, got {other:?}"),
        }
        runner.assert_done();
    }

    #[tokio::test]
    async fn load_commits_passes_its_query_to_git() {
        let runner = Arc::new(FakeRunner::new().expect(
            &[
                "log",
                "--no-show-signature",
                "--no-color",
                "-z",
                "--format=%H%x1f%h%x1f%P%x1f%at%x1f%an%x1f%s",
                "--skip=0",
                "-n",
                "200",
                "--author=Ann",
                "--fixed-strings",
                "HEAD",
                "--",
            ],
            ok(b""),
        ));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);

        runtime.spawn(Effect::LoadCommits {
            tag: background(5),
            tip: None,
            skip: 0,
            query: LogQuery {
                path: None,
                author: Some("Ann".into()),
            },
        });

        assert!(matches!(
            rx.recv().await,
            Some(AppIntent::CommitsLoaded { result: Ok(_), .. })
        ));
        runner.assert_done();
    }

    #[tokio::test]
    async fn panicking_commit_and_marker_loads_still_report_back() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(HostilePanic), tx);

        runtime.spawn(Effect::LoadCommits {
            tag: background(6),
            tip: None,
            skip: 0,
            query: LogQuery::default(),
        });
        match rx.recv().await {
            Some(AppIntent::CommitsLoaded {
                tag,
                result: Err(message),
                ..
            }) => {
                assert_eq!(tag, background(6));
                assert!(!message.contains(char::is_control), "{message:?}");
            }
            other => panic!("expected a failed CommitsLoaded, got {other:?}"),
        }

        runtime.spawn(Effect::LoadMarkers { tag: background(7) });
        match rx.recv().await {
            Some(AppIntent::MarkersLoaded {
                tag,
                result: Err(message),
            }) => {
                assert_eq!(tag, background(7));
                assert!(!message.contains(char::is_control), "{message:?}");
            }
            other => panic!("expected a failed MarkersLoaded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn load_commit_files_reports_back_with_the_same_tag_even_on_a_panic() {
        use crate::git::commit_files::{CommitFile, CommitId};
        let range = CommitRange {
            oid: CommitId::parse("abc").unwrap(),
            parent: None,
        };
        let runner = Arc::new(FakeRunner::new().expect(
            &[
                "diff-tree",
                "-r",
                "-z",
                "--no-commit-id",
                "--name-status",
                "-M",
                "--root",
                "abc",
                "--",
            ],
            ok(b"A\0new\0"),
        ));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 2,
            seq: 8,
            user: true,
        };

        runtime.spawn(Effect::LoadCommitFiles {
            tag,
            range: range.clone(),
        });

        let new = CommitFile {
            status: 'A',
            path: "new".into(),
            orig_path: None,
        };
        assert_eq!(
            rx.recv().await,
            Some(AppIntent::CommitFilesLoaded {
                tag,
                result: Ok(vec![new])
            })
        );
        runner.assert_done();

        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(HostilePanic), tx);
        runtime.spawn(Effect::LoadCommitFiles { tag, range });
        match rx.recv().await {
            Some(AppIntent::CommitFilesLoaded {
                tag: got,
                result: Err(message),
            }) => {
                assert_eq!(got, tag);
                assert!(
                    message.starts_with("git diff-tree crashed: "),
                    "{message:?}"
                );
                assert!(!message.contains(char::is_control), "{message:?}");
            }
            other => panic!("expected a failed CommitFilesLoaded, got {other:?}"),
        }
    }
}
