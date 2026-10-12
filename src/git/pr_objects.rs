//! A pull request's commits in the local repository: whether they are
//! here, fetching the missing ones from the forge's remote by id, and the
//! changed files between the merge base and the head. Ids reach argv only
//! as [`CommitId`]s, hex digits, so nothing in it reads as an option; no
//! ref name from the server ever does, and no ref is written.

use super::commit_files::{CommitFile, CommitId, CommitRange, commit_files, numstat};
use super::diff::terminal_safe;
use super::remote::{AskpassEnv, askpass_for, safe, with_prompts};
use super::remotes::RemoteName;
use super::runner::{GitCommand, GitRunner};

/// What a changed file's line counts are known to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileCounts {
    Lines {
        added: u32,
        removed: u32,
    },
    Binary,
    /// Git listed the file but gave no count for it.
    Unknown,
}

/// Whether a pull request's commits are here, and where the missing ones
/// may be fetched from: `None` when no remote names the reviewed
/// repository on a configured host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitsHere {
    All,
    Missing {
        oids: Vec<CommitId>,
        remote: Option<RemoteName>,
    },
}

/// One file a pull request changes, as local git sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrFileRow {
    pub file: CommitFile,
    pub counts: FileCounts,
}

/// A read-only query: no lock, git's text in English, and in a partial
/// clone no object fetched behind the user's back (`GIT_NO_LAZY_FETCH`,
/// which git before 2.44 ignores).
fn query(cmd: GitCommand) -> GitCommand {
    cmd.env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .env("GIT_NO_LAZY_FETCH", "1")
}

/// `git cat-file -e <oid>^{commit}`: succeeds only when the commit is here.
pub fn has_commit_command(oid: &CommitId) -> GitCommand {
    query(
        GitCommand::new("cat-file")
            .arg("-e")
            .arg(format!("{}^{{commit}}", oid.as_str())),
    )
}

/// `git merge-base <base> <head>`: where the head left the base, which a
/// pull request's changes are counted from.
pub fn merge_base_command(base: &CommitId, head: &CommitId) -> GitCommand {
    query(
        GitCommand::new("merge-base")
            .arg(base.as_str())
            .arg(head.as_str()),
    )
}

/// `git fetch` of `oids` alone from `remote`. With no destination, no ref
/// moves; `fetch.writeFetchHEAD=false` keeps `FETCH_HEAD` as it is on git
/// 2.29 and later, and older git ignores it (`--no-write-fetch-head`
/// would fail there). No tags and no submodules: only the forge's host is
/// reached.
pub fn fetch_commits_command(remote: &RemoteName, oids: &[CommitId]) -> GitCommand {
    let cmd = GitCommand::new("-c")
        .arg("fetch.writeFetchHEAD=false")
        .arg("fetch")
        .arg("--quiet")
        .arg("--no-tags")
        .arg("--no-recurse-submodules")
        .arg(remote.as_os_string());
    oids.iter()
        .fold(cmd, |cmd, oid| cmd.arg(oid.as_str()))
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
}

/// Which of `oids` are not commits here, in order. A failure to run git at
/// all is an error; a missing or broken object is only missing.
pub fn missing_commits(
    runner: &dyn GitRunner,
    oids: &[&CommitId],
) -> Result<Vec<CommitId>, String> {
    let mut missing = Vec::new();
    for oid in oids {
        let output = runner
            .run(&has_commit_command(oid))
            .map_err(|e| terminal_safe(e.to_string().as_bytes()))?;
        if !output.success && !missing.contains(*oid) {
            missing.push((*oid).clone());
        }
    }
    Ok(missing)
}

/// Fetch `oids` from `remote`, asking through `askpass` like a fetch the
/// user started. A remote named like an option is refused, as for a tag
/// push. Errors are safe to draw and hold no credentials.
pub fn fetch_commits(
    runner: &dyn GitRunner,
    remote: &RemoteName,
    oids: &[CommitId],
    askpass: Option<&AskpassEnv>,
) -> Result<(), String> {
    if remote.0.is_empty() || remote.0.starts_with(b"-") || oids.is_empty() {
        return Err("Fetch from a remote named like an option with git directly".into());
    }
    let askpass = askpass_for(runner, askpass, &|key| std::env::var_os(key).is_some());
    let cmd = with_prompts(fetch_commits_command(remote, oids), askpass.as_ref());
    let output = runner
        .run(&cmd)
        .map_err(|e| safe(e.to_string().as_bytes()))?;
    if output.success {
        return Ok(());
    }
    let message = safe(&output.stderr);
    Err(if message.is_empty() {
        "git fetch failed".into()
    } else {
        message
    })
}

/// The merge base of `base` and `head`.
pub fn merge_base(
    runner: &dyn GitRunner,
    base: &CommitId,
    head: &CommitId,
) -> Result<CommitId, String> {
    let output = runner
        .run(&merge_base_command(base, head))
        .map_err(|e| terminal_safe(e.to_string().as_bytes()))?;
    if !output.success {
        let message = terminal_safe(&output.stderr);
        return Err(if message.is_empty() {
            "The pull request's head and base share no history".into()
        } else {
            message
        });
    }
    let text = String::from_utf8_lossy(&output.stdout);
    CommitId::parse(text.trim())
        .ok_or_else(|| "git merge-base gave an answer Kika could not read".into())
}

/// The range a pull request is reviewed as, merge base to head, and the
/// files it changes with their line counts.
pub fn pr_files(
    runner: &dyn GitRunner,
    base: &CommitId,
    head: &CommitId,
) -> Result<(CommitRange, Vec<PrFileRow>), String> {
    let range = CommitRange {
        oid: head.clone(),
        parent: Some(merge_base(runner, base, head)?),
    };
    let files = commit_files(runner, &range)?;
    let stats = numstat(runner, &range)?;
    let rows = files
        .into_iter()
        .map(|file| {
            let counts = match stats.iter().find(|(path, _)| *path == file.path) {
                Some((_, Some((added, removed)))) => FileCounts::Lines {
                    added: *added,
                    removed: *removed,
                },
                Some((_, None)) => FileCounts::Binary,
                None => FileCounts::Unknown,
            };
            PrFileRow { file, counts }
        })
        .collect();
    Ok((range, rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::fake::{FakeRunner, fail, ok};

    fn id(hex: &str) -> CommitId {
        CommitId::parse(hex).unwrap()
    }

    fn args(cmd: &GitCommand) -> Vec<String> {
        cmd.args()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn has_env(cmd: &GitCommand, key: &str, value: &str) -> bool {
        cmd.envs().iter().any(|(k, v)| k == key && v == value)
    }

    #[test]
    fn each_command_has_a_fixed_argv_with_only_hex_ids() {
        assert_eq!(
            args(&has_commit_command(&id("abc123"))),
            ["cat-file", "-e", "abc123^{commit}"]
        );
        assert_eq!(
            args(&merge_base_command(&id("b0"), &id("a1"))),
            ["merge-base", "b0", "a1"]
        );
        assert_eq!(
            args(&fetch_commits_command(
                &RemoteName::from("origin"),
                &[id("a1"), id("b0")]
            )),
            [
                "-c",
                "fetch.writeFetchHEAD=false",
                "fetch",
                "--quiet",
                "--no-tags",
                "--no-recurse-submodules",
                "origin",
                "a1",
                "b0",
            ]
        );
        for cmd in [
            has_commit_command(&id("a1")),
            merge_base_command(&id("b0"), &id("a1")),
            fetch_commits_command(&RemoteName::from("origin"), &[id("a1")]),
        ] {
            assert!(has_env(&cmd, "GIT_OPTIONAL_LOCKS", "0"), "{cmd:?}");
            assert!(has_env(&cmd, "LC_ALL", "C"), "{cmd:?}");
        }
        assert_eq!(
            fetch_commits_command(&RemoteName::from("o"), &[]).subcommand(),
            "fetch"
        );
    }

    #[test]
    fn a_missing_commit_is_listed_once_and_a_present_one_not() {
        let fake = FakeRunner::new()
            .expect(&["cat-file", "-e", "a1^{commit}"], fail(""))
            .expect(&["cat-file", "-e", "b0^{commit}"], ok(b""))
            .expect(&["cat-file", "-e", "a1^{commit}"], fail("fatal: bad"));
        assert_eq!(
            missing_commits(&fake, &[&id("a1"), &id("b0"), &id("a1")]),
            Ok(vec![id("a1")])
        );
        fake.assert_done();
    }

    #[test]
    fn the_fetch_asks_through_the_bridge_and_names_only_the_remote_and_ids() {
        let fake = FakeRunner::new()
            .expect(&["config", "--get", "core.askPass"], fail(""))
            .expect(
                &[
                    "-c",
                    "fetch.writeFetchHEAD=false",
                    "fetch",
                    "--quiet",
                    "--no-tags",
                    "--no-recurse-submodules",
                    "origin",
                    "a1",
                ],
                ok(b""),
            );
        let bridge = AskpassEnv {
            program: "/bin/kika".into(),
            socket: "/tmp/s".into(),
            git_askpass: true,
        };
        assert_eq!(
            fetch_commits(
                &fake,
                &RemoteName::from("origin"),
                &[id("a1")],
                Some(&bridge)
            ),
            Ok(())
        );
        let env = fake.envs().pop().unwrap();
        for key in ["GIT_TERMINAL_PROMPT", "SSH_ASKPASS", "SSH_ASKPASS_REQUIRE"] {
            assert!(env.iter().any(|(k, _)| k == key), "{key} missing: {env:?}");
        }
        fake.assert_done();
    }

    #[test]
    fn a_fetch_failure_hides_credentials_and_an_option_like_remote_runs_nothing() {
        let fake = FakeRunner::new().expect(
            &[
                "-c",
                "fetch.writeFetchHEAD=false",
                "fetch",
                "--quiet",
                "--no-tags",
                "--no-recurse-submodules",
                "origin",
                "a1",
            ],
            fail("fatal: https://user:tok@github.com/a/b \x1b[2J refused\n"),
        );
        let message =
            fetch_commits(&fake, &RemoteName::from("origin"), &[id("a1")], None).unwrap_err();
        assert!(!message.contains("tok"), "{message}");
        assert!(!message.contains('\x1b'), "{message}");

        let fake = FakeRunner::new();
        assert!(
            fetch_commits(
                &fake,
                &RemoteName::from("--upload-pack=x"),
                &[id("a1")],
                None
            )
            .is_err()
        );
        assert!(fetch_commits(&fake, &RemoteName::from("origin"), &[], None).is_err());
        fake.assert_done();
    }

    #[test]
    fn the_files_run_from_the_merge_base_with_their_counts() {
        let fake = FakeRunner::new()
            .expect(&["merge-base", "b0", "a1"], ok(b"c2\n"))
            .expect(
                &[
                    "diff-tree",
                    "-r",
                    "-z",
                    "--no-commit-id",
                    "--name-status",
                    "-M",
                    "--root",
                    "c2",
                    "a1",
                    "--",
                ],
                ok(b"M\0a.rs\0A\0logo.png\0R090\0old\0new\0D\0gone\0"),
            )
            .expect(
                &[
                    "diff-tree",
                    "-r",
                    "-z",
                    "--numstat",
                    "-M",
                    "--no-commit-id",
                    "c2",
                    "a1",
                    "--",
                ],
                ok(b"3\t1\ta.rs\0-\t-\tlogo.png\x000\t2\t\0old\0new\0"),
            );
        let (range, rows) = pr_files(&fake, &id("b0"), &id("a1")).unwrap();
        assert_eq!(
            range,
            CommitRange {
                oid: id("a1"),
                parent: Some(id("c2"))
            }
        );
        let counts: Vec<FileCounts> = rows.iter().map(|r| r.counts).collect();
        assert_eq!(
            counts,
            [
                FileCounts::Lines {
                    added: 3,
                    removed: 1
                },
                FileCounts::Binary,
                FileCounts::Lines {
                    added: 0,
                    removed: 2
                },
                FileCounts::Unknown,
            ]
        );
        assert_eq!(rows[2].file.orig_path, Some("old".into()));
    }

    #[test]
    fn unrelated_histories_or_a_garbled_answer_say_why() {
        let fake = FakeRunner::new()
            .expect(&["merge-base", "b0", "a1"], fail(""))
            .expect(&["merge-base", "b0", "a1"], ok(b"--evil\n"));
        assert!(merge_base(&fake, &id("b0"), &id("a1")).is_err());
        assert!(merge_base(&fake, &id("b0"), &id("a1")).is_err());
    }

    #[test]
    fn against_real_git_a_missing_commit_is_fetched_by_id_without_a_ref() {
        use crate::git::CliRunner;
        use crate::git::fake::{TempDir, git};

        let tmp = TempDir::new("pr-objects");
        let remote = tmp.path().join("remote.git");
        let work = tmp.path().join("work");
        let clone = tmp.path().join("clone");
        std::fs::create_dir_all(&work).unwrap();
        git(&work, &["init", "-q", "-b", "main"]);
        git(&work, &["config", "user.name", "T"]);
        git(&work, &["config", "user.email", "t@example.com"]);
        std::fs::write(work.join("a.txt"), "one\n").unwrap();
        git(&work, &["add", "-A"]);
        git(&work, &["commit", "-q", "-m", "base"]);
        let rev = |dir: &std::path::Path| {
            String::from_utf8(git(dir, &["rev-parse", "HEAD"]))
                .unwrap()
                .trim()
                .to_string()
        };
        let base = rev(&work);
        git(tmp.path(), &["clone", "-q", "--bare", "work", "remote.git"]);
        git(tmp.path(), &["clone", "-q", "remote.git", "clone"]);
        // The pull request's head lives only under a ref the clone never fetches.
        std::fs::write(work.join("a.txt"), "two\n").unwrap();
        git(&work, &["commit", "-q", "-am", "head"]);
        let head = rev(&work);
        let remote_url = remote.to_string_lossy().into_owned();
        git(&work, &["push", "-q", &remote_url, "HEAD:refs/pull/7/head"]);

        let runner = CliRunner::isolated(&clone);
        let (head, base) = (id(&head), id(&base));
        assert_eq!(
            missing_commits(&runner, &[&head, &base]),
            Ok(vec![head.clone()])
        );
        let refs_before = git(&clone, &["for-each-ref"]);
        fetch_commits(
            &runner,
            &RemoteName::from("origin"),
            std::slice::from_ref(&head),
            None,
        )
        .unwrap();
        assert_eq!(missing_commits(&runner, &[&head, &base]), Ok(vec![]));
        assert_eq!(
            git(&clone, &["for-each-ref"]),
            refs_before,
            "a ref was written"
        );

        let (range, rows) = pr_files(&runner, &base, &head).unwrap();
        assert_eq!(range.parent, Some(base));
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].counts,
            FileCounts::Lines {
                added: 1,
                removed: 1
            }
        );
    }
}
