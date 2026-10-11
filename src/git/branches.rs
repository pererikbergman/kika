//! Local branches for the Branches panel: each with its upstream, how far
//! it is ahead or behind, and whether deleting it would lose commits, in
//! the order the user last checked them out. Also a branch's recent log.

use std::ffi::{OsStr, OsString};

use super::diff::terminal_safe;
use super::runner::{GitCommand, GitOutput, GitRunner};

/// A branch's short name as the raw bytes of its ref, so a name that is
/// not UTF-8 reaches argv unchanged. Equality is on the bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchName(pub Vec<u8>);

/// How a branch stands against its upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Track {
    /// No upstream is configured.
    None,
    /// An upstream is configured but its ref no longer exists.
    Gone,
    Counts {
        ahead: u32,
        behind: u32,
    },
}

/// One local branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    pub name: BranchName,
    /// `HEAD` points at it.
    pub head: bool,
    /// The upstream's short name, terminal-safe.
    pub upstream: Option<String>,
    pub track: Track,
    /// Whether `git branch -d` would delete it without losing commits.
    pub merged: bool,
}

/// One commit of a branch's log, every field terminal-safe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRow {
    pub short_oid: String,
    pub date: String,
    pub author: String,
    pub subject: String,
}

/// How many commits [`branch_log`] reads; enough to see what a branch holds.
const LOG_LIMIT: &str = "200";

impl BranchName {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(bytes.to_vec())
    }

    /// For drawing only: lossy, with control characters replaced.
    pub fn display(&self) -> String {
        terminal_safe(&self.0)
    }

    /// The short name, for argv.
    pub fn as_os_string(&self) -> OsString {
        super::path::os_string(&self.0)
    }

    /// `refs/heads/<name>`, for argv where a short name could be taken for
    /// something else, such as a tag or an option.
    pub fn full_ref(&self) -> OsString {
        super::path::os_string(&self.ref_bytes())
    }

    /// `refs/heads/<name>` as bytes, to name the ref a log belongs to.
    pub fn ref_bytes(&self) -> Vec<u8> {
        let mut bytes = b"refs/heads/".to_vec();
        bytes.extend(&self.0);
        bytes
    }
}

impl From<&str> for BranchName {
    fn from(name: &str) -> Self {
        Self::from_bytes(name.as_bytes())
    }
}

/// Every local branch: the current one first, then those checked out
/// most recently, then the rest by newest commit.
pub fn list_branches(runner: &dyn GitRunner) -> Result<Vec<Branch>, String> {
    let refs = run(
        runner,
        GitCommand::new("for-each-ref")
            .arg(
                "--format=%(refname)%00%(HEAD)%00%(upstream:short)%00\
                 %(upstream:track,nobracket)%00%(committerdate:unix)",
            )
            .arg("refs/heads"),
    )?;
    let mut rows: Vec<(Branch, i64)> = refs
        .split(|&b| b == b'\n')
        .filter_map(parse_ref_line)
        .collect();
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let merged = merged_into_head(runner);
    for (branch, _) in &mut rows {
        branch.merged = match branch.track {
            Track::Counts { ahead, .. } => ahead == 0,
            Track::None | Track::Gone => merged.contains(&branch.name),
        };
    }
    let recent = recent_checkouts(runner);
    // Newest commit first; the sort is stable, so ties keep ref order.
    rows.sort_by(|(_, a), (_, b)| b.cmp(a));
    let rank = |branch: &Branch| {
        if branch.head {
            (0, 0)
        } else {
            match recent.iter().position(|name| *name == branch.name) {
                Some(i) => (1, i),
                None => (2, 0),
            }
        }
    };
    rows.sort_by_key(|(branch, _)| rank(branch));
    Ok(rows.into_iter().map(|(branch, _)| branch).collect())
}

/// The newest commits of `refname`, a full ref such as `refs/heads/main`
/// or `refs/remotes/origin/main`, so no tag or option can stand in for it.
pub fn branch_log(runner: &dyn GitRunner, refname: &OsStr) -> Result<Vec<LogRow>, String> {
    let out = run(
        runner,
        GitCommand::new("log")
            .arg("-n")
            .arg(LOG_LIMIT)
            .arg("--date=short")
            .arg("--format=%h%x1f%ad%x1f%an%x1f%s")
            .arg(refname)
            .arg("--"),
    )?;
    Ok(out
        .split(|&b| b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            let mut fields = line.splitn(4, |&b| b == 0x1f).map(terminal_safe);
            let mut next = || fields.next().unwrap_or_default();
            LogRow {
                short_oid: next(),
                date: next(),
                author: next(),
                subject: next(),
            }
        })
        .collect())
}

/// One `for-each-ref` row and its committer date, or `None` for a blank
/// or malformed one.
fn parse_ref_line(line: &[u8]) -> Option<(Branch, i64)> {
    let mut fields = line.split(|&b| b == 0);
    let refname = fields.next()?;
    let name = refname.strip_prefix(b"refs/heads/")?;
    let head = fields.next()? == b"*";
    let upstream = fields.next()?;
    let track = fields.next()?;
    let date = fields.next()?;
    let upstream = (!upstream.is_empty()).then(|| terminal_safe(upstream));
    let track = match (&upstream, track) {
        (None, _) => Track::None,
        (Some(_), b"gone") => Track::Gone,
        (Some(_), counts) => parse_counts(counts),
    };
    let date = std::str::from_utf8(date).ok()?.trim().parse().unwrap_or(0);
    Some((
        Branch {
            name: BranchName::from_bytes(name),
            head,
            upstream,
            track,
            merged: false,
        },
        date,
    ))
}

/// `ahead 1, behind 2`, either part alone, or nothing when in step.
fn parse_counts(text: &[u8]) -> Track {
    let text = String::from_utf8_lossy(text);
    let (mut ahead, mut behind) = (0, 0);
    for part in text.split(',') {
        match part.trim().split_once(' ') {
            Some(("ahead", n)) => ahead = n.parse().unwrap_or(0),
            Some(("behind", n)) => behind = n.parse().unwrap_or(0),
            _ => {}
        }
    }
    Track::Counts { ahead, behind }
}

/// The branches whose tips `HEAD` contains. An unborn `HEAD` makes git
/// fail; then nothing counts as merged, which only asks for a stronger
/// confirmation.
fn merged_into_head(runner: &dyn GitRunner) -> Vec<BranchName> {
    let cmd = GitCommand::new("for-each-ref")
        .arg("--merged=HEAD")
        .arg("--format=%(refname)")
        .arg("refs/heads");
    let Ok(out) = run(runner, cmd) else {
        return Vec::new();
    };
    out.split(|&b| b == b'\n')
        .filter_map(|line| line.strip_prefix(b"refs/heads/"))
        .map(BranchName::from_bytes)
        .collect()
}

/// Branch names in the order `HEAD`'s reflog last checked them out,
/// newest first. No reflog means no order, not an error.
fn recent_checkouts(runner: &dyn GitRunner) -> Vec<BranchName> {
    let cmd = GitCommand::new("reflog")
        .arg("show")
        .arg("--format=%gs")
        .arg("-n")
        .arg("1000")
        .arg("HEAD");
    let Ok(out) = run(runner, cmd) else {
        return Vec::new();
    };
    let mut names: Vec<BranchName> = Vec::new();
    for line in out.split(|&b| b == b'\n') {
        let Some(moves) = line.strip_prefix(b"checkout: moving from ") else {
            continue;
        };
        let Some(at) = moves.windows(4).position(|w| w == b" to ") else {
            continue;
        };
        // The branch moved to is the more recent of the two.
        for name in [&moves[at + 4..], &moves[..at]] {
            let name = BranchName::from_bytes(name);
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
}

/// Run a read-only query that must not take the index lock from the user,
/// returning its stdout or git's message.
pub(super) fn run(runner: &dyn GitRunner, cmd: GitCommand) -> Result<Vec<u8>, String> {
    let cmd = cmd.env("GIT_OPTIONAL_LOCKS", "0").env("LC_ALL", "C");
    let GitOutput {
        success,
        stdout,
        stderr,
    } = runner
        .run(&cmd)
        .map_err(|e| terminal_safe(e.to_string().as_bytes()))?;
    if success {
        return Ok(stdout);
    }
    let message = terminal_safe(&stderr);
    if message.is_empty() {
        let subcommand = cmd.args()[0].to_string_lossy();
        return Err(format!("git {subcommand} failed"));
    }
    Err(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::CliRunner;
    use crate::git::fake::{TempDir, git};

    /// A repo on `main` with one commit, made at a fixed date.
    fn repo() -> TempDir {
        let tmp = TempDir::new("branches");
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        commit(dir, "init", 1_000);
        tmp
    }

    /// Commit a change to `f.txt` with `message`, dated `at` seconds.
    fn commit(dir: &std::path::Path, message: &str, at: u64) {
        std::fs::write(dir.join("f.txt"), format!("{message}\n")).unwrap();
        git(dir, &["add", "-A"]);
        let date = format!("@{at} +0000");
        let cmd = std::process::Command::new("git")
            .current_dir(dir)
            .envs(crate::git::runner::isolation_env())
            .env("GIT_COMMITTER_DATE", &date)
            .env("GIT_AUTHOR_DATE", &date)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(["-c", "commit.gpgsign=false", "commit", "-q", "-m", message])
            .output()
            .unwrap();
        assert!(cmd.status.success(), "{cmd:?}");
    }

    fn names(branches: &[Branch]) -> Vec<String> {
        branches.iter().map(|b| b.name.display()).collect()
    }

    fn named<'a>(branches: &'a [Branch], name: &str) -> &'a Branch {
        branches
            .iter()
            .find(|b| b.name.display() == name)
            .unwrap_or_else(|| panic!("{name} in {:?}", names(branches)))
    }

    #[test]
    fn recent_checkouts_come_first_then_the_rest_by_newest_commit_in_real_git() {
        let tmp = repo();
        let dir = tmp.path();
        for name in ["a", "b", "old", "new"] {
            git(dir, &["branch", name]);
        }
        git(dir, &["checkout", "-q", "new"]);
        commit(dir, "on new", 3_000);
        git(dir, &["checkout", "-q", "old"]);
        commit(dir, "on old", 2_000);
        git(dir, &["checkout", "-q", "main"]);
        // Forget those checkouts, so only the next three count.
        git(dir, &["reflog", "expire", "--expire=all", "--all"]);
        git(dir, &["checkout", "-q", "a"]);
        git(dir, &["checkout", "-q", "b"]);
        git(dir, &["checkout", "-q", "main"]);

        let branches = list_branches(&CliRunner::isolated(dir)).unwrap();

        assert_eq!(names(&branches), ["main", "b", "a", "new", "old"]);
        assert!(branches[0].head);
        assert!(branches[1..].iter().all(|b| !b.head));
    }

    fn rev(dir: &std::path::Path, rev: &str) -> String {
        String::from_utf8(git(dir, &["rev-parse", rev]))
            .unwrap()
            .trim()
            .to_string()
    }

    /// `main` at the first commit, plus:
    /// - `t`: one commit of its own, tracking `origin/t`, which has two
    ///   commits `t` lacks;
    /// - `g`: tracking `origin/g`, which was deleted;
    /// - `old`: at an ancestor of `HEAD`, no upstream;
    /// - `own`: a commit of its own, no upstream.
    fn tracking_repo() -> TempDir {
        let tmp = repo();
        let dir = tmp.path();
        let base = rev(dir, "HEAD");
        git(dir, &["remote", "add", "origin", "/nowhere"]);
        git(dir, &["checkout", "-q", "-b", "up"]);
        commit(dir, "up 1", 1_100);
        commit(dir, "up 2", 1_200);
        let up = rev(dir, "HEAD");
        git(dir, &["update-ref", "refs/remotes/origin/t", &up]);
        git(dir, &["update-ref", "refs/remotes/origin/g", &base]);
        git(dir, &["checkout", "-q", "-b", "t", &base]);
        commit(dir, "t 1", 1_300);
        git(dir, &["branch", "-u", "origin/t"]);
        git(dir, &["checkout", "-q", "-b", "own", &base]);
        commit(dir, "own 1", 1_400);
        git(dir, &["checkout", "-q", "main"]);
        git(dir, &["branch", "-D", "up"]);
        git(dir, &["branch", "old", &base]);
        git(dir, &["branch", "g", &base]);
        git(dir, &["branch", "-u", "origin/g", "g"]);
        git(dir, &["update-ref", "-d", "refs/remotes/origin/g"]);
        tmp
    }

    #[test]
    fn upstream_counts_and_a_gone_upstream_parse_in_real_git() {
        let tmp = tracking_repo();
        let branches = list_branches(&CliRunner::isolated(tmp.path())).unwrap();

        let t = named(&branches, "t");
        assert_eq!(t.upstream.as_deref(), Some("origin/t"));
        assert_eq!(
            t.track,
            Track::Counts {
                ahead: 1,
                behind: 2
            }
        );
        let g = named(&branches, "g");
        assert_eq!(g.upstream.as_deref(), Some("origin/g"));
        assert_eq!(g.track, Track::Gone);
        let main = named(&branches, "main");
        assert_eq!((main.upstream.as_deref(), main.track), (None, Track::None));
    }

    #[test]
    fn merged_follows_the_rule_branch_d_uses_in_real_git() {
        let tmp = tracking_repo();
        let branches = list_branches(&CliRunner::isolated(tmp.path())).unwrap();

        // Ancestors of `HEAD`, with no live upstream to ask instead.
        assert!(named(&branches, "old").merged);
        assert!(named(&branches, "g").merged);
        assert!(named(&branches, "main").merged);
        // A commit of its own, checked against `HEAD` or the upstream.
        assert!(!named(&branches, "own").merged);
        assert!(!named(&branches, "t").merged);
    }

    #[test]
    fn a_branch_in_step_with_its_upstream_counts_zero_and_is_merged() {
        let tmp = repo();
        let dir = tmp.path();
        git(dir, &["remote", "add", "origin", "/nowhere"]);
        git(dir, &["checkout", "-q", "-b", "side"]);
        commit(dir, "side 1", 1_100);
        let side = rev(dir, "HEAD");
        git(dir, &["update-ref", "refs/remotes/origin/side", &side]);
        git(dir, &["branch", "-u", "origin/side"]);
        git(dir, &["checkout", "-q", "main"]);

        let branches = list_branches(&CliRunner::isolated(dir)).unwrap();

        let side = named(&branches, "side");
        assert_eq!(
            side.track,
            Track::Counts {
                ahead: 0,
                behind: 0
            }
        );
        assert!(side.merged, "pushed, so -d would take it");
    }

    #[test]
    fn without_a_reflog_branches_sort_by_newest_commit_in_real_git() {
        let tmp = repo();
        let dir = tmp.path();
        git(dir, &["branch", "older"]);
        git(dir, &["checkout", "-q", "-b", "newer"]);
        commit(dir, "newer", 5_000);
        git(dir, &["checkout", "-q", "main"]);
        std::fs::remove_dir_all(dir.join(".git/logs")).unwrap();

        let branches = list_branches(&CliRunner::isolated(dir)).unwrap();

        assert_eq!(names(&branches), ["main", "newer", "older"]);
    }

    #[test]
    fn a_repository_with_no_commits_has_no_branches_in_real_git() {
        let tmp = TempDir::new("branches-unborn");
        git(tmp.path(), &["init", "-q"]);
        assert_eq!(list_branches(&CliRunner::isolated(tmp.path())), Ok(vec![]));
    }

    #[test]
    fn the_log_lists_the_branch_commits_newest_first_in_real_git() {
        let tmp = repo();
        let dir = tmp.path();
        git(dir, &["checkout", "-q", "-b", "side"]);
        commit(dir, "side \x1b[2J work", 86_400 * 365);
        git(dir, &["checkout", "-q", "main"]);

        let rows = branch_log(&CliRunner::isolated(dir), OsStr::new("refs/heads/side")).unwrap();

        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].short_oid,
            rev(dir, "side")[..rows[0].short_oid.len()]
        );
        assert_eq!(rows[0].date, "1971-01-01");
        assert_eq!(rows[0].author, "t");
        assert_eq!(rows[0].subject, "side \u{FFFD}[2J work");
        assert_eq!(rows[1].subject, "init");
    }

    #[test]
    fn a_failing_query_is_an_error_made_safe() {
        use crate::git::fake::{FakeRunner, fail};
        let fake = FakeRunner::new().expect(
            &[
                "log",
                "-n",
                "200",
                "--date=short",
                "--format=%h%x1f%ad%x1f%an%x1f%s",
                "refs/heads/x",
                "--",
            ],
            fail("fatal: bad \x1b[2Jref\n"),
        );
        assert_eq!(
            branch_log(&fake, OsStr::new("refs/heads/x")),
            Err("fatal: bad \u{FFFD}[2Jref".into())
        );
        let env: Vec<(OsString, OsString)> = vec![
            ("GIT_OPTIONAL_LOCKS".into(), "0".into()),
            ("LC_ALL".into(), "C".into()),
        ];
        assert_eq!(fake.envs(), [env]);
    }

    /// Branch names that are not UTF-8 cannot be loose ref files on every
    /// filesystem, so these are written straight into `packed-refs`.
    #[cfg(unix)]
    #[test]
    fn a_name_that_is_not_utf8_reaches_argv_as_its_own_bytes_in_real_git() {
        use std::io::Write;

        let tmp = repo();
        let dir = tmp.path();
        let base = rev(dir, "HEAD");
        git(dir, &["checkout", "-q", "-b", "other"]);
        commit(dir, "look-alike", 1_100);
        let other = rev(dir, "HEAD");
        git(dir, &["checkout", "-q", "main"]);
        let mut packed = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(dir.join(".git/packed-refs"))
            .unwrap();
        packed
            .write_all(format!("{base} refs/heads/x").as_bytes())
            .unwrap();
        packed.write_all(b"\xff\n").unwrap();
        packed
            .write_all(format!("{other} refs/heads/x\u{FFFD}\n").as_bytes())
            .unwrap();
        drop(packed);
        let runner = CliRunner::isolated(dir);

        let branches = list_branches(&runner).unwrap();
        let raw = branches
            .iter()
            .find(|b| b.name.0 == b"x\xff")
            .expect("the raw name is listed");
        assert_eq!(raw.name.display(), "x\u{FFFD}");

        let rows = branch_log(&runner, &raw.name.full_ref()).unwrap();
        assert_eq!(rows.len(), 1, "only init, not the look-alike's commit");
        assert_eq!(rows[0].subject, "init");
    }
}
