//! The diff mark: a commit or local branch the user picked to compare
//! another commit or branch against. Pure; `AppState` keeps the mark and
//! the task resolves its ends to commits.

use crate::git::branches::BranchName;
use crate::git::commit_files::CommitId;

/// What the user marked for diffing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffMark {
    /// A log commit, with the short id the log showed for it.
    Commit { id: CommitId, short: String },
    /// A local branch, resolved to its commit only when the diff opens, so
    /// it compares where the branch is then.
    Branch(BranchName),
}

impl DiffMark {
    /// Terminal-safe: the short id, or the branch name made safe to draw.
    pub fn label(&self) -> String {
        match self {
            DiffMark::Commit { short, .. } => short.clone(),
            DiffMark::Branch(name) => name.display(),
        }
    }

    /// The end of a comparison this mark stands for.
    pub fn end(&self) -> DiffEnd {
        match self {
            DiffMark::Commit { id, .. } => DiffEnd::Commit(id.clone()),
            DiffMark::Branch(name) => DiffEnd::Branch(name.clone()),
        }
    }
}

/// One end of a comparison, as the task gets it: a commit is used as it
/// is, a branch is read through `refs/heads/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffEnd {
    Commit(CommitId),
    Branch(BranchName),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_label_is_the_short_id_or_the_branch_name_made_safe() {
        let commit = DiffMark::Commit {
            id: CommitId::parse("a1b2c3d4").unwrap(),
            short: "a1b2c3d".into(),
        };
        assert_eq!(commit.label(), "a1b2c3d");
        let branch = DiffMark::Branch(BranchName::from_bytes(b"x\x1b[2J"));
        assert!(!branch.label().contains(char::is_control));
        assert_eq!(
            branch.end(),
            DiffEnd::Branch(BranchName::from_bytes(b"x\x1b[2J"))
        );
    }
}
