//! Answering on GitHub from review mode: a reply to the thread the reader
//! shows (`a`), resolving or unresolving it (`R`), and a general comment
//! from the conversation (`a`). A reply or a comment is typed in the
//! composer and posted only on the `y` of a question; `R` is sent at once,
//! since `R` again undoes it and it posts no text. One write is in flight
//! at a time, on a host the global config lists and `[forge.allow]`
//! allows. A failure keeps the text, and nothing is tried again.

use super::comment_editor::{Composer, ComposerKind};
use super::files::files_screen;
use super::reader::{thread_label, thread_reader_open};
use super::state::ReviewScreen;
use crate::effect::{Effect, Tag};
use crate::keymap::Reason;
use crate::model::AppState;
use kika_forge::discussion_write::{DiscussionWrite, Written};
use kika_forge::review::node_id_ok;
use kika_forge::{CreateError, ForgeHost, ForgeRepo, ForgeStatus};

pub(super) const POSTING: &str = "Already posting";
pub(super) const WAIT: &str = "Wait for GitHub to answer";
const NO_THREAD: &str = "Open a review thread first";
const NO_THREAD_ID: &str = "GitHub sent no id for this thread; refresh with r";
const NO_PR_ID: &str = "GitHub sent no pull request id; refresh with r";
const NOT_CONVERSATION: &str = "Open the conversation first (C)";
const GONE: &str = "That thread is no longer shown; refresh with r";
const OFFLINE: &str = "GitHub could not be reached, or did not answer in time";
// The warning leads, so a narrow bar keeps it.
const MAYBE_POSTED: &str = "May have been posted - check before posting again";
const NO_ANSWER: &str = "GitHub did not answer";
const NO_USABLE: &str = "GitHub gave no usable answer";

/// The one write in flight: its sequence, the pull request it is on and
/// what was sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscussionWriting {
    pub seq: u64,
    pub number: u64,
    pub write: DiscussionWrite,
}

/// The thread the reader shows, as a write may be aimed at it.
struct ThreadTarget {
    host: ForgeHost,
    repo: ForgeRepo,
    number: u64,
    /// Index into the discussion's threads.
    index: usize,
    node_id: String,
    resolved: bool,
}

/// Why `a` and `R` in the reader cannot act on the thread shown.
pub(crate) fn thread_write_disabled(state: &AppState) -> Option<Reason> {
    state.thread_target().err()
}

/// Why `a` on the conversation cannot start a comment: it is not shown,
/// or GitHub sent no id for the pull request.
pub(crate) fn general_comment_disabled(state: &AppState) -> Option<Reason> {
    state.general_subject().err()
}

/// What the user is told when a write failed. A post that timed out or
/// had no usable answer may still have been made.
fn failure_notice(error: &CreateError, write: &DiscussionWrite) -> String {
    let post = !matches!(write, DiscussionWrite::Resolve { .. });
    match error {
        CreateError::Status(ForgeStatus::Offline) if post => {
            format!("{MAYBE_POSTED} ({NO_ANSWER})")
        }
        CreateError::Status(ForgeStatus::Failed) if post => {
            format!("{MAYBE_POSTED} ({NO_USABLE})")
        }
        CreateError::Status(ForgeStatus::Offline) => OFFLINE.to_string(),
        other => other.message(),
    }
}

impl AppState {
    /// The reader's thread, open with an id, while no write runs, on a
    /// host that may be asked now.
    fn thread_target(&self) -> Result<ThreadTarget, Reason> {
        if !thread_reader_open(self) {
            return Err(NO_THREAD.into());
        }
        let files = files_screen(self).ok_or(NO_THREAD)?;
        let index = files
            .reader
            .as_ref()
            .and_then(|r| r.shown())
            .ok_or(NO_THREAD)?;
        let thread = self
            .review
            .discussion
            .as_ref()
            .and_then(|d| d.threads.get(index))
            .ok_or(NO_THREAD)?;
        if self.review.writing.is_some() {
            return Err(POSTING.into());
        }
        let node_id = thread.node_id.clone().ok_or(NO_THREAD_ID)?;
        let (host, repo) = self.review_forge_target()?;
        Ok(ThreadTarget {
            host,
            repo,
            number: files.number,
            index,
            node_id,
            resolved: thread.resolved,
        })
    }

    /// The conversation's pull request number and node id.
    fn general_subject(&self) -> Result<(u64, String), Reason> {
        let ReviewScreen::Conversation(number) = self.review.screen else {
            return Err(NOT_CONVERSATION.into());
        };
        let subject = self
            .review
            .detail
            .as_ref()
            .filter(|d| d.number == number)
            .and_then(|d| d.node_id.clone())
            .ok_or(NO_PR_ID)?;
        Ok((number, subject))
    }

    /// `a` in the reader: the composer, for a reply to the thread shown.
    pub(crate) fn reply_to_thread(&mut self) {
        let target = match self.thread_target() {
            Ok(target) => target,
            Err(reason) => {
                self.notice = Some(reason);
                return;
            }
        };
        let (Some(files), Some(discussion)) = (files_screen(self), &self.review.discussion) else {
            return;
        };
        let label = thread_label(files, discussion, target.index);
        self.review.composer = Some(Composer::new(ComposerKind::Reply {
            number: target.number,
            thread: target.node_id,
            label,
        }));
    }

    /// `a` on the conversation: the composer, for a comment on the pull
    /// request.
    pub(crate) fn start_general_comment(&mut self) {
        let checked = self.general_subject().and_then(|subject| {
            self.review_forge_target()?;
            if self.review.writing.is_some() {
                return Err(POSTING.into());
            }
            Ok(subject)
        });
        match checked {
            Ok((number, subject)) => {
                self.review.composer =
                    Some(Composer::new(ComposerKind::General { number, subject }));
            }
            Err(reason) => self.notice = Some(reason),
        }
    }

    /// `R` in the reader: resolve the thread shown, or unresolve it when
    /// it is resolved. Nothing asks first, since `R` again undoes it.
    pub(crate) fn toggle_thread_resolved(&mut self) -> Vec<Effect> {
        let target = match self.thread_target() {
            Ok(target) => target,
            Err(reason) => {
                self.notice = Some(reason);
                return vec![];
            }
        };
        let resolved = !target.resolved;
        let notice = if resolved {
            "Resolving…"
        } else {
            "Unresolving…"
        };
        let write = DiscussionWrite::Resolve {
            thread: target.node_id,
            resolved,
        };
        self.send_write(target.host, target.repo, target.number, write, notice)
    }

    /// The `y` of "Post this … to GitHub?": post the composer's text,
    /// checked again, as a reply or a general comment.
    pub(crate) fn post_comment(&mut self) -> Vec<Effect> {
        match self.post_target() {
            Ok((host, repo, number, write)) => {
                let notice = match write {
                    DiscussionWrite::Reply { .. } => "Posting reply…",
                    _ => "Posting comment…",
                };
                self.send_write(host, repo, number, write, notice)
            }
            Err(reason) => {
                self.notice = Some(reason);
                vec![]
            }
        }
    }

    /// What the composer would post now, or why not: no write runs, its
    /// pull request and target are still shown, the host may be asked, and
    /// the text is a comment's.
    fn post_target(&mut self) -> Result<(ForgeHost, ForgeRepo, u64, DiscussionWrite), Reason> {
        if self.review.writing.is_some() {
            return Err(POSTING.into());
        }
        let Some(composer) = self.review.composer.clone() else {
            return Err(GONE.into());
        };
        let number = match &composer.kind {
            ComposerKind::Reply { number, thread, .. } => {
                let known = files_screen(self).is_some_and(|f| f.number == *number)
                    && self.review.discussion.as_ref().is_some_and(|d| {
                        d.threads
                            .iter()
                            .any(|t| t.node_id.as_deref() == Some(thread.as_str()))
                    });
                if !known || !node_id_ok(thread) {
                    return Err(GONE.into());
                }
                *number
            }
            ComposerKind::General { number, subject } => {
                if self.general_subject()? != (*number, subject.clone()) {
                    return Err(NO_PR_ID.into());
                }
                *number
            }
            ComposerKind::Line { .. } => return Err(GONE.into()),
        };
        let (host, repo) = self.review_forge_target()?;
        let body = self
            .comment_body(&composer.text.text())
            .ok_or_else(|| self.notice.clone().unwrap_or_else(|| GONE.into()))?;
        let write = match composer.kind {
            ComposerKind::Reply { thread, .. } => DiscussionWrite::Reply { thread, body },
            ComposerKind::General { subject, .. } => DiscussionWrite::Comment { subject, body },
            ComposerKind::Line { .. } => return Err(GONE.into()),
        };
        Ok((host, repo, number, write))
    }

    /// Send `write` as the one write in flight.
    fn send_write(
        &mut self,
        host: ForgeHost,
        repo: ForgeRepo,
        number: u64,
        write: DiscussionWrite,
        notice: &'static str,
    ) -> Vec<Effect> {
        let seq = self.take_seq();
        let tag = self.user_tag(seq);
        self.notice = Some(notice.into());
        self.review.writing = Some(DiscussionWriting {
            seq,
            number,
            write: write.clone(),
        });
        vec![Effect::WriteDiscussion {
            tag,
            host,
            repo,
            number,
            write,
        }]
    }

    /// GitHub answered a write. A post closes the composer and reads the
    /// discussion again, once; a resolve is applied where it is shown. A
    /// failure keeps the composer and its text. An answer to any other
    /// write changes nothing.
    pub(crate) fn discussion_written(
        &mut self,
        tag: Tag,
        host: &ForgeHost,
        number: u64,
        write: DiscussionWrite,
        result: Result<Written, CreateError>,
    ) -> Vec<Effect> {
        self.user_done(tag);
        let sent = DiscussionWriting {
            seq: tag.seq,
            number,
            write,
        };
        // The answer to this write from before the generation moved: none
        // of it applies any more, but nothing stays locked behind it.
        if tag.generation != self.generation {
            let ours = self.review.writing.as_ref();
            if ours.is_some_and(|w| w.seq == sent.seq && w.number == number) {
                self.review.writing = None;
            }
            return vec![];
        }
        if self.review.writing.as_ref() != Some(&sent) {
            return vec![];
        }
        self.review.writing = None;
        let write = sent.write;
        let result = match (&write, result) {
            (DiscussionWrite::Resolve { resolved, .. }, Ok(Written::Resolved(now)))
                if now == *resolved =>
            {
                Ok(Written::Resolved(now))
            }
            (
                DiscussionWrite::Reply { .. } | DiscussionWrite::Comment { .. },
                Ok(Written::Posted),
            ) => Ok(Written::Posted),
            // An answer of the wrong kind is no answer.
            (_, Ok(_)) => Err(CreateError::Status(ForgeStatus::Failed)),
            (_, Err(error)) => Err(error),
        };
        match result {
            Ok(_) => self.take_write(number, write),
            Err(error) => {
                // Remembered against the host it went to, so the next
                // write asks nobody until it resets.
                if let CreateError::RateLimited { until } = error {
                    self.forge.record_rate_limit(host, until);
                }
                self.notice = Some(failure_notice(&error, &write).into());
                vec![]
            }
        }
    }

    /// Show what GitHub took.
    fn take_write(&mut self, number: u64, write: DiscussionWrite) -> Vec<Effect> {
        let shown = matches!(
            self.review.screen,
            ReviewScreen::Detail(n) | ReviewScreen::Files(n) | ReviewScreen::Conversation(n)
                if n == number
        );
        match write {
            DiscussionWrite::Resolve { thread, resolved } => {
                self.notice = Some(
                    if resolved {
                        "Thread resolved"
                    } else {
                        "Thread unresolved"
                    }
                    .into(),
                );
                if !shown {
                    return vec![];
                }
                let found = self.review.discussion.as_mut().and_then(|d| {
                    d.threads
                        .iter_mut()
                        .find(|t| t.node_id.as_deref() == Some(thread.as_str()))
                });
                if let Some(found) = found {
                    found.resolved = resolved;
                }
                if let Some(reader) = self.review.files.as_mut().and_then(|f| f.reader.as_mut()) {
                    reader.rendered = None;
                }
                self.render_thread_reader();
                vec![]
            }
            DiscussionWrite::Reply { .. } | DiscussionWrite::Comment { .. } => {
                let reply = match write {
                    DiscussionWrite::Reply { thread, .. } => Some(thread),
                    _ => None,
                };
                let posted = if reply.is_some() {
                    "Reply posted"
                } else {
                    "Comment posted"
                };
                self.notice = Some(posted.into());
                if self.review.composer.as_ref().is_some_and(Composer::posts) {
                    self.review.composer = None;
                }
                if !shown {
                    return vec![];
                }
                // The reader opens on the same thread once it is read again.
                if let Some(files) = self.review.files.as_mut().filter(|f| f.number == number) {
                    files.reopen_thread = reply;
                }
                self.load_review_discussion(number)
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use crossterm::event::KeyCode;

    use super::*;
    use crate::features::review::ReviewIntent;
    use crate::features::review::conversation::tests::detail_with;
    use crate::features::review::pending::tests::{ctrl, type_text};
    use crate::features::review::reader::tests::{reader, two_files};
    use crate::features::review::state::NO_FORGE;
    use crate::features::review::state::tests::{press, repo, type_char};
    use crate::features::review::threads::tests::{answer, fixture_discussion};
    use crate::intent::AppIntent;
    use crate::keymap::KeyScope;
    use crate::popup::Popup;
    use kika_forge::REFRESH_LIMITED;
    use kika_forge::discussion::PrDiscussion;

    const NOW: i64 = 1_700_000_000;

    fn github_host() -> ForgeHost {
        ForgeHost::parse("github.com").unwrap()
    }

    /// The reader open on cache.rs's unresolved thread 0 (`PRRT_kwDOA1`).
    pub(crate) fn reading() -> AppState {
        let mut state = two_files();
        type_char(&mut state, ']');
        press(&mut state, KeyCode::Enter);
        assert_eq!(reader(&state).shown(), Some(0));
        state
    }

    /// The conversation of pull request 12, its detail's number matching.
    pub(crate) fn conversing() -> AppState {
        let mut state = detail_with(fixture_discussion());
        state.review.detail.as_mut().unwrap().number = 12;
        state.update(AppIntent::Review(ReviewIntent::OpenConversation));
        assert_eq!(state.review.screen, ReviewScreen::Conversation(12));
        state
    }

    fn composer(state: &AppState) -> &Composer {
        state.review.composer.as_ref().expect("a composer")
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

    /// The reply composer with `text`, asked about and answered `y`.
    fn post_reply(state: &mut AppState, text: &str) -> (Tag, u64, DiscussionWrite) {
        assert_eq!(type_char(state, 'a'), []);
        type_text(state, text);
        assert_eq!(ctrl(state, 's'), []);
        sent(&type_char(state, 'y'))
    }

    fn discussion_loads(effects: &[Effect]) -> usize {
        effects
            .iter()
            .filter(|e| matches!(e, Effect::LoadPrDiscussion { number: 12, .. }))
            .count()
    }

    #[test]
    fn a_in_the_reader_opens_a_reply_and_only_y_posts_it() {
        let mut state = reading();
        assert_eq!(type_char(&mut state, 'a'), []);
        assert_eq!(state.key_focus(), KeyScope::ReviewComment);
        let ComposerKind::Reply {
            number,
            thread,
            label,
        } = &composer(&state).kind
        else {
            panic!("{:?}", composer(&state));
        };
        assert_eq!((*number, thread.as_str()), (12, "PRRT_kwDOA1"));
        assert!(label.starts_with("src/cache.rs · "), "{label}");
        // The reader stays under it.
        assert!(state.review.files.as_ref().unwrap().reader.is_some());

        let text = "Fixed in \"abc\"\nthanks, café";
        for line in ["Fixed in \"abc\"", "thanks, café"] {
            if !composer(&state).text.text().is_empty() {
                press(&mut state, KeyCode::Enter);
            }
            type_text(&mut state, line);
        }
        assert_eq!(ctrl(&mut state, 's'), []);
        assert_eq!(
            question_text(&state).as_deref(),
            Some("Post this reply to GitHub? (y/n)")
        );
        // `n`, `esc` and `enter` send nothing and keep the text.
        for key in [KeyCode::Char('n'), KeyCode::Esc, KeyCode::Enter] {
            let effects = press(&mut state, key);
            assert_eq!(effects, [], "{key:?}");
            assert_eq!(composer(&state).text.text(), text, "{key:?}");
            if question_text(&state).is_none() {
                assert_eq!(state.key_focus(), KeyScope::ReviewComment, "{key:?}");
                assert_eq!(ctrl(&mut state, 's'), []);
            }
        }
        let busy = state.busy_count();
        let (tag, number, write) = sent(&type_char(&mut state, 'y'));
        assert!(tag.user);
        assert_eq!(state.busy_count(), busy + 1);
        assert_eq!(number, 12);
        assert_eq!(
            write,
            DiscussionWrite::Reply {
                thread: "PRRT_kwDOA1".into(),
                body: text.into(),
            }
        );
        assert_eq!(state.notice.as_deref(), Some("Posting reply…"));
        assert_eq!(composer(&state).text.text(), text);
    }

    #[test]
    fn a_blank_reply_is_not_asked_about() {
        let mut state = reading();
        type_char(&mut state, 'a');
        type_text(&mut state, "  ");
        assert_eq!(ctrl(&mut state, 's'), []);
        assert_eq!(question_text(&state), None);
        assert_eq!(state.notice.as_deref(), Some("Write something first"));
    }

    #[test]
    fn while_a_post_is_in_flight_the_text_and_the_composer_stay() {
        let mut state = reading();
        let sent = post_reply(&mut state, "ok");
        type_text(&mut state, "more");
        press(&mut state, KeyCode::Backspace);
        press(&mut state, KeyCode::Enter);
        assert_eq!(composer(&state).text.text(), "ok");
        assert_eq!(press(&mut state, KeyCode::Esc), []);
        assert_eq!(state.notice.as_deref(), Some(WAIT));
        assert!(state.review.composer.is_some());
        assert_eq!(question_text(&state), None);
        assert_eq!(ctrl(&mut state, 's'), []);
        assert_eq!(state.notice.as_deref(), Some(POSTING));
        assert_eq!(question_text(&state), None);
        // A `y` meant for nothing sends nothing more.
        assert_eq!(
            state.update(AppIntent::Review(ReviewIntent::PostComment)),
            []
        );
        assert_eq!(state.review.writing.as_ref().unwrap().seq, sent.0.seq);
    }

    #[test]
    fn a_posted_reply_closes_the_composer_and_reads_the_thread_again() {
        let mut state = reading();
        let sent = post_reply(&mut state, "Done");
        let busy = state.busy_count();
        let effects = written(&mut state, &sent, Ok(Written::Posted));
        assert_eq!(state.busy_count(), busy - 1);
        assert_eq!(discussion_loads(&effects), 1);
        assert_eq!(effects.len(), 1, "{effects:?}");
        assert_eq!(state.review.composer, None);
        assert_eq!(state.review.writing, None);
        assert_eq!(state.notice.as_deref(), Some("Reply posted"));
        let Effect::LoadPrDiscussion { tag, .. } = effects[0].clone() else {
            unreachable!()
        };
        // The thread comes back with the reply, at another index.
        let mut discussion = fixture_discussion();
        let thread = discussion.threads.remove(0);
        discussion.threads.push(thread);
        answer(&mut state, tag, Ok(Some(discussion.clone())));
        let shown = reader(&state).shown().unwrap();
        assert_eq!(
            discussion.threads[shown].node_id.as_deref(),
            Some("PRRT_kwDOA1")
        );
        assert!(reader(&state).rendered.is_some());
        assert_eq!(state.review.files.as_ref().unwrap().reopen_thread, None);
    }

    #[test]
    fn a_thread_gone_from_the_reread_leaves_the_reader_closed() {
        let mut state = reading();
        let sent = post_reply(&mut state, "Done");
        let effects = written(&mut state, &sent, Ok(Written::Posted));
        let [Effect::LoadPrDiscussion { tag, .. }] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        let mut discussion = fixture_discussion();
        discussion.threads.remove(0);
        answer(&mut state, *tag, Ok(Some(discussion)));
        assert!(state.review.files.as_ref().unwrap().reader.is_none());
    }

    #[test]
    fn a_failed_reread_forgets_the_thread_to_reopen() {
        let mut state = reading();
        let sent = post_reply(&mut state, "Done");
        let effects = written(&mut state, &sent, Ok(Written::Posted));
        let [Effect::LoadPrDiscussion { tag, .. }] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        answer(
            &mut state,
            *tag,
            Err(kika_forge::review::ReviewError::Status(
                ForgeStatus::Offline,
            )),
        );
        let files = state.review.files.as_ref().unwrap();
        assert_eq!(files.reopen_thread, None);
        assert!(files.reader.is_none());
    }

    #[test]
    fn a_failed_post_keeps_the_text_and_says_why() {
        let cases = [
            (
                CreateError::Status(ForgeStatus::Offline),
                format!("{MAYBE_POSTED} ({NO_ANSWER})"),
            ),
            (
                CreateError::Status(ForgeStatus::Failed),
                format!("{MAYBE_POSTED} ({NO_USABLE})"),
            ),
            (
                CreateError::Status(ForgeStatus::NoGh),
                "The gh command is not installed.".to_string(),
            ),
            (
                CreateError::Status(ForgeStatus::NotLoggedIn),
                CreateError::Status(ForgeStatus::NotLoggedIn).message(),
            ),
            (
                CreateError::Status(ForgeStatus::NoAccess),
                CreateError::Status(ForgeStatus::NoAccess).message(),
            ),
            (
                CreateError::RateLimited { until: NOW + 600 },
                "GitHub is rate limiting this account.".to_string(),
            ),
            (
                CreateError::Refused("Thread is locked".into()),
                "Thread is locked".to_string(),
            ),
        ];
        for (error, want) in cases {
            let mut state = reading();
            let sent = post_reply(&mut state, "Done");
            let busy = state.busy_count();
            let effects = written(&mut state, &sent, Err(error.clone()));
            assert_eq!(effects, [], "{error:?}");
            assert_eq!(state.busy_count(), busy - 1);
            assert_eq!(state.review.writing, None, "{error:?}");
            assert_eq!(composer(&state).text.text(), "Done", "{error:?}");
            assert_eq!(state.notice.as_deref(), Some(want.as_str()), "{error:?}");
            if let CreateError::RateLimited { until } = error {
                assert_eq!(state.forge.limited.get("github.com"), Some(&until));
                // So the next post asks nobody.
                assert_eq!(ctrl(&mut state, 's'), []);
                assert_eq!(type_char(&mut state, 'y'), []);
                assert_eq!(state.notice.as_deref(), Some(REFRESH_LIMITED));
            } else {
                // The text can be posted again, by the user only.
                assert_eq!(ctrl(&mut state, 's'), []);
                sent_again(&mut state);
            }
        }
    }

    fn sent_again(state: &mut AppState) {
        let (_, _, write) = sent(&type_char(state, 'y'));
        assert!(matches!(write, DiscussionWrite::Reply { .. }));
    }

    #[test]
    fn a_stale_answer_changes_nothing() {
        type Stale = fn(&mut (Tag, u64, DiscussionWrite));
        let cases: [Stale; 4] = [
            |sent| sent.0.seq += 100,
            |sent| sent.0.generation += 1,
            |sent| sent.1 = 13,
            |sent| {
                sent.2 = DiscussionWrite::Reply {
                    thread: "PRRT_other".into(),
                    body: "Done".into(),
                }
            },
        ];
        for (i, stale) in cases.into_iter().enumerate() {
            let mut state = reading();
            let mut sent = post_reply(&mut state, "Done");
            stale(&mut sent);
            let writing = state.review.writing.clone();
            let busy = state.busy_count();
            let effects = written(&mut state, &sent, Ok(Written::Posted));
            assert_eq!(effects, [], "case {i}");
            assert_eq!(state.busy_count(), busy - 1, "case {i}");
            assert_eq!(composer(&state).text.text(), "Done", "case {i}");
            assert_eq!(state.notice.as_deref(), Some("Posting reply…"), "case {i}");
            if i == 1 {
                // Its own answer from before the generation moved: the
                // composer is no longer locked, and nothing was applied.
                assert_eq!(state.review.writing, None, "case {i}");
                assert!(!state.composer_posting());
            } else {
                assert_eq!(state.review.writing, writing, "case {i}");
            }
        }
    }

    #[test]
    fn r_resolves_at_once_and_applies_githubs_answer_in_place() {
        let mut state = reading();
        let busy = state.busy_count();
        let effects = type_char(&mut state, 'R');
        assert_eq!(question_text(&state), None);
        let sent = sent(&effects);
        assert_eq!(
            sent.2,
            DiscussionWrite::Resolve {
                thread: "PRRT_kwDOA1".into(),
                resolved: true,
            }
        );
        assert_eq!(state.busy_count(), busy + 1);
        assert_eq!(state.notice.as_deref(), Some("Resolving…"));
        // A second `R` waits for the first.
        assert_eq!(type_char(&mut state, 'R'), []);
        assert_eq!(state.notice.as_deref(), Some(POSTING));
        assert_eq!(type_char(&mut state, 'a'), []);
        assert_eq!(state.review.composer, None);
        let effects = written(&mut state, &sent, Ok(Written::Resolved(true)));
        assert_eq!(effects, []);
        assert_eq!(discussion_loads(&effects), 0);
        let discussion = state.review.discussion.as_ref().unwrap();
        assert!(discussion.threads[0].resolved);
        assert_eq!(state.notice.as_deref(), Some("Thread resolved"));
        assert!(reader(&state).rendered.is_some());
        // And back.
        let sent = super::tests::sent(&type_char(&mut state, 'R'));
        assert!(matches!(
            sent.2,
            DiscussionWrite::Resolve {
                resolved: false,
                ..
            }
        ));
        assert_eq!(state.notice.as_deref(), Some("Unresolving…"));
        written(&mut state, &sent, Ok(Written::Resolved(false)));
        assert!(!state.review.discussion.as_ref().unwrap().threads[0].resolved);
        assert_eq!(state.notice.as_deref(), Some("Thread unresolved"));
    }

    #[test]
    fn a_resolve_answer_that_disagrees_is_a_failure() {
        let mut state = reading();
        let sent = sent(&type_char(&mut state, 'R'));
        written(&mut state, &sent, Ok(Written::Resolved(false)));
        assert!(!state.review.discussion.as_ref().unwrap().threads[0].resolved);
        assert_eq!(state.review.writing, None);
        assert_eq!(
            state.notice.as_deref(),
            Some("GitHub gave no usable answer.")
        );
        // A failed resolve carries no "may have been posted" warning.
        let sent = super::tests::sent(&type_char(&mut state, 'R'));
        written(
            &mut state,
            &sent,
            Err(CreateError::Status(ForgeStatus::Offline)),
        );
        assert_eq!(state.notice.as_deref(), Some(OFFLINE));
    }

    #[test]
    fn a_line_comment_may_be_typed_while_a_resolve_runs() {
        use crate::features::review::pending::CommentTarget;
        use crate::features::review::pending::tests::{HEAD, right};
        let mut state = reading();
        sent(&type_char(&mut state, 'R'));
        press(&mut state, KeyCode::Esc);
        state.review.composer = Some(Composer::new(ComposerKind::Line {
            target: CommentTarget {
                path: "src/cache.rs".into(),
                start: None,
                end: right(1),
                head: HEAD.into(),
            },
            editing: None,
        }));
        assert!(!state.composer_posting());
        type_text(&mut state, "typed");
        assert_eq!(composer(&state).text.text(), "typed");
    }

    #[test]
    fn the_reader_keys_say_why_they_cannot_write() {
        let refused = |state: &mut AppState, want: &str| {
            assert_eq!(
                thread_write_disabled(state).map(String::from).as_deref(),
                Some(want)
            );
            assert_eq!(
                state.update(AppIntent::Review(ReviewIntent::ToggleThreadResolved)),
                []
            );
            assert_eq!(state.notice.as_deref(), Some(want));
            assert_eq!(
                state.update(AppIntent::Review(ReviewIntent::ReplyToThread)),
                []
            );
            assert_eq!(state.review.composer, None);
            assert_eq!(state.review.writing, None);
        };
        assert_eq!(thread_write_disabled(&reading()), None);

        let mut state = reading();
        state.review.discussion.as_mut().unwrap().threads[0].node_id = None;
        refused(&mut state, NO_THREAD_ID);

        let mut state = reading();
        state.forge.hosts.clear();
        refused(&mut state, NO_FORGE);

        let mut state = reading();
        state.forge.limited.insert("github.com".into(), NOW + 60);
        refused(&mut state, REFRESH_LIMITED);

        let mut state = two_files();
        refused(&mut state, NO_THREAD);
    }

    #[test]
    fn a_on_the_conversation_opens_a_general_comment_and_y_posts_it() {
        let mut state = conversing();
        assert_eq!(general_comment_disabled(&state), None);
        assert_eq!(type_char(&mut state, 'a'), []);
        assert_eq!(state.key_focus(), KeyScope::ReviewComment);
        assert_eq!(
            composer(&state).kind,
            ComposerKind::General {
                number: 12,
                subject: "PR_kwDOA1".into(),
            }
        );
        type_text(&mut state, "LGTM ✓");
        assert_eq!(ctrl(&mut state, 's'), []);
        assert_eq!(
            question_text(&state).as_deref(),
            Some("Post this comment on #12 to GitHub? (y/n)")
        );
        assert_eq!(type_char(&mut state, 'n'), []);
        assert_eq!(composer(&state).text.text(), "LGTM ✓");
        assert_eq!(ctrl(&mut state, 's'), []);
        let sent = sent(&type_char(&mut state, 'y'));
        assert_eq!(
            sent.2,
            DiscussionWrite::Comment {
                subject: "PR_kwDOA1".into(),
                body: "LGTM ✓".into(),
            }
        );
        assert_eq!(state.notice.as_deref(), Some("Posting comment…"));
        let effects = written(&mut state, &sent, Ok(Written::Posted));
        let [
            Effect::LoadPrDiscussion {
                tag, number: 12, ..
            },
        ] = effects.as_slice()
        else {
            panic!("{effects:?}");
        };
        assert_eq!(state.review.composer, None);
        assert_eq!(state.notice.as_deref(), Some("Comment posted"));
        // The re-read shows the new comment at the bottom.
        let mut discussion: PrDiscussion = fixture_discussion();
        let mut posted = discussion.conversation[0].clone();
        posted.body = "LGTM ✓ the new one".into();
        discussion.conversation.push(posted);
        state.size = (80, 12);
        answer(&mut state, *tag, Ok(Some(discussion)));
        assert_eq!(state.review.screen, ReviewScreen::Conversation(12));
        let drawn = state.review.conversation.as_ref().unwrap();
        let rows = usize::from(state.review_area().height);
        assert!(drawn.lines.len() > rows, "{} lines", drawn.lines.len());
        assert_eq!(state.review.conversation_scroll, drawn.lines.len() - rows);
    }

    #[test]
    fn a_on_the_conversation_is_refused_without_an_id_or_a_host() {
        let mut state = conversing();
        state.review.detail.as_mut().unwrap().node_id = None;
        assert_eq!(
            general_comment_disabled(&state)
                .map(String::from)
                .as_deref(),
            Some(NO_PR_ID)
        );
        assert_eq!(type_char(&mut state, 'a'), []);
        assert_eq!(state.notice.as_deref(), Some(NO_PR_ID));
        assert_eq!(state.review.composer, None);

        let mut state = conversing();
        state.forge.hosts.clear();
        assert_eq!(
            state.update(AppIntent::Review(ReviewIntent::StartGeneralComment)),
            []
        );
        assert_eq!(state.notice.as_deref(), Some(NO_FORGE));
        assert_eq!(state.review.composer, None);

        // Not on the detail either.
        let mut state = conversing();
        press(&mut state, KeyCode::Esc);
        assert_eq!(
            general_comment_disabled(&state)
                .map(String::from)
                .as_deref(),
            Some(NOT_CONVERSATION)
        );
        assert_eq!(
            state.update(AppIntent::Review(ReviewIntent::StartGeneralComment)),
            []
        );
        assert_eq!(state.review.composer, None);
    }

    #[test]
    fn the_host_is_checked_again_at_the_y() {
        let mut state = conversing();
        type_char(&mut state, 'a');
        type_text(&mut state, "hi");
        ctrl(&mut state, 's');
        state.forge.hosts.clear();
        assert_eq!(type_char(&mut state, 'y'), []);
        assert_eq!(state.notice.as_deref(), Some(NO_FORGE));
        assert_eq!(state.review.writing, None);
        assert_eq!(composer(&state).text.text(), "hi");
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

    fn drawn(rows: &[String], text: &str) -> bool {
        rows.iter().any(|r| r.contains(text))
    }

    #[test]
    fn the_reader_header_shows_the_state_github_took() {
        let mut state = reading();
        let rows = draw(&state);
        assert!(drawn(&rows, "· unresolved ·"), "{rows:#?}");
        let sent = sent(&type_char(&mut state, 'R'));
        written(&mut state, &sent, Ok(Written::Resolved(true)));
        let rows = draw(&state);
        assert!(drawn(&rows, "· resolved ·"), "{rows:#?}");
        assert!(!drawn(&rows, "· unresolved ·"), "{rows:#?}");
    }

    #[test]
    fn the_reply_composer_names_the_thread_and_says_it_posts() {
        let mut state = reading();
        type_char(&mut state, 'a');
        type_text(&mut state, "On it");
        let rows = draw(&state);
        let title = rows
            .iter()
            .find(|r| r.contains("Reply to src/cache.rs"))
            .unwrap_or_else(|| panic!("{rows:#?}"));
        assert!(title.contains("ctrl-s posts to GitHub"), "{title}");
        assert!(drawn(&rows, "On it"), "{rows:#?}");
    }

    #[test]
    fn the_general_composer_draws_over_the_conversation() {
        let mut state = conversing();
        type_char(&mut state, 'a');
        type_text(&mut state, "Ship it");
        let rows = draw(&state);
        assert!(
            drawn(&rows, "Comment on #12 · ctrl-s posts to GitHub"),
            "{rows:#?}"
        );
        assert!(drawn(&rows, "Ship it"), "{rows:#?}");
        // The posting question names where it goes.
        ctrl(&mut state, 's');
        let rows = draw(&state);
        assert!(
            drawn(&rows, "Post this comment on #12 to GitHub? (y/n)"),
            "{rows:#?}"
        );
    }

    #[test]
    fn the_bar_offers_reply_and_resolve_in_the_reader_and_comment_only_on_the_conversation() {
        let state = reading();
        let rows = draw(&state);
        let bar = rows.last().unwrap();
        assert!(bar.contains("a reply"), "{bar:?}");
        assert!(bar.contains("R resolve"), "{bar:?}");

        let state = conversing();
        let bar = draw(&state).last().unwrap().clone();
        assert!(bar.contains("a comment"), "{bar:?}");
        // Not on the detail, where `a` means nothing.
        let mut state = conversing();
        press(&mut state, KeyCode::Esc);
        let bar = draw(&state).last().unwrap().clone();
        assert!(!bar.contains("a comment"), "{bar:?}");
    }

    /// `state` drawn at `width` x `height`, resized to it first.
    fn draw_at(state: &mut AppState, width: u16, height: u16) -> Vec<String> {
        state.update(AppIntent::Resize { width, height });
        draw(state)
    }

    const SIZES: [(u16, u16); 5] = [(80, 24), (40, 12), (24, 8), (20, 6), (1, 1)];

    #[test]
    fn the_may_have_been_posted_warning_survives_an_80_column_bar() {
        for error in [
            CreateError::Status(ForgeStatus::Offline),
            CreateError::Status(ForgeStatus::Failed),
        ] {
            for start in [reading as fn() -> AppState, conversing] {
                let mut state = start();
                state.update(AppIntent::Resize {
                    width: 80,
                    height: 24,
                });
                let sent = if matches!(state.review.screen, ReviewScreen::Conversation(_)) {
                    type_char(&mut state, 'a');
                    type_text(&mut state, "Done");
                    ctrl(&mut state, 's');
                    super::tests::sent(&type_char(&mut state, 'y'))
                } else {
                    post_reply(&mut state, "Done")
                };
                written(&mut state, &sent, Err(error.clone()));
                let rows = draw_at(&mut state, 80, 24);
                let bar = rows.last().unwrap();
                assert!(
                    bar.contains("May have been posted - check before posting again"),
                    "{error:?}: {bar:?}"
                );
            }
        }
    }

    #[test]
    fn a_long_reply_label_gives_way_to_the_posting_cue() {
        let mut state = reading();
        type_char(&mut state, 'a');
        if let Some(Composer {
            kind: ComposerKind::Reply { label, .. },
            ..
        }) = &mut state.review.composer
        {
            *label = format!("{}/漢字ファイル名.rs · lines 10-14", "deep/".repeat(30));
        }
        let rows = draw_at(&mut state, 80, 24);
        let title = rows
            .iter()
            .find(|r| r.contains("Reply to "))
            .unwrap_or_else(|| panic!("{rows:#?}"));
        assert!(title.contains("…"), "{title}");
        assert!(
            title.contains("lines 10-14 · ctrl-s posts to GitHub"),
            "{title}"
        );
        // Narrower: the label goes, the cue stays.
        let rows = draw_at(&mut state, 40, 12);
        assert!(drawn(&rows, "posts to GitHub"), "{rows:#?}");
        for (width, height) in SIZES {
            draw_at(&mut state, width, height);
        }
    }

    #[test]
    fn the_composers_and_the_question_draw_at_every_size() {
        let mut reply = reading();
        type_char(&mut reply, 'a');
        type_text(&mut reply, "On it");
        let mut general = conversing();
        type_char(&mut general, 'a');
        type_text(&mut general, "Ship it");
        let mut asked = conversing();
        type_char(&mut asked, 'a');
        type_text(&mut asked, "Ship it");
        ctrl(&mut asked, 's');
        for (width, height) in SIZES {
            for state in [&mut reply, &mut general, &mut asked] {
                let rows = draw_at(state, width, height);
                assert_eq!(rows.len(), usize::from(height));
            }
        }
        let rows = draw_at(&mut reply, 80, 24);
        assert!(drawn(&rows, "ctrl-s posts to GitHub"), "{rows:#?}");
        assert!(drawn(&rows, "On it"), "{rows:#?}");
        let rows = draw_at(&mut general, 80, 24);
        assert!(
            drawn(&rows, " Comment on #12 · ctrl-s posts to GitHub "),
            "{rows:#?}"
        );
        assert!(drawn(&rows, "Ship it"), "{rows:#?}");
        // The smallest box still says the text goes to GitHub.
        let rows = draw_at(&mut general, 30, 10);
        assert!(drawn(&rows, "posts to GitHub"), "{rows:#?}");
        let rows = draw_at(&mut asked, 80, 24);
        assert!(
            drawn(&rows, "Post this comment on #12 to GitHub? (y/n)"),
            "{rows:#?}"
        );
    }

    #[test]
    fn the_title_says_posting_while_the_text_is_on_its_way() {
        let mut state = reading();
        post_reply(&mut state, "Done");
        let rows = draw_at(&mut state, 80, 24);
        let title = rows
            .iter()
            .find(|r| r.contains("Reply to "))
            .unwrap_or_else(|| panic!("{rows:#?}"));
        assert!(title.contains("· posting…"), "{title}");
        assert!(!title.contains("ctrl-s posts"), "{title}");
    }

    #[test]
    fn the_reader_and_conversation_bars_keep_their_write_keys_at_80_columns() {
        let mut state = reading();
        let rows = draw_at(&mut state, 80, 24);
        let bar = rows.last().unwrap();
        assert!(bar.contains("a reply"), "{bar:?}");
        assert!(bar.contains("R resolve"), "{bar:?}");
        let mut state = conversing();
        let rows = draw_at(&mut state, 80, 24);
        let bar = rows.last().unwrap();
        assert!(bar.contains("a comment"), "{bar:?}");
        for (width, height) in SIZES {
            draw_at(&mut reading(), width, height);
            draw_at(&mut conversing(), width, height);
        }
    }
}
