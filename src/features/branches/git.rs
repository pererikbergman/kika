//! The argv for creating, renaming and deleting one branch and for
//! setting or unsetting its upstream, which `op_command` in `op.rs` builds
//! through these five functions. Argv only, with `--` before the names, the
//! start point and the upstream as full refs, and raw name bytes.

use std::ffi::OsString;

use kika_git::branches::BranchName;
use kika_git::remotes::RemoteBranch;
use kika_git::runner::GitCommand;

/// `git checkout -b` for `name` at the branch `from`.
///
/// The start point as a full ref, so a tag of the same name, or a
/// name like an option, cannot stand in for it.
pub(crate) fn create_command(name: &str, from: &BranchName) -> GitCommand {
    GitCommand::new("checkout")
        .arg("-q")
        .arg("-b")
        .arg(name)
        .arg(from.full_ref())
        .arg("--")
}

/// `git branch -m` from `from` to `to`.
pub(crate) fn rename_command(from: &BranchName, to: &str) -> GitCommand {
    GitCommand::new("branch")
        .arg("-m")
        .arg("--")
        .arg(from.as_os_string())
        .arg(to)
}

/// `git branch -d`, or `-D` when `force`, for `name`.
pub(crate) fn delete_command(name: &BranchName, force: bool) -> GitCommand {
    GitCommand::new("branch")
        .arg(if force { "-D" } else { "-d" })
        .arg("--")
        .arg(name.as_os_string())
}

/// `git branch --set-upstream-to` for `branch`, naming `upstream` by its
/// full ref.
pub(crate) fn set_upstream_command(branch: &BranchName, upstream: &RemoteBranch) -> GitCommand {
    let mut flag = OsString::from("--set-upstream-to=");
    flag.push(upstream.full_ref());
    GitCommand::new("branch")
        .arg(flag)
        .arg("--")
        .arg(branch.as_os_string())
}

/// `git branch --unset-upstream` for `branch`.
pub(crate) fn unset_upstream_command(branch: &BranchName) -> GitCommand {
    GitCommand::new("branch")
        .arg("--unset-upstream")
        .arg("--")
        .arg(branch.as_os_string())
}

#[cfg(test)]
mod tests {
    use crate::op::{GitOp, op_command, run_op};
    use crate::test_support::{argv, cloned_repo, dirty_repo};
    use kika_git::fake::{FakeRunner, config, ok};
    use kika_git::remotes::RemoteBranch;

    #[test]
    fn branch_ops_issue_exact_argv_with_the_separator_and_full_refs() {
        let cases: [(GitOp, &[&str]); 7] = [
            (
                GitOp::Checkout("feature".into()),
                &["checkout", "-q", "feature", "--"],
            ),
            // A ref made outside git's porcelain can look like an option;
            // it must stay a branch name, never become `-f`.
            (
                GitOp::Checkout("-f".into()),
                &["checkout", "-q", "--end-of-options", "-f", "--"],
            ),
            (
                GitOp::Checkout("--force".into()),
                &["checkout", "-q", "--end-of-options", "--force", "--"],
            ),
            (
                GitOp::CreateBranch {
                    name: "new".into(),
                    from: "feature".into(),
                },
                &["checkout", "-q", "-b", "new", "refs/heads/feature", "--"],
            ),
            (
                GitOp::RenameBranch {
                    from: "-old".into(),
                    to: "fresh".into(),
                },
                &["branch", "-m", "--", "-old", "fresh"],
            ),
            (
                GitOp::DeleteBranch {
                    name: "done".into(),
                    force: false,
                },
                &["branch", "-d", "--", "done"],
            ),
            (
                GitOp::DeleteBranch {
                    name: "done".into(),
                    force: true,
                },
                &["branch", "-D", "--", "done"],
            ),
        ];
        for (op, argv) in cases {
            let runner = FakeRunner::new().expect(argv, ok(b""));
            assert_eq!(run_op(&runner, &op), Ok(()), "{op:?}");
            runner.assert_done();
        }
    }

    #[cfg(unix)]
    #[test]
    fn branch_ops_put_the_raw_bytes_of_a_name_in_argv() {
        use std::os::unix::ffi::OsStrExt;
        let raw = kika_git::branches::BranchName::from_bytes(b"x\xff");
        let create = op_command(&GitOp::CreateBranch {
            name: "new".into(),
            from: raw.clone(),
        });
        assert_eq!(create.args()[4].as_bytes(), b"refs/heads/x\xff");
        let delete = op_command(&GitOp::DeleteBranch {
            name: raw,
            force: true,
        });
        assert_eq!(delete.args()[3].as_bytes(), b"x\xff");
    }

    #[test]
    fn a_new_branch_named_like_an_option_is_refused_by_git_in_real_git() {
        let tmp = dirty_repo();
        let dir = tmp.path();
        let runner = kika_git::CliRunner::isolated(dir);
        for name in ["-f", "--orphan"] {
            let create = GitOp::CreateBranch {
                name: name.into(),
                from: "main".into(),
            };
            assert!(run_op(&runner, &create).is_err(), "{name}");
            let head = kika_git::fake::git(dir, &["symbolic-ref", "HEAD"]);
            assert_eq!(head, b"refs/heads/main\n", "{name}");
            assert_eq!(
                std::fs::read_to_string(dir.join("a.txt")).unwrap(),
                "work in progress\n"
            );
        }
    }

    #[test]
    fn checkout_create_rename_and_delete_change_branches_in_real_git() {
        let tmp = kika_git::fake::TempDir::new("branch-ops");
        let dir = tmp.path();
        kika_git::fake::git(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        kika_git::fake::git(dir, &["add", "-A"]);
        kika_git::fake::git(dir, &["commit", "-q", "-m", "init"]);
        kika_git::fake::git(dir, &["branch", "side"]);
        let runner = kika_git::CliRunner::isolated(dir);
        let head = || {
            String::from_utf8(kika_git::fake::git(
                dir,
                &["symbolic-ref", "--short", "HEAD"],
            ))
            .unwrap()
        };
        let branches = || {
            String::from_utf8(kika_git::fake::git(
                dir,
                &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
            ))
            .unwrap()
        };

        run_op(&runner, &GitOp::Checkout("side".into())).unwrap();
        assert_eq!(head(), "side\n");
        let create = GitOp::CreateBranch {
            name: "made".into(),
            from: "main".into(),
        };
        run_op(&runner, &create).unwrap();
        assert_eq!(head(), "made\n");
        let rename = GitOp::RenameBranch {
            from: "side".into(),
            to: "renamed".into(),
        };
        run_op(&runner, &rename).unwrap();
        let delete = GitOp::DeleteBranch {
            name: "renamed".into(),
            force: false,
        };
        run_op(&runner, &delete).unwrap();
        assert_eq!(branches(), "made\nmain\n");
    }

    fn origin_main() -> RemoteBranch {
        RemoteBranch {
            remote: "origin".into(),
            short: b"main".to_vec(),
            refname: b"refs/remotes/origin/main".to_vec(),
        }
    }

    #[test]
    fn remote_and_upstream_ops_build_exact_argv_and_never_fetch() {
        let cases: [(GitOp, &[&str]); 7] = [
            (
                GitOp::CheckoutTracking {
                    name: "main".into(),
                    from: origin_main(),
                },
                &[
                    "checkout",
                    "-q",
                    "--track",
                    "-b",
                    "main",
                    "refs/remotes/origin/main",
                    "--",
                ],
            ),
            (
                GitOp::SetUpstream {
                    branch: "topic".into(),
                    upstream: origin_main(),
                },
                &[
                    "branch",
                    "--set-upstream-to=refs/remotes/origin/main",
                    "--",
                    "topic",
                ],
            ),
            (
                GitOp::UnsetUpstream("topic".into()),
                &["branch", "--unset-upstream", "--", "topic"],
            ),
            (
                GitOp::AddRemote {
                    name: "fork".into(),
                    url: "https://h/fork.git".into(),
                },
                &["remote", "add", "--", "fork", "https://h/fork.git"],
            ),
            // No `--`: git 2.15's `remote rename` and `remote remove` parse
            // no options and would take it for a name.
            (
                GitOp::RenameRemote {
                    from: "fork".into(),
                    to: "mine".into(),
                },
                &["remote", "rename", "fork", "mine"],
            ),
            (
                GitOp::RemoveRemote("fork".into()),
                &["remote", "remove", "fork"],
            ),
            (
                GitOp::RemoveRemote("my.fork/x".into()),
                &["remote", "remove", "my.fork/x"],
            ),
        ];
        for (op, expected) in cases {
            let cmd = op_command(&op);
            let got = argv(&cmd);
            assert_eq!(got, expected, "{op:?}");
            for banned in ["-f", "--fetch", "fetch", "--end-of-options"] {
                assert!(!got.iter().any(|a| a == banned), "{op:?}: {banned}");
            }
            let runner = FakeRunner::new().expect(expected, ok(b""));
            assert_eq!(run_op(&runner, &op), Ok(()), "{op:?}");
            runner.assert_done();
        }
    }

    #[test]
    fn a_tracking_checkout_then_set_and_unset_upstream_in_real_git() {
        let (tmp, _bare) = cloned_repo();
        let dir = tmp.path();
        let runner = kika_git::CliRunner::isolated(dir);
        let feature = RemoteBranch {
            remote: "origin".into(),
            short: b"feature".to_vec(),
            refname: b"refs/remotes/origin/feature".to_vec(),
        };

        let checkout = GitOp::CheckoutTracking {
            name: "feature".into(),
            from: feature.clone(),
        };
        run_op(&runner, &checkout).unwrap();
        let head = kika_git::fake::git(dir, &["symbolic-ref", "--short", "HEAD"]);
        assert_eq!(head, b"feature\n");
        assert_eq!(
            config(dir, "branch.feature.merge").as_deref(),
            Some("refs/heads/feature")
        );
        assert_eq!(
            config(dir, "branch.feature.remote").as_deref(),
            Some("origin")
        );

        assert_eq!(config(dir, "branch.main.merge"), None);
        let set = GitOp::SetUpstream {
            branch: "main".into(),
            upstream: feature,
        };
        run_op(&runner, &set).unwrap();
        assert_eq!(
            config(dir, "branch.main.merge").as_deref(),
            Some("refs/heads/feature")
        );
        run_op(&runner, &GitOp::UnsetUpstream("main".into())).unwrap();
        assert_eq!(config(dir, "branch.main.merge"), None);
    }
}
