//! The I/O behind the operation's one effect: looking for work done since
//! the operation stopped, off the event loop, before a Skip throws it away.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use super::OperationIntent;
use crate::background;
use crate::effect::Tag;
use crate::git::GitRunner;
use crate::git::commit_files::CommitId;
use crate::git::repo_state::Operation;
use crate::git::stop_work::read_stop_work;
use crate::intent::AppIntent;

/// Look for work done since `operation` stopped and report it as
/// `OperationIntent::StopWorkChecked`, with the `head` Skip was pressed
/// from.
pub(crate) fn check_stop_work(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    operation: Operation,
    head: Option<CommitId>,
) {
    background::run(
        tx,
        move || read_stop_work(runner.as_ref(), operation),
        // A panic still reports back, as work found, or the busy counter
        // would stay raised and Skip would never ask.
        background::crashed("Checking for changes since the stop"),
        move |result| {
            AppIntent::Operation(OperationIntent::StopWorkChecked {
                tag,
                operation,
                head,
                result,
            })
        },
    );
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;

    use super::*;
    use crate::effect::{Effect, Runtime};
    use crate::git::fake::{FakeRunner, ok};

    const TAG: Tag = Tag {
        generation: 1,
        seq: 4,
        user: true,
    };

    #[tokio::test]
    async fn check_stop_work_answers_with_the_operation_head_and_result() {
        let head = CommitId::parse("aa11").unwrap();
        let runner = Arc::new(
            FakeRunner::new()
                .expect(&["rev-parse", "--absolute-git-dir"], ok(b"/r/.git\n"))
                .expect(
                    &[
                        "status",
                        "--porcelain=v2",
                        "--branch",
                        "-z",
                        "--untracked-files=all",
                    ],
                    ok(b"# branch.oid aa11\0# branch.head (detached)\0"),
                )
                .expect(&["ls-files", "--resolve-undo", "-z"], ok(b"")),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);

        runtime.spawn(Effect::CheckStopWork {
            tag: TAG,
            operation: Operation::Rebase,
            head: Some(head.clone()),
        });

        assert_eq!(
            rx.recv().await,
            Some(AppIntent::Operation(OperationIntent::StopWorkChecked {
                tag: TAG,
                operation: Operation::Rebase,
                head: Some(head),
                result: Ok(false),
            }))
        );
        runner.assert_done();
    }

    #[tokio::test]
    async fn a_panicking_check_still_reports_back_as_an_error() {
        // No expectations queued, so the fake panics on the call.
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(FakeRunner::new()), tx);
        runtime.spawn(Effect::CheckStopWork {
            tag: TAG,
            operation: Operation::CherryPick,
            head: None,
        });
        match rx.recv().await {
            Some(AppIntent::Operation(OperationIntent::StopWorkChecked {
                tag,
                operation: Operation::CherryPick,
                head: None,
                result: Err(message),
            })) => {
                assert_eq!(tag, TAG);
                assert!(
                    message.contains("Checking for changes since the stop crashed"),
                    "{message}"
                );
            }
            other => panic!("expected a failed StopWorkChecked, got {other:?}"),
        }
    }
}
