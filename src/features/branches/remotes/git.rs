//! The argv for checking out a remote branch as a tracking branch and for
//! adding, renaming and removing a remote, which `op_command` in `op.rs`
//! builds through these four functions.

use kika_git::remotes::{RemoteBranch, RemoteName};
use kika_git::runner::GitCommand;

/// `git checkout --track -b` for `name` at the remote branch `from`.
///
/// The start point as a full ref, as for `CreateBranch`.
pub(crate) fn track_command(name: &str, from: &RemoteBranch) -> GitCommand {
    GitCommand::new("checkout")
        .arg("-q")
        .arg("--track")
        .arg("-b")
        .arg(name)
        .arg(from.full_ref())
        .arg("--")
}

/// `git remote add` for `name` at `url`.
///
/// Never `-f`: adding a remote must not reach the network.
pub(crate) fn add_command(name: &str, url: &str) -> GitCommand {
    GitCommand::new("remote")
        .arg("add")
        .arg("--")
        .arg(name)
        .arg(url)
}

/// `git remote rename` from `from` to `to`.
///
/// In git 2.15 `remote rename` and `remote remove` parse no options,
/// so `--` would be a usage error. The new name passed
/// `validate_remote_name`, and no key acts on an old name that
/// starts with `-`, so neither is read as an option.
pub(crate) fn rename_command(from: &RemoteName, to: &str) -> GitCommand {
    GitCommand::new("remote")
        .arg("rename")
        .arg(from.as_os_string())
        .arg(to)
}

/// `git remote remove` for `name`, with no `--`, as for `rename_command`.
pub(crate) fn remove_command(name: &RemoteName) -> GitCommand {
    GitCommand::new("remote")
        .arg("remove")
        .arg(name.as_os_string())
}

#[cfg(test)]
mod tests {
    use crate::op::{GitOp, run_op};
    use crate::test_support::cloned_repo;
    use kika_git::fake::{FakeRunner, config, fail};

    #[test]
    fn a_failed_remote_op_says_why_without_credentials() {
        let runner = FakeRunner::new().expect(
            &["remote", "add", "--", "o", "https://u:tok@h/r"],
            fail("error: remote o already exists at 'https://u:tok@h/r'\n"),
        );
        let op = GitOp::AddRemote {
            name: "o".into(),
            url: "https://u:tok@h/r".into(),
        };
        assert_eq!(
            run_op(&runner, &op),
            Err("error: remote o already exists at 'https://h/r'".into())
        );
    }

    fn remote_refs(dir: &std::path::Path) -> String {
        String::from_utf8(kika_git::fake::git(
            dir,
            &["for-each-ref", "--format=%(refname)", "refs/remotes"],
        ))
        .unwrap()
    }

    #[test]
    fn remotes_are_added_renamed_and_removed_without_a_fetch_in_real_git() {
        let (tmp, bare) = cloned_repo();
        let dir = tmp.path();
        let runner = kika_git::CliRunner::isolated(dir);
        let url = bare.path().to_str().unwrap().to_string();

        let add = GitOp::AddRemote {
            name: "fork".into(),
            url: url.clone(),
        };
        run_op(&runner, &add).unwrap();
        assert_eq!(config(dir, "remote.fork.url"), Some(url));
        assert!(!remote_refs(dir).contains("refs/remotes/fork/"), "fetched");

        // Its refs move with it once there are some.
        kika_git::fake::git(dir, &["fetch", "-q", "fork"]);
        let rename = GitOp::RenameRemote {
            from: "fork".into(),
            to: "mine".into(),
        };
        run_op(&runner, &rename).unwrap();
        assert_eq!(config(dir, "remote.fork.url"), None);
        let refs = remote_refs(dir);
        assert!(refs.contains("refs/remotes/mine/feature"), "{refs}");
        assert!(!refs.contains("refs/remotes/fork/"), "{refs}");

        run_op(&runner, &GitOp::RemoveRemote("mine".into())).unwrap();
        assert_eq!(config(dir, "remote.mine.url"), None);
        assert!(!remote_refs(dir).contains("refs/remotes/mine/"));
    }
}
