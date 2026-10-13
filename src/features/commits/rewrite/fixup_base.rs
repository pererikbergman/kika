//! Which commit a staged change fixes: the commits that last touched the
//! lines it changes, by `git blame` of `HEAD`. Read only; the parsing and
//! the choice are pure.

use crate::git::blame::parse_blame;
use crate::git::commit_files::CommitId;
use crate::git::diff::EmojiZwj;
use crate::git::path::RepoPath;
use crate::git::runner::{GitCommand, GitRunner};

/// The commits the staged lines come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixupBase {
    /// One commit: the fixup's base.
    One(CommitId),
    /// Several, in first-seen order; choosing one would be a guess.
    Many(Vec<CommitId>),
    /// None, as when only new files are staged.
    None,
}

/// The lines of `HEAD`'s version of a file one staged hunk replaces:
/// `len` lines from `start`, 1-based. With `len` 0 the hunk only adds,
/// after line `start` (0 for the top of the file).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OldRange {
    pub start: u32,
    pub len: u32,
}

/// One staged file that `HEAD` has, and the ranges of it the stage changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedFile {
    pub path: RepoPath,
    pub hunks: Vec<OldRange>,
}

/// The staged change against `HEAD`, with no context lines, so each hunk
/// covers only changed lines. Every option exists in git 2.15. The
/// prefixes are set so `diff.noprefix` and `diff.mnemonicPrefix` cannot
/// change the names, and textconv is off so line numbers are the file's.
/// A read that names no paths, so literal pathspecs change nothing.
pub fn staged_diff_command() -> GitCommand {
    GitCommand::new("diff")
        .arg("--cached")
        .arg("-U0")
        .arg("--no-color")
        .arg("--no-ext-diff")
        .arg("--no-textconv")
        .arg("--no-renames")
        .arg("--ignore-submodules")
        .arg("--src-prefix=a/")
        .arg("--dst-prefix=b/")
        .standard_env()
}

/// `git blame` of `path` as `HEAD` has it. The name goes in as raw bytes
/// after `--` and is never read as a pathspec.
pub fn head_blame_command(path: &RepoPath) -> GitCommand {
    GitCommand::new("blame")
        .arg("--porcelain")
        .arg("HEAD")
        .arg("--")
        .arg(path.as_os_str())
        .standard_env()
}

/// The files of [`staged_diff_command`]'s output that `HEAD` has, each
/// with its hunks' old ranges. A new file (`--- /dev/null`) is left out,
/// since no commit touched it; so is a file with no hunks, such as a
/// binary or a mode change.
pub fn parse_staged(diff: &[u8]) -> Result<Vec<StagedFile>, String> {
    let bad = |what: &str| format!("Could not read git diff's output: {what}");
    let mut files: Vec<StagedFile> = Vec::new();
    // The old name of the file being read, until its first hunk; `None`
    // once the file is new or done with its header.
    let mut old: Option<Option<RepoPath>> = None;
    let mut in_hunks = false;
    for line in diff.split(|&b| b == b'\n') {
        if line.starts_with(b"diff ") {
            old = Some(None);
            in_hunks = false;
            continue;
        }
        if !in_hunks && let Some(name) = line.strip_prefix(b"--- ") {
            let path = old_name(name).ok_or_else(|| bad("a bad file name"))?;
            old = Some(path.map(|p| RepoPath::from_bytes(&p)));
            continue;
        }
        if line.starts_with(b"@@") {
            let Some(file) = &old else {
                return Err(bad("a hunk before its file"));
            };
            let range = old_range(line).ok_or_else(|| bad("a bad hunk header"))?;
            if !in_hunks {
                in_hunks = true;
                if let Some(path) = file {
                    files.push(StagedFile {
                        path: path.clone(),
                        hunks: Vec::new(),
                    });
                }
            }
            if file.is_some()
                && let Some(staged) = files.last_mut()
            {
                staged.hunks.push(range);
            }
        }
    }
    Ok(files)
}

/// The name after `--- `: `None` for `/dev/null`, else the bytes after
/// the `a/` prefix, unquoted. Git puts a tab after a name with a space.
fn old_name(name: &[u8]) -> Option<Option<Vec<u8>>> {
    let name = name.strip_suffix(b"\t").unwrap_or(name);
    if name == b"/dev/null" {
        return Some(None);
    }
    let name = match name.first() {
        Some(b'"') => unquote(name)?,
        _ => name.to_vec(),
    };
    name.strip_prefix(b"a/").map(|n| Some(n.to_vec()))
}

/// A name as git C-quotes it: in double quotes, with `\` escapes for
/// `\a\b\t\n\v\f\r\"\\` and three octal digits for any other byte.
fn unquote(quoted: &[u8]) -> Option<Vec<u8>> {
    let inner = quoted.strip_prefix(b"\"")?.strip_suffix(b"\"")?;
    let mut out = Vec::with_capacity(inner.len());
    let mut bytes = inner.iter().copied();
    while let Some(b) = bytes.next() {
        if b == b'"' {
            return None;
        }
        if b != b'\\' {
            out.push(b);
            continue;
        }
        let escaped = match bytes.next()? {
            b'a' => 0x07,
            b'b' => 0x08,
            b't' => b'\t',
            b'n' => b'\n',
            b'v' => 0x0b,
            b'f' => 0x0c,
            b'r' => b'\r',
            b'"' => b'"',
            b'\\' => b'\\',
            first @ b'0'..=b'3' => {
                let digit = |d: u8| (b'0'..=b'7').contains(&d).then(|| d - b'0');
                let (second, third) = (digit(bytes.next()?)?, digit(bytes.next()?)?);
                ((first - b'0') << 6) | (second << 3) | third
            }
            _ => return None,
        };
        out.push(escaped);
    }
    Some(out)
}

/// The old side of `@@ -a[,b] +c[,d] @@`.
fn old_range(line: &[u8]) -> Option<OldRange> {
    let rest = line.strip_prefix(b"@@ -")?;
    let end = rest.iter().position(|&b| b == b' ')?;
    let text = std::str::from_utf8(&rest[..end]).ok()?;
    let number = |s: &str| {
        s.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| s.parse().ok())
            .flatten()
    };
    let (start, len) = match text.split_once(',') {
        Some((start, len)) => (number(start)?, number(len)?),
        None => (number(text)?, 1),
    };
    Some(OldRange { start, len })
}

/// The commit of each line of `HEAD`'s version of a file, from `git blame
/// --porcelain HEAD`: index 0 is line 1.
pub fn parse_line_commits(blame: &[u8]) -> Result<Vec<CommitId>, String> {
    let lines = parse_blame(blame, EmojiZwj::default())?;
    let mut commits = vec![None; lines.len()];
    for line in lines {
        let slot = line
            .line_no
            .checked_sub(1)
            .and_then(|i| commits.get_mut(i))
            .ok_or("Could not read git blame's output: a line out of place")?;
        *slot = CommitId::parse(&line.oid);
    }
    commits
        .into_iter()
        .map(|c| c.ok_or_else(|| "Could not read git blame's output: a line missing".into()))
        .collect()
}

/// The commits the staged hunks come from: for each file, `hunks` with
/// `blames`, the commit of each of its `HEAD` lines. A hunk that removes
/// or changes lines takes their commits; one that only adds takes the
/// commits of the lines either side of it, as the change belongs with
/// them. Distinct, in first-seen order.
pub fn suggest(files: &[(Vec<OldRange>, Vec<CommitId>)]) -> FixupBase {
    let mut found: Vec<CommitId> = Vec::new();
    for (hunks, blames) in files {
        for hunk in hunks {
            let (start, len) = (hunk.start as usize, hunk.len as usize);
            let lines = if len > 0 {
                start..start + len
            } else {
                start.max(1)..start + 2
            };
            for line in lines {
                let Some(commit) = line.checked_sub(1).and_then(|i| blames.get(i)) else {
                    continue;
                };
                let uncommitted = commit.as_str().bytes().all(|b| b == b'0');
                if !uncommitted && !found.contains(commit) {
                    found.push(commit.clone());
                }
            }
        }
    }
    match found.len() {
        0 => FixupBase::None,
        1 => FixupBase::One(found.remove(0)),
        _ => FixupBase::Many(found),
    }
}

/// The most short ids [`candidates_notice`] lists, so that the notice fits
/// an 80-column bar whole rather than being cut mid-list.
pub const MAX_LISTED: usize = 3;

/// What the bar says when the staged lines come from several commits:
/// how many, then the first few of `shorts` (the caller orders them newest
/// first), and how many more there are.
pub fn candidates_notice(shorts: &[String]) -> String {
    let listed = shorts[..shorts.len().min(MAX_LISTED)].join(", ");
    let more = match shorts.len().saturating_sub(MAX_LISTED) {
        0 => String::new(),
        n => format!(" +{n} more"),
    };
    format!(
        "Staged lines come from {} commits: {listed}{more}",
        shorts.len()
    )
}

/// Find the commits the staged change fixes: one diff, then one blame
/// per file `HEAD` has. Errors are made safe to draw, since git's message
/// can quote repo-controlled names.
pub fn find_fixup_base(runner: &dyn GitRunner) -> Result<FixupBase, String> {
    let diff = run(runner, &staged_diff_command())?;
    let mut files = Vec::new();
    for staged in parse_staged(&diff)? {
        let blame = run(runner, &head_blame_command(&staged.path))?;
        files.push((staged.hunks, parse_line_commits(&blame)?));
    }
    Ok(suggest(&files))
}

/// `cmd`'s stdout. A silent failure reads `git diff failed` or `git blame
/// failed`, after the subcommand.
fn run(runner: &dyn GitRunner, cmd: &GitCommand) -> Result<Vec<u8>, String> {
    runner
        .run_checked(cmd)
        .map(|output| output.stdout)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cid(digit: char) -> CommitId {
        CommitId::parse(&digit.to_string().repeat(40)).unwrap()
    }

    fn range(start: u32, len: u32) -> OldRange {
        OldRange { start, len }
    }

    /// A file of `HEAD` whose lines were last changed by these commits.
    fn blamed(digits: &str) -> Vec<CommitId> {
        digits.chars().map(cid).collect()
    }

    #[test]
    fn the_candidates_notice_lists_a_few_and_counts_the_rest() {
        let shorts = |n: usize| (0..n).map(|i| format!("abcdef{i}")).collect::<Vec<_>>();
        assert_eq!(
            candidates_notice(&shorts(2)),
            "Staged lines come from 2 commits: abcdef0, abcdef1"
        );
        assert_eq!(
            candidates_notice(&shorts(MAX_LISTED)),
            "Staged lines come from 3 commits: abcdef0, abcdef1, abcdef2"
        );
        let many = candidates_notice(&shorts(20));
        assert_eq!(
            many,
            "Staged lines come from 20 commits: abcdef0, abcdef1, abcdef2 +17 more"
        );
        // With room for the bar's padding at 80 columns.
        assert!(many.chars().count() <= 76, "{many}");
    }

    #[test]
    fn a_changed_line_takes_its_commit() {
        let files = [(vec![range(2, 1)], blamed("aba"))];
        assert_eq!(suggest(&files), FixupBase::One(cid('b')));
    }

    #[test]
    fn hunks_in_two_files_from_two_commits_give_both_in_order() {
        let files = [
            (vec![range(1, 1)], blamed("cc")),
            (vec![range(2, 1)], blamed("cbc")),
        ];
        assert_eq!(suggest(&files), FixupBase::Many(vec![cid('c'), cid('b')]));
    }

    #[test]
    fn a_pure_add_takes_the_lines_either_side() {
        // Added after line 2, between two lines of `b`.
        let between = [(vec![range(2, 0)], blamed("abbc"))];
        assert_eq!(suggest(&between), FixupBase::One(cid('b')));
        // At the top, only line 1 is beside it.
        let top = [(vec![range(0, 0)], blamed("ab"))];
        assert_eq!(suggest(&top), FixupBase::One(cid('a')));
        // At the end, only the last line.
        let end = [(vec![range(2, 0)], blamed("ab"))];
        assert_eq!(suggest(&end), FixupBase::One(cid('b')));
        // Between two commits, both.
        let seam = [(vec![range(1, 0)], blamed("ab"))];
        assert_eq!(suggest(&seam), FixupBase::Many(vec![cid('a'), cid('b')]));
    }

    #[test]
    fn nothing_staged_in_a_file_head_has_gives_none() {
        assert_eq!(suggest(&[]), FixupBase::None);
        assert_eq!(suggest(&[(vec![range(0, 0)], vec![])]), FixupBase::None);
    }

    #[test]
    fn only_a_new_file_staged_has_no_file_to_blame() {
        let diff = b"diff --git a/new.txt b/new.txt\n\
new file mode 100644\n\
index 0000000..3b18e51\n\
--- /dev/null\n\
+++ b/new.txt\n\
@@ -0,0 +1 @@\n\
+hello\n";
        assert_eq!(parse_staged(diff), Ok(vec![]));
        assert_eq!(suggest(&[]), FixupBase::None);
    }

    #[test]
    fn parse_staged_reads_each_file_s_old_ranges() {
        let diff = b"diff --git a/a.txt b/a.txt\n\
index 1..2 100644\n\
--- a/a.txt\n\
+++ b/a.txt\n\
@@ -2 +2 @@ fn x\n\
-old\n\
+new\n\
@@ -5,0 +6,2 @@\n\
+one\n\
+two\n\
diff --git a/b c.txt b/b c.txt\n\
deleted file mode 100644\n\
index 3..0\n\
--- a/b c.txt\t\n\
+++ /dev/null\n\
@@ -1,3 +0,0 @@\n\
--- not a header\n\
-x\n\
-y\n\
diff --git a/bin b/bin\n\
index 4..5 100644\n\
Binary files a/bin and b/bin differ\n";
        assert_eq!(
            parse_staged(diff),
            Ok(vec![
                StagedFile {
                    path: RepoPath::from("a.txt"),
                    hunks: vec![range(2, 1), range(5, 0)],
                },
                StagedFile {
                    path: RepoPath::from("b c.txt"),
                    hunks: vec![range(1, 3)],
                },
            ])
        );
    }

    #[test]
    fn quoted_names_are_read_back_to_their_raw_bytes() {
        let diff = b"diff --git \"a/t\\303\\244b\\t\\\"q\\\"\" \"b/t\\303\\244b\\t\\\"q\\\"\"\n\
--- \"a/t\\303\\244b\\t\\\"q\\\"\"\n\
+++ \"b/t\\303\\244b\\t\\\"q\\\"\"\n\
@@ -1 +1 @@\n\
-a\n\
+b\n";
        let files = parse_staged(diff).unwrap();
        assert_eq!(files[0].path.as_bytes(), "t\u{e4}b\t\"q\"".as_bytes());
        assert_eq!(unquote(b"\"a\\777\""), None);
        assert_eq!(unquote(b"\"a\\x\""), None);
        assert_eq!(unquote(b"\"a\"b\""), None);
    }

    #[test]
    fn a_bad_hunk_header_is_an_error() {
        let diff = b"diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -x +1 @@\n";
        assert!(parse_staged(diff).is_err());
    }

    #[test]
    fn every_command_is_argv_with_the_name_after_dashes_and_no_new_options() {
        let diff = staged_diff_command();
        let args: Vec<_> = diff.args().iter().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args,
            [
                "diff",
                "--cached",
                "-U0",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                "--no-renames",
                "--ignore-submodules",
                "--src-prefix=a/",
                "--dst-prefix=b/",
            ]
        );
        let path = RepoPath::from_bytes(b"-n\xff:(top)*");
        let blame = head_blame_command(&path);
        assert_eq!(blame.args()[..4], ["blame", "--porcelain", "HEAD", "--"]);
        assert_eq!(blame.args()[4].as_encoded_bytes(), b"-n\xff:(top)*");
        for cmd in [&diff, &blame] {
            assert!(!cmd.args().iter().any(|a| a == "--end-of-options"));
            assert_eq!(cmd.env_profile(), crate::git::runner::EnvProfile::Standard);
            assert_eq!(
                cmd.process_env(),
                [
                    ("LC_ALL".into(), "C".into()),
                    ("GIT_OPTIONAL_LOCKS".into(), "0".into()),
                    ("GIT_LITERAL_PATHSPECS".into(), "1".into())
                ]
            );
        }
    }

    #[test]
    fn a_failed_read_names_its_subcommand_or_shows_git_s_words_made_safe() {
        use crate::git::fake::{FakeRunner, fail, ok};
        let diff: Vec<String> = staged_diff_command()
            .args()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let diff: Vec<&str> = diff.iter().map(String::as_str).collect();
        let runner = FakeRunner::new().expect(&diff, fail(""));
        assert_eq!(find_fixup_base(&runner), Err("git diff failed".into()));

        let staged = b"diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-x\n+y\n";
        let blame = ["blame", "--porcelain", "HEAD", "--", "a"];
        let runner = FakeRunner::new()
            .expect(&diff, ok(staged))
            .expect(&blame, fail(""));
        assert_eq!(find_fixup_base(&runner), Err("git blame failed".into()));
        let runner = FakeRunner::new()
            .expect(&diff, ok(staged))
            .expect(&blame, fail("fatal: \x1b[2Jno\n"));
        assert_eq!(find_fixup_base(&runner), Err("fatal: \u{FFFD}[2Jno".into()));
        let path = RepoPath::from_bytes(b"a");
        assert_eq!(
            runner.process_envs(),
            [
                staged_diff_command().process_env(),
                head_blame_command(&path).process_env()
            ]
        );
    }

    #[test]
    fn blame_lines_map_to_their_commits() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let out = format!(
            "{a} 1 1 1\nauthor Ann\n\tone\n{b} 2 2 1\nauthor Bo\n\ttwo\n{a} 3 3\n\tthree\n"
        );
        assert_eq!(
            parse_line_commits(out.as_bytes()),
            Ok(vec![cid('a'), cid('b'), cid('a')])
        );
    }

    #[test]
    fn the_task_blames_a_staged_edit_of_a_line_the_second_commit_changed_in_real_git() {
        use crate::git::fake::{TempDir, git};

        let tmp = TempDir::new("fixup-base");
        let dir = tmp.path();
        git(dir, &["init", "-q", "--template="]);
        for (key, value) in [
            ("user.name", "Kika Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
        ] {
            git(dir, &["config", key, value]);
        }
        let name = "a \"b\".txt";
        std::fs::write(dir.join(name), "one\ntwo\nthree\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "first"]);
        std::fs::write(dir.join(name), "one\nTWO\nthree\n").unwrap();
        git(dir, &["commit", "-q", "-am", "second"]);
        let second = String::from_utf8(git(dir, &["rev-parse", "HEAD"])).unwrap();
        std::fs::write(dir.join(name), "one\nfixed\nthree\n").unwrap();
        std::fs::write(dir.join("new.txt"), "new\n").unwrap();
        git(dir, &["add", "-A"]);
        let runner = crate::git::CliRunner::isolated(dir);

        assert_eq!(
            find_fixup_base(&runner),
            Ok(FixupBase::One(CommitId::parse(second.trim()).unwrap()))
        );

        // Only the first commit's line changes now.
        git(dir, &["reset", "-q"]);
        std::fs::write(dir.join(name), "ONE\nTWO\nthree\n").unwrap();
        git(dir, &["add", "-A"]);
        let first = String::from_utf8(git(dir, &["rev-parse", "HEAD~1"])).unwrap();
        assert_eq!(
            find_fixup_base(&runner),
            Ok(FixupBase::One(CommitId::parse(first.trim()).unwrap()))
        );
    }
}
