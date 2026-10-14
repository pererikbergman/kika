//! A cheap fingerprint of every ref and of `HEAD`, polled to notice commits,
//! checkouts and fetches made outside Kika without running `git status`.

use super::error::GitError;
use super::runner::{GitCommand, GitOutput, GitRunner};

/// Opaque: only ever compared with the previous one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefsSnapshot(Vec<u8>);

#[cfg(test)]
impl RefsSnapshot {
    /// A snapshot that equals only another made from the same bytes.
    pub fn of(bytes: &[u8]) -> Self {
        Self(bytes.to_vec())
    }
}

/// Every ref with its object, where `HEAD` points, and what it resolves to.
/// A detached or unborn `HEAD` makes the last two fail; that is part of the
/// snapshot, not an error.
pub fn read_refs(runner: &dyn GitRunner) -> Result<RefsSnapshot, String> {
    let refs_cmd = GitCommand::new("for-each-ref").arg("--format=%(objectname) %(refname)");
    let refs = run(runner, &refs_cmd)?;
    if !refs.success {
        return Err(GitError::failed(&refs_cmd, &refs).message_or("git for-each-ref failed"));
    }
    let symbolic = run(
        runner,
        &GitCommand::new("symbolic-ref").arg("-q").arg("HEAD"),
    )?;
    let head = run(
        runner,
        &GitCommand::new("rev-parse")
            .arg("-q")
            .arg("--verify")
            .arg("HEAD"),
    )?;
    let mut bytes = Vec::new();
    for output in [refs, symbolic, head] {
        bytes.push(u8::from(output.success));
        bytes.extend((output.stdout.len() as u64).to_le_bytes());
        bytes.extend(output.stdout);
    }
    Ok(RefsSnapshot(bytes))
}

/// Run a read-only query that must not take the index lock from the user.
/// Ref reads name no path, so the whole standard profile is a no-op past
/// the lock and the locale. A failed exit is part of the snapshot.
fn run(runner: &dyn GitRunner, cmd: &GitCommand) -> Result<GitOutput, String> {
    runner
        .run(&cmd.clone().standard_env())
        .map_err(|e| GitError::io(&e).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::fake::{FakeRunner, fail, ok};
    use crate::git::fake::{STANDARD_ENV, env_list};

    const REFS: &[&str] = &["for-each-ref", "--format=%(objectname) %(refname)"];
    const SYMBOLIC: &[&str] = &["symbolic-ref", "-q", "HEAD"];
    const HEAD: &[&str] = &["rev-parse", "-q", "--verify", "HEAD"];

    fn runner(refs: &[u8], symbolic: &[u8], head: &[u8]) -> FakeRunner {
        FakeRunner::new()
            .expect(REFS, ok(refs))
            .expect(SYMBOLIC, ok(symbolic))
            .expect(HEAD, ok(head))
    }

    #[test]
    fn three_lock_free_queries_make_the_snapshot() {
        let fake = runner(b"1111 refs/heads/main\n", b"refs/heads/main\n", b"1111\n");

        assert!(read_refs(&fake).is_ok());

        fake.assert_done();
        assert_eq!(fake.process_envs(), vec![env_list(&STANDARD_ENV); 3]);
    }

    #[test]
    fn the_same_refs_give_equal_snapshots_and_a_moved_ref_a_different_one() {
        let main = read_refs(&runner(
            b"1 refs/heads/main\n",
            b"refs/heads/main\n",
            b"1\n",
        ));
        let again = read_refs(&runner(
            b"1 refs/heads/main\n",
            b"refs/heads/main\n",
            b"1\n",
        ));
        let moved = read_refs(&runner(
            b"2 refs/heads/main\n",
            b"refs/heads/main\n",
            b"2\n",
        ));
        assert_eq!(main, again);
        assert_ne!(main, moved);
    }

    #[test]
    fn an_unborn_head_is_a_snapshot_not_an_error() {
        let fake = FakeRunner::new()
            .expect(REFS, ok(b""))
            .expect(SYMBOLIC, ok(b"refs/heads/main\n"))
            .expect(HEAD, fail(""));
        assert!(read_refs(&fake).is_ok());
        fake.assert_done();
    }

    #[test]
    fn a_detached_head_differs_from_the_branch_at_the_same_commit() {
        let on_branch = read_refs(&runner(
            b"1 refs/heads/main\n",
            b"refs/heads/main\n",
            b"1\n",
        ));
        let detached = read_refs(
            &FakeRunner::new()
                .expect(REFS, ok(b"1 refs/heads/main\n"))
                .expect(SYMBOLIC, fail(""))
                .expect(HEAD, ok(b"1\n")),
        );
        assert!(detached.is_ok());
        assert_ne!(on_branch, detached);
    }

    #[test]
    fn a_failing_for_each_ref_is_an_error_made_safe() {
        let fake = FakeRunner::new().expect(REFS, fail("fatal: not a \x1b[2Jrepo\n"));
        assert_eq!(read_refs(&fake), Err("fatal: not a \u{FFFD}[2Jrepo".into()));
        fake.assert_done();
    }

    #[test]
    fn a_real_commit_changes_the_snapshot() {
        use crate::git::fake::{TempDir, git};

        let tmp = TempDir::new("refs");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        let runner = crate::git::CliRunner::isolated(dir);
        let unborn = read_refs(&runner).unwrap();
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "one"]);
        let first = read_refs(&runner).unwrap();
        assert_ne!(unborn, first);
        assert_eq!(first, read_refs(&runner).unwrap());
    }
}
