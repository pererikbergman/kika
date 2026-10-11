use super::patch::{self, FilePatch};
use super::path::RepoPath;
use super::runner::{GitCommand, GitRunner};
use super::status::{EntryKind, FileEntry};

/// Which of a file's changes to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffSide {
    /// Unstaged changes: worktree against the index.
    Worktree,
    /// Staged changes: index against `HEAD`.
    Index,
    /// A new file, shown as all additions.
    Untracked,
}

/// One diff to load. Built from a status entry so the model never spells argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRequest {
    pub path: RepoPath,
    pub orig_path: Option<RepoPath>,
    pub side: DiffSide,
}

impl DiffRequest {
    /// Unstaged changes win when a file has both, since that is what the user
    /// is most likely editing; otherwise the staged diff is shown.
    pub fn for_entry(entry: &FileEntry) -> Self {
        let side = if entry.kind == EntryKind::Untracked {
            DiffSide::Untracked
        } else if entry.worktree != '.' {
            DiffSide::Worktree
        } else {
            DiffSide::Index
        };
        Self {
            path: entry.path.clone(),
            orig_path: entry.orig_path.clone(),
            side,
        }
    }
}

/// The `git diff` for a request. Colour and external diff drivers are off so
/// the output is plain text we classify ourselves.
pub fn diff_command(request: &DiffRequest) -> GitCommand {
    let mut cmd = GitCommand::new("diff")
        .arg("--no-color")
        .arg("--no-ext-diff");
    match request.side {
        DiffSide::Worktree => cmd = cmd.arg("--").arg(request.path.as_os_str()),
        DiffSide::Index => {
            cmd = cmd.arg("--cached").arg("-M").arg("--");
            if let Some(orig) = &request.orig_path {
                cmd = cmd.arg(orig.as_os_str());
            }
            cmd = cmd.arg(request.path.as_os_str());
        }
        DiffSide::Untracked => {
            cmd = cmd
                .arg("--no-index")
                .arg("--")
                .arg("/dev/null")
                .arg(request.path.as_os_str())
        }
    }
    // File names come from the repo, so a hostile `:(top)x` or `x*` must not
    // be read as pathspec magic or a glob. A no-op under `--no-index`.
    cmd.env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .env("GIT_LITERAL_PATHSPECS", "1")
}

/// The whole `git diff` of one file in the plain form the patch engine
/// parses: no colour, external drivers or textconv, renames off so one path
/// gives one file, and fixed `a/` `b/` prefixes whatever the user's config.
pub fn patch_diff_command(request: &DiffRequest) -> GitCommand {
    let mut cmd = GitCommand::new("diff")
        .arg("--no-color")
        .arg("--no-ext-diff")
        .arg("--no-textconv")
        .arg("--no-renames")
        .arg("-U3")
        .arg("--src-prefix=a/")
        .arg("--dst-prefix=b/");
    cmd = match request.side {
        DiffSide::Worktree => cmd.arg("--"),
        DiffSide::Index => cmd.arg("--cached").arg("--"),
        DiffSide::Untracked => cmd.arg("--no-index").arg("--").arg("/dev/null"),
    };
    cmd.arg(request.path.as_os_str())
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .env("GIT_LITERAL_PATHSPECS", "1")
}

/// Load and parse the file's whole diff for patch building. Errors are
/// made safe to draw, since git's message can quote repo-controlled paths.
pub fn load_patch(runner: &dyn GitRunner, request: &DiffRequest) -> Result<FilePatch, String> {
    let output = runner
        .run(&patch_diff_command(request))
        .map_err(|e| terminal_safe(e.to_string().as_bytes()))?;
    // `--no-index` exits 1 when the files differ, which is the point. The
    // runner reports no exit code; a real failure says why on stderr.
    let differs = request.side == DiffSide::Untracked
        && output.stderr.is_empty()
        && !output.stdout.is_empty();
    if !output.success && !differs {
        let message = terminal_safe(&output.stderr);
        return Err(if message.is_empty() {
            "git diff failed".into()
        } else {
            message
        });
    }
    patch::parse(&output.stdout).map_err(|e| e.to_string())
}

/// What a diff line is, which decides its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Header,
    Hunk,
    Added,
    Removed,
    Context,
}

/// One classified, terminal-safe diff line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub text: String,
}

/// A batch of lines from a streaming diff. `done` marks the last one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffChunk {
    pub lines: Vec<DiffLine>,
    pub done: bool,
}

/// Classifies raw diff lines in order. Stateful because `---`/`+++` mean
/// "file header" before the first `@@` and "removed/added" after it.
#[derive(Debug, Default)]
pub struct Classifier {
    in_hunk: bool,
}

impl Classifier {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn classify(&mut self, raw: &[u8]) -> DiffLine {
        let raw = raw.strip_suffix(b"\n").unwrap_or(raw);
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        if raw.starts_with(b"diff --git") {
            self.in_hunk = false;
        } else if raw.starts_with(b"@@") {
            self.in_hunk = true;
            return DiffLine {
                kind: LineKind::Hunk,
                text: sanitize(raw),
            };
        }
        let kind = if !self.in_hunk {
            LineKind::Header
        } else {
            match raw.first() {
                Some(b'+') => LineKind::Added,
                Some(b'-') => LineKind::Removed,
                _ => LineKind::Context,
            }
        };
        DiffLine {
            kind,
            text: sanitize(raw),
        }
    }
}

/// Decode lossily and make the text safe to draw: tabs become spaces and any
/// other control character becomes `U+FFFD`, so file bytes never drive the terminal.
pub fn sanitize(raw: &[u8]) -> String {
    let mut text = String::with_capacity(raw.len());
    for c in String::from_utf8_lossy(raw).chars() {
        match c {
            '\t' => text.push_str("    "),
            c if c.is_control() => text.push('\u{FFFD}'),
            c => text.push(c),
        }
    }
    text
}

/// git's message, line by line through the same sanitiser as diff lines,
/// since it can quote repo-controlled paths.
pub fn terminal_safe(stderr: &[u8]) -> String {
    stderr
        .trim_ascii()
        .split(|&b| b == b'\n')
        .map(|line| sanitize(line.strip_suffix(b"\r").unwrap_or(line)))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(index: char, worktree: char, kind: EntryKind) -> FileEntry {
        FileEntry {
            path: "src/a.rs".into(),
            orig_path: None,
            index,
            worktree,
            kind,
        }
    }

    fn argv(cmd: &GitCommand) -> Vec<String> {
        cmd.args()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn env(cmd: &GitCommand) -> Vec<(String, String)> {
        cmd.envs()
            .iter()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
            .collect()
    }

    fn request(side: DiffSide, orig: Option<&str>) -> DiffRequest {
        DiffRequest {
            path: "new name.rs".into(),
            orig_path: orig.map(Into::into),
            side,
        }
    }

    #[test]
    fn worktree_changes_give_the_worktree_side() {
        for (x, y) in [('.', 'M'), ('M', 'M')] {
            let req = DiffRequest::for_entry(&entry(x, y, EntryKind::Changed));
            assert_eq!(req.side, DiffSide::Worktree, "{x}{y}");
            assert_eq!(req.path, "src/a.rs".into());
        }
    }

    #[test]
    fn staged_only_changes_give_the_index_side() {
        let req = DiffRequest::for_entry(&entry('M', '.', EntryKind::Changed));
        assert_eq!(req.side, DiffSide::Index);
    }

    #[test]
    fn staged_rename_keeps_its_original_path() {
        let mut e = entry('R', '.', EntryKind::Renamed);
        e.orig_path = Some("src/old.rs".into());
        assert_eq!(
            DiffRequest::for_entry(&e),
            DiffRequest {
                path: "src/a.rs".into(),
                orig_path: Some("src/old.rs".into()),
                side: DiffSide::Index,
            }
        );
    }

    #[test]
    fn untracked_file_gives_the_untracked_side() {
        let req = DiffRequest::for_entry(&entry('?', '?', EntryKind::Untracked));
        assert_eq!(req.side, DiffSide::Untracked);
    }

    #[test]
    fn worktree_command() {
        let cmd = diff_command(&request(DiffSide::Worktree, None));
        assert_eq!(
            argv(&cmd),
            ["diff", "--no-color", "--no-ext-diff", "--", "new name.rs"]
        );
    }

    #[test]
    fn index_command_with_and_without_original_path() {
        let cmd = diff_command(&request(DiffSide::Index, Some("old.rs")));
        assert_eq!(
            argv(&cmd),
            [
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--cached",
                "-M",
                "--",
                "old.rs",
                "new name.rs"
            ]
        );
        let cmd = diff_command(&request(DiffSide::Index, None));
        assert_eq!(
            argv(&cmd),
            [
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--cached",
                "-M",
                "--",
                "new name.rs"
            ]
        );
    }

    #[test]
    fn untracked_command_diffs_against_dev_null() {
        let cmd = diff_command(&request(DiffSide::Untracked, None));
        assert_eq!(
            argv(&cmd),
            [
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--no-index",
                "--",
                "/dev/null",
                "new name.rs"
            ]
        );
    }

    #[test]
    fn every_side_sets_the_same_env() {
        for side in [DiffSide::Worktree, DiffSide::Index, DiffSide::Untracked] {
            let cmd = diff_command(&request(side, None));
            assert_eq!(
                env(&cmd),
                [
                    ("GIT_OPTIONAL_LOCKS".into(), "0".into()),
                    ("LC_ALL".into(), "C".into()),
                    ("GIT_LITERAL_PATHSPECS".into(), "1".into())
                ],
                "{side:?}"
            );
        }
    }

    #[test]
    fn pathspec_magic_and_globs_in_names_pass_through_untouched() {
        for name in [":(top)other", "x*", "[ab]?"] {
            for side in [DiffSide::Worktree, DiffSide::Index] {
                let req = DiffRequest {
                    path: name.into(),
                    orig_path: None,
                    side,
                };
                let cmd = diff_command(&req);
                assert_eq!(argv(&cmd).last().map(String::as_str), Some(name));
                assert!(
                    env(&cmd).contains(&("GIT_LITERAL_PATHSPECS".into(), "1".into())),
                    "{name} {side:?}"
                );
            }
        }
    }

    #[test]
    fn hostile_file_names_diff_only_themselves_in_real_git() {
        use crate::git::fake::git;
        let tmp = crate::git::fake::TempDir::new("pathspec");
        let dir = tmp.path();
        let names = ["other", ":(top)other", "x1", "x*"];
        git(dir, &["init", "-q"]);
        for name in names {
            std::fs::write(dir.join(name), "old\n").unwrap();
        }
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "init"]);
        for name in names {
            std::fs::write(dir.join(name), format!("new {name}\n")).unwrap();
        }
        let runner = crate::git::CliRunner::new(dir);

        for (name, other) in [(":(top)other", "a/other\n"), ("x*", "a/x1\n")] {
            let request = DiffRequest {
                path: name.into(),
                orig_path: None,
                side: DiffSide::Worktree,
            };
            let out = crate::git::GitRunner::run(&runner, &diff_command(&request)).unwrap();
            let text = String::from_utf8_lossy(&out.stdout);
            assert!(text.contains(&format!("+new {name}")), "{name}: {text}");
            assert!(!text.contains(other), "{name} leaked {other}: {text}");
            assert_eq!(text.matches("diff --git").count(), 1, "{name}: {text}");
        }
    }

    /// Regression: a name that is not UTF-8 used to be decoded lossily into
    /// argv, so its diff showed its look-alike's changes instead.
    #[cfg(unix)]
    #[test]
    fn a_name_that_is_not_utf8_diffs_itself_not_its_look_alike() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        use crate::git::fake::{LOOK_ALIKE, RAW_NAME, entry_named, git, look_alike_repo};

        let tmp = look_alike_repo();
        let dir = tmp.path();
        // The look-alike has both staged and unstaged changes, so a diff of
        // the wrong name on either side would show them.
        std::fs::write(dir.join(LOOK_ALIKE), "look-alike new\n").unwrap();
        git(dir, &["add", "--", LOOK_ALIKE]);
        std::fs::write(dir.join(LOOK_ALIKE), "look-alike newer\n").unwrap();
        let runner = crate::git::CliRunner::new(dir);
        let diff_of_raw = |side| {
            let request = DiffRequest::for_entry(&entry_named(&runner, RAW_NAME).unwrap());
            assert_eq!(request.side, side);
            let out = crate::git::GitRunner::run(&runner, &diff_command(&request)).unwrap();
            assert!(out.success, "{}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).into_owned()
        };

        let text = diff_of_raw(DiffSide::Worktree);
        assert!(text.contains("-raw old"), "{text}");
        assert!(!text.contains("look-alike"), "{text}");

        let rm = ["rm", "-q", "--cached", "--"].map(OsStr::new);
        git(dir, &[&rm[..], &[OsStr::from_bytes(RAW_NAME)]].concat());
        let text = diff_of_raw(DiffSide::Index);
        assert!(text.contains("-raw old"), "{text}");
        assert!(!text.contains("look-alike"), "{text}");
    }

    const PATCH_FLAGS: [&str; 8] = [
        "diff",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
        "-U3",
        "--src-prefix=a/",
        "--dst-prefix=b/",
    ];

    #[test]
    fn patch_diff_command_for_each_side() {
        let cases: [(DiffSide, &[&str]); 3] = [
            (DiffSide::Worktree, &["--", "new name.rs"]),
            (DiffSide::Index, &["--cached", "--", "new name.rs"]),
            (
                DiffSide::Untracked,
                &["--no-index", "--", "/dev/null", "new name.rs"],
            ),
        ];
        for (side, tail) in cases {
            // The original path never matters: renames are off.
            let cmd = patch_diff_command(&request(side, Some("old.rs")));
            assert_eq!(argv(&cmd), [&PATCH_FLAGS[..], tail].concat(), "{side:?}");
            assert_eq!(
                env(&cmd),
                [
                    ("GIT_OPTIONAL_LOCKS".into(), "0".into()),
                    ("LC_ALL".into(), "C".into()),
                    ("GIT_LITERAL_PATHSPECS".into(), "1".into())
                ],
                "{side:?}"
            );
        }
    }

    fn patch_argv(tail: &[&'static str]) -> Vec<&'static str> {
        [&PATCH_FLAGS[..], tail].concat()
    }

    const ONE_HUNK: &[u8] = b"diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n";

    #[test]
    fn load_patch_parses_the_output() {
        use crate::git::fake::{FakeRunner, ok};
        let runner = FakeRunner::new().expect(&patch_argv(&["--", "x"]), ok(ONE_HUNK));
        let patch = load_patch(&runner, &request_for("x", DiffSide::Worktree)).unwrap();
        assert_eq!(patch.hunks.len(), 1);
        runner.assert_done();
    }

    fn request_for(path: &str, side: DiffSide) -> DiffRequest {
        DiffRequest {
            path: path.into(),
            orig_path: None,
            side,
        }
    }

    #[test]
    fn load_patch_takes_exit_1_from_no_index_as_a_diff() {
        use crate::git::fake::FakeRunner;
        use crate::git::runner::GitOutput;
        let differs = GitOutput {
            success: false,
            stdout: ONE_HUNK.to_vec(),
            stderr: Vec::new(),
        };
        let runner = FakeRunner::new().expect(
            &patch_argv(&["--no-index", "--", "/dev/null", "x"]),
            differs.clone(),
        );
        let patch = load_patch(&runner, &request_for("x", DiffSide::Untracked)).unwrap();
        assert_eq!(patch.hunks.len(), 1);

        // Anywhere else a failure is a failure.
        let runner = FakeRunner::new().expect(&patch_argv(&["--", "x"]), differs);
        assert_eq!(
            load_patch(&runner, &request_for("x", DiffSide::Worktree)),
            Err("git diff failed".into())
        );
    }

    #[test]
    fn load_patch_errors_are_terminal_safe() {
        use crate::git::fake::{FakeRunner, fail, ok};
        let runner = FakeRunner::new().expect(
            &patch_argv(&["--cached", "--", "x"]),
            fail("fatal: bad \x1b[2J path\n"),
        );
        assert_eq!(
            load_patch(&runner, &request_for("x", DiffSide::Index)),
            Err("fatal: bad \u{FFFD}[2J path".into())
        );
        let two_files = [ONE_HUNK, ONE_HUNK].concat();
        let runner = FakeRunner::new().expect(&patch_argv(&["--", "x"]), ok(&two_files));
        assert_eq!(
            load_patch(&runner, &request_for("x", DiffSide::Worktree)),
            Err("The diff holds more than one file".into())
        );
    }

    #[test]
    fn load_patch_of_a_new_file_in_real_git() {
        use crate::git::fake::{TempDir, git};
        let tmp = TempDir::new("load-patch");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        std::fs::write(dir.join("new.txt"), "a\nb\n").unwrap();
        let runner = crate::git::CliRunner::isolated(dir);
        let patch = load_patch(&runner, &request_for("new.txt", DiffSide::Untracked)).unwrap();
        assert_eq!(patch.kind, crate::git::patch::FileKind::Added);
        assert_eq!(patch.hunks.len(), 1);
        assert_eq!(patch.hunks[0].new_len, 2);
    }

    fn kinds(lines: &[&str]) -> Vec<LineKind> {
        let mut classifier = Classifier::new();
        lines
            .iter()
            .map(|l| classifier.classify(l.as_bytes()).kind)
            .collect()
    }

    #[test]
    fn classifies_a_whole_diff() {
        use LineKind::*;
        let got = kinds(&[
            "diff --git a/x b/x\n",
            "index 1..2 100644\n",
            "--- a/x\n",
            "+++ b/x\n",
            "@@ -1,3 +1,3 @@ fn main\n",
            " same\n",
            "-old\n",
            "+new\n",
            "--- not a header inside a hunk\n",
            "\\ No newline at end of file\n",
        ]);
        assert_eq!(
            got,
            [
                Header, Header, Header, Header, Hunk, Context, Removed, Added, Removed, Context
            ]
        );
    }

    #[test]
    fn a_new_file_header_resets_hunk_mode() {
        use LineKind::*;
        let got = kinds(&[
            "@@ -1 +1 @@\n",
            "+a\n",
            "diff --git a/y b/y\n",
            "--- a/y\n",
            "+++ b/y\n",
        ]);
        assert_eq!(got, [Hunk, Added, Header, Header, Header]);
    }

    #[test]
    fn strips_line_endings() {
        let mut c = Classifier::new();
        assert_eq!(c.classify(b"index abc\r\n").text, "index abc");
        assert_eq!(c.classify(b"index abc\n").text, "index abc");
        assert_eq!(c.classify(b"index abc").text, "index abc");
    }

    #[test]
    fn expands_tabs_and_replaces_control_bytes() {
        let mut c = Classifier::new();
        c.classify(b"@@ -1 +1 @@\n");
        let line = c.classify(b"+\tx\x1b[31m\x07\n");
        assert_eq!(line.kind, LineKind::Added);
        assert_eq!(line.text, "+    x\u{FFFD}[31m\u{FFFD}");
    }

    #[test]
    fn invalid_utf8_is_decoded_lossily() {
        let mut c = Classifier::new();
        assert_eq!(c.classify(b"a\xffb\n").text, "a\u{FFFD}b");
    }
}
