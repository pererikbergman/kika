//! The git commands Kika ran this session, oldest first, held in memory
//! and capped so a long session cannot grow it without limit. Pure.

use std::collections::VecDeque;
use std::time::Duration;

use crate::list_filter;
use kika_git::recording::{CommandEvent, CommandOutcome};

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

/// Wide enough for `not started`, so the times line up.
const STATE_WIDTH: usize = 11;
/// Wide enough for `999ms` and `99.9s`.
const ELAPSED_WIDTH: usize = 5;

/// ` <state> <elapsed> [terminal] <line>`, the columns padded to line up.
/// The popup draws this and its filter matches it, so a filter only ever
/// sees what is on screen: never argv, env or an unredacted secret.
pub(crate) fn row_text(entry: &LogEntry) -> String {
    let marker = if entry.attached { "[terminal] " } else { "" };
    let elapsed = entry.elapsed.map(elapsed).unwrap_or_default();
    format!(
        " {:<STATE_WIDTH$} {elapsed:>ELAPSED_WIDTH$} {marker}{}",
        state(entry),
        entry.line
    )
}

/// How the command stands, in a word or two.
pub(crate) fn state(entry: &LogEntry) -> String {
    match &entry.outcome {
        None => "running".into(),
        Some(CommandOutcome::Ok) => "ok".into(),
        Some(CommandOutcome::Failed(None)) => "failed".into(),
        Some(CommandOutcome::Failed(Some(exit))) => format!("failed {exit}"),
        Some(CommandOutcome::NotStarted) => "not started".into(),
        Some(CommandOutcome::Stopped) => "stopped".into(),
    }
}

/// `12ms` under a second, `1.4s` above.
pub(crate) fn elapsed(time: Duration) -> String {
    if time < Duration::from_secs(1) {
        format!("{}ms", time.as_millis())
    } else {
        format!("{:.1}s", time.as_secs_f64())
    }
}

/// The Command log popup's cursor and filter. While `follow` is set the
/// cursor stays on the newest shown entry as new ones arrive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandLogView {
    pub cursor: usize,
    pub follow: bool,
    /// Narrows the rows shown; only ever matched against [`row_text`].
    pub filter: String,
}

impl CommandLogView {
    /// On the newest of `len` entries, following, unfiltered.
    pub fn open(len: usize) -> Self {
        Self {
            cursor: len.saturating_sub(1),
            follow: true,
            filter: String::new(),
        }
    }

    /// The indexes of the entries the filter shows, in log order.
    pub fn visible(&self, log: &CommandLog) -> Vec<usize> {
        if self.filter.is_empty() {
            return (0..log.len()).collect();
        }
        let texts: Vec<String> = log.entries().iter().map(row_text).collect();
        list_filter::visible(&self.filter, texts.iter().map(String::as_str))
    }

    /// The filter hides the cursor's entry.
    pub fn selected_hidden(&self, log: &CommandLog) -> bool {
        !self.filter.is_empty()
            && log
                .entries()
                .get(self.cursor)
                .is_some_and(|e| !list_filter::matches(&self.filter, &row_text(e)))
    }

    /// Put a cursor the filter hides on the first entry shown; leave it be
    /// when none is. Following stays on exactly when the cursor is on the
    /// newest shown entry, so a moved cursor does not jump on the next
    /// command.
    pub fn reconcile(&mut self, log: &CommandLog) {
        let visible = self.visible(log);
        let Some(&last) = visible.last() else {
            return;
        };
        if self.selected_hidden(log) {
            self.cursor = visible[0];
        }
        self.follow = self.cursor == last;
    }

    /// The log changed, after `evicted` entries were dropped from the
    /// front. The cursor stays on its entry, or follows the newest shown.
    /// A finished command reads differently, so it may start or stop
    /// matching; a cursor it hides moves to a shown one.
    pub fn logged(&mut self, log: &CommandLog, evicted: usize) {
        if self.follow {
            self.cursor = match self.visible(log).last() {
                Some(&last) => last,
                None => log.len().saturating_sub(1),
            };
        } else {
            self.cursor = self.cursor.saturating_sub(evicted);
            self.reconcile(log);
        }
    }

    /// Move by `delta` shown rows, stopping at the ends. Following is on
    /// exactly when the cursor lands on the newest shown entry. With
    /// nothing shown nothing changes.
    pub fn move_by(&mut self, delta: isize, log: &CommandLog) {
        let visible = self.visible(log);
        if let Some(cursor) = list_filter::step(&visible, self.cursor, delta) {
            self.cursor = cursor;
            self.follow = visible.last() == Some(&cursor);
        }
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

    /// A log of `n` running commands, `git cmd<id>`.
    fn log_of(n: u64) -> CommandLog {
        let mut log = CommandLog::default();
        for id in 0..n {
            log.apply(started(id));
        }
        log
    }

    fn view(cursor: usize, follow: bool, filter: &str) -> CommandLogView {
        CommandLogView {
            cursor,
            follow,
            filter: filter.into(),
        }
    }

    #[test]
    fn the_cursor_follows_the_newest_entry_until_moved_up() {
        let mut log = CommandLog::default();
        let mut v = CommandLogView::open(0);
        assert_eq!(v, view(0, true, ""));
        for id in 0..3 {
            log.apply(started(id));
        }
        v.logged(&log, 0);
        assert_eq!(v.cursor, 2);

        v.move_by(-1, &log);
        assert_eq!(v, view(1, false, ""));
        log.apply(started(3));
        v.logged(&log, 0);
        assert_eq!(v.cursor, 1);

        // Back onto the newest entry follows again.
        v.move_by(5, &log);
        assert_eq!(v, view(3, true, ""));
        v.move_by(-10, &log);
        assert_eq!(v, view(0, false, ""));
    }

    #[test]
    fn eviction_keeps_the_cursor_on_its_entry_and_stops_at_the_top() {
        let mut log = log_of(CommandLog::CAP as u64);
        let mut v = view(5, false, "");
        let evicted = log.apply(started(1000)) + log.apply(started(1001));
        v.logged(&log, evicted);
        assert_eq!(v.cursor, 3);
        assert_eq!(log.entries()[v.cursor].id, 5);
        let evicted: usize = (0..10).map(|i| log.apply(started(2000 + i))).sum();
        v.logged(&log, evicted);
        assert_eq!(v.cursor, 0);

        let mut following = CommandLogView::open(CommandLog::CAP);
        let evicted = log.apply(started(3000));
        following.logged(&log, evicted);
        assert_eq!(following.cursor, CommandLog::CAP - 1);
    }

    #[test]
    fn eviction_under_a_filter_keeps_the_cursor_on_its_entry() {
        let mut log = log_of(CommandLog::CAP as u64);
        let mut v = view(7, false, "cmd7");
        let evicted = log.apply(started(1000)) + log.apply(started(1001));
        v.logged(&log, evicted);
        assert_eq!(log.entries()[v.cursor].id, 7);
    }

    #[test]
    fn an_empty_filter_shows_every_entry() {
        let log = log_of(4);
        assert_eq!(view(0, true, "").visible(&log), [0, 1, 2, 3]);
        assert!(!view(0, true, "").selected_hidden(&log));
        assert_eq!(
            view(0, true, "").visible(&CommandLog::default()),
            Vec::<usize>::new()
        );
    }

    #[test]
    fn the_filter_sees_the_redacted_line_never_the_secret() {
        use kika_git::runner::GitCommand;

        let add = GitCommand::new("remote")
            .arg("add")
            .arg("--")
            .arg("o")
            .arg("https://u:tok@h/r");
        let mut log = CommandLog::default();
        log.started(0, kika_git::command_line::display_line(&add), false);
        assert_eq!(view(0, true, "tok").visible(&log), Vec::<usize>::new());
        assert_eq!(view(0, true, "u:tok@").visible(&log), Vec::<usize>::new());
        assert_eq!(view(0, true, "h/r").visible(&log), [0]);
    }

    #[test]
    fn the_state_time_and_terminal_marker_match_too() {
        let mut log = log_of(4);
        log.apply(finished(1, CommandOutcome::Failed(Some("status 1".into()))));
        log.apply(finished(2, CommandOutcome::Ok));
        log.started(9, "git commit".into(), true);
        assert_eq!(view(0, true, "failed").visible(&log), [1]);
        assert_eq!(view(0, true, "running").visible(&log), [0, 3, 4]);
        assert_eq!(view(0, true, "2ms").visible(&log), [2]);
        assert_eq!(view(0, true, "[terminal]").visible(&log), [4]);
    }

    #[test]
    fn smart_case_and_every_term_apply() {
        let mut log = CommandLog::default();
        log.started(0, "git Fetch origin".into(), false);
        log.started(1, "git fetch --all".into(), false);
        log.started(2, "git push origin".into(), false);
        assert_eq!(view(0, true, "fetch").visible(&log), [0, 1]);
        assert_eq!(view(0, true, "Fetch").visible(&log), [0]);
        assert_eq!(view(0, true, "fetch origin").visible(&log), [0]);
        assert_eq!(
            view(0, true, "origin all").visible(&log),
            Vec::<usize>::new()
        );
        assert_eq!(view(0, true, "origin git").visible(&log), [0, 2]);
    }

    #[test]
    fn moving_skips_hidden_rows_and_stops_at_the_ends() {
        let log = log_of(12);
        // `cmd1`, `cmd10`, `cmd11`.
        let mut v = view(1, false, "cmd1");
        assert_eq!(v.visible(&log), [1, 10, 11]);
        v.move_by(1, &log);
        assert_eq!(v, view(10, false, "cmd1"));
        v.move_by(5, &log);
        assert_eq!(v, view(11, true, "cmd1"), "the last shown row follows");
        v.move_by(-10, &log);
        assert_eq!(v, view(1, false, "cmd1"));

        // Nothing shown: nothing moves.
        let mut none = view(4, false, "zzz");
        none.move_by(1, &log);
        assert_eq!(none, view(4, false, "zzz"));
    }

    #[test]
    fn following_under_a_filter_moves_only_to_matching_entries() {
        let mut log = log_of(3);
        let mut v = view(2, true, "cmd");
        log.apply(started(3));
        v.logged(&log, 0);
        assert_eq!(v.cursor, 3, "a matching entry takes the cursor");

        let mut v = view(3, true, "3");
        log.started(4, "git status".into(), false);
        v.logged(&log, 0);
        assert_eq!(v.cursor, 3, "a non-matching entry leaves it");
        assert!(v.follow);

        // Nothing shown yet: on the newest entry, as unfiltered.
        let mut empty = view(0, true, "zzz");
        empty.logged(&log, 0);
        assert_eq!(empty.cursor, 4);
    }

    #[test]
    fn reconcile_moves_a_hidden_cursor_to_the_first_shown_row() {
        let log = log_of(12);
        let mut v = view(5, false, "cmd1");
        assert!(v.selected_hidden(&log));
        v.reconcile(&log);
        assert_eq!(v.cursor, 1);
        assert!(!v.selected_hidden(&log));

        // A shown cursor stays; with nothing shown it stays too.
        let mut shown = view(10, false, "cmd1");
        shown.reconcile(&log);
        assert_eq!(shown.cursor, 10);
        let mut none = view(5, false, "zzz");
        none.reconcile(&log);
        assert_eq!(none.cursor, 5);
    }

    #[test]
    fn reconcile_follows_only_on_the_newest_shown_row() {
        let log = log_of(12);
        let mut v = CommandLogView::open(log.len());
        v.filter = "cmd1".into();
        v.reconcile(&log);
        assert_eq!(v, view(11, true, "cmd1"), "the newest still shows");

        v.filter = "cmd1 0".into();
        v.reconcile(&log);
        assert_eq!(
            v,
            view(10, true, "cmd1 0"),
            "the only row is the newest shown"
        );

        v.filter = "cmd".into();
        v.reconcile(&log);
        assert_eq!(v, view(10, false, "cmd"), "newer rows show again");
    }

    #[test]
    fn a_filter_moving_the_followed_cursor_up_leaves_it_on_new_commands() {
        let mut log = CommandLog::default();
        for (id, line) in ["git fetch a", "git fetch b", "git status"]
            .iter()
            .enumerate()
        {
            log.started(id as u64, (*line).into(), false);
        }
        let mut v = CommandLogView::open(log.len());
        v.filter = "fetch".into();
        v.reconcile(&log);
        assert_eq!(v, view(0, false, "fetch"));
        log.started(3, "git push".into(), false);
        v.logged(&log, 0);
        assert_eq!(v.cursor, 0, "a non-matching command leaves it");
    }

    #[test]
    fn a_finished_command_that_stops_matching_moves_the_cursor() {
        let mut log = log_of(3);
        let mut v = view(1, false, "running");
        log.apply(finished(1, CommandOutcome::Ok));
        v.logged(&log, 0);
        assert_eq!(v.cursor, 0);
    }

    #[test]
    fn times_read_in_milliseconds_then_seconds() {
        assert_eq!(elapsed(Duration::from_micros(400)), "0ms");
        assert_eq!(elapsed(Duration::from_millis(12)), "12ms");
        assert_eq!(elapsed(Duration::from_millis(999)), "999ms");
        assert_eq!(elapsed(Duration::from_millis(1400)), "1.4s");
        assert_eq!(elapsed(Duration::from_secs(75)), "75.0s");
    }

    #[test]
    fn every_outcome_has_a_state_word() {
        let entry = |outcome| LogEntry {
            id: 0,
            line: "git x".into(),
            attached: false,
            outcome,
            elapsed: None,
        };
        let cases = [
            (None, "running"),
            (Some(CommandOutcome::Ok), "ok"),
            (Some(CommandOutcome::Failed(None)), "failed"),
            (
                Some(CommandOutcome::Failed(Some("signal 2".into()))),
                "failed signal 2",
            ),
            (Some(CommandOutcome::NotStarted), "not started"),
            (Some(CommandOutcome::Stopped), "stopped"),
        ];
        for (outcome, word) in cases {
            assert_eq!(state(&entry(outcome)), word);
        }
    }
}
