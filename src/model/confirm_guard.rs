//! Answering a question that moves `HEAD`: a bisect or another operation
//! may have started outside Kika while it was open, so `y` asks the guard
//! the opening key asked once more before anything runs.

use super::AppState;
use crate::op::GitOp;

impl AppState {
    /// Whether the guard the key that opened the question uses for `op`
    /// now refuses it; if so, its reason goes in the bar. A reset waits
    /// for any operation in progress, a checkout (of a commit or a branch,
    /// new or not) only for a bisect, as their keys do. A pull request checkout waits for any, as `b` does.
    pub(crate) fn confirm_refused(&mut self, op: &GitOp) -> bool {
        let reason = match op {
            GitOp::Reset { .. } | GitOp::CheckoutPrBranch { .. } => {
                crate::keymap::in_progress_disabled(self)
            }
            GitOp::CheckoutCommit(_)
            | GitOp::Checkout(_)
            | GitOp::CheckoutTracking { .. }
            | GitOp::CreateBranch { .. } => crate::bisect::head_move_disabled(self),
            _ => None,
        };
        let Some(reason) = reason else {
            return false;
        };
        self.notice = Some(reason);
        true
    }
}

#[cfg(test)]
mod tests {
    use crate::bisect::RESET_FIRST;
    use crate::model::AppState;
    use crate::op::{GitOp, ResetMode};
    use crate::test_support::{
        cid, on_commit, reset_op, start_bisecting, start_merging, three_remotes,
    };

    fn merging() -> AppState {
        let mut state = on_commit();
        start_merging(&mut state);
        state
    }

    fn bisecting() -> AppState {
        let mut state = on_commit();
        start_bisecting(&mut state);
        state
    }

    #[test]
    fn a_reset_is_refused_while_an_operation_is_in_progress() {
        let reset = reset_op(ResetMode::Hard);
        let mut state = merging();
        assert!(state.confirm_refused(&reset));
        assert_eq!(
            state.notice,
            Some("Finish or abort the operation in progress".into())
        );

        let mut state = bisecting();
        assert!(state.confirm_refused(&reset));
        assert_eq!(state.notice, Some(RESET_FIRST.into()));
    }

    #[test]
    fn a_checkout_is_refused_only_during_a_bisect() {
        let checkout = GitOp::CheckoutCommit(cid("c1"));
        let mut state = bisecting();
        assert!(state.confirm_refused(&checkout));
        assert_eq!(state.notice, Some(RESET_FIRST.into()));

        let mut state = merging();
        assert!(!state.confirm_refused(&checkout));
        assert_eq!(state.notice, None);
    }

    #[test]
    fn a_branch_checkout_is_refused_only_during_a_bisect() {
        for op in [
            GitOp::Checkout("main".into()),
            GitOp::CreateBranch {
                name: "fresh".into(),
                from: "main".into(),
            },
            GitOp::CheckoutTracking {
                name: "topic".into(),
                from: three_remotes()[1].branches[1].clone(),
            },
        ] {
            let mut state = bisecting();
            assert!(state.confirm_refused(&op), "{op:?}");
            assert_eq!(state.notice, Some(RESET_FIRST.into()), "{op:?}");

            let mut state = merging();
            assert!(!state.confirm_refused(&op), "{op:?}");
            assert_eq!(state.notice, None, "{op:?}");
        }
    }

    #[test]
    fn a_pull_request_checkout_is_refused_while_any_operation_is_in_progress() {
        let checkout = GitOp::CheckoutPrBranch {
            number: 7,
            branch: "pr/7".into(),
            head: cid("c1"),
            old: None,
            current: false,
        };
        let mut state = merging();
        assert!(state.confirm_refused(&checkout));
        assert_eq!(
            state.notice,
            Some("Finish or abort the operation in progress".into())
        );
        let mut state = bisecting();
        assert!(state.confirm_refused(&checkout));
        assert_eq!(state.notice, Some(RESET_FIRST.into()));
        assert!(!on_commit().confirm_refused(&checkout));
    }

    #[test]
    fn other_ops_and_a_clean_state_are_not_refused_and_keep_the_notice() {
        for op in [
            GitOp::Revert(cid("c1")),
            reset_op(ResetMode::Hard),
            GitOp::CheckoutCommit(cid("c1")),
        ] {
            let mut clean = on_commit();
            clean.notice = Some("earlier".into());
            assert!(!clean.confirm_refused(&op), "{op:?}");
            assert_eq!(clean.notice, Some("earlier".into()), "{op:?}");
        }
        for mut state in [merging(), bisecting()] {
            state.notice = Some("earlier".into());
            assert!(!state.confirm_refused(&GitOp::Revert(cid("c1"))));
            assert_eq!(state.notice, Some("earlier".into()));
        }
    }
}
