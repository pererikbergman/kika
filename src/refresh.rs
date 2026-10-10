//! The refresh planner: one table from each Git operation to the data it
//! makes stale. New mutations add a row here instead of picking refreshes at
//! each call site.

use std::collections::BTreeSet;

use crate::git::ops::GitOp;

/// A piece of loaded data that can go stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    /// The `git status` file list.
    Files,
    /// The selected file's diff.
    Diff,
}

/// How the file selection survives a refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    /// Stay on the same path; if it is gone, stay near the same index.
    KeepPath,
}

/// What to reload after an operation, and how to keep the selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshPlan {
    pub scopes: BTreeSet<Scope>,
    pub selection: Selection,
}

/// `(a, b)`: when `a` is stale, so is `b`. The selected file's diff depends
/// on its status, because staging flips which side of it is shown.
const DEPENDS: &[(Scope, Scope)] = &[(Scope::Files, Scope::Diff)];

/// The table. No `_` arm, so a new [`GitOp`] does not compile without a row.
pub fn plan(op: &GitOp) -> RefreshPlan {
    let (scopes, selection) = match op {
        GitOp::Stage(_) | GitOp::Unstage(_) | GitOp::StageAll | GitOp::UnstageAll => {
            ([Scope::Files], Selection::KeepPath)
        }
    };
    RefreshPlan {
        scopes: expand(scopes.into()),
        selection,
    }
}

/// Add every scope that depends on one already stale, until nothing changes.
pub fn expand(mut scopes: BTreeSet<Scope>) -> BTreeSet<Scope> {
    loop {
        let before = scopes.len();
        for &(from, to) in DEPENDS {
            if scopes.contains(&from) {
                scopes.insert(to);
            }
        }
        if scopes.len() == before {
            return scopes;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(scopes: &[Scope]) -> BTreeSet<Scope> {
        scopes.iter().copied().collect()
    }

    #[test]
    fn every_index_op_reloads_files_and_diff_and_keeps_the_path() {
        let ops = [
            GitOp::Stage(vec!["a".into()]),
            GitOp::Unstage(vec!["a".into()]),
            GitOp::StageAll,
            GitOp::UnstageAll,
        ];
        for op in ops {
            assert_eq!(
                plan(&op),
                RefreshPlan {
                    scopes: set(&[Scope::Files, Scope::Diff]),
                    selection: Selection::KeepPath,
                },
                "{op:?}"
            );
        }
    }

    #[test]
    fn files_pull_in_the_diff() {
        assert_eq!(
            expand(set(&[Scope::Files])),
            set(&[Scope::Files, Scope::Diff])
        );
    }

    #[test]
    fn diff_depends_on_nothing_else() {
        assert_eq!(expand(set(&[Scope::Diff])), set(&[Scope::Diff]));
    }

    #[test]
    fn nothing_expands_to_nothing() {
        assert_eq!(expand(set(&[])), set(&[]));
    }
}
