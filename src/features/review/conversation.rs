//! The open pull request's conversation: its issue comments and review
//! bodies, oldest first, as the discussion read after the detail holds
//! them. `C` on the detail shows it and `esc` goes back; nothing here asks
//! GitHub anything. Drawn by `conversation_view.rs`.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::markdown;
use super::state::{RenderedBody, ReviewScreen};
use super::threads_view::short_time;
use crate::keymap::Reason;
use crate::model::AppState;
use kika_forge::discussion::{ConversationKind, PrDiscussion};
use kika_forge::forge::Words;
use kika_forge::review::ReviewVerdict;

/// What a key that acts on the detail says on the conversation.
fn back_first(words: &Words) -> String {
    format!("Go back to the {} first", words.request)
}

pub(super) fn back_to_refresh(words: &Words) -> String {
    format!("Go back to the {} to refresh", words.request)
}

fn no_pr_open(words: &Words) -> String {
    format!("Open a {} first", words.request)
}

const COMMENTS_LOADING: &str = "Comments are still loading";
const COMMENTS_FAILED: &str = "Comments could not be read (r to try again)";
const NOTHING_TO_PAGE: &str = "Nothing to page here";
fn no_comments(words: &Words) -> String {
    format!("No comments on this {}", words.request)
}

/// Refuses a key meant for the detail while the conversation shows.
pub(crate) fn on_conversation(state: &AppState) -> Option<Reason> {
    matches!(state.review.screen, ReviewScreen::Conversation(_))
        .then(|| back_first(state.review_words()).into())
}

/// Why `C` cannot show the conversation: no pull request's detail is
/// shown, or its comments are loading or could not be read.
pub(crate) fn conversation_disabled(state: &AppState) -> Option<Reason> {
    let review = &state.review;
    match review.screen {
        ReviewScreen::Detail(_) => {}
        ReviewScreen::Queue => return Some(no_pr_open(state.review_words()).into()),
        ReviewScreen::Files(_) | ReviewScreen::Conversation(_) => {
            return Some(back_first(state.review_words()).into());
        }
    }
    if review.discussion_fetch.error.is_some() {
        return Some(COMMENTS_FAILED.into());
    }
    // Loading, or not asked yet while the detail loads.
    review
        .discussion
        .is_none()
        .then_some(COMMENTS_LOADING.into())
}

/// Why the page keys have nothing to scroll.
pub(crate) fn page_disabled(state: &AppState) -> Option<Reason> {
    let pages = matches!(
        state.review.screen,
        ReviewScreen::Detail(_) | ReviewScreen::Conversation(_)
    );
    (!pages).then_some(NOTHING_TO_PAGE.into())
}

/// What a conversation item did, in words.
fn verb(kind: ConversationKind) -> &'static str {
    match kind {
        ConversationKind::Comment | ConversationKind::Review(ReviewVerdict::Commented) => {
            "commented"
        }
        ConversationKind::Review(ReviewVerdict::Approved) => "approved",
        ConversationKind::Review(ReviewVerdict::ChangesRequested) => "requested changes",
        ConversationKind::Review(ReviewVerdict::Dismissed) => "dismissed review",
        ConversationKind::Review(ReviewVerdict::Pending) => "pending review",
    }
}

/// Every line of the conversation at `width`: each item's header, its
/// body as markdown and a blank row, then a note when the forge has more.
pub fn conversation_lines(
    discussion: &PrDiscussion,
    words: &Words,
    width: usize,
) -> Vec<Line<'static>> {
    let items = &discussion.conversation;
    if items.is_empty() {
        return vec![Line::raw(no_comments(words))];
    }
    let dim = Style::new().add_modifier(Modifier::DIM);
    let mut lines = Vec::new();
    for item in items {
        lines.push(Line::from(vec![
            Span::styled(
                item.author.clone(),
                Style::new().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" · {} · {}", short_time(&item.created), verb(item.kind)),
                dim,
            ),
        ]));
        lines.extend(markdown::render(&item.body, width));
        lines.push(Line::default());
    }
    if discussion.conversation_capped {
        lines.push(Line::styled(
            format!("More on {} (showing the first {})", words.name, items.len()),
            dim,
        ));
    }
    lines
}

impl AppState {
    /// `C`: the detail's conversation, from its top.
    pub(crate) fn open_review_conversation(&mut self) {
        if let Some(reason) = conversation_disabled(self) {
            self.notice = Some(reason);
            return;
        }
        let ReviewScreen::Detail(number) = self.review.screen else {
            return;
        };
        self.review.screen = ReviewScreen::Conversation(number);
        self.review.conversation = None;
        self.review.conversation_scroll = 0;
        self.render_review_conversation();
    }

    /// `esc`: back to the detail, as it was left.
    pub(super) fn close_review_conversation(&mut self, number: u64) {
        self.review.screen = ReviewScreen::Detail(number);
        self.review.conversation = None;
        self.review.conversation_scroll = 0;
    }

    /// Render the conversation for the panel's width, once: when it opens
    /// and when the screen is resized.
    pub(super) fn render_review_conversation(&mut self) {
        if !matches!(self.review.screen, ReviewScreen::Conversation(_)) {
            return;
        }
        let width = usize::from(self.review_area().width);
        let drawn = self.review.conversation.as_ref();
        if !drawn.is_some_and(|c| c.width == width) {
            let words = self.review_words();
            self.review.conversation = self.review.discussion.as_ref().map(|d| RenderedBody {
                width,
                lines: conversation_lines(d, words, width),
            });
        }
        // A taller screen may have room for lines scrolled past.
        self.review.conversation_scroll = self
            .review
            .conversation_scroll
            .min(self.conversation_max_scroll());
    }

    /// The furthest the conversation scrolls, with its last line at the
    /// bottom.
    fn conversation_max_scroll(&self) -> usize {
        let lines = self
            .review
            .conversation
            .as_ref()
            .map_or(0, |c| c.lines.len());
        lines.saturating_sub(usize::from(self.review_area().height))
    }

    pub(super) fn scroll_review_conversation(&mut self, delta: isize) {
        let max = self.conversation_max_scroll();
        self.review.conversation_scroll = self
            .review
            .conversation_scroll
            .saturating_add_signed(delta)
            .min(max);
    }

    /// `pgdn`/`pgup` (`pages` 1 or -1): the detail or the conversation by
    /// the panel's height less a line, so one line stays in sight.
    pub(crate) fn review_page(&mut self, pages: isize) {
        if let Some(reason) = page_disabled(self) {
            self.notice = Some(reason);
            return;
        }
        let page = usize::from(self.review_area().height)
            .saturating_sub(1)
            .max(1);
        let delta = pages.saturating_mul(isize::try_from(page).unwrap_or(isize::MAX));
        match self.review.screen {
            ReviewScreen::Conversation(_) => self.scroll_review_conversation(delta),
            ReviewScreen::Detail(_) => self.scroll_review_detail(delta),
            ReviewScreen::Queue | ReviewScreen::Files(_) => {}
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use crossterm::event::KeyCode;

    use super::*;
    use crate::effect::Effect;
    use crate::features::review::ReviewIntent;
    use crate::features::review::commit_view::tests::assert_local;
    use crate::features::review::state::ReviewProblem;
    use crate::features::review::state::tests::{in_queues, press, type_char};
    use crate::features::review::threads::tests::{
        answer, detail_then_load, files_with, fixture_discussion,
    };
    use crate::intent::AppIntent;
    use kika_forge::ForgeStatus;
    use kika_forge::discussion::ConversationItem;
    use kika_forge::forge::words_or_default;

    /// The texts as GitHub words them.
    pub(crate) const BACK_FIRST: &str = "Go back to the pull request first";
    pub(crate) const BACK_TO_REFRESH: &str = "Go back to the pull request to refresh";
    const NO_PR_OPEN: &str = "Open a pull request first";
    pub(crate) const NO_COMMENTS: &str = "No comments on this pull request";

    /// Pull request 12's detail with `discussion` read.
    pub(crate) fn detail_with(discussion: PrDiscussion) -> AppState {
        let (mut state, tag) = detail_then_load();
        answer(&mut state, tag, Ok(Some(discussion)));
        state
    }

    /// The fixture's conversation with `n` more one-line comments, so it
    /// is longer than the screen.
    pub(crate) fn long_discussion(n: usize) -> PrDiscussion {
        let mut discussion = fixture_discussion();
        for i in 0..n {
            discussion.conversation.push(ConversationItem {
                author: format!("user{i}"),
                created: format!("2023-11-16T10:{:02}:00Z", i % 60),
                body: format!("Comment number {i}."),
                kind: ConversationKind::Comment,
            });
        }
        discussion
    }

    #[test]
    fn c_on_a_loaded_detail_shows_the_conversation_and_esc_goes_back() {
        let mut state = detail_with(fixture_discussion());
        state.review.detail_scroll = 0;
        let detail = state.review.detail.clone();
        let effects = state.update(AppIntent::Review(ReviewIntent::OpenConversation));
        assert_eq!(effects, []);
        assert_eq!(state.review.screen, ReviewScreen::Conversation(12));
        assert_eq!(state.review.conversation_scroll, 0);
        assert!(state.review.conversation.is_some());
        // By the key too, from the detail.
        assert_eq!(press(&mut state, KeyCode::Esc), []);
        assert_eq!(state.review.screen, ReviewScreen::Detail(12));
        assert_eq!(state.review.detail, detail);
        assert_eq!(state.review.discussion, Some(fixture_discussion()));
        assert_eq!(state.review.conversation, None);
        assert_eq!(type_char(&mut state, 'C'), []);
        assert_eq!(state.review.screen, ReviewScreen::Conversation(12));
    }

    #[test]
    fn c_says_why_it_cannot_show_the_conversation() {
        // The queue.
        let mut state = in_queues();
        assert_eq!(type_char(&mut state, 'C'), []);
        assert_eq!(state.notice.as_deref(), Some(NO_PR_OPEN));
        assert_eq!(state.review.screen, ReviewScreen::Queue);
        // The files screen.
        let mut state = files_with(&["src/cache.rs"], fixture_discussion());
        assert_eq!(type_char(&mut state, 'C'), []);
        assert_eq!(state.notice.as_deref(), Some(BACK_FIRST));
        assert_eq!(state.review.screen, ReviewScreen::Files(12));
        // Still loading.
        let (mut state, _) = detail_then_load();
        assert_eq!(type_char(&mut state, 'C'), []);
        assert_eq!(state.notice.as_deref(), Some(COMMENTS_LOADING));
        assert_eq!(state.review.screen, ReviewScreen::Detail(12));
        // Failed.
        let (mut state, tag) = detail_then_load();
        answer(
            &mut state,
            tag,
            Err(kika_forge::review::ReviewError::Status(
                ForgeStatus::Offline,
            )),
        );
        assert_eq!(
            state.review.discussion_fetch.error,
            Some(ReviewProblem::Status(ForgeStatus::Offline))
        );
        assert_eq!(type_char(&mut state, 'C'), []);
        assert_eq!(state.notice.as_deref(), Some(COMMENTS_FAILED));
        assert_eq!(
            conversation_disabled(&state).as_deref(),
            Some(COMMENTS_FAILED)
        );
    }

    #[test]
    fn the_conversation_scrolls_and_pages_within_its_lines_and_refuses_detail_keys() {
        let mut state = detail_with(long_discussion(40));
        state.size = (100, 30);
        let mut effects = state.update(AppIntent::Review(ReviewIntent::OpenConversation));
        let lines = state.review.conversation.as_ref().unwrap().lines.len();
        let height = usize::from(state.review_area().height);
        let max = lines - height;
        assert!(max > height, "{lines} lines, {height} rows");

        effects.extend(type_char(&mut state, 'k'));
        assert_eq!(state.review.conversation_scroll, 0);
        effects.extend(press(&mut state, KeyCode::PageUp));
        assert_eq!(state.review.conversation_scroll, 0);
        effects.extend(type_char(&mut state, 'j'));
        assert_eq!(state.review.conversation_scroll, 1);
        effects.extend(press(&mut state, KeyCode::PageDown));
        assert_eq!(state.review.conversation_scroll, height);
        effects.extend(press(&mut state, KeyCode::Up));
        assert_eq!(state.review.conversation_scroll, height - 1);
        for _ in 0..20 {
            effects.extend(press(&mut state, KeyCode::PageDown));
        }
        assert_eq!(state.review.conversation_scroll, max);
        effects.extend(type_char(&mut state, 'j'));
        assert_eq!(state.review.conversation_scroll, max);
        effects.extend(press(&mut state, KeyCode::PageUp));
        assert_eq!(state.review.conversation_scroll, max - (height - 1));

        for (key, notice) in [
            ('r', BACK_TO_REFRESH),
            ('f', BACK_FIRST),
            ('M', BACK_FIRST),
            ('o', BACK_FIRST),
        ] {
            state.notice = None;
            let got = type_char(&mut state, key);
            assert_eq!(got, [], "{key}");
            assert_eq!(state.notice.as_deref(), Some(notice), "{key}");
            assert_eq!(state.review.screen, ReviewScreen::Conversation(12));
        }
        // Nor do the queue's keys or enter move anything.
        for code in [KeyCode::Enter, KeyCode::Tab, KeyCode::BackTab] {
            effects.extend(press(&mut state, code));
            assert_eq!(state.review.screen, ReviewScreen::Conversation(12));
        }
        effects.extend(type_char(&mut state, '/'));
        assert_eq!(state.input, None);
        effects.extend(type_char(&mut state, 'C'));
        assert_eq!(state.notice.as_deref(), Some(BACK_FIRST));
        effects.extend(press(&mut state, KeyCode::Esc));
        assert_local(&effects);
        assert_eq!(effects, []);
    }

    #[test]
    fn the_page_keys_page_the_detail_and_nothing_else() {
        let mut state = detail_with(fixture_discussion());
        state.size = (80, 8);
        press(&mut state, KeyCode::PageDown);
        let max = state.review_max_scroll();
        assert!(max > 0);
        assert_eq!(
            state.review.detail_scroll,
            (usize::from(state.review_area().height) - 1).min(max)
        );
        press(&mut state, KeyCode::PageUp);
        assert_eq!(state.review.detail_scroll, 0);

        let mut state = in_queues();
        assert_eq!(press(&mut state, KeyCode::PageDown), []);
        assert_eq!(state.notice.as_deref(), Some(NOTHING_TO_PAGE));
        // On the files list the review's own page keys say so; the diff's
        // win once it has the keys.
        let mut state = files_with(&["src/cache.rs"], fixture_discussion());
        assert_eq!(press(&mut state, KeyCode::PageDown), []);
        assert_eq!(state.notice.as_deref(), Some(NOTHING_TO_PAGE));
        state.notice = None;
        press(&mut state, KeyCode::Tab);
        press(&mut state, KeyCode::PageDown);
        assert_eq!(state.notice, None);
    }

    #[test]
    fn a_resize_renders_the_conversation_for_the_new_width() {
        let mut state = detail_with(fixture_discussion());
        type_char(&mut state, 'C');
        assert_eq!(
            state.review.conversation.as_ref().map(|c| c.width),
            Some(78)
        );
        state.update(AppIntent::Resize {
            width: 50,
            height: 20,
        });
        assert_eq!(
            state.review.conversation.as_ref().map(|c| c.width),
            Some(48)
        );
        // Scrolled to the end, a taller screen of the same width pulls it back.
        let mut long = detail_with(long_discussion(40));
        type_char(&mut long, 'C');
        for _ in 0..200 {
            type_char(&mut long, 'j');
        }
        assert!(long.review.conversation_scroll > 0);
        long.update(AppIntent::Resize {
            width: 80,
            height: 200,
        });
        assert_eq!(long.review.conversation_scroll, 0);
        // Leaving the pull request drops it.
        press(&mut state, KeyCode::Esc);
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.review.screen, ReviewScreen::Queue);
        assert_eq!(state.review.conversation, None);
    }

    #[test]
    fn the_conversation_lines_say_who_when_and_what() {
        let mut discussion = fixture_discussion();
        for (kind, word) in [
            (
                ConversationKind::Review(ReviewVerdict::Approved),
                "approved",
            ),
            (
                ConversationKind::Review(ReviewVerdict::ChangesRequested),
                "requested changes",
            ),
            (
                ConversationKind::Review(ReviewVerdict::Dismissed),
                "dismissed review",
            ),
            (
                ConversationKind::Review(ReviewVerdict::Pending),
                "pending review",
            ),
        ] {
            discussion.conversation[0].kind = kind;
            let first = conversation_lines(&discussion, words_or_default(None), 60)[0].to_string();
            assert_eq!(first, format!("frank · 2023-11-14 20:00 · {word}"));
        }
        let mut empty = fixture_discussion();
        empty.conversation.clear();
        empty.conversation_capped = true;
        let lines = conversation_lines(&empty, words_or_default(None), 60);
        assert_eq!(lines, [Line::raw(NO_COMMENTS)]);
    }

    /// An `update` on this screen that returns an effect fails here.
    #[test]
    fn opening_and_leaving_the_conversation_returns_no_effect() {
        let mut state = detail_with(fixture_discussion());
        let effects: Vec<Effect> = [
            AppIntent::Review(ReviewIntent::OpenConversation),
            AppIntent::Review(ReviewIntent::Next),
            AppIntent::Review(ReviewIntent::PageDown),
            AppIntent::Review(ReviewIntent::Refresh),
            AppIntent::Review(ReviewIntent::OpenFiles),
            AppIntent::Review(ReviewIntent::OpenMergePr),
            AppIntent::Review(ReviewIntent::OpenCheckDetails),
            AppIntent::Review(ReviewIntent::OpenPr),
            AppIntent::Review(ReviewIntent::CloseDetail),
        ]
        .into_iter()
        .flat_map(|intent| state.update(intent))
        .collect();
        assert_local(&effects);
        assert_eq!(effects, []);
        assert_eq!(state.review.screen, ReviewScreen::Detail(12));
    }
}
