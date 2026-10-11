//! The git commands Kika ran this session, oldest first, held in memory
//! and capped so a long session cannot grow it without limit. Pure.

use std::collections::VecDeque;
use std::time::Duration;

use crate::git::recording::{CommandEvent, CommandOutcome};

/// One command in the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub id: u64,
    /// Already cleaned of secrets by the runner.
    pub line: String,
    /// Run on the terminal during a hand-off.
    pub attached: bool,
    /// `None` while it runs.
    pub outcome: Option<CommandOutcome>,
    pub elapsed: Option<Duration>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandLog {
    entries: VecDeque<LogEntry>,
}

impl CommandLog {
    /// Entries kept; the oldest go first.
    pub const CAP: usize = 500;

    pub fn entries(&self) -> &VecDeque<LogEntry> {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Apply a runner's event. Returns how many old entries it evicted.
    pub fn apply(&mut self, event: CommandEvent) -> usize {
        match event {
            CommandEvent::Started { id, line, attached } => self.started(id, line, attached),
            CommandEvent::Finished {
                id,
                outcome,
                elapsed,
            } => {
                self.finished(id, outcome, elapsed);
                0
            }
        }
    }

    /// Add a running command at the end, dropping the oldest over
    /// [`Self::CAP`]. Returns how many were dropped.
    pub fn started(&mut self, id: u64, line: String, attached: bool) -> usize {
        self.entries.push_back(LogEntry {
            id,
            line,
            attached,
            outcome: None,
            elapsed: None,
        });
        let evicted = self.entries.len().saturating_sub(Self::CAP);
        self.entries.drain(..evicted);
        evicted
    }

    /// Record how command `id` ended. An id no longer in the log, because
    /// it was evicted, is ignored.
    pub fn finished(&mut self, id: u64, outcome: CommandOutcome, elapsed: Duration) {
        // Recent commands are at the end, so search from there.
        if let Some(entry) = self.entries.iter_mut().rev().find(|e| e.id == id) {
            entry.outcome = Some(outcome);
            entry.elapsed = Some(elapsed);
        }
    }
}

/// The Command log popup's cursor. While `follow` is set the cursor stays
/// on the newest entry as new ones arrive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandLogView {
    pub cursor: usize,
    pub follow: bool,
}

impl CommandLogView {
    /// On the newest of `len` entries, following.
    pub fn open(len: usize) -> Self {
        Self {
            cursor: len.saturating_sub(1),
            follow: true,
        }
    }

    /// The log now has `len` entries, after `evicted` were dropped from
    /// the front. The cursor stays on its entry, or follows the newest.
    pub fn logged(&mut self, len: usize, evicted: usize) {
        self.cursor = if self.follow {
            len.saturating_sub(1)
        } else {
            self.cursor.saturating_sub(evicted)
        };
    }

    /// Move by `delta` rows within `len` entries. Following is on exactly
    /// when the cursor lands on the newest entry.
    pub fn move_by(&mut self, delta: isize, len: usize) {
        let last = len.saturating_sub(1);
        self.cursor = self.cursor.saturating_add_signed(delta).min(last);
        self.follow = self.cursor == last;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started(id: u64) -> CommandEvent {
        CommandEvent::Started {
            id,
            line: format!("git cmd{id}"),
            attached: false,
        }
    }

    fn finished(id: u64, outcome: CommandOutcome) -> CommandEvent {
        CommandEvent::Finished {
            id,
            outcome,
            elapsed: Duration::from_millis(id),
        }
    }

    fn ids(log: &CommandLog) -> Vec<u64> {
        log.entries().iter().map(|e| e.id).collect()
    }

    #[test]
    fn started_commands_append_in_order_and_finish_in_place() {
        let mut log = CommandLog::default();
        assert!(log.is_empty());
        for id in 0..3 {
            assert_eq!(log.apply(started(id)), 0);
        }
        assert_eq!(ids(&log), [0, 1, 2]);
        assert_eq!(log.apply(finished(1, CommandOutcome::Failed(None))), 0);

        let entries = log.entries();
        assert_eq!(entries[1].outcome, Some(CommandOutcome::Failed(None)));
        assert_eq!(entries[1].elapsed, Some(Duration::from_millis(1)));
        assert_eq!(entries[1].line, "git cmd1");
        for i in [0, 2] {
            assert_eq!(
                (entries[i].outcome.clone(), entries[i].elapsed),
                (None, None)
            );
        }
    }

    #[test]
    fn the_log_keeps_only_the_newest_cap_entries() {
        let mut log = CommandLog::default();
        let total = CommandLog::CAP as u64 + 10;
        let evicted: usize = (0..total).map(|id| log.apply(started(id))).sum();
        assert_eq!(evicted, 10);
        assert_eq!(log.len(), CommandLog::CAP);
        assert_eq!(log.entries().front().unwrap().id, 10);
        assert_eq!(log.entries().back().unwrap().id, total - 1);
    }

    #[test]
    fn a_finish_for_an_evicted_or_unknown_id_changes_nothing() {
        let mut log = CommandLog::default();
        for id in 0..CommandLog::CAP as u64 + 1 {
            log.apply(started(id));
        }
        let before = log.clone();
        log.apply(finished(0, CommandOutcome::Ok));
        log.apply(finished(9999, CommandOutcome::Ok));
        assert_eq!(log, before);
    }

    #[test]
    fn the_cursor_follows_the_newest_entry_until_moved_up() {
        let mut view = CommandLogView::open(0);
        assert_eq!(
            view,
            CommandLogView {
                cursor: 0,
                follow: true
            }
        );
        view.logged(3, 0);
        assert_eq!(view.cursor, 2);

        view.move_by(-1, 3);
        assert_eq!(
            view,
            CommandLogView {
                cursor: 1,
                follow: false
            }
        );
        view.logged(4, 0);
        assert_eq!(view.cursor, 1);

        // Back onto the newest entry follows again.
        view.move_by(5, 4);
        assert_eq!(
            view,
            CommandLogView {
                cursor: 3,
                follow: true
            }
        );
        view.move_by(-10, 4);
        assert_eq!(
            view,
            CommandLogView {
                cursor: 0,
                follow: false
            }
        );
    }

    #[test]
    fn eviction_keeps_the_cursor_on_its_entry_and_stops_at_the_top() {
        let mut view = CommandLogView {
            cursor: 5,
            follow: false,
        };
        view.logged(CommandLog::CAP, 2);
        assert_eq!(view.cursor, 3);
        view.logged(CommandLog::CAP, 10);
        assert_eq!(view.cursor, 0);

        let mut following = CommandLogView::open(CommandLog::CAP);
        following.logged(CommandLog::CAP, 1);
        assert_eq!(following.cursor, CommandLog::CAP - 1);
    }
}
