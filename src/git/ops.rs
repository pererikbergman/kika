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
    ///
    /// With `amend`, the staged changes are folded into `HEAD` instead; a
    /// `None` message keeps `HEAD`'s, and is only valid with `amend`. With
    /// `no_verify`, git skips the pre-commit and commit-msg hooks, and only
    /// then, since a hook is the user's own check.
    Commit {
        message: Option<String>,
        stage_all: bool,
        amend: bool,
        no_verify: bool,
    },
    /// Apply `patch` to the index only, from stdin; with `reverse`, take it
    /// back out. How a hunk or some lines are staged or unstaged.
    ApplyCached {
        patch: Vec<u8>,
        reverse: bool,
    },
    /// Throw away the worktree changes to these files: each goes back to its
    /// index version, which is `HEAD` when nothing is staged. The index is
    /// not touched.
    DiscardWorktree(Vec<RepoPath>),
    /// Delete these untracked files from disk. Ignored files are never
    /// removed.
    DeleteUntracked(Vec<RepoPath>),
    /// Take `patch` back out of the worktree, from stdin. How a hunk or some
    /// lines are discarded; the index is not touched.
    ApplyWorktree {
        patch: Vec<u8>,
    },
    /// Apply a hunk the user edited to the index, from stdin. Its `@@`
    /// counts are recounted, since the edit invalidates them; git applies
    /// all or nothing, so a broken edit leaves the index as it was.
    ApplyEdited {
        patch: Vec<u8>,
    },
    /// Record the submodule's URL in the parent's config. It stays
    /// uninitialized in the list until it is cloned by an update.
    SubmoduleInit(RepoPath),
    /// Clone the submodule if needed and check out the commit the parent
    /// records, initializing it first.
    SubmoduleUpdate(RepoPath),
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

    /// What discarding `entry` from the Files panel does: delete it when
    /// untracked, else restore its worktree from the index. Only the current
    /// path is restored, so a staged rename stays staged. Conflicts are never
    /// touched, and a file with only staged changes has nothing to discard.
    pub fn discard_for(entry: &FileEntry) -> Result<Option<GitOp>, &'static str> {
        match entry.kind {
            EntryKind::Conflicted => Err("Resolve the conflict first"),
            EntryKind::Untracked => Ok(Some(GitOp::DeleteUntracked(vec![entry.path.clone()]))),
            _ if entry.worktree == '.' => Err("No unstaged changes to discard"),
            _ => Ok(Some(GitOp::DiscardWorktree(vec![entry.path.clone()]))),
        }
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
        GitOp::DiscardWorktree(paths) => with_paths(GitCommand::new("checkout").arg("-q"), paths),
        // Status lists untracked files one by one, so `-d` is never needed;
        // without `-x`, ignored files are left alone.
        GitOp::DeleteUntracked(paths) => {
            with_paths(GitCommand::new("clean").arg("-f").arg("-q"), paths)
        }
        // Without `--cached`, git applies to the worktree only.
        GitOp::ApplyWorktree { patch } => GitCommand::new("apply")
            .arg("--reverse")
            .arg("--whitespace=nowarn")
            .arg("-")
            .stdin(patch.clone()),
        GitOp::ApplyEdited { patch } => GitCommand::new("apply")
            .arg("--cached")
            .arg("--recount")
            .arg("--whitespace=nowarn")
            .arg("-")
            .stdin(patch.clone()),
        GitOp::SubmoduleInit(path) => with_paths(
            GitCommand::new("submodule").arg("init"),
            std::slice::from_ref(path),
        ),
        // Git has no terminal while Kika holds the screen, so a credential
        // prompt would hang; it fails instead.
        GitOp::SubmoduleUpdate(path) => with_paths(
            GitCommand::new("submodule").arg("update").arg("--init"),
            std::slice::from_ref(path),
        )
        .env("GIT_TERMINAL_PROMPT", "0"),
        GitOp::Commit {
            message,
            amend,
            no_verify,
            ..
        } => {
            let cmd = GitCommand::new("commit");
            let cmd = if *amend { cmd.arg("--amend") } else { cmd };
            let cmd = if *no_verify {
                cmd.arg("--no-verify")
            } else {
                cmd
            };
            return match message {
                Some(message) => cmd.arg("-F").arg("-").stdin(message.clone().into_bytes()),
                None => cmd.arg("--no-edit"),
            };
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
    fn submodule_ops_name_one_path_literally_and_update_never_prompts() {
        let literal = ("GIT_LITERAL_PATHSPECS".into(), "1".into());
        let c = ("LC_ALL".into(), "C".into());
        let no_prompt = ("GIT_TERMINAL_PROMPT".into(), "0".into());
        let cases = [
            (
                GitOp::SubmoduleInit("libs/a b".into()),
                &["submodule", "init", "--", "libs/a b"][..],
                vec![literal.clone(), c.clone()],
            ),
            (
                GitOp::SubmoduleUpdate("libs/a b".into()),
                &["submodule", "update", "--init", "--", "libs/a b"][..],
                vec![no_prompt, literal, c],
            ),
        ];
        for (op, argv, env) in cases {
            let runner = FakeRunner::new().expect(argv, ok(b""));
            assert_eq!(run_op(&runner, &op), Ok(()), "{op:?}");
            runner.assert_done();
            assert_eq!(runner.envs(), [env], "{op:?}");
            assert_eq!(runner.inputs(), [None], "{op:?}");
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

    #[test]
    fn discard_for_picks_the_op_or_says_why_not() {
        let modified = entry("a.rs", '.', 'M', EntryKind::Changed);
        let partly = entry("mm.rs", 'M', 'M', EntryKind::Changed);
        let deleted = entry("gone.rs", '.', 'D', EntryKind::Changed);
        let untracked = entry("new.rs", '?', '?', EntryKind::Untracked);
        let staged = entry("s.rs", 'M', '.', EntryKind::Changed);
        let conflicted = entry("both.rs", 'U', 'U', EntryKind::Conflicted);
        let mut renamed = entry("new_name.rs", 'R', 'M', EntryKind::Renamed);
        renamed.orig_path = Some("old_name.rs".into());
        let discard = |p: &str| Ok(Some(GitOp::DiscardWorktree(vec![p.into()])));
        assert_eq!(GitOp::discard_for(&modified), discard("a.rs"));
        assert_eq!(GitOp::discard_for(&partly), discard("mm.rs"));
        assert_eq!(GitOp::discard_for(&deleted), discard("gone.rs"));
        assert_eq!(GitOp::discard_for(&renamed), discard("new_name.rs"));
        assert_eq!(
            GitOp::discard_for(&untracked),
            Ok(Some(GitOp::DeleteUntracked(vec!["new.rs".into()])))
        );
        assert_eq!(
            GitOp::discard_for(&staged),
            Err("No unstaged changes to discard")
        );
        assert_eq!(
            GitOp::discard_for(&conflicted),
            Err("Resolve the conflict first")
        );
    }

    #[test]
    fn discard_ops_issue_exact_argv_with_literal_pathspecs() {
        let cases: [(GitOp, &[&str]); 2] = [
            (
                GitOp::DiscardWorktree(paths(&["a", "-b.rs"])),
                &["checkout", "-q", "--", "a", "-b.rs"],
            ),
            (
                GitOp::DeleteUntracked(paths(&["*.rs"])),
                &["clean", "-f", "-q", "--", "*.rs"],
            ),
        ];
        for (op, argv) in cases {
            let runner = FakeRunner::new().expect(argv, ok(b""));
            assert_eq!(run_op(&runner, &op), Ok(()), "{op:?}");
            runner.assert_done();
            assert_eq!(runner.inputs(), [None], "{op:?}");
            assert_eq!(
                runner.envs(),
                [vec![
                    ("GIT_LITERAL_PATHSPECS".into(), "1".into()),
                    ("LC_ALL".into(), "C".into())
                ]],
                "{op:?}"
            );
        }
    }

    #[test]
    fn apply_worktree_sends_the_reverse_patch_on_stdin_without_cached() {
        let runner =
            FakeRunner::new().expect(&["apply", "--reverse", "--whitespace=nowarn", "-"], ok(b""));
        let op = GitOp::ApplyWorktree {
            patch: b"the patch\n".to_vec(),
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

    #[test]
    fn apply_edited_recounts_and_sends_the_patch_on_stdin() {
        let runner = FakeRunner::new().expect(
            &["apply", "--cached", "--recount", "--whitespace=nowarn", "-"],
            ok(b""),
        );
        let op = GitOp::ApplyEdited {
            patch: b"the patch\n".to_vec(),
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

    /// `e.txt` committed as `one`, then `two` and `three` added after it.
    fn edit_repo() -> crate::git::fake::TempDir {
        use crate::git::fake::{TempDir, git};

        let tmp = TempDir::new("edited");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        std::fs::write(dir.join("e.txt"), "one\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "init"]);
        std::fs::write(dir.join("e.txt"), "one\ntwo\nthree\n").unwrap();
        tmp
    }

    #[test]
    fn an_edited_hunk_with_stale_counts_stages_exactly_what_is_left_in_real_git() {
        use crate::git::fake::git;

        let tmp = edit_repo();
        let dir = tmp.path();
        let runner = crate::git::CliRunner::isolated(dir);
        // `+three` was deleted in the editor; the counts still say 3.
        let patch = b"diff --git a/e.txt b/e.txt\n--- a/e.txt\n+++ b/e.txt\n\
            @@ -1 +1,3 @@\n one\n+two\n"
            .to_vec();
        run_op(&runner, &GitOp::ApplyEdited { patch }).unwrap();
        assert_eq!(git(dir, &["show", ":e.txt"]), b"one\ntwo\n");
        assert_eq!(
            std::fs::read_to_string(dir.join("e.txt")).unwrap(),
            "one\ntwo\nthree\n"
        );
    }

    #[test]
    fn a_garbage_edit_fails_and_leaves_the_index_alone_in_real_git() {
        use crate::git::fake::git;

        let tmp = edit_repo();
        let dir = tmp.path();
        let runner = crate::git::CliRunner::isolated(dir);
        let header = "diff --git a/e.txt b/e.txt\n--- a/e.txt\n+++ b/e.txt\n";
        for body in ["garbage\n", "@@ -1 +1,2 @@\n nope\n+two\n"] {
            let patch = format!("{header}{body}").into_bytes();
            let result = run_op(&runner, &GitOp::ApplyEdited { patch });
            assert!(result.is_err(), "{body:?}: {result:?}");
            assert_eq!(git(dir, &["diff", "--cached"]), b"", "{body:?}");
        }
    }

    /// `d.txt` committed as `head`, `staged` in the index, `work` in the
    /// worktree.
    fn staged_and_changed_repo() -> crate::git::fake::TempDir {
        use crate::git::fake::{TempDir, git};

        let tmp = TempDir::new("discard");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        std::fs::write(dir.join("d.txt"), "head\n").unwrap();
        std::fs::write(dir.join("other.txt"), "head\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "init"]);
        std::fs::write(dir.join("d.txt"), "staged\n").unwrap();
        git(dir, &["add", "--", "d.txt"]);
        std::fs::write(dir.join("d.txt"), "work\n").unwrap();
        std::fs::write(dir.join("other.txt"), "other work\n").unwrap();
        tmp
    }

    #[test]
    fn discard_worktree_restores_the_index_version_and_keeps_staged_content_in_real_git() {
        use crate::git::fake::git;

        let tmp = staged_and_changed_repo();
        let dir = tmp.path();
        let runner = crate::git::CliRunner::isolated(dir);
        run_op(&runner, &GitOp::DiscardWorktree(paths(&["d.txt"]))).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("d.txt")).unwrap(),
            "staged\n"
        );
        assert_eq!(git(dir, &["show", ":d.txt"]), b"staged\n");
        // Only the named file goes back.
        assert_eq!(
            std::fs::read_to_string(dir.join("other.txt")).unwrap(),
            "other work\n"
        );

        // A file deleted in the worktree comes back too.
        std::fs::remove_file(dir.join("other.txt")).unwrap();
        run_op(&runner, &GitOp::DiscardWorktree(paths(&["other.txt"]))).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("other.txt")).unwrap(),
            "head\n"
        );
    }

    #[test]
    fn apply_worktree_removes_only_the_selected_lines_and_leaves_the_index_in_real_git() {
        use crate::git::diff::{DiffRequest, DiffSide, load_patch};
        use crate::git::fake::{TempDir, git};
        use crate::git::patch::{Direction, LineId, PatchSelection, build};

        let tmp = TempDir::new("discard-lines");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        let lines: String = (1..=20).map(|i| format!("line {i}\n")).collect();
        std::fs::write(dir.join("l.txt"), &lines).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "init"]);
        let changed = lines
            .replace("line 1\n", "line one\n")
            .replace("line 10\n", "ten a\nten b\n");
        std::fs::write(dir.join("l.txt"), &changed).unwrap();
        let runner = crate::git::CliRunner::isolated(dir);
        let request = DiffRequest {
            path: "l.txt".into(),
            orig_path: None,
            side: DiffSide::Worktree,
            options: Default::default(),
        };
        let patch = load_patch(&runner, &request).unwrap();
        // `ten a` is the 10th line of the new side.
        let selection = PatchSelection::Lines([LineId::added(10)].into());
        let reverse = build(&patch, &selection, Direction::Unstage).unwrap();
        run_op(&runner, &GitOp::ApplyWorktree { patch: reverse }).unwrap();

        let expected = lines
            .replace("line 1\n", "line one\n")
            .replace("line 10\n", "ten b\n");
        assert_eq!(
            std::fs::read_to_string(dir.join("l.txt")).unwrap(),
            expected
        );
        assert_eq!(git(dir, &["show", ":l.txt"]), lines.as_bytes());
    }

    #[test]
    fn delete_untracked_on_a_glob_name_deletes_only_that_file_in_real_git() {
        use crate::git::fake::{TempDir, git};

        let tmp = TempDir::new("clean");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        std::fs::write(dir.join("tracked.rs"), "keep\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "init"]);
        for name in ["*.rs", "a.rs", ":(glob)b.rs", "b.rs"] {
            std::fs::write(dir.join(name), "new\n").unwrap();
        }
        let runner = crate::git::CliRunner::isolated(dir);
        run_op(&runner, &GitOp::DeleteUntracked(paths(&["*.rs"]))).unwrap();
        run_op(&runner, &GitOp::DeleteUntracked(paths(&[":(glob)b.rs"]))).unwrap();
        assert!(!dir.join("*.rs").exists());
        assert!(!dir.join(":(glob)b.rs").exists());
        for name in ["a.rs", "b.rs", "tracked.rs"] {
            assert!(dir.join(name).exists(), "{name}");
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
            options: Default::default(),
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
            options: Default::default(),
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
            options: Default::default(),
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
            message: Some(message.into()),
            stage_all,
            amend: false,
            no_verify: false,
        }
    }

    #[test]
    fn amend_and_skip_hooks_add_their_flags_before_the_message_source() {
        let op = |message: Option<&str>, amend, no_verify| GitOp::Commit {
            message: message.map(Into::into),
            stage_all: false,
            amend,
            no_verify,
        };
        /// The op, its argv and its stdin.
        type Case = (GitOp, &'static [&'static str], Option<&'static [u8]>);
        let cases: [Case; 5] = [
            (
                op(Some("m\n"), false, false),
                &["commit", "-F", "-"],
                Some(b"m\n"),
            ),
            (
                op(Some("m\n"), true, false),
                &["commit", "--amend", "-F", "-"],
                Some(b"m\n"),
            ),
            (
                op(None, true, false),
                &["commit", "--amend", "--no-edit"],
                None,
            ),
            (
                op(Some("m\n"), false, true),
                &["commit", "--no-verify", "-F", "-"],
                Some(b"m\n"),
            ),
            (
                op(None, true, true),
                &["commit", "--amend", "--no-verify", "--no-edit"],
                None,
            ),
        ];
        for (op, argv, input) in cases {
            let runner = FakeRunner::new().expect(argv, ok(b""));
            assert_eq!(run_op(&runner, &op), Ok(()), "{op:?}");
            runner.assert_done();
            assert_eq!(runner.envs(), [vec![]], "{op:?}");
            assert_eq!(runner.inputs(), [input.map(<[u8]>::to_vec)], "{op:?}");
        }
    }

    /// One commit of `a.txt` as `one`, with an identity, no signing and only
    /// this repo's hooks, then `a.txt` changed to `two` and staged.
    fn committed_and_staged_repo() -> crate::git::fake::TempDir {
        use crate::git::fake::{TempDir, git};

        let tmp = TempDir::new("amend");
        let dir = tmp.path();
        git(dir, &["init", "-q", "--template="]);
        for (key, value) in [
            ("user.name", "Kika Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
            ("core.hooksPath", ".git/hooks"),
        ] {
            git(dir, &["config", key, value]);
        }
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "first\n\nbody"]);
        std::fs::write(dir.join("a.txt"), "two\n").unwrap();
        git(dir, &["add", "-A"]);
        tmp
    }

    #[test]
    fn amend_folds_the_staged_change_into_head_and_keeps_or_replaces_the_message_in_real_git() {
        use crate::git::fake::git;

        let tmp = committed_and_staged_repo();
        let dir = tmp.path();
        let runner = crate::git::CliRunner::isolated(dir);
        let amend = |message: Option<&str>| GitOp::Commit {
            message: message.map(Into::into),
            stage_all: false,
            amend: true,
            no_verify: false,
        };

        run_op(&runner, &amend(None)).unwrap();
        assert_eq!(git(dir, &["rev-list", "--count", "HEAD"]), b"1\n");
        assert_eq!(git(dir, &["show", "HEAD:a.txt"]), b"two\n");
        assert_eq!(
            git(dir, &["log", "-1", "--format=%B"]),
            b"first\n\nbody\n\n"
        );
        assert_eq!(git(dir, &["diff", "--cached"]), b"");

        run_op(&runner, &amend(Some("new\n"))).unwrap();
        assert_eq!(git(dir, &["rev-list", "--count", "HEAD"]), b"1\n");
        assert_eq!(git(dir, &["log", "-1", "--format=%B"]), b"new\n\n");
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_pre_commit_hook_blocks_a_commit_unless_hooks_are_skipped_in_real_git() {
        use crate::git::fake::git;
        use std::os::unix::fs::PermissionsExt;

        let tmp = committed_and_staged_repo();
        let dir = tmp.path();
        let hooks = dir.join(".git").join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("pre-commit");
        std::fs::write(&hook, "#!/bin/sh\necho 'lint failed'\nexit 1\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        let runner = crate::git::CliRunner::isolated(dir);
        let commit = |no_verify| GitOp::Commit {
            message: Some("second\n".into()),
            stage_all: false,
            amend: false,
            no_verify,
        };

        let blocked = run_op(&runner, &commit(false));
        assert!(
            blocked.as_ref().is_err_and(|e| e.contains("lint failed")),
            "{blocked:?}"
        );
        assert_eq!(git(dir, &["rev-list", "--count", "HEAD"]), b"1\n");

        run_op(&runner, &commit(true)).unwrap();
        assert_eq!(git(dir, &["rev-list", "--count", "HEAD"]), b"2\n");
        assert_eq!(git(dir, &["log", "-1", "--format=%B"]), b"second\n\n");
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
