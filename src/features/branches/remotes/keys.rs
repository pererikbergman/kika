//! The Remotes feature's key bindings: the Remotes tab's own scope and the
//! remove question answering with `y` or `n`. `CHUNKS` in `keymap.rs`
//! places both chunks, so their position there is behaviour. The tag push
//! guard and its helpers stay in core, because sync and tags use them too.

use crate::intent::AppIntent;
use crate::keymap::{
    Binding, KeyPattern, KeyScope, NONE, OP_RUNNING, Reason, list_guard, never, op_running,
};
use crate::model::AppState;
use crossterm::event::{KeyCode, KeyModifiers};

/// Why no remote can be added now: git one operation at a time.
pub(crate) fn add_remote_disabled(state: &AppState) -> Option<Reason> {
    state.op_seq.is_some().then_some(OP_RUNNING.into())
}

/// Why the selected remote branch cannot be checked out: a header row
/// is a remote, not a branch.
pub(crate) fn track_disabled(state: &AppState) -> Option<Reason> {
    if let Some(reason) = list_guard(state, false) {
        return Some(reason);
    }
    if state.op_seq.is_some() {
        Some(OP_RUNNING.into())
    } else if state.branches.remotes.selected_branch().is_none() {
        Some("Select a remote branch".into())
    } else {
        crate::bisect::head_move_disabled(state)
    }
}

/// Why the selected remote cannot be removed. Over a range, `d` asks
/// about the range, which skips the remotes this would refuse; only the
/// one-at-a-time rule is left to check.
pub(crate) fn change_remote_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, true).or_else(|| {
        if state.branches.remotes.range_anchor.is_some() {
            op_running(state)
        } else {
            remote_selected_disabled(state)
        }
    })
}

/// Why the selected remote cannot be renamed.
pub(crate) fn rename_remote_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| remote_selected_disabled(state))
}

/// Why the selected remote cannot be renamed or removed. `remote rename`
/// and `remote remove` take no `--` in older git, so a name like an option
/// is never passed; nor is a group of refs no configured remote owns.
fn remote_selected_disabled(state: &AppState) -> Option<Reason> {
    if state.op_seq.is_some() {
        return Some(OP_RUNNING.into());
    }
    match state.branches.remotes.selected_remote() {
        None => Some("No remote selected".into()),
        Some(remote) if !remote.configured => Some("Not a configured remote".into()),
        Some(remote) if remote.name.0.starts_with(b"-") => {
            Some("Change a remote named like an option with git directly".into())
        }
        Some(_) => None,
    }
}

// Chunk 49: remotes
pub(crate) static REMOTES_KEYS: &[Binding] = &[
    Binding {
        id: "remotes.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::Remotes,
        intent: AppIntent::RemoteNext,
        description: "Select the next remote or remote branch",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "remotes.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::Remotes,
        intent: AppIntent::RemotePrev,
        description: "Select the previous remote or remote branch",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "remotes.checkout",
        keys: &[KeyPattern::exact(KeyCode::Char(' '), NONE)],
        scope: KeyScope::Remotes,
        intent: AppIntent::TrackRemoteBranch,
        description: "Check out the selected remote branch as a new branch tracking it",
        label: "checkout",
        bar: true,
        disabled: track_disabled,
    },
    Binding {
        id: "remotes.add",
        keys: &[KeyPattern::exact(KeyCode::Char('a'), NONE)],
        scope: KeyScope::Remotes,
        intent: AppIntent::AddRemote,
        description: "Add a remote; fetch to see its branches",
        label: "add",
        bar: true,
        disabled: add_remote_disabled,
    },
    // Not `r`, which reloads everywhere in the workspace.
    Binding {
        id: "remotes.rename",
        keys: &[KeyPattern::any(KeyCode::Char('R'))],
        scope: KeyScope::Remotes,
        intent: AppIntent::RenameRemote,
        description: "Rename the selected remote",
        label: "rename",
        bar: true,
        disabled: rename_remote_disabled,
    },
    Binding {
        id: "remotes.remove",
        keys: &[KeyPattern::exact(KeyCode::Char('d'), NONE)],
        scope: KeyScope::Remotes,
        intent: AppIntent::RemoveRemote,
        description: "Remove the selected remote and its remote-tracking branches, asking first",
        label: "remove",
        bar: true,
        disabled: change_remote_disabled,
    },
    Binding {
        id: "remotes.switch_tab",
        keys: &[KeyPattern::exact(KeyCode::Char(']'), NONE)],
        scope: KeyScope::Remotes,
        intent: AppIntent::SwitchBranchesTab,
        description: "Show the tags",
        label: "tags",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "remotes.prev_tab",
        keys: &[KeyPattern::exact(KeyCode::Char('['), NONE)],
        scope: KeyScope::Remotes,
        intent: AppIntent::PrevBranchesTab,
        description: "Show the local branches",
        label: "local",
        bar: false,
        disabled: never,
    },
];

// Chunk 57: remotes
pub(crate) static REMOTES_REMOVE_CONFIRM: &[Binding] = &[
    // As with deleting a branch, Enter is left unbound: the remote's
    // branches go with it.
    Binding {
        id: "remove_remote.yes",
        keys: &[KeyPattern::exact(KeyCode::Char('y'), NONE)],
        scope: KeyScope::RemoveRemote,
        intent: AppIntent::ConfirmRemoveRemote,
        description: "Yes, remove the remote and its remote-tracking branches",
        label: "remove",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "remove_remote.no",
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::RemoveRemote,
        intent: AppIntent::ConfirmNo,
        description: "No, keep the remote",
        label: "keep",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "remove_remote.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::RemoveRemote,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{Keymap, Resolution, Trust, registry, resolve};
    use crate::model::Context;
    use crate::popup::Popup;
    use crate::test_support::{build, key, plain};

    #[test]
    fn the_remove_remote_question_answers_only_to_y_n_and_esc() {
        let mut state = AppState::new(Context::Workspace);
        state.popups = vec![Popup::RemoveRemote(crate::popup::RemoveRemoteConfirm {
            name: "fork".into(),
            op: crate::op::GitOp::RemoveRemote("fork".into()),
        })];
        let resolve_in = |key| {
            resolve(
                registry(),
                &Keymap::default(),
                KeyScope::RemoveRemote,
                &state,
                key,
            )
        };
        let ran = |key| match resolve_in(key) {
            Resolution::Run(binding) => binding.intent.clone(),
            other => panic!("{key:?}: {other:?}"),
        };
        assert_eq!(ran(plain('y')), AppIntent::ConfirmRemoveRemote);
        assert_eq!(ran(plain('n')), AppIntent::ConfirmNo);
        assert_eq!(
            ran(key(KeyCode::Esc, KeyModifiers::NONE)),
            AppIntent::ConfirmNo
        );
        assert_eq!(
            ran(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            AppIntent::Quit
        );
        for unbound in [key(KeyCode::Enter, KeyModifiers::NONE), plain('q')] {
            assert!(
                matches!(resolve_in(unbound), Resolution::Unbound),
                "{unbound:?}"
            );
        }
        assert_eq!(KeyScope::RemoveRemote.parent(), None);
        for id in [
            "remotes.remove",
            "remove_remote.yes",
            "remove_remote.no",
            "remove_remote.quit",
        ] {
            assert!(build(&[(id, &["F"])], Trust::Repo).is_err(), "{id}");
        }
        assert!(build(&[("remotes.add", &["F"])], Trust::Repo).is_ok());
    }
}
