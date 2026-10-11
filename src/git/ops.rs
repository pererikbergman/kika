//! Mutations of the index and history. Each is a [`GitOp`] so the reducer can
//! name the work, and the refresh planner can say what it invalidates.

use super::diff::terminal_safe;
use super::path::RepoPath;
use super::runner::{GitCommand, GitRunner};
use super::status::{EntryKind, FileEntry};

/// One repository-changing Git operation. Path payloads are repo file names,
/// in argv order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitOp {
    Stage(Vec<RepoPath>),
    Unstage(Vec<RepoPath>),
    StageAll,
    UnstageAll,
    /// Plain `git commit` with `message` on stdin, so the user's hooks,
    /// signing and cleanup config apply exactly as on the command line.
    /// With `stage_all`, everything is staged first, at submit time.
    ///
    /// Git runs without the terminal while Kika holds the screen, so signing
    /// and hooks that need no terminal work (ssh-agent, gpg-agent with a
    /// cached passphrase or a GUI pinentry), but a terminal pinentry is not
    /// supported: it fails, or blocks and shows as "Committing…" until git
    /// exits or Ctrl+C quits Kika, which stops git and its hooks.
    Commit {
        message: String,
        stage_all: bool,
    },
    /// Apply `patch` to the index only, from stdin; with `reverse`, take it
    /// back out. How a hunk or some lines are staged or unstaged.
    ApplyCached {
        patch: Vec<u8>,
        reverse: bool,
    },
}

/// Why staging everything is refused while conflicts exist.
pub const CONFLICTS_REFUSAL: &str = "Resolve conflicts before staging everything";

impl GitOp {
    /// What one key press does to `entry`: stage it while it has unstaged
    /// changes, else unstage it. Conflicts are never touched, since staging
    /// one would mark it resolved.
    pub fn toggle_for(entry: &FileEntry) -> Option<GitOp> {
        if entry.kind == EntryKind::Conflicted {
            return None;
        }
        if has_unstaged(entry) {
            return Some(GitOp::Stage(vec![entry.path.clone()]));
        }
        // A staged rename also deletes its old path, which must be unstaged too.
        let paths = entry
            .orig_path
            .iter()
            .chain([&entry.path])
            .cloned()
            .collect();
        Some(GitOp::Unstage(paths))
    }

    /// What the stage-all key does: stage everything while anything is
    /// unstaged, else unstage everything. Refused while conflicts exist,
    /// because `add -A` would mark them resolved and `reset` would drop their
    /// conflict stages.
    pub fn toggle_all(files: &[FileEntry]) -> Result<Option<GitOp>, String> {
        if files.iter().any(|f| f.kind == EntryKind::Conflicted) {
            return Err(CONFLICTS_REFUSAL.into());
        }
        if files.is_empty() {
            return Ok(None);
        }
        Ok(Some(if files.iter().any(has_unstaged) {
            GitOp::StageAll
        } else {
            GitOp::UnstageAll
        }))
    }
}

/// The command for `op`. Unlike reads, these need `index.lock`, so
/// `GIT_OPTIONAL_LOCKS=0` is not set. File names come from the repo, so they
/// are read literally, never as globs or pathspec magic. A commit adds no
/// env at all: its hooks inherit it, and must see what they would see
/// outside Kika. Git runs hooks with stdin closed, so the message on stdin
/// never reaches them.
pub fn op_command(op: &GitOp) -> GitCommand {
    let with_paths = |cmd: GitCommand, paths: &[RepoPath]| {
        paths
            .iter()
            .fold(cmd.arg("--"), |cmd, p| cmd.arg(p.as_os_str()))
    };
    let cmd = match op {
        GitOp::Stage(paths) => with_paths(GitCommand::new("add"), paths),
        GitOp::Unstage(paths) => with_paths(GitCommand::new("reset").arg("-q"), paths),
        GitOp::StageAll => GitCommand::new("add").arg("-A"),
        GitOp::UnstageAll => GitCommand::new("reset").arg("-q"),
        // `--whitespace=nowarn` so the user's `apply.whitespace` can never
        // reject or rewrite the patch.
        GitOp::ApplyCached { patch, reverse } => {
            let cmd = GitCommand::new("apply").arg("--cached");
            let cmd = if *reverse { cmd.arg("--reverse") } else { cmd };
            cmd.arg("--whitespace=nowarn").arg("-").stdin(patch.clone())
        }
        GitOp::Commit { message, .. } => {
            return GitCommand::new("commit")
                .arg("-F")
                .arg("-")
                .stdin(message.clone().into_bytes());
        }
    };
    cmd.env("GIT_LITERAL_PATHSPECS", "1").env("LC_ALL", "C")
}

/// Run `op`. Failures carry git's message, made safe to draw since it can
/// quote repo-controlled paths. A commit's failure also carries stdout:
/// hooks print to stderr, but git says "nothing to commit" on stdout.
pub fn run_op(runner: &dyn GitRunner, op: &GitOp) -> Result<(), String> {
    if let GitOp::Commit {
        stage_all: true, ..
    } = op
    {
        run_op(runner, &GitOp::StageAll)?;
    }
    let cmd = op_command(op);
    let output = runner.run(&cmd).map_err(|e| e.to_string())?;
    if output.success {
        return Ok(());
    }
    let message = match op {
        GitOp::Commit { .. } => [&output.stderr, &output.stdout]
            .map(|bytes| terminal_safe(bytes))
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => terminal_safe(&output.stderr),
    };
    if message.is_empty() {
        let subcommand = cmd.args()[0].to_string_lossy();
        return Err(format!("git {subcommand} failed"));
    }
    Err(message)
}

/// Untracked, or changed in the worktree (including partly staged files).
fn has_unstaged(entry: &FileEntry) -> bool {
    entry.kind == EntryKind::Untracked || entry.worktree != '.'
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::fake::{FakeRunner, fail, ok};
    use crate::git::runner::GitOutput;

    fn entry(path: &str, index: char, worktree: char, kind: EntryKind) -> FileEntry {
        FileEntry {
            path: path.into(),
            orig_path: None,
            index,
            worktree,
            kind,
        }
    }

    fn stage(paths: &[&str]) -> Option<GitOp> {
        Some(GitOp::Stage(paths.iter().map(|&p| p.into()).collect()))
    }

    fn unstage(paths: &[&str]) -> Option<GitOp> {
        Some(GitOp::Unstage(paths.iter().map(|&p| p.into()).collect()))
    }

    #[test]
    fn files_with_unstaged_changes_are_staged() {
        let unstaged = entry("a.rs", '.', 'M', EntryKind::Changed);
        let untracked = entry("new.rs", '?', '?', EntryKind::Untracked);
        let partly = entry("mm.rs", 'M', 'M', EntryKind::Changed);
        assert_eq!(GitOp::toggle_for(&unstaged), stage(&["a.rs"]));
        assert_eq!(GitOp::toggle_for(&untracked), stage(&["new.rs"]));
        assert_eq!(GitOp::toggle_for(&partly), stage(&["mm.rs"]));
    }

    #[test]
    fn staged_only_files_are_unstaged() {
        let staged = entry("a.rs", 'M', '.', EntryKind::Changed);
        assert_eq!(GitOp::toggle_for(&staged), unstage(&["a.rs"]));
    }

    #[test]
    fn a_staged_rename_unstages_both_paths() {
        let mut renamed = entry("new.rs", 'R', '.', EntryKind::Renamed);
        renamed.orig_path = Some("old.rs".into());
        assert_eq!(GitOp::toggle_for(&renamed), unstage(&["old.rs", "new.rs"]));
    }

    #[test]
    fn conflicted_files_are_left_alone() {
        let conflicted = entry("both.rs", 'U', 'U', EntryKind::Conflicted);
        assert_eq!(GitOp::toggle_for(&conflicted), None);
    }

    #[test]
    fn toggle_all_stages_everything_while_anything_is_unstaged() {
        let staged = entry("a.rs", 'M', '.', EntryKind::Changed);
        for other in [
            entry("b.rs", '.', 'M', EntryKind::Changed),
            entry("c.rs", '?', '?', EntryKind::Untracked),
        ] {
            let files = [staged.clone(), other];
            assert_eq!(GitOp::toggle_all(&files), Ok(Some(GitOp::StageAll)));
        }
    }

    #[test]
    fn toggle_all_unstages_everything_when_all_is_staged() {
        let files = [
            entry("a.rs", 'M', '.', EntryKind::Changed),
            entry("b.rs", 'A', '.', EntryKind::Changed),
        ];
        assert_eq!(GitOp::toggle_all(&files), Ok(Some(GitOp::UnstageAll)));
    }

    #[test]
    fn toggle_all_on_a_clean_tree_does_nothing() {
        assert_eq!(GitOp::toggle_all(&[]), Ok(None));
    }

    #[test]
    fn toggle_all_refuses_while_conflicts_exist() {
        let files = [
            entry("a.rs", '.', 'M', EntryKind::Changed),
            entry("both.rs", 'U', 'U', EntryKind::Conflicted),
        ];
        assert_eq!(
            GitOp::toggle_all(&files),
            Err("Resolve conflicts before staging everything".into())
        );
    }

    fn paths(paths: &[&str]) -> Vec<RepoPath> {
        paths.iter().map(|&p| p.into()).collect()
    }

    #[test]
    fn run_op_issues_exact_argv_with_literal_pathspecs() {
        let cases: [(GitOp, &[&str]); 4] = [
            (
                GitOp::Stage(paths(&["a", "b.rs"])),
                &["add", "--", "a", "b.rs"],
            ),
            (
                GitOp::Unstage(paths(&["old.rs", "new.rs"])),
                &["reset", "-q", "--", "old.rs", "new.rs"],
            ),
            (GitOp::StageAll, &["add", "-A"]),
            (GitOp::UnstageAll, &["reset", "-q"]),
        ];
        for (op, argv) in cases {
            let runner = FakeRunner::new().expect(argv, ok(b""));
            assert_eq!(run_op(&runner, &op), Ok(()), "{op:?}");
            runner.assert_done();
            let envs = runner.envs();
            assert!(
                envs[0].contains(&("GIT_LITERAL_PATHSPECS".into(), "1".into())),
                "{op:?}: {envs:?}"
            );
            assert!(
                envs[0].contains(&("LC_ALL".into(), "C".into())),
                "{op:?}: {envs:?}"
            );
        }
    }

    #[test]
    fn apply_cached_sends_the_patch_on_stdin() {
        for (reverse, argv) in [
            (
                false,
                &["apply", "--cached", "--whitespace=nowarn", "-"][..],
            ),
            (
                true,
                &["apply", "--cached", "--reverse", "--whitespace=nowarn", "-"][..],
            ),
        ] {
            let runner = FakeRunner::new().expect(argv, ok(b""));
            let op = GitOp::ApplyCached {
                patch: b"the patch\n".to_vec(),
                reverse,
            };
            assert_eq!(run_op(&runner, &op), Ok(()));
            runner.assert_done();
            assert_eq!(runner.inputs(), [Some(b"the patch\n".to_vec())]);
            assert_eq!(
                runner.envs(),
                [vec![
                    ("GIT_LITERAL_PATHSPECS".into(), "1".into()),
                    ("LC_ALL".into(), "C".into())
                ]]
            );
        }
    }

    /// Stage hunk 1 of a two-hunk file through the real patch engine and
    /// `git apply`, then unstage it again from the staged diff.
    fn stage_and_unstage_one_hunk(name: &str) {
        use crate::git::diff::{DiffRequest, DiffSide, load_patch};
        use crate::git::fake::{TempDir, git};
        use crate::git::patch::{Direction, PatchSelection, build};

        let tmp = TempDir::new("apply");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        let lines: Vec<String> = (1..=20).map(|i| format!("line {i}\n")).collect();
        std::fs::write(dir.join(name), lines.concat()).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "init"]);
        let mut changed = lines.clone();
        changed[0] = "line 1a\nline 1b\n".into();
        changed[19] = "line twenty\n".into();
        std::fs::write(dir.join(name), changed.concat()).unwrap();
        let runner = crate::git::CliRunner::isolated(dir);
        let request = |side| DiffRequest {
            path: name.into(),
            orig_path: None,
            side,
        };
        let cached = || String::from_utf8(git(dir, &["diff", "--cached"])).unwrap();
        let worktree = || String::from_utf8(git(dir, &["diff"])).unwrap();

        let patch = load_patch(&runner, &request(DiffSide::Worktree)).unwrap();
        let hunk = build(&patch, &PatchSelection::Hunk(1), Direction::Stage).unwrap();
        let stage = GitOp::ApplyCached {
            patch: hunk,
            reverse: false,
        };
        run_op(&runner, &stage).unwrap();
        let (index, work) = (cached(), worktree());
        assert!(
            index.contains("+line twenty") && !index.contains("+line 1a"),
            "{index}"
        );
        assert!(
            work.contains("+line 1a") && !work.contains("line twenty"),
            "{work}"
        );

        // Stage the rest too, then take only hunk 0 back out of the index.
        git(dir, &["add", "--", name]);
        let patch = load_patch(&runner, &request(DiffSide::Index)).unwrap();
        assert_eq!(patch.hunks.len(), 2);
        let hunk = build(&patch, &PatchSelection::Hunk(0), Direction::Unstage).unwrap();
        let unstage = GitOp::ApplyCached {
            patch: hunk,
            reverse: true,
        };
        run_op(&runner, &unstage).unwrap();
        let (index, work) = (cached(), worktree());
        assert!(
            index.contains("+line twenty") && !index.contains("+line 1a"),
            "{index}"
        );
        assert!(
            work.contains("+line 1a") && !work.contains("line twenty"),
            "{work}"
        );
    }

    #[test]
    fn a_built_hunk_stages_and_unstages_only_itself_in_real_git() {
        stage_and_unstage_one_hunk("a.txt");
    }

    /// git quotes this name in the diff header; the patch keeps it quoted.
    #[cfg(unix)]
    #[test]
    fn a_hunk_of_a_file_whose_name_needs_quoting_stages_in_real_git() {
        stage_and_unstage_one_hunk("tab\there \"q\".txt");
    }

    /// Stage one added line of a later hunk, then unstage one line from the
    /// staged view, each time through the real engine and `git apply`.
    #[test]
    fn single_lines_stage_and_unstage_alone_in_real_git() {
        use crate::git::diff::{DiffRequest, DiffSide, load_patch};
        use crate::git::fake::{TempDir, git};
        use crate::git::patch::{Direction, FilePatch, LineId, PatchSelection, build};

        /// The id of the one changed line whose text is `text`.
        fn id_of(patch: &FilePatch, text: &str) -> LineId {
            let wanted = format!("{text}\n");
            patch
                .hunks
                .iter()
                .flat_map(|h| h.lines.iter().zip(h.line_ids()))
                .find_map(|(line, id)| id.filter(|_| line.text == wanted.as_bytes()))
                .unwrap()
        }

        let tmp = TempDir::new("lines");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        let lines: Vec<String> = (1..=20).map(|i| format!("line {i}\n")).collect();
        std::fs::write(dir.join("l.txt"), lines.concat()).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "init"]);
        let mut changed = lines.clone();
        changed[0] = "line 1a\nline 1b\n".into();
        changed[14] = "fifteen a\nfifteen b\n".into();
        std::fs::write(dir.join("l.txt"), changed.concat()).unwrap();
        let runner = crate::git::CliRunner::isolated(dir);
        let request = |side| DiffRequest {
            path: "l.txt".into(),
            orig_path: None,
            side,
        };
        let apply = |patch: &FilePatch, text: &str, direction| {
            let selection = PatchSelection::Lines([id_of(patch, text)].into());
            let op = GitOp::ApplyCached {
                patch: build(patch, &selection, direction).unwrap(),
                reverse: direction == Direction::Unstage,
            };
            run_op(&runner, &op).unwrap();
        };
        let index = || String::from_utf8(git(dir, &["show", ":l.txt"])).unwrap();
        let with = |at: usize, text: &str| {
            let mut out = lines.clone();
            out[at] = text.into();
            out
        };

        let patch = load_patch(&runner, &request(DiffSide::Worktree)).unwrap();
        apply(&patch, "fifteen a", Direction::Stage);
        assert_eq!(index(), with(14, "line 15\nfifteen a\n").concat());
        let cached = String::from_utf8(git(dir, &["diff", "--cached"])).unwrap();
        let added: Vec<&str> = cached.lines().filter(|l| l.starts_with('+')).collect();
        assert_eq!(added, ["+++ b/l.txt", "+fifteen a"], "{cached}");
        assert!(!cached.lines().any(|l| l.starts_with("-l")), "{cached}");

        // Everything staged; take only "fifteen b" back out, past hunk 0.
        git(dir, &["add", "--", "l.txt"]);
        let patch = load_patch(&runner, &request(DiffSide::Index)).unwrap();
        assert_eq!(patch.hunks.len(), 2);
        apply(&patch, "fifteen b", Direction::Unstage);
        let mut expected = with(14, "fifteen a\n");
        expected[0] = "line 1a\nline 1b\n".into();
        assert_eq!(index(), expected.concat());
        let work = String::from_utf8(git(dir, &["diff"])).unwrap();
        let changes: Vec<&str> = work
            .lines()
            .filter(|l| l.starts_with(['+', '-']) && !l.starts_with("+++") && !l.starts_with("---"))
            .collect();
        assert_eq!(changes, ["+fifteen b"], "{work}");
    }

    /// Regression: a reverse patch must carry the index-side start of its
    /// hunk. The file holds identical blocks, and the first staged hunk adds
    /// exactly one block's worth of lines, so a start shifted as if for a
    /// forward apply lands on an identical copy and unstages that instead.
    #[test]
    fn unstaging_one_of_two_staged_hunks_next_to_repeated_blocks_takes_out_only_it() {
        use crate::git::diff::{DiffRequest, DiffSide, load_patch};
        use crate::git::fake::{TempDir, git};
        use crate::git::patch::{Direction, PatchSelection, build};

        let tmp = TempDir::new("reverse");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        let block = |middle: &str| format!("a\nb\nc\n{middle}\nd\ne\nf\n");
        let filler: String = (1..=8).map(|i| format!("filler {i}\n")).collect();
        let head = format!(
            "top\n{filler}{}{}{}{filler}",
            block("Y"),
            block("X"),
            block("Y")
        );
        std::fs::write(dir.join("r.txt"), &head).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "init"]);
        let inserted: String = (1..=7).map(|i| format!("new {i}\n")).collect();
        let changed = format!(
            "top\n{inserted}{filler}{}{}{}{filler}",
            block("Y"),
            block("Y"),
            block("Y")
        );
        std::fs::write(dir.join("r.txt"), &changed).unwrap();
        git(dir, &["add", "--", "r.txt"]);

        let runner = crate::git::CliRunner::isolated(dir);
        let request = DiffRequest {
            path: "r.txt".into(),
            orig_path: None,
            side: DiffSide::Index,
        };
        let patch = load_patch(&runner, &request).unwrap();
        assert_eq!(patch.hunks.len(), 2);
        let hunk = build(&patch, &PatchSelection::Hunk(1), Direction::Unstage).unwrap();
        let unstage = GitOp::ApplyCached {
            patch: hunk,
            reverse: true,
        };
        run_op(&runner, &unstage).unwrap();

        let index = git(dir, &["show", ":r.txt"]);
        let only_first = format!(
            "top\n{inserted}{filler}{}{}{}{filler}",
            block("Y"),
            block("X"),
            block("Y")
        );
        assert_eq!(String::from_utf8(index).unwrap(), only_first);
        let work = String::from_utf8(git(dir, &["diff"])).unwrap();
        assert!(
            work.contains("-X\n+Y\n") && !work.contains("new 1"),
            "{work}"
        );
        let cached = String::from_utf8(git(dir, &["diff", "--cached"])).unwrap();
        assert!(
            cached.contains("+new 7") && !cached.contains("+Y") && !cached.contains("-Y"),
            "{cached}"
        );
    }

    #[test]
    fn a_failed_op_returns_terminal_safe_stderr() {
        let runner = FakeRunner::new().expect(
            &["add", "--", "a"],
            fail("fatal: Unable to create '\x1b[2J.git/index.lock': File exists.\r\nhint: retry\n"),
        );
        assert_eq!(
            run_op(&runner, &GitOp::Stage(paths(&["a"]))),
            Err(
                "fatal: Unable to create '\u{FFFD}[2J.git/index.lock': File exists.\nhint: retry"
                    .into()
            )
        );
    }

    #[test]
    fn a_quiet_failure_still_has_a_message() {
        let runner = FakeRunner::new().expect(&["reset", "-q"], fail(" \n"));
        assert_eq!(
            run_op(&runner, &GitOp::UnstageAll),
            Err("git reset failed".into())
        );
    }

    fn commit(message: &str, stage_all: bool) -> GitOp {
        GitOp::Commit {
            message: message.into(),
            stage_all,
        }
    }

    #[test]
    fn commit_is_plain_git_commit_with_the_message_on_stdin_and_no_env() {
        let runner = FakeRunner::new().expect(&["commit", "-F", "-"], ok(b""));
        assert_eq!(run_op(&runner, &commit("subj\n\nbody\n", false)), Ok(()));
        runner.assert_done();
        assert_eq!(runner.envs(), [vec![]]);
        assert_eq!(runner.inputs(), [Some(b"subj\n\nbody\n".to_vec())]);
    }

    #[test]
    fn commit_with_stage_all_runs_add_first() {
        let runner = FakeRunner::new()
            .expect(&["add", "-A"], ok(b""))
            .expect(&["commit", "-F", "-"], ok(b""));
        assert_eq!(run_op(&runner, &commit("subj\n", true)), Ok(()));
        runner.assert_done();
        assert_eq!(runner.inputs(), [None, Some(b"subj\n".to_vec())]);
    }

    #[test]
    fn a_failed_stage_all_returns_its_error_and_does_not_commit() {
        let runner = FakeRunner::new().expect(&["add", "-A"], fail("fatal: index.lock\n"));
        assert_eq!(
            run_op(&runner, &commit("subj\n", true)),
            Err("fatal: index.lock".into())
        );
        // The fake panics on any call not expected, so no commit was attempted.
        runner.assert_done();
    }

    #[test]
    fn a_failed_commit_shows_stderr_then_stdout_made_safe() {
        let output = GitOutput {
            success: false,
            stdout: b"nothing added to commit\n".to_vec(),
            stderr: b"lint \x1b[31mfailed\n".to_vec(),
        };
        let runner = FakeRunner::new().expect(&["commit", "-F", "-"], output);
        assert_eq!(
            run_op(&runner, &commit("subj\n", false)),
            Err("lint \u{FFFD}[31mfailed\nnothing added to commit".into())
        );

        let stdout_only = GitOutput {
            success: false,
            stdout: b"nothing to commit, working tree clean\n".to_vec(),
            stderr: Vec::new(),
        };
        let runner = FakeRunner::new().expect(&["commit", "-F", "-"], stdout_only);
        assert_eq!(
            run_op(&runner, &commit("subj\n", false)),
            Err("nothing to commit, working tree clean".into())
        );

        let runner = FakeRunner::new().expect(&["commit", "-F", "-"], fail(""));
        assert_eq!(
            run_op(&runner, &commit("subj\n", false)),
            Err("git commit failed".into())
        );
    }

    #[test]
    fn a_file_named_like_a_glob_stages_and_unstages_only_itself_in_real_git() {
        use crate::git::fake::{TempDir, git};
        use crate::git::status::read_status;

        let tmp = TempDir::new("ops");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        for name in ["a.rs", "*.rs"] {
            std::fs::write(dir.join(name), "old\n").unwrap();
        }
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "init"]);
        for name in ["a.rs", "*.rs"] {
            std::fs::write(dir.join(name), "new\n").unwrap();
        }
        let runner = crate::git::CliRunner::new(dir);
        let letters = || {
            let mut files: Vec<_> = read_status(&runner)
                .unwrap()
                .files
                .into_iter()
                .map(|f| {
                    let xy = f.status_letters();
                    (f.path.display().to_string(), xy)
                })
                .collect();
            files.sort();
            files
        };
        let glob = paths(&["*.rs"]);

        run_op(&runner, &GitOp::Stage(glob.clone())).unwrap();
        assert_eq!(
            letters(),
            [("*.rs".into(), "M ".into()), ("a.rs".into(), " M".into())]
        );

        run_op(&runner, &GitOp::Unstage(glob)).unwrap();
        assert_eq!(
            letters(),
            [("*.rs".into(), " M".into()), ("a.rs".into(), " M".into())]
        );
    }

    /// Regression: a name that is not UTF-8 used to be decoded lossily into
    /// argv, so toggling it staged its look-alike instead.
    #[cfg(unix)]
    #[test]
    fn a_name_that_is_not_utf8_stages_and_unstages_itself_not_its_look_alike() {
        use crate::git::fake::{LOOK_ALIKE, RAW_NAME, entry_named, git, look_alike_repo};

        let tmp = look_alike_repo();
        let dir = tmp.path();
        std::fs::write(dir.join(LOOK_ALIKE), "look-alike new\n").unwrap();
        let runner = crate::git::CliRunner::new(dir);
        let letters = |raw: &[u8]| entry_named(&runner, raw).map(|e| e.status_letters());
        assert_eq!(letters(RAW_NAME).as_deref(), Some(" D"));

        let raw = entry_named(&runner, RAW_NAME).unwrap();
        run_op(&runner, &GitOp::toggle_for(&raw).unwrap()).unwrap();
        assert_eq!(letters(RAW_NAME).as_deref(), Some("D "));
        assert_eq!(letters(LOOK_ALIKE.as_bytes()).as_deref(), Some(" M"));

        git(dir, &["add", "--", LOOK_ALIKE]);
        let raw = entry_named(&runner, RAW_NAME).unwrap();
        run_op(&runner, &GitOp::toggle_for(&raw).unwrap()).unwrap();
        assert_eq!(letters(RAW_NAME).as_deref(), Some(" D"));
        assert_eq!(letters(LOOK_ALIKE.as_bytes()).as_deref(), Some("M "));
    }
}
