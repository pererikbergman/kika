//! Keys held while a user status reload is in flight. A key that acts on
//! the selected file, pressed before the reload lands, would act on a row
//! the reloaded list may no longer have. So keys pressed in the Files
//! panel or the Diff pane wait for the load and replay in order on what
//! it shows.
//!
//! Only reloads the user waits on hold keys (`refresh_status`). A
//! background load does not, or every file-system event would add lag to
//! every key. Keys typed into an input, prompt, popup, help or the commit
//! editor are never held: `key_focus` names another scope for them.
//!
//! [`KeyHold`]'s fields are private to this file, so the hold is started,
//! released and dropped only through the methods here.

use std::collections::VecDeque;

use crate::effect::Effect;
use crate::intent::AppIntent;
use crate::keymap::{KeyPress, KeyScope};
use crate::model::AppState;

/// How many keys wait for one reload. A reload that takes long enough to
/// fill this is stuck, and replaying a long burst later would surprise.
const MAX_HELD: usize = 16;

/// Said when a key is dropped because the queue is full.
const KEY_DROPPED: &str = "Still reloading; key dropped";

/// Said when a failed reload or a repository change drops held keys.
const KEYS_DROPPED: &str = "Keys pressed while reloading were dropped";

/// The keys waiting on a status reload, held by `AppState`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct KeyHold {
    /// The status load keys wait on; `None` when none does.
    seq: Option<u64>,
    /// Keys pressed while it was in flight, oldest first.
    keys: VecDeque<KeyPress>,
}

impl KeyHold {
    /// Keys from now on wait for the status load under `seq`.
    pub(crate) fn wait_for(&mut self, seq: u64) {
        self.seq = Some(seq);
    }

    /// Whether keys are queued.
    pub(crate) fn has_keys(&self) -> bool {
        !self.keys.is_empty()
    }
}

impl AppState {
    /// Whether a key pressed now waits for the status reload: one the user
    /// started is still in flight, and the key would go to Files or the
    /// Diff pane.
    pub(crate) fn holds_keys(&self) -> bool {
        self.key_hold.seq.is_some()
            && self.files.seq == self.key_hold.seq
            && matches!(
                self.key_focus(),
                KeyScope::Files | KeyScope::Diff | KeyScope::Lines
            )
    }

    /// Queue `key` for the reload; past [`MAX_HELD`] it is dropped.
    pub(crate) fn hold_key(&mut self, key: KeyPress) {
        if self.key_hold.keys.len() >= MAX_HELD {
            self.notice = Some(KEY_DROPPED.into());
            return;
        }
        self.key_hold.keys.push_back(key);
    }

    /// The reload landed: replay the held keys in order on what it shows.
    /// A replayed key that starts another reload holds the rest for it.
    pub(crate) fn release_keys(&mut self) -> Vec<Effect> {
        self.key_hold.seq = None;
        let mut effects = Vec::new();
        while self.key_hold.seq.is_none()
            && let Some(key) = self.key_hold.keys.pop_front()
        {
            // A notice the reload set stays unless the key sets its own.
            let before = self.notice.clone();
            effects.extend(self.update(AppIntent::Key(key)));
            if self.notice.is_none() {
                self.notice = before;
            }
        }
        effects
    }

    /// Forget the hold and its keys, saying so if any were waiting.
    pub(crate) fn drop_held_keys(&mut self) {
        self.key_hold.seq = None;
        if !self.key_hold.keys.is_empty() {
            self.key_hold.keys.clear();
            self.notice = Some(KEYS_DROPPED.into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::Tag;
    use crate::features::custom_commands::{OutputMode, Panel, test_table};
    use crate::handoff::HandOff;
    use crate::keymap::Keymap;
    use crate::op::GitOp;
    use crate::popup::{ErrorPopup, Popup};
    use crate::test_support::*;
    use crate::watch::WatchEvent;
    use crossterm::event::KeyCode;
    use kika_git::FileEntry;

    /// Files lists `files` with the first selected, and a captured
    /// command on ctrl-t that echoes the selected path.
    fn with_echo(files: Vec<FileEntry>) -> AppState {
        let (mut state, _) = with_files(files, 24);
        let table = test_table(
            Panel::Files,
            &["ctrl-t"],
            &["echo", "path: {path}"],
            &[],
            OutputMode::None,
        );
        let (keymap, _) = Keymap::default()
            .with_commands(&[table], &Default::default())
            .unwrap();
        let repo = state.repo.clone();
        state.update(AppIntent::ConfigLoaded {
            repo,
            keymap,
            errors: vec![],
        });
        state
    }

    fn ctrl_t(state: &mut AppState) -> Vec<Effect> {
        ctrl(state, 't')
    }

    fn space(state: &mut AppState) -> Vec<Effect> {
        press_char(state, ' ')
    }

    fn landed(state: &mut AppState, tag: Tag, files: Vec<FileEntry>) -> Vec<Effect> {
        state.update(AppIntent::StatusLoaded {
            tag,
            result: Ok(repo(files)),
        })
    }

    fn commands(effects: &[Effect]) -> Vec<&Effect> {
        effects
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    Effect::RunCommand { .. } | Effect::HandOff(HandOff::RunCommand { .. })
                )
            })
            .collect()
    }

    fn stages(effects: &[Effect]) -> Vec<GitOp> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::RunOp { op, .. } if matches!(op, GitOp::Stage(_)) => Some(op.clone()),
                _ => None,
            })
            .collect()
    }

    /// Three files with unstaged changes, so space stages any of them.
    fn unstaged() -> Vec<FileEntry> {
        ["a.rs", "b.rs", "c.rs"]
            .map(|path| changed(path, '.', 'M'))
            .to_vec()
    }

    fn app_txt() -> FileEntry {
        changed("src/app.txt", '.', 'M')
    }

    #[test]
    fn a_command_pressed_during_a_reload_never_runs_on_a_file_it_dropped() {
        let mut state = with_echo(vec![app_txt()]);
        let tag = refresh_tag(&mut state);

        assert!(commands(&ctrl_t(&mut state)).is_empty(), "held");
        let effects = landed(&mut state, tag, vec![]);

        assert!(commands(&effects).is_empty(), "{effects:?}");
        assert_eq!(state.notice, Some("No file selected".into()));
    }

    #[test]
    fn a_command_pressed_during_a_reload_runs_once_on_the_file_still_listed() {
        let mut state = with_echo(vec![app_txt()]);
        let tag = refresh_tag(&mut state);

        assert!(commands(&ctrl_t(&mut state)).is_empty(), "held");
        let effects = landed(&mut state, tag, vec![app_txt()]);

        assert_eq!(
            commands(&effects),
            [&Effect::RunCommand {
                title: "Run echo".into(),
                argv: vec!["echo".into(), "path: src/app.txt".into()],
                output: OutputMode::None,
            }]
        );
        assert!(
            commands(&ctrl_t(&mut state)).is_empty(),
            "replayed only once"
        );
    }

    #[test]
    fn stage_pressed_during_a_reload_stages_what_the_reloaded_list_shows() {
        // The file went: nothing to stage.
        let (mut state, _) = with_files(vec![app_txt()], 24);
        let tag = refresh_tag(&mut state);
        assert!(stages(&space(&mut state)).is_empty(), "held");
        let effects = landed(&mut state, tag, vec![]);
        assert!(stages(&effects).is_empty(), "{effects:?}");

        // The last file went and the cursor clamped onto the one above.
        let (mut state, _) = with_files(unstaged(), 24);
        select_path(&mut state, "c.rs");
        let tag = refresh_tag(&mut state);
        assert!(stages(&space(&mut state)).is_empty(), "held");
        let effects = landed(&mut state, tag, unstaged()[..2].to_vec());
        assert_eq!(stages(&effects), [GitOp::Stage(vec!["b.rs".into()])]);
    }

    #[test]
    fn held_keys_replay_in_order() {
        let (mut state, _) = with_files(unstaged(), 24);
        let tag = refresh_tag(&mut state);

        press_char(&mut state, 'j');
        space(&mut state);
        assert_eq!(state.files.selected, 0, "nothing moved yet");
        let effects = landed(&mut state, tag, unstaged());

        assert_eq!(state.files.selected, 1);
        assert_eq!(stages(&effects), [GitOp::Stage(vec!["b.rs".into()])]);
    }

    #[test]
    fn a_failed_reload_drops_the_held_keys() {
        let (mut state, _) = with_files(three_files(), 24);
        let tag = refresh_tag(&mut state);
        space(&mut state);

        let effects = state.update(AppIntent::StatusLoaded {
            tag,
            result: Err("boom".into()),
        });

        assert!(stages(&effects).is_empty(), "{effects:?}");
        assert!(matches!(state.popups.last(), Some(Popup::Error(_))));
        assert_eq!(state.notice, Some(KEYS_DROPPED.into()));
        // A later key acts at once: Esc closes the error, then space stages.
        press(&mut state, KeyCode::Esc);
        assert!(state.popups.is_empty());
        assert_eq!(
            stages(&space(&mut state)),
            [GitOp::Stage(vec!["a.rs".into()])]
        );
    }

    #[test]
    fn a_background_reload_holds_nothing() {
        let mut state = idle_with_files();
        background_status_tag(&watch(&mut state, WatchEvent::PollFiles));

        assert_eq!(
            stages(&space(&mut state)),
            [GitOp::Stage(vec!["a.rs".into()])]
        );
    }

    #[test]
    fn keys_for_an_input_or_a_popup_pass_through_during_a_reload() {
        let (mut state, _) = with_files(three_files(), 24);
        press_char(&mut state, '/');
        refresh_tag(&mut state);

        type_text(&mut state, "b");
        assert_eq!(state.files.filter, "b", "the filter took the key at once");
        press(&mut state, KeyCode::Enter);

        state.push_error(ErrorPopup::new("Something", "went wrong", "Close this."));
        press(&mut state, KeyCode::Esc);
        assert!(state.popups.is_empty(), "the popup closed at once");
        assert!(!state.key_hold.has_keys());
    }

    #[test]
    fn keys_past_the_cap_are_dropped_and_the_rest_replay() {
        let files: Vec<FileEntry> = (0..MAX_HELD + 4)
            .map(|i| changed(&format!("f{i:02}.rs"), '.', 'M'))
            .collect();
        let (mut state, _) = with_files(files.clone(), 60);
        let tag = refresh_tag(&mut state);

        for _ in 0..MAX_HELD + 2 {
            press_char(&mut state, 'j');
        }
        assert_eq!(state.notice, Some(KEY_DROPPED.into()));
        landed(&mut state, tag, files);

        assert_eq!(state.files.selected, MAX_HELD);
        assert!(!state.key_hold.has_keys());
    }

    #[test]
    fn switching_repository_clears_the_hold() {
        let (mut state, _) = with_files(three_files(), 24);
        refresh_tag(&mut state);
        space(&mut state);

        state.switch_repo();

        assert_eq!(state.key_hold, KeyHold::default());
        assert_eq!(state.notice, None, "cleared quietly");
    }
}
