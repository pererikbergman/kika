//! The Command log's `update`, which core's one `AppIntent::CommandLog` arm
//! calls, and its handlers: recording each runner event and opening and
//! moving its popup.

use super::{CommandLogIntent, CommandLogView};
use crate::effect::Effect;
use crate::git::recording::CommandEvent;
use crate::model::AppState;
use crate::popup::Popup;

/// The feature's half of `update`: core's one `AppIntent::CommandLog` arm
/// lands here. A free function, so its name cannot clash with handlers.
pub(crate) fn update(state: &mut AppState, intent: CommandLogIntent) -> Vec<Effect> {
    match intent {
        CommandLogIntent::Logged(event) => state.command_logged(event),
        CommandLogIntent::Open => state.open_command_log(),
        CommandLogIntent::Next => state.move_command_log(1),
        CommandLogIntent::Prev => state.move_command_log(-1),
        CommandLogIntent::PageDown => state.move_command_log(state.command_log_page()),
        CommandLogIntent::PageUp => state.move_command_log(-state.command_log_page()),
    }
    Vec::new()
}

/// The Command log: recording every git command, and its popup.
impl AppState {
    /// Record a runner's event, keeping an open log's cursor on its entry
    /// or on the newest one.
    pub(crate) fn command_logged(&mut self, event: CommandEvent) {
        let evicted = self.command_log.apply(event);
        let log = &self.command_log;
        for popup in &mut self.popups {
            if let Popup::CommandLog(view) = popup {
                view.logged(log, evicted);
            }
        }
    }

    pub(crate) fn open_command_log(&mut self) {
        let view = CommandLogView::open(self.command_log.len());
        self.popups.push(Popup::CommandLog(view));
    }

    pub(crate) fn move_command_log(&mut self, delta: isize) {
        if let Some(Popup::CommandLog(view)) = self.popups.last_mut() {
            view.move_by(delta, &self.command_log);
        }
    }

    /// Rows a page moves: the log fills the screen above the bottom bar,
    /// inside its border.
    pub(crate) fn command_log_page(&self) -> isize {
        isize::try_from(self.size.1.saturating_sub(3))
            .unwrap_or(isize::MAX)
            .max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::command_log::CommandLog;
    use crate::intent::AppIntent;
    use crate::keymap::{KeyPress, KeyScope};
    use crate::list_filter::{self, InputTarget};
    use crate::menu;
    use crate::model::Context;
    use crate::test_support::{press, press_char, type_text};
    use crossterm::event::{KeyCode, KeyModifiers};

    fn log_started(state: &mut AppState, id: u64) {
        state.update(AppIntent::CommandLog(CommandLogIntent::Logged(
            crate::git::recording::CommandEvent::Started {
                id,
                line: format!("git cmd{id}"),
                attached: false,
            },
        )));
    }

    fn command_log_view(state: &AppState) -> CommandLogView {
        match state.popups.last() {
            Some(Popup::CommandLog(view)) => view.clone(),
            other => panic!("expected the command log on top, got {other:?}"),
        }
    }

    #[test]
    fn the_command_log_records_in_order_and_stays_bounded() {
        use crate::git::recording::{CommandEvent, CommandOutcome};

        let mut state = AppState::new(Context::Workspace);
        for id in 0..3 {
            log_started(&mut state, id);
        }
        let effects = state.update(AppIntent::CommandLog(CommandLogIntent::Logged(
            CommandEvent::Finished {
                id: 1,
                outcome: CommandOutcome::Failed(None),
                elapsed: std::time::Duration::from_millis(3),
            },
        )));
        assert_eq!(effects, []);
        let lines: Vec<_> = state
            .command_log
            .entries()
            .iter()
            .map(|e| &e.line)
            .collect();
        assert_eq!(lines, ["git cmd0", "git cmd1", "git cmd2"]);
        let outcomes: Vec<_> = state
            .command_log
            .entries()
            .iter()
            .map(|e| e.outcome.clone())
            .collect();
        assert_eq!(outcomes, [None, Some(CommandOutcome::Failed(None)), None]);

        let total = CommandLog::CAP as u64 + 10;
        for id in 3..total {
            log_started(&mut state, id);
        }
        assert_eq!(state.command_log.len(), CommandLog::CAP);
        assert_eq!(state.command_log.entries()[0].id, 10);
        let before = state.command_log.clone();
        state.update(AppIntent::CommandLog(CommandLogIntent::Logged(
            CommandEvent::Finished {
                id: 1,
                outcome: CommandOutcome::Ok,
                elapsed: std::time::Duration::ZERO,
            },
        )));
        assert_eq!(state.command_log, before);
    }

    #[test]
    fn the_command_log_popup_follows_new_commands_until_moved_up() {
        let mut state = AppState::new(Context::Workspace);
        state.size = (80, 13);
        for id in 0..3 {
            log_started(&mut state, id);
        }
        assert_eq!(
            state.update(AppIntent::CommandLog(CommandLogIntent::Open)),
            []
        );
        assert_eq!(state.key_focus(), KeyScope::CommandLog);
        assert_eq!(
            command_log_view(&state),
            CommandLogView {
                cursor: 2,
                follow: true,
                filter: String::new()
            }
        );

        log_started(&mut state, 3);
        assert_eq!(command_log_view(&state).cursor, 3);

        state.update(AppIntent::CommandLog(CommandLogIntent::Prev));
        assert_eq!(
            command_log_view(&state),
            CommandLogView {
                cursor: 2,
                follow: false,
                filter: String::new()
            }
        );
        log_started(&mut state, 4);
        assert_eq!(command_log_view(&state).cursor, 2);

        state.update(AppIntent::CommandLog(CommandLogIntent::Next));
        state.update(AppIntent::CommandLog(CommandLogIntent::Next));
        assert_eq!(
            command_log_view(&state),
            CommandLogView {
                cursor: 4,
                follow: true,
                filter: String::new()
            }
        );

        // A page is the rows inside the popup: 13 - bar - border = 10.
        for id in 5..30 {
            log_started(&mut state, id);
        }
        state.update(AppIntent::CommandLog(CommandLogIntent::PageUp));
        assert_eq!(command_log_view(&state).cursor, 19);
        state.update(AppIntent::CommandLog(CommandLogIntent::PageDown));
        assert_eq!(command_log_view(&state).cursor, 29);

        state.update(AppIntent::PopPopup);
        assert_eq!(state.popups, []);
        // Recording carries on while closed.
        log_started(&mut state, 30);
        assert_eq!(state.command_log.len(), 31);
    }

    /// The Command log open on `git cmd0`..`git cmd11`, all running.
    fn open_log_of_twelve() -> AppState {
        let mut state = AppState::new(Context::Workspace);
        state.size = (80, 24);
        for id in 0..12 {
            log_started(&mut state, id);
        }
        state.update(AppIntent::CommandLog(CommandLogIntent::Open));
        state
    }

    fn command_log_rows(state: &AppState) -> Vec<usize> {
        command_log_view(state).visible(&state.command_log)
    }

    #[test]
    fn slash_in_the_command_log_filters_it_as_typed() {
        let mut state = open_log_of_twelve();
        assert_eq!(press_char(&mut state, '/'), vec![]);
        assert_eq!(state.key_focus(), KeyScope::Input);
        let input = state.input.as_ref().expect("the input is open");
        assert_eq!(input.target, InputTarget::CommandLogFilter);
        assert_eq!(input.target.prompt(), "/");
        type_text(&mut state, "cmd1");
        assert_eq!(command_log_view(&state).filter, "cmd1");
        assert_eq!(command_log_rows(&state), [1, 10, 11]);
        type_text(&mut state, "0");
        assert_eq!(command_log_rows(&state), [10]);
        assert_eq!(command_log_view(&state).cursor, 10, "moved to a shown row");

        // `q` and `?` are typed, not run.
        type_text(&mut state, "q?");
        assert_eq!(command_log_view(&state).filter, "cmd10q?");
        assert_eq!(state.key_focus(), KeyScope::Input);
        assert!(matches!(state.popups.as_slice(), [Popup::CommandLog(_)]));
    }

    #[test]
    fn the_command_log_filter_stops_at_its_limit() {
        let mut state = open_log_of_twelve();
        press_char(&mut state, '/');
        for _ in 0..300 {
            press_char(&mut state, 'z');
        }
        assert_eq!(
            command_log_view(&state).filter.chars().count(),
            list_filter::MAX_INPUT
        );
        assert_eq!(state.notice, Some(list_filter::input_full_notice().into()));
    }

    #[test]
    fn enter_keeps_the_command_log_filter_and_esc_in_the_input_clears_it() {
        let mut state = open_log_of_twelve();
        press_char(&mut state, '/');
        type_text(&mut state, "cmd1");
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.input, None);
        assert_eq!(state.key_focus(), KeyScope::CommandLog);
        assert_eq!(command_log_rows(&state), [1, 10, 11]);

        // `/` again starts from the kept text; Esc there clears it.
        press_char(&mut state, '/');
        assert_eq!(state.input.as_ref().unwrap().line.text, "cmd1");
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.input, None);
        assert_eq!(command_log_view(&state).filter, "");
        assert_eq!(state.key_focus(), KeyScope::CommandLog);
    }

    #[test]
    fn esc_in_the_command_log_clears_the_filter_then_closes_it() {
        let mut state = open_log_of_twelve();
        press_char(&mut state, '/');
        type_text(&mut state, "cmd1");
        press(&mut state, KeyCode::Enter);
        assert_eq!(press(&mut state, KeyCode::Esc), vec![]);
        assert_eq!(command_log_view(&state).filter, "");
        assert_eq!(state.key_focus(), KeyScope::CommandLog, "still open");
        assert_eq!(press(&mut state, KeyCode::Esc), vec![]);
        assert!(state.popups.is_empty());
        assert!(!state.should_quit);

        // `q` closes at once, filter or not.
        let mut state = open_log_of_twelve();
        press_char(&mut state, '/');
        type_text(&mut state, "cmd1");
        press(&mut state, KeyCode::Enter);
        press_char(&mut state, 'q');
        assert!(state.popups.is_empty());
        assert!(!state.should_quit);

        // Opened again, the log starts unfiltered.
        state.update(AppIntent::CommandLog(CommandLogIntent::Open));
        assert_eq!(command_log_view(&state).filter, "");
        assert_eq!(command_log_rows(&state).len(), 12);
    }

    #[test]
    fn moving_in_a_filtered_command_log_skips_hidden_rows() {
        let mut state = open_log_of_twelve();
        press_char(&mut state, '/');
        type_text(&mut state, "cmd1");
        press(&mut state, KeyCode::Enter);
        assert_eq!(command_log_view(&state).cursor, 11, "still on the newest");
        press_char(&mut state, 'k');
        assert_eq!(command_log_view(&state).cursor, 10);
        press_char(&mut state, 'k');
        assert_eq!(command_log_view(&state).cursor, 1);
        assert!(!command_log_view(&state).follow);
        press(&mut state, KeyCode::PageDown);
        assert_eq!(command_log_view(&state).cursor, 11);
        assert!(command_log_view(&state).follow);

        // Following, a new matching command takes the cursor; others not.
        log_started(&mut state, 20);
        assert_eq!(command_log_view(&state).cursor, 11);
        log_started(&mut state, 21);
        assert_eq!(command_log_view(&state).cursor, 13);
    }

    #[test]
    fn the_command_log_filter_never_reaches_an_effect() {
        let mut state = open_log_of_twelve();
        let filter = "--force \x1b[31m";
        let mut effects = press_char(&mut state, '/');
        for c in filter.chars() {
            effects.extend(press(&mut state, KeyCode::Char(c)));
        }
        effects.extend(press(&mut state, KeyCode::Enter));
        // The input drops the control char; the rest is kept.
        assert_eq!(command_log_view(&state).filter, "--force [31m");
        for code in [
            KeyCode::Char('j'),
            KeyCode::Char('k'),
            KeyCode::Down,
            KeyCode::Up,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Char('q'),
        ] {
            effects.extend(press(&mut state, code));
        }
        assert!(state.popups.is_empty());
        let shown = format!("{effects:?}");
        for part in [filter, "--force [31m", "--force", "force"] {
            assert!(!shown.contains(part), "{part:?} in {shown}");
        }
    }

    #[test]
    fn at_and_the_menu_open_the_command_log() {
        let mut state = AppState::new(Context::Workspace);
        state.update(AppIntent::Key(KeyPress::new(
            KeyCode::Char('@'),
            KeyModifiers::SHIFT,
        )));
        assert!(matches!(state.popups.as_slice(), [Popup::CommandLog(_)]));
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.popups, []);

        let items = menu::items(menu::MenuId::Actions);
        let at = items
            .iter()
            .position(|item| item.label == "Command log…")
            .expect("the Actions menu has the command log");
        assert_eq!(items[at - 1].label, "Worktrees…");
        assert_eq!(
            items[at].intent,
            AppIntent::CommandLog(CommandLogIntent::Open)
        );
        assert_eq!((items[at].disabled)(&state), None);
    }

    #[test]
    fn the_command_log_menu_item_opens_the_log_through_update() {
        let mut state = AppState::new(Context::Workspace);
        let item = menu::items(menu::MenuId::Actions)
            .iter()
            .find(|item| item.label == "Command log…")
            .expect("the Actions menu has the command log");
        assert_eq!(state.update(item.intent.clone()), []);
        assert!(matches!(state.popups.last(), Some(Popup::CommandLog(_))));
    }
}
