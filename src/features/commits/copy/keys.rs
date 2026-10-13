//! Copy and paste's key bindings: `c` copies the selected commit or
//! range, `v` pastes the copied set, `C` clears it, and `y` opens the
//! copy menu. `CHUNKS` in `keymap.rs` places the chunk, so its position
//! there is behaviour.

use crate::git::status::{EntryKind, Head};
use crate::intent::AppIntent;
use crate::keymap::{
    Binding, KeyPattern, KeyScope, NONE, OP_RUNNING, Reason, in_progress_disabled, list_guard,
    never, no_commit,
};
use crate::menu::MenuId;
use crate::model::AppState;
use crossterm::event::KeyCode;

/// Why the selected commit cannot be copied. A merge commit needs a
/// mainline chosen to be picked, which Kika does not ask for. Copying
/// runs no git, so it works while an operation runs.
pub(crate) fn copy_disabled(state: &AppState) -> Option<Reason> {
    no_commit(state).or_else(|| {
        let row = state.commits.rows.get(state.commits.selected);
        row.is_some_and(|r| r.parents.len() > 1)
            .then_some("Cannot cherry-pick a merge commit".into())
    })
}

/// Why the copied commits cannot be pasted now. Untracked files may stay:
/// git refuses to overwrite one before it changes anything. Any tracked
/// change is refused, though git would pick around some, because it would
/// mix into a later conflict.
pub(crate) fn paste_disabled(state: &AppState) -> Option<Reason> {
    let unborn = state
        .branch
        .as_ref()
        .is_none_or(|b| matches!(b.head, Head::Unborn(_)));
    if state.op_is_running() {
        Some(OP_RUNNING.into())
    } else if state.copied.is_empty() {
        Some("Nothing copied".into())
    } else if unborn {
        Some("No commit to cherry-pick onto".into())
    } else if let Some(reason) = in_progress_disabled(state) {
        Some(reason)
    } else if state.files.iter().any(|f| f.kind != EntryKind::Untracked) {
        Some("Commit or stash your changes first".into())
    } else {
        None
    }
}

/// The copy menu copies the selected commit's text: one row's action.
fn commits_copy_menu_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false)
}

/// `c` copies a range too, but a range of merge commits only has nothing
/// it can copy.
pub(crate) fn commits_copy_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, true).or_else(|| {
        if state.commits.range_anchor.is_none() {
            return copy_disabled(state);
        }
        let rows = &state.commits.rows;
        let range = state.commits.range();
        no_commit(state).or_else(|| {
            range
                .iter()
                .all(|&i| rows[i].parents.len() > 1)
                .then_some("Cannot cherry-pick a merge commit".into())
        })
    })
}

// Chunk 14: copy
pub(crate) static COPY_COMMITS: &[Binding] = &[
    Binding {
        id: "commits.copy",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), NONE)],
        scope: KeyScope::Commits,
        intent: AppIntent::CopyCommit,
        description: "Add the selected commit to the copied set, or take it out",
        label: "copy",
        bar: true,
        disabled: commits_copy_disabled,
    },
    Binding {
        id: "commits.paste",
        keys: &[KeyPattern::exact(KeyCode::Char('v'), NONE)],
        scope: KeyScope::Commits,
        intent: AppIntent::PasteCommits,
        description: "Cherry-pick the copied commits onto the current branch, oldest first",
        label: "paste",
        bar: true,
        disabled: paste_disabled,
    },
    Binding {
        id: "commits.clear_copied",
        keys: &[KeyPattern::any(KeyCode::Char('C'))],
        scope: KeyScope::Commits,
        intent: AppIntent::ClearCopied,
        description: "Clear the copied set",
        label: "clear",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "commits.copy_menu",
        keys: &[KeyPattern::exact(KeyCode::Char('y'), NONE)],
        scope: KeyScope::Commits,
        intent: AppIntent::OpenMenu(MenuId::Copy),
        description: "Copy hash or message (the terminal must allow OSC 52; in tmux, set-clipboard on)",
        label: "clipboard",
        // Out of the bar, which is full at 80 columns; the help menu lists it.
        bar: false,
        disabled: commits_copy_menu_disabled,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::commit_files::CommitId;
    use crate::keymap::{Keymap, REPO_LOCKED, Resolution, Trust, registry, resolve};
    use crate::model::Context;
    use crate::test_support::{binding, build, key, plain};
    use crossterm::event::KeyModifiers;

    #[test]
    fn c_v_and_shift_c_copy_paste_and_clear_in_the_commits_panel() {
        use AppIntent::*;
        // Everything enabled: a commit to copy, one copied, a clean branch.
        let mut state = AppState::new(Context::Workspace);
        let oid = "a".repeat(40);
        state.commits.rows = vec![crate::git::log::CommitRow {
            oid: oid.clone(),
            short_oid: "aaaaaaa".into(),
            parents: vec![],
            author: "Ann".into(),
            time: 0,
            subject: "s".into(),
            subject_altered: false,
        }];
        state.copied.toggle(super::super::copied::CopiedCommit {
            id: CommitId::parse(&oid).unwrap(),
            short: "aaaaaaa".into(),
            subject: "s".into(),
        });
        state.branch = Some(crate::git::status::BranchStatus {
            head: Head::Branch("main".into()),
            upstream: None,
            ahead_behind: None,
            in_progress: vec![],
            bisect: None,
        });
        let run = |k| match resolve(registry(), &Keymap::default(), KeyScope::Commits, &state, k) {
            Resolution::Run(binding) => binding.intent.clone(),
            other => panic!("{k:?}: {other:?}"),
        };
        assert_eq!(run(plain('c')), CopyCommit);
        assert_eq!(run(plain('v')), PasteCommits);
        assert_eq!(
            run(key(KeyCode::Char('C'), KeyModifiers::SHIFT)),
            ClearCopied
        );
        assert!(binding("commits.copy").bar && binding("commits.paste").bar);
        assert!(!binding("commits.clear_copied").bar);
    }

    #[test]
    fn the_copy_keys_clash_with_nothing_the_commits_panel_reaches() {
        let reached = [KeyScope::Commits, KeyScope::Workspace, KeyScope::Global];
        for id in ["commits.copy", "commits.paste", "commits.clear_copied"] {
            let new = binding(id);
            for other in registry()
                .iter()
                .filter(|b| b.id != id && reached.contains(&b.scope))
            {
                for a in new.keys {
                    for b in other.keys {
                        assert!(!a.overlaps(b), "{id} {a:?} clashes with {}", other.id);
                    }
                }
            }
        }
    }

    #[test]
    fn paste_is_locked_and_copy_and_clear_are_not() {
        assert!(REPO_LOCKED.contains(&"commits.paste"));
        assert!(build(&[("commits.paste", &["Z"])], Trust::Repo).is_err());
        for id in ["commits.copy", "commits.clear_copied"] {
            assert!(!REPO_LOCKED.contains(&id), "{id}");
            assert!(build(&[(id, &["Z"])], Trust::Repo).is_ok(), "{id}");
        }
    }
}
