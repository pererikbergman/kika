//! A scripted [`GitRunner`] for tests: each call must match the next expected
//! argv, and gets the canned output back.

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use super::runner::{AttachedExit, ChildHandle, GitChild, GitCommand, GitOutput, GitRunner};

/// Looks at an attached run's command while it "runs".
type AttachedProbe = Box<dyn Fn(&GitCommand) + Send + Sync>;

/// In an `expect_attached` argv, matches any one argument, such as a temp
/// file's name.
pub const ANY_ARG: &str = "<any>";

/// The whole env of a read: the `Standard` profile's vars, spelled out so
/// a test pins them rather than repeating the profile table.
pub const STANDARD_ENV: [(&str, &str); 3] = [
    ("LC_ALL", "C"),
    ("GIT_OPTIONAL_LOCKS", "0"),
    ("GIT_LITERAL_PATHSPECS", "1"),
];

/// `pairs` as `GitCommand::process_env` and `FakeRunner::process_envs`
/// list them, to compare a command's whole env.
pub fn env_list(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    pairs.iter().map(|(k, v)| (k.into(), v.into())).collect()
}

#[derive(Default)]
pub struct FakeRunner {
    expected: Mutex<VecDeque<(Vec<String>, GitOutput)>>,
    spawns: Mutex<VecDeque<(Vec<String>, FakeChild)>>,
    attached: Mutex<VecDeque<(Vec<String>, bool)>>,
    on_attached: Option<AttachedProbe>,
    envs: Mutex<Vec<Vec<(OsString, OsString)>>>,
    process_envs: Mutex<Vec<Vec<(OsString, OsString)>>>,
    inputs: Mutex<Vec<Option<Vec<u8>>>>,
    interrupts: AtomicUsize,
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

    /// Expect an attached run with `argv`, where [`ANY_ARG`] matches any
    /// one argument; it exits with `success`. Matched in order, separately
    /// from runs and spawns. Any attached run not expected panics.
    pub fn expect_attached(self, argv: &[&str], success: bool) -> Self {
        let argv = argv.iter().map(|s| s.to_string()).collect();
        self.attached.lock().unwrap().push_back((argv, success));
        self
    }

    /// Call `probe` with each attached run's command while it runs, to see
    /// files that only exist until it returns.
    pub fn on_attached(mut self, probe: impl Fn(&GitCommand) + Send + Sync + 'static) -> Self {
        self.on_attached = Some(Box::new(probe));
        self
    }

    /// Environment of every call so far, in order.
    pub fn envs(&self) -> Vec<Vec<(OsString, OsString)>> {
        self.envs.lock().unwrap().clone()
    }

    /// The env every call so far would give git, profile vars included, in
    /// order.
    pub fn process_envs(&self) -> Vec<Vec<(OsString, OsString)>> {
        self.process_envs.lock().unwrap().clone()
    }

    /// Stdin of every call so far, in order; `None` where it was closed.
    pub fn inputs(&self) -> Vec<Option<Vec<u8>>> {
        self.inputs.lock().unwrap().clone()
    }

    /// How many times `interrupt` was called.
    pub fn interrupts(&self) -> usize {
        self.interrupts.load(Ordering::SeqCst)
    }

    /// Record what every call, run or spawn, was given besides its argv.
    /// A command that named no env profile is a bug in its builder.
    fn record(&self, cmd: &GitCommand) {
        assert!(
            cmd.env_profile().is_some(),
            "git command with no env profile: {:?}",
            argv_of(cmd)
        );
        self.envs.lock().unwrap().push(cmd.envs().to_vec());
        self.process_envs.lock().unwrap().push(cmd.process_env());
        self.inputs
            .lock()
            .unwrap()
            .push(cmd.input().map(<[u8]>::to_vec));
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
        let attached = self.attached.lock().unwrap();
        assert!(
            attached.is_empty(),
            "expected attached runs never made: {attached:?}"
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
        self.record(cmd);
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
        self.record(cmd);
        Ok(output)
    }

    fn run_attached(&self, cmd: &GitCommand) -> io::Result<AttachedExit> {
        let actual = argv_of(cmd);
        let (expected, success) = self
            .attached
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("unexpected attached git run: {actual:?}"));
        let matches = actual.len() == expected.len()
            && actual
                .iter()
                .zip(&expected)
                .all(|(a, e)| e == ANY_ARG || a == e);
        assert!(
            matches,
            "attached git argv mismatch: {actual:?} vs {expected:?}"
        );
        self.record(cmd);
        if let Some(probe) = &self.on_attached {
            probe(cmd);
        }
        Ok(AttachedExit {
            success,
            status: if success { "status 0" } else { "status 1" }.into(),
        })
    }

    fn interrupt(&self) {
        self.interrupts.fetch_add(1, Ordering::SeqCst);
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

/// Run git in `dir` for test setup, failing loudly, with the host's global
/// and system config shut out. Arguments are `OsStr`s so tests can pass
/// names that are not UTF-8.
pub fn git<A: AsRef<OsStr>>(dir: &std::path::Path, args: &[A]) -> Vec<u8> {
    let out = setup_git(dir, args).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// Like [`git`], for a step expected to fail, such as a merge that stops
/// on a conflict. Returns whether git succeeded.
pub fn try_git<A: AsRef<OsStr>>(dir: &std::path::Path, args: &[A]) -> bool {
    setup_git(dir, args).output().unwrap().status.success()
}

/// The value of `key` in `dir`'s config, or `None` when it is unset.
pub fn config(dir: &std::path::Path, key: &str) -> Option<String> {
    let mut cmd = std::process::Command::new("git");
    let out = cmd
        .current_dir(dir)
        .envs(crate::runner::isolation_env())
        .args(["config", "--get", key])
        .output()
        .unwrap();
    out.status
        .success()
        .then(|| String::from_utf8(out.stdout).unwrap().trim().to_string())
}

fn setup_git<A: AsRef<OsStr>>(dir: &std::path::Path, args: &[A]) -> std::process::Command {
    let mut command = std::process::Command::new("git");
    command
        .current_dir(dir)
        .envs(super::runner::isolation_env())
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .stdin(std::process::Stdio::null());
    command
}

/// A file name that is not UTF-8, and a different file whose name is what
/// a lossy decode of it would show.
#[cfg(unix)]
pub const RAW_NAME: &[u8] = b"x\xff";
#[cfg(unix)]
pub const LOOK_ALIKE: &str = "x\u{FFFD}";

/// A repo with both names committed. [`RAW_NAME`] exists only in the index,
/// since filesystems such as APFS refuse names that are not UTF-8, so it
/// shows as deleted in the worktree (` D`). [`LOOK_ALIKE`] holds
/// `look-alike old` and is clean.
#[cfg(unix)]
pub fn look_alike_repo() -> TempDir {
    use std::os::unix::ffi::OsStrExt;

    let tmp = TempDir::new("look-alike");
    let dir = tmp.path();
    git(dir, &["init", "-q"]);
    std::fs::write(dir.join(LOOK_ALIKE), "look-alike old\n").unwrap();
    std::fs::write(dir.join("blob"), "raw old\n").unwrap();
    let oid = git(dir, &["hash-object", "-w", "blob"]);
    std::fs::remove_file(dir.join("blob")).unwrap();
    let mut cacheinfo = b"100644,".to_vec();
    cacheinfo.extend(oid.trim_ascii());
    cacheinfo.push(b',');
    cacheinfo.extend(RAW_NAME);
    git(
        dir,
        &[
            OsStr::new("update-index"),
            OsStr::new("--add"),
            OsStr::new("--cacheinfo"),
            OsStr::from_bytes(&cacheinfo),
        ],
    );
    git(dir, &["add", "--", LOOK_ALIKE]);
    git(dir, &["commit", "-q", "-m", "init"]);
    tmp
}

/// The status entry for the file whose raw name is `raw`, if listed.
#[cfg(unix)]
pub fn entry_named(runner: &dyn GitRunner, raw: &[u8]) -> Option<crate::FileEntry> {
    use std::os::unix::ffi::OsStrExt;

    crate::status::read_status(runner)
        .unwrap()
        .files
        .into_iter()
        .find(|e| e.path.as_os_str().as_bytes() == raw)
}
