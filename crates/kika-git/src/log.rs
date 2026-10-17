//! The history of `HEAD`, a page at a time, optionally narrowed to one
//! path or author, and the sets that say which of its commits are pushed
//! or already merged into the main branch.

use std::collections::HashSet;

use super::branches::BranchName;
use super::commit_files::CommitId;
use super::error::GitError;
use super::path::RepoPath;
use super::runner::{GitCommand, GitOutput, GitRunner};
use super::safe_text::terminal_safe;

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
    /// Making `subject` terminal-safe changed it: it is not git's subject
    /// byte for byte, so it must not be matched against another one.
    pub subject_altered: bool,
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

/// What git narrows the log to. The default is the whole history.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogQuery {
    /// One file or directory, followed across renames.
    pub path: Option<RepoPath>,
    /// Matched as fixed text against "Name <email>", with smart case.
    pub author: Option<String>,
}

impl LogQuery {
    pub fn is_active(&self) -> bool {
        self.path.is_some() || self.author.is_some()
    }

    /// The author half from typed text: trimmed, `None` when blank. A
    /// control char could not be typed on purpose, so it is refused.
    pub fn author_from(text: &str) -> Result<Option<String>, &'static str> {
        let text = text.trim();
        if text.contains(char::is_control) {
            return Err("An author cannot contain control characters");
        }
        Ok((!text.is_empty()).then(|| text.to_string()))
    }

    /// The path half from typed text, relative to the repository root:
    /// trimmed, one trailing `/` dropped, `None` when blank.
    pub fn path_from(text: &str) -> Result<Option<RepoPath>, &'static str> {
        let text = text.trim();
        if text.contains(char::is_control) {
            return Err("A path cannot contain control characters");
        }
        let text = text.strip_suffix('/').unwrap_or(text);
        Ok((!text.is_empty()).then(|| RepoPath::from_bytes(text.as_bytes())))
    }
}

/// Past this many commits a set is dropped instead of kept partial, so a
/// marker is never wrong, only missing.
pub const MARKER_CAP: usize = 10_000;

/// The hex fields and the time first, the free text last: a stray
/// separator in an author name can only shift text into the subject.
const FORMAT: &str = "--format=%H%x1f%h%x1f%P%x1f%at%x1f%an%x1f%s";

/// Up to `limit` commits of `tip` (or `HEAD`) that match `query`, after
/// skipping `skip`. Later pages pass the first page's newest commit as
/// `tip`, so a page never mixes two histories when `HEAD` moves in
/// between; under a query that commit is the newest match, so the set is
/// the same. A path query cannot skip, since `--follow` would lose a
/// rename it skipped: it reads from the top and reads ahead as far as is
/// already loaded, so its page can hold more than `limit` rows and the
/// git work over a long history grows linearly, not quadratically. An
/// unborn `HEAD` is an empty, finished log, not an error.
pub fn log_page(
    runner: &dyn GitRunner,
    tip: Option<&str>,
    skip: usize,
    limit: usize,
    query: &LogQuery,
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
    // `--follow` finds a rename only in a commit it shows, so a rename
    // inside the skipped commits would lose the file: a path query reads
    // from the top every time and drops what earlier pages had. Reading
    // `max(limit, skip)` past them doubles each request (200, 400, 800,
    // ...), so git is asked for at most about 3x what ends up loaded.
    let (drop, skip, limit) = match &query.path {
        Some(_) => (skip, 0, skip + limit.max(skip)),
        None => (0, skip, limit),
    };
    let cmd = log_command(tip, skip, limit, query);
    let output = run(runner, &cmd)?;
    if !output.success {
        if tip.is_none() && !resolves(runner, "HEAD")? {
            return Ok(LogPage {
                rows: Vec::new(),
                done: true,
            });
        }
        return Err(GitError::failed(&cmd, &output).message_or("git log failed"));
    }
    let rows = output
        .stdout
        .split(|&b| b == 0)
        .map(|record| record.strip_prefix(b"\n").unwrap_or(record))
        .filter(|record| !record.is_empty())
        .skip(drop)
        .map(parse_row)
        .collect::<Vec<_>>();
    Ok(LogPage {
        done: drop + rows.len() < limit,
        rows,
    })
}

/// The `git log` behind [`log_page`]. Typed text never stands alone in
/// argv: the author is glued to `--author=`, and the path comes after
/// `--`, read literally.
fn log_command(tip: Option<&str>, skip: usize, limit: usize, query: &LogQuery) -> GitCommand {
    let mut cmd = GitCommand::new("log")
        .arg("--no-show-signature")
        .arg("--no-color")
        .arg("-z")
        .arg(FORMAT)
        .arg(format!("--skip={skip}"))
        .arg("-n")
        .arg(limit.to_string());
    if let Some(author) = &query.author {
        cmd = cmd.arg(format!("--author={author}")).arg("--fixed-strings");
        if !author.chars().any(char::is_uppercase) {
            cmd = cmd.arg("--regexp-ignore-case");
        }
    }
    if query.path.is_some() {
        cmd = cmd.arg("--follow");
    }
    cmd = cmd.arg(tip.unwrap_or("HEAD")).arg("--");
    if let Some(path) = &query.path {
        cmd = cmd.arg(path.as_os_str());
    }
    cmd.standard_env()
}

/// The commit local branch `name` points at. The argument starts with
/// `refs/heads/`, so git can never read it as an option.
pub fn branch_commit(runner: &dyn GitRunner, name: &BranchName) -> Result<CommitId, String> {
    let mut rev = name.ref_bytes();
    rev.extend(b"^{commit}");
    let output = run(
        runner,
        &GitCommand::new("rev-parse")
            .arg("-q")
            .arg("--verify")
            .arg(super::path::os_string(&rev)),
    )?;
    if !output.success {
        return Err(format!("No branch named {}", name.display()));
    }
    let text = String::from_utf8_lossy(output.stdout.trim_ascii());
    CommitId::parse(&text)
        .ok_or_else(|| format!("not a commit id: {}", terminal_safe(text.as_bytes())))
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
            subject_altered: std::str::from_utf8(subject)
                .map_or(true, |raw| raw != terminal_safe(subject)),
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
        subject_altered: true,
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
    let remotes_cmd = GitCommand::new("for-each-ref")
        .arg("--count=1")
        .arg("--format=%(refname)")
        .arg("refs/remotes");
    let remotes = run(runner, &remotes_cmd)?;
    if !remotes.success {
        return Err(GitError::failed(&remotes_cmd, &remotes).message_or("git for-each-ref failed"));
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
            &GitCommand::new("rev-parse")
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
    let output = run(runner, &cmd.arg("--"))?;
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
        &GitCommand::new("rev-parse")
            .arg("-q")
            .arg("--verify")
            .arg(rev),
    )?;
    Ok(output.success)
}

/// Run a read-only query that must not take the index lock from the user.
/// Only [`log_command`] passes a path, after `--`, which must be read
/// literally; `--follow` comes only with it. The rest name no path, so
/// the whole [`Standard`](super::runner::EnvProfile::Standard) profile fits
/// every query here. A failed exit is the caller's to read.
fn run(runner: &dyn GitRunner, cmd: &GitCommand) -> Result<GitOutput, String> {
    runner
        .run(&cmd.clone().standard_env())
        .map_err(|e| GitError::io(&e).to_string())
}

#[cfg(test)]
mod tests {
    use crate::fake::{STANDARD_ENV, env_list};
    use std::path::Path;

    use super::*;
    use crate::CliRunner;
    use crate::fake::{TempDir, git};

    #[test]
    fn a_subject_made_safe_to_draw_is_flagged_as_altered() {
        let record = |subject: &[u8]| {
            let mut record = b"a\x1fa\x1f\x1f0\x1fAnn\x1f".to_vec();
            record.extend_from_slice(subject);
            parse_row(&record)
        };
        assert!(!record(b"fixup! A").subject_altered);
        assert!(!record("caf\u{e9}".as_bytes()).subject_altered);
        for raw in [&b" fixup! A"[..], b"A\x01", b"A\x1b[1m", b"A\xff"] {
            let row = record(raw);
            assert!(row.subject_altered, "{raw:?} became {:?}", row.subject);
        }
        assert_eq!(record(b" fixup! A").subject, "fixup! A");
    }

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
            .envs(crate::runner::isolation_env())
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

        let page = log_page(&runner, None, 0, 10, &LogQuery::default()).unwrap();

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
        let all = log_page(&runner, None, 0, 100, &LogQuery::default())
            .unwrap()
            .rows;
        let tip = all[0].oid.clone();
        // HEAD moves on; the fixed tip keeps the pages on the old history.
        commit(dir, "later", "Ann", 5_000);

        let mut paged = Vec::new();
        let mut skip = 0;
        loop {
            let page = log_page(&runner, Some(&tip), skip, 3, &LogQuery::default()).unwrap();
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
    fn pages_of_a_query_from_its_newest_match_concatenate_to_the_whole_query() {
        let tmp = repo("log-query-pages");
        let dir = tmp.path();
        let touch = |file: &str, text: &str, author: &str, time: i64| {
            std::fs::write(dir.join(file), text).unwrap();
            git(dir, &["add", "-A"]);
            commit(dir, &format!("{file} {text}"), author, time);
        };
        for i in 0..8 {
            touch("a.txt", &format!("a{i}\n").repeat(9), "Ann", 1_000 + i);
            touch("b.txt", &format!("b{i}"), "Bob", 2_000 + i);
        }
        git(dir, &["mv", "a.txt", "c.txt"]);
        commit(dir, "rename", "Ann", 3_000);
        touch("c.txt", &"a3\nz\n".repeat(9), "Ann", 4_000);
        touch("b.txt", "last", "Bob", 5_000);
        let runner = CliRunner::isolated(dir);
        let queries = [
            LogQuery {
                path: Some(RepoPath::from("c.txt")),
                author: None,
            },
            author("bob"),
        ];

        for query in &queries {
            let all = log_page(&runner, None, 0, 100, query).unwrap().rows;
            assert!(all.len() >= 9, "{query:?}: {all:?}");
            let tip = all[0].oid.clone();
            let mut paged = Vec::new();
            let mut skip = 0;
            let mut calls = 0u32;
            loop {
                let page = log_page(&runner, Some(&tip), skip, 2, query).unwrap();
                calls += 1;
                skip += page.rows.len();
                paged.extend(page.rows);
                if page.done {
                    break;
                }
            }
            assert_eq!(paged, all, "{query:?}");
            if query.path.is_some() {
                // ceil(log2(n / 2)) + 2: path pages grow instead of
                // staying at the limit.
                let bound = (all.len() / 2).next_power_of_two().trailing_zeros() + 2;
                assert!(calls <= bound, "{calls} pages for {} rows", all.len());
            }
        }
    }

    #[test]
    fn an_unborn_repo_gives_an_empty_finished_page() {
        let tmp = repo("log-unborn");
        let runner = CliRunner::isolated(tmp.path());
        assert_eq!(
            log_page(&runner, None, 0, 200, &LogQuery::default()),
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
        use crate::fake::{FakeRunner, ok};

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

        let page = log_page(&fake, None, 0, 3, &LogQuery::default()).unwrap();

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
        use crate::fake::FakeRunner;

        let fake = FakeRunner::new();
        for tip in ["--output=x", "", "HEAD~1", "abc def", "abc\u{1b}"] {
            assert!(
                log_page(&fake, Some(tip), 0, 10, &LogQuery::default()).is_err(),
                "{tip:?}"
            );
        }
        fake.assert_done();
    }

    /// The argv of a first-page log with `query`, as `log_page` runs it.
    fn query_argv<'a>(query: &[&'a str], path: &[&'a str]) -> Vec<&'a str> {
        let mut argv = vec![
            "log",
            "--no-show-signature",
            "--no-color",
            "-z",
            FORMAT,
            "--skip=0",
            "-n",
            "5",
        ];
        argv.extend(query);
        argv.extend(["HEAD", "--"]);
        argv.extend(path);
        argv
    }

    fn author(text: &str) -> LogQuery {
        LogQuery {
            path: None,
            author: Some(text.into()),
        }
    }

    #[test]
    fn an_author_query_is_one_glued_argument_matched_as_fixed_text_with_smart_case() {
        use crate::fake::{FakeRunner, ok};

        let lower = query_argv(
            &["--author=ann", "--fixed-strings", "--regexp-ignore-case"],
            &[],
        );
        let upper = query_argv(&["--author=Ann", "--fixed-strings"], &[]);
        let hostile = query_argv(
            &[
                "--author=--output=/tmp/x",
                "--fixed-strings",
                "--regexp-ignore-case",
            ],
            &[],
        );
        let fake = FakeRunner::new()
            .expect(&lower, ok(b""))
            .expect(&upper, ok(b""))
            .expect(&hostile, ok(b""));

        for text in ["ann", "Ann", "--output=/tmp/x"] {
            let page = log_page(&fake, None, 0, 5, &author(text)).unwrap();
            assert!(page.done && page.rows.is_empty(), "{text}");
        }
        fake.assert_done();
        // No path, so literal pathspecs are a no-op: the whole profile.
        assert_eq!(fake.process_envs(), vec![env_list(&STANDARD_ENV); 3]);
    }

    #[test]
    fn a_path_query_follows_renames_and_names_the_path_only_after_the_separator() {
        use crate::fake::{FakeRunner, ok};

        let fake = FakeRunner::new()
            .expect(&query_argv(&["--follow"], &["src/a.rs"]), ok(b""))
            .expect(&query_argv(&["--follow"], &["-p"]), ok(b""));
        for path in ["src/a.rs", "-p"] {
            let query = LogQuery {
                path: Some(RepoPath::from(path)),
                author: None,
            };
            log_page(&fake, None, 0, 5, &query).unwrap();
        }
        fake.assert_done();
        assert_eq!(fake.process_envs(), vec![env_list(&STANDARD_ENV); 2]);
    }

    #[test]
    fn a_later_page_of_a_path_query_reads_from_the_top_and_drops_what_was_shown() {
        use crate::fake::{FakeRunner, ok};

        let mut argv = query_argv(&["--follow"], &["a.rs"]);
        argv[7] = "4";
        argv[9] = "abc";
        let fake = FakeRunner::new().expect(
            &argv,
            ok(b"a1\x1fa1\x1f\x1f1\x1fA\x1fone\0\
                 b1\x1fb1\x1f\x1f1\x1fA\x1ftwo\0\
                 c1\x1fc1\x1f\x1f1\x1fA\x1fthree\0"),
        );
        let query = LogQuery {
            path: Some(RepoPath::from("a.rs")),
            author: None,
        };

        let page = log_page(&fake, Some("abc"), 2, 2, &query).unwrap();

        let subjects: Vec<&str> = page.rows.iter().map(|r| r.subject.as_str()).collect();
        assert_eq!(subjects, ["three"]);
        assert!(page.done);
        fake.assert_done();
    }

    #[test]
    fn path_query_pages_read_ahead_as_far_as_already_loaded_and_drop_what_was_shown() {
        use crate::fake::{FakeRunner, ok};

        let record = |i: usize| format!("{i:02x}\x1f{i:02x}\x1f\x1f1\x1fA\x1fs{i}\0");
        let history = |n: usize| (0..n).map(record).collect::<String>().into_bytes();
        let argv = |n: &'static str| {
            let mut argv = query_argv(&["--follow"], &["a.rs"]);
            argv[7] = n;
            argv[9] = "abc";
            argv
        };
        // Seven matching commits: git returns at most what `-n` asks for.
        let fake = FakeRunner::new()
            .expect(&argv("2"), ok(&history(2)))
            .expect(&argv("4"), ok(&history(4)))
            .expect(&argv("8"), ok(&history(7)));
        let query = LogQuery {
            path: Some(RepoPath::from("a.rs")),
            author: None,
        };

        let mut loaded = Vec::new();
        let mut pages = Vec::new();
        loop {
            let page = log_page(&fake, Some("abc"), loaded.len(), 2, &query).unwrap();
            let subjects: Vec<String> = page.rows.iter().map(|r| r.subject.clone()).collect();
            pages.push((subjects, page.done));
            loaded.extend(page.rows);
            if page.done {
                break;
            }
        }

        let page = |s: &[&str], done: bool| (s.iter().map(|s| s.to_string()).collect(), done);
        assert_eq!(
            pages,
            vec![
                page(&["s0", "s1"], false),
                page(&["s2", "s3"], false),
                page(&["s4", "s5", "s6"], true),
            ]
        );
        fake.assert_done();
        assert_eq!(fake.process_envs(), vec![env_list(&STANDARD_ENV); 3]);
    }

    #[test]
    fn a_path_reaches_argv_as_its_raw_bytes() {
        // Windows file names are UTF-16, so a name that is not UTF-8 cannot exist there.
        #[cfg(unix)]
        let name: &[u8] = b"x\xff.rs";
        #[cfg(not(unix))]
        let name: &[u8] = "x\u{e9}.rs".as_bytes();
        let query = LogQuery {
            path: Some(RepoPath::from_bytes(name)),
            author: Some("ann".into()),
        };
        let cmd = log_command(Some("abc"), 200, 200, &query);
        let args = cmd.args();
        let sep = args.iter().position(|a| a == "--").unwrap();
        assert_eq!(args[sep + 1].as_encoded_bytes(), name);
        assert_eq!(args.len(), sep + 2);
        let follow = args.iter().position(|a| a == "--follow").unwrap();
        let tip = args.iter().position(|a| a == "abc").unwrap();
        assert!(follow < tip && tip < sep);
        // The builder names its profile itself, literal pathspecs included.
        assert_eq!(
            cmd.process_env(),
            crate::fake::env_list(&crate::fake::STANDARD_ENV)
        );
    }

    #[test]
    fn no_log_query_uses_end_of_options() {
        let queries = [
            LogQuery::default(),
            author("ann"),
            LogQuery {
                path: Some(RepoPath::from("-p")),
                author: Some("--end-of-options".into()),
            },
        ];
        for query in &queries {
            for tip in [None, Some("abc")] {
                let cmd = log_command(tip, 0, 5, query);
                assert!(!cmd.args().iter().any(|a| a == "--end-of-options"));
            }
        }
    }

    #[test]
    fn typed_query_text_is_trimmed_blank_is_none_and_control_chars_are_refused() {
        assert_eq!(LogQuery::author_from("  ann "), Ok(Some("ann".into())));
        assert_eq!(LogQuery::author_from("   "), Ok(None));
        assert_eq!(LogQuery::author_from(""), Ok(None));
        for bad in ["a\0b", "a\x1bb", "a\nb", "\u{7f}"] {
            assert!(LogQuery::author_from(bad).is_err(), "{bad:?}");
            assert!(LogQuery::path_from(bad).is_err(), "{bad:?}");
        }
        assert_eq!(LogQuery::path_from(" "), Ok(None));
        assert_eq!(
            LogQuery::path_from(" src/a.rs "),
            Ok(Some(RepoPath::from("src/a.rs")))
        );
        assert_eq!(LogQuery::path_from("src/"), Ok(Some(RepoPath::from("src"))));
        assert_eq!(
            LogQuery::path_from("src//"),
            Ok(Some(RepoPath::from("src/"))),
            "only one slash"
        );
        assert!(!LogQuery::default().is_active());
        assert!(author("a").is_active());
    }

    #[test]
    fn a_branch_resolves_through_its_full_ref_to_a_commit() {
        use crate::fake::{FakeRunner, fail, ok};

        let argv = ["rev-parse", "-q", "--verify", "refs/heads/-x^{commit}"];
        let fake = FakeRunner::new()
            .expect(&argv, ok(b"abc123\n"))
            .expect(&argv, ok(b"not hex\n"))
            .expect(&argv, fail(""));
        let name = BranchName::from_bytes(b"-x");

        assert_eq!(
            branch_commit(&fake, &name).map(|id| id.as_str().to_string()),
            Ok("abc123".to_string())
        );
        assert!(branch_commit(&fake, &name).is_err());
        assert!(branch_commit(&fake, &name).is_err());
        fake.assert_done();
    }

    #[test]
    fn every_query_is_lock_free_and_in_the_c_locale() {
        use crate::fake::{FakeRunner, ok};

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
        let page = log_page(&fake, None, 0, 2, &LogQuery::default()).unwrap();
        assert!(page.done);
        assert_eq!(page.rows[0].time, 5);
        fake.assert_done();
        assert_eq!(fake.process_envs(), [env_list(&STANDARD_ENV)]);
    }
}
