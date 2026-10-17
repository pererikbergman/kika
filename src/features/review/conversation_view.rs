//! The conversation screen: the lines `conversation.rs` rendered, from
//! the scroll, each cut to the panel. Reads state only; the forge's text
//! was made terminal-safe when its answer was parsed.

use ratatui::layout::Rect;
use ratatui::text::Line;

use super::conversation::conversation_lines;
use super::state::ReviewState;
use super::threads_view::clip_line;
use kika_forge::forge::Words;

/// The conversation's rows for `inner`.
pub fn conversation_view(review: &ReviewState, words: &Words, inner: Rect) -> Vec<Line<'static>> {
    let (width, rows) = (usize::from(inner.width), usize::from(inner.height));
    let fresh;
    let lines = match (&review.conversation, &review.discussion) {
        (Some(drawn), _) if drawn.width == width => &drawn.lines,
        // Not rendered for this width yet: drawn here this once.
        (_, Some(discussion)) => {
            fresh = conversation_lines(discussion, words, width);
            &fresh
        }
        (_, None) => return Vec::new(),
    };
    lines
        .iter()
        .skip(review.conversation_scroll)
        .take(rows)
        .map(|line| clip_line(line.clone(), width))
        .collect()
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::features::review::conversation::tests::NO_COMMENTS;
    use crate::features::review::conversation::tests::{detail_with, long_discussion};
    use crate::features::review::state::tests::type_char;
    use crate::features::review::threads::tests::{discussion_from, fixture_discussion};
    use crate::model::AppState;
    use crate::view::{buffer_rows, width_of};
    use kika_forge::discussion::PrDiscussion;
    use kika_forge::discussion::fixtures::hostile_discussion;

    fn draw(state: &AppState, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| crate::view::render(state, frame))
            .unwrap();
        buffer_rows(terminal.backend().buffer())
    }

    fn row_with(rows: &[String], text: &str) -> usize {
        rows.iter()
            .position(|r| r.contains(text))
            .unwrap_or_else(|| panic!("{text:?} not in {rows:#?}"))
    }

    fn shown(discussion: PrDiscussion) -> AppState {
        let mut state = detail_with(discussion);
        state.size = (100, 30);
        type_char(&mut state, 'C');
        state
    }

    #[test]
    fn the_conversation_shows_each_item_in_time_order_with_its_verb_and_body() {
        let mut discussion = fixture_discussion();
        discussion.conversation[1].body = "**Looks** good:\n\n- one\n- two".into();
        discussion.conversation[2].kind = kika_forge::discussion::ConversationKind::Review(
            kika_forge::review::ReviewVerdict::ChangesRequested,
        );
        let state = shown(discussion);
        let rows = draw(&state, 100, 30);
        assert!(rows[0].contains("Review · acme/widgets #12"), "{rows:#?}");
        let frank = row_with(&rows, "frank · 2023-11-14 20:00 · commented");
        let alice = row_with(&rows, "alice · 2023-11-14 22:00 · commented");
        let grace = row_with(&rows, "grace · 2023-11-15 09:00 · requested changes");
        assert!(frank < alice && alice < grace, "{rows:#?}");
        assert!(rows[frank + 1].contains("Thanks for this!"), "{rows:#?}");
        // Markdown: the stars are gone, the list has its bullets.
        assert!(rows[alice + 1].contains("Looks good:"), "{rows:#?}");
        assert!(!rows[alice + 1].contains("**"), "{rows:#?}");
        assert!(rows.iter().any(|r| r.contains("• one")), "{rows:#?}");
        assert!(rows[grace + 1].contains("Ping"), "{rows:#?}");
        assert!(!rows.iter().any(|r| r.contains("More on GitHub")));

        let mut capped = fixture_discussion();
        capped.conversation_capped = true;
        let rows = draw(&shown(capped), 100, 30);
        let more = row_with(&rows, "More on GitHub (showing the first 3)");
        assert!(more > row_with(&rows, "Ping"), "{rows:#?}");

        // On GitLab, rendered for the screen again: GitLab has the rest.
        let mut capped = fixture_discussion();
        capped.conversation_capped = true;
        let mut state = crate::features::review::state::tests::on_gitlab(shown(capped));
        state.review.conversation = None;
        let rows = draw(&state, 100, 30);
        row_with(&rows, "More on GitLab (showing the first 3)");
        let mut empty = fixture_discussion();
        empty.conversation.clear();
        let mut state = crate::features::review::state::tests::on_gitlab(shown(empty));
        state.review.conversation = None;
        row_with(&draw(&state, 100, 30), "No comments on this merge request");

        let mut empty = fixture_discussion();
        empty.conversation.clear();
        let rows = draw(&shown(empty), 100, 30);
        row_with(&rows, NO_COMMENTS);
    }

    #[test]
    fn the_bar_offers_c_only_where_it_shows_the_conversation() {
        use crate::features::review::threads::tests::{detail_then_load, files_with};
        let bar = |state: &AppState| draw(state, 160, 30).last().cloned().unwrap();
        let mut state = detail_with(fixture_discussion());
        state.size = (160, 30);
        assert!(bar(&state).contains("C conversation"), "{}", bar(&state));
        let (loading, _) = detail_then_load();
        assert!(!bar(&loading).contains("conversation"), "{}", bar(&loading));
        let files = files_with(&["src/cache.rs"], fixture_discussion());
        assert!(!bar(&files).contains("conversation"), "{}", bar(&files));
        type_char(&mut state, 'C');
        assert!(!bar(&state).contains("conversation"), "{}", bar(&state));
    }

    #[test]
    fn scrolling_moves_the_conversation() {
        let mut state = shown(long_discussion(40));
        let rows = draw(&state, 100, 30);
        assert!(rows.iter().any(|r| r.contains("frank ·")), "{rows:#?}");
        for _ in 0..30 {
            type_char(&mut state, 'j');
        }
        let rows = draw(&state, 100, 30);
        assert!(!rows.iter().any(|r| r.contains("frank ·")), "{rows:#?}");
        assert!(rows.iter().any(|r| r.contains("user")), "{rows:#?}");
    }

    #[test]
    fn hostile_text_draws_escaped_and_no_size_overruns_the_panel() {
        let mut state = shown(discussion_from(&hostile_discussion()));
        let rows = draw(&state, 100, 30);
        assert!(rows.iter().any(|r| r.contains("<U+202E>")), "{rows:#?}");
        for (w, h) in [(80u16, 24u16), (40, 10), (20, 6), (100, 30)] {
            state.size = (w, h);
            state.render_review_body();
            let rows = draw(&state, w, h);
            assert_eq!(rows.len(), usize::from(h));
            for row in &rows {
                assert!(width_of(row) <= usize::from(w), "{w}x{h}: {row:?}");
                assert!(!row.chars().any(char::is_control), "{row:?}");
            }
            // The panel's right border closes every row inside it.
            for row in &rows[1..rows.len() - 2] {
                let last = row.chars().last().unwrap_or(' ');
                assert!(matches!(last, '│' | '┃'), "{w}x{h}: {row:?}");
            }
        }
    }
}
