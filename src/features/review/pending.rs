//! The pending review: line comments the reviewer has written on the open
//! pull or merge request and not submitted, which only they see. Read from
//! the state directory when the files screen opens (`LoadPendingReview`)
//! and written back after every change (`SavePendingReview`), one save at
//! a time so the file on disk is never older than what is shown. A stored
//! file Kika cannot vouch for is left alone: comments on that request are
//! then off, and nothing is written. Nothing reaches the forge until a
//! submit.

use super::comment_pick::numbered_in_view;
use super::discussion_write::wait;
use super::files::{FilesStage, ReviewView, files_screen};
use crate::confirm_question::{Answer, ConfirmQuestion, Then};
use crate::effect::{Effect, Tag};
use crate::features::review::ReviewIntent;
use crate::intent::AppIntent;
use crate::keymap::{KeyScope, Reason};
use crate::model::AppState;
use crate::popup::Popup;
use std::path::{Path, PathBuf};

use kika_forge::anchor::{GhLine, GhSide};
use kika_forge::forge::Words;
use kika_forge::pending::{
    MAX_BODY, MAX_COMMENTS, PendingComment, PendingError, PendingKey, PendingReview, RowPos,
};

const LOADING: &str = "The pending review is still loading";
const NO_PLACE: &str = "No place to keep a pending review for this repository";
const NOT_UTF8: &str = "Comments need a file name that is UTF-8";
const NO_ROW: &str = "No line of the diff in view to comment on";
const NO_COMMENTS: &str = "No pending comments";
const GONE: &str = "That comment is no longer in the pending review";
pub(super) const BLANK: &str = "Write something first";

fn files_not_ready(words: &Words) -> String {
    format!("Wait for the {}'s files to list", words.request)
}

fn in_commit(words: &Words) -> String {
    format!(
        "Comments are made on the whole {}: press c to go back",
        words.request
    )
}

pub(super) fn not_files(words: &Words) -> String {
    format!("Open the {}'s files first (f)", words.request)
}

fn full() -> String {
    format!("A pending review holds at most {MAX_COMMENTS} comments")
}

/// What is known of the stored pending review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingLoad {
    /// Being read under this tag; any other answer is dropped.
    Loading(Tag),
    Ready(PendingReview),
    /// The stored file, at this path when known, could not be read; it is
    /// never written.
    Unreadable(PendingError, Option<PathBuf>),
}

/// The pending review of pull request `number`, kept while its detail is
/// open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSlot {
    pub number: u64,
    pub key: PendingKey,
    pub load: PendingLoad,
    /// The save in flight; a change meanwhile waits for its answer.
    pub saving: Option<Tag>,
    /// Changed since the save in flight was sent.
    pub dirty: bool,
}

impl PendingSlot {
    /// The comments, once read.
    pub fn review(&self) -> Option<&PendingReview> {
        match &self.load {
            PendingLoad::Ready(review) => Some(review),
            _ => None,
        }
    }
}

/// Where a new comment goes: the pull request's file and lines, at the
/// head the files screen shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentTarget {
    pub path: String,
    pub start: Option<GhLine>,
    pub end: GhLine,
    pub head: String,
    /// Both line counters at `start` and `end`, kept for a forge that
    /// names a line by both sides.
    pub start_pos: Option<RowPos>,
    pub end_pos: Option<RowPos>,
}

impl CommentTarget {
    fn of(comment: &PendingComment) -> Self {
        Self {
            path: comment.path.clone(),
            start: comment.start,
            end: comment.end,
            head: comment.head.clone(),
            start_pos: comment.start_pos,
            end_pos: comment.end_pos,
        }
    }
}

/// `L12`, or `R10-R14` for a range: the side and line as GitHub has them.
pub fn lines_label(start: Option<GhLine>, end: GhLine) -> String {
    let one = |l: GhLine| {
        let side = match l.side {
            GhSide::Left => 'L',
            GhSide::Right => 'R',
        };
        format!("{side}{}", l.line)
    };
    match start {
        Some(start) => format!("{}-{}", one(start), one(end)),
        None => one(end),
    }
}

/// Why comments are off, naming the file so the user can remove it.
fn unreadable(number: u64, why: PendingError, file: Option<&Path>) -> Reason {
    let lead = format!(
        "The saved pending review for #{number} could not be read ({why}); it was left as it is"
    );
    match file {
        Some(file) => Reason::with_path(
            &format!("{lead}. To start again, remove "),
            &file.to_string_lossy(),
        ),
        None => lead.into(),
    }
}

/// Why the pending review cannot be changed now, from its slot.
pub(super) fn slot_disabled(state: &AppState) -> Result<&PendingReview, Reason> {
    let slot = state.review.pending.as_ref().ok_or(NO_PLACE)?;
    match &slot.load {
        PendingLoad::Loading(_) => Err(LOADING.into()),
        PendingLoad::Unreadable(why, file) => Err(unreadable(slot.number, *why, file.as_deref())),
        PendingLoad::Ready(review) => Ok(review),
    }
}

/// Why `a` cannot start a comment, the first reason that applies: the
/// files are not listed, a commit is shown, the pending review is not
/// ready, the file's name is not UTF-8, no numbered row is in view, or
/// the review is full.
pub(crate) fn comment_disabled(state: &AppState) -> Option<Reason> {
    let words = state.review_words();
    let Some(files) =
        files_screen(state).filter(|f| f.stage == FilesStage::Ready && state.review_files_shown())
    else {
        return Some(files_not_ready(words).into());
    };
    if matches!(files.view, ReviewView::Commit(_)) {
        return Some(in_commit(words).into());
    }
    let review = match slot_disabled(state) {
        Ok(review) => review,
        Err(reason) => return Some(reason),
    };
    if state.review.submitting() {
        return Some(wait(words).into());
    }
    let utf8 = files
        .selected()
        .is_some_and(|row| std::str::from_utf8(row.file.path.as_bytes()).is_ok());
    if !utf8 {
        return Some(NOT_UTF8.into());
    }
    if numbered_in_view(state).is_none() {
        return Some(NO_ROW.into());
    }
    (review.comments.len() >= MAX_COMMENTS).then(|| full().into())
}

/// Why `P` cannot list the pending comments.
pub(crate) fn pending_list_disabled(state: &AppState) -> Option<Reason> {
    if files_screen(state).is_none() || !state.review_files_shown() {
        return Some(not_files(state.review_words()).into());
    }
    match slot_disabled(state) {
        Err(reason) => Some(reason),
        Ok(review) => review.comments.is_empty().then_some(NO_COMMENTS.into()),
    }
}

/// The pending comments the list shows, while it is open.
pub(crate) fn pending_list_open(state: &AppState) -> bool {
    state.review_files_shown() && state.review.pending_list.is_some()
}

impl AppState {
    /// Read pull request `number`'s pending review, unless it is the one
    /// held already. Without a repository or a UTF-8 top level there is
    /// nowhere to keep one, and comments stay off.
    pub(super) fn load_pending_review(&mut self, number: u64) -> Vec<Effect> {
        if self
            .review
            .pending
            .as_ref()
            .is_some_and(|s| s.number == number)
        {
            return vec![];
        }
        let mut effects = self.drop_pending_slot();
        let key = self
            .review_repo()
            .zip(self.repo.as_deref())
            .and_then(|(repo, top)| PendingKey::new(&repo, top, number));
        let Some(key) = key else {
            return effects;
        };
        let tag = self.review_tag();
        self.review.pending = Some(PendingSlot {
            number,
            key: key.clone(),
            load: PendingLoad::Loading(tag),
            saving: None,
            dirty: false,
        });
        effects.push(Effect::LoadPendingReview { tag, key });
        effects
    }

    /// Forget the pending review held, once its last change is on its way
    /// to disk: a change still waiting on a save in flight is sent now.
    pub(super) fn drop_pending_slot(&mut self) -> Vec<Effect> {
        self.review.pending_list = None;
        self.review.composer = None;
        let Some(slot) = self.review.pending.take() else {
            return vec![];
        };
        match slot.load {
            PendingLoad::Ready(review) if slot.dirty => {
                let tag = self.review_tag();
                vec![Effect::SavePendingReview {
                    tag,
                    key: slot.key,
                    review,
                }]
            }
            _ => vec![],
        }
    }

    pub(crate) fn pending_loaded(
        &mut self,
        tag: Tag,
        result: Result<Option<PendingReview>, PendingError>,
        file: Option<PathBuf>,
    ) {
        let Some(slot) = self.review.pending.as_mut() else {
            return;
        };
        if slot.load != PendingLoad::Loading(tag) || tag.generation != self.generation {
            return;
        }
        slot.load = match result {
            Ok(found) => PendingLoad::Ready(found.unwrap_or_default()),
            Err(why) => {
                self.notice = Some(unreadable(slot.number, why, file.as_deref()));
                PendingLoad::Unreadable(why, file)
            }
        };
    }

    pub(crate) fn pending_saved(&mut self, tag: Tag, result: Result<(), String>) -> Vec<Effect> {
        if tag.generation != self.generation {
            return vec![];
        }
        if let Err(message) = result {
            // The comments stay in memory; the next change tries again.
            self.notice = Some(format!("The pending review was not saved: {message}").into());
        }
        let Some(slot) = self.review.pending.as_mut() else {
            return vec![];
        };
        if slot.saving != Some(tag) {
            return vec![];
        }
        slot.saving = None;
        if !std::mem::take(&mut slot.dirty) {
            return vec![];
        }
        self.save_pending()
    }

    /// Write the pending review as it is now, after the save in flight.
    fn save_pending(&mut self) -> Vec<Effect> {
        let tag = self.review_tag();
        let Some(slot) = self.review.pending.as_mut() else {
            return vec![];
        };
        let PendingLoad::Ready(review) = &slot.load else {
            return vec![];
        };
        if slot.saving.is_some() {
            slot.dirty = true;
            return vec![];
        }
        slot.saving = Some(tag);
        vec![Effect::SavePendingReview {
            tag,
            key: slot.key.clone(),
            review: review.clone(),
        }]
    }

    /// Why a new comment (`editing` is `None`) or a change to comment
    /// `editing` cannot be kept now.
    pub(super) fn pending_change_refused(&self, editing: Option<u64>) -> Option<Reason> {
        let review = match slot_disabled(self) {
            Ok(review) => review,
            Err(reason) => return Some(reason),
        };
        // The review on its way to GitHub was made from these comments.
        if self.review.submitting() {
            return Some(wait(self.review_words()).into());
        }
        match editing {
            Some(id) => (!review.comments.iter().any(|c| c.id == id)).then(|| GONE.into()),
            None => (review.comments.len() >= MAX_COMMENTS).then(|| full().into()),
        }
    }

    /// The comments, to change, once read and while no review made from
    /// them is on its way; else why not, in the notice.
    fn pending_review_mut(&mut self) -> Option<&mut PendingReview> {
        if let Err(reason) = slot_disabled(self) {
            self.notice = Some(reason);
            return None;
        }
        if self.refused_while_submitting() {
            return None;
        }
        match self.review.pending.as_mut().map(|s| &mut s.load) {
            Some(PendingLoad::Ready(review)) => Some(review),
            _ => None,
        }
    }

    /// Add a comment on `target`, with a new id, and save.
    pub(super) fn add_pending(&mut self, target: CommentTarget, body: String) -> Vec<Effect> {
        let Some(review) = self.pending_review_mut() else {
            return vec![];
        };
        if review.comments.len() >= MAX_COMMENTS {
            self.notice = Some(full().into());
            return vec![];
        }
        let id = review.comments.iter().map(|c| c.id).max().unwrap_or(0) + 1;
        review.comments.push(PendingComment {
            id,
            path: target.path,
            start: target.start,
            end: target.end,
            body,
            head: target.head,
            start_pos: target.start_pos,
            end_pos: target.end_pos,
        });
        self.save_pending()
    }

    /// Replace comment `id`'s text, keeping its id and lines, and save.
    pub(super) fn update_pending(&mut self, id: u64, body: String) -> Vec<Effect> {
        let Some(review) = self.pending_review_mut() else {
            return vec![];
        };
        let Some(comment) = review.comments.iter_mut().find(|c| c.id == id) else {
            self.notice = Some(GONE.into());
            return vec![];
        };
        comment.body = body;
        self.save_pending()
    }

    /// The yes of "Delete this comment?": remove comment `id`, checked
    /// again, and save. The list closes once it is empty.
    pub(crate) fn delete_pending(&mut self, id: u64) -> Vec<Effect> {
        let Some(review) = self.pending_review_mut() else {
            return vec![];
        };
        let Some(at) = review.comments.iter().position(|c| c.id == id) else {
            self.notice = Some(GONE.into());
            return vec![];
        };
        review.comments.remove(at);
        let left = review.comments.len();
        self.review.pending_list = match self.review.pending_list {
            _ if left == 0 => None,
            Some(cursor) => Some(cursor.min(left - 1)),
            None => None,
        };
        self.save_pending()
    }

    /// A review made from the pending comments is on its way to GitHub,
    /// so they may not change: say so.
    fn refused_while_submitting(&mut self) -> bool {
        let submitting = self.review.submitting();
        if submitting {
            self.notice = Some(wait(self.review_words()).into());
        }
        submitting
    }

    /// Take the comments a submitted review carried out of pull request
    /// `number`'s pending review, by id, and save what is left. Comments
    /// it did not carry stay.
    pub(super) fn remove_sent_pending(
        &mut self,
        number: u64,
        sent: &[PendingComment],
    ) -> Vec<Effect> {
        let review = match self.review.pending.as_mut() {
            Some(PendingSlot {
                number: held,
                load: PendingLoad::Ready(review),
                ..
            }) if *held == number => review,
            _ => return vec![],
        };
        review
            .comments
            .retain(|c| !sent.iter().any(|s| s.id == c.id));
        self.save_pending()
    }

    /// `P`: list the pending comments.
    pub(crate) fn open_pending_list(&mut self) {
        if let Some(reason) = pending_list_disabled(self) {
            self.notice = Some(reason);
            return;
        }
        self.review.pending_list = Some(0);
    }

    pub(crate) fn close_pending_list(&mut self) {
        self.review.pending_list = None;
    }

    /// The comment under the list's cursor.
    fn listed_comment(&self) -> Option<&PendingComment> {
        let cursor = self.review.pending_list?;
        self.review.pending.as_ref()?.review()?.comments.get(cursor)
    }

    /// `j`/`k` in the list, kept within it.
    pub(crate) fn move_pending_list(&mut self, delta: isize) {
        let len = self
            .review
            .pending
            .as_ref()
            .and_then(PendingSlot::review)
            .map_or(0, |r| r.comments.len());
        if let Some(cursor) = &mut self.review.pending_list {
            *cursor = cursor
                .saturating_add_signed(delta)
                .min(len.saturating_sub(1));
        }
    }

    /// `enter`/`e` in the list: the comment's text in the composer.
    pub(crate) fn edit_pending(&mut self) {
        if self.refused_while_submitting() {
            return;
        }
        let Some(comment) = self.listed_comment() else {
            return;
        };
        let composer = super::comment_editor::Composer {
            kind: super::comment_editor::ComposerKind::Line {
                target: CommentTarget::of(comment),
                editing: Some(comment.id),
            },
            text: super::comment_editor::CommentEditor::from_text(&comment.body),
        };
        self.review.composer = Some(composer);
    }

    /// `d` in the list: ask before deleting the comment under the cursor.
    pub(crate) fn ask_delete_pending(&mut self) {
        if self.refused_while_submitting() {
            return;
        }
        let Some(comment) = self.listed_comment() else {
            return;
        };
        let what = lines_label(comment.start, comment.end);
        let id = comment.id;
        let path = kika_git::safe_text::sanitize(comment.path.as_bytes());
        self.popups.push(Popup::Question(ConfirmQuestion {
            scope: KeyScope::ReviewCommentConfirm,
            title: "Delete comment".into(),
            text: format!("Delete this comment? {path}:{what} (y/n)"),
            answer: Answer::YOnly,
            then: Then::Intent(Box::new(AppIntent::Review(ReviewIntent::DeletePending(id)))),
            range: false,
            head: None,
        }));
    }

    /// The text of a comment about to be stored, or why it may not be.
    pub(super) fn comment_body(&mut self, text: &str) -> Option<String> {
        let body = kika_forge::pending::clean_body(text);
        if body.trim().is_empty() {
            self.notice = Some(BLANK.into());
            return None;
        }
        if body.len() > MAX_BODY {
            self.notice = Some(format!("A comment holds at most {} KiB", MAX_BODY / 1024).into());
            return None;
        }
        Some(body)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use crossterm::event::{KeyCode, KeyModifiers};

    use super::*;
    use crate::features::review::pending_view::no_discussion;
    use crate::features::review::state::tests::{press, type_char};
    use crate::features::review::threads::tests::{chunk, files_with};
    use crate::keymap::KeyPress;
    use kika_git::diff::{Classifier, DiffLine, EmojiZwj};

    pub(crate) const HEAD: &str = "1111111111111111111111111111111111111111";

    /// Two hunks of `src/cache.rs`; the first numbered row is `+fn new() {}`.
    /// Rows: 0-2 file header, 3 `@@`, 4 `+fn new` (R1), 5 context (L1/R2),
    /// 6 `-fn old` (L2), 7 `+fn newer` (R3), 8 `@@`, 9 context (L10/R11),
    /// 10 `-b` (L11), 11 `+c` (R12).
    pub(crate) fn two_hunks() -> Vec<DiffLine> {
        let mut classifier = Classifier::new(EmojiZwj::Escape);
        [
            "diff --git a/src/cache.rs b/src/cache.rs",
            "--- a/src/cache.rs",
            "+++ b/src/cache.rs",
            "@@ -1,2 +1,3 @@",
            "+fn new() {}",
            " use std::fmt;",
            "-fn old() {}",
            "+fn newer() {}",
            "@@ -10,2 +11,2 @@",
            " a",
            "-b",
            "+c",
        ]
        .iter()
        .map(|t| classifier.classify(t.as_bytes()))
        .collect()
    }

    pub(crate) fn key() -> PendingKey {
        PendingKey {
            host: "github.com".into(),
            owner: "acme".into(),
            repo: "widgets".into(),
            number: 12,
            top: "/r".into(),
        }
    }

    /// The files screen of pull request 12 with `two_hunks` shown and its
    /// pending review not yet read; the load's tag.
    pub(crate) fn loading() -> (AppState, Tag) {
        let mut state = files_with(&["src/cache.rs", "src/lib.rs"], no_discussion());
        state.size = (100, 30);
        chunk(&mut state, two_hunks(), true);
        let Some(PendingSlot {
            load: PendingLoad::Loading(tag),
            ..
        }) = state.review.pending
        else {
            panic!("{:?}", state.review.pending);
        };
        (state, tag)
    }

    /// [`loading`], with `review` read.
    pub(crate) fn with_review(review: PendingReview) -> AppState {
        let (mut state, tag) = loading();
        loaded(&mut state, tag, Ok(Some(review)));
        state
    }

    /// [`loading`], with no pending review stored.
    pub(crate) fn commenting() -> AppState {
        with_review(PendingReview::default())
    }

    pub(crate) fn loaded(
        state: &mut AppState,
        tag: Tag,
        result: Result<Option<PendingReview>, PendingError>,
    ) -> Vec<Effect> {
        state.update(AppIntent::Review(ReviewIntent::PendingLoaded {
            tag,
            result,
            file: Some(PathBuf::from("/s/kika/reviews/0123456789abcdef.json")),
        }))
    }

    pub(crate) fn ctrl(state: &mut AppState, c: char) -> Vec<Effect> {
        state.update(AppIntent::Key(KeyPress::new(
            KeyCode::Char(c),
            KeyModifiers::CONTROL,
        )))
    }

    pub(crate) fn type_text(state: &mut AppState, text: &str) {
        for c in text.chars() {
            assert_eq!(type_char(state, c), [], "{c:?}");
        }
    }

    pub(crate) fn comments(state: &AppState) -> Vec<PendingComment> {
        state
            .review
            .pending
            .as_ref()
            .and_then(PendingSlot::review)
            .map(|r| r.comments.clone())
            .unwrap_or_default()
    }

    /// The one save `effects` holds, with nothing else.
    pub(crate) fn saved(effects: &[Effect]) -> (Tag, PendingReview) {
        match effects {
            [
                Effect::SavePendingReview {
                    tag,
                    key: k,
                    review,
                },
            ] => {
                assert_eq!(k, &key());
                (*tag, review.clone())
            }
            other => panic!("expected one save and nothing else, got {other:?}"),
        }
    }

    pub(crate) fn right(line: u32) -> GhLine {
        GhLine {
            side: GhSide::Right,
            line,
        }
    }

    pub(crate) fn left(line: u32) -> GhLine {
        GhLine {
            side: GhSide::Left,
            line,
        }
    }

    pub(crate) fn comment(id: u64, end: GhLine, body: &str) -> PendingComment {
        PendingComment {
            id,
            path: "src/cache.rs".into(),
            start: None,
            end,
            body: body.into(),
            head: HEAD.into(),
            start_pos: None,
            end_pos: None,
        }
    }

    /// `a`, `enter`, `text` and `ctrl-s`: the effects of the save.
    fn write_comment(state: &mut AppState, text: &str) -> Vec<Effect> {
        assert_eq!(type_char(state, 'a'), []);
        assert_eq!(press(state, KeyCode::Enter), []);
        type_text(state, text);
        ctrl(state, 's')
    }

    #[test]
    fn on_gitlab_the_reasons_say_merge_request() {
        let mut state = crate::features::review::state::tests::on_gitlab(commenting());
        state.review.files.as_mut().unwrap().view = ReviewView::Commit(0);
        assert_eq!(
            comment_disabled(&state).map(String::from).as_deref(),
            Some("Comments are made on the whole merge request: press c to go back")
        );
        press(&mut state, KeyCode::Esc);
        assert_eq!(
            pending_list_disabled(&state).map(String::from).as_deref(),
            Some("Open the merge request's files first (f)")
        );
    }

    #[test]
    fn opening_the_files_reads_that_pull_requests_pending_review() {
        let mut state = crate::features::review::state::tests::in_detail(Vec::new());
        let effects = type_char(&mut state, 'f');
        let [
            Effect::CheckReviewCommits { .. },
            Effect::LoadPendingReview { tag, key: k },
        ] = effects.as_slice()
        else {
            panic!("{effects:?}");
        };
        assert_eq!(k, &key());
        assert_eq!(
            state.review.pending.as_ref().unwrap().load,
            PendingLoad::Loading(*tag)
        );
        // Back to the detail and in again: the review held is kept.
        press(&mut state, KeyCode::Esc);
        let effects = type_char(&mut state, 'f');
        assert!(matches!(
            effects.as_slice(),
            [Effect::CheckReviewCommits { .. }]
        ));
        // Back to the queue: it is forgotten.
        press(&mut state, KeyCode::Esc);
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.review.pending, None);
    }

    #[test]
    fn a_line_on_the_new_side_is_kept_in_the_pending_review_and_only_saved() {
        let mut state = commenting();
        assert_eq!(type_char(&mut state, 'a'), []);
        assert_eq!(press(&mut state, KeyCode::Enter), []);
        assert_eq!(state.key_focus(), KeyScope::ReviewComment);
        type_text(&mut state, "h");
        // Control characters, and ctrl keys, type nothing.
        assert_eq!(ctrl(&mut state, 'x'), []);
        assert_eq!(press(&mut state, KeyCode::Char('\u{1b}')), []);
        type_text(&mut state, "i");
        let effects = ctrl(&mut state, 's');
        let (_, review) = saved(&effects);
        let want = PendingComment {
            id: 1,
            path: "src/cache.rs".into(),
            start: None,
            end: right(1),
            body: "hi".into(),
            head: HEAD.into(),
            start_pos: None,
            // The added row's counters: the old one is the next old line.
            end_pos: Some(RowPos {
                old: 1,
                new: 1,
                both: false,
            }),
        };
        assert_eq!(review.comments, std::slice::from_ref(&want));
        assert_eq!(comments(&state), [want]);
        assert_eq!(state.review.composer, None);
        assert_eq!(state.review.files.as_ref().unwrap().pick, None);
    }

    #[test]
    fn blank_text_is_refused_and_the_composer_stays() {
        let mut state = commenting();
        type_char(&mut state, 'a');
        press(&mut state, KeyCode::Enter);
        type_text(&mut state, "  ");
        press(&mut state, KeyCode::Enter);
        assert_eq!(ctrl(&mut state, 's'), []);
        assert_eq!(state.notice.as_deref(), Some(BLANK));
        assert!(state.review.composer.is_some());
        assert_eq!(comments(&state), []);
    }

    #[test]
    fn a_stale_load_is_ignored_and_a_read_review_shows() {
        let (mut state, tag) = loading();
        let stale = Tag {
            seq: tag.seq + 100,
            ..tag
        };
        let review = PendingReview {
            comments: vec![comment(4, right(3), "hi")],
        };
        assert_eq!(loaded(&mut state, stale, Ok(Some(review.clone()))), []);
        assert_eq!(
            state.review.pending.as_ref().unwrap().load,
            PendingLoad::Loading(tag)
        );
        assert_eq!(loaded(&mut state, tag, Ok(Some(review.clone()))), []);
        assert_eq!(comments(&state), review.comments);
        // A new id is the largest plus one.
        let (_, saved_review) = saved(&write_comment(&mut state, "next"));
        assert_eq!(saved_review.comments[1].id, 5);
    }

    #[test]
    fn a_change_while_a_save_runs_is_sent_after_it_with_the_latest_review() {
        let mut state = commenting();
        let (first, review) = saved(&write_comment(&mut state, "one"));
        assert_eq!(review.comments.len(), 1);
        // The second waits for the first.
        assert_eq!(write_comment(&mut state, "two"), []);
        assert!(state.review.pending.as_ref().unwrap().dirty);
        let (second, review) =
            saved(&state.update(AppIntent::Review(ReviewIntent::PendingSaved {
                tag: first,
                result: Ok(()),
            })));
        assert_ne!(second, first);
        let bodies: Vec<&str> = review.comments.iter().map(|c| c.body.as_str()).collect();
        assert_eq!(bodies, ["one", "two"]);
        // Its answer leaves nothing more to save.
        let done = state.update(AppIntent::Review(ReviewIntent::PendingSaved {
            tag: second,
            result: Ok(()),
        }));
        assert_eq!(done, []);
        assert_eq!(state.review.pending.as_ref().unwrap().saving, None);
    }

    #[test]
    fn a_failed_save_says_why_and_keeps_the_comments() {
        let mut state = commenting();
        let (tag, _) = saved(&write_comment(&mut state, "one"));
        let effects = state.update(AppIntent::Review(ReviewIntent::PendingSaved {
            tag,
            result: Err("disk full".into()),
        }));
        assert_eq!(effects, []);
        assert_eq!(
            state.notice.as_deref(),
            Some("The pending review was not saved: disk full")
        );
        assert_eq!(comments(&state).len(), 1);
        // The next change tries again.
        saved(&write_comment(&mut state, "two"));
    }

    #[test]
    fn leaving_with_a_change_still_waiting_sends_it() {
        let mut state = commenting();
        saved(&write_comment(&mut state, "one"));
        assert_eq!(write_comment(&mut state, "two"), []);
        press(&mut state, KeyCode::Esc);
        let effects = press(&mut state, KeyCode::Esc);
        let review = effects
            .iter()
            .find_map(|e| match e {
                Effect::SavePendingReview { review, .. } => Some(review.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{effects:?}"));
        assert_eq!(review.comments.len(), 2);
        assert_eq!(state.review.pending, None);
    }

    #[test]
    fn an_unreadable_file_turns_comments_off_and_is_never_saved() {
        let (mut state, tag) = loading();
        let why = PendingError::Unreadable("it is a symbolic link");
        assert_eq!(loaded(&mut state, tag, Err(why)), []);
        let file = "/s/kika/reviews/0123456789abcdef.json";
        let reason = unreadable(12, why, Some(Path::new(file)));
        assert_eq!(
            &*reason,
            "The saved pending review for #12 could not be read (it is a symbolic link); \
             it was left as it is. To start again, remove /s/kika/reviews/0123456789abcdef.json"
        );
        // The path is marked, so a narrow bar keeps the file's name.
        assert_eq!(reason.path_split().map(|(_, path)| path), Some(file));
        assert_eq!(state.notice.as_ref(), Some(&reason));
        for key in ['a', 'P'] {
            assert_eq!(type_char(&mut state, key), [], "{key}");
            assert_eq!(state.notice.as_ref(), Some(&reason), "{key}");
        }
        assert_eq!(comment_disabled(&state), Some(reason.clone()));
        assert_eq!(pending_list_disabled(&state), Some(reason.clone()));
        // Without a store there is no file to name.
        let bare = unreadable(12, why, None);
        assert!(bare.ends_with("it was left as it is"));
        // A hostile store path is drawn safe.
        let hostile = unreadable(12, why, Some(Path::new("/s/\u{1b}[2J.json")));
        assert!(!hostile.contains('\u{1b}'));
        // Nothing reaches the store, whatever asks.
        for intent in [ReviewIntent::DeletePending(1), ReviewIntent::SaveComment] {
            assert_eq!(state.update(AppIntent::Review(intent)), []);
        }
        let target = CommentTarget {
            path: "src/cache.rs".into(),
            start: None,
            end: right(1),
            head: HEAD.into(),
            start_pos: None,
            end_pos: None,
        };
        assert_eq!(state.add_pending(target, "x".into()), []);
        assert_eq!(state.update_pending(1, "x".into()), []);
    }

    #[test]
    fn a_narrow_bar_names_the_unreadable_file_to_remove() {
        let name = "0123456789abcdef.json";
        let file = format!("/Users/someone/Library/Application Support/kika/reviews/{name}");
        for why in [
            "it is a symbolic link",
            "it is not a pending review Kika wrote",
        ] {
            let (mut state, tag) = loading();
            state.update(AppIntent::Review(ReviewIntent::PendingLoaded {
                tag,
                result: Err(PendingError::Unreadable(why)),
                file: Some(PathBuf::from(&file)),
            }));
            assert_eq!(type_char(&mut state, 'a'), [], "{why}");
            for width in [80u16, 95] {
                let bar = &crate::test_support::screen::draw(&state, width, 24)[23];
                assert!(bar.trim_end().ends_with(name), "{why} at {width}: {bar:?}");
                assert!(
                    bar.starts_with(" The saved pending review"),
                    "{why} at {width}: {bar:?}"
                );
            }
        }
    }

    #[test]
    fn p_lists_e_edits_in_place_keeping_the_id_and_lines() {
        let range = PendingComment {
            start: Some(left(2)),
            ..comment(3, right(3), "first")
        };
        let mut state = with_review(PendingReview {
            comments: vec![comment(1, right(1), "zero"), range.clone()],
        });
        assert_eq!(type_char(&mut state, 'P'), []);
        assert_eq!(state.review.pending_list, Some(0));
        assert_eq!(state.key_focus(), KeyScope::ReviewPending);
        assert_eq!(type_char(&mut state, 'j'), []);
        assert_eq!(type_char(&mut state, 'e'), []);
        assert_eq!(state.key_focus(), KeyScope::ReviewComment);
        let composer = state.review.composer.as_ref().unwrap();
        assert!(matches!(
            composer.kind,
            crate::features::review::comment_editor::ComposerKind::Line {
                editing: Some(3),
                ..
            }
        ));
        assert_eq!(composer.text.text(), "first");
        type_text(&mut state, " and more");
        let (_, review) = saved(&ctrl(&mut state, 's'));
        let edited = PendingComment {
            body: "first and more".into(),
            ..range
        };
        assert_eq!(review.comments[1], edited);
        assert_eq!(comments(&state)[1], edited);
        // Back in the list, which stays open.
        assert_eq!(state.key_focus(), KeyScope::ReviewPending);
    }

    #[test]
    fn d_asks_and_only_y_deletes_and_the_last_one_closes_the_list() {
        let mut state = with_review(PendingReview {
            comments: vec![comment(1, right(1), "one"), comment(2, right(3), "two")],
        });
        type_char(&mut state, 'P');
        type_char(&mut state, 'j');
        for keep in [KeyCode::Char('n'), KeyCode::Esc, KeyCode::Enter] {
            assert_eq!(type_char(&mut state, 'd'), []);
            let Some(Popup::Question(q)) = state.popups.last() else {
                panic!("no question");
            };
            assert!(q.text.starts_with("Delete this comment?"), "{}", q.text);
            assert_eq!(q.scope, KeyScope::ReviewCommentConfirm);
            assert_eq!(press(&mut state, keep), [], "{keep:?}");
            assert_eq!(comments(&state).len(), 2, "{keep:?}");
            // Enter answers nothing: the question is still there.
            if keep == KeyCode::Enter {
                assert!(matches!(state.popups.last(), Some(Popup::Question(_))));
                state.popups.clear();
            }
        }
        type_char(&mut state, 'd');
        let (tag, review) = saved(&type_char(&mut state, 'y'));
        assert_eq!(review.comments, [comment(1, right(1), "one")]);
        assert_eq!(state.review.pending_list, Some(0));
        state.update(AppIntent::Review(ReviewIntent::PendingSaved {
            tag,
            result: Ok(()),
        }));
        type_char(&mut state, 'd');
        let (tag, review) = saved(&type_char(&mut state, 'y'));
        assert_eq!(review, PendingReview::default());
        assert_eq!(state.review.pending_list, None);
        // A stale yes finds nothing to delete.
        state.update(AppIntent::Review(ReviewIntent::PendingSaved {
            tag,
            result: Ok(()),
        }));
        assert_eq!(
            state.update(AppIntent::Review(ReviewIntent::DeletePending(1))),
            []
        );
        assert_eq!(state.notice.as_deref(), Some(GONE));
    }

    #[test]
    fn esc_in_a_composer_with_text_asks_first_and_a_blank_one_closes_at_once() {
        let mut state = commenting();
        type_char(&mut state, 'a');
        press(&mut state, KeyCode::Enter);
        assert_eq!(press(&mut state, KeyCode::Esc), []);
        assert_eq!(state.review.composer, None);

        type_char(&mut state, 'a');
        press(&mut state, KeyCode::Enter);
        type_text(&mut state, "draft");
        assert_eq!(press(&mut state, KeyCode::Esc), []);
        let Some(Popup::Question(q)) = state.popups.last() else {
            panic!("no question");
        };
        assert_eq!(q.text, "Discard this comment? (y/n)");
        assert_eq!(type_char(&mut state, 'n'), []);
        assert_eq!(state.review.composer.as_ref().unwrap().text.text(), "draft");
        press(&mut state, KeyCode::Esc);
        assert_eq!(type_char(&mut state, 'y'), []);
        assert_eq!(state.review.composer, None);
        assert_eq!(comments(&state), []);
    }

    #[test]
    fn each_reason_a_is_off_shows_in_the_notice() {
        use crate::features::review::files::ReviewView;
        let notice = |state: &mut AppState| {
            assert_eq!(type_char(state, 'a'), []);
            assert_eq!(state.review.files.as_ref().unwrap().pick, None);
            state.notice.as_deref().map(str::to_string)
        };
        // Files not listed yet.
        let mut state = crate::features::review::state::tests::in_detail(Vec::new());
        type_char(&mut state, 'f');
        assert_eq!(
            notice(&mut state).as_deref(),
            Some("Wait for the pull request's files to list")
        );
        // A commit view.
        let mut state = commenting();
        state.review.files.as_mut().unwrap().view = ReviewView::Commit(0);
        assert_eq!(
            notice(&mut state).as_deref(),
            Some("Comments are made on the whole pull request: press c to go back")
        );
        // Still loading.
        let (mut state, _) = loading();
        assert_eq!(notice(&mut state).as_deref(), Some(LOADING));
        // A name that is not UTF-8.
        // Windows file names are UTF-16, so a name that is not UTF-8 cannot exist there.
        #[cfg(unix)]
        {
            let mut state = commenting();
            state.review.files.as_mut().unwrap().rows[0].file.path =
                kika_git::path::RepoPath::from_bytes(b"bad\xff.rs");
            assert_eq!(notice(&mut state).as_deref(), Some(NOT_UTF8));
        }
        // No numbered row in view.
        let mut state = commenting();
        state.diff.lines.truncate(4);
        assert_eq!(notice(&mut state).as_deref(), Some(NO_ROW));
        // The cap reached.
        let full_review = PendingReview {
            comments: (1..=MAX_COMMENTS as u64)
                .map(|id| comment(id, right(1), "x"))
                .collect(),
        };
        let mut state = with_review(full_review);
        assert_eq!(notice(&mut state), Some(full()));
    }
}
