//! Where the repository's worktree starts. `git status` names files relative
//! to it, so every command must run there for those names to mean the same
//! files wherever Kika was started.

use std::path::PathBuf;

use super::path::path_from_stdout;
use super::runner::{GitCommand, GitRunner};

/// Ask git for the top level of the worktree. It takes no paths and no
/// lock, so the whole standard profile is safe on it.
pub fn toplevel_command() -> GitCommand {
    GitCommand::new("rev-parse")
        .arg("--show-toplevel")
        .standard_env()
}

/// The path `rev-parse --show-toplevel` printed, from its raw bytes so a
/// directory name that is not UTF-8 survives. `None` when it printed nothing.
pub fn toplevel_from(stdout: &[u8]) -> Option<PathBuf> {
    let path = path_from_stdout(stdout);
    (!path.as_os_str().is_empty()).then_some(path)
}

/// The top level of the worktree `runner` is in, or `None` outside one
/// (outside a repo, in a bare repo or inside `.git`).
pub fn find_toplevel(runner: &dyn GitRunner) -> Option<PathBuf> {
    match runner.run(&toplevel_command()) {
        Ok(output) if output.success => toplevel_from(&output.stdout),
        _ => None,
    }
}

/// The directory to run every git command in: the worktree's top level,
/// because status paths are root-relative. When there is none, `launch`
/// is kept.
#[cfg(test)]
pub fn resolve_workdir(runner: &dyn GitRunner, launch: PathBuf) -> PathBuf {
    find_toplevel(runner).unwrap_or(launch)
}

/// `git init -q`: a write, so only the locale.
fn init_command() -> GitCommand {
    GitCommand::new("init").arg("-q").locale_env()
}

/// Make the runner's directory a new repository and return its top level.
/// `git init` takes no path here: it acts where the runner runs, which is
/// the launch directory whenever there is no repository to offer.
pub fn init_repo(runner: &dyn GitRunner) -> Result<PathBuf, String> {
    runner
        .run_checked(&init_command())
        .map_err(|e| e.message_or("git init failed"))?;
    find_toplevel(runner).ok_or_else(|| "git init made no worktree here".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::CliRunner;
    use crate::git::fake::{FakeRunner, TempDir, fail, git, ok};
    use crate::git::runner::EnvProfile;
    use crate::test_support::{STANDARD_ENV, env_list};

    const TOPLEVEL: &[&str] = &["rev-parse", "--show-toplevel"];

    fn launch() -> PathBuf {
        PathBuf::from("/r/top/d/e")
    }

    #[test]
    fn toplevel_command_is_argv_only_in_the_c_locale() {
        let cmd = toplevel_command();
        let args: Vec<_> = cmd.args().iter().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(args, TOPLEVEL);
        // A read with no path and no lock: the whole standard profile.
        assert_eq!(cmd.env_profile(), Some(EnvProfile::Standard));
        assert_eq!(cmd.process_env(), env_list(&STANDARD_ENV));
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
    fn find_toplevel_is_none_outside_a_worktree() {
        let runner = FakeRunner::new().expect(TOPLEVEL, fail("fatal: not a git repository"));
        assert_eq!(find_toplevel(&runner), None);
        let runner = FakeRunner::new().expect(TOPLEVEL, ok(b"/r/top\n"));
        assert_eq!(find_toplevel(&runner), Some(PathBuf::from("/r/top")));
    }

    #[test]
    fn init_runs_git_init_then_reads_the_new_toplevel() {
        let runner = FakeRunner::new()
            .expect(&["init", "-q"], ok(b""))
            .expect(TOPLEVEL, ok(b"/r/new\n"));
        assert_eq!(init_repo(&runner), Ok(PathBuf::from("/r/new")));
        runner.assert_done();
        // The init is a write, so only the locale; the read gets it all.
        assert_eq!(init_command().env_profile(), Some(EnvProfile::Locale));
        assert_eq!(
            runner.process_envs(),
            [env_list(&[("LC_ALL", "C")]), env_list(&STANDARD_ENV)]
        );
    }

    #[test]
    fn a_failed_init_returns_gits_words() {
        let runner = FakeRunner::new().expect(&["init", "-q"], fail("fatal: cannot mkdir \x1b[2J"));
        let error = init_repo(&runner).unwrap_err();
        assert!(error.starts_with("fatal: cannot mkdir"), "{error:?}");
        assert!(!error.contains(char::is_control), "{error:?}");
    }

    #[test]
    fn real_git_init_makes_the_launch_dir_a_repository() {
        let tmp = TempDir::new("init");
        let runner = CliRunner::isolated(tmp.path());
        let top = init_repo(&runner).unwrap();
        assert_eq!(
            top.canonicalize().unwrap(),
            tmp.path().canonicalize().unwrap()
        );
        assert!(tmp.path().join(".git").is_dir());
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
