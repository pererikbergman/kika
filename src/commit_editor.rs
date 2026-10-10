//! The commit message popup's text: a one-line subject and a multi-line body,
//! edited key by key. Pure, so `update` can own it and tests can drive it.

use crossterm::event::{KeyCode, KeyModifiers};

use crate::keymap::KeyPress;

/// Which box the cursor is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Subject,
    Body,
}

/// A commit message being typed. `row` indexes `body` and `col` counts chars,
/// not bytes, in the focused line; both always point inside the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitEditor {
    pub subject: String,
    /// Never empty: an empty body is one empty line.
    pub body: Vec<String>,
    pub field: Field,
    pub row: usize,
    pub col: usize,
    /// Stage everything before committing, because nothing was staged.
    pub stage_all: bool,
}

impl CommitEditor {
    pub fn new(stage_all: bool) -> Self {
        Self {
            subject: String::new(),
            body: vec![String::new()],
            field: Field::Subject,
            row: 0,
            col: 0,
            stage_all,
        }
    }

    /// The message for `git commit -F -`: the subject, then a blank line and
    /// the body when the body has any text. Always ends in a newline.
    pub fn message(&self) -> String {
        let mut message = self.subject.clone();
        if self.body.iter().any(|line| !line.trim().is_empty()) {
            message.push_str("\n\n");
            message.push_str(&self.body.join("\n"));
        }
        message.push('\n');
        message
    }

    /// Nothing worth keeping has been typed.
    pub fn is_blank(&self) -> bool {
        self.subject.trim().is_empty() && self.body.iter().all(|line| line.trim().is_empty())
    }

    /// Jump to the start of the body, or to the end of the subject.
    pub fn toggle_field(&mut self) {
        match self.field {
            Field::Subject => self.enter_body(0, 0),
            Field::Body => self.enter_subject(char_len(&self.subject)),
        }
    }

    /// Apply one key. Keys that mean nothing here are ignored.
    pub fn edit(&mut self, key: KeyPress) {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return;
        }
        match key.code {
            KeyCode::Char(c) if !c.is_control() => {
                let col = self.col;
                let line = self.line_mut();
                line.insert(byte_at(line, col), c);
                self.col += 1;
            }
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left => self.col = self.col.saturating_sub(1),
            KeyCode::Right => self.col = (self.col + 1).min(char_len(self.line())),
            KeyCode::Home => self.col = 0,
            KeyCode::End => self.col = char_len(self.line()),
            KeyCode::Up => self.up(),
            KeyCode::Down => self.down(),
            KeyCode::Enter => self.enter(),
            KeyCode::Tab => self.toggle_field(),
            _ => {}
        }
    }

    fn line(&self) -> &str {
        match self.field {
            Field::Subject => &self.subject,
            Field::Body => &self.body[self.row],
        }
    }

    fn line_mut(&mut self) -> &mut String {
        match self.field {
            Field::Subject => &mut self.subject,
            Field::Body => &mut self.body[self.row],
        }
    }

    fn enter_subject(&mut self, col: usize) {
        self.field = Field::Subject;
        self.row = 0;
        self.col = col.min(char_len(&self.subject));
    }

    fn enter_body(&mut self, row: usize, col: usize) {
        self.field = Field::Body;
        self.row = row;
        self.col = col.min(char_len(&self.body[row]));
    }

    fn backspace(&mut self) {
        if self.col > 0 {
            self.col -= 1;
            let col = self.col;
            let line = self.line_mut();
            line.remove(byte_at(line, col));
        } else if self.field == Field::Body && self.row > 0 {
            let line = self.body.remove(self.row);
            self.row -= 1;
            self.col = char_len(&self.body[self.row]);
            self.body[self.row].push_str(&line);
        }
    }

    fn delete(&mut self) {
        let col = self.col;
        if col < char_len(self.line()) {
            let line = self.line_mut();
            line.remove(byte_at(line, col));
        } else if self.field == Field::Body && self.row + 1 < self.body.len() {
            let next = self.body.remove(self.row + 1);
            self.body[self.row].push_str(&next);
        }
    }

    fn up(&mut self) {
        match self.field {
            Field::Subject => {}
            Field::Body if self.row == 0 => self.enter_subject(self.col),
            Field::Body => self.enter_body(self.row - 1, self.col),
        }
    }

    fn down(&mut self) {
        match self.field {
            Field::Subject => self.enter_body(0, self.col),
            Field::Body if self.row + 1 < self.body.len() => {
                self.enter_body(self.row + 1, self.col)
            }
            Field::Body => {}
        }
    }

    fn enter(&mut self) {
        match self.field {
            Field::Subject => self.enter_body(0, 0),
            Field::Body => {
                let line = &mut self.body[self.row];
                let rest = line.split_off(byte_at(line, self.col));
                self.body.insert(self.row + 1, rest);
                self.enter_body(self.row + 1, 0);
            }
        }
    }
}

pub(crate) fn char_len(text: &str) -> usize {
    text.chars().count()
}

/// The byte offset of char `col` in `text`, or its end.
pub(crate) fn byte_at(text: &str, col: usize) -> usize {
    text.char_indices().nth(col).map_or(text.len(), |(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(editor: &mut CommitEditor, code: KeyCode) {
        editor.edit(KeyPress::new(code, KeyModifiers::NONE));
    }

    fn type_text(editor: &mut CommitEditor, text: &str) {
        for c in text.chars() {
            match c {
                '\n' => press(editor, KeyCode::Enter),
                c => press(editor, KeyCode::Char(c)),
            }
        }
    }

    fn typed(text: &str) -> CommitEditor {
        let mut editor = CommitEditor::new(false);
        type_text(&mut editor, text);
        editor
    }

    #[test]
    fn a_new_editor_is_empty_in_the_subject() {
        let editor = CommitEditor::new(true);
        assert_eq!(editor.subject, "");
        assert_eq!(editor.body, [""]);
        assert_eq!(editor.field, Field::Subject);
        assert_eq!((editor.row, editor.col), (0, 0));
        assert!(editor.stage_all);
    }

    #[test]
    fn subject_enter_and_two_body_lines_make_the_message() {
        let editor = typed("subj\nline 1\nline 2");
        assert_eq!(editor.subject, "subj");
        assert_eq!(editor.body, ["line 1", "line 2"]);
        assert_eq!(editor.message(), "subj\n\nline 1\nline 2\n");
    }

    #[test]
    fn a_blank_body_is_left_out_of_the_message() {
        assert_eq!(typed("subj").message(), "subj\n");
        assert_eq!(typed("subj\n  \n\t").message(), "subj\n");
    }

    #[test]
    fn shifted_letters_are_typed_but_control_and_alt_chords_are_not() {
        let mut editor = CommitEditor::new(false);
        editor.edit(KeyPress::new(KeyCode::Char('A'), KeyModifiers::SHIFT));
        editor.edit(KeyPress::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        editor.edit(KeyPress::new(KeyCode::Char('x'), KeyModifiers::ALT));
        press(&mut editor, KeyCode::F(2));
        // Control characters, such as a C1 CSI, are never typed.
        press(&mut editor, KeyCode::Char('\u{9b}'));
        press(&mut editor, KeyCode::Char('\x1b'));
        assert_eq!(editor.subject, "A");
        assert_eq!(editor.col, 1);
    }

    #[test]
    fn utf8_chars_are_edited_as_whole_chars() {
        let mut editor = typed("héllo 日本");
        assert_eq!(editor.col, 8);
        press(&mut editor, KeyCode::Backspace);
        assert_eq!(editor.subject, "héllo 日");
        for _ in 0..6 {
            press(&mut editor, KeyCode::Left);
        }
        press(&mut editor, KeyCode::Delete);
        assert_eq!(editor.subject, "hllo 日");
        type_text(&mut editor, "ë");
        assert_eq!(editor.subject, "hëllo 日");
        assert_eq!(editor.col, 2);
    }

    #[test]
    fn arrows_home_and_end_move_within_the_line_and_stop_at_its_ends() {
        let mut editor = typed("abc");
        press(&mut editor, KeyCode::Right);
        assert_eq!(editor.col, 3);
        press(&mut editor, KeyCode::Home);
        assert_eq!(editor.col, 0);
        press(&mut editor, KeyCode::Left);
        assert_eq!(editor.col, 0);
        type_text(&mut editor, "x");
        assert_eq!(editor.subject, "xabc");
        press(&mut editor, KeyCode::End);
        assert_eq!(editor.col, 4);
        press(&mut editor, KeyCode::Delete);
        assert_eq!(editor.subject, "xabc");
    }

    #[test]
    fn backspace_at_the_start_of_the_subject_does_nothing() {
        let mut editor = typed("ab");
        press(&mut editor, KeyCode::Home);
        press(&mut editor, KeyCode::Backspace);
        assert_eq!(editor.subject, "ab");
    }

    #[test]
    fn enter_in_the_body_splits_the_line_at_the_cursor() {
        let mut editor = typed("s\nonetwo");
        for _ in 0..3 {
            press(&mut editor, KeyCode::Left);
        }
        press(&mut editor, KeyCode::Enter);
        assert_eq!(editor.body, ["one", "two"]);
        assert_eq!((editor.row, editor.col), (1, 0));
    }

    #[test]
    fn backspace_at_column_zero_joins_with_the_previous_line() {
        let mut editor = typed("s\none\ntwo");
        press(&mut editor, KeyCode::Home);
        press(&mut editor, KeyCode::Backspace);
        assert_eq!(editor.body, ["onetwo"]);
        assert_eq!((editor.row, editor.col), (0, 3));
        // At the start of the body there is nothing to join with.
        press(&mut editor, KeyCode::Home);
        press(&mut editor, KeyCode::Backspace);
        assert_eq!(editor.body, ["onetwo"]);
        assert_eq!(editor.field, Field::Body);
    }

    #[test]
    fn delete_at_the_end_of_a_body_line_joins_the_next_one() {
        let mut editor = typed("s\none\ntwo");
        press(&mut editor, KeyCode::Up);
        press(&mut editor, KeyCode::End);
        press(&mut editor, KeyCode::Delete);
        assert_eq!(editor.body, ["onetwo"]);
        assert_eq!((editor.row, editor.col), (0, 3));
    }

    #[test]
    fn up_and_down_move_rows_and_clamp_the_column() {
        let mut editor = typed("subject\nlong line\nab");
        press(&mut editor, KeyCode::Up);
        assert_eq!((editor.row, editor.col), (0, 2));
        press(&mut editor, KeyCode::End);
        press(&mut editor, KeyCode::Down);
        assert_eq!((editor.row, editor.col), (1, 2));
        press(&mut editor, KeyCode::Down);
        assert_eq!((editor.row, editor.col), (1, 2));
    }

    #[test]
    fn up_on_the_first_body_row_goes_to_the_subject_and_down_comes_back() {
        let mut editor = typed("subject\nab");
        press(&mut editor, KeyCode::Up);
        assert_eq!(editor.field, Field::Subject);
        assert_eq!(editor.col, 2);
        press(&mut editor, KeyCode::Down);
        assert_eq!(editor.field, Field::Body);
        assert_eq!((editor.row, editor.col), (0, 2));
    }

    #[test]
    fn tab_toggles_between_subject_and_body_keeping_the_text() {
        let mut editor = typed("subject");
        press(&mut editor, KeyCode::Tab);
        assert_eq!(editor.field, Field::Body);
        assert_eq!((editor.row, editor.col), (0, 0));
        type_text(&mut editor, "b");
        press(&mut editor, KeyCode::Tab);
        assert_eq!(editor.field, Field::Subject);
        assert_eq!(editor.col, 7);
        type_text(&mut editor, "!");
        assert_eq!(editor.subject, "subject!");
        assert_eq!(editor.body, ["b"]);
    }
}
