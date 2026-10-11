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
    Bisect,
}

/// Ask git where its dir is. It is not always `.git/`: linked worktrees and
/// `GIT_DIR` put it elsewhere.
pub fn git_dir_command() -> GitCommand {
    GitCommand::new("rev-parse")
        .arg("--absolute-git-dir")
        .env("LC_ALL", "C")
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

    #[test]
    fn git_dir_command_is_argv_only_in_the_c_locale() {
        let cmd = git_dir_command();
        let args: Vec<_> = cmd.args().iter().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(args, ["rev-parse", "--absolute-git-dir"]);
        let envs: Vec<_> = cmd
            .envs()
            .iter()
            .map(|(k, v)| (k.to_str().unwrap(), v.to_str().unwrap()))
            .collect();
        assert_eq!(envs, [("LC_ALL", "C")]);
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
        let cases: [(&[&str], &[&str], Operation); 5] = [
            (&["rebase-merge"], &[], Operation::Rebase),
            (&["rebase-apply"], &[], Operation::Rebase),
            (&[], &["MERGE_HEAD"], Operation::Merge),
            (&[], &["CHERRY_PICK_HEAD"], Operation::CherryPick),
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
