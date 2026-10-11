//! Noticing changes made outside Kika: what to poll, how often, and which
//! background loads are already on their way.

use std::time::Duration;

use crate::git::refs::RefsSnapshot;

/// How often each watch runs. `fetch: None` turns auto-fetch off; a
/// `Some` interval is the user's opt-in to background network access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchConfig {
    pub refs: Duration,
    pub files: Duration,
    pub fetch: Option<Duration>,
}

/// Built in until the settings file exists. Refs are cheap to read, so they
/// are polled more often than `git status`. Auto-fetch is off: it talks to
/// the network, which Kika does only when the user has turned it on.
pub const DEFAULT_WATCH: WatchConfig = WatchConfig {
    refs: Duration::from_secs(2),
    files: Duration::from_secs(5),
    fetch: None,
};

/// A reason to look at the repository again without the user asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchEvent {
    PollRefs,
    PollFiles,
    /// The terminal got focus back: the user may have changed the repo elsewhere.
    FocusGained,
    Fetch,
}

/// The watch's own bookkeeping. Each `*_pending` flag stays up until its load
/// reports, so ticks that pile up (say, while an editor has the terminal)
/// start one load, not one each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchState {
    /// The last refs snapshot; `None` until the first one arrives.
    pub refs: Option<RefsSnapshot>,
    pub refs_pending: bool,
    pub status_pending: bool,
    pub fetch_pending: bool,
    /// Whether the user opted in to auto-fetch; off unless they did.
    pub fetch_enabled: bool,
}

impl Default for WatchState {
    fn default() -> Self {
        Self {
            refs: None,
            refs_pending: false,
            status_pending: false,
            fetch_pending: false,
            fetch_enabled: DEFAULT_WATCH.fetch.is_some(),
        }
    }
}

impl WatchState {
    /// Claim the refs slot; `false` when a snapshot is already on its way.
    pub fn start_refs(&mut self) -> bool {
        !std::mem::replace(&mut self.refs_pending, true)
    }

    /// Claim the fetch slot; `false` when fetch is off or already running.
    pub fn start_fetch(&mut self) -> bool {
        self.fetch_enabled && !std::mem::replace(&mut self.fetch_pending, true)
    }

    /// Keep `snapshot` and say whether the refs moved since the last one.
    /// The first snapshot has nothing to compare with, so it is no change.
    pub fn refs_changed(&mut self, snapshot: RefsSnapshot) -> bool {
        let changed = self.refs.as_ref().is_some_and(|old| *old != snapshot);
        self.refs = Some(snapshot);
        changed
    }

    /// Whether any background load has yet to report; tests wait on it.
    #[cfg(test)]
    pub fn is_pending(&self) -> bool {
        self.refs_pending || self.status_pending || self.fetch_pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_is_off_by_default_because_it_needs_the_network() {
        assert_eq!(DEFAULT_WATCH.fetch, None);
        assert!(!WatchState::default().fetch_enabled);
        assert!(!WatchState::default().is_pending());
    }

    #[test]
    fn a_slot_is_claimed_once_until_it_is_cleared() {
        let mut watch = WatchState {
            fetch_enabled: true,
            ..WatchState::default()
        };
        assert!(watch.start_refs());
        assert!(!watch.start_refs());
        assert!(watch.start_fetch());
        assert!(!watch.start_fetch());
        assert!(watch.is_pending());
        watch.refs_pending = false;
        assert!(watch.start_refs());
    }

    #[test]
    fn a_disabled_fetch_never_starts() {
        let mut watch = WatchState {
            fetch_enabled: false,
            ..WatchState::default()
        };
        assert!(!watch.start_fetch());
        assert!(!watch.fetch_pending);
    }
}
