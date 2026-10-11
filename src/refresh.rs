//! The refresh planner: one table from each Git operation to the data it
//! makes stale. New mutations add a row here instead of picking refreshes at
//! each call site.

use std::collections::BTreeSet;

use crate::git::ops::GitOp;
use crate::git::remote::RemoteOp;

/// A piece of loaded data that can go stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    /// The `git status` file list.
    Files,
    /// The status panel: branch, upstream and operations in progress.
    Branch,
    /// The selected file's diff.
    Diff,
    /// The Submodules popup's list.
    Submodules,
    /// The Branches panel's list, with upstreams and ahead/behind.
    Branches,
    /// The Commits panel's log and markers.
    Commits,
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
        GitOp::Stage(_)
        | GitOp::Unstage(_)
        | GitOp::StageAll
        | GitOp::UnstageAll
        | GitOp::ApplyCached { .. }
        | GitOp::DiscardWorktree(_)
        | GitOp::DeleteUntracked(_)
        | GitOp::ApplyWorktree { .. }
        | GitOp::ApplyEdited { .. } => (&[Scope::Files][..], Selection::KeepPath),
        // Ahead/behind moves with a commit, and so does the log.
        GitOp::Commit { .. } => (
            &[Scope::Files, Scope::Branch, Scope::Branches, Scope::Commits][..],
            Selection::KeepPath,
        ),
        // A checkout changes the worktree, which branch is current and so
        // the log of HEAD.
        GitOp::Checkout(_) | GitOp::CreateBranch { .. } => (
            &[Scope::Files, Scope::Branch, Scope::Branches, Scope::Commits][..],
            Selection::KeepPath,
        ),
        // These move HEAD, rewrite the worktree and index, and start or
        // end an operation the Status panel shows. A detached checkout
        // also changes what the Status panel says HEAD is.
        GitOp::Merge { .. }
        | GitOp::Rebase { .. }
        | GitOp::InteractiveRebase(_)
        | GitOp::CheckoutCommit(_)
        | GitOp::Revert(_)
        | GitOp::Reset { .. }
        | GitOp::Continue(_)
        | GitOp::Skip(_)
        | GitOp::Abort(_) => (
            &[Scope::Files, Scope::Branch, Scope::Branches, Scope::Commits][..],
            Selection::KeepPath,
        ),
        // Renaming or deleting a branch leaves HEAD's commits where they are.
        GitOp::RenameBranch { .. } => (&[Scope::Branch, Scope::Branches][..], Selection::KeepPath),
        GitOp::DeleteBranch { .. } => (&[Scope::Branches][..], Selection::KeepPath),
        // The tool resolves and stages the file, which can end the
        // conflicts the Status panel counts on.
        GitOp::MergeTool(_) => (&[Scope::Files, Scope::Branch][..], Selection::KeepPath),
        // The parent lists a submodule whose checkout moved as modified.
        GitOp::SubmoduleInit(_) | GitOp::SubmoduleUpdate(_) => {
            (&[Scope::Files, Scope::Submodules][..], Selection::KeepPath)
        }
    };
    RefreshPlan {
        scopes: expand(scopes.iter().copied().collect()),
        selection,
    }
}

/// What a fetch, pull or push makes stale. Each moves remote refs, so
/// ahead/behind and the log's markers; a pull also moves `HEAD` and
/// changes the worktree.
pub fn remote_plan(op: RemoteOp) -> RefreshPlan {
    let scopes: &[Scope] = match op {
        RemoteOp::Pull => &[Scope::Files, Scope::Branch, Scope::Branches, Scope::Commits],
        RemoteOp::Fetch | RemoteOp::Push { .. } => {
            &[Scope::Branch, Scope::Branches, Scope::Commits]
        }
    };
    RefreshPlan {
        scopes: expand(scopes.iter().copied().collect()),
        selection: Selection::KeepPath,
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
    fn stage_and_discard_ops_reload_files_and_diff_and_keep_the_path() {
        let ops = [
            GitOp::Stage(vec!["a".into()]),
            GitOp::Unstage(vec!["a".into()]),
            GitOp::StageAll,
            GitOp::UnstageAll,
            GitOp::ApplyCached {
                patch: b"p".to_vec(),
                reverse: true,
            },
            GitOp::DiscardWorktree(vec!["a".into()]),
            GitOp::DeleteUntracked(vec!["a".into()]),
            GitOp::ApplyWorktree {
                patch: b"p".to_vec(),
            },
            GitOp::ApplyEdited {
                patch: b"p".to_vec(),
            },
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
    fn a_commit_also_reloads_the_branch_the_list_and_the_commits() {
        let op = GitOp::Commit {
            message: Some("subj\n".into()),
            stage_all: true,
            amend: false,
            no_verify: false,
        };
        assert_eq!(
            plan(&op),
            RefreshPlan {
                scopes: set(&[
                    Scope::Files,
                    Scope::Branch,
                    Scope::Diff,
                    Scope::Branches,
                    Scope::Commits
                ]),
                selection: Selection::KeepPath,
            }
        );
    }

    #[test]
    fn checkout_and_create_reload_files_diff_the_branch_the_list_and_the_commits() {
        let ops = [
            GitOp::Checkout("b".into()),
            GitOp::CreateBranch {
                name: "n".into(),
                from: "b".into(),
            },
        ];
        for op in ops {
            assert_eq!(
                plan(&op).scopes,
                set(&[
                    Scope::Files,
                    Scope::Branch,
                    Scope::Diff,
                    Scope::Branches,
                    Scope::Commits
                ]),
                "{op:?}"
            );
        }
    }

    #[test]
    fn commit_actions_and_history_rewrites_reload_everything_head_touches() {
        use crate::git::commit_files::CommitId;
        use crate::git::ops::ResetMode;
        let oid = || CommitId::parse("abc").unwrap();
        let ops = [
            GitOp::CheckoutCommit(oid()),
            GitOp::Revert(oid()),
            GitOp::Reset {
                to: oid(),
                mode: ResetMode::Hard,
            },
            GitOp::InteractiveRebase(crate::git::rebase_plan::sample()),
        ];
        for op in ops {
            assert_eq!(
                plan(&op),
                RefreshPlan {
                    scopes: set(&[
                        Scope::Files,
                        Scope::Branch,
                        Scope::Diff,
                        Scope::Branches,
                        Scope::Commits
                    ]),
                    selection: Selection::KeepPath,
                },
                "{op:?}"
            );
        }
    }

    #[test]
    fn merge_rebase_and_their_controls_reload_everything_head_touches() {
        use crate::git::ops::MergeStrategy;
        use crate::git::repo_state::Operation;
        let ops = [
            GitOp::Merge {
                branch: "b".into(),
                strategy: MergeStrategy::Squash,
            },
            GitOp::Rebase { onto: "b".into() },
            GitOp::Continue(Operation::Merge),
            GitOp::Skip(Operation::Rebase),
            GitOp::Abort(Operation::CherryPick),
        ];
        for op in ops {
            assert_eq!(
                plan(&op),
                RefreshPlan {
                    scopes: set(&[
                        Scope::Files,
                        Scope::Branch,
                        Scope::Diff,
                        Scope::Branches,
                        Scope::Commits
                    ]),
                    selection: Selection::KeepPath,
                },
                "{op:?}"
            );
        }
    }

    #[test]
    fn the_merge_tool_reloads_the_files_and_the_status_panel() {
        assert_eq!(
            plan(&GitOp::MergeTool("a".into())).scopes,
            set(&[Scope::Files, Scope::Branch, Scope::Diff])
        );
    }

    /// Neither moves `HEAD`, so the log stays as it is.
    #[test]
    fn rename_reloads_the_branch_and_the_list_and_delete_only_the_list() {
        let rename = GitOp::RenameBranch {
            from: "a".into(),
            to: "b".into(),
        };
        assert_eq!(plan(&rename).scopes, set(&[Scope::Branch, Scope::Branches]));
        let delete = GitOp::DeleteBranch {
            name: "a".into(),
            force: true,
        };
        assert_eq!(plan(&delete).scopes, set(&[Scope::Branches]));
    }

    #[test]
    fn remote_ops_reload_the_branch_the_lists_and_a_pull_the_files_too() {
        let push = RemoteOp::Push {
            force_with_lease: false,
        };
        for op in [RemoteOp::Fetch, push] {
            assert_eq!(
                remote_plan(op).scopes,
                set(&[Scope::Branch, Scope::Branches, Scope::Commits]),
                "{op:?}"
            );
        }
        assert_eq!(
            remote_plan(RemoteOp::Pull).scopes,
            set(&[
                Scope::Files,
                Scope::Branch,
                Scope::Diff,
                Scope::Branches,
                Scope::Commits
            ])
        );
    }

    #[test]
    fn the_branch_list_pulls_in_nothing_else() {
        assert_eq!(expand(set(&[Scope::Branches])), set(&[Scope::Branches]));
    }

    #[test]
    fn submodule_ops_reload_files_diff_and_the_submodule_list() {
        for op in [
            GitOp::SubmoduleInit("sub".into()),
            GitOp::SubmoduleUpdate("sub".into()),
        ] {
            assert_eq!(
                plan(&op),
                RefreshPlan {
                    scopes: set(&[Scope::Files, Scope::Diff, Scope::Submodules]),
                    selection: Selection::KeepPath,
                },
                "{op:?}"
            );
        }
    }

    #[test]
    fn the_branch_pulls_in_nothing_else() {
        assert_eq!(expand(set(&[Scope::Branch])), set(&[Scope::Branch]));
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
