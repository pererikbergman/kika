//! Where every local branch points, so a history rewrite can tell which
//! branches sit on the commits it would replace, and the argv that checks
//! out a commit with no branch.

use crate::git::commit_files::CommitId;
use crate::git::log::CommitRow;
use crate::git::rebase_plan::branch_name;
use crate::git::runner::{GitCommand, GitRunner};

/// One branch tip: the commit and the full ref name, raw bytes, as
/// `refs/heads/<name>`.
pub type Tip = (CommitId, Vec<u8>);

/// List every local branch with its commit, one `<oid>NUL<ref>` line each.
/// A read: no optional locks, and the C locale. `refs/heads/` is a ref
/// pattern, not a pathspec, so literal pathspecs change nothing.
pub fn tips_command() -> GitCommand {
    GitCommand::new("for-each-ref")
        .arg("--format=%(objectname)%00%(refname)")
        .arg("refs/heads/")
        .standard_env()
}

/// [`tips_command`]'s output as tips. A line that is not an id and a ref
/// is skipped: it cannot be a branch Kika would have to move.
pub fn parse_tips(stdout: &[u8]) -> Vec<Tip> {
    stdout
        .split(|&b| b == b'\n')
        .filter_map(|line| {
            let at = line.iter().position(|&b| b == 0)?;
            let oid = std::str::from_utf8(&line[..at]).ok()?;
            let name = &line[at + 1..];
            if name.is_empty() {
                return None;
            }
            Some((CommitId::parse(oid)?, name.to_vec()))
        })
        .collect()
}

/// Read the branch tips.
pub fn read_tips(runner: &dyn GitRunner) -> Result<Vec<Tip>, String> {
    let output = runner
        .run_checked(&tips_command())
        .map_err(|e| e.to_string())?;
    Ok(parse_tips(&output.stdout))
}

/// The refs other than `head_ref`, the branch being rewritten, that point
/// at any of `rows[..range_len]`: branches stacked on the commits a
/// rewrite would replace.
pub fn stacked(
    rows: &[CommitRow],
    range_len: usize,
    tips: &[Tip],
    head_ref: Option<&[u8]>,
) -> Vec<Vec<u8>> {
    let range = &rows[..range_len.min(rows.len())];
    tips.iter()
        .filter(|(oid, name)| {
            Some(name.as_slice()) != head_ref && range.iter().any(|row| row.oid == oid.as_str())
        })
        .map(|(_, name)| name.clone())
        .collect()
}

/// Why a rewrite is refused on a git without `--update-refs`.
pub fn left_behind(branch: &[u8]) -> String {
    format!(
        "Branch {} sits on these commits; Git 2.38 or newer moves it along",
        branch_name(branch)
    )
}

/// Check out `oid` with no branch. The id is hex digits only, so it cannot
/// be read as an option; `--` keeps it from being read as a path.
pub(crate) fn detached_checkout(oid: &CommitId) -> GitCommand {
    GitCommand::new("checkout")
        .arg("-q")
        .arg("--detach")
        .arg(oid.as_str())
        .arg("--")
        .write_env()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::runner::EnvProfile;
    use crate::op::{GitOp, ResetMode, op_command, run_op};
    use crate::test_support::{argv, rev_parse, two_commit_repo};

    fn row(digit: char) -> CommitRow {
        CommitRow {
            oid: digit.to_string().repeat(40),
            short_oid: digit.to_string().repeat(7),
            parents: vec![],
            author: "Ann".into(),
            time: 0,
            subject: "s".into(),
            subject_altered: false,
        }
    }

    fn tip(digit: char, name: &str) -> Tip {
        (
            CommitId::parse(&digit.to_string().repeat(40)).unwrap(),
            name.as_bytes().to_vec(),
        )
    }

    #[test]
    fn the_command_is_a_read_in_the_c_locale() {
        let cmd = tips_command();
        assert_eq!(
            cmd.args(),
            [
                "for-each-ref",
                "--format=%(objectname)%00%(refname)",
                "refs/heads/"
            ]
        );
        assert_eq!(cmd.env_profile(), EnvProfile::Standard);
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
    fn reading_tips_runs_the_read_and_names_a_silent_failure() {
        use crate::git::fake::{FakeRunner, fail, ok};
        let argv = [
            "for-each-ref",
            "--format=%(objectname)%00%(refname)",
            "refs/heads/",
        ];
        let oid = "a".repeat(40);
        let line = format!("{oid}\0refs/heads/main\n");
        let runner = FakeRunner::new().expect(&argv, ok(line.as_bytes()));
        assert_eq!(read_tips(&runner), Ok(vec![tip('a', "refs/heads/main")]));
        assert_eq!(runner.process_envs(), [tips_command().process_env()]);

        let runner = FakeRunner::new()
            .expect(&argv, fail(""))
            .expect(&argv, fail("fatal: \x1b[2Jbad\n"));
        assert_eq!(read_tips(&runner), Err("git for-each-ref failed".into()));
        assert_eq!(read_tips(&runner), Err("fatal: \u{FFFD}[2Jbad".into()));
    }

    #[test]
    fn tips_keep_raw_ref_bytes_and_skip_odd_lines() {
        let out = b"aaaa\0refs/heads/main\nbbbb\0refs/heads/caf\xe9\n\nzz\0refs/heads/x\ncccc\0\n";
        assert_eq!(
            parse_tips(out),
            [
                (
                    CommitId::parse("aaaa").unwrap(),
                    b"refs/heads/main".to_vec()
                ),
                (
                    CommitId::parse("bbbb").unwrap(),
                    b"refs/heads/caf\xe9".to_vec()
                ),
            ]
        );
    }

    #[test]
    fn stacked_lists_other_branches_inside_the_range() {
        let rows = [row('a'), row('b'), row('c')];
        let tips = [
            tip('a', "refs/heads/main"),
            tip('b', "refs/heads/feature"),
            tip('c', "refs/heads/below"),
            tip('a', "refs/heads/also-head"),
        ];
        assert_eq!(
            stacked(&rows, 2, &tips, Some(b"refs/heads/main")),
            [
                b"refs/heads/feature".to_vec(),
                b"refs/heads/also-head".to_vec()
            ]
        );
        assert_eq!(stacked(&rows, 1, &tips, None).len(), 2);
        assert_eq!(stacked(&rows, 9, &tips, Some(b"refs/heads/main")).len(), 3);
    }

    #[test]
    fn the_reason_names_the_branch_safely() {
        assert_eq!(
            left_behind(b"refs/heads/feature"),
            "Branch feature sits on these commits; Git 2.38 or newer moves it along"
        );
        assert!(!left_behind(b"refs/heads/x\x1b]0;t\x07").contains('\x1b'));
    }

    fn oid(text: &str) -> CommitId {
        CommitId::parse(text).unwrap()
    }

    #[test]
    fn checkout_and_reset_name_the_commit_before_the_path_separator() {
        let cases: [(GitOp, &[&str]); 4] = [
            (
                GitOp::CheckoutCommit(oid("abc123")),
                &["checkout", "-q", "--detach", "abc123", "--"],
            ),
            (
                GitOp::Reset {
                    to: oid("abc123"),
                    mode: ResetMode::Soft,
                },
                &["reset", "-q", "--soft", "abc123", "--"],
            ),
            (
                GitOp::Reset {
                    to: oid("abc123"),
                    mode: ResetMode::Mixed,
                },
                &["reset", "-q", "--mixed", "abc123", "--"],
            ),
            (
                GitOp::Reset {
                    to: oid("abc123"),
                    mode: ResetMode::Hard,
                },
                &["reset", "-q", "--hard", "abc123", "--"],
            ),
        ];
        for (op, expected) in cases {
            let cmd = op_command(&op);
            assert_eq!(argv(&cmd), expected, "{op:?}");
            assert_eq!(cmd.env_profile(), EnvProfile::Write, "{op:?}");
            let envs: Vec<_> = cmd
                .process_env()
                .iter()
                .map(|(k, v)| {
                    (
                        k.to_str().unwrap().to_owned(),
                        v.to_str().unwrap().to_owned(),
                    )
                })
                .collect();
            assert_eq!(
                envs,
                [("LC_ALL", "C"), ("GIT_LITERAL_PATHSPECS", "1")]
                    .map(|(k, v)| (k.to_owned(), v.to_owned())),
                "{op:?}"
            );
        }
    }

    #[test]
    fn revert_is_plain_git_revert_and_sets_no_env() {
        let cmd = op_command(&GitOp::Revert(oid("abc123")));
        assert_eq!(argv(&cmd), ["revert", "abc123"]);
        assert!(cmd.envs().is_empty(), "{:?}", cmd.envs());
        assert_eq!(cmd.input(), None);
    }

    #[test]
    fn checkout_commit_leaves_head_detached_at_it_in_real_git() {
        use crate::git::fake::{git, try_git};

        let (tmp, first) = two_commit_repo();
        let dir = tmp.path();
        let runner = crate::git::CliRunner::isolated(dir);
        run_op(&runner, &GitOp::CheckoutCommit(first.clone())).unwrap();
        assert_eq!(rev_parse(dir, "HEAD"), first);
        assert!(
            !try_git(dir, &["symbolic-ref", "-q", "HEAD"]),
            "HEAD is on a branch"
        );
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one\n");
        assert_eq!(git(dir, &["status", "--porcelain"]), b"");
    }

    #[test]
    fn revert_makes_a_commit_on_head_that_undoes_the_change_in_real_git() {
        use crate::git::fake::git;

        let (tmp, _) = two_commit_repo();
        let dir = tmp.path();
        let second = rev_parse(dir, "HEAD");
        let runner = crate::git::CliRunner::isolated(dir).with_env("GIT_EDITOR", "true");
        run_op(&runner, &GitOp::Revert(second.clone())).unwrap();
        assert_eq!(rev_parse(dir, "HEAD^"), second);
        assert_eq!(git(dir, &["show", "HEAD:a.txt"]), b"one\n");
        assert_eq!(git(dir, &["rev-list", "--count", "HEAD"]), b"3\n");
        assert_eq!(git(dir, &["status", "--porcelain"]), b"");
    }

    #[test]
    fn each_reset_mode_keeps_what_it_says_in_real_git() {
        use crate::git::fake::git;

        let cases = [
            (ResetMode::Soft, "M  a.txt\n"),
            (ResetMode::Mixed, " M a.txt\n"),
            (ResetMode::Hard, ""),
        ];
        for (mode, status) in cases {
            let (tmp, first) = two_commit_repo();
            let dir = tmp.path();
            let runner = crate::git::CliRunner::isolated(dir);
            run_op(
                &runner,
                &GitOp::Reset {
                    to: first.clone(),
                    mode,
                },
            )
            .unwrap();
            assert_eq!(rev_parse(dir, "HEAD"), first, "{mode:?}");
            // The branch moved with it; HEAD is still on it.
            assert!(
                crate::git::fake::try_git(dir, &["symbolic-ref", "-q", "HEAD"]),
                "{mode:?}"
            );
            assert_eq!(
                String::from_utf8(git(dir, &["status", "--porcelain"])).unwrap(),
                status,
                "{mode:?}"
            );
            let worktree = std::fs::read_to_string(dir.join("a.txt")).unwrap();
            let expected = if mode == ResetMode::Hard {
                "one\n"
            } else {
                "two\n"
            };
            assert_eq!(worktree, expected, "{mode:?}");
        }
    }
}
