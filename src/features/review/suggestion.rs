//! Code suggestions: `s` in pick mode opens the line-comment composer
//! with a suggestion block holding the picked lines exactly as they are
//! at the head, so the author can apply the change on the forge in one
//! click. Each forge has its own fence (`suggestion`, or GitLab's
//! `suggestion:-N+0`, counting the lines above the note's line). The
//! lines are read raw from the head's blob, because the diff's text is
//! made terminal-safe and would not match the file.

use super::comment_editor::{CommentEditor, Composer, ComposerKind};
use super::comment_pick::live_pick;
use super::pending::{CommentTarget, comment_disabled};
use super::state::ReviewFetch;
use crate::effect::{Effect, Tag};
use crate::model::AppState;
use kika_forge::anchor::GhSide;
use kika_forge::forge::{Words, suggestion_info};
use kika_git::safe_text::is_invisible_or_bidi;

const NOT_NEW_SIDE: &str = "A suggestion goes on added or unchanged lines";
const UNSAFE: &str = "These lines hold characters a suggestion cannot show";

fn too_long(words: &Words) -> String {
    format!("{} takes a suggestion of at most 101 lines", words.name)
}

/// The comment text for a suggestion replacing the picked lines with
/// `lines`, its fence opened with the forge's `info`. The fence is longer
/// than any run of backticks in the lines, so none of them ends it, and
/// the text ends on an empty line for a note.
pub(crate) fn suggestion_body(lines: &[String], info: &str) -> String {
    let longest = lines
        .iter()
        .map(|l| longest_backticks(l))
        .max()
        .unwrap_or(0);
    let fence = "`".repeat((longest + 1).max(3));
    format!("{fence}{info}\n{}\n{fence}\n", lines.join("\n"))
}

fn longest_backticks(line: &str) -> usize {
    line.split(|c| c != '`').map(str::len).max().unwrap_or(0)
}

/// A line the composer cannot show as it is: it draws its text as typed,
/// so a control character (tabs aside) or an invisible or bidi one would
/// either be dropped or hide what the suggestion really holds.
fn unsafe_line(line: &str) -> bool {
    line.chars()
        .any(|c| (c.is_control() && c != '\t') || is_invisible_or_bidi(c))
}

impl AppState {
    /// The fence info for a suggestion on `target` on the reviewed forge;
    /// `None` when the forge cannot take one that long.
    fn suggestion_info(&self, target: &CommentTarget) -> Option<String> {
        let first = target.start.unwrap_or(target.end).line;
        let above = target.end.line.saturating_sub(first);
        suggestion_info(self.review_forge_kind(), above)
    }

    /// `s` in pick mode: read the picked lines at the head, then open the
    /// composer on them. Only lines on the new side can be suggested on;
    /// a pick that ends on a removed line says so and stays.
    pub(crate) fn confirm_suggestion_pick(&mut self) -> Vec<Effect> {
        if live_pick(self).is_some()
            && let Some(reason) = comment_disabled(self)
        {
            self.notice = Some(reason);
            return vec![];
        }
        let Some(target) = self.picked_target() else {
            return vec![];
        };
        let new_side = target.start.unwrap_or(target.end).side == GhSide::Right
            && target.end.side == GhSide::Right;
        if !new_side {
            self.notice = Some(NOT_NEW_SIDE.into());
            return vec![];
        }
        if self.suggestion_info(&target).is_none() {
            self.notice = Some(too_long(self.review_words()).into());
            return vec![];
        }
        let Some((head, path)) = self
            .review
            .files
            .as_ref()
            .and_then(|files| Some((files.head.clone(), files.selected()?.file.path.clone())))
        else {
            return self.end_comment_pick();
        };
        self.end_comment_pick();
        let tag = self.review_tag();
        self.review.suggestion_fetch = ReviewFetch {
            seq: Some(tag.seq),
            loading: true,
            error: None,
        };
        vec![Effect::ReadSuggestionLines {
            tag,
            target,
            head,
            path,
        }]
    }

    /// The picked lines came: the composer opens on a suggestion of them,
    /// unless the answer is stale, a composer is open already, or the
    /// files screen has gone.
    pub(crate) fn suggestion_read(
        &mut self,
        tag: Tag,
        target: CommentTarget,
        result: Result<Vec<String>, String>,
    ) {
        if !self.review_current(tag, &self.review.suggestion_fetch) {
            return;
        }
        // Answered: a second answer under this tag is dropped too.
        self.review.suggestion_fetch = ReviewFetch::default();
        if self.review.composer.is_some() || !self.review_files_shown() {
            return;
        }
        let lines = match result {
            Ok(lines) => lines,
            Err(why) => {
                self.notice = Some(format!("Could not read the lines: {why}").into());
                return;
            }
        };
        if lines.iter().any(|l| unsafe_line(l)) {
            self.notice = Some(UNSAFE.into());
            return;
        }
        let Some(info) = self.suggestion_info(&target) else {
            self.notice = Some(too_long(self.review_words()).into());
            return;
        };
        self.review.composer = Some(Composer {
            kind: ComposerKind::Line {
                target,
                editing: None,
            },
            text: CommentEditor::from_text(&suggestion_body(&lines, &info)),
        });
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::features::review::ReviewIntent;
    use crate::features::review::comment_pick::ACROSS_HUNKS;
    use crate::features::review::pending::tests::{
        HEAD, commenting, ctrl, right, saved, type_text,
    };
    use crate::features::review::state::tests::{press, type_char};
    use crate::intent::AppIntent;
    use crate::keymap::KeyScope;
    use crate::view::buffer_rows;
    use kika_forge::anchor::GhLine;
    use kika_forge::discussion_write::DiscussionWrite;
    use kika_git::commit_files::CommitId;
    use kika_git::path::RepoPath;

    fn strings(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|l| l.to_string()).collect()
    }

    /// The one read `effects` holds: its tag and target, after checking
    /// it reads the shown file at the head.
    fn read(effects: &[Effect]) -> (Tag, CommentTarget) {
        match effects {
            [
                Effect::ReadSuggestionLines {
                    tag,
                    target,
                    head,
                    path,
                },
            ] => {
                assert_eq!(head, &CommitId::parse(HEAD).unwrap());
                assert_eq!(path, &RepoPath::from_bytes(b"src/cache.rs"));
                (*tag, target.clone())
            }
            other => panic!("expected one read, got {other:?}"),
        }
    }

    fn answer(
        state: &mut AppState,
        tag: Tag,
        target: &CommentTarget,
        result: Result<Vec<String>, String>,
    ) -> Vec<Effect> {
        state.update(AppIntent::Review(ReviewIntent::SuggestionLinesRead {
            tag,
            target: target.clone(),
            result,
        }))
    }

    fn span(target: &CommentTarget) -> (Option<GhLine>, GhLine) {
        (target.start, target.end)
    }

    fn composer_text(state: &AppState) -> Option<String> {
        state.review.composer.as_ref().map(|c| c.text.text())
    }

    #[test]
    fn the_fence_is_longer_than_any_backticks_in_the_lines() {
        assert_eq!(
            suggestion_body(&strings(&["let x = 1;"]), "suggestion"),
            "```suggestion\nlet x = 1;\n```\n"
        );
        assert_eq!(
            suggestion_body(&strings(&["\tif x {", "  ``a``"]), "suggestion"),
            "```suggestion\n\tif x {\n  ``a``\n```\n"
        );
        assert_eq!(
            suggestion_body(&strings(&["```rust", "x"]), "suggestion"),
            "````suggestion\n```rust\nx\n````\n"
        );
        assert_eq!(
            suggestion_body(&strings(&["a `````` b"]), "suggestion"),
            "```````suggestion\na `````` b\n```````\n"
        );
        assert!(!unsafe_line("\tif x { // ok ✓"));
        for bad in ["a\u{202E}b", "\u{1b}[2J", "a\u{200B}", "x\r"] {
            assert!(unsafe_line(bad), "{bad:?}");
        }
    }

    #[test]
    fn s_on_one_new_line_reads_it_and_opens_the_composer_on_a_suggestion() {
        let mut state = commenting();
        type_char(&mut state, 'a');
        let (tag, target) = read(&type_char(&mut state, 's'));
        assert_eq!(span(&target), (None, right(1)));
        assert_eq!(state.review.files.as_ref().unwrap().pick, None);
        assert_eq!(state.review.composer, None);

        assert_eq!(
            answer(&mut state, tag, &target, Ok(strings(&["let x = 1;"]))),
            []
        );
        let composer = state.review.composer.as_ref().expect("a composer");
        assert_eq!(
            composer.kind,
            ComposerKind::Line {
                target: target.clone(),
                editing: None
            }
        );
        assert_eq!(composer.text.text(), "```suggestion\nlet x = 1;\n```\n");
        assert_eq!((composer.text.row, composer.text.col), (3, 0));
        assert_eq!(composer.text.lines[3], "");
        assert_eq!(state.key_focus(), KeyScope::ReviewComment);

        // A context row is on the new side too.
        let mut state = commenting();
        type_char(&mut state, 'a');
        type_char(&mut state, 'j');
        let (_, target) = read(&type_char(&mut state, 's'));
        assert_eq!(span(&target), (None, right(2)));
    }

    #[test]
    fn a_range_with_a_removed_line_inside_reads_from_its_first_to_its_last_new_line() {
        let mut state = commenting();
        type_char(&mut state, 'a');
        type_char(&mut state, 'v');
        for _ in 0..3 {
            type_char(&mut state, 'j');
        }
        let (tag, target) = read(&type_char(&mut state, 's'));
        assert_eq!(span(&target), (Some(right(1)), right(3)));
        let lines = strings(&["fn new() {}", "use std::fmt;", "fn newer() {}"]);
        answer(&mut state, tag, &target, Ok(lines));
        assert_eq!(
            composer_text(&state).as_deref(),
            Some("```suggestion\nfn new() {}\nuse std::fmt;\nfn newer() {}\n```\n")
        );
    }

    #[test]
    fn on_gitlab_the_fence_counts_the_lines_above_and_stops_at_101() {
        use crate::features::review::state::tests::on_gitlab;
        // A 3-line pick: two lines above the last, where the note sits.
        let mut state = on_gitlab(commenting());
        type_char(&mut state, 'a');
        type_char(&mut state, 'v');
        for _ in 0..3 {
            type_char(&mut state, 'j');
        }
        let (tag, target) = read(&type_char(&mut state, 's'));
        assert_eq!(span(&target), (Some(right(1)), right(3)));
        let lines = strings(&["fn new() {}", "use std::fmt;", "fn newer() {}"]);
        answer(&mut state, tag, &target, Ok(lines));
        assert_eq!(
            composer_text(&state).as_deref(),
            Some("```suggestion:-2+0\nfn new() {}\nuse std::fmt;\nfn newer() {}\n```\n")
        );

        // One line: nothing above it.
        let mut state = on_gitlab(commenting());
        type_char(&mut state, 'a');
        let (tag, target) = read(&type_char(&mut state, 's'));
        answer(&mut state, tag, &target, Ok(strings(&["let x = 1;"])));
        assert_eq!(
            composer_text(&state).as_deref(),
            Some("```suggestion:-0+0\nlet x = 1;\n```\n")
        );

        // 102 lines: the notice, and no composer.
        let mut state = on_gitlab(commenting());
        type_char(&mut state, 'a');
        let (tag, target) = read(&type_char(&mut state, 's'));
        let long = CommentTarget {
            start: Some(right(1)),
            end: right(102),
            ..target
        };
        let lines: Vec<String> = (1..=102).map(|n| format!("line {n}")).collect();
        answer(&mut state, tag, &long, Ok(lines));
        assert_eq!(state.review.composer, None);
        assert_eq!(
            state.notice.as_deref(),
            Some("GitLab takes a suggestion of at most 101 lines")
        );
        // 101 lines still fit.
        let mut state = on_gitlab(commenting());
        type_char(&mut state, 'a');
        let (tag, target) = read(&type_char(&mut state, 's'));
        let longest = CommentTarget {
            start: Some(right(1)),
            end: right(101),
            ..target
        };
        let lines: Vec<String> = (1..=101).map(|n| format!("line {n}")).collect();
        answer(&mut state, tag, &longest, Ok(lines));
        assert!(
            composer_text(&state)
                .unwrap()
                .starts_with("```suggestion:-100+0\n")
        );
    }

    #[test]
    fn a_pick_ending_on_a_removed_line_or_across_hunks_is_refused_and_stays() {
        // One removed row.
        let mut state = commenting();
        type_char(&mut state, 'a');
        type_char(&mut state, 'j');
        type_char(&mut state, 'j');
        assert_eq!(type_char(&mut state, 's'), []);
        assert_eq!(state.notice.as_deref(), Some(NOT_NEW_SIDE));
        assert_eq!(state.review.composer, None);
        assert_eq!(state.key_focus(), KeyScope::ReviewPick);
        // A range from a new row to a removed one, either way round.
        for (from, to) in [(0, 2), (2, 3)] {
            let mut state = commenting();
            type_char(&mut state, 'a');
            for _ in 0..from {
                type_char(&mut state, 'j');
            }
            type_char(&mut state, 'v');
            for _ in from..to {
                type_char(&mut state, 'j');
            }
            if from == 2 {
                // Anchor on the removed row, cursor on the new one.
                assert!(matches!(live_pick(&state).map(|p| p.anchor), Some(Some(6))));
            }
            assert_eq!(type_char(&mut state, 's'), [], "{from}..{to}");
            assert_eq!(state.notice.as_deref(), Some(NOT_NEW_SIDE));
            assert_eq!(state.key_focus(), KeyScope::ReviewPick);
        }
        // Across a hunk header.
        let mut state = commenting();
        type_char(&mut state, 'a');
        for _ in 0..3 {
            type_char(&mut state, 'j');
        }
        type_char(&mut state, 'v');
        type_char(&mut state, 'j');
        assert_eq!(type_char(&mut state, 's'), []);
        assert_eq!(state.notice.as_deref(), Some(ACROSS_HUNKS));
        assert_eq!(state.review.composer, None);
        assert_eq!(state.key_focus(), KeyScope::ReviewPick);
    }

    #[test]
    fn tabs_stay_and_lines_the_composer_cannot_show_are_refused() {
        let mut state = commenting();
        type_char(&mut state, 'a');
        let (tag, target) = read(&type_char(&mut state, 's'));
        answer(&mut state, tag, &target, Ok(strings(&["\tif x {"])));
        assert!(composer_text(&state).unwrap().contains("\n\tif x {\n"));

        let mut state = commenting();
        type_char(&mut state, 'a');
        let (tag, target) = read(&type_char(&mut state, 's'));
        answer(&mut state, tag, &target, Ok(strings(&["a ``` b"])));
        assert_eq!(
            composer_text(&state).as_deref(),
            Some("````suggestion\na ``` b\n````\n")
        );

        for bad in ["let \u{202E}x = 1;", "\u{1b}[2J"] {
            let mut state = commenting();
            type_char(&mut state, 'a');
            let (tag, target) = read(&type_char(&mut state, 's'));
            answer(&mut state, tag, &target, Ok(strings(&["ok", bad])));
            assert_eq!(state.review.composer, None, "{bad:?}");
            assert_eq!(state.notice.as_deref(), Some(UNSAFE), "{bad:?}");
        }
    }

    #[test]
    fn a_stale_answer_one_over_an_open_composer_and_a_failure_open_nothing() {
        let mut state = commenting();
        type_char(&mut state, 'a');
        let (tag, target) = read(&type_char(&mut state, 's'));
        let lines = || Ok(strings(&["x"]));
        let stale = Tag {
            seq: tag.seq + 100,
            ..tag
        };
        answer(&mut state, stale, &target, lines());
        assert_eq!(state.review.composer, None);
        assert!(state.review.suggestion_fetch.loading);

        // A comment started meanwhile keeps its text.
        type_char(&mut state, 'a');
        press(&mut state, KeyCode::Enter);
        type_text(&mut state, "draft");
        answer(&mut state, tag, &target, lines());
        assert_eq!(composer_text(&state).as_deref(), Some("draft"));
        assert!(!state.review.suggestion_fetch.loading);

        // A failure says why, made safe, and opens nothing.
        let mut state = commenting();
        type_char(&mut state, 'a');
        let (tag, target) = read(&type_char(&mut state, 's'));
        answer(&mut state, tag, &target, Err("fatal: bad \u{1b}[2J".into()));
        assert_eq!(state.review.composer, None);
        let notice = state.notice.as_deref().unwrap();
        assert!(
            notice.starts_with("Could not read the lines: fatal: bad"),
            "{notice}"
        );
        assert!(!notice.contains('\u{1b}'), "{notice}");
        // Answered once: the same tag again changes nothing.
        state.notice = None;
        answer(&mut state, tag, &target, lines());
        assert_eq!(state.review.composer, None);

        // Off the files screen, nothing opens.
        let mut state = commenting();
        type_char(&mut state, 'a');
        let (tag, target) = read(&type_char(&mut state, 's'));
        press(&mut state, KeyCode::Esc);
        assert!(!state.review_files_shown());
        answer(&mut state, tag, &target, lines());
        assert_eq!(state.review.composer, None);
    }

    #[test]
    fn a_saved_suggestion_goes_to_github_with_the_review_body_unchanged() {
        let mut state = commenting();
        state.review.detail.as_mut().unwrap().number = 12;
        type_char(&mut state, 'a');
        let (tag, target) = read(&type_char(&mut state, 's'));
        answer(&mut state, tag, &target, Ok(strings(&["\tlet x = 2;"])));
        type_text(&mut state, "Two reads better.");
        let (saved_tag, review) = saved(&ctrl(&mut state, 's'));
        let body = "```suggestion\n\tlet x = 2;\n```\nTwo reads better.";
        assert_eq!(review.comments[0].body, body);
        assert_eq!(review.comments[0].end.side, GhSide::Right);
        state.update(AppIntent::Review(ReviewIntent::PendingSaved {
            tag: saved_tag,
            result: Ok(()),
        }));

        // Submit it as a comment: `S`, the third type, a summary, `y`.
        assert_eq!(type_char(&mut state, 'S'), []);
        press(&mut state, KeyCode::Down);
        press(&mut state, KeyCode::Down);
        assert_eq!(press(&mut state, KeyCode::Enter), []);
        type_text(&mut state, "See the suggestion");
        assert_eq!(ctrl(&mut state, 's'), []);
        let effects = type_char(&mut state, 'y');
        let [Effect::WriteDiscussion { write, .. }] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        let DiscussionWrite::SubmitReview { threads, .. } = write else {
            panic!("{write:?}");
        };
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].body, body);
        assert_eq!(threads[0].end, right(1));
    }

    #[test]
    fn the_key_bar_offers_suggest_while_picking() {
        let mut state = commenting();
        type_char(&mut state, 'a');
        let mut terminal = Terminal::new(TestBackend::new(160, 30)).unwrap();
        terminal
            .draw(|frame| crate::view::render(&state, frame))
            .unwrap();
        let rows = buffer_rows(terminal.backend().buffer());
        let bar = rows.last().unwrap();
        assert!(bar.contains("suggest"), "{rows:#?}");
    }
}
