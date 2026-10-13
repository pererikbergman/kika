//! Blocking work run off the event loop. One place owns the spawn, the
//! panic conversion and the send, so no effect can forget to report back:
//! an effect states only its work, what a panic becomes, and which intent
//! carries the outcome.

use std::fmt::Display;

use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinError;

use crate::git::safe_text::terminal_safe;
use crate::intent::AppIntent;

/// Run `work` on the blocking pool and send what `reply` makes of its
/// outcome. `None` sends nothing: the base form for the effects that report
/// only on success, only on panic, or only when a condition holds.
pub(crate) fn run_with<T, W, R>(tx: UnboundedSender<AppIntent>, work: W, reply: R)
where
    T: Send + 'static,
    W: FnOnce() -> T + Send + 'static,
    R: FnOnce(Result<T, JoinError>) -> Option<AppIntent> + Send + 'static,
{
    tokio::spawn(async move {
        let outcome = tokio::task::spawn_blocking(work).await;
        if let Some(intent) = reply(outcome) {
            // A closed channel means the loop is gone; nobody is left to tell.
            let _ = tx.send(intent);
        }
    });
}

/// The common form: a panic becomes `on_panic(e)`, and `reply` turns the
/// value into exactly one intent, so the reducer always hears back.
pub(crate) fn run<T, W, P, R>(tx: UnboundedSender<AppIntent>, work: W, on_panic: P, reply: R)
where
    T: Send + 'static,
    W: FnOnce() -> T + Send + 'static,
    P: FnOnce(JoinError) -> T + Send + 'static,
    R: FnOnce(T) -> AppIntent + Send + 'static,
{
    run_with(tx, work, move |outcome| {
        Some(reply(outcome.unwrap_or_else(on_panic)))
    });
}

/// The usual panic value, `Err("<what> crashed: <panic>")`. The panic text
/// may quote a repo path and is drawn on screen, so it is sanitised.
pub(crate) fn crashed<T>(
    what: impl Display + Send + 'static,
) -> impl FnOnce(JoinError) -> Result<T, String> + Send + 'static {
    move |e| Err(terminal_safe(format!("{what} crashed: {e}").as_bytes()))
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;

    use super::{crashed, run, run_with};
    use crate::effect::Tag;
    use crate::intent::AppIntent;

    const TAG: Tag = Tag {
        generation: 0,
        seq: 1,
        user: true,
    };

    fn loaded(result: Result<Vec<crate::git::branches::Branch>, String>) -> AppIntent {
        AppIntent::BranchesLoaded { tag: TAG, result }
    }

    #[tokio::test]
    async fn a_panicking_work_sends_one_crashed_error() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        run(
            tx,
            || -> Result<_, String> { panic!("boom") },
            crashed("git x"),
            loaded,
        );
        match rx.recv().await {
            Some(AppIntent::BranchesLoaded {
                result: Err(message),
                ..
            }) => assert!(message.starts_with("git x crashed: "), "{message}"),
            other => panic!("unexpected {other:?}"),
        }
        // The sender went with the task: exactly one intent was sent.
        assert_eq!(rx.recv().await, None);
    }

    #[tokio::test]
    async fn a_successful_work_sends_its_reply() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        run(tx, || Ok(vec![]), crashed("git x"), loaded);
        assert_eq!(rx.recv().await, Some(loaded(Ok(vec![]))));
        assert_eq!(rx.recv().await, None);
    }

    #[tokio::test]
    async fn run_with_sends_nothing_when_reply_declines() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        run_with(tx, || 7, |_| None);
        // `recv` returns `None` only once the task has dropped the sender.
        assert_eq!(rx.recv().await, None);
    }

    #[tokio::test]
    async fn crashed_strips_control_bytes_from_the_panic_text() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        run(
            tx,
            || -> Result<_, String> { panic!("evil\x1b[2Jpath") },
            crashed("git x"),
            loaded,
        );
        match rx.recv().await {
            Some(AppIntent::BranchesLoaded {
                result: Err(message),
                ..
            }) => {
                assert!(message.starts_with("git x crashed: "), "{message}");
                assert!(!message.contains('\x1b'), "{message:?}");
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
