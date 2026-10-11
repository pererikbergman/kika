//! Git attributes Kika reads for one file.

use super::diff::terminal_safe;
use super::path::RepoPath;
use super::runner::{GitCommand, GitRunner};
use crate::conflict::DEFAULT_MARKER_SIZE;

/// `git check-attr -z conflict-marker-size -- <path>`. The path is read
/// literally, never as a pattern.
pub fn marker_size_command(path: &RepoPath) -> GitCommand {
    GitCommand::new("check-attr")
        .arg("-z")
        .arg("conflict-marker-size")
        .arg("--")
        .arg(path.as_os_str())
        .env("GIT_LITERAL_PATHSPECS", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
}

/// How long `path`'s conflict markers are: its `conflict-marker-size`
/// attribute, or git's default when that is unset or not a positive number.
pub fn conflict_marker_size(runner: &dyn GitRunner, path: &RepoPath) -> Result<usize, String> {
    let output = runner
        .run(&marker_size_command(path))
        .map_err(|e| e.to_string())?;
    if !output.success {
        let message = terminal_safe(&output.stderr);
        return Err(if message.is_empty() {
            "git check-attr failed".to_string()
        } else {
            message
        });
    }
    Ok(parse_marker_size(&output.stdout))
}

/// The value from `-z` output, `<path>\0<attr>\0<value>\0`.
fn parse_marker_size(stdout: &[u8]) -> usize {
    stdout
        .split(|&b| b == 0)
        .nth(2)
        .and_then(|value| std::str::from_utf8(value).ok())
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|&size| size > 0)
        .unwrap_or(DEFAULT_MARKER_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::fake::{FakeRunner, TempDir, fail, git, ok};

    #[test]
    fn the_command_is_literal_and_names_the_path_after_the_separator() {
        let cmd = marker_size_command(&"-x".into());
        let args: Vec<_> = cmd.args().iter().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args,
            ["check-attr", "-z", "conflict-marker-size", "--", "-x"]
        );
        assert!(
            cmd.envs()
                .contains(&("GIT_LITERAL_PATHSPECS".into(), "1".into()))
        );
    }

    #[test]
    fn a_value_that_is_not_a_positive_number_means_the_default() {
        for (value, size) in [
            ("12", 12),
            ("unspecified", 7),
            ("set", 7),
            ("unset", 7),
            ("0", 7),
            ("-3", 7),
            ("x", 7),
        ] {
            let out = format!("a.txt\0conflict-marker-size\0{value}\0");
            let runner = FakeRunner::new().expect(
                &["check-attr", "-z", "conflict-marker-size", "--", "a.txt"],
                ok(out.as_bytes()),
            );
            assert_eq!(
                conflict_marker_size(&runner, &"a.txt".into()),
                Ok(size),
                "{value}"
            );
        }
    }

    #[test]
    fn a_failure_carries_gits_message() {
        let runner = FakeRunner::new().expect(
            &["check-attr", "-z", "conflict-marker-size", "--", "a.txt"],
            fail("fatal: bad\n"),
        );
        assert_eq!(
            conflict_marker_size(&runner, &"a.txt".into()),
            Err("fatal: bad".into())
        );
    }

    #[test]
    fn real_git_reads_the_attribute_from_gitattributes() {
        let tmp = TempDir::new("marker-size");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        std::fs::write(dir.join(".gitattributes"), "*.md conflict-marker-size=12\n").unwrap();
        let runner = crate::git::CliRunner::isolated(dir);
        assert_eq!(conflict_marker_size(&runner, &"a.md".into()), Ok(12));
        assert_eq!(conflict_marker_size(&runner, &"a.txt".into()), Ok(7));
    }
}
