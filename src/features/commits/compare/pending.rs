//! The comparison in flight: what `open_comparison` asked for, kept on
//! `AppState.pending_diff` until both ends are read as commits.

/// A comparison asked for, kept until its ends are read as commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDiff {
    pub seq: u64,
    /// The mark's label, for the title.
    pub against: String,
    /// The compared commit's or branch's label, for the title.
    pub target: String,
}
