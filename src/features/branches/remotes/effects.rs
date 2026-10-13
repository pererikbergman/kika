//! The I/O behind the Remotes feature's effect: listing the remotes off the
//! event loop and reporting back with the load's tag.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use crate::background;
use crate::effect::Tag;
use crate::git::GitRunner;
use crate::git::remotes::list_remotes;
use crate::intent::AppIntent;

/// List the remotes and report them as `RemotesLoaded`.
pub(crate) fn load(runner: Arc<dyn GitRunner>, tx: UnboundedSender<AppIntent>, tag: Tag) {
    background::run(
        tx,
        move || list_remotes(runner.as_ref()),
        // A panic still reports back, or the busy counter would stay raised.
        background::crashed("git for-each-ref"),
        move |result| AppIntent::RemotesLoaded { tag, result },
    );
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::mpsc;

    use crate::effect::{Effect, Runtime, Tag};
    use crate::git::fake::{FakeRunner, ok};
    use crate::intent::AppIntent;

    #[tokio::test]
    async fn a_remotes_load_reports_back_with_the_same_tag() {
        let runner = Arc::new(
            FakeRunner::new()
                .expect(
                    &["config", "-z", "--get-regexp", r"^remote\..*\.url$"],
                    ok(b"remote.origin.url\n/srv/r\0"),
                )
                .expect(
                    &[
                        "for-each-ref",
                        "--format=%(refname)%00%(symref)",
                        "refs/remotes",
                    ],
                    ok(b"refs/remotes/origin/main\0\n"),
                ),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 1,
            seq: 5,
            user: false,
        };

        runtime.spawn(Effect::LoadRemotes { tag });

        match rx.recv().await {
            Some(AppIntent::RemotesLoaded {
                tag: got,
                result: Ok(remotes),
            }) => {
                assert_eq!(got, tag);
                assert_eq!(remotes.len(), 1);
                assert_eq!(remotes[0].branches[0].display(), "origin/main");
            }
            other => panic!("expected RemotesLoaded, got {other:?}"),
        }
        runner.assert_done();
    }
}
