//! The I/O behind the diff's own effect: the timer that marks a slow load.
//! `LoadDiff`, `DiffDemand` and `CancelDiff` go straight to the
//! `DiffStreamer` that `Runtime` holds.

use tokio::sync::mpsc::UnboundedSender;

use crate::effect::{DIFF_LOADING_DELAY, Tag};
use crate::intent::AppIntent;

/// Report `DiffSlow` after [`DIFF_LOADING_DELAY`], so fast loads never
/// flash "Loading…".
pub(crate) fn loading_delay(tx: UnboundedSender<AppIntent>, tag: Tag) {
    tokio::spawn(async move {
        tokio::time::sleep(DIFF_LOADING_DELAY).await;
        let _ = tx.send(AppIntent::DiffSlow { tag });
    });
}
