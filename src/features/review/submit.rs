//! Submitting the pending review (`S` on the files screen or in `P`): pick
//! Approve, Request changes or Comment, write a summary in the composer,
//! and answer the one `y` question. GitHub gets the summary and every
//! pending comment as one review. A failure leaves every draft where it
//! was; a success empties the pending review and reads the discussion
//! again, so the new threads show as posted.

use super::comment_editor::{Composer, ComposerKind};
use super::discussion_write::{POSTING, WAIT};
use super::files::files_screen;
use super::pending::{BLANK, NOT_FILES, slot_disabled};
use super::state::ReviewState;
use crate::effect::Effect;
use crate::keymap::Reason;
use crate::model::AppState;
use crate::pick_list::{PickList, PickPurpose};
use crate::popup::Popup;
use kika_forge::discussion_write::{DiscussionWrite, ReviewEvent};
use kika_forge::pending::{MAX_BODY, PendingComment};
use kika_forge::{ForgeHost, ForgeRepo};

const NO_PR_ID: &str = "GitHub sent no pull request id; refresh with r";
const NO_HEAD: &str = "GitHub sent no head commit for this pull request; refresh with r";

/// The rows of the review-type pick, in order.
const EVENTS: [ReviewEvent; 3] = [
    ReviewEvent::Approve,
    ReviewEvent::RequestChanges,
    ReviewEvent::Comment,
];

/// "1 comment", "3 comments".
pub(super) fn comments_label(count: usize) -> String {
    match count {
        1 => "1 comment".to_string(),
        n => format!("{n} comments"),
    }
}

/// What a review would be sent with now: where it goes, the pull request
/// and its node id, the head the comments share, and the comments.
struct SubmitTarget {
    host: ForgeHost,
    repo: ForgeRepo,
    number: u64,
    pull: String,
    commit: String,
    threads: Vec<PendingComment>,
}

/// Why `S` cannot submit the pending review now.
pub(crate) fn submit_disabled(state: &AppState) -> Option<Reason> {
    state.submit_target().err()
}

impl ReviewState {
    /// A review is on its way to GitHub: the pending review it was made
    /// from may not change until GitHub answers.
    pub(super) fn submitting(&self) -> bool {
        self.writing
            .as_ref()
            .is_some_and(|w| matches!(w.write, DiscussionWrite::SubmitReview { .. }))
    }

    /// How many comments the pending review holds, once read.
    pub(super) fn pending_count(&self) -> usize {
        self.pending
            .as_ref()
            .and_then(|s| s.review())
            .map_or(0, |r| r.comments.len())
    }
}

impl AppState {
    /// The review `S` would submit, or the first reason it cannot.
    fn submit_target(&self) -> Result<SubmitTarget, Reason> {
        let files = files_screen(self)
            .filter(|_| self.review_files_shown())
            .ok_or(NOT_FILES)?;
        let number = files.number;
        let review = slot_disabled(self)?;
        if self.review.writing.is_some() {
            return Err(POSTING.into());
        }
        if self
            .review
            .pending
            .as_ref()
            .is_some_and(|s| s.saving.is_some())
        {
            return Err(WAIT.into());
        }
        let detail = self.review.detail.as_ref().filter(|d| d.number == number);
        let pull = detail.and_then(|d| d.node_id.clone()).ok_or(NO_PR_ID)?;
        let (host, repo) = self.review_forge_target()?;
        let commit = match review.comments.split_first() {
            Some((first, rest)) => {
                if rest.iter().any(|c| c.head != first.head) {
                    return Err(format!(
                        "Pending comments were written on different heads of #{number}; \
                         edit or delete them in P first"
                    )
                    .into());
                }
                first.head.clone()
            }
            None => detail
                .and_then(|d| d.head_oid.as_ref())
                .map(|oid| oid.as_str().to_string())
                .ok_or(NO_HEAD)?,
        };
        Ok(SubmitTarget {
            host,
            repo,
            number,
            pull,
            commit,
            threads: review.comments.clone(),
        })
    }

    /// `S`: pick how the review judges the pull request.
    pub(crate) fn open_submit(&mut self) {
        if let Some(reason) = submit_disabled(self) {
            self.notice = Some(reason);
            return;
        }
        let rows = EVENTS.iter().map(|e| e.label().to_string()).collect();
        self.popups.push(Popup::PickList(PickList::new(
            PickPurpose::ReviewEvent,
            rows,
            0,
        )));
    }

    /// The row picked from the "Review type" list: the summary composer,
    /// checked again.
    pub(crate) fn pick_review_event(&mut self, row: usize) {
        let Some(&event) = EVENTS.get(row) else {
            return;
        };
        match self.submit_target() {
            Ok(target) => {
                self.review.composer = Some(Composer::new(ComposerKind::Review {
                    number: target.number,
                    event,
                }));
            }
            Err(reason) => self.notice = Some(reason),
        }
    }

    /// The summary as it is sent, or why not, in the notice. It may be
    /// blank for an approval, and for a comment review with line comments.
    pub(super) fn review_summary(&mut self, event: ReviewEvent, text: &str) -> Option<String> {
        let body = kika_forge::pending::clean_body(text);
        let may_be_blank = match event {
            ReviewEvent::Approve => true,
            ReviewEvent::RequestChanges => false,
            ReviewEvent::Comment => self.review.pending_count() > 0,
        };
        if body.trim().is_empty() {
            if may_be_blank {
                return Some(String::new());
            }
            self.notice = Some(BLANK.into());
            return None;
        }
        if body.len() > MAX_BODY {
            self.notice = Some(format!("A summary holds at most {} KiB", MAX_BODY / 1024).into());
            return None;
        }
        Some(body)
    }

    /// The question before a submit: what kind, where and with how much.
    pub(super) fn submit_question(&self, number: u64, event: ReviewEvent) -> String {
        let comments = comments_label(self.review.pending_count());
        format!(
            "Submit review ({}) on #{number} with {comments} to GitHub? (y/n)",
            event.label()
        )
    }

    /// The `y` of "Submit review…?": the review to send, checked again.
    pub(super) fn submit_write(
        &mut self,
        number: u64,
        event: ReviewEvent,
        text: &str,
    ) -> Result<(ForgeHost, ForgeRepo, u64, DiscussionWrite), Reason> {
        let target = self.submit_target()?;
        if target.number != number {
            return Err(NOT_FILES.into());
        }
        let body = self
            .review_summary(event, text)
            .ok_or_else(|| self.notice.clone().unwrap_or_else(|| BLANK.into()))?;
        let write = DiscussionWrite::SubmitReview {
            pull: target.pull,
            commit: target.commit,
            event,
            body,
            threads: target.threads,
        };
        Ok((target.host, target.repo, target.number, write))
    }

    /// GitHub took the review: the comments sent leave the pending review,
    /// which is saved, and the discussion is read again where it shows.
    pub(super) fn review_submitted(
        &mut self,
        number: u64,
        event: ReviewEvent,
        sent: &[PendingComment],
        shown: bool,
    ) -> Vec<Effect> {
        self.notice = Some(format!("Review submitted ({})", event.label()).into());
        if self.review.composer.as_ref().is_some_and(Composer::posts) {
            self.review.composer = None;
        }
        self.review.pending_list = None;
        let mut effects = self.remove_sent_pending(number, sent);
        if shown {
            effects.extend(self.load_review_discussion(number));
        }
        effects
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;

    use super::*;
    use crate::effect::Tag;
    use crate::features::review::ReviewIntent;
    use crate::features::review::pending::tests::{
        HEAD, comment, comments, ctrl, left, loading, right, type_text, with_review,
    };
    use crate::features::review::state::NO_FORGE;
    use crate::features::review::state::tests::{press, repo, type_char};
    use crate::intent::AppIntent;
    use crate::keymap::KeyScope;
    use kika_forge::discussion_write::Written;
    use kika_forge::pending::{PendingError, PendingReview};
    use kika_forge::{CreateError, ForgeStatus, REFRESH_LIMITED};

    const NOW: i64 = 1_700_000_000;
    const OTHER_HEAD: &str = "2222222222222222222222222222222222222222";

    fn github_host() -> ForgeHost {
        ForgeHost::parse("github.com").unwrap()
    }

    /// Two pending comments, one a range, on the files screen of #12.
    fn drafts() -> Vec<PendingComment> {
        vec![
            comment(1, right(1), "one"),
            PendingComment {
                start: Some(left(2)),
                ..comment(2, right(3), "two")
            },
        ]
    }

    fn reviewing() -> AppState {
        on_files(PendingReview { comments: drafts() })
    }

    /// The files screen of #12 holding `review`, its detail's number
    /// matching the screen.
    fn on_files(review: PendingReview) -> AppState {
        let mut state = with_review(review);
        state.review.detail.as_mut().unwrap().number = 12;
        state
    }

    fn picked(state: &AppState) -> Option<&PickList> {
        match state.popups.last() {
            Some(Popup::PickList(list)) => Some(list),
            _ => None,
        }
    }

    fn question_text(state: &AppState) -> Option<String> {
        match state.popups.last() {
            Some(Popup::Question(q)) => {
                assert_eq!(q.scope, KeyScope::ReviewCommentConfirm);
                Some(q.text.clone())
            }
            _ => None,
        }
    }

    /// `S`, then the pick's row `row` taken with `enter`.
    fn pick(state: &mut AppState, row: usize) {
        assert_eq!(type_char(state, 'S'), []);
        for _ in 0..row {
            press(state, KeyCode::Down);
        }
        assert_eq!(press(state, KeyCode::Enter), []);
    }

    /// The one write `effects` holds.
    fn sent(effects: &[Effect]) -> (Tag, u64, DiscussionWrite) {
        match effects {
            [
                Effect::WriteDiscussion {
                    tag,
                    host,
                    repo: to,
                    number,
                    write,
                },
            ] => {
                assert_eq!(*host, github_host());
                assert_eq!(*to, repo());
                (*tag, *number, write.clone())
            }
            other => panic!("expected one write, got {other:?}"),
        }
    }

    /// Request changes with `summary`, asked about and answered `y`.
    fn submit(state: &mut AppState, summary: &str) -> (Tag, u64, DiscussionWrite) {
        pick(state, 1);
        type_text(state, summary);
        assert_eq!(ctrl(state, 's'), []);
        sent(&type_char(state, 'y'))
    }

    fn written(
        state: &mut AppState,
        sent: &(Tag, u64, DiscussionWrite),
        result: Result<Written, CreateError>,
    ) -> Vec<Effect> {
        let (tag, number, write) = sent.clone();
        state.update(AppIntent::Review(ReviewIntent::DiscussionWritten {
            tag,
            host: github_host(),
            number,
            write,
            result,
        }))
    }

    fn summary(state: &AppState) -> String {
        state
            .review
            .composer
            .as_ref()
            .expect("a composer")
            .text
            .text()
    }

    #[test]
    fn s_picks_a_type_and_y_sends_every_pending_comment_with_their_head() {
        let mut state = reviewing();
        assert_eq!(submit_disabled(&state), None);
        assert_eq!(type_char(&mut state, 'S'), []);
        let list = picked(&state).expect("the review-type pick");
        assert_eq!(list.purpose, PickPurpose::ReviewEvent);
        assert_eq!(list.rows, ["Approve", "Request changes", "Comment"]);
        press(&mut state, KeyCode::Down);
        assert_eq!(press(&mut state, KeyCode::Enter), []);
        assert_eq!(state.key_focus(), KeyScope::ReviewComment);
        assert_eq!(
            state.review.composer.as_ref().unwrap().kind,
            ComposerKind::Review {
                number: 12,
                event: ReviewEvent::RequestChanges,
            }
        );
        type_text(&mut state, "Needs tests");
        assert_eq!(ctrl(&mut state, 's'), []);
        assert_eq!(
            question_text(&state).as_deref(),
            Some("Submit review (Request changes) on #12 with 2 comments to GitHub? (y/n)")
        );
        // `n` and `esc` send nothing and keep the summary.
        for key in [KeyCode::Char('n'), KeyCode::Esc] {
            assert_eq!(press(&mut state, key), [], "{key:?}");
            assert_eq!(question_text(&state), None, "{key:?}");
            assert_eq!(summary(&state), "Needs tests", "{key:?}");
            assert_eq!(ctrl(&mut state, 's'), []);
        }
        let (tag, number, write) = sent(&type_char(&mut state, 'y'));
        assert!(tag.user);
        assert_eq!(number, 12);
        assert_eq!(
            write,
            DiscussionWrite::SubmitReview {
                pull: "PR_kwDOA1".into(),
                commit: HEAD.into(),
                event: ReviewEvent::RequestChanges,
                body: "Needs tests".into(),
                threads: drafts(),
            }
        );
        assert_eq!(state.notice.as_deref(), Some("Submitting review…"));
        assert_eq!(comments(&state), drafts());
    }

    #[test]
    fn a_blank_summary_is_fine_for_approve_and_for_comment_with_comments_only() {
        // Approve, with or without comments.
        for review in [
            PendingReview { comments: drafts() },
            PendingReview::default(),
        ] {
            let empty = review.comments.is_empty();
            let mut state = on_files(review);
            pick(&mut state, 0);
            assert_eq!(ctrl(&mut state, 's'), []);
            let want = if empty { "0 comments" } else { "2 comments" };
            assert!(question_text(&state).unwrap().contains(want));
            let (_, _, write) = sent(&type_char(&mut state, 'y'));
            let DiscussionWrite::SubmitReview { body, commit, .. } = write else {
                panic!("{write:?}");
            };
            assert_eq!(body, "");
            // With no comments, the head the detail names.
            let head = state.review.detail.as_ref().unwrap().head_oid.clone();
            if empty {
                assert_eq!(Some(commit.as_str()), head.as_ref().map(|h| h.as_str()));
            }
        }
        // Request changes needs one.
        let mut state = reviewing();
        pick(&mut state, 1);
        type_text(&mut state, "  ");
        assert_eq!(ctrl(&mut state, 's'), []);
        assert_eq!(question_text(&state), None);
        assert_eq!(state.notice.as_deref(), Some(BLANK));
        assert!(state.review.composer.is_some());
        // Comment needs one only without line comments.
        let mut state = reviewing();
        pick(&mut state, 2);
        assert_eq!(ctrl(&mut state, 's'), []);
        assert!(question_text(&state).is_some());
        let mut state = on_files(PendingReview::default());
        pick(&mut state, 2);
        assert_eq!(ctrl(&mut state, 's'), []);
        assert_eq!(question_text(&state), None);
        assert_eq!(state.notice.as_deref(), Some(BLANK));
    }

    #[test]
    fn a_submitted_review_empties_the_pending_review_and_reads_the_discussion() {
        let mut state = reviewing();
        type_char(&mut state, 'P');
        let sent = submit(&mut state, "Needs tests");
        assert_eq!(state.review.pending_list, Some(0));
        let effects = written(&mut state, &sent, Ok(Written::Posted));
        let saves: Vec<&PendingReview> = effects
            .iter()
            .filter_map(|e| match e {
                Effect::SavePendingReview { review, .. } => Some(review),
                _ => None,
            })
            .collect();
        assert_eq!(saves, [&PendingReview::default()]);
        let loads = effects
            .iter()
            .filter(|e| matches!(e, Effect::LoadPrDiscussion { number: 12, .. }))
            .count();
        assert_eq!(loads, 1, "{effects:?}");
        assert_eq!(effects.len(), 2, "{effects:?}");
        assert_eq!(comments(&state), []);
        assert_eq!(state.review.composer, None);
        assert_eq!(state.review.pending_list, None);
        assert_eq!(state.review.writing, None);
        assert_eq!(
            state.notice.as_deref(),
            Some("Review submitted (Request changes)")
        );
    }

    #[test]
    fn a_failed_submit_keeps_every_draft_and_the_summary_and_says_why() {
        let cases = [
            (
                CreateError::Status(ForgeStatus::Offline),
                "May have been posted - check before submitting again (GitHub did not answer)"
                    .to_string(),
            ),
            (
                CreateError::Status(ForgeStatus::Failed),
                "May have been posted - check before submitting again (GitHub gave no usable answer)"
                    .to_string(),
            ),
            (
                CreateError::Status(ForgeStatus::NoAccess),
                CreateError::Status(ForgeStatus::NoAccess).message(),
            ),
            (
                CreateError::Status(ForgeStatus::NotLoggedIn),
                CreateError::Status(ForgeStatus::NotLoggedIn).message(),
            ),
            (
                CreateError::Refused("Can not approve your own pull request".into()),
                "Can not approve your own pull request".to_string(),
            ),
            (
                CreateError::RateLimited { until: NOW + 600 },
                "GitHub is rate limiting this account.".to_string(),
            ),
        ];
        for (error, want) in cases {
            let mut state = reviewing();
            let sent = submit(&mut state, "Needs tests");
            let effects = written(&mut state, &sent, Err(error.clone()));
            assert_eq!(effects, [], "{error:?}");
            assert_eq!(comments(&state), drafts(), "{error:?}");
            assert_eq!(summary(&state), "Needs tests", "{error:?}");
            assert_eq!(state.review.writing, None, "{error:?}");
            assert_eq!(state.notice.as_deref(), Some(want.as_str()), "{error:?}");
            if let CreateError::RateLimited { until } = error {
                assert_eq!(state.forge.limited.get("github.com"), Some(&until));
            } else {
                // The user may try again.
                assert_eq!(ctrl(&mut state, 's'), []);
                sent_again(&mut state);
            }
        }
    }

    fn sent_again(state: &mut AppState) {
        let (_, _, write) = sent(&type_char(state, 'y'));
        assert!(matches!(write, DiscussionWrite::SubmitReview { .. }));
    }

    #[test]
    fn while_a_review_is_on_its_way_the_pending_review_cannot_change() {
        let mut state = reviewing();
        type_char(&mut state, 'P');
        submit(&mut state, "Needs tests");
        let writing = state.review.writing.clone();
        // The composer keeps the keys: esc waits and nothing is typed.
        assert_eq!(press(&mut state, KeyCode::Esc), []);
        assert_eq!(state.notice.as_deref(), Some(WAIT));
        type_text(&mut state, "more");
        assert_eq!(summary(&state), "Needs tests");
        // Whatever asks, nothing in the pending review changes.
        let asks = [
            ReviewIntent::StartComment,
            ReviewIntent::AskDeletePending,
            ReviewIntent::DeletePending(1),
            ReviewIntent::EditPending,
            ReviewIntent::OpenSubmitReview,
        ];
        for intent in asks {
            state.notice = None;
            assert_eq!(
                state.update(AppIntent::Review(intent.clone())),
                [],
                "{intent:?}"
            );
            assert!(state.notice.is_some(), "{intent:?}");
            assert!(picked(&state).is_none(), "{intent:?}");
            assert_eq!(question_text(&state), None, "{intent:?}");
            assert_eq!(comments(&state), drafts(), "{intent:?}");
            assert_eq!(summary(&state), "Needs tests", "{intent:?}");
        }
        // A line comment's ctrl-s, had one been open.
        let review_composer = state.review.composer.clone();
        state.review.composer = Some(Composer::new(ComposerKind::Line {
            target: super::super::pending::CommentTarget {
                path: "src/cache.rs".into(),
                start: None,
                end: right(1),
                head: HEAD.into(),
            },
            editing: None,
        }));
        type_text(&mut state, "x");
        assert_eq!(ctrl(&mut state, 's'), []);
        assert_eq!(state.notice.as_deref(), Some(WAIT));
        assert_eq!(comments(&state), drafts());
        state.review.composer = review_composer;
        assert_eq!(state.review.writing, writing);
    }

    #[test]
    fn s_says_why_it_cannot_submit() {
        let refused = |state: &mut AppState, want: &str| {
            assert_eq!(
                submit_disabled(state).map(String::from).as_deref(),
                Some(want)
            );
            assert_eq!(type_char(state, 'S'), []);
            assert!(picked(state).is_none());
            assert_eq!(state.notice.as_deref(), Some(want));
        };
        let (mut state, _) = loading();
        state.review.detail.as_mut().unwrap().number = 12;
        refused(&mut state, "The pending review is still loading");

        let (mut state, tag) = loading();
        crate::features::review::pending::tests::loaded(
            &mut state,
            tag,
            Err(PendingError::Unreadable("it is too large")),
        );
        let reason = submit_disabled(&state).unwrap();
        assert!(reason.contains("could not be read"), "{reason}");

        let mut state = on_files(PendingReview {
            comments: vec![
                comment(1, right(1), "one"),
                PendingComment {
                    head: OTHER_HEAD.into(),
                    ..comment(2, right(3), "two")
                },
            ],
        });
        refused(
            &mut state,
            "Pending comments were written on different heads of #12; \
             edit or delete them in P first",
        );

        let mut state = reviewing();
        state.review.detail.as_mut().unwrap().node_id = None;
        refused(&mut state, NO_PR_ID);

        let mut state = reviewing();
        state.forge.hosts.clear();
        refused(&mut state, NO_FORGE);

        let mut state = reviewing();
        state.forge.limited.insert("github.com".into(), NOW + 60);
        refused(&mut state, REFRESH_LIMITED);

        let mut state = on_files(PendingReview::default());
        state.review.detail.as_mut().unwrap().head_oid = None;
        refused(&mut state, NO_HEAD);

        // A save still on its way to disk.
        let mut state = reviewing();
        state.review.pending.as_mut().unwrap().saving = Some(state.user_tag(0));
        refused(&mut state, WAIT);

        // Not on the detail.
        let mut state = reviewing();
        press(&mut state, KeyCode::Esc);
        assert_eq!(
            submit_disabled(&state).map(String::from).as_deref(),
            Some(NOT_FILES)
        );
    }

    #[test]
    fn the_host_and_heads_are_checked_again_at_the_y() {
        let mut state = reviewing();
        pick(&mut state, 0);
        ctrl(&mut state, 's');
        state.forge.hosts.clear();
        assert_eq!(type_char(&mut state, 'y'), []);
        assert_eq!(state.notice.as_deref(), Some(NO_FORGE));
        assert_eq!(state.review.writing, None);
        assert!(state.review.composer.is_some());
    }

    #[test]
    fn ctrl_e_on_a_summary_comes_back_to_the_submit_question_and_never_sends() {
        use crate::features::review::comment_external::EditedComment;
        use crate::handoff::{HandOff, KeptFile};
        let mut state = reviewing();
        pick(&mut state, 1);
        type_text(&mut state, "draft");
        let effects = ctrl(&mut state, 'e');
        let [Effect::HandOff(HandOff::EditReviewComment { initial, stamp })] = effects.as_slice()
        else {
            panic!("{effects:?}");
        };
        assert_eq!(initial, "draft");
        let dir = std::path::PathBuf::from("/tmp/kika-comment-1-submit");
        let file = KeptFile {
            path: dir.join("kika-comment-1-submit.md"),
            dir,
        };
        let effects = state.update(AppIntent::Review(ReviewIntent::CommentEdited {
            stamp: stamp.clone(),
            result: Ok(EditedComment {
                text: "Needs tests".into(),
                file: file.clone(),
            }),
        }));
        // Only the file is removed: nothing goes to GitHub until `y`.
        assert_eq!(effects, [Effect::RemoveCommentFile(file)]);
        assert_eq!(state.review.writing, None);
        assert_eq!(
            question_text(&state).as_deref(),
            Some("Submit review (Request changes) on #12 with 2 comments to GitHub? (y/n)")
        );
        let (_, _, write) = sent(&type_char(&mut state, 'y'));
        let DiscussionWrite::SubmitReview { body, threads, .. } = write else {
            panic!("{write:?}");
        };
        assert_eq!((body.as_str(), threads), ("Needs tests", drafts()));
    }

    fn draw(state: &AppState) -> Vec<String> {
        let (width, height) = state.size;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| crate::view::render(state, frame))
            .unwrap();
        crate::view::buffer_rows(terminal.backend().buffer())
    }

    #[test]
    fn the_summary_composer_names_the_type_and_the_comment_count() {
        let mut state = reviewing();
        pick(&mut state, 0);
        type_text(&mut state, "Ship it");
        let rows = draw(&state);
        let title = rows
            .iter()
            .find(|r| r.contains("Review summary: Approve #12 (2 comments)"))
            .unwrap_or_else(|| panic!("{rows:#?}"));
        assert!(title.contains("posts to GitHub"), "{title}");
        assert!(rows.iter().any(|r| r.contains("Ship it")), "{rows:#?}");
        for (width, height) in [(80, 24), (40, 12), (24, 8), (20, 6), (1, 1)] {
            state.update(AppIntent::Resize { width, height });
            assert_eq!(draw(&state).len(), usize::from(height));
        }
        // Narrow: the cue that it goes to GitHub stays.
        state.update(AppIntent::Resize {
            width: 40,
            height: 12,
        });
        assert!(draw(&state).iter().any(|r| r.contains("posts to GitHub")));
    }
}
