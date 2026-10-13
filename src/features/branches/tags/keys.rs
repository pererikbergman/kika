//! The Tags feature's key bindings: the Tags tab's own scope, making a tag
//! from the Commits panel, and the delete question answering with `y` or
//! `n`. `CHUNKS` in `keymap.rs` places both chunks, so their position there
//! is behaviour. The push guard and its helpers stay in core, because sync
//! starts the push through them too.

use super::TagsIntent;
use crate::confirm_question::Answer;
use crate::features::branches::BranchesIntent;
use crate::git::commit_files::CommitId;
use crate::intent::AppIntent;
use crate::keymap::{
    Binding, KeyPattern, KeyScope, NONE, OP_RUNNING, Reason, list_guard, never, push_tag_disabled,
    tag_selected_disabled,
};
use crate::model::AppState;
use crossterm::event::{KeyCode, KeyModifiers};

/// Why the selected commit cannot be tagged: the log needs a commit with
/// an id git can use. Not refused while an operation runs, since the
/// prompt comes first; the answer waits for it instead.
pub(crate) fn tag_commit_disabled(state: &AppState) -> Option<Reason> {
    let row = state.commits.rows.get(state.commits.selected);
    let valid = row.is_some_and(|r| CommitId::parse(&r.oid).is_some());
    (!valid).then_some("No commit".into())
}

/// Why the selected tag, or the range, cannot be deleted now: git one
/// operation at a time.
pub(crate) fn delete_tag_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, true).or_else(|| {
        if state.op_is_running() {
            Some(OP_RUNNING.into())
        } else if state.branches.tags.range_anchor.is_some() {
            None
        } else {
            tag_selected_disabled(state)
        }
    })
}

fn commits_tag_disabled(state: &AppState) -> Option<Reason> {
    list_guard(state, false).or_else(|| tag_commit_disabled(state))
}

// Chunk 50: tags
pub(crate) static TAGS_KEYS: &[Binding] = &[
    Binding {
        id: "tags.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::Tags,
        intent: AppIntent::Branches(BranchesIntent::Tags(TagsIntent::Next)),
        description: "Select the next tag",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "tags.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::Tags,
        intent: AppIntent::Branches(BranchesIntent::Tags(TagsIntent::Prev)),
        description: "Select the previous tag",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "tags.delete",
        keys: &[KeyPattern::exact(KeyCode::Char('d'), NONE)],
        scope: KeyScope::Tags,
        intent: AppIntent::Branches(BranchesIntent::Tags(TagsIntent::Delete)),
        description: "Delete the selected tag here, not on any remote, asking first",
        label: "delete",
        bar: true,
        disabled: delete_tag_disabled,
    },
    // Shadows `workspace.push` on purpose: here `P` pushes the tag.
    Binding {
        id: "tags.push",
        keys: &[KeyPattern::any(KeyCode::Char('P'))],
        scope: KeyScope::Tags,
        intent: AppIntent::Branches(BranchesIntent::Tags(TagsIntent::Push)),
        description: "Push the selected tag to the upstream's remote, or the only remote",
        label: "push tag",
        bar: true,
        disabled: push_tag_disabled,
    },
    Binding {
        id: "tags.switch_tab",
        keys: &[KeyPattern::exact(KeyCode::Char(']'), NONE)],
        scope: KeyScope::Tags,
        intent: AppIntent::Branches(BranchesIntent::SwitchTab),
        description: "Show the local branches",
        label: "local",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "tags.prev_tab",
        keys: &[KeyPattern::exact(KeyCode::Char('['), NONE)],
        scope: KeyScope::Tags,
        intent: AppIntent::Branches(BranchesIntent::PrevTab),
        description: "Show the remotes and their branches",
        label: "remotes",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "commits.tag",
        keys: &[KeyPattern::exact(KeyCode::Char('t'), NONE)],
        scope: KeyScope::Commits,
        intent: AppIntent::Branches(BranchesIntent::Tags(TagsIntent::New)),
        description: "Tag the selected commit with a lightweight tag",
        label: "tag",
        bar: true,
        disabled: commits_tag_disabled,
    },
    Binding {
        id: "commits.tag_annotated",
        keys: &[KeyPattern::any(KeyCode::Char('T'))],
        scope: KeyScope::Commits,
        intent: AppIntent::Branches(BranchesIntent::Tags(TagsIntent::NewAnnotated)),
        description: "Tag the selected commit with an annotated tag and a message",
        label: "annotated tag",
        bar: false,
        disabled: commits_tag_disabled,
    },
];

// Chunk 55: tags
pub(crate) static TAGS_DELETE_CONFIRM: &[Binding] = &[
    // As with deleting a branch, Enter is left unbound: a deleted tag
    // cannot be listed again to undo it.
    Binding {
        id: "delete_tag.yes",
        keys: Answer::YOnly.keys(),
        scope: KeyScope::DeleteTag,
        intent: AppIntent::Branches(BranchesIntent::Tags(TagsIntent::ConfirmDelete)),
        description: "Yes, delete the tag here",
        label: "delete",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "delete_tag.no",
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::DeleteTag,
        intent: AppIntent::ConfirmNo,
        description: "No, keep the tag",
        label: "keep",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "delete_tag.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::DeleteTag,
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
    use crate::keymap::{Keymap, REPO_LOCKED, Resolution, Trust, registry, resolve};
    use crate::model::Context;
    use crate::popup::Popup;
    use crate::test_support::{build, key, plain};

    #[test]
    fn t_and_shift_t_tag_the_selected_commit_and_need_a_usable_id() {
        let row = |oid: &str| crate::git::log::CommitRow {
            oid: oid.into(),
            short_oid: "abc".into(),
            parents: vec![],
            author: "Ann".into(),
            time: 0,
            subject: "s".into(),
            subject_altered: false,
        };
        let mut state = AppState::new(Context::Workspace);
        let resolve_in = |state: &AppState, key| {
            resolve(
                registry(),
                &Keymap::default(),
                KeyScope::Commits,
                state,
                key,
            )
        };
        let shift_t = key(KeyCode::Char('T'), KeyModifiers::SHIFT);
        for key in [plain('t'), shift_t] {
            assert!(matches!(
                resolve_in(&state, key),
                Resolution::Disabled(ref reason) if reason == "No commit"
            ));
        }
        state.commits.rows = vec![row("not hex")];
        assert_eq!(tag_commit_disabled(&state), Some("No commit".into()));
        state.commits.rows = vec![row(&"a".repeat(40))];
        // An operation running does not refuse the prompt; the answer waits.
        state.hold_op(1);
        let ran = |key| match resolve_in(&state, key) {
            Resolution::Run(binding) => binding.intent.clone(),
            other => panic!("{key:?}: {other:?}"),
        };
        assert_eq!(
            ran(plain('t')),
            AppIntent::Branches(BranchesIntent::Tags(TagsIntent::New))
        );
        assert_eq!(
            ran(shift_t),
            AppIntent::Branches(BranchesIntent::Tags(TagsIntent::NewAnnotated))
        );
        assert!(!REPO_LOCKED.contains(&"commits.tag"));
        assert!(!REPO_LOCKED.contains(&"commits.tag_annotated"));
    }

    #[test]
    fn the_delete_tag_question_answers_only_to_y_n_and_esc() {
        let mut state = AppState::new(Context::Workspace);
        state.popups = vec![Popup::Question(super::super::update::delete_tag_question(
            "v1".into(),
            crate::op::GitOp::DeleteTag {
                name: "v1".into(),
                oid: "1".repeat(40),
            },
        ))];
        let resolve_in = |key| {
            resolve(
                registry(),
                &Keymap::default(),
                KeyScope::DeleteTag,
                &state,
                key,
            )
        };
        let ran = |key| match resolve_in(key) {
            Resolution::Run(binding) => binding.intent.clone(),
            other => panic!("{key:?}: {other:?}"),
        };
        assert_eq!(
            ran(plain('y')),
            AppIntent::Branches(BranchesIntent::Tags(TagsIntent::ConfirmDelete))
        );
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
        assert_eq!(KeyScope::DeleteTag.parent(), None);
        // No binding in the scope has Enter among its keys.
        let enter = KeyPattern::any(KeyCode::Enter);
        for b in registry().iter().filter(|b| b.scope == KeyScope::DeleteTag) {
            assert!(!b.keys.iter().any(|k| k.code == enter.code), "{}", b.id);
        }
        for id in [
            "tags.delete",
            "tags.push",
            "delete_tag.yes",
            "delete_tag.no",
            "delete_tag.quit",
        ] {
            assert!(REPO_LOCKED.contains(&id), "{id}");
            assert!(build(&[(id, &["F"])], Trust::Repo).is_err(), "{id}");
        }
        assert!(build(&[("tags.next", &["F"])], Trust::Repo).is_ok());
    }
}
