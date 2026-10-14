//! The I/O behind the commit popup's effects: reading `HEAD`'s message
//! for an amend, and probing whether a commit needs the terminal. Each
//! reports back with its tag, even when the read panics.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use crate::background;
use crate::effect::Tag;
use crate::features::commit::CommitIntent;
use crate::intent::AppIntent;
use crate::op::GitOp;
use crate::prompts::read_prompt_facts;
use kika_git::GitRunner;
use kika_git::head::read_head_message;

/// Read `HEAD`'s message and report it as `CommitIntent::HeadMessageLoaded`.
pub(crate) fn load_head_message(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
) {
    background::run(
        tx,
        move || read_head_message(runner.as_ref()),
        // A panic still reports back, or the busy counter would stay raised.
        background::crashed("git log"),
        move |result| AppIntent::Commit(CommitIntent::HeadMessageLoaded { tag, result }),
    );
}

/// Read whether `op` may sign or run hooks, and report it as
/// `CommitIntent::Probed`.
pub(crate) fn probe(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    tag: Tag,
    op: GitOp,
) {
    background::run(
        tx,
        move || read_prompt_facts(runner.as_ref()).ok(),
        // Errors and panics both report `None`, which hands over.
        |_| None,
        move |facts| AppIntent::Commit(CommitIntent::Probed { tag, op, facts }),
    );
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::mpsc;

    use crate::effect::{Effect, Runtime, Tag};
    use crate::features::commit::CommitIntent;
    use crate::intent::AppIntent;
    use crate::test_support::{HostilePanic, commit_op};
    use kika_git::fake::{FakeRunner, fail, ok};

    #[tokio::test]
    async fn probe_commit_reports_the_facts_with_the_same_tag_and_op() {
        let runner = Arc::new(
            FakeRunner::new()
                .expect(
                    &["config", "--type=bool", "--get", "commit.gpgsign"],
                    ok(b"true\n"),
                )
                .expect(&["rev-parse", "--git-path", "hooks"], ok(b"/nowhere\n")),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 1,
            seq: 6,
            user: true,
        };

        runtime.spawn(Effect::ProbeCommit {
            tag,
            op: commit_op(),
        });

        assert_eq!(
            rx.recv().await,
            Some(AppIntent::Commit(CommitIntent::Probed {
                tag,
                op: commit_op(),
                facts: Some(crate::prompts::PromptFacts {
                    signs: true,
                    hooks: vec![],
                }),
            }))
        );
        runner.assert_done();
    }

    #[tokio::test]
    async fn a_failed_or_panicking_probe_reports_no_facts() {
        let tag = Tag {
            generation: 0,
            seq: 1,
            user: true,
        };
        let failing = FakeRunner::new().expect(
            &["config", "--type=bool", "--get", "commit.gpgsign"],
            fail("fatal: bad config\n"),
        );
        // No expectations at all: the fake panics on the first read.
        for runner in [failing, FakeRunner::new()] {
            let (tx, mut rx) = mpsc::unbounded_channel();
            let runtime = Runtime::new(Arc::new(runner), tx);
            runtime.spawn(Effect::ProbeCommit {
                tag,
                op: commit_op(),
            });
            assert_eq!(
                rx.recv().await,
                Some(AppIntent::Commit(CommitIntent::Probed {
                    tag,
                    op: commit_op(),
                    facts: None,
                }))
            );
        }
    }

    #[tokio::test]
    async fn load_head_message_reports_back_with_the_same_tag() {
        let runner = Arc::new(FakeRunner::new().expect(
            &["log", "-1", "--format=%B", "HEAD", "--"],
            ok(b"subj\n\nbody\n\n"),
        ));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 1,
            seq: 4,
            user: true,
        };

        runtime.spawn(Effect::LoadHeadMessage { tag });

        assert_eq!(
            rx.recv().await,
            Some(AppIntent::Commit(CommitIntent::HeadMessageLoaded {
                tag,
                result: Ok("subj\n\nbody".into()),
            }))
        );
        runner.assert_done();
    }

    #[tokio::test]
    async fn a_panicking_head_message_load_still_reports_back() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(Arc::new(HostilePanic), tx);
        let tag = Tag {
            generation: 0,
            seq: 5,
            user: true,
        };

        runtime.spawn(Effect::LoadHeadMessage { tag });

        match rx.recv().await {
            Some(AppIntent::Commit(CommitIntent::HeadMessageLoaded {
                tag: got,
                result: Err(message),
            })) => {
                assert_eq!(got, tag);
                assert!(message.starts_with("git log crashed: "), "{message:?}");
                assert!(!message.contains(char::is_control), "{message:?}");
            }
            other => panic!("expected a failed HeadMessageLoaded, got {other:?}"),
        }
    }
}
