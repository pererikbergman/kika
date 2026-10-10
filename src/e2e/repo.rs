//! A real Git repository in a temp dir, built step by step for a test and
//! queried afterwards to check what the app did to it.

use std::ffi::OsStr;

use crate::git::fake::{TempDir, git, try_git};
use crate::git::path::RepoPath;
use crate::git::status::read_status;
use crate::git::{CliRunner, FileEntry};

/// Setup steps take and return the repo, so a test reads as one chain:
/// `TestRepo::new().write("a.txt", "one\n").commit("init")`.
pub struct TestRepo {
    dir: TempDir,
}

impl TestRepo {
    /// An empty repository with no commits, made from no template. The
    /// app's runner ignores the host's global and system config (see
    /// [`TestRepo::runner`]), so the local config gives its `git commit` an
    /// identity, no signing and only this repo's hooks.
    pub fn new() -> Self {
        let dir = TempDir::new("e2e");
        git(dir.path(), &["init", "-q", "--template="]);
        for (key, value) in [
            ("user.name", "Kika Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
            ("core.hooksPath", ".git/hooks"),
        ] {
            git(dir.path(), &["config", key, value]);
        }
        Self { dir }
    }

    /// Install an executable hook script as `.git/hooks/<name>`.
    #[cfg(unix)]
    pub fn hook(self, name: &str, script: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;

        let hooks = self.dir.path().join(".git").join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let file = hooks.join(name);
        std::fs::write(&file, script).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        self
    }

    /// The full message of the `HEAD` commit, as git prints it.
    pub fn head_message(&self) -> String {
        String::from_utf8(self.git(["log", "-1", "--format=%B"])).unwrap()
    }

    /// The commit `HEAD` points at.
    pub fn head(&self) -> String {
        String::from_utf8(self.git(["rev-parse", "HEAD"])).unwrap()
    }

    /// Write `contents` to `path`, creating parent directories as needed.
    pub fn write(self, path: &str, contents: &str) -> Self {
        let file = self.dir.path().join(path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(file, contents).unwrap();
        self
    }

    /// Delete the file at `path`.
    pub fn remove(&self, path: &str) {
        std::fs::remove_file(self.dir.path().join(path)).unwrap();
    }

    /// Commit everything in the worktree.
    pub fn commit(self, message: &str) -> Self {
        self.git(["add", "-A"]);
        self.git(["commit", "-q", "-m", message]);
        self
    }

    pub fn stage(self, path: &str) -> Self {
        self.git(["add", "--", path]);
        self
    }

    /// Run any git command in the repo, failing the test if git fails.
    pub fn git<A: AsRef<OsStr>>(&self, args: impl IntoIterator<Item = A>) -> Vec<u8> {
        let args: Vec<A> = args.into_iter().collect();
        git(self.dir.path(), &args)
    }

    /// Run a git command that may fail, such as a merge that stops on a
    /// conflict. Returns whether it succeeded.
    pub fn try_git<A: AsRef<OsStr>>(&self, args: impl IntoIterator<Item = A>) -> bool {
        let args: Vec<A> = args.into_iter().collect();
        try_git(self.dir.path(), &args)
    }

    pub fn exists(&self, path: &str) -> bool {
        self.dir.path().join(path).exists()
    }

    pub fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(path)).unwrap()
    }

    /// The paths in the index that differ from `HEAD`.
    pub fn staged(&self) -> Vec<String> {
        self.git(["diff", "--cached", "--name-only", "-z"])
            .split(|&b| b == 0)
            .filter(|name| !name.is_empty())
            .map(|name| String::from_utf8_lossy(name).into_owned())
            .collect()
    }

    /// The runner the app uses in tests: real git, host config shut out.
    pub fn runner(&self) -> CliRunner {
        CliRunner::isolated(self.dir.path())
    }

    /// The file's status as the app's own status reader sees it.
    pub fn entry(&self, path: &str) -> Option<FileEntry> {
        let runner = self.runner();
        let path = RepoPath::from(path);
        read_status(&runner)
            .unwrap()
            .files
            .into_iter()
            .find(|entry| entry.path == path)
    }
}
