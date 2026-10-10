use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A `git` invocation as argv plus environment. Kept as separate arguments so
/// paths and user input can never be interpreted by a shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCommand {
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
}

impl GitCommand {
    pub fn new(subcommand: impl Into<OsString>) -> Self {
        Self {
            args: vec![subcommand.into()],
            env: Vec::new(),
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

    pub fn args(&self) -> &[OsString] {
        &self.args
    }

    pub fn envs(&self) -> &[(OsString, OsString)] {
        &self.env
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
}

impl GitRunner for CliRunner {
    fn run(&self, cmd: &GitCommand) -> io::Result<GitOutput> {
        let output = Command::new("git")
            .current_dir(&self.workdir)
            .args(cmd.args())
            .envs(cmd.envs().iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .output()?;
        Ok(GitOutput {
            success: output.status.success(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}
