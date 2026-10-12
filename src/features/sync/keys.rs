//! The sync feature's key bindings and their guards: `f`, `p` and `P` in
//! the workspace fetch, pull and push, and the credential question's own
//! scope sends, cancels or quits. `CHUNKS` in `keymap.rs` places the two
//! chunks, so their position there is behaviour.

use crate::git::status::Head;
use crate::intent::AppIntent;
use crate::keymap::{Binding, KeyPattern, KeyScope, NONE, Reason, never, remote_disabled};
use crate::model::AppState;
use crossterm::event::{KeyCode, KeyModifiers};

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
        intent: AppIntent::Fetch,
        description: "Fetch every remote",
        label: "fetch",
        bar: false,
        disabled: fetch_disabled,
    },
    Binding {
        id: "workspace.pull",
        keys: &[KeyPattern::exact(KeyCode::Char('p'), NONE)],
        scope: KeyScope::Workspace,
        intent: AppIntent::Pull,
        description: "Pull into the current branch",
        label: "pull",
        bar: false,
        disabled: pull_push_disabled,
    },
    Binding {
        id: "workspace.push",
        keys: &[KeyPattern::any(KeyCode::Char('P'))],
        scope: KeyScope::Workspace,
        intent: AppIntent::Push,
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
