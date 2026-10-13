//! The reload executor: it turns a [`RefreshPlan`] from `refresh.rs` into
//! the loads that bring each stale [`Scope`] back. It is core, so it calls
//! each feature's reload; features ask for a plan instead of calling one
//! another's.

use crate::effect::Effect;
use crate::model::AppState;
use crate::refresh::{RefreshPlan, Scope};

impl AppState {
    /// Reload what the plan says is stale. One status load serves both the
    /// files and the branch; reloading files reselects, which reloads the
    /// diff too. The submodule list reloads only while it is shown.
    pub(crate) fn apply_plan(&mut self, plan: RefreshPlan) -> Vec<Effect> {
        let mut effects =
            if plan.scopes.contains(&Scope::Files) || plan.scopes.contains(&Scope::Branch) {
                self.status_selection = plan.selection;
                self.refresh_status()
            } else {
                match self.selected {
                    Some(index) if plan.scopes.contains(&Scope::Diff) => self.select(index),
                    _ => vec![],
                }
            };
        if plan.scopes.contains(&Scope::Submodules) {
            effects.extend(self.reload_submodules());
        }
        if plan.scopes.contains(&Scope::Branches) {
            effects.extend(self.reload_branches());
        }
        if plan.scopes.contains(&Scope::Commits) {
            effects.extend(self.reload_commits());
        }
        if plan.scopes.contains(&Scope::Stash) {
            effects.extend(self.reload_stashes());
        }
        if plan.scopes.contains(&Scope::PullRequests) {
            effects.extend(self.load_pull_requests());
        }
        effects
    }

    /// Reload everything the repository shows, for opening, entering or
    /// leaving one, or a refresh the user asked for.
    pub(crate) fn reload_repo(&mut self) -> Vec<Effect> {
        self.apply_plan(RefreshPlan::of(&[Scope::Repo]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Context;
    use crate::test_support::forge_state;

    /// Each effect's name and seq, so a test can read the order at a glance.
    fn names(effects: &[Effect]) -> Vec<(&'static str, u64)> {
        effects
            .iter()
            .map(|effect| match effect {
                Effect::LoadStatus { tag } => ("LoadStatus", tag.seq),
                Effect::LoadBranches { tag } => ("LoadBranches", tag.seq),
                Effect::LoadRemotes { tag } => ("LoadRemotes", tag.seq),
                Effect::LoadTags { tag } => ("LoadTags", tag.seq),
                Effect::LoadCommits { tag, .. } => ("LoadCommits", tag.seq),
                Effect::LoadMarkers { tag } => ("LoadMarkers", tag.seq),
                Effect::LoadBranchTips { tag } => ("LoadBranchTips", tag.seq),
                Effect::LoadStashes { tag } => ("LoadStashes", tag.seq),
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_whole_repo_reload_loads_status_branches_log_and_stashes_in_order() {
        let mut state = AppState::new(Context::Workspace);
        let loads = names(&state.reload_repo());
        let order: Vec<_> = loads.iter().map(|&(name, _)| name).collect();
        assert_eq!(
            order,
            [
                "LoadStatus",
                "LoadBranches",
                "LoadRemotes",
                "LoadTags",
                "LoadCommits",
                "LoadMarkers",
                "LoadBranchTips",
                "LoadStashes",
            ]
        );
        assert!(
            loads.windows(2).all(|pair| pair[0].1 < pair[1].1),
            "{loads:?}"
        );
    }

    #[test]
    fn a_whole_repo_reload_loads_nothing_outside_the_workspace() {
        let mut state = AppState::new(Context::Review { number: Some(7) });
        assert_eq!(state.reload_repo(), []);
    }

    #[test]
    fn the_badges_scope_starts_the_same_load_as_asking_for_badges() {
        let mut state = forge_state(&[]);
        let mut twin = state.clone();
        let effects = state.apply_plan(RefreshPlan::of(&[Scope::PullRequests]));
        assert!(
            matches!(effects.as_slice(), [Effect::LoadPullRequests { .. }]),
            "{effects:?}"
        );
        assert_eq!(effects, twin.load_pull_requests());
    }

    #[test]
    fn the_badges_scope_loads_nothing_with_the_forge_off() {
        let mut state = AppState::new(Context::Workspace);
        assert_eq!(
            state.apply_plan(RefreshPlan::of(&[Scope::PullRequests])),
            []
        );
    }

    #[test]
    fn the_submodules_scope_loads_nothing_while_the_popup_is_closed() {
        let mut state = AppState::new(Context::Workspace);
        let effects = state.apply_plan(RefreshPlan::of(&[Scope::Submodules]));
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::LoadSubmodules { .. })),
            "{effects:?}"
        );
    }
}
