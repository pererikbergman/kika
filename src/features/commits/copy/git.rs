//! The order a paste picks its commits in: parents before children,
//! whatever order they were copied in. Git picks the commits named on
//! `cherry-pick`'s command line one by one in argv order, so this order is
//! the argv.

use crate::git::GitRunner;
use crate::git::commit_files::CommitId;
use crate::git::diff::terminal_safe;
use crate::git::runner::GitCommand;

/// `picks` in the order `stdout`, a `rev-list --topo-order --reverse`
/// listing, has them. Lines that are not picks are dropped. A pick the
/// listing lacks is reachable from `HEAD`, so it is already on the
/// current branch, and the paste is refused rather than picked again.
pub fn pick_order(stdout: &[u8], picks: &[CommitId]) -> Result<Vec<CommitId>, String> {
    let ordered: Vec<CommitId> = stdout
        .split(|&b| b == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
        .filter_map(|line| picks.iter().find(|pick| pick.as_str().as_bytes() == line))
        .cloned()
        .collect();
    match picks.iter().find(|pick| !ordered.contains(pick)) {
        Some(missing) => Err(format!(
            "{} is already on the current branch",
            short(missing)
        )),
        None => Ok(ordered),
    }
}

/// Ask git for `picks` parents first, then put them in that order. The
/// walk starts at the picks and stops at `HEAD`'s history, so it is as
/// long as the branches have diverged, not as long as the history.
/// Blocking; run it from a task or a hand-off.
pub fn ordered(runner: &dyn GitRunner, picks: &[CommitId]) -> Result<Vec<CommitId>, String> {
    if picks.is_empty() {
        return Err("Nothing copied".into());
    }
    // Only full ids reach argv: one starts with a hex digit, so it is never
    // an option, and git takes it as an id before any ref of that name.
    if let Some(bad) = picks.iter().find(|pick| !is_full_id(pick)) {
        return Err(format!("{} is not a full commit id", short(bad)));
    }
    let cmd = picks
        .iter()
        .fold(
            GitCommand::new("rev-list")
                .arg("--topo-order")
                .arg("--reverse"),
            |cmd, pick| cmd.arg(pick.as_str()),
        )
        .arg("--not")
        .arg("HEAD")
        .arg("--")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C");
    let output = runner.run(&cmd).map_err(|e| e.to_string())?;
    if !output.success {
        let message = terminal_safe(&output.stderr);
        return Err(if message.is_empty() {
            "git rev-list failed".into()
        } else {
            message
        });
    }
    pick_order(&output.stdout, picks)
}

/// A SHA-1 or SHA-256 id written out in full.
pub fn is_full_id(id: &CommitId) -> bool {
    matches!(id.as_str().len(), 40 | 64)
}

/// The first seven hex digits, as named in messages. Made from the id, so
/// it is hex whatever the log printed.
pub fn short(id: &CommitId) -> &str {
    let text = id.as_str();
    &text[..text.len().min(7)]
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::git::CliRunner;
    use crate::git::fake::{FakeRunner, TempDir, git, ok, try_git};
    use crate::git::repo_state::Operation;
    use crate::git::status::read_status;
    use crate::op::{GitOp, op_command, run_op};
    use crate::test_support::{argv, cid};

    fn id(hex: char) -> CommitId {
        CommitId::parse(&hex.to_string().repeat(40)).unwrap()
    }

    fn listing(ids: &[char]) -> Vec<u8> {
        ids.iter()
            .map(|&hex| format!("{}\n", hex.to_string().repeat(40)))
            .collect::<String>()
            .into_bytes()
    }

    #[test]
    fn picks_come_out_in_rev_list_order_whatever_their_input_order() {
        let stdout = listing(&['a', 'b', 'c']);
        for picks in [
            [id('c'), id('a'), id('b')],
            [id('b'), id('c'), id('a')],
            [id('a'), id('b'), id('c')],
        ] {
            assert_eq!(
                pick_order(&stdout, &picks),
                Ok(vec![id('a'), id('b'), id('c')])
            );
        }
    }

    #[test]
    fn a_pick_missing_from_the_listing_is_already_on_the_branch() {
        let stdout = listing(&['a', 'c']);
        assert_eq!(
            pick_order(&stdout, &[id('c'), id('b'), id('a')]),
            Err("bbbbbbb is already on the current branch".into())
        );
    }

    #[test]
    fn unrelated_lines_and_a_trailing_cr_are_ignored() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let stdout = format!("{}\n{a}\r\n\nnot an id\n{b}", "d".repeat(40));
        assert_eq!(
            pick_order(stdout.as_bytes(), &[id('b'), id('a')]),
            Ok(vec![id('a'), id('b')])
        );
    }

    #[test]
    fn ordered_asks_rev_list_with_full_ids_only_and_no_option_before_them() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let runner = FakeRunner::new().expect(
            &[
                "rev-list",
                "--topo-order",
                "--reverse",
                &b,
                &a,
                "--not",
                "HEAD",
                "--",
            ],
            ok(listing(&['a', 'b']).as_slice()),
        );
        assert_eq!(
            ordered(&runner, &[id('b'), id('a')]),
            Ok(vec![id('a'), id('b')])
        );
        runner.assert_done();
        let envs = runner.envs();
        let env: Vec<_> = envs[0]
            .iter()
            .map(|(k, v)| (k.to_str().unwrap(), v.to_str().unwrap()))
            .collect();
        assert_eq!(env, [("GIT_OPTIONAL_LOCKS", "0"), ("LC_ALL", "C")]);

        // A short id never reaches git, and nor does an empty set.
        let none = FakeRunner::new();
        let short = CommitId::parse("abc1234").unwrap();
        assert_eq!(
            ordered(&none, &[id('a'), short]),
            Err("abc1234 is not a full commit id".into())
        );
        assert_eq!(ordered(&none, &[]), Err("Nothing copied".into()));
        none.assert_done();
    }

    #[test]
    fn cherry_pick_names_each_commit_in_order_and_sets_no_env() {
        let (one, two) = ("1".repeat(40), "2".repeat(40));
        let cmd = op_command(&GitOp::CherryPick(vec![cid(&one), cid(&two)]));
        assert_eq!(argv(&cmd), ["cherry-pick", one.as_str(), two.as_str()]);
        assert!(cmd.envs().is_empty(), "{:?}", cmd.envs());
        assert_eq!(cmd.input(), None);
    }

    /// An empty repository with an identity, no signing and branch `main`.
    fn repo() -> TempDir {
        let tmp = TempDir::new("cherry-pick");
        let dir = tmp.path();
        git(dir, &["init", "-q", "--template="]);
        for (key, value) in [
            ("user.name", "Kika Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
        ] {
            git(dir, &["config", key, value]);
        }
        git(dir, &["checkout", "-q", "-b", "main"]);
        tmp
    }

    fn commit(dir: &Path, file: &str, text: &str, subject: &str, date: &str) -> CommitId {
        std::fs::write(dir.join(file), text).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "--date", date, "-m", subject]);
        let out = git(dir, &["rev-parse", "HEAD"]);
        CommitId::parse(String::from_utf8(out).unwrap().trim()).unwrap()
    }

    fn subjects(dir: &Path, n: usize) -> String {
        let out = git(dir, &["log", "--format=%s", &format!("-{n}"), "--reverse"]);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn three_picks_land_parents_first_and_none_already_on_head_in_real_git() {
        let tmp = repo();
        let dir = tmp.path();
        let early = "2020-01-01T00:00:00Z";
        commit(dir, "base.txt", "base\n", "base", early);
        git(dir, &["checkout", "-q", "-b", "topic"]);
        // A's author date is the newest, so an order by date would pick
        // it last.
        let a = commit(dir, "a.txt", "a\n", "A", "2030-01-01T00:00:00Z");
        let b = commit(dir, "b.txt", "b\n", "B", "2021-01-01T00:00:00Z");
        let c = commit(dir, "c.txt", "c\n", "C", "2022-01-01T00:00:00Z");
        git(dir, &["checkout", "-q", "main"]);
        commit(dir, "main.txt", "main\n", "on main", early);

        let runner = CliRunner::isolated(dir);
        let picks = ordered(&runner, &[c.clone(), a.clone(), b.clone()]).unwrap();
        assert_eq!(picks, [a.clone(), b.clone(), c.clone()]);

        run_op(&runner, &GitOp::CherryPick(picks)).unwrap();
        assert_eq!(subjects(dir, 3), "A\nB\nC\n");
        assert_eq!(git(dir, &["status", "--porcelain"]), b"");

        // The picks made new commits, so the copied ids are still not on
        // `main`; a second paste there stops on an empty pick instead.
        let again = [c.clone(), a, b];
        assert!(ordered(&runner, &again).is_ok());
        // Where the copied commits themselves are, nothing is picked.
        git(dir, &["checkout", "-q", "topic"]);
        assert_eq!(
            ordered(&runner, &again),
            Err(format!(
                "{} is already on the current branch",
                &c.as_str()[..7]
            ))
        );
    }

    #[test]
    fn a_conflicting_pick_stops_with_a_cherry_pick_in_progress_in_real_git() {
        let tmp = repo();
        let dir = tmp.path();
        let date = "2020-01-01T00:00:00Z";
        commit(dir, "a.txt", "base\n", "base", date);
        git(dir, &["checkout", "-q", "-b", "topic"]);
        let one = commit(dir, "a.txt", "topic one\n", "one", date);
        let two = commit(dir, "a.txt", "topic two\n", "two", date);
        git(dir, &["checkout", "-q", "main"]);
        commit(dir, "a.txt", "main\n", "on main", date);

        let runner = CliRunner::isolated(dir);
        let picks = ordered(&runner, &[two, one]).unwrap();
        let error = run_op(&runner, &GitOp::CherryPick(picks)).unwrap_err();
        assert!(error.contains("a.txt"), "{error}");
        assert!(try_git(
            dir,
            &["rev-parse", "-q", "--verify", "CHERRY_PICK_HEAD"]
        ));
        let status = read_status(&runner).unwrap();
        assert_eq!(status.branch.in_progress, [Operation::CherryPick]);
    }
}
