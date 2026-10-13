//! The Custom commands feature's `update` handlers: starting a command on
//! the selection, asking its prompts, running it, cancelling it and
//! showing how it finished.

use super::command::{CommandFailure, OutputMode, Target};
use super::pending::PendingCommand;
use crate::effect::Effect;
use crate::git::diff::sanitize;
use crate::handoff::HandOff;
use crate::model::{AppState, Focus};
use crate::popup::{ErrorPopup, LineInput, Popup, PromptKind};
use crate::refresh::Selection;

/// Custom commands: prompts, run and cancel.
impl AppState {
    /// Start the user's command `index` on the current selection: ask its
    /// prompts first, if it has any.
    pub(crate) fn start_command(&mut self, index: usize) -> Vec<Effect> {
        let Some(command) = self.keymap.commands.get(index).cloned() else {
            return vec![];
        };
        let Some(target) = self.command_target() else {
            self.notice = Some("No file selected".into());
            return vec![];
        };
        if self.command_running {
            self.notice = Some("A command is running".into());
            return vec![];
        }
        // Say now what the selection lacks, not after the prompts.
        let blanks = vec![String::new(); command.prompts.len()];
        if let Err(reason) = command.expand(&target, &blanks) {
            self.notice = Some(reason.into());
            return vec![];
        }
        self.ask_or_run(PendingCommand {
            command,
            target,
            answers: Vec::new(),
        })
    }

    /// What a command in the focused panel runs on: the selected file, and
    /// in the Diff pane its line, in the blame the cursor's line and commit.
    fn command_target(&self) -> Option<Target> {
        match &self.focus {
            // Commands bind only in Files, the diff and the blame.
            Focus::Status
            | Focus::Branches
            | Focus::Commits
            | Focus::Stash
            | Focus::Conflict(_) => None,
            Focus::Files => {
                let entry = self.selected.and_then(|i| self.files.get(i))?;
                Some(Target {
                    path: entry.path.clone(),
                    line: None,
                    commit: None,
                })
            }
            Focus::Diff => {
                let view = self.hunk_view()?;
                Some(Target {
                    path: view.path.clone(),
                    line: view.editor_line(),
                    commit: None,
                })
            }
            Focus::Blame(view) => {
                let line = view.selected();
                Some(Target {
                    path: view.path.clone(),
                    line: line.and_then(|l| u32::try_from(l.line_no).ok()),
                    commit: line
                        .filter(|l| !l.oid.is_empty() && !l.is_uncommitted())
                        .map(|l| l.oid.clone()),
                })
            }
        }
    }

    /// Open the next unanswered prompt, or run the command once all are.
    fn ask_or_run(&mut self, pending: PendingCommand) -> Vec<Effect> {
        if pending.answers.len() < pending.command.prompts.len() {
            self.pending_command = Some(pending);
            self.popups.push(Popup::Prompt {
                kind: PromptKind::Command,
                input: LineInput::default(),
            });
            return vec![];
        }
        self.run_command(pending)
    }

    pub(crate) fn command_answered(&mut self, text: String) -> Vec<Effect> {
        let Some(mut pending) = self.pending_command.take() else {
            return vec![];
        };
        pending.answers.push(text);
        self.ask_or_run(pending)
    }

    /// Hand the terminal to the command, or run it in the background when
    /// its output is captured.
    fn run_command(&mut self, pending: PendingCommand) -> Vec<Effect> {
        let PendingCommand {
            command,
            target,
            answers,
        } = pending;
        let argv = match command.expand(&target, &answers) {
            Ok(argv) => argv,
            Err(reason) => {
                self.notice = Some(reason.into());
                return vec![];
            }
        };
        // A popup title: one line, safe to draw.
        let title = sanitize(command.description.as_bytes());
        match command.output {
            OutputMode::Terminal => vec![Effect::HandOff(HandOff::RunCommand { title, argv })],
            output => {
                self.command_running = true;
                self.raise_busy();
                vec![Effect::RunCommand {
                    title,
                    argv,
                    output,
                }]
            }
        }
    }

    /// Show what the command printed or why it failed, then reload: it
    /// may have changed the repository. Only a command that could not run
    /// points at the config file.
    pub(crate) fn command_finished(
        &mut self,
        title: &str,
        output: OutputMode,
        result: Result<String, CommandFailure>,
    ) -> Vec<Effect> {
        if output != OutputMode::Terminal && self.command_running {
            self.command_running = false;
            self.lower_busy();
        }
        match result {
            Err(CommandFailure::Launch(message)) => self.push_error(ErrorPopup::new(
                format!("{title} failed"),
                &message,
                "Check the command in your config file, then try again.",
            )),
            Err(CommandFailure::Exited(message)) => self.push_error(ErrorPopup::new(
                format!("{title} failed"),
                &message,
                "Press Esc to close",
            )),
            Err(CommandFailure::Cancelled) => self.notice = Some("Command cancelled".into()),
            Ok(text) if output == OutputMode::Popup => {
                let text = if text.is_empty() {
                    "(no output)"
                } else {
                    &text
                };
                self.push_error(ErrorPopup::new(title, text, "Press Esc to close"));
            }
            Ok(_) => {}
        }
        self.status_selection = Selection::KeepPath;
        self.refresh_status()
    }

    /// Stop the running captured command, if there is one.
    pub(crate) fn cancel_command(&mut self) -> Vec<Effect> {
        // The flag stays set until the command answers.
        if self.command_running {
            return vec![Effect::CancelCommand];
        }
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::super::command::{CommandTable, Panel, test_table};
    use super::*;
    #[cfg(unix)]
    use crate::git::FileEntry;
    #[cfg(unix)]
    use crate::git::path::RepoPath;
    use crate::intent::AppIntent;
    use crate::keymap::{KeyPress, Keymap};
    use crate::model::Context;
    use crate::test_support::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    /// `state` with these commands from the global file.
    fn with_commands(state: &mut AppState, tables: &[CommandTable]) {
        let (keymap, _) = Keymap::default()
            .with_commands(tables, &Default::default())
            .unwrap();
        let repo = state.repo.clone();
        state.update(AppIntent::ConfigLoaded {
            repo,
            keymap,
            errors: vec![],
        });
    }

    fn ctrl_t(state: &mut AppState) -> Vec<Effect> {
        state.update(AppIntent::Key(KeyPress::new(
            KeyCode::Char('t'),
            KeyModifiers::CONTROL,
        )))
    }

    fn files_command(command: &[&str], prompts: &[&str], output: OutputMode) -> CommandTable {
        test_table(Panel::Files, &["ctrl-t"], command, prompts, output)
    }

    fn os(items: &[&str]) -> Vec<std::ffi::OsString> {
        items.iter().map(std::ffi::OsString::from).collect()
    }

    /// A workspace whose one file has a hostile name, selected.
    fn hostile_file() -> AppState {
        let (state, _) = with_files(vec![changed("a b;$(rm -rf x).rs", '.', 'M')], 24);
        state
    }

    #[test]
    fn a_captured_command_runs_on_the_selected_file_as_one_argument() {
        let mut state = hostile_file();
        with_commands(
            &mut state,
            &[files_command(
                &["wc", "--", "{path}"],
                &[],
                OutputMode::None,
            )],
        );
        let busy = state.busy_count();

        let effects = ctrl_t(&mut state);

        assert_eq!(
            effects,
            [Effect::RunCommand {
                title: "Run wc".into(),
                argv: os(&["wc", "--", "a b;$(rm -rf x).rs"]),
                output: OutputMode::None,
            }]
        );
        assert!(state.command_running);
        assert_eq!(state.busy_count(), busy + 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_command_gets_the_raw_bytes_of_the_file_name() {
        use std::os::unix::ffi::OsStrExt;
        let raw = FileEntry {
            path: RepoPath::from_bytes(b"x\xff.rs"),
            ..changed("x", '.', 'M')
        };
        let (mut state, _) = with_files(vec![raw], 24);
        with_commands(
            &mut state,
            &[files_command(
                &["less", "{path}"],
                &[],
                OutputMode::Terminal,
            )],
        );

        match ctrl_t(&mut state).as_slice() {
            [Effect::HandOff(HandOff::RunCommand { title, argv })] => {
                assert_eq!(title, "Run less");
                assert_eq!(argv[1].as_bytes(), b"x\xff.rs");
            }
            other => panic!("expected a RunCommand hand-off, got {other:?}"),
        }
        // The terminal is handed over; nothing counts as running here.
        assert!(!state.command_running);
    }

    #[test]
    fn prompts_are_asked_in_order_then_the_command_runs_with_the_answers() {
        let mut state = hostile_file();
        with_commands(
            &mut state,
            &[files_command(
                &["cp", "{path}", "{prompt1}", "--mode={prompt2}"],
                &["Copy to", "Mode\x1b[2J"],
                OutputMode::Popup,
            )],
        );

        assert_eq!(ctrl_t(&mut state), []);
        assert!(matches!(
            state.popups.as_slice(),
            [Popup::Prompt {
                kind: PromptKind::Command,
                ..
            }]
        ));
        assert_eq!(state.prompt_title(PromptKind::Command), "Copy to");
        type_text(&mut state, "dir with space");
        assert_eq!(press(&mut state, KeyCode::Enter), []);
        assert_eq!(state.popups.len(), 1);
        let title = state.prompt_title(PromptKind::Command);
        assert_eq!(title, "Mode\u{FFFD}[2J");
        // A reload while the user types does not change what runs.
        with_commands(&mut state, &[]);

        let effects = state.update(AppIntent::SubmitPrompt);

        assert_eq!(
            effects,
            [Effect::RunCommand {
                title: "Run cp".into(),
                argv: os(&["cp", "a b;$(rm -rf x).rs", "dir with space", "--mode="]),
                output: OutputMode::Popup,
            }]
        );
        assert!(state.popups.is_empty());
        assert_eq!(state.pending_command, None);
    }

    #[test]
    fn answers_arrive_as_intents_in_order() {
        let mut state = hostile_file();
        with_commands(
            &mut state,
            &[files_command(
                &["echo", "{prompt2}", "{prompt1}"],
                &["One", "Two"],
                OutputMode::None,
            )],
        );
        ctrl_t(&mut state);
        state.popups.pop();
        assert_eq!(
            state.update(AppIntent::CommandPromptAnswered("1".into())),
            []
        );
        state.popups.pop();
        let effects = state.update(AppIntent::CommandPromptAnswered("2".into()));
        match effects.as_slice() {
            [Effect::RunCommand { argv, .. }] => assert_eq!(argv, &os(&["echo", "2", "1"])),
            other => panic!("expected RunCommand, got {other:?}"),
        }
    }

    #[test]
    fn cancelling_a_prompt_cancels_the_command() {
        let mut state = hostile_file();
        with_commands(
            &mut state,
            &[files_command(
                &["echo", "{prompt1}"],
                &["Q"],
                OutputMode::None,
            )],
        );
        ctrl_t(&mut state);
        assert!(state.pending_command.is_some());

        assert_eq!(state.update(AppIntent::PopPopup), []);

        assert_eq!(state.pending_command, None);
        assert!(state.popups.is_empty());
        assert!(!state.command_running);
        // A late answer has nothing to run.
        assert_eq!(
            state.update(AppIntent::CommandPromptAnswered("x".into())),
            []
        );
        // Esc does the same.
        ctrl_t(&mut state);
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.pending_command, None);
    }

    #[test]
    fn a_command_needs_a_selection_and_what_its_template_uses() {
        let mut state = AppState::new(Context::Workspace);
        with_commands(
            &mut state,
            &[files_command(&["echo", "{path}"], &["Q"], OutputMode::None)],
        );
        assert_eq!(ctrl_t(&mut state), []);
        assert_eq!(state.notice, Some("No file selected".into()));
        assert!(state.popups.is_empty());
    }

    #[test]
    fn a_blame_command_gets_the_line_and_commit_or_says_why_not() {
        let mut state = blamed(3, 24);
        let table = test_table(
            Panel::Blame,
            &["ctrl-t"],
            &["git", "show", "{commit}", "{path}:{line}"],
            &["Q"],
            OutputMode::Terminal,
        );
        with_commands(&mut state, &[table]);
        state.update(AppIntent::BlameNext);
        state.update(AppIntent::Key(KeyPress::new(
            KeyCode::Char('t'),
            KeyModifiers::CONTROL,
        )));
        type_text(&mut state, "x");
        let effects = press(&mut state, KeyCode::Enter);
        assert_eq!(
            effects,
            [Effect::HandOff(HandOff::RunCommand {
                title: "Run git".into(),
                argv: os(&["git", "show", OID, "a.rs:2"]),
            })]
        );

        // The last line is not committed: refused before any prompt.
        state.update(AppIntent::BlameNext);
        assert_eq!(ctrl_t(&mut state), []);
        assert_eq!(state.notice, Some("Not committed yet".into()));
        assert!(state.popups.is_empty());
    }

    #[test]
    fn only_one_captured_command_runs_at_a_time() {
        let mut state = hostile_file();
        with_commands(
            &mut state,
            &[files_command(&["sleep", "9"], &[], OutputMode::None)],
        );
        assert_eq!(ctrl_t(&mut state).len(), 1);
        let busy = state.busy_count();

        assert_eq!(ctrl_t(&mut state), []);

        assert_eq!(state.notice, Some("A command is running".into()));
        assert_eq!(state.busy_count(), busy);
        assert!(state.popups.is_empty());
    }

    fn command_finished(output: OutputMode, result: Result<&str, CommandFailure>) -> AppIntent {
        AppIntent::CommandFinished {
            title: "Show it".into(),
            output,
            result: result.map(String::from),
        }
    }

    fn exited(text: &str) -> CommandFailure {
        CommandFailure::Exited(text.into())
    }

    fn is_status_load(effects: &[Effect]) -> bool {
        matches!(effects, [Effect::LoadStatus { .. }])
    }

    #[test]
    fn captured_output_is_shown_safely_and_the_status_reloaded() {
        let mut state = hostile_file();
        with_commands(
            &mut state,
            &[files_command(&["sh", "x"], &[], OutputMode::Popup)],
        );
        ctrl_t(&mut state);
        let busy = state.busy_count();

        let effects = state.update(command_finished(
            OutputMode::Popup,
            Ok("red \x1b[31mtext\nline 2"),
        ));

        assert!(is_status_load(&effects), "{effects:?}");
        assert!(!state.command_running);
        // One down for the command, one up for the reload.
        assert_eq!(state.busy_count(), busy);
        match state.popups.as_slice() {
            [Popup::Error(popup)] => {
                assert_eq!(popup.title, "Show it");
                assert!(!popup.message.contains('\x1b'), "{popup:?}");
                assert!(popup.message.starts_with("red "), "{popup:?}");
                assert!(popup.message.ends_with("text\nline 2"), "{popup:?}");
            }
            other => panic!("expected the output popup, got {other:?}"),
        }
        // And another command may start.
        state.popups.clear();
        assert_eq!(ctrl_t(&mut state).len(), 1);
    }

    #[test]
    fn a_command_that_exited_non_zero_says_so_and_still_reloads() {
        let mut state = hostile_file();
        for output in [OutputMode::None, OutputMode::Popup, OutputMode::Terminal] {
            state.popups.clear();
            let effects = state.update(command_finished(
                output,
                Err(exited("exited with exit status: 3\nno")),
            ));
            assert!(is_status_load(&effects), "{effects:?}");
            match state.popups.as_slice() {
                [Popup::Error(popup)] => {
                    assert_eq!(popup.title, "Show it failed");
                    assert_eq!(popup.message, "exited with exit status: 3\nno");
                    // It ran as configured: the config is not to blame.
                    assert_eq!(popup.next, "Press Esc to close");
                }
                other => panic!("expected the failure popup, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_command_that_could_not_launch_points_at_the_config() {
        let mut state = hostile_file();
        let effects = state.update(command_finished(
            OutputMode::None,
            Err(CommandFailure::Launch(
                "Could not start nope: not found".into(),
            )),
        ));
        assert!(is_status_load(&effects), "{effects:?}");
        match state.popups.as_slice() {
            [Popup::Error(popup)] => {
                assert_eq!(popup.title, "Show it failed");
                assert_eq!(popup.message, "Could not start nope: not found");
                assert_eq!(
                    popup.next,
                    "Check the command in your config file, then try again."
                );
            }
            other => panic!("expected the failure popup, got {other:?}"),
        }
    }

    #[test]
    fn the_cancel_key_stops_only_a_running_command() {
        let mut state = hostile_file();
        assert_eq!(state.update(AppIntent::CancelCommand), []);

        with_commands(
            &mut state,
            &[files_command(&["sleep", "9"], &[], OutputMode::Popup)],
        );
        ctrl_t(&mut state);
        assert_eq!(
            state.update(AppIntent::CancelCommand),
            [Effect::CancelCommand]
        );
        // Still running until the command answers.
        assert!(state.command_running);
    }

    #[test]
    fn a_cancelled_command_says_so_without_a_popup_and_reloads() {
        let mut state = hostile_file();
        with_commands(
            &mut state,
            &[files_command(&["sleep", "9"], &[], OutputMode::Popup)],
        );
        ctrl_t(&mut state);
        let busy = state.busy_count();

        let effects = state.update(command_finished(
            OutputMode::Popup,
            Err(CommandFailure::Cancelled),
        ));

        assert!(is_status_load(&effects), "{effects:?}");
        assert!(!state.command_running);
        // One down for the command, one up for the reload.
        assert_eq!(state.busy_count(), busy);
        assert!(state.popups.is_empty(), "{:?}", state.popups);
        assert_eq!(state.notice, Some("Command cancelled".into()));
    }

    #[test]
    fn a_quiet_success_shows_nothing_but_reloads() {
        let mut state = hostile_file();
        for output in [OutputMode::None, OutputMode::Terminal] {
            let effects = state.update(command_finished(output, Ok("")));
            assert!(is_status_load(&effects), "{effects:?}");
            assert!(state.popups.is_empty());
        }
    }
}
