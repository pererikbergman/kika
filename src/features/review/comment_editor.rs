//! The built-in comment composer: a multi-line markdown buffer edited key
//! by key over the files screen or the conversation. For a line comment
//! `ctrl-s` keeps it in the pending review; for a reply or a general
//! comment it asks before posting to GitHub. `esc` drops it (asking first
//! when it has text), and every other key is typed. Pure, so `update`
//! owns it and tests drive it.

use crossterm::event::{KeyCode, KeyModifiers};

use super::discussion_write::{POSTING, WAIT};
use super::pending::CommentTarget;
use super::state::{ReviewScreen, ReviewState};
use crate::confirm_question::{Answer, ConfirmQuestion, Then};
use crate::effect::Effect;
use crate::features::review::ReviewIntent;
use crate::intent::AppIntent;
use crate::keymap::{KeyPress, KeyScope};
use crate::model::AppState;
use crate::popup::{Popup, byte_at, char_len};
use kika_forge::discussion_write::DiscussionWrite;

/// Text being typed. `row` indexes `lines` and `col` counts chars, not
/// bytes, in that line; both always point inside the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentEditor {
    /// Never empty: no text is one empty line.
    pub lines: Vec<String>,
    pub row: usize,
    pub col: usize,
}

impl Default for CommentEditor {
    fn default() -> Self {
        Self {
            lines: vec![String::new()],
            row: 0,
            col: 0,
        }
    }
}

impl CommentEditor {
    /// An editor holding `text`, the cursor at its end. Control characters
    /// other than tabs are dropped, since the text is drawn.
    pub fn from_text(text: &str) -> Self {
        let clean = |line: &str| {
            line.chars()
                .filter(|&c| c == '\t' || !c.is_control())
                .collect::<String>()
        };
        let lines: Vec<String> = text.split('\n').map(clean).collect();
        let row = lines.len() - 1;
        let col = char_len(&lines[row]);
        Self { lines, row, col }
    }

    /// The text, its lines joined by `\n`.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// Nothing worth keeping has been typed.
    pub fn is_blank(&self) -> bool {
        self.lines.iter().all(|line| line.trim().is_empty())
    }

    /// Apply one key. Keys that mean nothing here, and any with Ctrl or
    /// Alt, are ignored.
    pub fn edit(&mut self, key: KeyPress) {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return;
        }
        match key.code {
            KeyCode::Char(c) if !c.is_control() => self.insert(c),
            KeyCode::Tab => self.insert('\t'),
            KeyCode::Enter => {
                let at = byte_at(&self.lines[self.row], self.col);
                let rest = self.lines[self.row].split_off(at);
                self.row += 1;
                self.lines.insert(self.row, rest);
                self.col = 0;
            }
            KeyCode::Backspace if self.col > 0 => {
                self.col -= 1;
                let line = &mut self.lines[self.row];
                line.remove(byte_at(line, self.col));
            }
            KeyCode::Backspace if self.row > 0 => {
                let line = self.lines.remove(self.row);
                self.row -= 1;
                self.col = char_len(&self.lines[self.row]);
                self.lines[self.row].push_str(&line);
            }
            KeyCode::Delete if self.col < char_len(&self.lines[self.row]) => {
                let line = &mut self.lines[self.row];
                line.remove(byte_at(line, self.col));
            }
            KeyCode::Delete if self.row + 1 < self.lines.len() => {
                let next = self.lines.remove(self.row + 1);
                self.lines[self.row].push_str(&next);
            }
            KeyCode::Left => self.col = self.col.saturating_sub(1),
            KeyCode::Right => self.col = (self.col + 1).min(char_len(&self.lines[self.row])),
            KeyCode::Home => self.col = 0,
            KeyCode::End => self.col = char_len(&self.lines[self.row]),
            KeyCode::Up if self.row > 0 => self.go_to(self.row - 1),
            KeyCode::Down if self.row + 1 < self.lines.len() => self.go_to(self.row + 1),
            _ => {}
        }
    }

    fn insert(&mut self, c: char) {
        let line = &mut self.lines[self.row];
        line.insert(byte_at(line, self.col), c);
        self.col += 1;
    }

    fn go_to(&mut self, row: usize) {
        self.row = row;
        self.col = self.col.min(char_len(&self.lines[row]));
    }
}

/// What the composer's text is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerKind {
    /// A line comment kept in the pending review: where it goes, and the
    /// pending comment it edits (`None` for a new one).
    Line {
        target: CommentTarget,
        editing: Option<u64>,
    },
    /// A reply to review thread `thread` (its node id) of pull request
    /// `number`, posted to GitHub; `label` names the thread, made safe to
    /// draw.
    Reply {
        number: u64,
        thread: String,
        label: String,
    },
    /// A general comment on pull request `number`, whose node id is
    /// `subject`, posted to GitHub.
    General { number: u64, subject: String },
}

/// The open composer: what its text is for, and the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Composer {
    pub kind: ComposerKind,
    pub text: CommentEditor,
}

impl Composer {
    /// A composer with no text yet.
    pub fn new(kind: ComposerKind) -> Self {
        Self {
            kind,
            text: CommentEditor::default(),
        }
    }

    /// Its text goes to GitHub, after a question, not to the pending review.
    pub fn posts(&self) -> bool {
        !matches!(self.kind, ComposerKind::Line { .. })
    }
}

/// The composer is open over the files screen or the conversation, so it
/// has the keys.
pub(crate) fn composer_open(state: &AppState) -> bool {
    let shown =
        state.review_files_shown() || matches!(state.review.screen, ReviewScreen::Conversation(_));
    shown && state.review.composer.is_some()
}

impl ReviewState {
    /// The open composer posts to GitHub and its write is in flight.
    pub(super) fn composer_posting(&self) -> bool {
        let posting = self
            .writing
            .as_ref()
            .is_some_and(|w| !matches!(w.write, DiscussionWrite::Resolve { .. }));
        posting && self.composer.as_ref().is_some_and(Composer::posts)
    }
}

impl AppState {
    /// The open composer's text is on its way to GitHub: it may not
    /// change or close until GitHub answers.
    pub(super) fn composer_posting(&self) -> bool {
        self.review.composer_posting()
    }

    /// A key no composer binding took: typed into the text, unless the
    /// text is being posted.
    pub(crate) fn type_comment(&mut self, key: KeyPress) {
        if self.composer_posting() {
            return;
        }
        if let Some(composer) = &mut self.review.composer {
            composer.text.edit(key);
        }
    }

    /// `ctrl-s`: keep a line comment in the pending review, never posted;
    /// for a reply or a general comment, ask before posting it to GitHub.
    /// Blank text is refused, and the composer stays.
    pub(crate) fn save_comment(&mut self) -> Vec<Effect> {
        let Some(composer) = &self.review.composer else {
            return vec![];
        };
        let text = composer.text.text();
        let question = match &composer.kind {
            ComposerKind::Line { editing, .. } => {
                // Refused: the composer stays, so the text can be tried again.
                if let Some(reason) = self.pending_change_refused(*editing) {
                    self.notice = Some(reason);
                    return vec![];
                }
                None
            }
            ComposerKind::Reply { .. } => Some("Post this reply to GitHub? (y/n)".to_string()),
            ComposerKind::General { number, .. } => {
                Some(format!("Post this comment on #{number} to GitHub? (y/n)"))
            }
        };
        if let Some(question) = question {
            if self.review.writing.is_some() {
                self.notice = Some(POSTING.into());
                return vec![];
            }
            return self.ask_post(question, &text);
        }
        let Some(body) = self.comment_body(&text) else {
            return vec![];
        };
        let Some(composer) = self.review.composer.take() else {
            return vec![];
        };
        match composer.kind {
            ComposerKind::Line {
                editing: Some(id), ..
            } => self.update_pending(id, body),
            ComposerKind::Line {
                target,
                editing: None,
            } => self.add_pending(target, body),
            // Asked about above; never kept.
            ComposerKind::Reply { .. } | ComposerKind::General { .. } => vec![],
        }
    }

    /// The question before a post. Only `y` answers it; the composer and
    /// its text stay under it either way.
    fn ask_post(&mut self, question: String, text: &str) -> Vec<Effect> {
        if self.comment_body(text).is_none() {
            return vec![];
        }
        self.popups.push(Popup::Question(ConfirmQuestion {
            scope: KeyScope::ReviewCommentConfirm,
            title: "Post to GitHub".into(),
            text: question,
            answer: Answer::YOnly,
            then: Then::Intent(Box::new(AppIntent::Review(ReviewIntent::PostComment))),
            range: false,
            head: None,
        }));
        vec![]
    }

    /// `esc`: close a blank composer at once; ask before dropping text.
    /// While its text is being posted it stays.
    pub(crate) fn escape_comment(&mut self) {
        if self.composer_posting() {
            self.notice = Some(WAIT.into());
            return;
        }
        let Some(composer) = &self.review.composer else {
            return;
        };
        if composer.text.is_blank() {
            self.review.composer = None;
            return;
        }
        self.popups.push(Popup::Question(ConfirmQuestion {
            scope: KeyScope::ReviewCommentConfirm,
            title: "Discard comment".into(),
            text: "Discard this comment? (y/n)".into(),
            answer: Answer::YOnly,
            then: Then::Intent(Box::new(AppIntent::Review(ReviewIntent::DiscardComment))),
            range: false,
            head: None,
        }));
    }

    /// The yes of "Discard this comment?".
    pub(crate) fn discard_comment(&mut self) {
        if self.composer_posting() {
            return;
        }
        self.review.composer = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyPress {
        KeyPress::new(code, KeyModifiers::NONE)
    }

    fn typed(text: &str) -> CommentEditor {
        let mut editor = CommentEditor::default();
        for c in text.chars() {
            editor.edit(key(match c {
                '\n' => KeyCode::Enter,
                c => KeyCode::Char(c),
            }));
        }
        editor
    }

    #[test]
    fn typing_builds_lines_and_enter_breaks_them() {
        let editor = typed("hi\nthere");
        assert_eq!(editor.lines, ["hi", "there"]);
        assert_eq!((editor.row, editor.col), (1, 5));
        assert_eq!(editor.text(), "hi\nthere");
        assert!(!editor.is_blank());
        assert!(typed(" \n ").is_blank());
    }

    #[test]
    fn control_characters_and_ctrl_or_alt_keys_are_not_inserted() {
        let mut editor = typed("a");
        editor.edit(key(KeyCode::Char('\u{1b}')));
        editor.edit(key(KeyCode::Char('\u{7}')));
        editor.edit(KeyPress::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        editor.edit(KeyPress::new(KeyCode::Char('x'), KeyModifiers::ALT));
        assert_eq!(editor.text(), "a");
        editor.edit(key(KeyCode::Tab));
        assert_eq!(editor.text(), "a\t");
    }

    #[test]
    fn backspace_and_delete_join_lines_and_the_cursor_stays_inside() {
        let mut editor = typed("ab\ncd");
        editor.edit(key(KeyCode::Home));
        editor.edit(key(KeyCode::Backspace));
        assert_eq!(editor.lines, ["abcd"]);
        assert_eq!((editor.row, editor.col), (0, 2));
        editor.edit(key(KeyCode::Delete));
        assert_eq!(editor.text(), "abd");
        editor.edit(key(KeyCode::Enter));
        editor.edit(key(KeyCode::Up));
        editor.edit(key(KeyCode::End));
        editor.edit(key(KeyCode::Delete));
        assert_eq!(editor.text(), "abd");
        editor.edit(key(KeyCode::Down));
        assert_eq!(editor.row, 0);
    }

    #[test]
    fn wide_characters_are_counted_as_one() {
        let mut editor = typed("é😀");
        editor.edit(key(KeyCode::Left));
        editor.edit(key(KeyCode::Char('x')));
        assert_eq!(editor.text(), "éx😀");
    }

    #[test]
    fn an_editor_from_text_drops_control_characters_and_ends_at_the_end() {
        let editor = CommentEditor::from_text("one\ntw\u{1b}o\tx");
        assert_eq!(editor.lines, ["one", "two\tx"]);
        assert_eq!((editor.row, editor.col), (1, 5));
    }
}
