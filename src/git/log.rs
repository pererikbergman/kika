//! The history of `HEAD`, a page at a time, and the sets that say which of
//! its commits are pushed or already merged into the main branch.

use std::collections::HashSet;

use super::diff::terminal_safe;
use super::runner::{GitCommand, GitOutput, GitRunner};

/// One commit of the log, as the Commits panel lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRow {
    pub oid: String,
    pub short_oid: String,
    /// First parent first; empty for a root commit.
    pub parents: Vec<String>,
    /// Terminal-safe: it is drawn as it is.
    pub author: String,
    /// Author time, seconds since the epoch.
    pub time: i64,
    /// Terminal-safe: it is drawn as it is.
    pub subject: String,
}

/// A slice of the log. `done` means there is nothing after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogPage {
    pub rows: Vec<CommitRow>,
    pub done: bool,
}

/// What decides each commit's marker. A `None` set means "not known", so
/// the marker is left out rather than guessed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Markers {
    /// The main branch's full ref name, if one was found.
    pub main: Option<String>,
    /// Commits of `HEAD` not reachable from the main branch.
    pub unmerged: Option<HashSet<String>>,
    /// Commits of `HEAD` not reachable from any remote-tracking ref.
    pub unpushed: Option<HashSet<String>>,
}

/// Past this many commits a set is dropped instead of kept partial, so a
/// marker is never wrong, only missing.
pub const MARKER_CAP: usize = 10_000;

/// The hex fields and the time first, the free text last: a stray
/// separator in an author name can only shift text into the subject.
const FORMAT: &str = "--format=%H%x1f%h%x1f%P%x1f%at%x1f%an%x1f%s";

/// Up to `limit` commits of `tip` (or `HEAD`), after skipping `skip`. Later
/// pages pass the first page's newest commit as `tip`, so a page never
/// mixes two histories when `HEAD` moves in between. An unborn `HEAD` is
/// an empty, finished log, not an error.
pub fn log_page(
    runner: &dyn GitRunner,
    tip: Option<&str>,
    skip: usize,
    limit: usize,
) -> Result<LogPage, String> {
    // Only an object id from an earlier page: nothing that git could read
    // as an option, without `--end-of-options`, which older git lacks.
    if let Some(tip) = tip
        && (tip.is_empty() || !tip.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(format!(
            "not a commit id: {}",
            terminal_safe(tip.as_bytes())
        ));
    }
    let output = run(
        runner,
        GitCommand::new("log")
            .arg("--no-show-signature")
            .arg("--no-color")
            .arg("-z")
            .arg(FORMAT)
            .arg(format!("--skip={skip}"))
            .arg("-n")
            .arg(limit.to_string())
            .arg(tip.unwrap_or("HEAD"))
            .arg("--"),
    )?;
    if !output.success {
        if tip.is_none() && !resolves(runner, "HEAD")? {
            return Ok(LogPage {
                rows: Vec::new(),
                done: true,
            });
        }
        return Err(failure(&output, "git log failed"));
    }
    let rows = output
        .stdout
        .split(|&b| b == 0)
        .map(|record| record.strip_prefix(b"\n").unwrap_or(record))
        .filter(|record| !record.is_empty())
        .map(parse_row)
        .collect::<Vec<_>>();
    Ok(LogPage {
        done: rows.len() < limit,
        rows,
    })
}

/// One `FORMAT` record. A record that cannot be read still makes a row,
/// so the page keeps its length and the next page's `skip` stays right.
fn parse_row(record: &[u8]) -> CommitRow {
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    let fields: Vec<&[u8]> = record.splitn(6, |&b| b == 0x1f).collect();
    if let [oid, short, parents, time, author, subject] = fields.as_slice()
        && let Ok(time) = text(time).parse()
    {
        return CommitRow {
            oid: text(oid),
            short_oid: text(short),
            parents: text(parents).split_whitespace().map(String::from).collect(),
            author: terminal_safe(author),
            time,
            subject: terminal_safe(subject),
        };
    }
    let oid = terminal_safe(fields.first().copied().unwrap_or_default());
    CommitRow {
        short_oid: oid.chars().take(7).collect(),
        oid,
        parents: Vec::new(),
        author: String::new(),
        time: 0,
        subject: "(a commit Kika could not read)".into(),
    }
}

/// Which commits of `HEAD` are unpushed and which are not yet merged into
/// the main branch. A set that cannot be known, or is too large, is `None`.
pub fn markers(runner: &dyn GitRunner) -> Result<Markers, String> {
    let main = main_branch(runner)?;
    let unmerged = match &main {
        Some(main) => rev_list(runner, &["HEAD", &format!("^{main}")])?,
        None => None,
    };
    let remotes = run(
        runner,
        GitCommand::new("for-each-ref")
            .arg("--count=1")
            .arg("--format=%(refname)")
            .arg("refs/remotes"),
    )?;
    if !remotes.success {
        return Err(failure(&remotes, "git for-each-ref failed"));
    }
    let unpushed = if remotes.stdout.trim_ascii().is_empty() {
        None
    } else {
        rev_list(runner, &["HEAD", "--not", "--remotes"])?
    };
    Ok(Markers {
        main,
        unmerged,
        unpushed,
    })
}

/// `origin/HEAD`'s target, else local `main`, else local `master`.
fn main_branch(runner: &dyn GitRunner) -> Result<Option<String>, String> {
    for candidate in [
        "refs/remotes/origin/HEAD",
        "refs/heads/main",
        "refs/heads/master",
    ] {
        let output = run(
            runner,
            GitCommand::new("rev-parse")
                .arg("-q")
                .arg("--verify")
                .arg("--symbolic-full-name")
                .arg(candidate),
        )?;
        let name = String::from_utf8_lossy(output.stdout.trim_ascii()).into_owned();
        if output.success && !name.is_empty() {
            return Ok(Some(name));
        }
    }
    Ok(None)
}

/// The commits `revs` select, or `None` past [`MARKER_CAP`] or when git
/// cannot list them (an unborn `HEAD`).
fn rev_list(runner: &dyn GitRunner, revs: &[&str]) -> Result<Option<HashSet<String>>, String> {
    let cmd = revs.iter().fold(
        GitCommand::new("rev-list")
            .arg("-n")
            .arg((MARKER_CAP + 1).to_string()),
        |cmd, rev| cmd.arg(*rev),
    );
    let output = run(runner, cmd.arg("--"))?;
    if !output.success {
        return Ok(None);
    }
    let set: HashSet<String> = String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .map(String::from)
        .collect();
    Ok((set.len() <= MARKER_CAP).then_some(set))
}

fn resolves(runner: &dyn GitRunner, rev: &str) -> Result<bool, String> {
    let output = run(
        runner,
        GitCommand::new("rev-parse")
            .arg("-q")
            .arg("--verify")
            .arg(rev),
    )?;
    Ok(output.success)
}

fn failure(output: &GitOutput, fallback: &str) -> String {
    let message = terminal_safe(&output.stderr);
    if message.is_empty() {
        fallback.into()
    } else {
        message
    }
}

/// Run a read-only query that must not take the index lock from the user.
fn run(runner: &dyn GitRunner, cmd: GitCommand) -> Result<GitOutput, String> {
    let cmd = cmd.env("GIT_OPTIONAL_LOCKS", "0").env("LC_ALL", "C");
    runner
        .run(&cmd)
        .map_err(|e| terminal_safe(e.to_string().as_bytes()))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::git::CliRunner;
    use crate::git::fake::{TempDir, git};

    /// A repository on `main` with an identity and fixed dates.
    fn repo(label: &str) -> TempDir {
        let tmp = TempDir::new(label);
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        tmp
    }

    /// An empty commit by `author` at unix time `time`.
    fn commit(dir: &Path, message: &str, author: &str, time: i64) {
        let date = format!("@{time} +0000");
        let mut cmd = std::process::Command::new("git");
        cmd.current_dir(dir)
            .envs(crate::git::runner::isolation_env())
            .env("GIT_AUTHOR_NAME", author)
            .env("GIT_AUTHOR_EMAIL", "a@a")
            .env("GIT_COMMITTER_NAME", author)
            .env("GIT_COMMITTER_EMAIL", "a@a")
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .args(["commit", "-q", "--allow-empty", "-m", message]);
        assert!(cmd.status().unwrap().success());
    }

    fn rev(dir: &Path, name: &str) -> String {
        String::from_utf8(git(dir, &["rev-parse", name]))
            .unwrap()
            .trim()
            .to_string()
    }

    #[test]
    fn a_merge_commit_reads_back_with_both_parents_and_its_fields() {
        let tmp = repo("log-merge");
        let dir = tmp.path();
        commit(dir, "base", "Ann", 1_000);
        git(dir, &["checkout", "-q", "-b", "side"]);
        commit(dir, "on side", "Bob", 2_000);
        git(dir, &["checkout", "-q", "main"]);
        commit(dir, "on main", "Ann", 3_000);
        let main = rev(dir, "main");
        let side = rev(dir, "side");
        git(
            dir,
            &["merge", "-q", "--no-ff", "-m", "Merge \x1b[2Jside", "side"],
        );
        let runner = CliRunner::isolated(dir);

        let page = log_page(&runner, None, 0, 10).unwrap();

        assert!(page.done);
        assert_eq!(page.rows.len(), 4);
        let merge = &page.rows[0];
        assert_eq!(merge.oid, rev(dir, "HEAD"));
        assert!(merge.oid.starts_with(&merge.short_oid));
        assert!(merge.short_oid.len() >= 7);
        assert_eq!(merge.parents, [main, side]);
        assert_eq!(merge.author, "t");
        assert_eq!(merge.subject, "Merge \u{FFFD}[2Jside");
        let base = page.rows.last().unwrap();
        assert_eq!(
            (base.author.as_str(), base.time, base.subject.as_str()),
            ("Ann", 1_000, "base")
        );
        assert!(base.parents.is_empty());
    }

    #[test]
    fn pages_from_a_fixed_tip_concatenate_to_the_whole_log() {
        let tmp = repo("log-pages");
        let dir = tmp.path();
        for i in 0..7 {
            commit(dir, &format!("c{i}"), "Ann", 1_000 + i);
        }
        let runner = CliRunner::isolated(dir);
        let all = log_page(&runner, None, 0, 100).unwrap().rows;
        let tip = all[0].oid.clone();
        // HEAD moves on; the fixed tip keeps the pages on the old history.
        commit(dir, "later", "Ann", 5_000);

        let mut paged = Vec::new();
        let mut skip = 0;
        loop {
            let page = log_page(&runner, Some(&tip), skip, 3).unwrap();
            skip += page.rows.len();
            paged.extend(page.rows);
            if page.done {
                break;
            }
        }

        assert_eq!(paged, all);
        assert_eq!(all.len(), 7);
    }

    #[test]
    fn an_unborn_repo_gives_an_empty_finished_page() {
        let tmp = repo("log-unborn");
        let runner = CliRunner::isolated(tmp.path());
        assert_eq!(
            log_page(&runner, None, 0, 200),
            Ok(LogPage {
                rows: vec![],
                done: true
            })
        );
    }

    #[test]
    fn only_the_commit_on_top_of_the_pushed_history_is_unpushed() {
        let origin = TempDir::new("log-origin");
        git(origin.path(), &["init", "-q", "--bare", "-b", "main"]);
        let tmp = repo("log-unpushed");
        let dir = tmp.path();
        commit(dir, "one", "Ann", 1_000);
        commit(dir, "two", "Ann", 2_000);
        let url = origin.path().to_str().unwrap();
        git(dir, &["remote", "add", "origin", url]);
        git(dir, &["push", "-q", "origin", "main"]);
        commit(dir, "three", "Ann", 3_000);
        let runner = CliRunner::isolated(dir);

        let markers = markers(&runner).unwrap();

        let head: HashSet<String> = [rev(dir, "HEAD")].into();
        assert_eq!(markers.unpushed, Some(head));
        // No origin/HEAD here, so local main is the main branch.
        assert_eq!(markers.main.as_deref(), Some("refs/heads/main"));
    }

    #[test]
    fn a_feature_branchs_own_commits_are_unmerged_and_mains_are_not() {
        let tmp = repo("log-unmerged");
        let dir = tmp.path();
        commit(dir, "base", "Ann", 1_000);
        git(dir, &["checkout", "-q", "-b", "feature"]);
        commit(dir, "f1", "Ann", 2_000);
        commit(dir, "f2", "Ann", 3_000);
        let runner = CliRunner::isolated(dir);

        let markers = markers(&runner).unwrap();

        let own: HashSet<String> = [rev(dir, "HEAD"), rev(dir, "HEAD~1")].into();
        assert_eq!(markers.unmerged, Some(own));
        assert!(!markers.unmerged.unwrap().contains(&rev(dir, "main")));
    }

    #[test]
    fn without_remotes_nothing_is_known_about_pushing() {
        let tmp = repo("log-no-remote");
        commit(tmp.path(), "one", "Ann", 1_000);
        let runner = CliRunner::isolated(tmp.path());

        let markers = markers(&runner).unwrap();

        assert_eq!(markers.unpushed, None);
        assert_eq!(markers.unmerged, Some(HashSet::new()));
    }

    #[test]
    fn origin_head_names_the_main_branch_before_local_ones() {
        let tmp = repo("log-origin-head");
        let dir = tmp.path();
        commit(dir, "one", "Ann", 1_000);
        git(dir, &["update-ref", "refs/remotes/origin/trunk", "HEAD"]);
        git(
            dir,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/trunk",
            ],
        );
        let runner = CliRunner::isolated(dir);
        assert_eq!(
            markers(&runner).unwrap().main.as_deref(),
            Some("refs/remotes/origin/trunk")
        );
    }

    #[test]
    fn with_no_main_branch_nothing_is_known_about_merging() {
        let tmp = TempDir::new("log-no-main");
        git(tmp.path(), &["init", "-q", "-b", "trunk"]);
        commit(tmp.path(), "one", "Ann", 1_000);
        let runner = CliRunner::isolated(tmp.path());
        let markers = markers(&runner).unwrap();
        assert_eq!((markers.main, markers.unmerged), (None, None));
    }

    #[test]
    fn an_unreadable_record_becomes_a_placeholder_row_and_the_page_keeps_its_length() {
        use crate::git::fake::{FakeRunner, ok};

        let argv = [
            "log",
            "--no-show-signature",
            "--no-color",
            "-z",
            FORMAT,
            "--skip=0",
            "-n",
            "3",
            "HEAD",
            "--",
        ];
        let fake = FakeRunner::new().expect(
            &argv,
            ok(b"a1\x1fa1\x1fb1\x1f5\x1fAn\x1fn\x1fsubj\0\
                 b1\x1fb1\x1fc1\x1fsoon\x1fBob\x1fbad time\0\
                 c1\x1fc1\0"),
        );

        let page = log_page(&fake, None, 0, 3).unwrap();

        assert_eq!(page.rows.len(), 3);
        assert!(!page.done);
        // A separator in the author only spills into the subject.
        let first = &page.rows[0];
        assert_eq!((first.time, first.author.as_str()), (5, "An"));
        assert!(first.subject.starts_with('n') && first.subject.ends_with("subj"));
        assert!(
            !first.subject.contains(char::is_control),
            "{:?}",
            first.subject
        );
        for (row, oid) in page.rows[1..].iter().zip(["b1", "c1"]) {
            assert_eq!(row.oid, oid);
            assert_eq!(row.subject, "(a commit Kika could not read)");
        }
    }

    #[test]
    fn a_tip_that_is_not_a_hex_object_id_is_refused_before_git_runs() {
        use crate::git::fake::FakeRunner;

        let fake = FakeRunner::new();
        for tip in ["--output=x", "", "HEAD~1", "abc def", "abc\u{1b}"] {
            assert!(log_page(&fake, Some(tip), 0, 10).is_err(), "{tip:?}");
        }
        fake.assert_done();
    }

    #[test]
    fn every_query_is_lock_free_and_in_the_c_locale() {
        use crate::git::fake::{FakeRunner, ok};
        use std::ffi::OsString;

        let fake = FakeRunner::new().expect(
            &[
                "log",
                "--no-show-signature",
                "--no-color",
                "-z",
                FORMAT,
                "--skip=0",
                "-n",
                "2",
                "HEAD",
                "--",
            ],
            ok(b"a\x1fa\x1f\x1f5\x1fAnn\x1fsubj\0"),
        );
        let page = log_page(&fake, None, 0, 2).unwrap();
        assert!(page.done);
        assert_eq!(page.rows[0].time, 5);
        fake.assert_done();
        let env: Vec<(OsString, OsString)> = vec![
            ("GIT_OPTIONAL_LOCKS".into(), "0".into()),
            ("LC_ALL".into(), "C".into()),
        ];
        assert_eq!(fake.envs(), [env]);
    }
}
