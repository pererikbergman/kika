//! Retries git commands that failed only because another process held a
//! lock, so a background `git status` elsewhere never shows as an error.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use super::runner::{GitChild, GitCommand, GitOutput, GitRunner};

/// Waits between attempts: short, since a lock is usually held briefly.
pub const LOCK_BACKOFF: [Duration; 4] = [
    Duration::from_millis(50),
    Duration::from_millis(100),
    Duration::from_millis(200),
    Duration::from_millis(400),
];

type Sleep = Box<dyn Fn(Duration) + Send + Sync>;

/// A [`GitRunner`] that runs a command again, after a pause, while it fails
/// on a lock. Streamed commands are not retried: their output is already
/// on its way to the reader.
pub struct RetryRunner {
    inner: Arc<dyn GitRunner>,
    delays: Vec<Duration>,
    sleep: Sleep,
}

impl RetryRunner {
    pub fn new(inner: Arc<dyn GitRunner>) -> Self {
        Self::with_backoff(inner, LOCK_BACKOFF.to_vec(), Box::new(std::thread::sleep))
    }

    /// Pause by `delays`, one per retry, through `sleep`; tests pass a
    /// recorder so they never wait.
    pub fn with_backoff(inner: Arc<dyn GitRunner>, delays: Vec<Duration>, sleep: Sleep) -> Self {
        Self {
            inner,
            delays,
            sleep,
        }
    }
}

/// Whether git failed because another process holds `index.lock` or a ref
/// lock, so the same command may well succeed a moment later.
pub fn is_lock_error(stderr: &[u8]) -> bool {
    let text = String::from_utf8_lossy(stderr);
    text.contains(".lock': File exists") || text.contains("cannot lock ref")
}

impl GitRunner for RetryRunner {
    fn run(&self, cmd: &GitCommand) -> io::Result<GitOutput> {
        let mut output = self.inner.run(cmd)?;
        for &delay in &self.delays {
            if output.success || !is_lock_error(&output.stderr) {
                break;
            }
            (self.sleep)(delay);
            output = self.inner.run(cmd)?;
        }
        Ok(output)
    }

    fn interrupt(&self) {
        self.inner.interrupt();
    }

    fn spawn(&self, cmd: &GitCommand) -> io::Result<GitChild> {
        self.inner.spawn(cmd)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::git::fake::{FakeChild, FakeRunner, fail, ok};

    const INDEX_LOCK: &str = "fatal: Unable to create '/r/.git/index.lock': File exists.";
    const ARGV: &[&str] = &["add", "-A"];

    /// A retrying runner over `fake` that records its sleeps instead of sleeping.
    fn retrying(fake: Arc<FakeRunner>) -> (RetryRunner, Arc<Mutex<Vec<Duration>>>) {
        let slept = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&slept);
        let runner = RetryRunner::with_backoff(
            fake,
            LOCK_BACKOFF.to_vec(),
            Box::new(move |d| record.lock().unwrap().push(d)),
        );
        (runner, slept)
    }

    fn ms(values: &[u64]) -> Vec<Duration> {
        values.iter().map(|&v| Duration::from_millis(v)).collect()
    }

    #[test]
    fn a_lock_failure_is_retried_with_growing_pauses_until_it_succeeds() {
        let fake = Arc::new(
            FakeRunner::new()
                .expect(ARGV, fail(INDEX_LOCK))
                .expect(ARGV, fail(INDEX_LOCK))
                .expect(ARGV, ok(b"done")),
        );
        let (runner, slept) = retrying(Arc::clone(&fake));

        let output = runner.run(&GitCommand::new("add").arg("-A")).unwrap();

        assert_eq!(output, ok(b"done"));
        fake.assert_done();
        assert_eq!(fake.envs().len(), 3);
        assert_eq!(*slept.lock().unwrap(), ms(&[50, 100]));
    }

    #[test]
    fn any_other_failure_is_returned_at_once() {
        let fake = Arc::new(FakeRunner::new().expect(ARGV, fail("fatal: not a git repository")));
        let (runner, slept) = retrying(Arc::clone(&fake));

        let output = runner.run(&GitCommand::new("add").arg("-A")).unwrap();

        assert_eq!(output, fail("fatal: not a git repository"));
        assert_eq!(fake.envs().len(), 1);
        assert!(slept.lock().unwrap().is_empty());
    }

    #[test]
    fn a_lock_that_never_clears_returns_the_last_failure_after_four_pauses() {
        let mut fake = FakeRunner::new();
        for i in 0..5 {
            fake = fake.expect(ARGV, fail(&format!("{INDEX_LOCK} {i}")));
        }
        let fake = Arc::new(fake);
        let (runner, slept) = retrying(Arc::clone(&fake));

        let output = runner.run(&GitCommand::new("add").arg("-A")).unwrap();

        assert_eq!(output, fail(&format!("{INDEX_LOCK} 4")));
        fake.assert_done();
        assert_eq!(*slept.lock().unwrap(), ms(&[50, 100, 200, 400]));
    }

    #[test]
    fn a_ref_lock_failure_is_retried_too() {
        let fake = Arc::new(
            FakeRunner::new()
                .expect(
                    ARGV,
                    fail("error: cannot lock ref 'refs/heads/main': is at 1 but expected 2"),
                )
                .expect(ARGV, ok(b"")),
        );
        let (runner, slept) = retrying(Arc::clone(&fake));

        assert!(
            runner
                .run(&GitCommand::new("add").arg("-A"))
                .unwrap()
                .success
        );
        fake.assert_done();
        assert_eq!(*slept.lock().unwrap(), ms(&[50]));
    }

    #[test]
    fn spawn_and_interrupt_go_straight_through() {
        let fake = Arc::new(FakeRunner::new().expect_spawn(&["diff"], FakeChild::lines(0)));
        let (runner, slept) = retrying(Arc::clone(&fake));

        runner.spawn(&GitCommand::new("diff")).unwrap();
        runner.interrupt();

        fake.assert_done();
        assert_eq!(fake.interrupts(), 1);
        assert!(slept.lock().unwrap().is_empty());
    }
}
