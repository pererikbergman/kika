//! Running a custom patch rewrite: build the patch from the source commit,
//! then rebase with a stop at each commit it changes, applying it there.
//! Every step fails closed: on any failure the rebase is aborted, or a
//! finished one reset, so `HEAD`, the index and the worktree end where
//! they began. Only hex ids and fixed words reach argv; the patch and the
//! new commit's message go on stdin, and Kika writes no file.

use std::collections::BTreeSet;

use super::commit_files::CommitId;
use super::diff::{DiffOptions, commit_patch_command, run_diff, terminal_safe};
use super::patch::{self, Direction, PatchSelection};
use super::rebase_plan::{RebasePlan, TodoAction};
use super::repo_state::{Operation, git_dir_command, git_dir_from, in_progress};
use super::runner::{GitCommand, GitRunner};
use crate::custom_patch::{CustomPatch, FilePick, PatchAction};

/// What is done at one stop of the rebase, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Work {
    /// Take the patch out of the commit: reverse-apply it, then amend.
    Reverse,
    /// Put the patch into the commit: apply it, then amend.
    Forward,
    /// Take the patch out of the commit, then commit it on its own with
    /// this message, right after.
    SplitOff(String),
    /// Check that the patch would apply here, changing nothing.
    Check,
}

/// What each `edit` stop of `plan` does, in the order git reaches them.
/// The source takes the patch out, unless it moves to an older commit,
/// where the later pick of the source finds the change already there.
/// The patch going to the index is checked at the last commit, while the
/// rebase can still be aborted, so the apply after it cannot fail.
pub fn stop_work(plan: &RebasePlan, action: &PatchAction, source: &CommitId) -> Vec<Vec<Work>> {
    let last = plan.steps.last().map(|step| &step.commit);
    plan.steps
        .iter()
        .filter(|step| step.action == TodoAction::Edit)
        .map(|step| {
            let mut work = Vec::new();
            if step.commit == *source {
                work.push(match action {
                    PatchAction::NewCommit(message) => Work::SplitOff(message.clone()),
                    _ => Work::Reverse,
                });
            }
            if matches!(action, PatchAction::MoveTo(target) if *target == step.commit) {
                work.push(Work::Forward);
            }
            if *action == PatchAction::ToIndex && Some(&step.commit) == last {
                work.push(Work::Check);
            }
            work
        })
        .collect()
}

/// One file's change in the source commit, as the patch engine reads it:
/// git's default context, whitespace kept, renames off, the name read
/// literally after `--`.
pub fn diff_command(patch: &CustomPatch, path: &super::path::RepoPath) -> GitCommand {
    commit_patch_command(&patch.source, path, DiffOptions::default())
}

/// `git apply --index` of the patch on stdin; reversed takes it out,
/// `check` only says whether it would apply. `--whitespace=nowarn` so the
/// user's `apply.whitespace` can never reject or rewrite it.
pub fn apply_command(reverse: bool, check: bool) -> GitCommand {
    let cmd = GitCommand::new("apply").arg("--index");
    let cmd = if reverse { cmd.arg("--reverse") } else { cmd };
    let cmd = if check { cmd.arg("--check") } else { cmd };
    cmd.arg("--whitespace=nowarn").arg("-").env("LC_ALL", "C")
}

/// Fold the index into the commit the rebase stopped at, keeping its
/// message. Empty is allowed: the patch may have been all it changed.
/// Hooks are skipped, as for the rebase's own picks.
pub fn amend_command() -> GitCommand {
    GitCommand::new("commit")
        .arg("--amend")
        .arg("--no-edit")
        .arg("--no-verify")
        .arg("--allow-empty")
}

/// Commit the index with `message`, on stdin.
pub fn new_commit_command(message: &str) -> GitCommand {
    GitCommand::new("commit")
        .arg("--no-verify")
        .arg("-F")
        .arg("-")
        .stdin(message.as_bytes().to_vec())
}

fn rebase_command(word: &str) -> GitCommand {
    GitCommand::new("rebase").arg(word)
}

/// Put the branch, the index and the worktree back at `orig`. `--keep`
/// refuses rather than lose a local change; the id is hex digits only.
pub fn reset_command(orig: &CommitId) -> GitCommand {
    GitCommand::new("reset")
        .arg("-q")
        .arg("--keep")
        .arg(orig.as_str())
        .arg("--")
}

fn head_command() -> GitCommand {
    GitCommand::new("rev-parse")
        .arg("-q")
        .arg("--verify")
        .arg("HEAD")
        .env("LC_ALL", "C")
}

/// The patch for every pick, one file after another, built to apply on
/// the source's parent. Any file the engine cannot take refuses the
/// whole patch, before anything changes.
pub fn build(runner: &dyn GitRunner, picks: &CustomPatch) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for (path, pick) in &picks.files {
        let stdout = run_diff(
            runner,
            &diff_command(picks, path),
            super::diff::DiffSide::Index,
        )?;
        let file = patch::parse(&stdout).map_err(|e| format!("{}: {e}", path.display()))?;
        let all: BTreeSet<_> = file
            .hunks
            .iter()
            .flat_map(patch::Hunk::line_ids)
            .flatten()
            .collect();
        let lines = match pick {
            FilePick::Whole => all.clone(),
            FilePick::Lines(lines) => lines.clone(),
        };
        // Built either way, so a file the engine cannot take is refused.
        let built = patch::build(
            &file,
            &PatchSelection::Lines(lines.clone()),
            Direction::Stage,
        )
        .map_err(|e| format!("{}: {e}", path.display()))?;
        // The whole change goes as git wrote it, with its `new file`,
        // `deleted file` and mode lines, which `git apply` needs to add or
        // delete the file and the engine leaves out.
        if lines == all {
            out.extend(stdout);
        } else {
            out.extend(built);
        }
    }
    Ok(out)
}

/// A rewrite to run: the plan, what it does, the picks and where `HEAD`
/// was when it was asked for.
pub struct Rewrite<'a> {
    pub plan: &'a RebasePlan,
    pub action: &'a PatchAction,
    pub picks: &'a CustomPatch,
    pub orig_head: &'a CommitId,
}

/// Run `rewrite`. `rebase` is the interactive rebase as it must start,
/// with Kika as the sequence editor; `attached` runs a command on the
/// terminal, for those that may make commits and so sign or ask. Blocking.
pub fn run(
    runner: &dyn GitRunner,
    rewrite: &Rewrite,
    rebase: GitCommand,
    attached: &dyn Fn(GitCommand) -> Result<(), String>,
) -> Result<(), String> {
    let head = run_checked(runner, &head_command())?;
    if String::from_utf8_lossy(&head).trim() != rewrite.orig_head.as_str() {
        return Err(HEAD_MOVED.into());
    }
    if !worktree_clean(runner)? {
        return Err(DIRTY.into());
    }
    let patch = build(runner, rewrite.picks)?;
    let work = stop_work(rewrite.plan, rewrite.action, &rewrite.picks.source.oid);
    let restore = |error: String| fail_closed(runner, rewrite.orig_head, error);
    if let Err(error) = attached(rebase) {
        return Err(restore(error));
    }
    for stop in &work {
        if !rebasing(runner)? {
            return Err(restore(NOT_STOPPED.into()));
        }
        for step in stop {
            if let Err(error) = do_work(runner, step, &patch, attached) {
                return Err(restore(error));
            }
        }
        if let Err(error) = attached(rebase_command("--continue")) {
            return Err(restore(error));
        }
    }
    if rebasing(runner)? {
        return Err(restore(EXTRA_STOP.into()));
    }
    if *rewrite.action == PatchAction::ToIndex
        && let Err(error) = apply(runner, &patch, false, false)
    {
        return Err(restore(error));
    }
    Ok(())
}

/// Whether no tracked file differs from `HEAD`, staged or not, read now:
/// the file list the question was asked with may be out of date. The
/// refresh first, so a file only touched is not taken as changed.
fn worktree_clean(runner: &dyn GitRunner) -> Result<bool, String> {
    // Its exit says some entries needed refreshing, which is no error.
    let _ = runner.run(&refresh_command());
    let output = runner.run(&dirty_command()).map_err(|e| e.to_string())?;
    if output.success {
        return Ok(true);
    }
    // `--quiet` exits 1 on a difference and says nothing; anything on
    // stderr is a real failure.
    if output.stderr.is_empty() {
        Ok(false)
    } else {
        Err(terminal_safe(&output.stderr))
    }
}

/// `git update-index -q --refresh`: the index's stat data brought up to
/// date, so the next compare reads content.
pub fn refresh_command() -> GitCommand {
    GitCommand::new("update-index")
        .arg("-q")
        .arg("--refresh")
        .env("LC_ALL", "C")
}

/// `git diff-index --quiet HEAD --`: exit 1 when a tracked file differs
/// from `HEAD` in the index or the worktree.
pub fn dirty_command() -> GitCommand {
    GitCommand::new("diff-index")
        .arg("--quiet")
        .arg("HEAD")
        .arg("--")
        .env("LC_ALL", "C")
}

/// Why nothing ran: a tracked file changed since the question was asked.
pub const DIRTY: &str = "Commit or stash your changes first; nothing was changed";

/// Why nothing ran: `HEAD` is not where it was when the user was asked.
pub const HEAD_MOVED: &str = "HEAD moved since the question was asked; nothing was changed";
const NOT_STOPPED: &str = "The rebase did not stop where it was planned to";
const EXTRA_STOP: &str = "The rebase stopped where it was not planned to, such as on a commit \
the move left empty; it was aborted";

fn do_work(
    runner: &dyn GitRunner,
    work: &Work,
    patch: &[u8],
    attached: &dyn Fn(GitCommand) -> Result<(), String>,
) -> Result<(), String> {
    match work {
        Work::Reverse => {
            apply(runner, patch, true, false)?;
            attached(amend_command())
        }
        Work::Forward => {
            apply(runner, patch, false, false)?;
            attached(amend_command())
        }
        Work::SplitOff(message) => {
            apply(runner, patch, true, false)?;
            attached(amend_command())?;
            apply(runner, patch, false, false)?;
            attached(new_commit_command(message))
        }
        Work::Check => apply(runner, patch, false, true),
    }
}

fn apply(runner: &dyn GitRunner, patch: &[u8], reverse: bool, check: bool) -> Result<(), String> {
    run_checked(runner, &apply_command(reverse, check).stdin(patch.to_vec())).map(|_| ())
}

/// Whether a rebase is stopped in the repository now.
fn rebasing(runner: &dyn GitRunner) -> Result<bool, String> {
    let dir = git_dir_from(&run_checked(runner, &git_dir_command())?);
    Ok(in_progress(&dir).contains(&Operation::Rebase))
}

/// Put everything back after `error`: abort a rebase in progress, else
/// reset to `orig`. The message says if that failed too.
fn fail_closed(runner: &dyn GitRunner, orig: &CommitId, error: String) -> String {
    let mut reset = false;
    let undo = match rebasing(runner) {
        Ok(true) => run_checked(runner, &rebase_command("--abort")).map(|_| ()),
        Ok(false) => {
            let head = run_checked(runner, &head_command());
            match head {
                Ok(head) if String::from_utf8_lossy(&head).trim() == orig.as_str() => Ok(()),
                _ => {
                    reset = true;
                    run_checked(runner, &reset_command(orig)).map(|_| ())
                }
            }
        }
        Err(e) => Err(e),
    };
    match undo {
        // A finished rebase was reset: only the current branch goes back.
        Ok(()) if reset => format!(
            "{error}\nThe branch was reset to {}; other branches the rebase moved \
             with --update-refs may have stayed moved.",
            orig.as_str()
        ),
        Ok(()) => error,
        Err(undo) => format!(
            "{error}\nPutting things back failed too: {undo}\nThe branch was at {}.",
            orig.as_str()
        ),
    }
}

/// Run `cmd` detached; its stdout, or git's message made safe to draw.
fn run_checked(runner: &dyn GitRunner, cmd: &GitCommand) -> Result<Vec<u8>, String> {
    super::ops::run_checked(runner, cmd).map(|output| output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::commit_files::CommitRange;
    use crate::git::fake::{FakeRunner, TempDir, fail, ok};
    use crate::git::ops::{GitOp, op_command};
    use crate::git::patch::LineId;
    use crate::git::path::RepoPath;
    use crate::git::rebase_plan::Step;

    fn id(digit: char) -> CommitId {
        CommitId::parse(&digit.to_string().repeat(40)).unwrap()
    }

    fn argv(cmd: &GitCommand) -> Vec<String> {
        cmd.args()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// Line 2 of `a.txt`, `two` in place of `2`, picked from commit `b`.
    fn picks() -> CustomPatch {
        let range = CommitRange {
            oid: id('b'),
            parent: Some(id('a')),
        };
        let mut picks = CustomPatch::new(range, "bbbbbbb".into());
        let path = RepoPath::from("a.txt");
        picks.toggle_lines(&path, &[LineId::removed(2), LineId::added(2)].into());
        picks
    }

    const DIFF: &[u8] = b"diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n\
@@ -1,3 +1,3 @@\n 1\n-2\n+two\n 3\n";

    fn step(action: TodoAction, commit: CommitId) -> Step {
        Step {
            action,
            commit,
            break_after: false,
        }
    }

    /// `b` (the source) then `c`, from `a`.
    fn plan(b: TodoAction, c: TodoAction) -> RebasePlan {
        RebasePlan {
            base: id('a'),
            steps: vec![step(b, id('b')), step(c, id('c'))],
            update_refs: false,
        }
    }

    #[test]
    fn the_diff_names_the_path_after_dashes_and_reads_it_literally() {
        let cmd = diff_command(&picks(), &RepoPath::from("-p :(glob)*"));
        let args = argv(&cmd);
        let dashes = args.iter().position(|a| a == "--").unwrap();
        assert_eq!(&args[dashes + 1..], ["-p :(glob)*"]);
        assert_eq!(
            &args[dashes - 2..dashes],
            [id('a').as_str(), id('b').as_str()]
        );
        assert!(args.contains(&"--no-renames".to_string()));
        assert!(args.contains(&"-U3".to_string()));
        assert!(!args.contains(&"-w".to_string()));
        assert!(
            cmd.envs()
                .contains(&("GIT_LITERAL_PATHSPECS".into(), "1".into()))
        );
    }

    #[test]
    fn the_applies_read_the_patch_from_stdin_into_the_index() {
        for (reverse, check, flags) in [
            (false, false, vec!["apply", "--index"]),
            (true, false, vec!["apply", "--index", "--reverse"]),
            (false, true, vec!["apply", "--index", "--check"]),
        ] {
            let mut want = flags;
            want.extend(["--whitespace=nowarn", "-"]);
            assert_eq!(argv(&apply_command(reverse, check)), want);
        }
        assert_eq!(
            argv(&amend_command()),
            [
                "commit",
                "--amend",
                "--no-edit",
                "--no-verify",
                "--allow-empty"
            ]
        );
        let commit = new_commit_command("-m --evil\n");
        assert_eq!(argv(&commit), ["commit", "--no-verify", "-F", "-"]);
        assert_eq!(commit.input(), Some(&b"-m --evil\n"[..]));
        assert_eq!(
            argv(&reset_command(&id('a'))),
            ["reset", "-q", "--keep", id('a').as_str(), "--"]
        );
    }

    #[test]
    fn each_stop_does_the_actions_part_in_history_order() {
        use TodoAction::{Edit, Pick};
        let source = id('b');
        let work = |plan, action| stop_work(&plan, &action, &source);
        assert_eq!(
            work(plan(Edit, Pick), PatchAction::Remove),
            [vec![Work::Reverse]]
        );
        assert_eq!(
            work(plan(Edit, Pick), PatchAction::NewCommit("m".into())),
            [vec![Work::SplitOff("m".into())]]
        );
        assert_eq!(
            work(plan(Edit, Edit), PatchAction::MoveTo(id('c'))),
            [vec![Work::Reverse], vec![Work::Forward]]
        );
        assert_eq!(
            work(plan(Edit, Edit), PatchAction::ToIndex),
            [vec![Work::Reverse], vec![Work::Check]]
        );
        // An older target: only the target stops.
        let older = RebasePlan {
            base: id('9'),
            steps: vec![step(Edit, id('a')), step(Pick, id('b'))],
            update_refs: false,
        };
        assert_eq!(
            work(older, PatchAction::MoveTo(id('a'))),
            [vec![Work::Forward]]
        );
        // The source is the last commit: one stop does both.
        let tip = RebasePlan {
            base: id('a'),
            steps: vec![step(Edit, id('b'))],
            update_refs: false,
        };
        assert_eq!(
            work(tip, PatchAction::ToIndex),
            [vec![Work::Reverse, Work::Check]]
        );
    }

    /// A git dir that shows a rebase stopped while `rebase -i` has run and
    /// the last `--continue` has not.
    fn faking_a_rebase(runner: FakeRunner, git_dir: &std::path::Path) -> FakeRunner {
        let dir = git_dir.join("rebase-merge");
        runner.on_attached(move |cmd| {
            let args = argv(cmd);
            if args.contains(&"-i".to_string()) {
                std::fs::create_dir_all(&dir).unwrap();
            } else if args.contains(&"--continue".to_string()) {
                let _ = std::fs::remove_dir(&dir);
            }
        })
    }

    fn rebase_argv(plan: &RebasePlan) -> Vec<String> {
        argv(&op_command(&GitOp::InteractiveRebase(plan.clone())))
    }

    fn run_with(
        runner: &FakeRunner,
        plan: &RebasePlan,
        action: &PatchAction,
    ) -> Result<(), String> {
        let picks = picks();
        let rewrite = Rewrite {
            plan,
            action,
            picks: &picks,
            orig_head: &id('c'),
        };
        let rebase = op_command(&GitOp::InteractiveRebase(plan.clone()));
        run(runner, &rewrite, rebase, &|cmd| {
            let exit = runner.run_attached(&cmd).map_err(|e| e.to_string())?;
            if exit.success {
                Ok(())
            } else {
                Err(format!("git exited with {}", exit.status))
            }
        })
    }

    fn strs(v: &[String]) -> Vec<&str> {
        v.iter().map(String::as_str).collect()
    }

    #[test]
    fn a_remove_reverses_the_patch_at_the_source_with_only_hex_ids_in_argv() {
        let tmp = TempDir::new("custom-patch-remove");
        let git_dir = format!("{}\n", tmp.path().display());
        let plan = plan(TodoAction::Edit, TodoAction::Pick);
        let head = format!("{}\n", id('c').as_str());
        let diff = argv(&diff_command(&picks(), &RepoPath::from("a.txt")));
        let rebase = rebase_argv(&plan);
        let runner = FakeRunner::new()
            .expect(
                &["rev-parse", "-q", "--verify", "HEAD"],
                ok(head.as_bytes()),
            )
            .expect(&["update-index", "-q", "--refresh"], ok(b""))
            .expect(&["diff-index", "--quiet", "HEAD", "--"], ok(b""))
            .expect(&strs(&diff), ok(DIFF))
            .expect(&["rev-parse", "--absolute-git-dir"], ok(git_dir.as_bytes()))
            .expect(
                &["apply", "--index", "--reverse", "--whitespace=nowarn", "-"],
                ok(b""),
            )
            .expect(&["rev-parse", "--absolute-git-dir"], ok(git_dir.as_bytes()))
            .expect_attached(&strs(&rebase), true)
            .expect_attached(
                &[
                    "commit",
                    "--amend",
                    "--no-edit",
                    "--no-verify",
                    "--allow-empty",
                ],
                true,
            )
            .expect_attached(&["rebase", "--continue"], true);
        let runner = faking_a_rebase(runner, tmp.path());

        assert_eq!(run_with(&runner, &plan, &PatchAction::Remove), Ok(()));

        runner.assert_done();
        let built = b"diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n\
@@ -1,3 +1,3 @@\n 1\n-2\n+two\n 3\n";
        assert!(runner.inputs().contains(&Some(built.to_vec())));
        let fixed = [
            "rev-parse",
            "-q",
            "--verify",
            "HEAD",
            "--absolute-git-dir",
            "apply",
            "--index",
            "--reverse",
            "--whitespace=nowarn",
            "-",
            "commit",
            "--amend",
            "--no-edit",
            "--no-verify",
            "--allow-empty",
            "rebase",
            "--continue",
            "--",
            "a.txt",
        ];
        for arg in diff.iter().chain(&rebase) {
            assert_ne!(arg, "--end-of-options");
            let hex = arg.bytes().all(|b| b.is_ascii_hexdigit());
            assert!(
                hex || arg.starts_with('-')
                    || fixed.contains(&arg.as_str())
                    || arg.contains('=')
                    || arg == "rebase"
                    || arg == "diff-tree",
                "{arg}"
            );
        }
    }

    #[test]
    fn a_failed_apply_at_a_stop_aborts_the_rebase() {
        let tmp = TempDir::new("custom-patch-abort");
        let git_dir = format!("{}\n", tmp.path().display());
        let plan = plan(TodoAction::Edit, TodoAction::Pick);
        let head = format!("{}\n", id('c').as_str());
        let diff = argv(&diff_command(&picks(), &RepoPath::from("a.txt")));
        let rebase = rebase_argv(&plan);
        let runner = FakeRunner::new()
            .expect(
                &["rev-parse", "-q", "--verify", "HEAD"],
                ok(head.as_bytes()),
            )
            .expect(&["update-index", "-q", "--refresh"], ok(b""))
            .expect(&["diff-index", "--quiet", "HEAD", "--"], ok(b""))
            .expect(&strs(&diff), ok(DIFF))
            .expect(&["rev-parse", "--absolute-git-dir"], ok(git_dir.as_bytes()))
            .expect(
                &["apply", "--index", "--reverse", "--whitespace=nowarn", "-"],
                fail("error: patch failed: a.txt:1"),
            )
            .expect(&["rev-parse", "--absolute-git-dir"], ok(git_dir.as_bytes()))
            .expect(&["rebase", "--abort"], ok(b""))
            .expect_attached(&strs(&rebase), true);
        let runner = faking_a_rebase(runner, tmp.path());

        assert_eq!(
            run_with(&runner, &plan, &PatchAction::Remove),
            Err("error: patch failed: a.txt:1".into())
        );
        runner.assert_done();
    }

    #[test]
    fn a_rebase_that_finished_without_its_stop_is_reset_and_says_other_branches_may_move() {
        let tmp = TempDir::new("custom-patch-reset");
        let git_dir = format!("{}\n", tmp.path().display());
        let plan = plan(TodoAction::Edit, TodoAction::Pick);
        let diff = argv(&diff_command(&picks(), &RepoPath::from("a.txt")));
        let rebase = rebase_argv(&plan);
        let head = |c| format!("{}\n", id(c).as_str());
        let runner = FakeRunner::new()
            .expect(
                &["rev-parse", "-q", "--verify", "HEAD"],
                ok(head('c').as_bytes()),
            )
            .expect(&["update-index", "-q", "--refresh"], ok(b""))
            .expect(&["diff-index", "--quiet", "HEAD", "--"], ok(b""))
            .expect(&strs(&diff), ok(DIFF))
            .expect(&["rev-parse", "--absolute-git-dir"], ok(git_dir.as_bytes()))
            .expect(&["rev-parse", "--absolute-git-dir"], ok(git_dir.as_bytes()))
            .expect(
                &["rev-parse", "-q", "--verify", "HEAD"],
                ok(head('e').as_bytes()),
            )
            .expect(&["reset", "-q", "--keep", id('c').as_str(), "--"], ok(b""))
            .expect_attached(&strs(&rebase), true);

        let error = run_with(&runner, &plan, &PatchAction::Remove).unwrap_err();

        assert!(error.starts_with(NOT_STOPPED), "{error}");
        assert!(error.contains("other branches"), "{error}");
        runner.assert_done();
    }

    #[test]
    fn nothing_runs_when_a_tracked_file_changed_since_the_question() {
        let plan = plan(TodoAction::Edit, TodoAction::Pick);
        let dirty = FakeRunner::new()
            .expect(
                &["rev-parse", "-q", "--verify", "HEAD"],
                ok(format!("{}\n", id('c').as_str()).as_bytes()),
            )
            .expect(&["update-index", "-q", "--refresh"], fail(""))
            .expect(&["diff-index", "--quiet", "HEAD", "--"], fail(""));
        assert_eq!(
            run_with(&dirty, &plan, &PatchAction::Remove),
            Err(DIRTY.into())
        );
        dirty.assert_done();
    }

    #[test]
    fn the_worktree_check_reads_real_changes_and_not_a_touched_file() {
        use crate::git::fake::git;
        let tmp = TempDir::new("custom-patch-dirty");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        git(dir, &["add", "a.txt"]);
        git(
            dir,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                "a",
            ],
        );
        let runner = crate::git::CliRunner::isolated(dir);
        std::fs::write(dir.join("u.txt"), "untracked\n").unwrap();
        // Rewritten with the same bytes: only its stat data changed.
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        assert_eq!(worktree_clean(&runner), Ok(true));
        std::fs::write(dir.join("a.txt"), "changed\n").unwrap();
        assert_eq!(worktree_clean(&runner), Ok(false));
        git(dir, &["add", "a.txt"]);
        assert_eq!(worktree_clean(&runner), Ok(false));
    }

    #[test]
    fn nothing_runs_when_head_moved_or_the_patch_cannot_be_built() {
        let plan = plan(TodoAction::Edit, TodoAction::Pick);
        let moved = FakeRunner::new().expect(
            &["rev-parse", "-q", "--verify", "HEAD"],
            ok(format!("{}\n", id('d').as_str()).as_bytes()),
        );
        assert_eq!(
            run_with(&moved, &plan, &PatchAction::Remove),
            Err(HEAD_MOVED.into())
        );
        moved.assert_done();

        let diff = argv(&diff_command(&picks(), &RepoPath::from("a.txt")));
        let binary = b"diff --git a/a.txt b/a.txt\nBinary files a/a.txt and b/a.txt differ\n";
        let refused = FakeRunner::new()
            .expect(
                &["rev-parse", "-q", "--verify", "HEAD"],
                ok(format!("{}\n", id('c').as_str()).as_bytes()),
            )
            .expect(&["update-index", "-q", "--refresh"], ok(b""))
            .expect(&["diff-index", "--quiet", "HEAD", "--"], ok(b""))
            .expect(&strs(&diff), ok(binary));
        let error = run_with(&refused, &plan, &PatchAction::Remove).unwrap_err();
        assert!(error.contains("Binary file"), "{error}");
        refused.assert_done();
    }
}
