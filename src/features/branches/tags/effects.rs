//! The I/O behind the Tags feature's effect: listing the tags off the event
//! loop and reporting back with the load's tag.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use crate::background;
use crate::effect::Tag;
use crate::git::GitRunner;
use crate::git::tags::list_tags;
use crate::intent::AppIntent;

/// List the tags and report them as `TagsLoaded`.
pub(crate) fn load(runner: Arc<dyn GitRunner>, tx: UnboundedSender<AppIntent>, tag: Tag) {
    background::run(
        tx,
        move || list_tags(runner.as_ref()),
        // A panic still reports back, or the busy counter would stay raised.
        background::crashed("git for-each-ref"),
        move |result| AppIntent::TagsLoaded { tag, result },
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
    async fn a_tags_load_reports_back_with_the_same_tag() {
        let oid = "1".repeat(40);
        let record = format!("refs/tags/v1\0commit\0{oid}\0\0\0\0d\0s\n");
        let list = crate::git::tags::list_command();
        let argv: Vec<String> = list
            .args()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let runner = Arc::new(FakeRunner::new().expect(&argv, ok(record.as_bytes())));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 1,
            seq: 6,
            user: false,
        };

        runtime.spawn(Effect::LoadTags { tag });

        match rx.recv().await {
            Some(AppIntent::TagsLoaded {
                tag: got,
                result: Ok(tags),
            }) => {
                assert_eq!(got, tag);
                assert_eq!(tags.len(), 1);
                assert_eq!(tags[0].name.display(), "v1");
            }
            other => panic!("expected TagsLoaded, got {other:?}"),
        }
        runner.assert_done();
    }
}
