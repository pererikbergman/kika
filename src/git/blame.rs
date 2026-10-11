//! Who last changed each line of a file, and the commit that did it.

use std::collections::HashMap;

use super::diff::{Classifier, DiffLine, sanitize, terminal_safe};
use super::path::RepoPath;
use super::runner::{GitCommand, GitRunner};

/// One line of the blamed file and the commit that last changed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameLine {
    pub oid: String,
    pub author: String,
    /// `YYYY-MM-DD` in the author's own timezone, as they saw the day.
    pub date: String,
    /// 1-based, in the blamed file.
    pub line_no: usize,
    pub text: String,
}

impl BlameLine {
    /// Git blames lines it has no commit for, edited in the worktree, on
    /// the all-zero oid.
    pub fn is_uncommitted(&self) -> bool {
        !self.oid.is_empty() && self.oid.bytes().all(|b| b == b'0')
    }
}

/// `git blame` of the file's working-tree contents, in the stable
/// porcelain form. The name goes in as raw bytes and is never read as a
/// pathspec, so a hostile `:(top)x` or `x*` names only itself.
pub fn blame_command(path: &RepoPath) -> GitCommand {
    GitCommand::new("blame")
        .arg("--porcelain")
        .arg("--")
        .arg(path.as_os_str())
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .env("GIT_LITERAL_PATHSPECS", "1")
}

/// What porcelain output says about a commit. Git prints it only on the
/// commit's first line, so it is kept for the lines after.
#[derive(Default)]
struct CommitInfo {
    author: String,
    time: Option<i64>,
    tz: Option<i64>,
}

/// Parse `git blame --porcelain`. Each line is a `<oid> <orig> <final>
/// [<count>]` header, then `key value` rows (only on a commit's first
/// line), then the line itself after a tab.
pub fn parse_blame(out: &[u8]) -> Result<Vec<BlameLine>, String> {
    let mut commits: HashMap<String, CommitInfo> = HashMap::new();
    let mut lines = Vec::new();
    let mut current: Option<(String, usize)> = None;
    for row in out.split(|&b| b == b'\n') {
        if let Some(text) = row.strip_prefix(b"\t") {
            let (oid, line_no) = current
                .take()
                .ok_or("Could not read git blame's output: a line without a commit")?;
            let info = commits.get(&oid);
            lines.push(BlameLine {
                author: info.map(|i| i.author.clone()).unwrap_or_default(),
                date: info.map(CommitInfo::date).unwrap_or_default(),
                oid,
                line_no,
                text: sanitize(text),
            });
            continue;
        }
        if row.is_empty() {
            continue;
        }
        if let Some((oid, _)) = &current {
            let info = commits.entry(oid.clone()).or_default();
            let (key, value) = split_once(row, b' ');
            match key {
                b"author" => info.author = sanitize(value),
                b"author-time" => info.time = ascii(value).and_then(|v| v.parse().ok()),
                b"author-tz" => info.tz = ascii(value).and_then(tz_seconds),
                _ => {}
            }
            continue;
        }
        current = Some(header(row)?);
    }
    if current.is_some() {
        return Err("Could not read git blame's output: it ended mid-line".into());
    }
    Ok(lines)
}

impl CommitInfo {
    fn date(&self) -> String {
        match self.time {
            Some(time) => ymd(time + self.tz.unwrap_or(0)),
            None => String::new(),
        }
    }
}

/// `<40 hex> <orig line> <final line> [<count>]` to the oid and final line.
fn header(row: &[u8]) -> Result<(String, usize), String> {
    let bad = || "Could not read git blame's output: a bad line header".to_string();
    let text = ascii(row).ok_or_else(bad)?;
    let mut fields = text.split(' ');
    let oid = fields.next().ok_or_else(bad)?;
    if oid.len() < 40 || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad());
    }
    let line_no = fields.nth(1).and_then(|n| n.parse().ok()).ok_or_else(bad)?;
    Ok((oid.to_string(), line_no))
}

fn split_once(row: &[u8], sep: u8) -> (&[u8], &[u8]) {
    match row.iter().position(|&b| b == sep) {
        Some(i) => (&row[..i], &row[i + 1..]),
        None => (row, &[]),
    }
}

fn ascii(bytes: &[u8]) -> Option<&str> {
    std::str::from_utf8(bytes).ok().filter(|s| s.is_ascii())
}

/// `+0200` or `-0930` to seconds east of UTC.
fn tz_seconds(tz: &str) -> Option<i64> {
    let (sign, digits) = match tz.as_bytes().first()? {
        b'+' => (1, &tz[1..]),
        b'-' => (-1, &tz[1..]),
        _ => return None,
    };
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let hours: i64 = digits[..2].parse().ok()?;
    let minutes: i64 = digits[2..].parse().ok()?;
    Some(sign * (hours * 3600 + minutes * 60))
}

/// Seconds since the epoch, already shifted to local time, as `YYYY-MM-DD`.
/// The proleptic Gregorian calendar, worked out by hand to avoid a date crate.
fn ymd(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400) + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Blame `path` as it is in the worktree. Errors are made safe to draw,
/// since git's message can quote repo-controlled paths.
pub fn read_blame(runner: &dyn GitRunner, path: &RepoPath) -> Result<Vec<BlameLine>, String> {
    let stdout = run(runner, &blame_command(path), "git blame failed")?;
    parse_blame(&stdout)
}

/// One commit's message and full diff, in plain text we classify ourselves.
pub fn show_command(oid: &str) -> GitCommand {
    GitCommand::new("show")
        .arg("--no-color")
        .arg("--no-ext-diff")
        .arg("--pretty=medium")
        .arg("--date=iso")
        .arg(oid)
        .arg("--")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
}

/// The commit `oid` as diff lines: everything before the first `@@`, the
/// message included, comes out as a header.
pub fn read_commit(runner: &dyn GitRunner, oid: &str) -> Result<Vec<DiffLine>, String> {
    let stdout = run(runner, &show_command(oid), "git show failed")?;
    let body = stdout.strip_suffix(b"\n").unwrap_or(&stdout);
    if body.is_empty() {
        return Ok(vec![]);
    }
    let mut classifier = Classifier::new();
    Ok(body
        .split(|&b| b == b'\n')
        .map(|line| classifier.classify(line))
        .collect())
}

/// Run `cmd` and return its stdout, or git's message, made safe, on failure.
fn run(runner: &dyn GitRunner, cmd: &GitCommand, failed: &str) -> Result<Vec<u8>, String> {
    let output = runner
        .run(cmd)
        .map_err(|e| terminal_safe(e.to_string().as_bytes()))?;
    if !output.success {
        let message = terminal_safe(&output.stderr);
        return Err(if message.is_empty() {
            failed.into()
        } else {
            message
        });
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::diff::LineKind;
    use crate::git::fake::{FakeRunner, fail, ok};

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    /// Two commits, each repeated with only its header line the second
    /// time. A's day is 2023-11-14 in UTC but 2023-11-15 at +0200; B's
    /// is 2020-01-01 in UTC but 2019-12-31 at -0500.
    fn fixture() -> Vec<u8> {
        format!(
            "{A} 1 1 2\n\
             author Ann \x1b[31mA\n\
             author-mail <ann@a>\n\
             author-time 1700000000\n\
             author-tz +0200\n\
             committer Ann\n\
             committer-mail <ann@a>\n\
             committer-time 1700000000\n\
             committer-tz +0200\n\
             summary first\n\
             boundary\n\
             filename a.txt\n\
             \tone\n\
             {A} 2 2\n\
             \ttwo\x1b[2J\n\
             {B} 1 3 1\n\
             author Bob\n\
             author-mail <bob@b>\n\
             author-time 1577840400\n\
             author-tz -0500\n\
             committer Bob\n\
             committer-mail <bob@b>\n\
             committer-time 1577840400\n\
             committer-tz -0500\n\
             summary second\n\
             previous {A} a.txt\n\
             filename a.txt\n\
             \tthree\twith tab\n\
             {A} 3 4 1\n\
             filename a.txt\n\
             \tfour\n"
        )
        .into_bytes()
    }

    fn line(oid: &str, author: &str, date: &str, line_no: usize, text: &str) -> BlameLine {
        BlameLine {
            oid: oid.into(),
            author: author.into(),
            date: date.into(),
            line_no,
            text: text.into(),
        }
    }

    #[test]
    fn every_line_gets_its_commit_author_local_date_and_sanitized_text() {
        let ann = "Ann \u{FFFD}[31mA";
        assert_eq!(
            parse_blame(&fixture()),
            Ok(vec![
                line(A, ann, "2023-11-15", 1, "one"),
                line(A, ann, "2023-11-15", 2, "two\u{FFFD}[2J"),
                line(B, "Bob", "2019-12-31", 3, "three    with tab"),
                line(A, ann, "2023-11-15", 4, "four"),
            ])
        );
    }

    #[test]
    fn empty_output_is_an_empty_file_and_garbage_is_an_error() {
        assert_eq!(parse_blame(b""), Ok(vec![]));
        assert!(parse_blame(b"\torphan line\n").is_err());
        assert!(parse_blame(b"nothex 1 1 1\n\tx\n").is_err());
    }

    #[test]
    fn dates_before_1970_and_half_hour_zones_are_worked_out() {
        let out = format!(
            "{A} 1 1 1\nauthor X\nauthor-time -86400\nauthor-tz +0530\n\tx\n\
             {B} 1 2 1\nauthor Y\nauthor-time 1700000000\nauthor-tz -0930\n\ty\n"
        );
        let lines = parse_blame(out.as_bytes()).unwrap();
        assert_eq!(lines[0].date, "1969-12-31");
        assert_eq!(lines[1].date, "2023-11-14");
    }

    #[test]
    fn the_all_zero_oid_is_uncommitted() {
        let zero = "0".repeat(40);
        assert!(line(&zero, "", "", 1, "").is_uncommitted());
        assert!(!line(A, "", "", 1, "").is_uncommitted());
    }

    fn env(cmd: &GitCommand) -> Vec<(String, String)> {
        strings(cmd.envs())
    }

    fn strings(envs: &[(std::ffi::OsString, std::ffi::OsString)]) -> Vec<(String, String)> {
        envs.iter()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
            .collect()
    }

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn read_blame_runs_porcelain_blame_on_the_raw_path_with_literal_pathspecs() {
        let runner = FakeRunner::new().expect(
            &["blame", "--porcelain", "--", ":(top)a*.txt"],
            ok(&fixture()),
        );
        let lines = read_blame(&runner, &":(top)a*.txt".into()).unwrap();
        assert_eq!(lines.len(), 4);
        runner.assert_done();
        assert_eq!(
            strings(&runner.envs()[0]),
            pairs(&[
                ("GIT_OPTIONAL_LOCKS", "0"),
                ("LC_ALL", "C"),
                ("GIT_LITERAL_PATHSPECS", "1"),
            ])
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_blame_argv_carries_the_raw_name_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let cmd = blame_command(&RepoPath::from_bytes(b"x\xff"));
        assert_eq!(cmd.args().last().unwrap().as_bytes(), b"x\xff");
    }

    #[test]
    fn a_failed_blame_returns_gits_message_made_safe() {
        const ARGV: &[&str] = &["blame", "--porcelain", "--", "a.txt"];
        let runner = FakeRunner::new().expect(ARGV, fail("fatal: no such \x1b[31mpath\n"));
        assert_eq!(
            read_blame(&runner, &"a.txt".into()),
            Err("fatal: no such \u{FFFD}[31mpath".into())
        );
        let runner = FakeRunner::new().expect(ARGV, fail(""));
        assert_eq!(
            read_blame(&runner, &"a.txt".into()),
            Err("git blame failed".into())
        );
    }

    #[test]
    fn a_real_blame_names_each_lines_author_and_marks_uncommitted_edits() {
        use crate::git::fake::{TempDir, git};

        let tmp = TempDir::new("blame");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["-c", "user.name=Ann", "commit", "-q", "-m", "first"]);
        std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        git(
            dir,
            &["-c", "user.name=Bob", "commit", "-q", "-am", "second"],
        );
        std::fs::write(dir.join("a.txt"), "one\nTWO\nthree\n").unwrap();

        let runner = crate::git::CliRunner::isolated(dir);
        let lines = read_blame(&runner, &"a.txt".into()).unwrap();
        let seen: Vec<(&str, bool, usize, &str)> = lines
            .iter()
            .map(|l| {
                (
                    l.author.as_str(),
                    l.is_uncommitted(),
                    l.line_no,
                    l.text.as_str(),
                )
            })
            .collect();
        assert_eq!(seen[0], ("Ann", false, 1, "one"));
        assert_eq!((seen[1].1, seen[1].2, seen[1].3), (true, 2, "TWO"));
        assert_eq!(seen[2], ("Bob", false, 3, "three"));
        assert_eq!(lines[0].date.len(), "YYYY-MM-DD".len());
    }

    const SHOW: &[&str] = &[
        "show",
        "--no-color",
        "--no-ext-diff",
        "--pretty=medium",
        "--date=iso",
        A,
        "--",
    ];

    #[test]
    fn read_commit_shows_the_header_then_classified_hunk_lines() {
        let out = format!(
            "commit {A}\n\
             Author: Ann <ann@a>\n\
             Date:   2023-11-15 00:13:20 +0200\n\
             \n    Why it \x1b[2Jchanged\n\n\
             diff --git a/a.txt b/a.txt\n\
             --- a/a.txt\n\
             +++ b/a.txt\n\
             @@ -1 +1 @@\n\
             -old\n\
             +new\n"
        );
        let runner = FakeRunner::new().expect(SHOW, ok(out.as_bytes()));
        let lines = read_commit(&runner, A).unwrap();
        runner.assert_done();
        assert_eq!(
            env(&show_command(A)),
            pairs(&[("GIT_OPTIONAL_LOCKS", "0"), ("LC_ALL", "C")])
        );
        let kinds: Vec<(LineKind, &str)> =
            lines.iter().map(|l| (l.kind, l.text.as_str())).collect();
        use LineKind::*;
        assert_eq!(
            kinds,
            [
                (Header, format!("commit {A}").as_str()),
                (Header, "Author: Ann <ann@a>"),
                (Header, "Date:   2023-11-15 00:13:20 +0200"),
                (Header, ""),
                (Header, "    Why it \u{FFFD}[2Jchanged"),
                (Header, ""),
                (Header, "diff --git a/a.txt b/a.txt"),
                (Header, "--- a/a.txt"),
                (Header, "+++ b/a.txt"),
                (Hunk, "@@ -1 +1 @@"),
                (Removed, "-old"),
                (Added, "+new"),
            ]
        );
    }

    #[test]
    fn a_failed_show_returns_gits_message_made_safe() {
        let runner = FakeRunner::new().expect(SHOW, fail("fatal: bad object\n"));
        assert_eq!(read_commit(&runner, A), Err("fatal: bad object".into()));
        let runner = FakeRunner::new().expect(SHOW, fail(""));
        assert_eq!(read_commit(&runner, A), Err("git show failed".into()));
    }
}
