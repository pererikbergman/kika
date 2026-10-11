use super::commit_files::{CommitFile, CommitRange};
use super::joiner;
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

/// The fewest context lines a diff can show. Not 0: a `-U0` patch only
/// applies with `--unidiff-zero`, and hunk staging uses the same setting.
pub const MIN_CONTEXT: u32 = 1;
/// The most context lines a diff can show.
pub const MAX_CONTEXT: u32 = 100;

/// How diffs are drawn: chosen by the user for the session, and passed to
/// both the streamed diff and the patch so they always agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffOptions {
    /// Unchanged lines shown around each change (`-U<n>`).
    pub context: u32,
    /// Hide changes that only touch whitespace (`-w`). Such a diff cannot be
    /// applied back safely, so staging from it is refused.
    pub ignore_whitespace: bool,
}

impl Default for DiffOptions {
    /// Git's own default context, with every change shown.
    fn default() -> Self {
        Self {
            context: 3,
            ignore_whitespace: false,
        }
    }
}

impl DiffOptions {
    /// The flags these options add to `git diff`, which go before `--`.
    fn args(self) -> impl Iterator<Item = String> {
        let whitespace = self.ignore_whitespace.then(|| "-w".to_string());
        std::iter::once(format!("-U{}", self.context)).chain(whitespace)
    }
}

/// One diff to load. Built from a status entry or a commit's file so the
/// model never spells argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRequest {
    pub path: RepoPath,
    pub orig_path: Option<RepoPath>,
    /// Ignored when `commit` is set: a commit's diff has no sides.
    pub side: DiffSide,
    pub options: DiffOptions,
    /// The file's change in this commit, instead of in the working tree.
    pub commit: Option<CommitRange>,
}

impl DiffRequest {
    /// Unstaged changes win when a file has both, since that is what the user
    /// is most likely editing; otherwise the staged diff is shown.
    pub fn for_entry(entry: &FileEntry, options: DiffOptions) -> Self {
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
            options,
            commit: None,
        }
    }

    /// `file`'s change in the commit `range` names, against its first parent.
    pub fn for_commit_file(range: &CommitRange, file: &CommitFile, options: DiffOptions) -> Self {
        Self {
            path: file.path.clone(),
            orig_path: file.orig_path.clone(),
            side: DiffSide::Index,
            options,
            commit: Some(range.clone()),
        }
    }
}

/// The `git diff` for a request. Colour and external diff drivers are off so
/// the output is plain text we classify ourselves.
pub fn diff_command(request: &DiffRequest) -> GitCommand {
    if let Some(range) = &request.commit {
        return commit_diff_command(range, request);
    }
    let mut cmd = GitCommand::new("diff")
        .arg("--no-color")
        .arg("--no-ext-diff");
    for arg in request.options.args() {
        cmd = cmd.arg(arg);
    }
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

/// One file's change in a commit, against its first parent or, for a root
/// commit, the empty tree. A rename names both paths, so `-M` can pair them.
fn commit_diff_command(range: &CommitRange, request: &DiffRequest) -> GitCommand {
    let cmd = GitCommand::new("diff-tree")
        .arg("-p")
        .arg("-r")
        .arg("-M")
        .arg("--no-commit-id")
        .arg("--no-color")
        .arg("--no-ext-diff");
    let cmd = request.options.args().fold(cmd, |cmd, arg| cmd.arg(arg));
    let cmd = range
        .args()
        .fold(cmd.arg("--root"), |cmd, arg| cmd.arg(arg))
        .arg("--");
    let cmd = match &request.orig_path {
        Some(orig) => cmd.arg(orig.as_os_str()),
        None => cmd,
    };
    cmd.arg(request.path.as_os_str())
        .env("GIT_OPTIONAL_LOCKS", "0")
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
        .arg("--no-renames");
    for arg in request.options.args() {
        cmd = cmd.arg(arg);
    }
    cmd = cmd.arg("--src-prefix=a/").arg("--dst-prefix=b/");
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
    let stdout = run_diff(runner, &patch_diff_command(request), request.side)?;
    patch::parse(&stdout).map_err(|e| e.to_string())
}

/// Run a whole `git diff` of `side` and return its output. Errors are
/// made safe to draw, since git's message can quote repo-controlled paths.
pub fn run_diff(
    runner: &dyn GitRunner,
    cmd: &GitCommand,
    side: DiffSide,
) -> Result<Vec<u8>, String> {
    let output = runner
        .run(cmd)
        .map_err(|e| terminal_safe(e.to_string().as_bytes()))?;
    // `--no-index` exits 1 when the files differ, which is the point. The
    // runner reports no exit code; a real failure says why on stderr.
    let differs =
        side == DiffSide::Untracked && output.stderr.is_empty() && !output.stdout.is_empty();
    if !output.success && !differs {
        let message = terminal_safe(&output.stderr);
        return Err(if message.is_empty() {
            "git diff failed".into()
        } else {
            message
        });
    }
    Ok(output.stdout)
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
        let text = match kind {
            LineKind::Added | LineKind::Removed | LineKind::Context => sanitize_body(raw),
            LineKind::Header | LineKind::Hunk => sanitize(raw),
        };
        DiffLine { kind, text }
    }
}

/// Decode lossily and make the text safe to draw: tabs become spaces and any
/// other control character becomes `U+FFFD`, so file bytes never drive the terminal.
/// Bidi controls and zero-width characters reorder or hide text, so a hostile
/// name could pass for another; they become a visible `<U+XXXX>` escape rather
/// than being dropped, so names that differ only by them still look different.
pub fn sanitize(raw: &[u8]) -> String {
    let mut text = String::with_capacity(raw.len());
    for c in String::from_utf8_lossy(raw).chars() {
        push_safe(&mut text, c);
    }
    text
}

/// `sanitize` for the lines of a file (diff, blame, rendered diff). Persian,
/// Arabic-script and Indic words need ZWJ/ZWNJ, which only shape their
/// neighbours and cannot reorder text, so a joiner is kept where it changes
/// the shaping, following UTS #39 section 3.1.1.1 with the deviations
/// listed in `git::joiner`: next to a virama in a consonant cluster, or a
/// ZWNJ breaking a join between Arabic-script letters. Anywhere else it
/// draws nothing, so `is\u{200D}admin` or `кл\u{200D}юч` would pass for the
/// plain word: those stay escaped.
///
/// Known limits: a terminal that does not shape these scripts draws a kept
/// joiner as nothing, so there `می\u{200C}خواهم` and `میخواهم` look alike.
/// Emoji ZWJ sequences stay escaped: a terminal without grapheme clustering
/// draws each part, wider than the two columns the layout reserves, which
/// pushes the row past its pane.
pub fn sanitize_body(raw: &[u8]) -> String {
    let mut text = String::with_capacity(raw.len());
    let mut before = None;
    let decoded = String::from_utf8_lossy(raw);
    let mut chars = decoded.chars().peekable();
    while let Some(c) = chars.next() {
        if matches!(c, '\u{200C}' | '\u{200D}')
            && before
                .zip(chars.peek().copied())
                .is_some_and(|(before, after)| joiner::shapes(c, before, after))
        {
            text.push(c);
        } else {
            push_safe(&mut text, c);
        }
        before = Some(c);
    }
    text
}

/// The one escaping rule both sanitisers share, so they cannot drift apart.
fn push_safe(text: &mut String, c: char) {
    match c {
        '\t' => text.push_str("    "),
        c if is_invisible_or_bidi(c) => text.push_str(&format!("<U+{:04X}>", u32::from(c))),
        c if c.is_control() => text.push('\u{FFFD}'),
        c => text.push(c),
    }
}

/// Format characters (not `is_control`) that reorder text or draw nothing:
/// Unicode's Default_Ignorable_Code_Point set plus the interlinear annotation
/// characters. U+FE0E and U+FE0F are left alone because emoji such as "❤️"
/// need them; a name differing only by one of those is a narrow, accepted gap.
fn is_invisible_or_bidi(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{061C}'
            | '\u{115F}'..='\u{1160}'
            | '\u{17B4}'..='\u{17B5}'
            | '\u{180B}'..='\u{180F}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0D}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{FFF0}'..='\u{FFFB}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0000}'..='\u{E0FFF}'
    )
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
            options: DiffOptions::default(),
            commit: None,
        }
    }

    fn with_options(side: DiffSide, context: u32, ignore_whitespace: bool) -> DiffRequest {
        DiffRequest {
            options: DiffOptions {
                context,
                ignore_whitespace,
            },
            ..request(side, None)
        }
    }

    /// The flags between `diff` and `--`, which is where the options go.
    fn before_dashdash(cmd: &GitCommand) -> Vec<String> {
        argv(cmd).into_iter().take_while(|a| a != "--").collect()
    }

    const SIDES: [DiffSide; 3] = [DiffSide::Worktree, DiffSide::Index, DiffSide::Untracked];

    #[test]
    fn default_options_are_three_lines_with_whitespace_shown() {
        assert_eq!(
            DiffOptions::default(),
            DiffOptions {
                context: 3,
                ignore_whitespace: false
            }
        );
    }

    #[test]
    fn diff_command_carries_the_options_before_the_paths_on_every_side() {
        for side in SIDES {
            let flags = before_dashdash(&diff_command(&with_options(side, 5, false)));
            assert!(flags.contains(&"-U5".into()), "{side:?} {flags:?}");
            assert!(!flags.contains(&"-w".into()), "{side:?} {flags:?}");

            let flags = before_dashdash(&diff_command(&with_options(side, 5, true)));
            assert!(flags.contains(&"-U5".into()), "{side:?} {flags:?}");
            assert!(flags.contains(&"-w".into()), "{side:?} {flags:?}");

            let all = argv(&diff_command(&request(side, None)));
            let flags = before_dashdash(&diff_command(&request(side, None)));
            assert!(flags.contains(&"-U3".into()), "{side:?} {flags:?}");
            assert!(!all.contains(&"-w".into()), "{side:?} {all:?}");
        }
    }

    #[test]
    fn patch_diff_command_carries_the_options_before_the_paths_on_every_side() {
        for side in SIDES {
            let flags = before_dashdash(&patch_diff_command(&with_options(side, 5, true)));
            assert!(flags.contains(&"-U5".into()), "{side:?} {flags:?}");
            assert!(!flags.contains(&"-U3".into()), "{side:?} {flags:?}");
            assert!(flags.contains(&"-w".into()), "{side:?} {flags:?}");

            let flags = before_dashdash(&patch_diff_command(&with_options(side, 5, false)));
            assert!(flags.contains(&"-U5".into()), "{side:?} {flags:?}");
            assert!(!flags.contains(&"-w".into()), "{side:?} {flags:?}");

            let all = argv(&patch_diff_command(&request(side, None)));
            assert!(all.contains(&"-U3".into()), "{side:?} {all:?}");
            assert!(!all.contains(&"-w".into()), "{side:?} {all:?}");
        }
    }

    #[test]
    fn worktree_changes_give_the_worktree_side() {
        for (x, y) in [('.', 'M'), ('M', 'M')] {
            let req =
                DiffRequest::for_entry(&entry(x, y, EntryKind::Changed), DiffOptions::default());
            assert_eq!(req.side, DiffSide::Worktree, "{x}{y}");
            assert_eq!(req.path, "src/a.rs".into());
        }
    }

    #[test]
    fn staged_only_changes_give_the_index_side() {
        let req =
            DiffRequest::for_entry(&entry('M', '.', EntryKind::Changed), DiffOptions::default());
        assert_eq!(req.side, DiffSide::Index);
    }

    #[test]
    fn staged_rename_keeps_its_original_path() {
        let mut e = entry('R', '.', EntryKind::Renamed);
        e.orig_path = Some("src/old.rs".into());
        assert_eq!(
            DiffRequest::for_entry(&e, DiffOptions::default()),
            DiffRequest {
                path: "src/a.rs".into(),
                orig_path: Some("src/old.rs".into()),
                side: DiffSide::Index,
                options: DiffOptions::default(),
                commit: None,
            }
        );
    }

    #[test]
    fn untracked_file_gives_the_untracked_side() {
        let req = DiffRequest::for_entry(
            &entry('?', '?', EntryKind::Untracked),
            DiffOptions::default(),
        );
        assert_eq!(req.side, DiffSide::Untracked);
    }

    #[test]
    fn worktree_command() {
        let cmd = diff_command(&request(DiffSide::Worktree, None));
        assert_eq!(
            argv(&cmd),
            [
                "diff",
                "--no-color",
                "--no-ext-diff",
                "-U3",
                "--",
                "new name.rs"
            ]
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
                "-U3",
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
                "-U3",
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
                "-U3",
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
                    options: DiffOptions::default(),
                    commit: None,
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
    #[cfg_attr(windows, ignore = "Windows forbids * and : in file names")]
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
                options: DiffOptions::default(),
                commit: None,
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
            let request = DiffRequest::for_entry(
                &entry_named(&runner, RAW_NAME).unwrap(),
                DiffOptions::default(),
            );
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

    fn commit_request(
        parent: Option<&str>,
        orig: Option<&str>,
        options: DiffOptions,
    ) -> DiffRequest {
        use crate::git::commit_files::{CommitFile, CommitId};
        let range = CommitRange {
            oid: CommitId::parse("c0ffee").unwrap(),
            parent: parent.map(|p| CommitId::parse(p).unwrap()),
        };
        let file = CommitFile {
            status: if orig.is_some() { 'R' } else { 'M' },
            path: "new name.rs".into(),
            orig_path: orig.map(Into::into),
        };
        DiffRequest::for_commit_file(&range, &file, options)
    }

    const COMMIT_FLAGS: [&str; 7] = [
        "diff-tree",
        "-p",
        "-r",
        "-M",
        "--no-commit-id",
        "--no-color",
        "--no-ext-diff",
    ];

    #[test]
    fn a_commit_file_diffs_against_the_first_parent_with_both_names_of_a_rename() {
        let request = commit_request(Some("beef"), Some("old.rs"), DiffOptions::default());
        assert_eq!(request.side, DiffSide::Index);
        let cmd = diff_command(&request);
        let tail = [
            "-U3",
            "--root",
            "beef",
            "c0ffee",
            "--",
            "old.rs",
            "new name.rs",
        ];
        assert_eq!(argv(&cmd), [&COMMIT_FLAGS[..], &tail].concat());
        assert_eq!(
            env(&cmd),
            [
                ("GIT_OPTIONAL_LOCKS".into(), "0".into()),
                ("LC_ALL".into(), "C".into()),
                ("GIT_LITERAL_PATHSPECS".into(), "1".into())
            ]
        );
    }

    #[test]
    fn a_root_commit_file_diffs_against_the_empty_tree_with_the_options() {
        let options = DiffOptions {
            context: 7,
            ignore_whitespace: true,
        };
        let cmd = diff_command(&commit_request(None, None, options));
        let tail = ["-U7", "-w", "--root", "c0ffee", "--", "new name.rs"];
        assert_eq!(argv(&cmd), [&COMMIT_FLAGS[..], &tail].concat());
    }

    #[cfg(unix)]
    #[test]
    fn a_commit_file_name_reaches_argv_as_its_raw_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let mut request = commit_request(None, None, DiffOptions::default());
        request.path = RepoPath::from_bytes(b"x\xff");
        let cmd = diff_command(&request);
        assert_eq!(cmd.args().last().unwrap().as_bytes(), b"x\xff");
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
            options: DiffOptions::default(),
            commit: None,
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

    /// Every class the strict sanitiser escapes, with sample code points.
    const STRICT_CLASSES: [(&str, &[char]); 17] = [
        (
            "embedding/override",
            &['\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}'],
        ),
        ("isolate", &['\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}']),
        ("mark", &['\u{200E}', '\u{200F}', '\u{061C}']),
        ("zero-width", &['\u{200B}', '\u{200C}', '\u{200D}']),
        ("word joiner", &['\u{2060}']),
        ("byte order mark", &['\u{FEFF}']),
        ("soft hyphen", &['\u{00AD}']),
        ("combining grapheme joiner", &['\u{034F}']),
        (
            "hangul filler",
            &['\u{115F}', '\u{1160}', '\u{3164}', '\u{FFA0}'],
        ),
        ("khmer vowel inherent", &['\u{17B4}', '\u{17B5}']),
        (
            "mongolian format",
            &['\u{180B}', '\u{180C}', '\u{180D}', '\u{180E}', '\u{180F}'],
        ),
        (
            "invisible operator and deprecated format",
            &['\u{2061}', '\u{2064}', '\u{2065}', '\u{206A}', '\u{206F}'],
        ),
        ("variation selector", &['\u{FE00}', '\u{FE0D}']),
        (
            "unassigned and interlinear annotation",
            &['\u{FFF0}', '\u{FFF8}', '\u{FFF9}', '\u{FFFA}', '\u{FFFB}'],
        ),
        ("shorthand format", &['\u{1BCA0}', '\u{1BCA3}']),
        ("musical format", &['\u{1D173}', '\u{1D17A}']),
        (
            "tag",
            &[
                '\u{E0000}',
                '\u{E0001}',
                '\u{E0041}',
                '\u{E007F}',
                '\u{E0FFF}',
            ],
        ),
    ];

    #[test]
    fn bidi_and_invisible_characters_become_visible_escapes() {
        for (class, chars) in STRICT_CLASSES {
            for &c in chars {
                let raw = format!("a{c}b");
                let expected = format!("a<U+{:04X}>b", c as u32);
                assert_eq!(sanitize(raw.as_bytes()), expected, "{class} {c:?}");
            }
        }
        assert_eq!(sanitize("a\u{202E}b".as_bytes()), "a<U+202E>b");
        assert_eq!(sanitize("\u{061C}".as_bytes()), "<U+061C>");
    }

    #[test]
    fn ordinary_text_is_unchanged() {
        // U+FE0E/U+FE0F are kept so emoji presentation still works.
        for text in [
            "src/café.rs",
            "日本語",
            "🙂",
            "plain ascii",
            "❤\u{FE0F}",
            "\u{263A}\u{FE0E}",
        ] {
            assert_eq!(sanitize(text.as_bytes()), text);
        }
    }

    #[test]
    fn git_error_text_shows_bidi_escapes() {
        assert_eq!(
            terminal_safe("fatal: bad \u{2066}path\u{2069}\n".as_bytes()),
            "fatal: bad <U+2066>path<U+2069>"
        );
    }

    #[test]
    fn body_text_keeps_joiners_that_change_how_letters_join() {
        for text in [
            // A ZWNJ breaking a join between Arabic-script letters.
            "می\u{200C}خواهم",
            "ب\u{200C}ا",
            // The same in Syriac, N'Ko and Mongolian.
            "ܒ\u{200C}ܐ",
            "ߊ\u{200C}ߓ",
            "ᠠ\u{200C}ᠢ",
            // After a virama, before a consonant of its script: Devanagari,
            // Malayalam, Sinhala and Myanmar.
            "क्\u{200D}ष",
            "ന്\u{200D}ന",
            "ක්\u{200D}ෂ",
            "က်\u{200D}ခ",
            // Tamil: a ZWNJ after pulli breaks the க்ஷ and ஸ்ரீ ligatures.
            "க்\u{200C}ஷ",
            "ஸ்\u{200C}ர",
            // Owner decision: Tamil pulli takes the same consonant rule, so
            // this is kept although it draws like the plain pair.
            "த்\u{200C}த",
            // A ZWJ between a consonant and a virama: eyelash ra, ya-phala.
            "र\u{200D}्य",
            "র\u{200D}্য",
            "ක\u{200D}්ව",
        ] {
            assert_eq!(sanitize_body(text.as_bytes()), text);
        }
    }

    #[test]
    fn body_text_still_escapes_every_other_strict_character() {
        for (class, chars) in STRICT_CLASSES {
            for &c in chars
                .iter()
                .filter(|&&c| c != '\u{200C}' && c != '\u{200D}')
            {
                let escape = format!("<U+{:04X}>", c as u32);
                for (before, after) in [('a', 'b'), ('é', 'ü'), ('👨', '👩'), ('م', 'ی')] {
                    let raw = format!("{before}{c}{after}");
                    let expected = format!("{before}{escape}{after}");
                    assert_eq!(sanitize_body(raw.as_bytes()), expected, "{class} {c:?}");
                }
            }
        }
        assert_eq!(sanitize_body("م\u{202E}ی".as_bytes()), "م<U+202E>ی");
        assert_eq!(sanitize_body("क\u{2066}ष".as_bytes()), "क<U+2066>ष");
    }

    #[test]
    fn body_text_escapes_joiners_that_would_change_nothing_on_screen() {
        let cases = [
            // Letters that never join on that side: right-joining ones,
            // hamza, digits, punctuation and Indic consonants with no virama.
            ("ر\u{200C}د", "ر<U+200C>د"),
            ("د\u{200C}ر", "د<U+200C>ر"),
            ("و\u{200C}ا", "و<U+200C>ا"),
            ("ء\u{200D}ب", "ء<U+200D>ب"),
            ("ܐ\u{200D}ܒ", "ܐ<U+200D>ܒ"),
            ("ب\u{200C}١", "ب<U+200C>١"),
            ("١\u{200D}٢", "١<U+200D>٢"),
            ("۱\u{200C}۲", "۱<U+200C>۲"),
            ("१\u{200D}२", "१<U+200D>२"),
            ("၁\u{200D}၂", "၁<U+200D>၂"),
            ("߁\u{200D}߂", "߁<U+200D>߂"),
            ("،\u{200D}؛", "،<U+200D>؛"),
            ("।\u{200D}॥", "।<U+200D>॥"),
            ("क\u{200C}ष", "क<U+200C>ष"),
            ("क\u{200D}ष", "क<U+200D>ष"),
            ("म\u{200D}न", "म<U+200D>न"),
            ("က\u{200C}ခ", "က<U+200C>ခ"),
            // A ZWJ between Arabic-script letters that already join draws
            // the plain pair; so does a ZWNJ between two tatweels.
            ("ن\u{200D}ہ", "ن<U+200D>ہ"),
            ("ب\u{200D}ب", "ب<U+200D>ب"),
            ("ب\u{200D}ا", "ب<U+200D>ا"),
            ("می\u{200D}خواهم", "می<U+200D>خواهم"),
            ("ـ\u{200D}ب", "ـ<U+200D>ب"),
            ("ـ\u{200C}ـ", "ـ<U+200C>ـ"),
            // UTS #39 A1 allows only Joining_Type D before and D or R after a
            // ZWNJ, so join-causing characters such as tatweel do not count.
            ("ـ\u{200C}ب", "ـ<U+200C>ب"),
            ("ب\u{200C}ـ", "ب<U+200C>ـ"),
            ("ب\u{200C}ߺ", "ب<U+200C>ߺ"),
            // Letters of two scripts never join, so a ZWNJ between them
            // breaks nothing.
            ("ب\u{200C}ܐ", "ب<U+200C>ܐ"),
            ("ܒ\u{200C}ا", "ܒ<U+200C>ا"),
            ("ب\u{200C}ᠠ", "ب<U+200C>ᠠ"),
            ("ᠠ\u{200C}ߊ", "ᠠ<U+200C>ߊ"),
            ("ߊ\u{200C}ب", "ߊ<U+200C>ب"),
            ("ܒ\u{200D}ܐ", "ܒ<U+200D>ܐ"),
            ("ߊ\u{200D}ߓ", "ߊ<U+200D>ߓ"),
            ("ᠠ\u{200D}ᠢ", "ᠠ<U+200D>ᠢ"),
            // After a virama, anything but a consonant of the same script.
            ("क्\u{200C}१", "क्<U+200C>१"),
            ("क्\u{200D}।", "क्<U+200D>।"),
            ("क्\u{200C}अ", "क्<U+200C>अ"),
            ("क्\u{200C}ं", "क्<U+200C>ं"),
            ("ক্\u{200D}১", "ক্<U+200D>১"),
            ("१्\u{200C}२", "१्<U+200C>२"),
            ("क्\u{200C}ক", "क्<U+200C>ক"),
            ("क्\u{200D}က", "क्<U+200D>က"),
            // Myanmar asat and Sinhala al-lakuna act only with a ZWJ.
            ("က်\u{200C}ခ", "က်<U+200C>ခ"),
            ("ක්\u{200C}ෂ", "ක්<U+200C>ෂ"),
            // Before a virama: only a ZWJ, only after a consonant of its script.
            ("र\u{200C}्य", "र<U+200C>्य"),
            ("१\u{200D}्", "१<U+200D>्"),
            ("ক\u{200D}्", "ক<U+200D>्"),
            // Before a virama outside Devanagari, Bengali and Sinhala, no
            // font draws a distinct form (UTS #39 3.1.1.1 has no such rule).
            ("க\u{200D}்", "க<U+200D>்"),
            ("క\u{200D}్", "క<U+200D>్"),
            ("က\u{200D}်", "က<U+200D>်"),
            ("က\u{200D}္", "က<U+200D>္"),
            // Myanmar's invisible stacker is not a virama for joiners.
            ("္\u{200C}က", "္<U+200C>က"),
            ("္\u{200D}က", "္<U+200D>က"),
            // A virama with no letter of its scripts on the other side.
            ("क्\u{200D}a", "क्<U+200D>a"),
            ("م\u{200D}्", "م<U+200D>्"),
            ("င်\u{200D}္က", "င်<U+200D>္က"),
            // Presentation forms are already shaped.
            ("\u{FE91}\u{200D}\u{FE92}", "\u{FE91}<U+200D>\u{FE92}"),
            ("\u{FB56}\u{200C}\u{FB57}", "\u{FB56}<U+200C>\u{FB57}"),
            // Scripts where a joiner draws nothing: two strings look the same.
            ("кл\u{200D}юч", "кл<U+200D>юч"),
            ("中\u{200D}文", "中<U+200D>文"),
            ("é\u{200D}ü", "é<U+200D>ü"),
            ("α\u{200C}β", "α<U+200C>β"),
            ("한\u{200D}국", "한<U+200D>국"),
            // Emoji sequences: their width depends on the terminal.
            ("👨\u{200D}👩\u{200D}👧", "👨<U+200D>👩<U+200D>👧"),
            ("❤\u{FE0F}\u{200D}🔥", "❤\u{FE0F}<U+200D>🔥"),
            // Only one neighbour in a joining script.
            ("is\u{200D}admin", "is<U+200D>admin"),
            ("م\u{200D}a", "م<U+200D>a"),
            ("a\u{200C}خ", "a<U+200C>خ"),
            ("م\u{200D}ю", "م<U+200D>ю"),
            // Start or end of the line.
            ("\u{200D}م", "<U+200D>م"),
            ("م\u{200D}", "م<U+200D>"),
            ("\u{200C}", "<U+200C>"),
            // A run of joiners.
            ("م\u{200D}\u{200D}ی", "م<U+200D><U+200D>ی"),
            ("می\u{200C}\u{200D}خ", "می<U+200C><U+200D>خ"),
            // Next to a tag, a strict variation selector or a bidi mark.
            ("م\u{200D}\u{E0041}", "م<U+200D><U+E0041>"),
            ("\u{E0041}\u{200D}م", "<U+E0041><U+200D>م"),
            ("म\u{200D}\u{FE00}", "म<U+200D><U+FE00>"),
            ("\u{FE00}\u{200C}م", "<U+FE00><U+200C>م"),
            ("م\u{200D}\u{061C}ی", "م<U+200D><U+061C>ی"),
            // Next to a control character or a noncharacter.
            ("م\u{200D}\u{7}", "م<U+200D>\u{FFFD}"),
            ("م\u{200D}\u{FDD0}", "م<U+200D>\u{FDD0}"),
        ];
        for (raw, expected) in cases {
            assert_eq!(sanitize_body(raw.as_bytes()), expected, "{raw:?}");
        }
    }

    #[test]
    fn body_text_escapes_a_joiner_next_to_any_strict_character() {
        for (class, chars) in STRICT_CLASSES {
            for &c in chars {
                let escape = format!("<U+{:04X}>", c as u32);
                for letter in ['ب', '्'] {
                    for (raw, expected) in [
                        (
                            format!("{letter}\u{200D}{c}"),
                            format!("{letter}<U+200D>{escape}"),
                        ),
                        (
                            format!("{c}\u{200C}{letter}"),
                            format!("{escape}<U+200C>{letter}"),
                        ),
                    ] {
                        assert_eq!(sanitize_body(raw.as_bytes()), expected, "{class} {c:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn body_text_expands_tabs_and_replaces_controls_like_strict() {
        let raw = b"\tx\x1b[31m\x07\xff";
        assert_eq!(sanitize_body(raw), sanitize(raw));
    }

    #[test]
    fn strict_text_still_escapes_joiners_inside_non_ascii_text() {
        assert_eq!(sanitize("👨\u{200D}👩".as_bytes()), "👨<U+200D>👩");
        assert_eq!(sanitize("می\u{200C}خواهم".as_bytes()), "می<U+200C>خواهم");
        assert_eq!(
            terminal_safe("fatal: 👨\u{200D}👩\n".as_bytes()),
            "fatal: 👨<U+200D>👩"
        );
    }

    #[test]
    fn classify_keeps_joiners_in_body_lines_and_escapes_them_in_headers() {
        let mut c = Classifier::new();
        let header = c.classify("+++ b/می\u{200C}خواهم.txt\n".as_bytes());
        assert_eq!(header.kind, LineKind::Header);
        assert_eq!(header.text, "+++ b/می<U+200C>خواهم.txt");
        let hunk = c.classify("@@ -1 +1 @@ می\u{200C}خواهم\n".as_bytes());
        assert_eq!(hunk.kind, LineKind::Hunk);
        assert_eq!(hunk.text, "@@ -1 +1 @@ می<U+200C>خواهم");
        for (raw, kind) in [
            ("+می\u{200C}خواهم", LineKind::Added),
            ("-می\u{200C}خواهم", LineKind::Removed),
            (" می\u{200C}خواهم", LineKind::Context),
        ] {
            let line = c.classify(raw.as_bytes());
            assert_eq!(line.kind, kind);
            assert_eq!(line.text, raw);
        }
    }
}
