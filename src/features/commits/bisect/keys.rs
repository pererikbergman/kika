//! Bisect's key bindings: `B` in the Commits panel opens the bisect
//! menu, and the Status panel's `g` and `b` mark the commit the bisect
//! is testing. `CHUNKS` in `keymap.rs` places the chunks, so their
//! position there is behaviour.

use crate::bisect;
use crate::git::bisect::BisectMark;
use crate::intent::AppIntent;
use crate::keymap::{Binding, KeyPattern, KeyScope, NONE, Reason, list_guard};
use crate::menu::MenuId;
use crate::model::AppState;
use crossterm::event::KeyCode;

/// The bisect menu marks the selected commit, so it is one row's action.
fn commits_bisect_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false)
}

// Chunk 51: bisect
#[rustfmt::skip]
pub(crate) static BISECT_COMMITS: &[Binding] = &[
    Binding {
        id: "commits.bisect",
        keys: &[KeyPattern::any(KeyCode::Char('B'))],
        scope: KeyScope::Commits,
        intent: AppIntent::OpenMenu(MenuId::Bisect),
        description: "Mark the selected commit good, bad or skipped for a bisect, or reset it",
        label: "bisect",
        bar: true,
        disabled: commits_bisect_disabled,
    },
];

// Chunk 53: bisect
pub(crate) static BISECT_STATUS: &[Binding] = &[
    Binding {
        id: "status.bisect_good",
        keys: &[KeyPattern::exact(KeyCode::Char('g'), NONE)],
        scope: KeyScope::Status,
        intent: AppIntent::BisectHead(BisectMark::Good),
        description: "Mark the commit the bisect is testing good",
        label: "good",
        bar: true,
        disabled: bisect::head_good_disabled,
    },
    Binding {
        id: "status.bisect_bad",
        keys: &[KeyPattern::exact(KeyCode::Char('b'), NONE)],
        scope: KeyScope::Status,
        intent: AppIntent::BisectHead(BisectMark::Bad),
        description: "Mark the commit the bisect is testing bad",
        label: "bad",
        bar: true,
        disabled: bisect::head_bad_disabled,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::repo_state::Operation;
    use crate::git::status::{EntryKind, Head};
    use crate::keymap::registry;
    use crate::model::Context;

    #[test]
    fn the_bisect_keys_clash_with_nothing_they_could_hide_or_be_hidden_by() {
        // Commits and Status fall back to Workspace, then Global; neither
        // has a scope below it.
        let ids = ["commits.bisect", "status.bisect_good", "status.bisect_bad"];
        for id in ids {
            let new = registry().iter().find(|b| b.id == id).unwrap();
            for other in registry() {
                let reachable = new.scope.chain().any(|s| s == other.scope)
                    || other.scope.chain().any(|s| s == new.scope);
                if other.id == id || !reachable {
                    continue;
                }
                for key in new.keys {
                    assert!(
                        !other.keys.iter().any(|k| k.overlaps(key)),
                        "{id} clashes with {}",
                        other.id
                    );
                }
            }
        }
    }

    #[test]
    fn the_bisect_menu_refuses_as_the_marks_do() {
        use crate::bisect::{Target, mark_disabled, reset_disabled};
        use crate::git::bisect::{BisectState, Terms};
        use crate::git::status::BranchStatus;

        let branch = |head: Head, ops: &[Operation], bisect: Option<BisectState>| {
            Some(BranchStatus {
                head,
                upstream: None,
                ahead_behind: None,
                in_progress: ops.to_vec(),
                bisect,
            })
        };
        let custom = BisectState {
            terms: Terms {
                bad: "new".into(),
                good: "old".into(),
            },
            ..BisectState::default()
        };
        let mut states = Vec::new();
        for (head, ops, bisect) in [
            (Head::Branch("main".into()), vec![], None),
            (Head::Unborn("main".into()), vec![], None),
            (Head::Branch("main".into()), vec![Operation::Merge], None),
            (
                Head::Detached("abc".into()),
                vec![Operation::Bisect],
                Some(BisectState::default()),
            ),
            (
                Head::Detached("abc".into()),
                vec![Operation::Bisect],
                Some(custom),
            ),
        ] {
            let mut state = AppState::new(Context::Workspace);
            state.commits.rows = vec![crate::git::log::CommitRow {
                oid: "abc".into(),
                short_oid: "abc".into(),
                parents: vec![],
                author: "Ann".into(),
                time: 0,
                subject: "s".into(),
                subject_altered: false,
            }];
            state.branch = branch(head, &ops, bisect);
            states.push(state.clone());
            state.hold_op(1);
            states.push(state.clone());
            state.free_op();
            state.files = vec![crate::git::FileEntry {
                path: "a.rs".into(),
                orig_path: None,
                index: 'M',
                worktree: '.',
                kind: EntryKind::Changed,
            }];
            states.push(state.clone());
            state.commits.rows.clear();
            states.push(state);
        }
        let items = crate::menu::items(MenuId::Bisect);
        assert_eq!(items.len(), 4);
        for state in &states {
            for item in items {
                let expected = match item.intent {
                    AppIntent::BisectSelected(mark) => mark_disabled(state, Target::Selected, mark),
                    AppIntent::AskBisectReset => reset_disabled(state),
                    ref other => panic!("unexpected item {other:?}"),
                };
                assert_eq!((item.disabled)(state), expected, "{}", item.label);
            }
        }
    }
}
