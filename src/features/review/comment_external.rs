//! Writing a review comment in the user's editor (`ctrl-e` in any
//! composer). The hand-off in core runs the editor on a private `.md` file
//! holding only the comment; its answer lands here. Closing the editor
//! counts as `ctrl-s`: the text goes through [`AppState::save_comment`], so
//! a line comment is kept and a reply or general comment is only asked
//! about. The answer is applied only to the composer that asked, which the
//! [`ComposerStamp`] names; for any other the file is kept and named.

use std::path::Path;

use super::comment_editor::{CommentEditor, ComposerKind};
use super::discussion_write::wait;
use crate::effect::Effect;
use crate::handoff::{HandOff, KeptFile};
use crate::keymap::Reason;
use crate::model::AppState;
use crate::popup::ErrorPopup;
use kika_git::safe_text::terminal_safe;

const EMPTY: &str = "Nothing was saved: the comment is empty";

/// Which composer asked for the editor: the repository generation and the
/// composer's kind, which holds its target and, for a line comment, the
/// head it was written at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerStamp {
    pub generation: u64,
    pub kind: ComposerKind,
}

/// The text the editor left, cleaned, and the file it is still in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditedComment {
    pub text: String,
    pub file: KeptFile,
}

/// Why the editor gave no comment back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentEditError {
    /// The editor was not found, did not start or exited with an error;
    /// its file is already gone.
    Launch(String),
    /// The editor closed but its file could not be read back, or is not
    /// UTF-8. The file is kept so nothing typed is lost.
    Unreadable { why: String, file: KeptFile },
}

/// The comment the editor left in `bytes`, cleaned as the composer cleans
/// typed text: CRLF as LF, no control character but `\n` and `\t`, and no
/// blank lines at the end. Markdown `#` lines are the user's and stay.
pub fn edited_comment(bytes: &[u8]) -> Result<String, String> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| "The comment is not valid UTF-8".to_string())?;
    let clean = kika_forge::pending::clean_body(text);
    let mut lines: Vec<&str> = clean.split('\n').collect();
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    Ok(lines.join("\n"))
}

/// Why `ctrl-e` cannot open the editor now: the text is being posted.
pub(crate) fn external_editor_disabled(state: &AppState) -> Option<Reason> {
    state
        .composer_posting()
        .then(|| wait(state.review_words()).into())
}

/// `path`, made safe to draw.
fn shown(path: &Path) -> String {
    terminal_safe(path.as_os_str().as_encoded_bytes())
}

impl AppState {
    /// `ctrl-e`: hand the composer's text to the user's editor. Refused
    /// while the text is being posted, as typing is.
    pub(crate) fn edit_comment_externally(&mut self) -> Vec<Effect> {
        let Some(composer) = &self.review.composer else {
            return vec![];
        };
        if self.composer_posting() {
            self.notice = Some(wait(self.review_words()).into());
            return vec![];
        }
        let stamp = ComposerStamp {
            generation: self.generation,
            kind: composer.kind.clone(),
        };
        vec![Effect::HandOff(HandOff::EditReviewComment {
            initial: composer.text.text(),
            stamp,
        })]
    }

    /// The editor closed. For the composer that asked, the text replaces
    /// its own and is saved as `ctrl-s` saves it; for any other, nothing
    /// is saved or posted and the file is kept for the user.
    pub(crate) fn comment_edited(
        &mut self,
        stamp: ComposerStamp,
        result: Result<EditedComment, CommentEditError>,
    ) -> Vec<Effect> {
        let same = self.generation == stamp.generation
            && self.review.composer.as_ref().map(|c| &c.kind) == Some(&stamp.kind);
        let edited = match result {
            Err(CommentEditError::Launch(why)) => {
                self.push_error(ErrorPopup::new(
                    "Could not open the editor",
                    &why,
                    "Your comment is kept. Check core.editor, $VISUAL or $EDITOR, then try again.",
                ));
                return vec![];
            }
            Err(CommentEditError::Unreadable { why, file }) => {
                self.push_error(ErrorPopup::new(
                    "Comment not saved",
                    &format!(
                        "{why}; your comment was not saved. It is kept in {}.",
                        shown(&file.path)
                    ),
                    "The text in Kika is unchanged.",
                ));
                return vec![];
            }
            Ok(edited) if !same => {
                self.push_error(ErrorPopup::new(
                    "Comment not saved",
                    &format!(
                        "The review changed while the editor was open; your comment was not \
                         saved. It is kept in {}.",
                        shown(&edited.file.path)
                    ),
                    "Copy it from that file into a new comment.",
                ));
                return vec![];
            }
            Ok(edited) => edited,
        };
        let remove = Effect::RemoveCommentFile(edited.file);
        if edited.text.trim().is_empty() {
            self.notice = Some(EMPTY.into());
            return vec![remove];
        }
        if let Some(composer) = &mut self.review.composer {
            composer.text = CommentEditor::from_text(&edited.text);
        }
        let mut effects = self.save_comment();
        effects.push(remove);
        effects
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::confirm_question::Answer;
    use crate::features::review::ReviewIntent;
    use crate::features::review::comment_editor::Composer;
    use crate::features::review::pending::tests::{
        HEAD, comment, commenting, comments, right, saved, type_text, with_review,
    };
    use crate::features::review::state::tests::{press, type_char};
    use crate::intent::AppIntent;
    use crate::popup::Popup;
    use crossterm::event::KeyCode;
    use kika_forge::pending::{MAX_BODY, PendingReview};

    fn kept(name: &str) -> KeptFile {
        let dir = PathBuf::from(format!("/tmp/kika-comment-1-{name}"));
        KeptFile {
            path: dir.join(format!("kika-comment-1-{name}.md")),
            dir,
        }
    }

    /// The composer of a new line comment on R1 holding `text`.
    fn line_composer(text: &str) -> AppState {
        let mut state = commenting();
        type_char(&mut state, 'a');
        press(&mut state, KeyCode::Enter);
        type_text(&mut state, text);
        state
    }

    /// The one hand-off `effects` holds: its initial text and stamp.
    fn handed_off(effects: &[Effect]) -> (String, ComposerStamp) {
        match effects {
            [Effect::HandOff(HandOff::EditReviewComment { initial, stamp })] => {
                (initial.clone(), stamp.clone())
            }
            other => panic!("expected one editor hand-off, got {other:?}"),
        }
    }

    fn ctrl_e(state: &mut AppState) -> Vec<Effect> {
        crate::features::review::pending::tests::ctrl(state, 'e')
    }

    fn edited(
        state: &mut AppState,
        stamp: ComposerStamp,
        result: Result<EditedComment, CommentEditError>,
    ) -> Vec<Effect> {
        state.update(AppIntent::Review(ReviewIntent::CommentEdited {
            stamp,
            result,
        }))
    }

    fn ok(text: &str, file: &KeptFile) -> Result<EditedComment, CommentEditError> {
        Ok(EditedComment {
            text: text.into(),
            file: file.clone(),
        })
    }

    fn error(state: &AppState) -> Option<&ErrorPopup> {
        match state.popups.last() {
            Some(Popup::Error(e)) => Some(e),
            _ => None,
        }
    }

    fn question(state: &AppState) -> Option<(String, Answer)> {
        match state.popups.last() {
            Some(Popup::Question(q)) => Some((q.text.clone(), q.answer)),
            _ => None,
        }
    }

    fn text(state: &AppState) -> String {
        state.review.composer.as_ref().unwrap().text.text()
    }

    fn reply_composer(text: &str) -> AppState {
        let mut state = crate::features::review::discussion_write::tests::reading();
        type_char(&mut state, 'a');
        type_text(&mut state, text);
        state
    }

    fn general_composer(text: &str) -> AppState {
        let mut state = crate::features::review::discussion_write::tests::conversing();
        type_char(&mut state, 'a');
        type_text(&mut state, text);
        state
    }

    fn edit_composer() -> AppState {
        let mut state = with_review(PendingReview {
            comments: vec![comment(4, right(1), "first")],
        });
        type_char(&mut state, 'P');
        type_char(&mut state, 'e');
        state
    }

    #[test]
    fn ctrl_e_hands_the_text_and_a_stamp_to_the_editor_in_every_kind() {
        let states = [
            line_composer("draft"),
            edit_composer(),
            reply_composer("draft"),
            general_composer("draft"),
        ];
        for mut state in states {
            let composer = state.review.composer.clone().unwrap();
            let (initial, stamp) = handed_off(&ctrl_e(&mut state));
            assert_eq!(initial, composer.text.text());
            assert_eq!(
                stamp,
                ComposerStamp {
                    generation: state.generation,
                    kind: composer.kind.clone(),
                }
            );
            // Nothing changes until the editor closes.
            assert_eq!(state.review.composer, Some(composer));
        }
    }

    #[test]
    fn ctrl_e_waits_while_the_text_is_posted() {
        let mut state = reply_composer("ok");
        crate::features::review::pending::tests::ctrl(&mut state, 's');
        type_char(&mut state, 'y');
        assert!(state.composer_posting());
        assert_eq!(ctrl_e(&mut state), []);
        assert_eq!(
            state.notice.as_deref(),
            Some(crate::features::review::discussion_write::tests::WAIT)
        );
        // Asked directly, as a stale key could, it still waits.
        state.notice = None;
        assert_eq!(state.edit_comment_externally(), []);
        assert_eq!(
            state.notice.as_deref(),
            Some(crate::features::review::discussion_write::tests::WAIT)
        );
    }

    #[test]
    fn a_new_line_comment_from_the_editor_is_kept_and_the_file_removed() {
        let mut state = line_composer("draft");
        let (_, stamp) = handed_off(&ctrl_e(&mut state));
        let file = kept("line");
        let effects = edited(&mut state, stamp, ok("# Title\nbody", &file));
        let [save, Effect::RemoveCommentFile(removed)] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        assert_eq!(removed, &file);
        let (_, review) = saved(std::slice::from_ref(save));
        assert_eq!(review.comments.len(), 1);
        assert_eq!(review.comments[0].body, "# Title\nbody");
        assert_eq!(review.comments[0].head, HEAD);
        assert_eq!(state.review.composer, None);
    }

    #[test]
    fn an_edit_from_the_list_updates_that_pending_comment() {
        let mut state = edit_composer();
        let (initial, stamp) = handed_off(&ctrl_e(&mut state));
        assert_eq!(initial, "first");
        let file = kept("edit");
        let effects = edited(&mut state, stamp, ok("first, edited", &file));
        let [save, Effect::RemoveCommentFile(_)] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        let (_, review) = saved(std::slice::from_ref(save));
        assert_eq!(review.comments.len(), 1);
        assert_eq!(review.comments[0].id, 4);
        assert_eq!(comments(&state)[0].body, "first, edited");
        assert_eq!(state.review.composer, None);
    }

    #[test]
    fn a_reply_or_general_comment_from_the_editor_is_only_asked_about() {
        for (mut state, asked) in [
            (reply_composer("x"), "Post this reply to GitHub? (y/n)"),
            (
                general_composer("x"),
                "Post this comment on #12 to GitHub? (y/n)",
            ),
        ] {
            let (_, stamp) = handed_off(&ctrl_e(&mut state));
            let file = kept("post");
            let effects = edited(&mut state, stamp, ok("from the editor", &file));
            // No write: only the removal of the file.
            assert_eq!(effects, [Effect::RemoveCommentFile(file)]);
            assert_eq!(question(&state), Some((asked.to_string(), Answer::YOnly)));
            assert_eq!(text(&state), "from the editor");
        }
    }

    #[test]
    fn blank_text_from_the_editor_keeps_the_composer_and_its_text() {
        let mut state = line_composer("draft");
        let (_, stamp) = handed_off(&ctrl_e(&mut state));
        let file = kept("blank");
        let effects = edited(&mut state, stamp, ok(" \n\t", &file));
        assert_eq!(effects, [Effect::RemoveCommentFile(file)]);
        assert_eq!(state.notice.as_deref(), Some(EMPTY));
        assert_eq!(text(&state), "draft");
        assert_eq!(comments(&state), []);
    }

    #[test]
    fn over_long_text_keeps_the_composer_with_the_edited_text() {
        let mut state = line_composer("draft");
        let (_, stamp) = handed_off(&ctrl_e(&mut state));
        let long = "a".repeat(MAX_BODY + 1);
        let file = kept("long");
        let effects = edited(&mut state, stamp, ok(&long, &file));
        assert_eq!(effects, [Effect::RemoveCommentFile(file)]);
        assert_eq!(text(&state), long);
        assert_eq!(
            state.notice.as_deref(),
            Some(format!("A comment holds at most {} KiB", MAX_BODY / 1024).as_str())
        );
        assert_eq!(comments(&state), []);
    }

    #[test]
    fn an_editor_that_fails_to_start_says_so_and_keeps_the_text() {
        let mut state = line_composer("draft");
        let (_, stamp) = handed_off(&ctrl_e(&mut state));
        let effects = edited(
            &mut state,
            stamp,
            Err(CommentEditError::Launch("The editor exited with 3".into())),
        );
        assert_eq!(effects, []);
        let popup = error(&state).expect("an error popup");
        assert_eq!(popup.title, "Could not open the editor");
        assert_eq!(popup.message, "The editor exited with 3");
        assert!(popup.next.starts_with("Your comment is kept."), "{popup:?}");
        assert_eq!(text(&state), "draft");
        assert_eq!(comments(&state), []);
    }

    #[test]
    fn an_unreadable_file_is_kept_and_named_and_the_text_stays() {
        let mut state = reply_composer("draft");
        let (_, stamp) = handed_off(&ctrl_e(&mut state));
        let file = kept("bad");
        let effects = edited(
            &mut state,
            stamp,
            Err(CommentEditError::Unreadable {
                why: "The comment is not valid UTF-8".into(),
                file: file.clone(),
            }),
        );
        assert_eq!(effects, []);
        let popup = error(&state).expect("an error popup");
        assert!(
            popup.message.contains(&file.path.display().to_string()),
            "{popup:?}"
        );
        assert_eq!(text(&state), "draft");
        assert_eq!(question(&state), None);
    }

    /// The ways a composer can be gone or changed by the time the editor
    /// closes.
    fn changed() -> Vec<(&'static str, AppState, ComposerStamp)> {
        let mut out = Vec::new();
        let mut gone = line_composer("draft");
        let (_, stamp) = handed_off(&ctrl_e(&mut gone));
        gone.review.composer = None;
        out.push(("gone", gone, stamp));
        let mut bumped = line_composer("draft");
        let (_, stamp) = handed_off(&ctrl_e(&mut bumped));
        bumped.generation += 1;
        out.push(("generation", bumped, stamp));
        let mut moved = line_composer("draft");
        let (_, stamp) = handed_off(&ctrl_e(&mut moved));
        if let Some(Composer {
            kind: ComposerKind::Line { target, .. },
            ..
        }) = &mut moved.review.composer
        {
            target.head = "2".repeat(40);
        }
        out.push(("head", moved, stamp));
        let mut other = general_composer("draft");
        let (_, stamp) = handed_off(&ctrl_e(&mut other));
        if let Some(Composer {
            kind: ComposerKind::General { number, .. },
            ..
        }) = &mut other.review.composer
        {
            *number = 13;
        }
        out.push(("target", other, stamp));
        out
    }

    #[test]
    fn a_changed_composer_saves_nothing_and_names_the_kept_file() {
        for (what, mut state, stamp) in changed() {
            let before = state.review.composer.clone();
            let dir = PathBuf::from("/tmp/kika-comment-1-\u{1b}[2J");
            let file = KeptFile {
                path: dir.join("c.md"),
                dir,
            };
            let effects = edited(&mut state, stamp, ok("from the editor", &file));
            assert_eq!(effects, [], "{what}");
            assert_eq!(comments(&state), [], "{what}");
            assert_eq!(question(&state), None, "{what}");
            assert_eq!(state.review.composer, before, "{what}");
            let popup = error(&state).unwrap_or_else(|| panic!("{what}: no popup"));
            assert_eq!(popup.title, "Comment not saved", "{what}");
            assert!(
                popup
                    .message
                    .starts_with("The review changed while the editor was open"),
                "{what}: {popup:?}"
            );
            let safe = terminal_safe(file.path.as_os_str().as_encoded_bytes());
            assert!(popup.message.contains(&safe), "{what}: {popup:?}");
            assert!(!popup.message.contains('\u{1b}'), "{what}: {popup:?}");
        }
    }

    #[test]
    fn edited_text_is_cleaned_and_keeps_its_heading_lines() {
        assert_eq!(
            edited_comment(b"# Title\r\nbody\n\n\n"),
            Ok("# Title\nbody".into())
        );
        assert_eq!(
            edited_comment(b"a\x1b[2J\x07\tb\n## two\n  \n\t\n"),
            Ok("a[2J\tb\n## two".into())
        );
        // Blank lines inside stay; only the end is trimmed.
        assert_eq!(edited_comment(b"a\n\n\nb\n"), Ok("a\n\n\nb".into()));
        assert_eq!(edited_comment(b"\n \n"), Ok(String::new()));
        assert!(edited_comment(b"bad \xff").is_err());
    }
}
