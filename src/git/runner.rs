use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStderr, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

/// A `git` invocation as argv plus environment, and optionally bytes for its
/// stdin. Kept as separate arguments so paths and user input can never be
/// interpreted by a shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCommand {
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
    stdin: Option<Vec<u8>>,
    // Only unix can take the terminal away.
    #[cfg_attr(not(unix), allow(dead_code))]
    no_terminal: bool,
}

impl GitCommand {
    pub fn new(subcommand: impl Into<OsString>) -> Self {
        Self {
            args: vec![subcommand.into()],
            env: Vec::new(),
            stdin: None,
            no_terminal: false,
        }
    }

    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Feed `bytes` to the process's stdin, which is otherwise closed.
    pub fn stdin(mut self, bytes: Vec<u8>) -> Self {
        self.stdin = Some(bytes);
        self
    }

    /// Run with no controlling terminal (on unix, in a session of its
    /// own), so nothing it starts can read the screen Kika owns, even a
    /// program that opens `/dev/tty` itself. Only for `run`.
    pub fn without_terminal(mut self) -> Self {
        self.no_terminal = true;
        self
    }

    #[cfg_attr(not(unix), allow(dead_code))]
    pub fn is_without_terminal(&self) -> bool {
        self.no_terminal
    }

    pub fn args(&self) -> &[OsString] {
        &self.args
    }

    /// The git subcommand, past any leading `-c key=value` pairs, for
    /// messages such as "git rebase failed".
    pub fn subcommand(&self) -> &OsStr {
        let mut args = self.args.as_slice();
        while let [first, _, rest @ ..] = args
            && first == "-c"
        {
            args = rest;
        }
        // Only `-c` pairs: name the first argument. Never index, so an
        // empty argv cannot panic the terminal hand-off.
        args.first()
            .or(self.args.first())
            .map_or(OsStr::new(""), OsString::as_os_str)
    }

    pub fn envs(&self) -> &[(OsString, OsString)] {
        &self.env
    }

    /// The bytes for stdin, if any; runners close stdin otherwise.
    pub fn input(&self) -> Option<&[u8]> {
        self.stdin.as_deref()
    }
}

/// What a finished `git` process left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOutput {
    pub success: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// How a command run on the real terminal ended. Its output went to the
/// terminal, so only the exit is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachedExit {
    pub success: bool,
    /// The exit for a message, such as `status 1` or `signal 2`.
    pub status: String,
}

/// Runs Git commands. Synchronous and object-safe so it can be shared as
/// `Arc<dyn GitRunner>` and called from tokio's blocking pool.
pub trait GitRunner: Send + Sync {
    fn run(&self, cmd: &GitCommand) -> io::Result<GitOutput>;

    /// Stop every `run` still in flight, so quitting never waits on git.
    /// Each stopped run returns as failed.
    fn interrupt(&self) {}

    /// Start a command whose stdout is read as it arrives. The pipe and the
    /// handle are separate so one thread can kill while another is blocked reading.
    fn spawn(&self, cmd: &GitCommand) -> io::Result<GitChild>;

    /// Run a command on the inherited terminal, so it can ask the user
    /// things, and wait for it. Only for hand-offs, while Kika has given
    /// the terminal up. With `stdin` bytes on `cmd`, git reads those
    /// instead of the terminal, which it still writes to; a signing
    /// prompt finds the terminal through `GPG_TTY`.
    fn run_attached(&self, _cmd: &GitCommand) -> io::Result<AttachedExit> {
        Err(io::ErrorKind::Unsupported.into())
    }

    /// The directory commands run in. Empty for runners with no directory.
    fn workdir(&self) -> PathBuf {
        PathBuf::new()
    }

    /// Run later commands in `dir` instead, such as a submodule's top
    /// level. Commands already started keep theirs.
    fn set_workdir(&self, _dir: PathBuf) {}
}

/// A running `git` process: its stdout pipe, plus a handle to kill or reap it.
pub struct GitChild {
    pub stdout: Box<dyn Read + Send>,
    pub handle: Arc<dyn ChildHandle>,
}

/// Controls a spawned process from any thread.
pub trait ChildHandle: Send + Sync {
    /// Stop the process. Non-blocking and idempotent; killing closes its
    /// stdout, so a blocked reader wakes up with EOF.
    fn kill(&self);
    /// Wait for the exit once stdout is drained. Returns `success` and
    /// `stderr`; `stdout` is always empty because the caller read it.
    fn finish(&self) -> io::Result<GitOutput>;
}

/// The env vars that start Kika in a hidden mode instead of the UI. Never
/// inherited by a git child: the one command that needs one sets it.
const HIDDEN_MODE_ENV: [&str; 2] = [
    crate::git::askpass::SOCKET_ENV,
    crate::sequence_editor::PLAN_ENV,
];

/// The real runner: spawns the `git` binary in a working directory.
pub struct CliRunner {
    /// Behind a lock so entering a submodule can move every later command.
    workdir: RwLock<PathBuf>,
    /// Added to every command; lets tests shut out the host's git config.
    env: Vec<(OsString, OsString)>,
    /// Removed from every command's environment, for the same reason.
    env_remove: Vec<OsString>,
    /// Processes started by `run` and not yet reaped, for `interrupt`.
    running: Mutex<Vec<(u64, Arc<Mutex<Child>>)>>,
    next_id: AtomicU64,
}

impl CliRunner {
    pub fn new(workdir: impl Into<PathBuf>) -> Self {
        Self {
            workdir: RwLock::new(workdir.into()),
            env: Vec::new(),
            env_remove: Vec::new(),
            running: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(0),
        }
    }

    /// A runner that ignores the global and system git config and the
    /// host's editor settings, so tests behave the same on every machine.
    #[cfg(test)]
    pub fn isolated(workdir: impl Into<PathBuf>) -> Self {
        Self {
            env: isolation_env(),
            env_remove: ["GIT_EDITOR", "VISUAL", "EDITOR"]
                .map(OsString::from)
                .into(),
            ..Self::new(workdir)
        }
    }

    /// Add `key=value` to every command, for test setups that need more
    /// than the isolated config.
    #[cfg(test)]
    pub fn with_env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    fn command(&self, cmd: &GitCommand) -> Command {
        let mut command = Command::new("git");
        // Each hidden mode's key reaches only the command that sets it.
        for key in HIDDEN_MODE_ENV
            .iter()
            .map(OsStr::new)
            .chain(self.env_remove.iter().map(|k| k.as_os_str()))
        {
            command.env_remove(key);
        }
        command
            .current_dir(&*self.workdir.read().unwrap_or_else(|e| e.into_inner()))
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .args(cmd.args())
            .envs(cmd.envs().iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null());
        command
    }

    /// Wait for `child`, feeding it `input` and draining both outputs on
    /// separate threads so no full pipe can deadlock. The child is locked
    /// only to poll its exit, never while blocked, so `interrupt` can get in.
    fn collect(child: &Mutex<Child>, input: Option<&[u8]>) -> io::Result<GitOutput> {
        let (stdin, stdout, stderr) = {
            let mut child = lock(child);
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        };
        let (stdout, stderr) = std::thread::scope(|scope| {
            if let (Some(mut pipe), Some(input)) = (stdin, input) {
                // Dropping the pipe afterwards sends EOF. A write error means
                // git exited early; its exit status tells the story.
                scope.spawn(move || {
                    let _ = pipe.write_all(input);
                });
            }
            let stderr = scope.spawn(|| read_all(stderr));
            let stdout = read_all(stdout);
            let stderr = stderr
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
            (stdout, stderr)
        });
        let (stdout, stderr) = (stdout?, stderr?);
        let status = loop {
            if let Some(status) = lock(child).try_wait()? {
                break status;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        Ok(GitOutput {
            success: status.success(),
            stdout,
            stderr,
        })
    }
}

/// Everything left in `pipe`, or nothing if there is no pipe.
fn read_all(pipe: Option<impl Read>) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    if let Some(mut pipe) = pipe {
        pipe.read_to_end(&mut bytes)?;
    }
    Ok(bytes)
}

/// Env that makes git read only the repository's own config.
#[cfg(test)]
pub fn isolation_env() -> Vec<(OsString, OsString)> {
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    vec![
        ("GIT_CONFIG_GLOBAL".into(), null.into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
    ]
}

/// Whether `pid` may be signalled as a group: a real process (not 0, init or
/// "every process") that leads its own group, and that group is not ours.
#[cfg(unix)]
fn own_child_group(pid: libc::pid_t, its_pgid: libc::pid_t, our_pgid: libc::pid_t) -> bool {
    pid > 1 && its_pgid == pid && pid != our_pgid
}

/// Ask `child` and everything it started to stop. On unix git leads its own
/// process group, so hooks it runs are stopped too; SIGTERM lets git remove
/// its lock files on the way out. The group is also continued: a background
/// read of `/dev/tty` stops it with SIGTTIN, and a stopped process only acts
/// on SIGTERM once it runs again. Off unix only `child` itself is killed.
///
/// The caller must hold `child` unreaped, so its pid, which is also its
/// group id, cannot have been reused by another process.
fn terminate(child: &mut Child) {
    // A direct signal, not a `kill` program, so nothing is looked up on `PATH`.
    #[cfg(unix)]
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: `getpgid` and `getpgrp` only read process group ids.
        let (its_pgid, our_pgid) = unsafe { (libc::getpgid(pid), libc::getpgrp()) };
        if own_child_group(pid, its_pgid, our_pgid) {
            // SAFETY: `killpg` only sends a signal, to the group the guard
            // above checked is led by our own unreaped child and is not ours.
            if unsafe { libc::killpg(pid, libc::SIGTERM) } == 0 {
                // SAFETY: as above. ESRCH (group already gone) is ignored.
                let _ = unsafe { libc::killpg(pid, libc::SIGCONT) };
                return;
            }
        }
    }
    let _ = child.kill();
}

impl GitRunner for CliRunner {
    /// The repository's top level, or the submodule's once one is entered.
    fn workdir(&self) -> PathBuf {
        self.workdir
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn set_workdir(&self, dir: PathBuf) {
        *self.workdir.write().unwrap_or_else(|e| e.into_inner()) = dir;
    }

    fn run(&self, cmd: &GitCommand) -> io::Result<GitOutput> {
        let mut command = self.command(cmd);
        if cmd.input().is_some() {
            command.stdin(Stdio::piped());
        }
        // Its own process group, so `interrupt` reaches hooks git starts.
        // A session of its own is a group of its own too.
        #[cfg(unix)]
        if cmd.is_without_terminal() {
            detach_from_terminal(&mut command);
        } else {
            std::os::unix::process::CommandExt::process_group(&mut command, 0);
        }
        let child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let child = Arc::new(Mutex::new(child));
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        lock(&self.running).push((id, Arc::clone(&child)));
        let output = Self::collect(&child, cmd.input());
        lock(&self.running).retain(|(other, _)| *other != id);
        if output.is_err() {
            let mut child = lock(&child);
            let _ = child.kill();
            let _ = child.wait();
        }
        output
    }

    /// Not in `running` and not in a group of its own: Ctrl+C on the
    /// terminal reaches git and its hooks directly, while the loop keeps
    /// Kika alive through it.
    fn run_attached(&self, cmd: &GitCommand) -> io::Result<AttachedExit> {
        let mut command = self.command(cmd);
        let stdin = match cmd.input() {
            Some(_) => Stdio::piped(),
            None => Stdio::inherit(),
        };
        let mut child = command
            .stdin(stdin)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?;
        // Dropped once written, so git sees the end of it; git is waited
        // for even when the write fails.
        let written = match (child.stdin.take(), cmd.input()) {
            (Some(mut pipe), Some(input)) => pipe.write_all(input),
            _ => Ok(()),
        };
        let status = child.wait()?;
        written?;
        Ok(AttachedExit {
            success: status.success(),
            status: describe_exit(status),
        })
    }

    fn interrupt(&self) {
        for (_, child) in lock(&self.running).iter() {
            let mut child = lock(child);
            // Only a child not yet reaped still owns its pid and group.
            if matches!(child.try_wait(), Ok(None)) {
                terminate(&mut child);
            }
        }
    }

    fn spawn(&self, cmd: &GitCommand) -> io::Result<GitChild> {
        let mut child = self
            .command(cmd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take();
        Ok(GitChild {
            stdout: Box::new(stdout),
            handle: Arc::new(CliChild {
                child: Mutex::new(child),
                stderr: Mutex::new(stderr),
            }),
        })
    }
}

/// The real [`ChildHandle`]. The reader owns only the stdout pipe, never this
/// mutex, so `kill` can always get in while a read is blocked.
struct CliChild {
    child: Mutex<Child>,
    stderr: Mutex<Option<ChildStderr>>,
}

impl ChildHandle for CliChild {
    fn kill(&self) {
        // Fails only if the process already exited, which is what we wanted.
        let _ = lock(&self.child).kill();
    }

    fn finish(&self) -> io::Result<GitOutput> {
        let mut stderr = Vec::new();
        if let Some(mut pipe) = lock(&self.stderr).take() {
            pipe.read_to_end(&mut stderr)?;
        }
        let status = lock(&self.child).wait()?;
        Ok(GitOutput {
            success: status.success(),
            stdout: Vec::new(),
            stderr,
        })
    }
}

impl Drop for CliChild {
    /// Reap the process whatever happened to the reader, so no zombies remain.
    fn drop(&mut self) {
        let child = self.child.get_mut().unwrap_or_else(|e| e.into_inner());
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Start `command` in a new session, which has no controlling terminal.
#[cfg(unix)]
fn detach_from_terminal(command: &mut Command) {
    // SAFETY: `setsid` is async-signal-safe, so it may run between fork
    // and exec, and the closure touches nothing else.
    unsafe {
        std::os::unix::process::CommandExt::pre_exec(command, || {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// `status 1`, or `signal 2` for a process that was stopped.
fn describe_exit(status: std::process::ExitStatus) -> String {
    if let Some(code) = status.code() {
        return format!("status {code}");
    }
    #[cfg(unix)]
    if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(&status) {
        return format!("signal {signal}");
    }
    status.to_string()
}

/// A poisoned lock only means another thread panicked; the child is still valid.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_subcommand_skips_config_pairs_and_never_panics_without_one() {
        let rebase = GitCommand::new("-c")
            .arg("core.editor=true")
            .arg("rebase")
            .arg("--continue");
        assert_eq!(rebase.subcommand(), "rebase");
        assert_eq!(GitCommand::new("status").subcommand(), "status");

        let only_config = GitCommand::new("-c").arg("key=value");
        assert_eq!(only_config.subcommand(), "-c");
        let empty = GitCommand {
            args: Vec::new(),
            ..GitCommand::new("unused")
        };
        assert_eq!(empty.subcommand(), "");
    }

    /// What `command` does with `key`: `None` if it leaves the parent's
    /// value, `Some(None)` if it removes it, `Some(Some(v))` if it sets it.
    fn env_of(command: &Command, key: &str) -> Option<Option<OsString>> {
        command
            .get_envs()
            .filter(|(k, _)| *k == OsStr::new(key))
            .last()
            .map(|(_, v)| v.map(OsStr::to_os_string))
    }

    #[test]
    fn a_hidden_mode_s_key_reaches_only_the_command_that_sets_it() {
        use crate::git::askpass::SOCKET_ENV;
        use crate::sequence_editor::PLAN_ENV;

        let runner = CliRunner::new("/r");
        let plain = runner.command(&GitCommand::new("status"));
        let rebase = runner.command(&GitCommand::new("rebase").env(PLAN_ENV, "pick x\n"));
        let fetch = runner.command(&GitCommand::new("fetch").env(SOCKET_ENV, "/s"));
        for key in [PLAN_ENV, SOCKET_ENV] {
            assert_eq!(env_of(&plain, key), Some(None), "{key}");
        }
        assert_eq!(env_of(&rebase, PLAN_ENV), Some(Some("pick x\n".into())));
        assert_eq!(env_of(&rebase, SOCKET_ENV), Some(None));
        assert_eq!(env_of(&fetch, SOCKET_ENV), Some(Some("/s".into())));
        assert_eq!(env_of(&fetch, PLAN_ENV), Some(None));
    }

    /// `run` and `spawn` both build their process here, so every command
    /// kind runs in the runner's workdir with its argv untouched.
    #[test]
    fn every_command_kind_runs_in_the_workdir() {
        use crate::git::diff::{DiffRequest, DiffSide, diff_command, patch_diff_command};
        use crate::git::ops::{GitOp, op_command};
        use crate::git::path::RepoPath;
        use crate::git::repo_state::git_dir_command;
        use crate::git::status::status_command;
        use std::path::Path;

        let top = Path::new("/r/top");
        let runner = CliRunner::new(top);
        let request = |side| DiffRequest {
            path: RepoPath::from("a.txt"),
            orig_path: None,
            side,
            options: Default::default(),
            commit: None,
        };
        let tracked = request(DiffSide::Worktree);
        let staged = request(DiffSide::Index);
        let untracked = request(DiffSide::Untracked);
        let paths = vec![RepoPath::from("a.txt")];
        let ops = [
            GitOp::Stage(paths.clone()),
            GitOp::Unstage(paths),
            GitOp::StageAll,
            GitOp::UnstageAll,
            GitOp::ApplyCached {
                patch: b"patch".to_vec(),
                reverse: false,
            },
            GitOp::ApplyCached {
                patch: b"patch".to_vec(),
                reverse: true,
            },
            GitOp::Commit {
                message: Some("msg".into()),
                stage_all: false,
                amend: false,
                no_verify: false,
                fixup: None,
            },
            GitOp::ApplyEdited {
                patch: b"patch".to_vec(),
            },
            GitOp::SubmoduleInit(RepoPath::from("sub")),
            GitOp::SubmoduleUpdate(RepoPath::from("sub")),
        ];
        let commands = [
            status_command(),
            diff_command(&tracked),
            diff_command(&staged),
            diff_command(&untracked),
            patch_diff_command(&tracked),
            patch_diff_command(&staged),
            patch_diff_command(&untracked),
            git_dir_command(),
            crate::git::submodule::status_command(),
        ]
        .into_iter()
        .chain(ops.iter().map(op_command));

        for cmd in commands {
            let process = runner.command(&cmd);
            assert_eq!(process.get_current_dir(), Some(top), "{cmd:?}");
            let argv: Vec<_> = process.get_args().collect();
            assert_eq!(argv, cmd.args(), "{cmd:?}");
        }
    }

    #[test]
    fn an_isolated_runner_hides_the_hosts_editor_settings() {
        use std::ffi::OsStr;

        let tmp = crate::git::fake::TempDir::new("editor-env");
        crate::git::fake::git(tmp.path(), &["init", "-q"]);
        crate::git::fake::git(tmp.path(), &["config", "core.editor", "repo-editor"]);
        let runner = CliRunner::isolated(tmp.path());
        assert_eq!(runner.workdir(), tmp.path());
        let process = runner.command(&GitCommand::new("var").arg("GIT_EDITOR"));
        let envs: Vec<_> = process.get_envs().collect();
        for key in ["GIT_EDITOR", "VISUAL", "EDITOR"] {
            assert!(envs.contains(&(OsStr::new(key), None)), "{key}: {envs:?}");
        }
        let output = runner
            .run(&GitCommand::new("var").arg("GIT_EDITOR"))
            .unwrap();
        assert_eq!(output.stdout, b"repo-editor\n");
    }

    #[test]
    fn spawn_streams_stdout_and_finish_reports_the_exit() {
        let runner = CliRunner::new(std::env::temp_dir());
        let child = runner.spawn(&GitCommand::new("--version")).unwrap();
        let mut stdout = String::new();
        let mut pipe = child.stdout;
        pipe.read_to_string(&mut stdout).unwrap();
        assert!(stdout.starts_with("git version"), "{stdout}");

        let output = child.handle.finish().unwrap();
        assert!(output.success);
        assert!(output.stdout.is_empty());
        // Killing an exited child is harmless, and so is doing it twice.
        child.handle.kill();
        child.handle.kill();
    }

    #[test]
    fn finish_carries_stderr_of_a_failed_command() {
        let runner = CliRunner::new(std::env::temp_dir());
        let child = runner
            .spawn(&GitCommand::new("no-such-subcommand"))
            .unwrap();
        let mut pipe = child.stdout;
        io::copy(&mut pipe, &mut io::sink()).unwrap();
        let output = child.handle.finish().unwrap();
        assert!(!output.success);
        assert!(!output.stderr.is_empty());
    }

    #[test]
    fn run_passes_stdin_bytes_to_the_process() {
        let runner = CliRunner::new(std::env::temp_dir());
        let cmd = GitCommand::new("hash-object")
            .arg("--stdin")
            .stdin(b"hello\n".to_vec());
        let output = runner.run(&cmd).unwrap();
        assert!(
            output.success,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"ce013625030ba8dba906f756967f9e9ca394464a\n");
    }

    #[test]
    fn stdin_larger_than_a_pipe_buffer_does_not_deadlock() {
        let runner = CliRunner::new(std::env::temp_dir());
        let big = vec![b'x'; 1 << 20];
        let cmd = GitCommand::new("hash-object").arg("--stdin").stdin(big);
        assert!(runner.run(&cmd).unwrap().success);
    }

    /// Regression: quitting while a hook waits used to leave git (and the
    /// hook) running, and the blocked read kept Kika from exiting.
    #[cfg(unix)]
    #[test]
    fn interrupt_stops_a_commit_whose_hook_hangs_and_git_cleans_its_lock() {
        use crate::git::fake::{TempDir, git};
        use std::os::unix::fs::PermissionsExt;
        use std::time::{Duration, Instant};

        let tmp = TempDir::new("interrupt");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        git(dir, &["config", "core.hooksPath", ".git/hooks"]);
        std::fs::create_dir_all(dir.join(".git/hooks")).unwrap();
        let hook = dir.join(".git/hooks/pre-commit");
        // `sleep` is a grandchild that holds git's output pipes open.
        std::fs::write(&hook, "#!/bin/sh\ntouch started\nsleep 30\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(dir.join("a"), "a\n").unwrap();
        git(dir, &["add", "a"]);

        let runner = Arc::new(CliRunner::new(dir));
        let cmd = GitCommand::new("-c")
            .arg("user.name=t")
            .arg("-c")
            .arg("user.email=t@t")
            .arg("commit")
            .arg("-F")
            .arg("-")
            .stdin(b"msg\n".to_vec());
        let worker = {
            let runner = Arc::clone(&runner);
            std::thread::spawn(move || runner.run(&cmd))
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !dir.join("started").exists() {
            assert!(Instant::now() < deadline, "the hook never started");
            std::thread::sleep(Duration::from_millis(10));
        }

        let start = Instant::now();
        runner.interrupt();
        let output = worker.join().unwrap().unwrap();

        assert!(!output.success);
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "{:?}",
            start.elapsed()
        );
        assert!(!dir.join(".git/index.lock").exists());
        // Nothing is left to interrupt, so a second call is harmless.
        runner.interrupt();
    }

    /// SIGKILLs what is left of a stopped group if the test fails before the
    /// interrupt ends it. Each pid is signalled only while it is still in
    /// git's group, and never if that group is ours, so a reused pid or an
    /// unrelated process is never touched.
    #[cfg(unix)]
    struct StoppedGroup {
        pids: Vec<String>,
        group: String,
    }

    #[cfg(unix)]
    impl Drop for StoppedGroup {
        fn drop(&mut self) {
            // SAFETY: `getpgrp` only reads this process's group id.
            let ours = unsafe { libc::getpgrp() }.to_string();
            if self.group.is_empty() || self.group == ours {
                return;
            }
            for pid in &self.pids {
                if pgid_of(pid) == self.group
                    && let Ok(pid) = pid.parse::<libc::pid_t>()
                    && pid > 1
                {
                    // SAFETY: only sends a signal, to a pid checked just above
                    // to still be in git's own group.
                    unsafe { libc::kill(pid, libc::SIGKILL) };
                }
            }
        }
    }

    /// Regression: a background read of `/dev/tty` stops git's whole group
    /// with SIGTTIN, and stopped members ignored the SIGTERM until Kika quit.
    #[cfg(unix)]
    #[test]
    fn interrupt_ends_a_commit_whose_group_is_stopped() {
        use crate::git::fake::{TempDir, git};
        use std::os::unix::fs::PermissionsExt;
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let tmp = TempDir::new("interrupt-stopped");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        git(dir, &["config", "core.hooksPath", ".git/hooks"]);
        std::fs::create_dir_all(dir.join(".git/hooks")).unwrap();
        let hook = dir.join(".git/hooks/pre-commit");
        // With a terminal, the read stops git's background group with
        // SIGTTIN; without one the hook stops the group itself. It refuses
        // to if it shares the test's group.
        std::fs::write(
            &hook,
            "#!/bin/sh\n\
             group=$(ps -o pgid= -p $$ | tr -d ' ')\n\
             [ -n \"$group\" ] && [ \"$group\" != \"$OUTER_GROUP\" ] || exit 1\n\
             echo \"$$ $PPID $group\" > started.tmp && mv started.tmp started\n\
             if (exec 3</dev/tty) 2>/dev/null; then read x </dev/tty; fi\n\
             kill -STOP 0\n\
             sleep 30\n",
        )
        .unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(dir.join("a"), "a\n").unwrap();
        git(dir, &["add", "a"]);

        let runner = Arc::new(CliRunner::new(dir));
        // SAFETY: `getpgrp` only reads this process's group id.
        let ours = unsafe { libc::getpgrp() };
        let cmd = GitCommand::new("-c")
            .arg("user.name=t")
            .arg("-c")
            .arg("user.email=t@t")
            .arg("commit")
            .arg("-F")
            .arg("-")
            .env("OUTER_GROUP", ours.to_string())
            .stdin(b"msg\n".to_vec());
        let (tx, rx) = mpsc::channel();
        {
            let runner = Arc::clone(&runner);
            std::thread::spawn(move || {
                let _ = tx.send(runner.run(&cmd));
            });
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while !dir.join("started").exists() {
            assert!(Instant::now() < deadline, "the hook never started");
            std::thread::sleep(Duration::from_millis(10));
        }
        let started = std::fs::read_to_string(dir.join("started")).unwrap();
        let mut ids: Vec<String> = started.split_whitespace().map(String::from).collect();
        let group = ids.pop().unwrap();
        // The hook, its git, and the group's leader.
        ids.push(group.clone());
        let stopped = StoppedGroup { pids: ids, group };
        loop {
            let stat = Command::new("ps")
                .args(["-o", "stat=", "-p", &stopped.pids[0]])
                .output()
                .unwrap();
            if String::from_utf8_lossy(&stat.stdout)
                .trim()
                .starts_with('T')
            {
                break;
            }
            assert!(Instant::now() < deadline, "the hook never stopped");
            std::thread::sleep(Duration::from_millis(10));
        }

        runner.interrupt();
        let output = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the stopped group kept the commit running")
            .unwrap();

        assert!(!output.success);
        assert!(!dir.join(".git/index.lock").exists());
    }

    #[cfg(unix)]
    #[test]
    fn only_a_child_leading_a_group_other_than_ours_is_signalled() {
        let ours = 500;
        assert!(own_child_group(4321, 4321, ours));
        assert!(!own_child_group(0, 0, ours), "0 means our own group");
        assert!(!own_child_group(1, 1, ours), "1 is init");
        assert!(!own_child_group(-1, -1, ours), "-1 means every process");
        assert!(!own_child_group(-4321, -4321, ours));
        assert!(!own_child_group(4321, 4000, ours), "not a group leader");
        assert!(!own_child_group(ours, ours, ours), "our own group");
    }

    /// The pgid of process `pid`, as `ps` prints it.
    #[cfg(unix)]
    fn pgid_of(pid: &str) -> String {
        let out = Command::new("ps")
            .args(["-o", "pgid=", "-p", pid])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    #[cfg(unix)]
    #[test]
    fn run_attached_runs_git_in_the_workdir_without_a_group_of_its_own() {
        use crate::git::fake::{TempDir, git};
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new("attached");
        let outside = tmp.path().join("outside");
        let dir = tmp.path().join("repo");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(dir.join(".git/hooks")).unwrap();
        git(&dir, &["init", "-q"]);
        git(&dir, &["config", "user.name", "t"]);
        git(&dir, &["config", "user.email", "t@t"]);
        let hook = dir.join(".git/hooks/pre-commit");
        std::fs::write(
            &hook,
            "#!/bin/sh\npwd > .git/cwd\nps -o pgid= -p $$ > .git/pgid\n",
        )
        .unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(dir.join("a"), "a\n").unwrap();
        git(&dir, &["add", "a"]);
        let runner = CliRunner::isolated(&outside);
        runner.set_workdir(dir.clone());

        let cmd = GitCommand::new("commit").arg("-q").arg("-m").arg("msg");
        let exit = runner.run_attached(&cmd).unwrap();

        assert_eq!(
            exit,
            AttachedExit {
                success: true,
                status: "status 0".into()
            }
        );
        let cwd = std::fs::read_to_string(dir.join(".git/cwd")).unwrap();
        assert_eq!(
            std::fs::canonicalize(cwd.trim_end()).unwrap(),
            std::fs::canonicalize(&dir).unwrap()
        );
        let hook_pgid = std::fs::read_to_string(dir.join(".git/pgid")).unwrap();
        assert_eq!(
            hook_pgid.trim(),
            pgid_of(&std::process::id().to_string()),
            "git started a new process group"
        );
        assert!(lock(&runner.running).is_empty());
    }

    /// A command run without the terminal cannot open `/dev/tty`, so an
    /// old ssh that ignores `SSH_ASKPASS_REQUIRE` cannot prompt on the
    /// screen and stop there. Only telling when the tests have a terminal.
    #[cfg(unix)]
    #[test]
    fn a_command_without_the_terminal_cannot_open_it_and_leads_its_group() {
        let runner = CliRunner::isolated(std::env::temp_dir());
        let probe = "!if (exec 3</dev/tty) 2>/dev/null; then echo tty; else echo none; fi; \
                     ps -o pgid= -p $PPID";
        let cmd = GitCommand::new("-c")
            .arg(format!("alias.probe={probe}"))
            .arg("probe")
            .without_terminal();

        let output = runner.run(&cmd).unwrap();

        assert!(
            output.success,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        let mut lines = stdout.lines();
        assert_eq!(lines.next(), Some("none"));
        // Git's group is not Kika's, so `interrupt` can stop it as a whole.
        let group = lines.next().unwrap().trim().to_string();
        assert_ne!(group, pgid_of(&std::process::id().to_string()));
    }

    #[test]
    fn run_attached_feeds_stdin_bytes_to_git() {
        use crate::git::fake::{TempDir, git};

        let tmp = TempDir::new("attached-stdin");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        let runner = CliRunner::isolated(dir);
        let cmd = GitCommand::new("commit")
            .arg("-q")
            .arg("--allow-empty")
            .arg("-F")
            .arg("-")
            .stdin(b"from stdin\n".to_vec());

        let exit = runner.run_attached(&cmd).unwrap();

        assert!(exit.success, "{exit:?}");
        assert_eq!(git(dir, &["log", "-1", "--format=%s"]), b"from stdin\n");
    }

    #[test]
    fn run_attached_reports_a_failed_exit() {
        // An unset key fails with status 1 and prints nothing.
        let runner = CliRunner::isolated(std::env::temp_dir());
        let exit = runner
            .run_attached(
                &GitCommand::new("config")
                    .arg("--get")
                    .arg("kika.no-such-key"),
            )
            .unwrap();
        assert!(!exit.success);
        assert_eq!(exit.status, "status 1");
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "kill stops git.exe but not the process it starts, which keeps writing; process-group kill is Unix-only"
    )]
    fn kill_unblocks_a_reader_of_a_large_diff() {
        let tmp = crate::git::fake::TempDir::new("kill");
        let dir = tmp.path();
        let big: String = (0..500_000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(dir.join("big.txt"), big).unwrap();
        let runner = CliRunner::new(dir);
        let cmd = GitCommand::new("diff")
            .arg("--no-index")
            .arg("--")
            .arg("/dev/null")
            .arg("big.txt");

        let child = runner.spawn(&cmd).unwrap();
        let mut reader = io::BufReader::new(child.stdout);
        let mut first = String::new();
        io::BufRead::read_line(&mut reader, &mut first).unwrap();
        assert!(first.starts_with("diff --git"), "{first}");

        // git is now blocked on a full pipe; killing it lets the reader finish.
        let handle = Arc::clone(&child.handle);
        std::thread::spawn(move || handle.kill()).join().unwrap();
        let mut rest = Vec::new();
        reader.read_to_end(&mut rest).unwrap();
        assert!(rest.len() < 1_000_000, "read {} bytes", rest.len());
        assert!(!child.handle.finish().unwrap().success);
    }
}
