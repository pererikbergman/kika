//! Where the repository's worktree starts. `git status` names files relative
//! to it, so every command must run there for those names to mean the same
//! files wherever Kika was started.

use std::path::PathBuf;

use super::path::path_from_stdout;
use super::runner::{GitCommand, GitRunner};

/// Ask git for the top level of the worktree. It takes no paths, so it
/// needs no `--` or literal pathspecs.
pub fn toplevel_command() -> GitCommand {
    GitCommand::new("rev-parse")
        .arg("--show-toplevel")
        .env("LC_ALL", "C")
}

/// The path `rev-parse --show-toplevel` printed, from its raw bytes so a
/// directory name that is not UTF-8 survives. `None` when it printed nothing.
pub fn toplevel_from(stdout: &[u8]) -> Option<PathBuf> {
    let path = path_from_stdout(stdout);
    (!path.as_os_str().is_empty()).then_some(path)
}

/// The directory to run every git command in: the worktree's top level,
/// because status paths are root-relative. When there is none (outside a
/// repo, in a bare repo or inside `.git`), `launch` is kept, so the first
/// status fails and shows git's own message.
pub fn resolve_workdir(runner: &dyn GitRunner, launch: PathBuf) -> PathBuf {
    match runner.run(&toplevel_command()) {
        Ok(output) if output.success => toplevel_from(&output.stdout).unwrap_or(launch),
        _ => launch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::CliRunner;
    use crate::git::fake::{FakeRunner, TempDir, fail, git, ok};

    const TOPLEVEL: &[&str] = &["rev-parse", "--show-toplevel"];

    fn launch() -> PathBuf {
        PathBuf::from("/r/top/d/e")
    }

    #[test]
    fn toplevel_command_is_argv_only_in_the_c_locale() {
        let cmd = toplevel_command();
        let args: Vec<_> = cmd.args().iter().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(args, TOPLEVEL);
        let envs: Vec<_> = cmd
            .envs()
            .iter()
            .map(|(k, v)| (k.to_str().unwrap(), v.to_str().unwrap()))
            .collect();
        assert_eq!(envs, [("LC_ALL", "C")]);
    }

    #[test]
    fn toplevel_drops_only_one_trailing_newline() {
        assert_eq!(
            toplevel_from(b"/work/my repo\n"),
            Some(PathBuf::from("/work/my repo"))
        );
        assert_eq!(
            toplevel_from(b"/work/x\n\n"),
            Some(PathBuf::from("/work/x\n"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn toplevel_keeps_bytes_that_are_not_utf8() {
        use std::os::unix::ffi::OsStrExt;
        let top = toplevel_from(b"/r\xff/top\n").unwrap();
        assert_eq!(top.as_os_str().as_bytes(), b"/r\xff/top");
    }

    #[test]
    fn empty_output_is_no_toplevel() {
        assert_eq!(toplevel_from(b""), None);
        assert_eq!(toplevel_from(b"\n"), None);
    }

    #[test]
    fn the_toplevel_git_prints_becomes_the_workdir() {
        let runner = FakeRunner::new().expect(TOPLEVEL, ok(b"/r/top\n"));
        assert_eq!(resolve_workdir(&runner, launch()), PathBuf::from("/r/top"));
        runner.assert_done();
    }

    #[test]
    fn outside_a_repo_the_launch_dir_is_kept() {
        let runner = FakeRunner::new().expect(TOPLEVEL, fail("fatal: not a git repository"));
        assert_eq!(resolve_workdir(&runner, launch()), launch());
        runner.assert_done();
    }

    #[test]
    fn without_a_work_tree_the_launch_dir_is_kept() {
        let runner = FakeRunner::new().expect(
            TOPLEVEL,
            fail("fatal: this operation must be run in a work tree"),
        );
        assert_eq!(resolve_workdir(&runner, launch()), launch());
        runner.assert_done();
    }

    #[test]
    fn an_empty_toplevel_keeps_the_launch_dir() {
        let runner = FakeRunner::new().expect(TOPLEVEL, ok(b""));
        assert_eq!(resolve_workdir(&runner, launch()), launch());
        runner.assert_done();
    }

    #[test]
    fn real_git_resolves_a_nested_launch_dir_to_the_toplevel() {
        let tmp = TempDir::new("repo-root");
        let root = tmp.path();
        git(root, &["init", "-q"]);
        let nested = root.join("d").join("e");
        std::fs::create_dir_all(&nested).unwrap();
        let expected = toplevel_from(&git(root, &["rev-parse", "--show-toplevel"])).unwrap();

        let resolved = resolve_workdir(&CliRunner::isolated(&nested), nested.clone());

        // The temp dir may sit behind a symlink, as `/tmp` does on macOS.
        assert_eq!(
            resolved.canonicalize().unwrap(),
            expected.canonicalize().unwrap()
        );
        assert_eq!(
            resolved.canonicalize().unwrap(),
            root.canonicalize().unwrap()
        );
    }

    #[test]
    fn real_git_outside_a_repo_keeps_the_launch_dir() {
        let tmp = TempDir::new("no-repo");
        let runner = CliRunner::isolated(tmp.path());
        assert_eq!(
            resolve_workdir(&runner, tmp.path().to_path_buf()),
            tmp.path().to_path_buf()
        );
    }

    #[test]
    fn real_git_without_a_work_tree_keeps_the_launch_dir() {
        let tmp = TempDir::new("bare");
        let bare = tmp.path().join("bare.git");
        git(tmp.path(), &["init", "-q", "--bare", "bare.git"]);
        let work = tmp.path().join("work");
        std::fs::create_dir(&work).unwrap();
        git(&work, &["init", "-q"]);
        let inside_git_dir = work.join(".git");

        for launch in [bare, inside_git_dir] {
            let runner = CliRunner::isolated(&launch);
            assert_eq!(resolve_workdir(&runner, launch.clone()), launch);
        }
    }
}
