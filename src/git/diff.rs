use super::runner::GitCommand;
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
    pub path: String,
    pub orig_path: Option<String>,
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
        DiffSide::Worktree => cmd = cmd.arg("--").arg(&request.path),
        DiffSide::Index => {
            cmd = cmd.arg("--cached").arg("-M").arg("--");
            if let Some(orig) = &request.orig_path {
                cmd = cmd.arg(orig);
            }
            cmd = cmd.arg(&request.path);
        }
        DiffSide::Untracked => {
            cmd = cmd
                .arg("--no-index")
                .arg("--")
                .arg("/dev/null")
                .arg(&request.path)
        }
    }
    // File names come from the repo, so a hostile `:(top)x` or `x*` must not
    // be read as pathspec magic or a glob. A no-op under `--no-index`.
    cmd.env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .env("GIT_LITERAL_PATHSPECS", "1")
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
            orig_path: orig.map(str::to_string),
            side,
        }
    }

    #[test]
    fn worktree_changes_give_the_worktree_side() {
        for (x, y) in [('.', 'M'), ('M', 'M')] {
            let req = DiffRequest::for_entry(&entry(x, y, EntryKind::Changed));
            assert_eq!(req.side, DiffSide::Worktree, "{x}{y}");
            assert_eq!(req.path, "src/a.rs");
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

    /// Run git in `dir` for test setup, failing loudly.
    fn git(dir: &std::path::Path, args: &[&str]) -> Vec<u8> {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    #[test]
    fn hostile_file_names_diff_only_themselves_in_real_git() {
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
