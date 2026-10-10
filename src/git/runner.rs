use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStderr, Command, Stdio};
use std::sync::{Arc, Mutex};

/// A `git` invocation as argv plus environment, and optionally bytes for its
/// stdin. Kept as separate arguments so paths and user input can never be
/// interpreted by a shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCommand {
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
    stdin: Option<Vec<u8>>,
}

impl GitCommand {
    pub fn new(subcommand: impl Into<OsString>) -> Self {
        Self {
            args: vec![subcommand.into()],
            env: Vec::new(),
            stdin: None,
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

    pub fn args(&self) -> &[OsString] {
        &self.args
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

/// Runs Git commands. Synchronous and object-safe so it can be shared as
/// `Arc<dyn GitRunner>` and called from tokio's blocking pool.
pub trait GitRunner: Send + Sync {
    fn run(&self, cmd: &GitCommand) -> io::Result<GitOutput>;

    /// Start a command whose stdout is read as it arrives. The pipe and the
    /// handle are separate so one thread can kill while another is blocked reading.
    fn spawn(&self, cmd: &GitCommand) -> io::Result<GitChild>;
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

/// The real runner: spawns the `git` binary in a working directory.
pub struct CliRunner {
    workdir: PathBuf,
}

impl CliRunner {
    pub fn new(workdir: impl Into<PathBuf>) -> Self {
        Self {
            workdir: workdir.into(),
        }
    }

    fn command(&self, cmd: &GitCommand) -> Command {
        let mut command = Command::new("git");
        command
            .current_dir(&self.workdir)
            .args(cmd.args())
            .envs(cmd.envs().iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null());
        command
    }
}

impl GitRunner for CliRunner {
    fn run(&self, cmd: &GitCommand) -> io::Result<GitOutput> {
        let output = match cmd.input() {
            None => self.command(cmd).output()?,
            Some(input) => {
                let mut child = self
                    .command(cmd)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()?;
                let mut pipe = child.stdin.take().expect("stdin is piped");
                // Write from another thread while this one drains stdout and
                // stderr, so git filling an output pipe cannot deadlock us.
                // Dropping the pipe afterwards sends EOF. A write error means
                // git exited early; its exit status tells the story.
                std::thread::scope(|scope| {
                    scope.spawn(move || {
                        let _ = pipe.write_all(input);
                    });
                    child.wait_with_output()
                })?
            }
        };
        Ok(GitOutput {
            success: output.status.success(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
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

/// A poisoned lock only means another thread panicked; the child is still valid.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
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
