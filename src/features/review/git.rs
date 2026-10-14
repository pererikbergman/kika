//! A pull request's commits in the local repository: whether they are
//! here, fetching the missing ones from the forge's remote by id, the
//! changed files between the merge base and the head, and the commits in
//! between, each of which can be reviewed on its own. Ids reach argv only
//! as [`CommitId`]s, hex digits, so nothing in it reads as an option; no
//! ref name from the server ever does, and no ref is written.

use std::collections::HashMap;

use kika_git::commit_files::{
    CommitFile, CommitId, CommitRange, LineCounts, commit_files, numstat,
};
use kika_git::error::GitError;
use kika_git::remote::{AskpassEnv, askpass_for, with_prompts};
use kika_git::remotes::RemoteName;
use kika_git::runner::{GitCommand, GitRunner};
use kika_git::safe_text::sanitize;

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
/// which git before 2.44 ignores). It names no path, so literal pathspecs
/// change nothing.
fn query(cmd: GitCommand) -> GitCommand {
    cmd.standard_env().env("GIT_NO_LAZY_FETCH", "1")
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
/// reached. `--` ends the options, as git 2.15's fetch already allows.
/// English and no optional locks, but not literal pathspecs: a fetch may gc
/// and run the `pre-auto-gc` hook, which keeps the user's.
pub fn fetch_commits_command(remote: &RemoteName, oids: &[CommitId]) -> GitCommand {
    let cmd = GitCommand::new("-c")
        .arg("fetch.writeFetchHEAD=false")
        .arg("fetch")
        .arg("--quiet")
        .arg("--no-tags")
        .arg("--no-recurse-submodules")
        // Options end here, so even a remote named like one is only a name.
        .arg("--")
        .arg(remote.as_os_string());
    oids.iter()
        .fold(cmd, |cmd, oid| cmd.arg(oid.as_str()))
        .locale_env()
        .env("GIT_OPTIONAL_LOCKS", "0")
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
            .map_err(|e| GitError::io(&e).to_string())?;
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
    runner
        .run_checked(&cmd)
        .map(drop)
        .map_err(|e| e.to_string())
}

/// The merge base of `base` and `head`.
pub fn merge_base(
    runner: &dyn GitRunner,
    base: &CommitId,
    head: &CommitId,
) -> Result<CommitId, String> {
    let output = runner
        .run_checked(&merge_base_command(base, head))
        .map_err(|e| e.message_or("The pull request's head and base share no history"))?;
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
    let rows = range_files(runner, &range)?;
    Ok((range, rows))
}

/// The files `range` changes with their line counts: the whole pull
/// request from its merge base, or one of its commits.
pub fn range_files(runner: &dyn GitRunner, range: &CommitRange) -> Result<Vec<PrFileRow>, String> {
    let files = commit_files(runner, range)?;
    // By name, so a pull request of thousands of files is no slower to match.
    let stats: HashMap<Vec<u8>, LineCounts> = numstat(runner, range)?
        .into_iter()
        .map(|(path, counts)| (path.as_bytes().to_vec(), counts))
        .collect();
    Ok(files
        .into_iter()
        .map(|file| {
            let counts = match stats.get(file.path.as_bytes()) {
                Some(Some((added, removed))) => FileCounts::Lines {
                    added: *added,
                    removed: *removed,
                },
                Some(None) => FileCounts::Binary,
                None => FileCounts::Unknown,
            };
            PrFileRow { file, counts }
        })
        .collect())
}

/// The most commits of a pull request offered one by one.
pub const MAX_PR_COMMITS: usize = 250;

/// One commit of a pull request, shown against its first parent, and its
/// subject, already safe to draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrCommit {
    pub range: CommitRange,
    pub subject: String,
}

/// `git log` of the commits in `range`, merge base to head, newest first,
/// one more than [`MAX_PR_COMMITS`] so a longer list is known to be cut.
/// Ids reach argv only as hex; `--` ends the revisions, so no path is
/// read. No signature is checked, which would run gpg and add lines. A
/// range with no base lists the head's history, as far as the cap.
pub fn pr_commits_command(range: &CommitRange) -> GitCommand {
    let revisions = match &range.parent {
        Some(base) => format!("{}..{}", base.as_str(), range.oid.as_str()),
        None => range.oid.as_str().to_string(),
    };
    query(
        GitCommand::new("log")
            .arg("--no-show-signature")
            .arg("--no-color")
            .arg("--format=%H%x00%P%x00%s")
            .arg("-n")
            .arg((MAX_PR_COMMITS + 1).to_string())
            .arg(revisions)
            .arg("--"),
    )
}

/// The commits in `range`, oldest first, at most [`MAX_PR_COMMITS`] of
/// the newest, and whether there were more. An id or parent that is not
/// hex refuses the whole list rather than showing a commit against the
/// wrong tree.
pub fn pr_commits(
    runner: &dyn GitRunner,
    range: &CommitRange,
) -> Result<(Vec<PrCommit>, bool), String> {
    let output = runner
        .run_checked(&pr_commits_command(range))
        .map_err(|e| e.message_or("git log failed"))?;
    let mut commits = output
        .stdout
        .split(|&b| b == b'\n')
        .filter(|line| !line.is_empty())
        .map(parse_commit)
        .collect::<Result<Vec<_>, _>>()?;
    let truncated = commits.len() > MAX_PR_COMMITS;
    commits.truncate(MAX_PR_COMMITS);
    commits.reverse();
    Ok((commits, truncated))
}

/// One `%H NUL %P NUL %s` line of [`pr_commits_command`].
fn parse_commit(line: &[u8]) -> Result<PrCommit, String> {
    const GARBLED: &str = "git log gave a commit Kika could not read";
    let mut fields = line.splitn(3, |&b| b == 0);
    let (Some(oid), Some(parents), Some(subject)) = (fields.next(), fields.next(), fields.next())
    else {
        return Err(GARBLED.into());
    };
    let id = |raw: &[u8]| {
        std::str::from_utf8(raw)
            .ok()
            .and_then(CommitId::parse)
            .ok_or_else(|| GARBLED.to_string())
    };
    let parent = parents
        .split(|&b| b == b' ')
        .find(|p| !p.is_empty())
        .map(id)
        .transpose()?;
    Ok(PrCommit {
        range: CommitRange {
            oid: id(oid)?,
            parent,
        },
        subject: sanitize(subject),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{STANDARD_ENV, env_list};
    use kika_git::fake::{FakeRunner, fail, ok};
    use kika_git::runner::EnvProfile;

    fn id(hex: &str) -> CommitId {
        CommitId::parse(hex).unwrap()
    }

    fn args(cmd: &GitCommand) -> Vec<String> {
        cmd.args()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
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
                "--",
                "origin",
                "a1",
                "b0",
            ]
        );
        let query = env_list(&[STANDARD_ENV.as_slice(), &[("GIT_NO_LAZY_FETCH", "1")]].concat());
        for cmd in [
            has_commit_command(&id("a1")),
            merge_base_command(&id("b0"), &id("a1")),
        ] {
            assert_eq!(cmd.env_profile(), Some(EnvProfile::Standard), "{cmd:?}");
            assert_eq!(cmd.process_env(), query, "{cmd:?}");
        }
        let fetch = fetch_commits_command(&RemoteName::from("origin"), &[id("a1")]);
        assert_eq!(fetch.env_profile(), Some(EnvProfile::Locale));
        assert_eq!(
            fetch.process_env(),
            env_list(&[("LC_ALL", "C"), ("GIT_OPTIONAL_LOCKS", "0")])
        );
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
        let query = has_commit_command(&id("a1")).process_env();
        assert_eq!(fake.process_envs(), vec![query; 3]);
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
                    "--",
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
        let env = fake.process_envs().pop().unwrap();
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
                "--",
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

    fn log_argv(mb: &str, head: &str) -> Vec<String> {
        [
            "log",
            "--no-show-signature",
            "--no-color",
            "--format=%H%x00%P%x00%s",
            "-n",
            "251",
            &format!("{mb}..{head}"),
            "--",
        ]
        .map(String::from)
        .to_vec()
    }

    fn whole() -> CommitRange {
        CommitRange {
            oid: id("a1"),
            parent: Some(id("c2")),
        }
    }

    #[test]
    fn the_commit_list_is_a_fixed_log_of_the_merge_base_to_the_head() {
        let cmd = pr_commits_command(&whole());
        assert_eq!(args(&cmd), log_argv("c2", "a1"));
        let query = env_list(&[STANDARD_ENV.as_slice(), &[("GIT_NO_LAZY_FETCH", "1")]].concat());
        assert_eq!(cmd.env_profile(), Some(EnvProfile::Standard));
        assert_eq!(cmd.process_env(), query);
    }

    #[test]
    fn commits_come_oldest_first_against_their_first_parent_with_safe_subjects() {
        let argv = log_argv("c2", "a1");
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let fake = FakeRunner::new().expect(
            &argv,
            ok(b"a1\0b3 d4\0Merge main\nb3\0c2\0Fix \x1b[2Jit\x07\n"),
        );
        let (commits, truncated) = pr_commits(&fake, &whole()).unwrap();
        assert!(!truncated);
        assert_eq!(
            commits,
            [
                PrCommit {
                    range: CommitRange {
                        oid: id("b3"),
                        parent: Some(id("c2"))
                    },
                    subject: "Fix \u{FFFD}[2Jit\u{FFFD}".into(),
                },
                PrCommit {
                    range: CommitRange {
                        oid: id("a1"),
                        parent: Some(id("b3"))
                    },
                    subject: "Merge main".into(),
                },
            ]
        );
        fake.assert_done();
    }

    #[test]
    fn past_250_commits_the_list_says_it_is_cut_and_keeps_the_latest() {
        let argv = log_argv("c2", "a1");
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let lines = |n: usize| -> Vec<u8> {
            (0..n)
                .flat_map(|i| format!("{:x}\0c2\0c {i}\n", 0x1000 + i).into_bytes())
                .collect()
        };
        let fake = FakeRunner::new()
            .expect(&argv, ok(&lines(251)))
            .expect(&argv, ok(&lines(250)));
        let (commits, truncated) = pr_commits(&fake, &whole()).unwrap();
        assert!(truncated);
        assert_eq!(commits.len(), MAX_PR_COMMITS);
        // git lists newest first: the 251st line, the oldest, is the one left out.
        assert_eq!(commits[0].subject, "c 249");
        assert_eq!(commits[249].subject, "c 0");
        let (commits, truncated) = pr_commits(&fake, &whole()).unwrap();
        assert!(!truncated);
        assert_eq!(commits.len(), 250);
    }

    #[test]
    fn a_non_hex_id_or_parent_or_a_failed_log_refuses_the_whole_list() {
        let argv = log_argv("c2", "a1");
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let fake = FakeRunner::new()
            .expect(&argv, ok(b"a1\0c2\0ok\n--evil\0c2\0bad\n"))
            .expect(&argv, ok(b"a1\0-c2\0bad\n"))
            .expect(&argv, ok(b"a1 no separators\n"))
            .expect(&argv, fail("fatal: bad revision \x1b[2J\n"));
        for _ in 0..3 {
            assert!(pr_commits(&fake, &whole()).is_err());
        }
        let message = pr_commits(&fake, &whole()).unwrap_err();
        assert!(!message.contains('\x1b'), "{message}");
        fake.assert_done();
    }

    #[test]
    fn a_root_commit_has_no_parent_and_a_range_without_a_base_lists_from_the_head() {
        let root = CommitRange {
            oid: id("a1"),
            parent: None,
        };
        assert_eq!(
            args(&pr_commits_command(&root)),
            [
                "log",
                "--no-show-signature",
                "--no-color",
                "--format=%H%x00%P%x00%s",
                "-n",
                "251",
                "a1",
                "--"
            ]
        );
        let argv = log_argv("c2", "a1");
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let fake = FakeRunner::new().expect(&argv, ok(b"a1\0\0root\n"));
        let (commits, _) = pr_commits(&fake, &whole()).unwrap();
        assert_eq!(commits[0].range.parent, None);
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
        use kika_git::CliRunner;
        use kika_git::fake::{TempDir, git};

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
        assert_eq!(range.parent, Some(base.clone()));
        let (commits, truncated) = pr_commits(&runner, &range).unwrap();
        assert!(!truncated);
        assert_eq!(
            commits,
            [PrCommit {
                range: range.clone(),
                subject: "head".into()
            }]
        );
        assert_eq!(range_files(&runner, &commits[0].range).unwrap(), rows);
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
