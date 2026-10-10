use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use crate::git::GitRunner;
use crate::git::status::read_status;
use crate::intent::AppIntent;

/// Identifies an effect's result so the reducer can drop stale ones.
/// `generation` changes when the repository changes; `seq` orders loads of the
/// same kind; `user` marks effects that count toward the busy indicator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tag {
    pub generation: u64,
    pub seq: u64,
    pub user: bool,
}

/// Work the reducer asks for. Executed by [`Runtime`], never by `update`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    LoadStatus { tag: Tag },
}

/// Executes effects off the UI loop and reports each result as an intent.
/// Makes no decisions: staleness and bookkeeping belong to the reducer.
pub struct Runtime {
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
}

impl Runtime {
    pub fn new(runner: Arc<dyn GitRunner>, tx: UnboundedSender<AppIntent>) -> Self {
        Self { runner, tx }
    }

    /// Start an effect on tokio's blocking pool and return at once. The
    /// result arrives later on the channel.
    pub fn spawn(&self, effect: Effect) {
        let runner = Arc::clone(&self.runner);
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || match effect {
            Effect::LoadStatus { tag } => {
                let result = read_status(runner.as_ref());
                // A closed channel means the app is quitting; nothing to report to.
                let _ = tx.send(AppIntent::StatusLoaded { tag, result });
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;

    use super::*;
    use crate::git::FileEntry;
    use crate::git::fake::{FakeRunner, ok};
    use crate::git::status::EntryKind;

    #[tokio::test]
    async fn load_status_reports_back_with_the_same_tag() {
        let runner = Arc::new(FakeRunner::new().expect(
            &["status", "--porcelain=v2", "-z", "--untracked-files=all"],
            ok(b"? new_file\0"),
        ));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 2,
            seq: 5,
            user: true,
        };

        runtime.spawn(Effect::LoadStatus { tag });

        assert_eq!(
            rx.recv().await,
            Some(AppIntent::StatusLoaded {
                tag,
                result: Ok(vec![FileEntry {
                    path: "new_file".into(),
                    orig_path: None,
                    index: '?',
                    worktree: '?',
                    kind: EntryKind::Untracked,
                }]),
            })
        );
        runner.assert_done();
    }
}
