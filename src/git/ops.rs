//! Mutations of the index. Each is a [`GitOp`] so the reducer can name the
//! work, and the refresh planner can say what it invalidates.

use super::diff::terminal_safe;
use super::runner::{GitCommand, GitRunner};
use super::status::{EntryKind, FileEntry};

/// One index-changing Git operation. Payloads are repo file names, in argv order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitOp {
    Stage(Vec<String>),
    Unstage(Vec<String>),
    StageAll,
    UnstageAll,
}

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
            return Err("Resolve conflicts before staging everything".into());
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

/// The argv for `op`. Unlike reads, these need `index.lock`, so
/// `GIT_OPTIONAL_LOCKS=0` is not set. File names come from the repo, so they
/// are read literally, never as globs or pathspec magic.
pub fn op_command(op: &GitOp) -> GitCommand {
    let with_paths =
        |cmd: GitCommand, paths: &[String]| paths.iter().fold(cmd.arg("--"), |cmd, p| cmd.arg(p));
    let cmd = match op {
        GitOp::Stage(paths) => with_paths(GitCommand::new("add"), paths),
        GitOp::Unstage(paths) => with_paths(GitCommand::new("reset").arg("-q"), paths),
        GitOp::StageAll => GitCommand::new("add").arg("-A"),
        GitOp::UnstageAll => GitCommand::new("reset").arg("-q"),
    };
    cmd.env("GIT_LITERAL_PATHSPECS", "1").env("LC_ALL", "C")
}

/// Run `op`. Failures carry git's message, made safe to draw since it can
/// quote repo-controlled paths.
pub fn run_op(runner: &dyn GitRunner, op: &GitOp) -> Result<(), String> {
    let cmd = op_command(op);
    let output = runner.run(&cmd).map_err(|e| e.to_string())?;
    if output.success {
        return Ok(());
    }
    let message = terminal_safe(&output.stderr);
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
        Some(GitOp::Stage(paths.iter().map(|p| p.to_string()).collect()))
    }

    fn unstage(paths: &[&str]) -> Option<GitOp> {
        Some(GitOp::Unstage(
            paths.iter().map(|p| p.to_string()).collect(),
        ))
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

    fn paths(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|p| p.to_string()).collect()
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
                .into_iter()
                .map(|f| {
                    let xy = f.status_letters();
                    (f.path, xy)
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
}
