//! A scripted [`GitRunner`] for tests: each call must match the next expected
//! argv, and gets the canned output back.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use super::runner::{ChildHandle, GitChild, GitCommand, GitOutput, GitRunner};

#[derive(Default)]
pub struct FakeRunner {
    expected: Mutex<VecDeque<(Vec<String>, GitOutput)>>,
    spawns: Mutex<VecDeque<(Vec<String>, FakeChild)>>,
    envs: Mutex<Vec<Vec<(OsString, OsString)>>>,
}

impl FakeRunner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Expect a call with exactly `argv`; answer it with `output`.
    pub fn expect(self, argv: &[&str], output: GitOutput) -> Self {
        let argv = argv.iter().map(|s| s.to_string()).collect();
        self.expected.lock().unwrap().push_back((argv, output));
        self
    }

    /// Expect a spawn with exactly `argv`; hand out `child`. Spawns are
    /// matched in order, separately from `expect`ed runs.
    pub fn expect_spawn(self, argv: &[&str], child: FakeChild) -> Self {
        let argv = argv.iter().map(|s| s.to_string()).collect();
        self.spawns.lock().unwrap().push_back((argv, child));
        self
    }

    /// Environment of every call so far, in order.
    pub fn envs(&self) -> Vec<Vec<(OsString, OsString)>> {
        self.envs.lock().unwrap().clone()
    }

    pub fn assert_done(&self) {
        let left = self.expected.lock().unwrap();
        assert!(left.is_empty(), "expected git calls never made: {left:?}");
        let spawns = self.spawns.lock().unwrap();
        assert!(
            spawns.is_empty(),
            "{} expected spawns never made",
            spawns.len()
        );
    }
}

fn argv_of(cmd: &GitCommand) -> Vec<String> {
    cmd.args()
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

impl GitRunner for FakeRunner {
    fn spawn(&self, cmd: &GitCommand) -> io::Result<GitChild> {
        let actual = argv_of(cmd);
        let (expected, child) = self
            .spawns
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("unexpected git spawn: {actual:?}"));
        assert_eq!(actual, expected, "git spawn argv mismatch");
        self.envs.lock().unwrap().push(cmd.envs().to_vec());
        child.probe.spawned.store(true, Ordering::SeqCst);
        Ok(GitChild {
            stdout: Box::new(FakeStdout {
                source: child.source,
                next: 0,
                probe: Arc::clone(&child.probe),
            }),
            handle: Arc::new(FakeHandle {
                exit: child.exit,
                probe: child.probe,
            }),
        })
    }

    fn run(&self, cmd: &GitCommand) -> io::Result<GitOutput> {
        let actual = argv_of(cmd);
        let (expected, output) = self
            .expected
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("unexpected git call: {actual:?}"));
        assert_eq!(actual, expected, "git argv mismatch");
        self.envs.lock().unwrap().push(cmd.envs().to_vec());
        Ok(output)
    }
}

/// A successful run that printed `stdout`.
pub fn ok(stdout: &[u8]) -> GitOutput {
    GitOutput {
        success: true,
        stdout: stdout.to_vec(),
        stderr: Vec::new(),
    }
}

/// A failed run that printed `stderr`.
pub fn fail(stderr: &str) -> GitOutput {
    GitOutput {
        success: false,
        stdout: Vec::new(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

/// A scripted spawned process. Clones share the probes, so a test keeps one
/// copy to watch what the code under test did with the other.
#[derive(Clone)]
pub struct FakeChild {
    source: Source,
    exit: GitOutput,
    probe: Arc<Probe>,
}

#[derive(Clone, Copy)]
enum Source {
    Lines(usize),
    Hanging,
}

#[derive(Default)]
struct Probe {
    spawned: AtomicBool,
    killed: Mutex<bool>,
    wake: Condvar,
    reads: AtomicUsize,
}

impl FakeChild {
    /// Stdout of `n` lines `+line 0` .. `+line n-1`, then a successful exit.
    pub fn lines(n: usize) -> Self {
        Self::new(Source::Lines(n))
    }

    /// Stdout that blocks until the child is killed, like a slow `git`.
    pub fn hanging() -> Self {
        Self::new(Source::Hanging)
    }

    fn new(source: Source) -> Self {
        Self {
            source,
            exit: ok(b""),
            probe: Arc::default(),
        }
    }

    /// Exit with this `success` and `stderr` once stdout is drained.
    pub fn exits(mut self, output: GitOutput) -> Self {
        self.exit = output;
        self
    }

    pub fn was_spawned(&self) -> bool {
        self.probe.spawned.load(Ordering::SeqCst)
    }

    pub fn was_killed(&self) -> bool {
        *self.probe.killed.lock().unwrap()
    }

    /// Lines handed to the reader so far.
    pub fn reads(&self) -> usize {
        self.probe.reads.load(Ordering::SeqCst)
    }
}

/// Hands out one line per `read`, so `reads` counts exactly what was consumed.
/// After a kill it reads as EOF, like a closed pipe.
struct FakeStdout {
    source: Source,
    next: usize,
    probe: Arc<Probe>,
}

impl Read for FakeStdout {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut killed = self.probe.killed.lock().unwrap();
        match self.source {
            Source::Hanging => {
                while !*killed {
                    killed = self.probe.wake.wait(killed).unwrap();
                }
                Ok(0)
            }
            Source::Lines(n) => {
                if *killed || self.next >= n {
                    return Ok(0);
                }
                let line = format!("+line {}\n", self.next);
                assert!(buf.len() >= line.len(), "read buffer too small");
                buf[..line.len()].copy_from_slice(line.as_bytes());
                self.next += 1;
                self.probe.reads.fetch_add(1, Ordering::SeqCst);
                Ok(line.len())
            }
        }
    }
}

struct FakeHandle {
    exit: GitOutput,
    probe: Arc<Probe>,
}

impl ChildHandle for FakeHandle {
    fn kill(&self) {
        *self.probe.killed.lock().unwrap() = true;
        self.probe.wake.notify_all();
    }

    fn finish(&self) -> io::Result<GitOutput> {
        Ok(GitOutput {
            stdout: Vec::new(),
            ..self.exit.clone()
        })
    }
}

/// A fresh directory under the temp dir, created exclusively so a
/// pre-existing path (or symlink) is never reused, and removed on drop even
/// when the test fails.
pub struct TempDir(std::path::PathBuf);

impl TempDir {
    pub fn new(label: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "kika-{label}-{}-{nanos}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir(&path).expect("a new, unique temp dir");
        Self(path)
    }

    pub fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Run git in `dir` for test setup, failing loudly.
pub fn git(dir: &std::path::Path, args: &[&str]) -> Vec<u8> {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}
