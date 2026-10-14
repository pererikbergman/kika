//! The commits a detached `HEAD` would leave behind when it moves: those
//! no branch, tag or remote-tracking branch reaches, so that once `HEAD`
//! moves away only the reflog still has them.

use kika_git::commit_files::CommitId;
use kika_git::runner::{GitCommand, GitRunner};

/// List the commits reachable from `head` but not from `target` or any
/// branch, tag or remote-tracking branch. A read: no optional locks, and
/// the C locale. Both ids are hex, so neither can be read as an option,
/// and no path follows `--`, so literal pathspecs change nothing.
pub fn left_behind_command(head: &CommitId, target: &CommitId) -> GitCommand {
    GitCommand::new("rev-list")
        .arg(head.as_str())
        .arg("--not")
        .arg(target.as_str())
        .arg("--branches")
        .arg("--tags")
        .arg("--remotes")
        .arg("--")
        .standard_env()
}

/// [`left_behind_command`]'s output, one id per line. A line that is not
/// hex is skipped: it cannot be a commit git listed.
pub fn parse_left_behind(stdout: &[u8]) -> Vec<CommitId> {
    stdout
        .split(|&b| b == b'\n')
        .filter_map(|line| CommitId::parse(std::str::from_utf8(line).ok()?.trim_end()))
        .collect()
}

/// Read the commits moving from `head` to `target` would leave behind.
pub fn read_left_behind(
    runner: &dyn GitRunner,
    head: &CommitId,
    target: &CommitId,
) -> Result<Vec<CommitId>, String> {
    let output = runner
        .run_checked(&left_behind_command(head, target))
        .map_err(|e| e.to_string())?;
    Ok(parse_left_behind(&output.stdout))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use kika_git::CliRunner;
    use kika_git::fake::{FakeRunner, TempDir, fail, git};
    use kika_git::runner::EnvProfile;

    fn id(text: &str) -> CommitId {
        CommitId::parse(text).unwrap()
    }

    fn rev(dir: &Path, name: &str) -> CommitId {
        id(String::from_utf8(git(dir, &["rev-parse", name]))
            .unwrap()
            .trim())
    }

    /// A repository with two commits on `main`, then `HEAD` detached on
    /// two more above it. Returns the repo, `main`'s tip and `HEAD`.
    fn detached_above_main(label: &str) -> (TempDir, CommitId, CommitId) {
        let tmp = TempDir::new(label);
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["commit", "-q", "--allow-empty", "-m", "one"]);
        git(dir, &["commit", "-q", "--allow-empty", "-m", "two"]);
        git(dir, &["checkout", "-q", "--detach"]);
        git(dir, &["commit", "-q", "--allow-empty", "-m", "three"]);
        git(dir, &["commit", "-q", "--allow-empty", "-m", "four"]);
        let main = rev(dir, "main");
        let head = rev(dir, "HEAD");
        (tmp, main, head)
    }

    #[test]
    fn the_command_is_a_read_in_the_c_locale_without_end_of_options() {
        let cmd = left_behind_command(&id("abc123"), &id("def456"));
        assert_eq!(
            cmd.args(),
            [
                "rev-list",
                "abc123",
                "--not",
                "def456",
                "--branches",
                "--tags",
                "--remotes",
                "--"
            ]
        );
        assert!(!cmd.args().iter().any(|a| a == "--end-of-options"));
        assert_eq!(cmd.env_profile(), Some(EnvProfile::Standard));
        assert_eq!(
            cmd.process_env(),
            [
                ("LC_ALL".into(), "C".into()),
                ("GIT_OPTIONAL_LOCKS".into(), "0".into()),
                ("GIT_LITERAL_PATHSPECS".into(), "1".into())
            ]
        );
    }

    #[test]
    fn parse_keeps_hex_lines_and_skips_the_rest() {
        assert_eq!(
            parse_left_behind(b"aaaa\nbbbb\r\n\n--x\nzz\ncccc"),
            [id("aaaa"), id("bbbb"), id("cccc")]
        );
    }

    #[test]
    fn a_failure_is_terminal_safe_and_never_empty() {
        let argv = [
            "rev-list",
            "aa",
            "--not",
            "bb",
            "--branches",
            "--tags",
            "--remotes",
            "--",
        ];
        let fake = FakeRunner::new().expect(&argv, fail("fatal: bad\x1b]0;t\x07"));
        let err = read_left_behind(&fake, &id("aa"), &id("bb")).unwrap_err();
        assert!(err.contains("fatal: bad"), "{err}");
        assert!(!err.contains('\x1b'), "{err}");
        fake.assert_done();

        let fake = FakeRunner::new().expect(&argv, fail(""));
        assert_eq!(
            read_left_behind(&fake, &id("aa"), &id("bb")).unwrap_err(),
            "git rev-list failed"
        );
        let read = left_behind_command(&id("aa"), &id("bb")).process_env();
        assert_eq!(fake.process_envs(), [read]);
    }

    #[test]
    fn the_commits_only_head_reaches_are_left_behind() {
        let (tmp, main, head) = detached_above_main("left-behind");
        let dir = tmp.path();
        let runner = CliRunner::isolated(dir);
        let left = read_left_behind(&runner, &head, &main).unwrap();
        assert_eq!(left, [head, rev(dir, "HEAD~1")]);
    }

    #[test]
    fn commits_a_branch_tag_or_remote_tracking_branch_reaches_are_kept() {
        for (label, keep) in [
            ("left-branch", vec!["branch", "-q", "keep"]),
            ("left-tag", vec!["tag", "keep"]),
            (
                "left-remote",
                vec!["update-ref", "refs/remotes/origin/keep", "HEAD"],
            ),
        ] {
            let (tmp, main, head) = detached_above_main(label);
            let dir = tmp.path();
            git(dir, &keep);
            let runner = CliRunner::isolated(dir);
            assert_eq!(
                read_left_behind(&runner, &head, &main),
                Ok(vec![]),
                "{label}"
            );
        }
    }

    #[test]
    fn moving_to_a_descendant_leaves_nothing_behind() {
        let (tmp, main, head) = detached_above_main("left-forward");
        let runner = CliRunner::isolated(tmp.path());
        assert_eq!(read_left_behind(&runner, &main, &head), Ok(vec![]));
    }
}
