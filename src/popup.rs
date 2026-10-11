//! Popups that sit on one stack over the workspace: menus, confirmations,
//! one-line prompts and errors. Pure types; the model owns the stack and
//! each popup hands its result back as an intent.

use crossterm::event::{KeyCode, KeyModifiers};

use crate::commit_editor::{byte_at, char_len};
use crate::git::diff::terminal_safe;
use crate::git::ops::GitOp;
use crate::intent::AppIntent;
use crate::keymap::KeyPress;
use crate::menu::MenuId;
use crate::recent_list::RecentList;
use crate::submodule_list::SubmoduleList;

/// One entry on the popup stack. The top one takes the keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Popup {
    Menu { id: MenuId, cursor: usize },
    Confirm(ConfirmKind),
    Prompt { kind: PromptKind, input: LineInput },
    Error(ErrorPopup),
    Discard(DiscardConfirm),
    Submodules(SubmoduleList),
    Recent(RecentList),
}

/// "Throw this work away?" Unlike [`ConfirmKind`], it carries the op chosen
/// when it was asked, so a refresh before the answer cannot change what goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscardConfirm {
    pub what: DiscardWhat,
    /// The file's display path, already terminal-safe.
    pub path: String,
    pub op: GitOp,
}

/// What a discard throws away, so the question can say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscardWhat {
    File,
    Untracked,
    Hunk,
    Lines,
}

impl DiscardWhat {
    pub fn title(self) -> &'static str {
        match self {
            DiscardWhat::File => "Discard file",
            DiscardWhat::Untracked => "Delete file",
            DiscardWhat::Hunk => "Discard hunk",
            DiscardWhat::Lines => "Discard lines",
        }
    }

    pub fn question(self) -> &'static str {
        match self {
            DiscardWhat::File => "Discard all unstaged changes to this file? (y/n)",
            DiscardWhat::Untracked => "Delete this untracked file from disk? (y/n)",
            DiscardWhat::Hunk => "Discard this hunk from the working tree? (y/n)",
            DiscardWhat::Lines => "Discard these lines from the working tree? (y/n)",
        }
    }
}

/// A yes/no question. Its answer is an intent, so the asker needs no callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmKind {
    Quit,
}

impl ConfirmKind {
    pub fn title(self) -> &'static str {
        match self {
            ConfirmKind::Quit => "Quit",
        }
    }

    pub fn question(self) -> &'static str {
        match self {
            ConfirmKind::Quit => "Quit Kika? (y/n)",
        }
    }

    pub fn yes(self) -> AppIntent {
        match self {
            ConfirmKind::Quit => AppIntent::Quit,
        }
    }

    pub fn no(self) -> Option<AppIntent> {
        match self {
            ConfirmKind::Quit => None,
        }
    }
}

/// A one-line text question. The typed text comes back as an intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    GoToFile,
}

impl PromptKind {
    pub fn title(self) -> &'static str {
        match self {
            PromptKind::GoToFile => "Go to file",
        }
    }

    pub fn submit(self, text: String) -> AppIntent {
        match self {
            PromptKind::GoToFile => AppIntent::GoToFile(text),
        }
    }
}

/// One line of text being typed. `col` counts chars, not bytes, and always
/// points inside the text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineInput {
    pub text: String,
    pub col: usize,
}

impl LineInput {
    /// Apply one key. Keys that mean nothing on one line are ignored.
    pub fn edit(&mut self, key: KeyPress) {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return;
        }
        match key.code {
            KeyCode::Char(c) if !c.is_control() => {
                self.text.insert(byte_at(&self.text, self.col), c);
                self.col += 1;
            }
            KeyCode::Backspace if self.col > 0 => {
                self.col -= 1;
                self.text.remove(byte_at(&self.text, self.col));
            }
            KeyCode::Delete if self.col < char_len(&self.text) => {
                self.text.remove(byte_at(&self.text, self.col));
            }
            KeyCode::Left => self.col = self.col.saturating_sub(1),
            KeyCode::Right => self.col = (self.col + 1).min(char_len(&self.text)),
            KeyCode::Home => self.col = 0,
            KeyCode::End => self.col = char_len(&self.text),
            _ => {}
        }
    }
}

/// Something failed: what, git's own words, and what to do now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorPopup {
    pub title: String,
    pub message: String,
    pub next: &'static str,
}

impl ErrorPopup {
    /// The message is made safe to draw here, whatever its source, since it
    /// can quote repo-controlled paths.
    pub fn new(title: impl Into<String>, message: &str, next: &'static str) -> Self {
        Self {
            title: title.into(),
            message: terminal_safe(message.as_bytes()),
            next,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(input: &mut LineInput, code: KeyCode) {
        input.edit(KeyPress::new(code, KeyModifiers::NONE));
    }

    fn typed(text: &str) -> LineInput {
        let mut input = LineInput::default();
        for c in text.chars() {
            press(&mut input, KeyCode::Char(c));
        }
        input
    }

    #[test]
    fn typing_inserts_at_the_cursor_by_char() {
        let mut input = typed("héllo 日本");
        assert_eq!(input.col, 8);
        press(&mut input, KeyCode::Home);
        press(&mut input, KeyCode::Right);
        press(&mut input, KeyCode::Char('x'));
        assert_eq!(input.text, "hxéllo 日本");
        assert_eq!(input.col, 2);
    }

    #[test]
    fn backspace_and_delete_remove_whole_chars_and_stop_at_the_ends() {
        let mut input = typed("a日b");
        press(&mut input, KeyCode::Left);
        press(&mut input, KeyCode::Backspace);
        assert_eq!((input.text.as_str(), input.col), ("ab", 1));
        press(&mut input, KeyCode::Delete);
        assert_eq!((input.text.as_str(), input.col), ("a", 1));
        press(&mut input, KeyCode::Delete);
        assert_eq!(input.text, "a");
        press(&mut input, KeyCode::Home);
        press(&mut input, KeyCode::Backspace);
        assert_eq!((input.text.as_str(), input.col), ("a", 0));
    }

    #[test]
    fn arrows_home_and_end_stay_inside_the_text() {
        let mut input = typed("abc");
        press(&mut input, KeyCode::Right);
        assert_eq!(input.col, 3);
        press(&mut input, KeyCode::Home);
        press(&mut input, KeyCode::Left);
        assert_eq!(input.col, 0);
        press(&mut input, KeyCode::End);
        assert_eq!(input.col, 3);
    }

    #[test]
    fn control_alt_chords_and_control_chars_are_not_typed() {
        let mut input = LineInput::default();
        input.edit(KeyPress::new(KeyCode::Char('A'), KeyModifiers::SHIFT));
        input.edit(KeyPress::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        input.edit(KeyPress::new(KeyCode::Char('x'), KeyModifiers::ALT));
        press(&mut input, KeyCode::Char('\x1b'));
        press(&mut input, KeyCode::Char('\u{9b}'));
        press(&mut input, KeyCode::Enter);
        press(&mut input, KeyCode::Tab);
        assert_eq!(input.text, "A");
        assert_eq!(input.col, 1);
    }

    #[test]
    fn an_error_message_holds_no_control_chars() {
        let error = ErrorPopup::new("Could not stage", "a\x1b[31mb\u{9b}c", "Try again.");
        assert!(!error.message.chars().any(char::is_control), "{error:?}");
        assert!(error.message.starts_with('a') && error.message.ends_with('c'));
        // Making it safe twice changes nothing, so any path may do it.
        let again = ErrorPopup::new("Could not stage", &error.message, "Try again.");
        assert_eq!(again, error);
    }

    #[test]
    fn an_error_keeps_the_lines_of_git_message() {
        let error = ErrorPopup::new("t", "fatal: one\nhint: two\n", "n");
        assert_eq!(error.message, "fatal: one\nhint: two");
    }

    #[test]
    fn confirm_and_prompt_answer_with_intents() {
        assert_eq!(ConfirmKind::Quit.yes(), AppIntent::Quit);
        assert_eq!(ConfirmKind::Quit.no(), None);
        assert_eq!(ConfirmKind::Quit.question(), "Quit Kika? (y/n)");
        assert_eq!(
            PromptKind::GoToFile.submit("b".into()),
            AppIntent::GoToFile("b".into())
        );
    }
}
