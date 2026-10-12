//! The argv for making and deleting a tag, which `op_command` in `op.rs`
//! builds through these two functions.

use crate::git::commit_files::CommitId;
use crate::git::runner::GitCommand;
use crate::git::tags::TagName;

/// `git tag` for `name` at `target`, annotated when there is a message.
///
/// The name passed `validate_tag_name` (see `refusal`), so it does
/// not start with `-`; the message is `-m`'s own argument, read as
/// its value whatever it starts with; the full id is hex digits.
/// So no `--end-of-options`, which git 2.15 lacks.
pub(crate) fn create_command(name: &str, target: &CommitId, message: Option<&str>) -> GitCommand {
    let cmd = GitCommand::new("tag");
    let cmd = match message {
        Some(message) => cmd.arg("-a").arg("-m").arg(message),
        None => cmd,
    };
    cmd.arg(name).arg(target.as_str())
}

/// `git update-ref -d` for tag `name`, only while it still holds `oid`.
///
/// The full ref, so a hand-made name like an option stays a name,
/// and the old value, so a tag moved since it was listed is kept.
/// `--no-deref`, so a symbolic ref under `refs/tags` that points at
/// a branch loses itself, never the branch.
pub(crate) fn delete_command(name: &TagName, oid: &str) -> GitCommand {
    GitCommand::new("update-ref")
        .arg("--no-deref")
        .arg("-d")
        .arg(name.full_ref())
        .arg(oid)
}

#[cfg(test)]
mod tests {
    use crate::git::commit_files::CommitId;
    use crate::git::fake::{FakeRunner, ok};
    use crate::op::{GitOp, op_command, refusal, run_op};
    use crate::test_support::argv;

    const OID: &str = "0123456789abcdef0123456789abcdef01234567";

    fn create_tag(name: &str, message: Option<&str>) -> GitOp {
        GitOp::CreateTag {
            name: name.into(),
            target: CommitId::parse(OID).unwrap(),
            message: message.map(Into::into),
        }
    }

    #[test]
    fn tag_ops_build_exact_argv() {
        let cases: [(GitOp, &[&str]); 4] = [
            (create_tag("v1", None), &["tag", "v1", OID]),
            (
                create_tag("v1", Some("-msg starting with dash")),
                &["tag", "-a", "-m", "-msg starting with dash", "v1", OID],
            ),
            (
                GitOp::DeleteTag {
                    name: "v1".into(),
                    oid: OID.into(),
                },
                &["update-ref", "--no-deref", "-d", "refs/tags/v1", OID],
            ),
            (
                GitOp::DeleteTag {
                    name: "-x".into(),
                    oid: OID.into(),
                },
                &["update-ref", "--no-deref", "-d", "refs/tags/-x", OID],
            ),
        ];
        for (op, expected) in cases {
            assert_eq!(argv(&op_command(&op)), expected, "{op:?}");
            let runner = FakeRunner::new().expect(expected, ok(b""));
            assert_eq!(run_op(&runner, &op), Ok(()), "{op:?}");
            runner.assert_done();
        }
    }

    #[test]
    fn a_tag_with_a_bad_name_is_refused_without_running_git() {
        for name in ["-f", "", "a..b", "HEAD", "a b"] {
            let runner = FakeRunner::new();
            let op = create_tag(name, Some("m"));
            assert!(run_op(&runner, &op).is_err(), "{name:?}");
            runner.assert_done();
        }
        assert_eq!(refusal(&create_tag("v1", None)), None);
    }

    #[test]
    fn tags_are_made_and_deleted_only_while_unmoved_in_real_git() {
        use crate::git::fake::{TempDir, git, try_git};
        let tmp = TempDir::new("tag-ops");
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        // An annotated tag records a tagger, and the runner below sees
        // only the repository's config.
        git(dir, &["config", "user.name", "Tagger"]);
        git(dir, &["config", "user.email", "tagger@example.com"]);
        git(dir, &["commit", "-q", "--allow-empty", "-m", "one"]);
        let head = String::from_utf8(git(dir, &["rev-parse", "HEAD"])).unwrap();
        let head = CommitId::parse(head.trim()).unwrap();
        let runner = crate::git::CliRunner::isolated(dir);
        let made = |name: &str, message: Option<&str>| GitOp::CreateTag {
            name: name.into(),
            target: head.clone(),
            message: message.map(Into::into),
        };
        let kind = |name: &str| git(dir, &["cat-file", "-t", &format!("refs/tags/{name}")]);

        run_op(&runner, &made("light", None)).unwrap();
        run_op(&runner, &made("heavy", Some("-m looks like an option"))).unwrap();
        assert_eq!(kind("light"), b"commit\n");
        assert_eq!(kind("heavy"), b"tag\n");
        let body = git(dir, &["tag", "-l", "--format=%(contents:subject)", "heavy"]);
        assert_eq!(body, b"-m looks like an option\n");
        let again = run_op(&runner, &made("light", None)).unwrap_err();
        assert!(again.contains("already exists"), "{again}");

        // A tag that moved since it was listed is not deleted.
        let stale = GitOp::DeleteTag {
            name: "heavy".into(),
            oid: head.as_str().into(),
        };
        assert!(run_op(&runner, &stale).is_err());
        assert!(try_git(
            dir,
            &["rev-parse", "--verify", "-q", "refs/tags/heavy"]
        ));
        let delete = GitOp::DeleteTag {
            name: "light".into(),
            oid: head.as_str().into(),
        };
        run_op(&runner, &delete).unwrap();
        assert!(!try_git(
            dir,
            &["rev-parse", "--verify", "-q", "refs/tags/light"]
        ));
    }

    #[test]
    fn deleting_a_symbolic_tag_removes_only_the_tag_never_its_branch() {
        use crate::git::fake::{TempDir, git, try_git};
        let tmp = TempDir::new("tag-symref");
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["commit", "-q", "--allow-empty", "-m", "one"]);
        git(dir, &["branch", "keep"]);
        git(dir, &["symbolic-ref", "refs/tags/evil", "refs/heads/keep"]);
        let head = String::from_utf8(git(dir, &["rev-parse", "HEAD"])).unwrap();
        let runner = crate::git::CliRunner::isolated(dir);

        let delete = GitOp::DeleteTag {
            name: "evil".into(),
            oid: head.trim().into(),
        };
        run_op(&runner, &delete).unwrap();

        assert!(try_git(
            dir,
            &["rev-parse", "--verify", "-q", "refs/heads/keep"]
        ));
        let left = git(dir, &["for-each-ref", "--format=%(refname)", "refs/tags"]);
        assert!(left.is_empty(), "{}", String::from_utf8_lossy(&left));
    }
}
