//! A pull request's commits in the local repository: whether they are
//! here, fetching the missing ones from the forge's remote by id, the
//! changed files between the merge base and the head, and the commits in
//! between, each of which can be reviewed on its own, and a file's lines at
//! the head for a code suggestion. Ids reach argv only as [`CommitId`]s,
//! hex digits, so nothing in it reads as an option; no ref name from the
//! server ever does. The one ref name fetched, a merge request's
//! `refs/merge-requests/<n>/head` when the forge will not serve the head
//! by id, is built from the number and checked again here, and no local
//! ref takes it. The one ref written is the local
//! branch `pr/<number>` a checkout makes or moves (see [`plan_checkout`]
//! and [`pr_checkout_commands`]), named from the number alone.

use std::collections::HashMap;

use kika_git::branches::BranchName;
use kika_git::commit_files::{
    CommitFile, CommitId, CommitRange, LineCounts, commit_files, numstat,
};
use kika_git::error::GitError;
use kika_git::path::{RepoPath, os_string};
use kika_git::remote::{AskpassEnv, askpass_for, with_prompts};
use kika_git::remotes::RemoteName;
use kika_git::repo_state::{Operation, git_dir_command, git_dir_from, in_progress};
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
    let ids: Vec<&str> = oids.iter().map(CommitId::as_str).collect();
    fetch_command(remote, &ids)
}

/// [`fetch_commits_command`] of the forge's head ref `head` instead of
/// ids: the same line, and with no destination no local ref moves.
pub fn fetch_head_ref_command(remote: &RemoteName, head: &str) -> GitCommand {
    fetch_command(remote, &[head])
}

fn fetch_command(remote: &RemoteName, what: &[&str]) -> GitCommand {
    let cmd = GitCommand::new("-c")
        .arg("fetch.writeFetchHEAD=false")
        .arg("fetch")
        .arg("--quiet")
        .arg("--no-tags")
        .arg("--no-recurse-submodules")
        // Options end here, so even a remote named like one is only a name.
        .arg("--")
        .arg(remote.as_os_string());
    what.iter()
        .fold(cmd, |cmd, item| cmd.arg(item))
        .locale_env()
        .env("GIT_OPTIONAL_LOCKS", "0")
}

/// Whether `head` is a merge request's head ref as the forge names it,
/// `refs/merge-requests/<digits>/head`, and nothing else.
fn head_ref_ok(head: &str) -> bool {
    head.strip_prefix("refs/merge-requests/")
        .and_then(|rest| rest.strip_suffix("/head"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
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
/// user started. When that fails and the forge has a `fallback` head ref,
/// that ref is fetched instead; whether the commits came is for the
/// caller to check again. A fallback that is not a merge request's head
/// ref is ignored. A remote named like an option is refused, as for a tag
/// push. Errors are safe to draw and hold no credentials; when both
/// fetches fail, the first one's is kept.
pub fn fetch_commits(
    runner: &dyn GitRunner,
    remote: &RemoteName,
    oids: &[CommitId],
    fallback: Option<&str>,
    askpass: Option<&AskpassEnv>,
) -> Result<(), String> {
    if remote.0.is_empty() || remote.0.starts_with(b"-") || oids.is_empty() {
        return Err("Fetch from a remote named like an option with git directly".into());
    }
    let askpass = askpass_for(runner, askpass, &|key| std::env::var_os(key).is_some());
    let fetch = |cmd: GitCommand| {
        runner
            .run_checked(&with_prompts(cmd, askpass.as_ref()))
            .map(drop)
            .map_err(|e| e.to_string())
    };
    let first = fetch(fetch_commits_command(remote, oids));
    match (first, fallback.filter(|head| head_ref_ok(head))) {
        (Err(error), Some(head)) => fetch(fetch_head_ref_command(remote, head)).map_err(|_| error),
        (result, _) => result,
    }
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

/// Where the local branch `pr/<number>` stands against the pull
/// request's head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchAt {
    /// No such branch: it is made at the head.
    Absent,
    /// It is at the head already.
    Same,
    /// It is at this older commit, which the head contains, so it can
    /// move forward. Before a missing head is fetched this is only what
    /// the branch holds; the plan made after the fetch checks it.
    FastForward(CommitId),
    /// It has commits the head does not: never moved by Kika.
    Diverged,
    /// Another worktree has it checked out: never moved by Kika.
    OtherWorktree,
}

/// What checking out a pull request's head as `pr/<number>` would do,
/// read from local git only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutPlan {
    /// The head commit is in the local repository.
    pub head_here: bool,
    /// Where a missing head may be fetched from; `None` when it is here
    /// or no remote names the reviewed repository on a configured host.
    pub remote: Option<RemoteName>,
    pub at: BranchAt,
    /// The branch is the one checked out in this worktree.
    pub current: bool,
    /// Operations in progress in this repository, which a checkout
    /// would mix with.
    pub in_progress: Vec<Operation>,
}

/// The local branch a pull request is checked out as: `pr/<number>`,
/// built from the number only, so no text from the server reaches argv.
pub fn pr_branch(number: u64) -> BranchName {
    BranchName(format!("pr/{number}").into_bytes())
}

/// `git for-each-ref` of `branch`: its full name, commit, whether it is
/// `HEAD` here, and the worktree that has it checked out, NUL-separated.
/// `%(worktreepath)` needs git 2.23; an older git fails the read, and the
/// checkout with it.
pub fn branch_ref_command(branch: &BranchName) -> GitCommand {
    query(
        GitCommand::new("for-each-ref")
            .arg("--format=%(refname)%00%(objectname)%00%(HEAD)%00%(worktreepath)")
            .arg(branch.full_ref()),
    )
}

/// `git merge-base --is-ancestor <old> <head>`: exit 0 when `head`
/// contains `old`.
pub fn is_ancestor_command(old: &CommitId, head: &CommitId) -> GitCommand {
    query(
        GitCommand::new("merge-base")
            .arg("--is-ancestor")
            .arg(old.as_str())
            .arg(head.as_str()),
    )
}

/// The git commands that check out `head` as `branch`, which held `old`
/// (`None`: no such branch) and is checked out here when `current`. Each
/// takes only fixed words, the branch built from the number and hex ids:
/// - absent: `checkout -q -b <branch> <head> --`;
/// - at the head, elsewhere: `checkout -q <branch> --`;
/// - behind, checked out: `merge --ff-only -q <head>`;
/// - behind, elsewhere: `checkout -q <branch> --`, then the same merge.
///
/// The branch only moves once it is checked out, so a switch git refuses
/// (for local changes it would overwrite) leaves it where it was. At the
/// head and checked out, there is nothing to run: none. Run them only
/// after [`verify_pr_branch`] says the branch is still as planned.
pub fn pr_checkout_commands(
    branch: &BranchName,
    head: &CommitId,
    old: Option<&CommitId>,
    current: bool,
) -> Vec<GitCommand> {
    let switch = || crate::op::checkout_command(branch).write_env();
    // It may run the user's post-merge hook, as any merge does.
    let forward = || {
        GitCommand::new("merge")
            .arg("--ff-only")
            .arg("-q")
            .arg(head.as_str())
            .user_env()
    };
    match old {
        None => vec![
            GitCommand::new("checkout")
                .arg("-q")
                .arg("-b")
                .arg(branch.as_os_string())
                .arg(head.as_str())
                .arg("--")
                .write_env(),
        ],
        Some(old) if old == head && current => vec![],
        Some(old) if old == head => vec![switch()],
        Some(_) if current => vec![forward()],
        Some(_) => vec![switch(), forward()],
    }
}

/// Whether `branch` is still where the question saw it, read again right
/// before the checkout runs: absent when `old` is `None`, else at `old`,
/// checked out here exactly when `current` (so `HEAD` is the branch at
/// `old` before a merge moves it), and in no other worktree. Anything else
/// may have moved while the question was open, and nothing runs.
pub fn verify_pr_branch(
    runner: &dyn GitRunner,
    branch: &BranchName,
    old: Option<&CommitId>,
    current: bool,
) -> Result<(), String> {
    let still = match (old, read_branch(runner, branch)?) {
        (None, None) => !current,
        (Some(old), Some((oid, here, elsewhere))) => &oid == old && here == current && !elsewhere,
        _ => false,
    };
    if still {
        Ok(())
    } else {
        Err(format!(
            "{} moved since the question; press b again",
            branch.display()
        ))
    }
}

/// `branch`'s commit, whether it is checked out here, and whether another
/// worktree has it, or `None` when there is no such branch.
fn read_branch(
    runner: &dyn GitRunner,
    branch: &BranchName,
) -> Result<Option<(CommitId, bool, bool)>, String> {
    let output = runner
        .run_checked(&branch_ref_command(branch))
        .map_err(|e| e.message_or("git could not list the branch"))?;
    let full = branch.full_ref();
    let full = full.as_encoded_bytes();
    for line in output.stdout.split(|&b| b == b'\n') {
        let fields: Vec<&[u8]> = line.split(|&b| b == 0).collect();
        // A pattern matches below a slash too: only the branch itself counts.
        let [name, oid, head, worktree] = fields.as_slice() else {
            continue;
        };
        if *name != full {
            continue;
        }
        let oid = std::str::from_utf8(oid)
            .ok()
            .and_then(CommitId::parse)
            .ok_or("git listed the branch with an id Kika could not read")?;
        let current = *head == b"*";
        return Ok(Some((oid, current, !current && !worktree.is_empty())));
    }
    Ok(None)
}

/// Whether `head` contains `old`. A failure with nothing on stderr is
/// git's "no"; anything else is an error.
fn contains(runner: &dyn GitRunner, old: &CommitId, head: &CommitId) -> Result<bool, String> {
    let output = runner
        .run(&is_ancestor_command(old, head))
        .map_err(|e| GitError::io(&e).to_string())?;
    if output.success {
        return Ok(true);
    }
    if output.stderr.iter().all(u8::is_ascii_whitespace) {
        return Ok(false);
    }
    Err(format!(
        "git merge-base failed: {}",
        sanitize(String::from_utf8_lossy(&output.stderr).trim().as_bytes())
    ))
}

/// The operations in progress here, read from git's own dir.
fn operations(runner: &dyn GitRunner) -> Result<Vec<Operation>, String> {
    let output = runner
        .run_checked(&git_dir_command())
        .map_err(|e| e.message_or("git could not say where its dir is"))?;
    Ok(in_progress(&git_dir_from(&output.stdout)))
}

/// What checking out `head` as `branch` would do. Local reads only: no
/// network, and `remote` is left for the caller to fill in.
pub fn plan_checkout(
    runner: &dyn GitRunner,
    head: &CommitId,
    branch: &BranchName,
) -> Result<CheckoutPlan, String> {
    let head_here = missing_commits(runner, &[head])?.is_empty();
    let (at, current) = match read_branch(runner, branch)? {
        None => (BranchAt::Absent, false),
        Some((_, _, true)) => (BranchAt::OtherWorktree, false),
        Some((old, current, false)) if &old == head => (BranchAt::Same, current),
        // Only once the head is here can git say whether it contains the branch.
        Some((old, current, false)) if !head_here => (BranchAt::FastForward(old), current),
        Some((old, current, false)) => {
            let at = if contains(runner, &old, head)? {
                BranchAt::FastForward(old)
            } else {
                BranchAt::Diverged
            };
            (at, current)
        }
    };
    Ok(CheckoutPlan {
        head_here,
        remote: None,
        at,
        current,
        in_progress: operations(runner)?,
    })
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

/// `git cat-file blob <head>:<path>`: a file as it is at the pull
/// request's head. The argument is built from the path's raw bytes, so a
/// name that is not UTF-8 still names its own file; it starts with hex,
/// so it never reads as an option.
pub fn blob_command(head: &CommitId, path: &RepoPath) -> GitCommand {
    let mut spec = format!("{}:", head.as_str()).into_bytes();
    spec.extend_from_slice(path.as_bytes());
    query(
        GitCommand::new("cat-file")
            .arg("blob")
            .arg(os_string(&spec)),
    )
}

/// Lines `first..=last` (1-based) of `path` at `head`, each without its
/// `\n` and one trailing `\r`, exactly as the file has them. A range past
/// the end of the file, or lines that are not UTF-8, are an error.
pub fn read_new_lines(
    runner: &dyn GitRunner,
    head: &CommitId,
    path: &RepoPath,
    first: u32,
    last: u32,
) -> Result<Vec<String>, String> {
    let output = runner
        .run_checked(&blob_command(head, path))
        .map_err(|e| e.message_or("git cat-file failed"))?;
    let text = output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout);
    let lines: Vec<&[u8]> = if output.stdout.is_empty() {
        Vec::new()
    } else {
        text.split(|&b| b == b'\n').collect()
    };
    let (Some(from), Some(to)) = (first.checked_sub(1), last.checked_sub(1)) else {
        return Err("Lines are counted from 1".into());
    };
    let picked = lines
        .get(from as usize..=to as usize)
        .filter(|_| from <= to)
        .ok_or_else(|| format!("line {last} is past the end of the file"))?;
    picked
        .iter()
        .map(|line| {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            String::from_utf8(line.to_vec()).map_err(|_| "the lines are not UTF-8".to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{STANDARD_ENV, env_list};
    use kika_git::fake::{FakeRunner, fail, ok};
    use kika_git::runner::{EnvProfile, GitOutput};
    use std::ffi::OsString;

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
                None,
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
            fetch_commits(&fake, &RemoteName::from("origin"), &[id("a1")], None, None).unwrap_err();
        assert!(!message.contains("tok"), "{message}");
        assert!(!message.contains('\x1b'), "{message}");

        let fake = FakeRunner::new();
        assert!(
            fetch_commits(
                &fake,
                &RemoteName::from("--upload-pack=x"),
                &[id("a1")],
                Some("refs/merge-requests/1/head"),
                None
            )
            .is_err()
        );
        assert!(fetch_commits(&fake, &RemoteName::from("origin"), &[], None, None).is_err());
        fake.assert_done();
    }

    /// The fixed fetch line of `what` from `origin`.
    fn fetch_argv(what: &str) -> Vec<&str> {
        vec![
            "-c",
            "fetch.writeFetchHEAD=false",
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-recurse-submodules",
            "--",
            "origin",
            what,
        ]
    }

    const MR_HEAD: &str = "refs/merge-requests/12/head";

    #[test]
    fn a_failed_fetch_by_id_falls_back_on_the_merge_requests_head_ref() {
        let fake = FakeRunner::new()
            .expect(&fetch_argv("a1"), fail("fatal: not our ref a1\n"))
            .expect(&fetch_argv(MR_HEAD), ok(b""));
        let origin = RemoteName::from("origin");
        assert_eq!(
            fetch_commits(&fake, &origin, &[id("a1")], Some(MR_HEAD), None),
            Ok(())
        );
        fake.assert_done();
        // The same environment as the fetch by id, prompts and all.
        let envs = fake.process_envs();
        assert_eq!(envs.len(), 2);
        assert_eq!(envs[0], envs[1]);
        assert_eq!(
            args(&fetch_head_ref_command(&origin, MR_HEAD)),
            fetch_argv(MR_HEAD)
        );
    }

    #[test]
    fn when_both_fetches_fail_the_first_error_is_kept() {
        let fake = FakeRunner::new()
            .expect(&fetch_argv("a1"), fail("fatal: first\n"))
            .expect(&fetch_argv(MR_HEAD), fail("fatal: second\n"));
        let message = fetch_commits(
            &fake,
            &RemoteName::from("origin"),
            &[id("a1")],
            Some(MR_HEAD),
            None,
        )
        .unwrap_err();
        assert!(message.contains("first"), "{message}");
        assert!(!message.contains("second"), "{message}");
        fake.assert_done();
    }

    #[test]
    fn with_no_fallback_or_a_malformed_one_there_is_no_second_fetch() {
        for fallback in [
            None,
            Some("refs/heads/main"),
            Some("refs/merge-requests//head"),
            Some("refs/merge-requests/1x/head"),
            Some("refs/merge-requests/12/head:refs/heads/main"),
            Some("refs/merge-requests/12/merge"),
            Some("--upload-pack=x"),
            Some("+refs/merge-requests/12/head"),
        ] {
            let fake = FakeRunner::new().expect(&fetch_argv("a1"), fail("fatal: no\n"));
            assert!(
                fetch_commits(
                    &fake,
                    &RemoteName::from("origin"),
                    &[id("a1")],
                    fallback,
                    None
                )
                .is_err(),
                "{fallback:?}"
            );
            fake.assert_done();
        }
        // A fetch by id that works needs no fallback.
        let fake = FakeRunner::new().expect(&fetch_argv("a1"), ok(b""));
        assert_eq!(
            fetch_commits(
                &fake,
                &RemoteName::from("origin"),
                &[id("a1")],
                Some(MR_HEAD),
                None
            ),
            Ok(())
        );
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

    const HEAD: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
    const OLD: &str = "b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0";

    fn ref_line(name: &str, oid: &str, head: &str, worktree: &str) -> Vec<u8> {
        format!("{name}\0{oid}\0{head}\0{worktree}\n").into_bytes()
    }

    const FOR_EACH_REF: [&str; 3] = [
        "for-each-ref",
        "--format=%(refname)%00%(objectname)%00%(HEAD)%00%(worktreepath)",
        "refs/heads/pr/7",
    ];

    /// A runner that answers the plan's reads: the head here or not, the
    /// branch's line, then `is_ancestor` when asked.
    fn plan_runner(here: bool, listed: &[u8], ancestor: Option<GitOutput>) -> FakeRunner {
        let has = format!("{HEAD}^{{commit}}");
        let fake = FakeRunner::new()
            .expect(
                &["cat-file", "-e", &has],
                if here { ok(b"") } else { fail("") },
            )
            .expect(&FOR_EACH_REF, ok(listed));
        let fake = match ancestor {
            Some(answer) => fake.expect(&["merge-base", "--is-ancestor", OLD, HEAD], answer),
            None => fake,
        };
        fake.expect(
            &["rev-parse", "--absolute-git-dir"],
            ok(b"/nonexistent/kika-test/.git\n"),
        )
    }

    fn plan(fake: &FakeRunner) -> Result<CheckoutPlan, String> {
        let result = plan_checkout(fake, &id(HEAD), &pr_branch(7));
        fake.assert_done();
        result
    }

    #[test]
    fn the_branch_is_named_from_the_number_and_its_reads_are_fixed_queries() {
        assert_eq!(pr_branch(7), BranchName::from("pr/7"));
        assert_eq!(pr_branch(12345).display(), "pr/12345");
        assert_eq!(args(&branch_ref_command(&pr_branch(7))), FOR_EACH_REF);
        assert_eq!(
            args(&is_ancestor_command(&id(OLD), &id(HEAD))),
            ["merge-base", "--is-ancestor", OLD, HEAD]
        );
        let query = env_list(&[STANDARD_ENV.as_slice(), &[("GIT_NO_LAZY_FETCH", "1")]].concat());
        for cmd in [
            branch_ref_command(&pr_branch(7)),
            is_ancestor_command(&id(OLD), &id(HEAD)),
        ] {
            assert_eq!(cmd.process_env(), query, "{cmd:?}");
        }
        // Each checkout names only fixed words, the branch and hex ids.
        let steps = |old: Option<&str>, current| {
            let old = old.map(id);
            pr_checkout_commands(&pr_branch(7), &id(HEAD), old.as_ref(), current)
                .iter()
                .map(args)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            steps(None, false),
            [["checkout", "-q", "-b", "pr/7", HEAD, "--"]]
        );
        assert_eq!(steps(Some(HEAD), false), [["checkout", "-q", "pr/7", "--"]]);
        assert!(steps(Some(HEAD), true).is_empty());
        assert_eq!(steps(Some(OLD), true), [["merge", "--ff-only", "-q", HEAD]]);
        assert_eq!(
            steps(Some(OLD), false),
            [
                vec!["checkout", "-q", "pr/7", "--"],
                vec!["merge", "--ff-only", "-q", HEAD],
            ]
        );
    }

    #[test]
    fn each_place_of_the_branch_plans_as_itself() {
        let pr7 = "refs/heads/pr/7";
        let absent = plan(&plan_runner(true, b"", None)).unwrap();
        assert_eq!(
            absent,
            CheckoutPlan {
                head_here: true,
                remote: None,
                at: BranchAt::Absent,
                current: false,
                in_progress: vec![],
            }
        );
        // Only the branch itself counts, not one below it.
        let below = ref_line("refs/heads/pr/7/x", OLD, " ", "");
        assert_eq!(
            plan(&plan_runner(true, &below, None)).unwrap().at,
            BranchAt::Absent
        );
        let same = plan(&plan_runner(true, &ref_line(pr7, HEAD, "*", "/w"), None)).unwrap();
        assert_eq!((same.at, same.current), (BranchAt::Same, true));
        let behind = ref_line(pr7, OLD, " ", "");
        let ff = plan(&plan_runner(true, &behind, Some(ok(b"")))).unwrap();
        assert_eq!(ff.at, BranchAt::FastForward(id(OLD)));
        assert!(!ff.current);
        let diverged = plan(&plan_runner(true, &behind, Some(fail("")))).unwrap();
        assert_eq!(diverged.at, BranchAt::Diverged);
        let elsewhere = ref_line(pr7, OLD, " ", "/other/worktree");
        assert_eq!(
            plan(&plan_runner(true, &elsewhere, None)).unwrap().at,
            BranchAt::OtherWorktree
        );
        // A missing head cannot be compared yet: no merge-base runs.
        let missing = plan(&plan_runner(false, &behind, None)).unwrap();
        assert!(!missing.head_here);
        assert_eq!(missing.at, BranchAt::FastForward(id(OLD)));
    }

    #[test]
    fn a_failed_read_or_a_garbled_id_refuses_the_plan() {
        let pr7 = "refs/heads/pr/7";
        let behind = ref_line(pr7, OLD, " ", "");
        let has = format!("{HEAD}^{{commit}}");
        let broken = FakeRunner::new()
            .expect(&["cat-file", "-e", &has], ok(b""))
            .expect(&FOR_EACH_REF, ok(&behind))
            .expect(
                &["merge-base", "--is-ancestor", OLD, HEAD],
                fail("fatal: bad object"),
            );
        assert!(plan(&broken).unwrap_err().contains("bad object"));
        let fake = FakeRunner::new()
            .expect(&["cat-file", "-e", &has], ok(b""))
            .expect(&FOR_EACH_REF, ok(&ref_line(pr7, "--evil", " ", "")));
        assert!(plan(&fake).is_err());
        let fake = FakeRunner::new()
            .expect(&["cat-file", "-e", &has], ok(b""))
            .expect(
                &FOR_EACH_REF,
                fail("fatal: unknown field name: worktreepath"),
            );
        assert!(plan(&fake).unwrap_err().contains("worktreepath"));
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

    #[test]
    fn against_real_git_a_pull_request_branch_is_made_moved_forward_and_never_forced() {
        use crate::op::{GitOp, run_op};
        use kika_git::CliRunner;
        use kika_git::fake::{TempDir, git};

        let tmp = TempDir::new("pr-checkout");
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let run = |args: &[&str]| git(&work, args);
        let rev = |name: &str| {
            String::from_utf8(git(&work, &["rev-parse", name]))
                .unwrap()
                .trim()
                .to_string()
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.name", "T"]);
        run(&["config", "user.email", "t@example.com"]);
        let commit = |file: &str, text: &str| {
            std::fs::write(work.join(file), text).unwrap();
            run(&["add", "-A"]);
            run(&["commit", "-q", "-m", text]);
            id(&rev("HEAD"))
        };
        let base = commit("a.txt", "base");
        run(&["checkout", "-q", "-b", "feature"]);
        let h1 = commit("a.txt", "one");
        let h2 = commit("a.txt", "two");
        run(&["checkout", "-q", "main"]);
        let runner = CliRunner::isolated(&work);
        let branch = pr_branch(7);
        let on = || {
            String::from_utf8(git(&work, &["symbolic-ref", "--short", "HEAD"]))
                .unwrap()
                .trim()
                .to_string()
        };
        let op = |head: &CommitId, plan: &CheckoutPlan| GitOp::CheckoutPrBranch {
            number: 7,
            branch: branch.clone(),
            head: head.clone(),
            old: match &plan.at {
                BranchAt::Absent => None,
                BranchAt::Same => Some(head.clone()),
                BranchAt::FastForward(old) => Some(old.clone()),
                other => panic!("{other:?}"),
            },
            current: plan.current,
        };

        // Absent: made at the head and checked out.
        let plan = plan_checkout(&runner, &h1, &branch).unwrap();
        assert_eq!((plan.at.clone(), plan.head_here), (BranchAt::Absent, true));
        run_op(&runner, &op(&h1, &plan)).unwrap();
        assert_eq!(
            (rev("pr/7"), on()),
            (h1.as_str().to_string(), "pr/7".into())
        );

        // A later head, checked out elsewhere: moved forward and switched to.
        run(&["checkout", "-q", "main"]);
        let plan = plan_checkout(&runner, &h2, &branch).unwrap();
        assert_eq!(plan.at, BranchAt::FastForward(h1.clone()));
        assert!(!plan.current);
        run_op(&runner, &op(&h2, &plan)).unwrap();
        assert_eq!(
            (rev("pr/7"), on()),
            (h2.as_str().to_string(), "pr/7".into())
        );

        // Checked out and behind: a fast-forward merge.
        run(&["reset", "-q", "--hard", h1.as_str()]);
        let plan = plan_checkout(&runner, &h2, &branch).unwrap();
        assert_eq!(
            (plan.at.clone(), plan.current),
            (BranchAt::FastForward(h1.clone()), true)
        );
        run_op(&runner, &op(&h2, &plan)).unwrap();
        assert_eq!(
            (rev("pr/7"), on()),
            (h2.as_str().to_string(), "pr/7".into())
        );
        let plan = plan_checkout(&runner, &h2, &branch).unwrap();
        assert_eq!((plan.at, plan.current), (BranchAt::Same, true));

        // Diverged, as after a force-push: planned as such, left alone.
        run(&["checkout", "-q", "main"]);
        let side = commit("b.txt", "side");
        run(&["branch", "-q", "-f", "pr/7", side.as_str()]);
        let plan = plan_checkout(&runner, &h2, &branch).unwrap();
        assert_eq!(plan.at, BranchAt::Diverged);
        assert_eq!(rev("pr/7"), side.as_str());

        // A stale old id: refused before anything runs, nothing moves.
        run(&["branch", "-q", "-f", "pr/7", h1.as_str()]);
        let stale = GitOp::CheckoutPrBranch {
            number: 7,
            branch: branch.clone(),
            head: h2.clone(),
            old: Some(base.clone()),
            current: false,
        };
        assert!(run_op(&runner, &stale).is_err());
        assert_eq!(
            (rev("pr/7"), on()),
            (h1.as_str().to_string(), "main".into())
        );
        let main = rev("main");

        // Planned on pr/7, then main checked out before `y`: no merge
        // lands on main.
        run(&["checkout", "-q", "pr/7"]);
        let plan = plan_checkout(&runner, &h2, &branch).unwrap();
        assert!(plan.current);
        let planned = op(&h2, &plan);
        run(&["checkout", "-q", "main"]);
        let refused = run_op(&runner, &planned).unwrap_err();
        assert!(refused.contains("moved since the question"), "{refused}");
        assert_eq!((rev("main"), on()), (main.clone(), "main".into()));
        assert_eq!(rev("pr/7"), h1.as_str());

        // Planned from main, then pr/7 checked out before `y`.
        let plan = plan_checkout(&runner, &h2, &branch).unwrap();
        assert!(!plan.current);
        let planned = op(&h2, &plan);
        run(&["checkout", "-q", "pr/7"]);
        assert!(run_op(&runner, &planned).is_err());
        assert_eq!(
            (rev("pr/7"), on()),
            (h1.as_str().to_string(), "pr/7".into())
        );

        // A switch git refuses for local changes leaves pr/7 unmoved.
        run(&["checkout", "-q", "main"]);
        let plan = plan_checkout(&runner, &h2, &branch).unwrap();
        std::fs::write(work.join("a.txt"), "local edit").unwrap();
        assert!(run_op(&runner, &op(&h2, &plan)).is_err());
        assert_eq!(
            (rev("pr/7"), on()),
            (h1.as_str().to_string(), "main".into())
        );
        run(&["checkout", "-q", "--", "a.txt"]);

        // Another worktree's branch is never moved.
        let other = tmp.path().join("other");
        let other_path = other.to_string_lossy().into_owned();
        run(&[
            "worktree",
            "add",
            "-q",
            "-b",
            "pr/8",
            &other_path,
            h1.as_str(),
        ]);
        let plan = plan_checkout(&runner, &h2, &pr_branch(8)).unwrap();
        assert_eq!(plan.at, BranchAt::OtherWorktree);

        // An operation in progress is seen.
        std::fs::write(work.join(".git/BISECT_LOG"), "").unwrap();
        let plan = plan_checkout(&runner, &h2, &branch).unwrap();
        assert_eq!(plan.in_progress, [Operation::Bisect]);
    }

    #[test]
    fn the_blob_is_read_from_the_raw_path_and_only_the_picked_lines_come_back() {
        let path = RepoPath::from_bytes(b"src/caf\xe9.rs");
        let cmd = blob_command(&id("a1"), &path);
        assert_eq!(
            cmd.args(),
            [
                OsString::from("cat-file"),
                OsString::from("blob"),
                kika_git::path::os_string(b"a1:src/caf\xe9.rs"),
            ]
        );
        let query = env_list(&[STANDARD_ENV.as_slice(), &[("GIT_NO_LAZY_FETCH", "1")]].concat());
        assert_eq!(cmd.process_env(), query);

        let path = RepoPath::from_bytes(b"src/lib.rs");
        let argv = ["cat-file", "blob", "a1:src/lib.rs"];
        let blob = b"one\r\n\ttwo\r\nthree\n";
        let fake = FakeRunner::new()
            .expect(&argv, ok(blob))
            .expect(&argv, ok(b"one\nlast"))
            .expect(&argv, ok(blob))
            .expect(&argv, ok(b"ok\n\xff\n"))
            .expect(&argv, fail("fatal: bad object"));
        assert_eq!(
            read_new_lines(&fake, &id("a1"), &path, 2, 3),
            Ok(vec!["\ttwo".to_string(), "three".to_string()])
        );
        // A last line with no newline is still a line.
        assert_eq!(
            read_new_lines(&fake, &id("a1"), &path, 2, 2),
            Ok(vec!["last".to_string()])
        );
        assert!(read_new_lines(&fake, &id("a1"), &path, 3, 4).is_err());
        assert!(read_new_lines(&fake, &id("a1"), &path, 1, 2).is_err());
        assert!(read_new_lines(&fake, &id("a1"), &path, 1, 1).is_err());
        fake.assert_done();
    }
}
