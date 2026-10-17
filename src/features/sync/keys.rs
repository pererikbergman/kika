//! The sync feature's key bindings and their guards: `f`, `p` and `P` in
//! the workspace fetch, pull and push, the credential question's own
//! scope sends, cancels or quits, and the force-push question's own keys
//! answer it. `CHUNKS` in `keymap.rs` places the three chunks, so their
//! position there is behaviour.

use crate::confirm_question::Answer;
use crate::features::sync::SyncIntent;
use crate::intent::AppIntent;
use crate::keymap::{Binding, KeyPattern, KeyScope, NONE, Reason, never, remote_disabled};
use crate::model::AppState;
use crossterm::event::{KeyCode, KeyModifiers};
use kika_git::status::Head;

/// A fetch also waits for auto-fetch, which would race it for the same refs.
pub(crate) fn fetch_disabled(state: &AppState) -> Option<Reason> {
    remote_disabled(state).or_else(|| {
        state
            .watch
            .fetch_pending
            .then_some("A background fetch is running".into())
    })
}

/// Pull and push work on the current branch, so they need one.
pub(crate) fn pull_push_disabled(state: &AppState) -> Option<Reason> {
    remote_disabled(state).or_else(|| {
        let detached = state
            .branch
            .as_ref()
            .is_some_and(|b| matches!(b.head, Head::Detached(_)));
        detached.then_some("HEAD is detached".into())
    })
}

// Chunk 2: sync
pub(crate) static SYNC_WORKSPACE: &[Binding] = &[
    Binding {
        id: "workspace.fetch",
        keys: &[KeyPattern::exact(KeyCode::Char('f'), NONE)],
        scope: KeyScope::Workspace,
        intent: AppIntent::Sync(SyncIntent::Fetch),
        description: "Fetch every remote",
        label: "fetch",
        bar: false,
        disabled: fetch_disabled,
    },
    Binding {
        id: "workspace.pull",
        keys: &[KeyPattern::exact(KeyCode::Char('p'), NONE)],
        scope: KeyScope::Workspace,
        intent: AppIntent::Sync(SyncIntent::Pull),
        description: "Pull into the current branch",
        label: "pull",
        bar: false,
        disabled: pull_push_disabled,
    },
    Binding {
        id: "workspace.push",
        keys: &[KeyPattern::any(KeyCode::Char('P'))],
        scope: KeyScope::Workspace,
        intent: AppIntent::Sync(SyncIntent::Push),
        description: "Push the current branch",
        label: "push",
        bar: false,
        disabled: pull_push_disabled,
    },
];

// Chunk 42: sync
pub(crate) static SYNC_CREDENTIAL: &[Binding] = &[
    Binding {
        id: "credential.answer",
        keys: &[KeyPattern::any(KeyCode::Enter)],
        scope: KeyScope::Credential,
        intent: AppIntent::SubmitCredential,
        description: "Send the typed answer to git",
        label: "send",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "credential.cancel",
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::Credential,
        intent: AppIntent::CredentialAnswered(None),
        description: "Cancel the question, which fails the operation",
        label: "cancel",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "credential.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::Credential,
        intent: AppIntent::Quit,
        description: "Cancel the question and quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
];

// Chunk 42b: sync
pub(crate) static SYNC_FORCE_PUSH_CONFIRM: &[Binding] = &[
    // As with discarding, Enter is left unbound: a force push replaces
    // commits on the remote.
    Binding {
        id: "force_push_confirm.yes",
        keys: Answer::YOnly.keys(),
        scope: KeyScope::ForcePushConfirm,
        intent: AppIntent::ConfirmYes,
        description: "Yes, push with --force-with-lease",
        label: "force",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "force_push_confirm.no",
        keys: &[
            KeyPattern::exact(KeyCode::Char('n'), NONE),
            KeyPattern::any(KeyCode::Esc),
        ],
        scope: KeyScope::ForcePushConfirm,
        intent: AppIntent::ConfirmNo,
        description: "No, push nothing",
        label: "cancel",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "force_push_confirm.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::ForcePushConfirm,
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
    use crate::keymap::{Keymap, Resolution, registry, resolve};
    use crate::model::Context;
    use crate::test_support::{key, plain};

    #[test]
    fn the_force_push_question_has_no_enter_and_n_or_esc_pushes_nothing() {
        let scope = KeyScope::ForcePushConfirm;
        let state = AppState::new(Context::Workspace);
        let resolve_in = |key| match resolve(registry(), &Keymap::default(), scope, &state, key) {
            Resolution::Run(binding) => Some(binding.intent.clone()),
            Resolution::Unbound => None,
            other => panic!("{key:?}: {other:?}"),
        };
        assert_eq!(resolve_in(key(KeyCode::Enter, NONE)), None);
        assert_eq!(resolve_in(plain('q')), None);
        assert_eq!(resolve_in(plain('y')), Some(AppIntent::ConfirmYes));
        assert_eq!(resolve_in(plain('n')), Some(AppIntent::ConfirmNo));
        assert_eq!(
            resolve_in(key(KeyCode::Esc, NONE)),
            Some(AppIntent::ConfirmNo)
        );
        assert_eq!(
            resolve_in(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(AppIntent::Quit)
        );
        assert_eq!(scope.parent(), None);
        assert_eq!(scope.title(), "Force push");

        let enter = key(KeyCode::Enter, NONE);
        for b in registry().iter().filter(|b| b.scope == scope) {
            assert!(!b.keys.iter().any(|k| k.matches(enter)), "{}", b.id);
        }
        let label = |id| registry().iter().find(|b| b.id == id).map(|b| b.label);
        assert_eq!(label("force_push_confirm.yes"), Some("force"));
        assert_eq!(label("force_push_confirm.no"), Some("cancel"));
        assert_eq!(label("force_push_confirm.quit"), Some("quit"));
    }
}
