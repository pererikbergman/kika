//! Operations left half done in the repository, read from the marker files
//! git itself writes into its git dir.

use std::path::{Path, PathBuf};

use super::path::path_from_stdout;
use super::runner::GitCommand;

/// An operation in progress, which the user has to finish or abort.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Rebase,
    Merge,
    CherryPick,
    Revert,
    Bisect,
}

impl Operation {
    /// The git subcommand that started it, and that continues, skips or
    /// aborts it.
    pub fn subcommand(self) -> &'static str {
        match self {
            Operation::Rebase => "rebase",
            Operation::Merge => "merge",
            Operation::CherryPick => "cherry-pick",
            Operation::Revert => "revert",
            Operation::Bisect => "bisect",
        }
    }

    /// Whether git can drop the commit it stopped on and go on. A merge
    /// has only the one commit to make, so there is nothing to skip.
    pub fn can_skip(self) -> bool {
        !matches!(self, Operation::Merge)
    }
}

/// The operation continue, skip and abort act on: the first of a rebase,
/// a merge, a cherry-pick and a revert in `ops`. A bisect is never one; it
/// has its own controls.
pub fn resumable(ops: &[Operation]) -> Option<Operation> {
    [
        Operation::Rebase,
        Operation::Merge,
        Operation::CherryPick,
        Operation::Revert,
    ]
    .into_iter()
    .find(|op| ops.contains(op))
}

/// Ask git where its dir is. It is not always `.git/`: linked worktrees and
/// `GIT_DIR` put it elsewhere.
pub fn git_dir_command() -> GitCommand {
    GitCommand::new("rev-parse")
        .arg("--absolute-git-dir")
        .standard_env()
}

/// The path `rev-parse --absolute-git-dir` printed, from its raw bytes so a
/// directory name that is not UTF-8 survives.
pub fn git_dir_from(stdout: &[u8]) -> PathBuf {
    path_from_stdout(stdout)
}

/// Every operation in progress in `git_dir`, in a fixed order. A
/// `rebase-apply/applying` marker means `git am`, which is not a rebase.
pub fn in_progress(git_dir: &Path) -> Vec<Operation> {
    let has = |name: &str| git_dir.join(name).exists();
    let rebase = has("rebase-merge") || (has("rebase-apply") && !has("rebase-apply/applying"));
    [
        (rebase, Operation::Rebase),
        (has("MERGE_HEAD"), Operation::Merge),
        (has("CHERRY_PICK_HEAD"), Operation::CherryPick),
        (has("REVERT_HEAD"), Operation::Revert),
        (has("BISECT_LOG"), Operation::Bisect),
    ]
    .into_iter()
    .filter_map(|(found, op)| found.then_some(op))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::fake::TempDir;
    use crate::git::fake::{STANDARD_ENV, env_list};
    use crate::git::runner::EnvProfile;

    #[test]
    fn resumable_is_the_first_of_rebase_merge_and_cherry_pick_never_bisect() {
        use Operation::*;
        assert_eq!(resumable(&[]), None);
        assert_eq!(resumable(&[Bisect]), None);
        assert_eq!(resumable(&[Merge, Bisect]), Some(Merge));
        assert_eq!(resumable(&[CherryPick]), Some(CherryPick));
        assert_eq!(resumable(&[Merge, CherryPick]), Some(Merge));
        // Ordered by kind, not by place in the list.
        assert_eq!(resumable(&[CherryPick, Rebase]), Some(Rebase));
        assert_eq!(resumable(&[Revert, Bisect]), Some(Revert));
        assert_eq!(resumable(&[Revert, CherryPick]), Some(CherryPick));
    }

    #[test]
    fn only_a_merge_cannot_be_skipped() {
        assert!(!Operation::Merge.can_skip());
        assert!(Operation::Rebase.can_skip());
        assert!(Operation::CherryPick.can_skip());
        assert!(Operation::Revert.can_skip());
        assert_eq!(Operation::Revert.subcommand(), "revert");
    }

    #[test]
    fn git_dir_command_is_argv_only_in_the_c_locale() {
        let cmd = git_dir_command();
        let args: Vec<_> = cmd.args().iter().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(args, ["rev-parse", "--absolute-git-dir"]);
        // A read with no path and no lock: the whole standard profile.
        assert_eq!(cmd.env_profile(), Some(EnvProfile::Standard));
        assert_eq!(cmd.process_env(), env_list(&STANDARD_ENV));
    }

    #[test]
    fn git_dir_drops_only_the_trailing_newline() {
        assert_eq!(
            git_dir_from(b"/work/my repo/.git\n"),
            PathBuf::from("/work/my repo/.git")
        );
    }

    #[cfg(unix)]
    #[test]
    fn git_dir_keeps_bytes_that_are_not_utf8() {
        use std::os::unix::ffi::OsStrExt;
        let dir = git_dir_from(b"/r\xff/.git\n");
        assert_eq!(dir.as_os_str().as_bytes(), b"/r\xff/.git");
    }

    #[test]
    fn a_clean_git_dir_has_nothing_in_progress() {
        let tmp = TempDir::new("repo-state");
        assert_eq!(in_progress(tmp.path()), []);
    }

    #[test]
    fn each_marker_names_its_operation() {
        let cases: [(&[&str], &[&str], Operation); 6] = [
            (&["rebase-merge"], &[], Operation::Rebase),
            (&["rebase-apply"], &[], Operation::Rebase),
            (&[], &["MERGE_HEAD"], Operation::Merge),
            (&[], &["CHERRY_PICK_HEAD"], Operation::CherryPick),
            (&[], &["REVERT_HEAD"], Operation::Revert),
            (&[], &["BISECT_LOG"], Operation::Bisect),
        ];
        for (dirs, files, op) in cases {
            let tmp = TempDir::new("repo-state");
            for dir in dirs {
                std::fs::create_dir(tmp.path().join(dir)).unwrap();
            }
            for file in files {
                std::fs::write(tmp.path().join(file), "x\n").unwrap();
            }
            assert_eq!(in_progress(tmp.path()), [op], "{dirs:?} {files:?}");
        }
    }

    #[test]
    fn a_revert_stopped_on_a_conflict_is_a_revert_in_progress_in_real_git() {
        use crate::git::fake::{git, try_git};

        let tmp = TempDir::new("revert-conflict");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        for text in ["one\n", "two\n", "three\n"] {
            std::fs::write(dir.join("a.txt"), text).unwrap();
            git(dir, &["add", "-A"]);
            git(dir, &["commit", "-q", "-m", text.trim()]);
        }
        // Undoing `two` clashes with `three`, which changed the same line.
        let reverted = try_git(dir, &["-c", "core.editor=true", "revert", "HEAD^"]);
        assert!(!reverted, "the revert was expected to stop on a conflict");
        assert_eq!(in_progress(&dir.join(".git")), [Operation::Revert]);
        assert_eq!(
            resumable(&in_progress(&dir.join(".git"))),
            Some(Operation::Revert)
        );
    }

    #[test]
    fn git_am_is_not_a_rebase() {
        let tmp = TempDir::new("repo-state");
        std::fs::create_dir(tmp.path().join("rebase-apply")).unwrap();
        std::fs::write(tmp.path().join("rebase-apply/applying"), "").unwrap();
        assert_eq!(in_progress(tmp.path()), []);
    }

    #[test]
    fn several_operations_are_listed_in_order() {
        let tmp = TempDir::new("repo-state");
        for file in ["BISECT_LOG", "MERGE_HEAD"] {
            std::fs::write(tmp.path().join(file), "x\n").unwrap();
        }
        std::fs::create_dir(tmp.path().join("rebase-merge")).unwrap();
        assert_eq!(
            in_progress(tmp.path()),
            [Operation::Rebase, Operation::Merge, Operation::Bisect]
        );
    }
}
