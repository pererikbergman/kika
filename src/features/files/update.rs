//! The Files panel's list navigation: moving the selection among the rows
//! the filter shows.

use super::gitignore::{self, IgnoreAdded, IgnoreDraft};
use crate::effect::{Effect, Tag};
use crate::git::diff::sanitize;
use crate::list_filter;
use crate::model::{AppState, Context};
use crate::popup::{ErrorPopup, LineInput, Popup, PromptKind};

impl AppState {
    /// Move by `delta` rows among those the filter shows.
    pub(crate) fn move_selection(&mut self, delta: isize) -> Vec<Effect> {
        let Some(current) = self.selected else {
            return vec![];
        };
        let Some(next) = list_filter::step(&self.files_visible(), current, delta) else {
            return vec![];
        };
        if next == current {
            return vec![];
        }
        self.select(next)
    }

    /// Select the first file whose path contains `text`, ignoring case.
    pub(crate) fn go_to_file(&mut self, text: &str) -> Vec<Effect> {
        let needle = text.trim().to_lowercase();
        if needle.is_empty() {
            return vec![];
        }
        let found = self
            .files
            .iter()
            .position(|f| f.path.display().to_lowercase().contains(&needle));
        match found {
            Some(index) => self.select(index),
            None => {
                self.notice = Some("No file matches".into());
                vec![]
            }
        }
    }

    /// Ask for the pattern to ignore the selected untracked file by,
    /// filled with the one that matches exactly that file.
    pub(crate) fn open_ignore(&mut self) {
        if self.context != Context::Workspace {
            return;
        }
        if let Some(reason) = super::keys::ignore_disabled(self) {
            self.notice = Some(reason);
            return;
        }
        let Some(file) = self.selected.and_then(|i| self.files.get(i)) else {
            return;
        };
        let bytes = match gitignore::escape(&file.path) {
            Ok(bytes) => bytes,
            Err(why) => {
                self.notice = Some(why.into());
                return;
            }
        };
        let shown = sanitize(&bytes);
        self.popups.push(Popup::Prompt {
            kind: PromptKind::IgnorePattern,
            input: LineInput::with_text(&shown),
        });
        self.ignore_draft = Some(IgnoreDraft { shown, bytes });
    }

    /// Add the accepted pattern: the offered one's exact bytes when it is
    /// unchanged, else the typed text as the user's own pattern. Empty
    /// text or a control character keeps the prompt open with a notice.
    pub(crate) fn ignore_answered(&mut self, text: String) -> Vec<Effect> {
        let Some(draft) = self.ignore_draft.take() else {
            return vec![];
        };
        let line = if text == draft.shown {
            draft.bytes.clone()
        } else if text.trim().is_empty() || text.contains(char::is_control) {
            self.notice = Some(if text.trim().is_empty() {
                "Type a pattern to ignore".into()
            } else {
                "The pattern has a control character".into()
            });
            self.popups.push(Popup::Prompt {
                kind: PromptKind::IgnorePattern,
                input: LineInput::with_text(&text),
            });
            self.ignore_draft = Some(draft);
            return vec![];
        } else {
            text.into_bytes()
        };
        let seq = self.take_seq();
        vec![Effect::AppendIgnore {
            tag: self.user_tag(seq),
            line,
        }]
    }

    /// Say what the add did and reload the status, so an ignored file
    /// leaves the list.
    pub(crate) fn ignore_written(
        &mut self,
        tag: Tag,
        result: Result<IgnoreAdded, String>,
    ) -> Vec<Effect> {
        if tag.user {
            self.busy = self.busy.saturating_sub(1);
        }
        if tag.generation != self.generation {
            return vec![];
        }
        match result {
            Ok(IgnoreAdded { pattern, added }) => {
                self.notice_text = Some(if added {
                    format!("Added {pattern} to .gitignore")
                } else {
                    format!("{pattern} is already in .gitignore")
                });
                self.refresh_status()
            }
            Err(message) => {
                self.push_error(ErrorPopup::new(
                    "Could not add to .gitignore",
                    &message,
                    "Close this, fix the cause, then try again.",
                ));
                vec![]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyModifiers};

    use crate::effect::Effect;
    use crate::features::files::IgnoreAdded;
    use crate::git::diff::{DiffOptions, DiffRequest};
    use crate::git::path::RepoPath;
    use crate::intent::AppIntent;
    use crate::keymap::{KeyPress, KeyScope};
    use crate::model::AppState;
    use crate::popup::{LineInput, Popup, PromptKind};
    use crate::test_support::*;

    #[test]
    fn select_next_loads_the_next_file_and_its_chunk_replaces_the_lines() {
        let (mut state, effects) = with_files(three_files(), 24);
        let first = diff_tag(&effects);
        state.update(chunk(first, 3, true));
        assert_eq!(state.diff.lines.len(), 3);

        let effects = state.update(AppIntent::SelectNext);
        let second = diff_tag(&effects);
        assert!(second.seq > first.seq);
        assert_eq!(state.selected, Some(1));
        match &effects[0] {
            Effect::LoadDiff { request, .. } => {
                assert_eq!(
                    request,
                    &DiffRequest::for_entry(&state.files[1], DiffOptions::default())
                )
            }
            other => panic!("{other:?}"),
        }
        // The old lines stay until the new ones arrive.
        assert_eq!(state.diff.lines.len(), 3);

        state.update(chunk(second, 2, true));
        let seq = second.seq;
        assert_eq!(
            state.diff.lines,
            [line(&format!("{seq}:0")), line(&format!("{seq}:1"))]
        );
        assert!(!state.diff.pending);
        assert!(state.diff.done);
    }

    #[test]
    fn selection_stops_at_the_ends_of_the_list() {
        let (mut state, _) = with_files(three_files(), 24);
        assert_eq!(state.update(AppIntent::SelectPrev), vec![]);
        assert_eq!(state.selected, Some(0));

        state.update(AppIntent::SelectNext);
        state.update(AppIntent::SelectNext);
        assert_eq!(state.update(AppIntent::SelectNext), vec![]);
        assert_eq!(state.selected, Some(2));

        diff_tag(&state.update(AppIntent::SelectPrev));
        assert_eq!(state.selected, Some(1));
    }

    #[test]
    fn selection_does_nothing_without_files() {
        let (mut state, effects) = with_files(vec![], 24);
        assert_eq!(effects, vec![Effect::CancelDiff]);
        assert_eq!(state.selected, None);
        assert_eq!(state.update(AppIntent::SelectNext), vec![]);
        assert_eq!(state.update(AppIntent::SelectPrev), vec![]);
    }

    #[test]
    fn the_go_to_file_prompt_selects_a_match_ignoring_case() {
        let mut state = two_txt_files();
        state.update(AppIntent::OpenPrompt(PromptKind::GoToFile));
        assert_eq!(state.key_focus(), KeyScope::Prompt);

        state.update(AppIntent::Key(KeyPress::new(
            KeyCode::Char('B'),
            KeyModifiers::SHIFT,
        )));
        assert_eq!(prompt_text(&state), "B");
        let effects = press(&mut state, KeyCode::Enter);

        let tag = diff_tag(&effects);
        assert!(tag.seq > 0);
        assert_eq!(state.selected, Some(1));
        assert_eq!(state.diff.path, Some("b.txt".into()));
        assert_eq!(state.popups, []);
    }

    #[test]
    fn q_and_question_mark_are_typed_into_the_prompt_and_esc_cancels() {
        let mut state = two_txt_files();
        state.update(AppIntent::OpenPrompt(PromptKind::GoToFile));
        type_text(&mut state, "q?b");
        assert_eq!(prompt_text(&state), "q?b");
        assert!(!state.should_quit);
        assert!(!state.help_open);

        assert_eq!(press(&mut state, KeyCode::Esc), vec![]);
        assert_eq!(state.popups, []);
        assert_eq!(state.selected, Some(0));
        assert!(!state.should_quit);
    }

    #[test]
    fn going_to_a_missing_file_says_so_and_empty_text_does_nothing() {
        let mut state = two_txt_files();
        assert_eq!(state.update(AppIntent::GoToFile("zzz".into())), vec![]);
        assert_eq!(state.notice, Some("No file matches".into()));
        assert_eq!(state.selected, Some(0));

        state.notice = None;
        assert_eq!(state.update(AppIntent::GoToFile("  ".into())), vec![]);
        assert_eq!(state.notice, None);
        assert_eq!(state.selected, Some(0));
    }

    /// The text and kind of the prompt on top.
    fn top_prompt(state: &AppState) -> (PromptKind, String) {
        match state.popups.last() {
            Some(Popup::Prompt { kind, input }) => (*kind, input.text.clone()),
            other => panic!("expected a prompt, got {other:?}"),
        }
    }

    /// Files listing a modified `a.rs` and an untracked file named by
    /// `name`'s bytes, with `a.rs` selected.
    fn ignorable(name: &[u8]) -> AppState {
        let mut file = untracked("x");
        file.path = RepoPath::from_bytes(name);
        let (state, _) = with_files(vec![changed("a.rs", '.', 'M'), file], 24);
        assert_eq!(state.selected, Some(0));
        state
    }

    #[test]
    fn i_on_a_tracked_file_says_why_it_cannot_be_ignored() {
        let mut state = ignorable(b"build.log");
        assert_eq!(press_char(&mut state, 'i'), vec![]);
        assert_eq!(
            state.notice,
            Some("Only untracked files can be ignored".into())
        );
        assert!(state.popups.is_empty());

        let (mut empty, _) = with_files(vec![], 24);
        press_char(&mut empty, 'i');
        assert_eq!(empty.notice, Some("No files".into()));
    }

    #[test]
    fn i_on_an_untracked_file_offers_its_exact_pattern_and_enter_adds_it() {
        let mut state = ignorable(b"build[1].log");
        press_char(&mut state, 'j');
        assert_eq!(press_char(&mut state, 'i'), vec![]);
        assert_eq!(
            top_prompt(&state),
            (PromptKind::IgnorePattern, "/build\\[1].log".into())
        );
        let effects = press(&mut state, KeyCode::Enter);
        let [Effect::AppendIgnore { tag, line }] = effects.as_slice() else {
            panic!("expected an append, got {effects:?}");
        };
        assert_eq!(line, b"/build\\[1].log");
        assert!(state.popups.is_empty());
        assert_eq!(state.ignore_draft, None);

        let effects = state.update(AppIntent::IgnoreWritten {
            tag: *tag,
            result: Ok(IgnoreAdded {
                pattern: "/build\\[1].log".into(),
                added: true,
            }),
        });
        assert_eq!(
            state.notice_text.as_deref(),
            Some("Added /build\\[1].log to .gitignore")
        );
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::LoadStatus { .. })),
            "{effects:?}"
        );
        assert!(
            !effects.iter().any(|e| matches!(e, Effect::RunOp { .. })),
            "no git command runs: {effects:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_name_that_is_not_utf8_is_added_by_its_exact_bytes() {
        let mut state = ignorable(b"x\xff");
        press_char(&mut state, 'j');
        press_char(&mut state, 'i');
        assert_eq!(
            top_prompt(&state),
            (PromptKind::IgnorePattern, "/x\u{FFFD}".into())
        );
        let effects = press(&mut state, KeyCode::Enter);
        assert!(
            matches!(effects.as_slice(), [Effect::AppendIgnore { line, .. }] if line == b"/x\xff"),
            "{effects:?}"
        );
    }

    #[test]
    fn an_edited_pattern_is_added_as_typed_and_a_blank_one_is_refused() {
        let mut state = ignorable(b"build.log");
        press_char(&mut state, 'j');
        press_char(&mut state, 'i');
        if let Some(Popup::Prompt { input, .. }) = state.popups.last_mut() {
            *input = LineInput::with_text("  ");
        }
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.notice, Some("Type a pattern to ignore".into()));
        assert_eq!(top_prompt(&state).0, PromptKind::IgnorePattern);

        if let Some(Popup::Prompt { input, .. }) = state.popups.last_mut() {
            *input = LineInput::with_text("*.log");
        }
        let effects = press(&mut state, KeyCode::Enter);
        assert!(
            matches!(effects.as_slice(), [Effect::AppendIgnore { line, .. }] if line == b"*.log"),
            "{effects:?}"
        );
    }

    #[test]
    fn esc_drops_the_draft_and_a_failed_write_shows_an_error() {
        let mut state = ignorable(b"build.log");
        press_char(&mut state, 'j');
        press_char(&mut state, 'i');
        press(&mut state, KeyCode::Esc);
        assert!(state.popups.is_empty());
        assert_eq!(state.ignore_draft, None);

        press_char(&mut state, 'i');
        let effects = press(&mut state, KeyCode::Enter);
        let [Effect::AppendIgnore { tag, .. }] = effects.as_slice() else {
            panic!("expected an append, got {effects:?}");
        };
        let effects = state.update(AppIntent::IgnoreWritten {
            tag: *tag,
            result: Err(crate::worktree_file::CHANGED_ON_DISK.into()),
        });
        assert_eq!(effects, vec![]);
        assert!(matches!(state.popups.last(), Some(Popup::Error(_))));
    }
}
