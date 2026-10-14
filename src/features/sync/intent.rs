//! The sync feature's intents. Core wraps them in one variant,
//! `AppIntent::Sync`, and hands them to this feature's `update`
//! (see `docs/adr/0002-nested-intents.md`).

use crate::effect::Tag;
use kika_git::remote::{RemoteFailure, RemoteOp};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SyncIntent {
    /// A background fetch finished.
    FetchFinished {
        tag: Tag,
        result: Result<(), String>,
    },
    /// Fetch every remote, pull into the current branch, or push it. Each
    /// reaches the network, so only a key the user pressed sends one.
    Fetch,
    Pull,
    Push,
    /// Push again with `--force-with-lease`, after a plain push was rejected.
    ForcePush,
    /// Ssh, for the remote operation `tag`, says something that needs no
    /// answer, such as asking to touch a security key. Terminal-safe.
    // Only the unix bridge passes these on.
    #[cfg_attr(not(unix), allow(dead_code))]
    RemoteNotice {
        tag: Tag,
        text: String,
    },
    /// A fetch, pull or push finished.
    RemoteFinished {
        tag: Tag,
        op: RemoteOp,
        result: Result<(), RemoteFailure>,
    },
    /// Time to draw the next frame of the remote operation's spinner.
    SpinnerTick,
}
