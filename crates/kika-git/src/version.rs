//! Which Git is installed, checked once before the terminal is taken, so
//! a Git too old for Kika is refused with a plain message.

use std::fmt;
use std::io;

use super::runner::{GitCommand, GitRunner};

/// A `major.minor.patch` Git release.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct GitVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl fmt::Display for GitVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// 2.15 brought `GIT_OPTIONAL_LOCKS`. Every read Kika runs sets it, and an
/// older Git ignores it silently and takes the index lock.
pub const MIN_GIT: GitVersion = GitVersion {
    major: 2,
    minor: 15,
    patch: 0,
};

/// 2.20 brought the `break` todo command, which stops a rebase after a
/// commit.
pub const BREAK: GitVersion = GitVersion {
    major: 2,
    minor: 20,
    patch: 0,
};

/// 2.38 brought `rebase --update-refs`, which moves branches stacked on
/// rewritten commits along with them.
pub const UPDATE_REFS: GitVersion = GitVersion {
    major: 2,
    minor: 38,
    patch: 0,
};

/// 2.45 brought `core.commentString`, a comment prefix longer than one
/// character; an older git ignores the key.
pub const COMMENT_STRING: GitVersion = GitVersion {
    major: 2,
    minor: 45,
    patch: 0,
};

/// `git --version`, in the C locale so the words around the number are
/// always English. It reads no repository, so the rest of the standard
/// profile changes nothing.
pub fn version_command() -> GitCommand {
    GitCommand::new("--version").standard_env()
}

/// The leading `major.minor.patch` of `git --version`'s output. Anything
/// after it, such as `(Apple Git-146)` or `.windows.1`, is ignored; a
/// missing patch counts as 0. `None` when there is no number to read.
pub fn parse_version(stdout: &[u8]) -> Option<GitVersion> {
    let text = String::from_utf8_lossy(stdout);
    let rest = text.trim_start().strip_prefix("git version ")?;
    let numbers: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let mut parts = numbers.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    Some(GitVersion {
        major,
        minor,
        patch,
    })
}

/// Whether Kika can run with the installed Git, and which it is. `Err`
/// holds the message to print. A version it cannot read is let through as
/// `None`: an odd build string should not lock anyone out, but features
/// that need a newer Git stay off.
pub fn check(runner: &dyn GitRunner) -> Result<Option<GitVersion>, String> {
    match runner.run(&version_command()) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err(format!(
            "Kika needs Git {MIN_GIT} or newer; git was not found."
        )),
        Ok(output) if output.success => match parse_version(&output.stdout) {
            Some(found) if found < MIN_GIT => {
                Err(format!("Kika needs Git {MIN_GIT} or newer; found {found}."))
            }
            found => Ok(found),
        },
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeRunner, fail, ok};
    use crate::fake::{STANDARD_ENV, env_list};
    use crate::runner::EnvProfile;
    use crate::runner::{GitChild, GitOutput};

    fn v(major: u32, minor: u32, patch: u32) -> GitVersion {
        GitVersion {
            major,
            minor,
            patch,
        }
    }

    #[test]
    fn the_command_is_argv_only_in_the_c_locale() {
        let cmd = version_command();
        assert_eq!(cmd.args(), ["--version"]);
        // It reads no repository: the whole standard profile.
        assert_eq!(cmd.env_profile(), Some(EnvProfile::Standard));
        assert_eq!(cmd.process_env(), env_list(&STANDARD_ENV));
    }

    #[test]
    fn plain_apple_and_windows_versions_are_read() {
        assert_eq!(parse_version(b"git version 2.45.1\n"), Some(v(2, 45, 1)));
        assert_eq!(
            parse_version(b"git version 2.39.3 (Apple Git-146)\n"),
            Some(v(2, 39, 3))
        );
        assert_eq!(
            parse_version(b"git version 2.45.0.windows.1\n"),
            Some(v(2, 45, 0))
        );
        assert_eq!(parse_version(b"git version 3.0\n"), Some(v(3, 0, 0)));
    }

    #[test]
    fn garbage_is_no_version() {
        for out in [
            &b""[..],
            b"\n",
            b"hello\n",
            b"git version \n",
            b"git version x.y.z\n",
            b"git version 2\n",
        ] {
            assert_eq!(
                parse_version(out),
                None,
                "{:?}",
                String::from_utf8_lossy(out)
            );
        }
    }

    fn check_with(stdout: &[u8]) -> Result<Option<GitVersion>, String> {
        check(&FakeRunner::new().expect(&["--version"], ok(stdout)))
    }

    #[test]
    fn a_git_older_than_the_minimum_is_refused_by_name() {
        assert_eq!(
            check_with(b"git version 2.9.5\n"),
            Err("Kika needs Git 2.15.0 or newer; found 2.9.5.".into())
        );
        assert_eq!(
            check_with(b"git version 2.14.9\n"),
            Err("Kika needs Git 2.15.0 or newer; found 2.14.9.".into())
        );
    }

    #[test]
    fn the_minimum_and_newer_pass() {
        assert_eq!(check_with(b"git version 2.15.0\n"), Ok(Some(MIN_GIT)));
        assert_eq!(
            check_with(b"git version 2.39.3 (Apple Git-146)\n"),
            Ok(Some(v(2, 39, 3)))
        );
        assert_eq!(check_with(b"git version 3.1.0\n"), Ok(Some(v(3, 1, 0))));
    }

    #[test]
    fn an_unreadable_version_or_a_failed_run_is_let_through() {
        assert_eq!(check_with(b"git version banana\n"), Ok(None));
        let runner = FakeRunner::new().expect(&["--version"], fail("odd"));
        assert_eq!(check(&runner), Ok(None));
        runner.assert_done();
    }

    #[test]
    fn break_and_update_refs_need_newer_gits_than_the_minimum() {
        assert!(MIN_GIT < BREAK && BREAK < UPDATE_REFS);
        assert_eq!(BREAK.to_string(), "2.20.0");
        assert_eq!(UPDATE_REFS.to_string(), "2.38.0");
    }

    /// A runner with no `git` to start.
    struct NoGit;

    impl GitRunner for NoGit {
        fn run(&self, _: &GitCommand) -> io::Result<GitOutput> {
            Err(io::Error::from(io::ErrorKind::NotFound))
        }

        fn spawn(&self, _: &GitCommand) -> io::Result<GitChild> {
            Err(io::Error::from(io::ErrorKind::NotFound))
        }
    }

    #[test]
    fn a_missing_git_is_refused_as_not_found() {
        assert_eq!(
            check(&NoGit),
            Err("Kika needs Git 2.15.0 or newer; git was not found.".into())
        );
    }
}
