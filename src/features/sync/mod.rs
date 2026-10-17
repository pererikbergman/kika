//! Sync: fetch, pull and push run by hand (starting one, the SSH notice,
//! the spinner, finishing, and the force-with-lease offer after a rejected
//! push), the auto-fetch result, and the quiet fetch command auto-fetch runs
//! (`git.rs`). The feature owns its `update` handlers (`update.rs`), its key
//! bindings and guards (`keys.rs`) and the I/O behind `Effect::Fetch` and
//! `Effect::Remote` (`effects.rs`).
//!
//! Its intents are nested (ADR-0002): core wraps [`SyncIntent`] in
//! `AppIntent::Sync`, and its one `update` arm calls [`update`] here.
//!
//! Its `pub(crate)` API: the intent enum [`SyncIntent`] and the free
//! function [`update`]; the handlers `fetch_finished`, `start_remote`,
//! `current_remote`, `remote_notice`, `spinner_tick` and `remote_finished` on
//! `AppState`, the last asking `force_push_question` after a rejected push;
//! the chunks `keys::{SYNC_WORKSPACE, SYNC_CREDENTIAL,
//! SYNC_FORCE_PUSH_CONFIRM}`; and `effects::{fetch, remote}`.
//!
//! Kept cross-feature reads and calls, left as they are because this slice
//! only moves code:
//! - `start_remote` reads `branches.current()` (branches) and calls
//!   `keymap::push_tag_disabled` (tags);
//! - `remote_finished` calls `forge.want_refresh()` (pull_requests);
//! - tags' `push_tag` calls `start_remote`.
//!
//! What stays in core: the credential plumbing, because review's commit
//! fetch shares it (`Bridge`, `AnswerSlot` and `lock` in `effect.rs`, the
//! `AnswerCredential` arm, `credential_asked`, `credential_answered`,
//! `close_credential` and `quit_interrupts` (`op_slot.rs`), the `Quit` arm, `Popup::Credential` and its view);
//! `RemoteRun`, `AppState::remote`, `remote_disabled` and `REMOTE_BUSY`; the
//! auto-fetch trigger in `watch()` and `WatchState`; `refresh::remote_plan`;
//! and `kika_git::remote` and `kika_git::askpass`.

pub(crate) mod effects;
mod git;
mod intent;
pub(crate) mod keys;
mod update;

pub(crate) use intent::SyncIntent;
pub(crate) use update::update;

#[cfg(test)]
pub(crate) use update::force_push_question;
