//! Marking the selected file viewed, or not viewed, on GitHub from the
//! files screen. GitHub keeps the mark per file for the whole pull
//! request. It is asked only on the key, one mark at a time, and only on
//! a host the global config lists and `[forge.allow]` allows.

use super::files::{PrFile, ViewedMark, files_screen};
use crate::effect::{Effect, Tag};
use crate::keymap::Reason;
use crate::model::AppState;
use kika_forge::review::ViewedState;
use kika_forge::{CreateError, ForgeHost, ForgeRepo};

const NO_FILE: &str = "Select a file first";
const MARKING: &str = "Already marking";
const NOT_UTF8: &str = "Kika cannot mark a file whose name is not UTF-8";
const NO_NODE_ID: &str = "GitHub sent no pull request id; refresh with r";

/// Why `v` cannot mark the selected file now.
pub(crate) fn review_viewed_disabled(state: &AppState) -> Option<Reason> {
    state.viewed_target().err()
}

/// One mark `v` may ask for now.
struct Target {
    host: ForgeHost,
    repo: ForgeRepo,
    node_id: String,
    number: u64,
    path: String,
    viewed: bool,
}

/// The path of `row`, when it can be sent to GitHub.
fn utf8_path(row: &PrFile) -> Option<&str> {
    std::str::from_utf8(row.file.path.as_bytes()).ok()
}

impl AppState {
    /// The mark `v` would ask for, or why there is none: the files screen
    /// with a file selected, no mark in flight, a name GitHub can be sent,
    /// the pull request's id, and a host that may be asked now.
    fn viewed_target(&self) -> Result<Target, Reason> {
        let files = files_screen(self)
            .filter(|_| self.review_files_shown())
            .ok_or(NO_FILE)?;
        let row = files.selected().ok_or(NO_FILE)?;
        if files.marking.is_some() {
            return Err(MARKING.into());
        }
        let path = utf8_path(row).ok_or(NOT_UTF8)?;
        let node_id = self
            .review
            .detail
            .as_ref()
            .filter(|d| d.number == files.number)
            .and_then(|d| d.node_id.clone())
            .ok_or(NO_NODE_ID)?;
        let (host, repo) = self.review_forge_target()?;
        Ok(Target {
            host,
            repo,
            node_id,
            number: files.number,
            path: path.to_string(),
            viewed: row.viewed != Some(ViewedState::Viewed),
        })
    }

    /// `v`: ask GitHub to mark the selected file viewed, or not viewed
    /// when it is. The cursor stays; nothing asks first, since `v` again
    /// undoes it.
    pub(crate) fn toggle_viewed(&mut self) -> Vec<Effect> {
        let target = match self.viewed_target() {
            Ok(target) => target,
            Err(reason) => {
                self.notice = Some(reason);
                return vec![];
            }
        };
        let seq = self.take_seq();
        let tag = self.user_tag(seq);
        let word = if target.viewed {
            "viewed"
        } else {
            "not viewed"
        };
        self.notice = Some(format!("Marking {} {word}…", target.path).into());
        if let Some(files) = &mut self.review.files {
            files.marking = Some(ViewedMark {
                seq,
                path: target.path.clone(),
                viewed: target.viewed,
            });
        }
        vec![Effect::SetFileViewed {
            tag,
            host: target.host,
            repo: target.repo,
            node_id: target.node_id,
            number: target.number,
            path: target.path,
            viewed: target.viewed,
        }]
    }

    /// GitHub answered a viewed mark. The row and the detail's list take
    /// it at once, so a reload of the files keeps it; GitHub is not asked
    /// again. An answer for a screen that has gone changes nothing.
    pub(crate) fn viewed_set(
        &mut self,
        tag: Tag,
        host: &ForgeHost,
        number: u64,
        path: String,
        viewed: bool,
        result: Result<(), CreateError>,
    ) {
        self.user_done(tag);
        if tag.generation != self.generation || !self.review_files_shown() {
            return;
        }
        let sent = ViewedMark {
            seq: tag.seq,
            path,
            viewed,
        };
        let Some(files) = self
            .review
            .files
            .as_mut()
            .filter(|f| f.number == number && f.marking.as_ref() == Some(&sent))
        else {
            return;
        };
        files.marking = None;
        match result {
            Ok(()) => self.take_viewed_mark(number, sent),
            Err(error) => {
                // Remembered against the host it came from, as for a
                // merge, so the next `v` asks nobody until it resets.
                if let CreateError::RateLimited { until } = error {
                    self.forge.record_rate_limit(host, until);
                }
                self.notice = Some(error.message().into());
            }
        }
    }

    /// Show the mark GitHub took on its row and in the detail's list.
    fn take_viewed_mark(&mut self, number: u64, mark: ViewedMark) {
        let word = if mark.viewed { "viewed" } else { "not viewed" };
        self.notice = Some(format!("Marked {} {word}", mark.path).into());
        let state = match mark.viewed {
            true => ViewedState::Viewed,
            false => ViewedState::Unviewed,
        };
        if let Some(files) = &mut self.review.files {
            for row in &mut files.rows {
                if utf8_path(row) == Some(mark.path.as_str()) {
                    row.viewed = Some(state);
                }
            }
        }
        if let Some(detail) = self.review.detail.as_mut().filter(|d| d.number == number) {
            match detail.viewed.iter_mut().find(|(p, _)| *p == mark.path) {
                Some((_, known)) => *known = state,
                None => detail.viewed.push((mark.path, state)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;

    use super::*;
    use crate::features::review::files::FilesStage;
    use crate::features::review::git::{CommitsHere, FileCounts, PrFileRow};
    use crate::features::review::state::NO_FORGE;
    use crate::features::review::state::tests::{in_detail, press, repo, type_char};
    use crate::features::review::{ReviewIntent, ReviewScreen};
    use crate::intent::AppIntent;
    use kika_forge::allow::AllowRule;
    use kika_forge::review::ViewedState;
    use kika_forge::{ForgeStatus, REFRESH_LIMITED};
    use kika_git::commit_files::{CommitFile, CommitId, CommitRange};

    const NOW: i64 = 1_700_000_000;

    fn row(path: &[u8]) -> PrFileRow {
        PrFileRow {
            file: CommitFile {
                status: 'M',
                path: kika_git::path::RepoPath::from_bytes(path),
                orig_path: None,
            },
            counts: FileCounts::Lines {
                added: 1,
                removed: 1,
            },
        }
    }

    /// Pull request 12's files screen, ready: `src/cache.rs` viewed,
    /// `src/lib.rs` dismissed, `docs/café.md` not viewed, `new.rs` unknown.
    fn ready_with(rows: Vec<PrFileRow>) -> AppState {
        let mut state = in_detail(Vec::new());
        state.review.detail.as_mut().unwrap().number = 12;
        let effects = type_char(&mut state, 'f');
        let [Effect::CheckReviewCommits { tag, .. }] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        let tag = *tag;
        state.update(AppIntent::Review(ReviewIntent::ObjectsChecked {
            tag,
            result: Ok(CommitsHere::All),
        }));
        let id = |c: char| CommitId::parse(&c.to_string().repeat(40)).unwrap();
        let range = CommitRange {
            oid: id('1'),
            parent: Some(id('3')),
        };
        state.update(AppIntent::Review(ReviewIntent::FilesLoaded {
            tag,
            result: Ok((range, rows)),
        }));
        assert_eq!(files(&state).stage, FilesStage::Ready);
        state
    }

    fn ready() -> AppState {
        ready_with(vec![
            row(b"src/cache.rs"),
            row(b"src/lib.rs"),
            row("docs/café.md".as_bytes()),
            row(b"new.rs"),
        ])
    }

    fn files(state: &AppState) -> &crate::features::review::files::PrFiles {
        state.review.files.as_ref().unwrap()
    }

    fn select(state: &mut AppState, at: usize) {
        state.review.files.as_mut().unwrap().cursor = at;
    }

    fn marks(state: &AppState) -> Vec<Option<ViewedState>> {
        files(state).rows.iter().map(|r| r.viewed).collect()
    }

    /// Press `v` and return the one mark it asks for.
    fn mark(state: &mut AppState) -> (Tag, u64, String, bool) {
        let effects = type_char(state, 'v');
        match effects.as_slice() {
            [
                Effect::SetFileViewed {
                    tag,
                    host,
                    repo: to,
                    node_id,
                    number,
                    path,
                    viewed,
                },
            ] => {
                assert_eq!(host.as_str(), "github.com");
                assert_eq!(*to, repo());
                assert_eq!(node_id, "PR_kwDOA1");
                (*tag, *number, path.clone(), *viewed)
            }
            other => panic!("expected one mark, got {other:?}"),
        }
    }

    fn answer(
        state: &mut AppState,
        sent: &(Tag, u64, String, bool),
        result: Result<(), CreateError>,
    ) {
        answer_from(state, &github_host(), sent, result);
    }

    fn github_host() -> ForgeHost {
        ForgeHost::parse("github.com").unwrap()
    }

    fn answer_from(
        state: &mut AppState,
        host: &ForgeHost,
        sent: &(Tag, u64, String, bool),
        result: Result<(), CreateError>,
    ) {
        let (tag, number, path, viewed) = sent.clone();
        state.update(AppIntent::Review(ReviewIntent::ViewedSet {
            tag,
            host: host.clone(),
            number,
            path,
            viewed,
            result,
        }));
    }

    fn viewed_in_detail(state: &AppState, path: &str) -> Option<ViewedState> {
        let detail = state.review.detail.as_ref().unwrap();
        detail
            .viewed
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, v)| *v)
    }

    #[test]
    fn v_on_a_file_not_viewed_asks_to_mark_it_viewed_once() {
        let mut state = ready();
        select(&mut state, 2);
        let busy = state.busy_count();
        let held = state.held_seq();
        let sent = mark(&mut state);
        let (tag, number, path, viewed) = sent.clone();
        assert_eq!((number, path.as_str(), viewed), (12, "docs/café.md", true));
        assert!(tag.user);
        assert_eq!(state.busy_count(), busy + 1);
        // It does not take the git slot, and the cursor stays.
        assert_eq!(state.held_seq(), held);
        assert_eq!(files(&state).cursor, 2);
        assert_eq!(
            files(&state).marking.as_ref().map(|m| (m.seq, m.viewed)),
            Some((tag.seq, true))
        );
        assert_eq!(
            state.notice.as_deref(),
            Some("Marking docs/café.md viewed…")
        );
        // A second `v` waits for the first.
        assert_eq!(type_char(&mut state, 'v'), []);
        assert_eq!(state.notice.as_deref(), Some("Already marking"));
        assert_eq!(
            state.update(AppIntent::Review(ReviewIntent::ToggleViewed)),
            []
        );
        assert_eq!(state.busy_count(), busy + 1);
    }

    #[test]
    fn v_on_a_viewed_file_unmarks_it_and_anything_else_marks_it() {
        for (at, want) in [(0, false), (1, true), (2, true), (3, true)] {
            let mut state = ready();
            select(&mut state, at);
            let (_, _, _, viewed) = mark(&mut state);
            assert_eq!(viewed, want, "row {at}");
        }
        let mut state = ready();
        mark(&mut state);
        assert_eq!(
            state.notice.as_deref(),
            Some("Marking src/cache.rs not viewed…")
        );
    }

    #[test]
    fn a_hostile_path_is_escaped_in_the_notice() {
        let mut state = ready_with(vec![row(b"evil\x1b]52;c;eA==\x07name")]);
        mark(&mut state);
        let notice = state.notice.clone().unwrap();
        assert!(!notice.chars().any(char::is_control), "{notice:?}");
        assert!(notice.contains("name"), "{notice:?}");
    }

    #[test]
    fn an_answer_flips_the_row_and_the_detail_and_the_count() {
        let mut state = ready();
        select(&mut state, 2);
        assert_eq!(files(&state).viewed_count(), 1);
        let sent = mark(&mut state);
        let busy = state.busy_count();
        answer(&mut state, &sent, Ok(()));
        assert_eq!(state.busy_count(), busy - 1);
        assert_eq!(marks(&state)[2], Some(ViewedState::Viewed));
        assert_eq!(files(&state).viewed_count(), 2);
        assert_eq!(files(&state).marking, None);
        assert_eq!(
            viewed_in_detail(&state, "docs/café.md"),
            Some(ViewedState::Viewed)
        );
        assert_eq!(state.notice.as_deref(), Some("Marked docs/café.md viewed"));

        // Back to not viewed.
        let sent = mark(&mut state);
        assert!(!sent.3);
        answer(&mut state, &sent, Ok(()));
        assert_eq!(marks(&state)[2], Some(ViewedState::Unviewed));
        assert_eq!(files(&state).viewed_count(), 1);
        assert_eq!(
            viewed_in_detail(&state, "docs/café.md"),
            Some(ViewedState::Unviewed)
        );
        assert_eq!(
            state.notice.as_deref(),
            Some("Marked docs/café.md not viewed")
        );

        // A file GitHub's list lacked is added to it.
        select(&mut state, 3);
        let before = state.review.detail.as_ref().unwrap().viewed.len();
        let sent = mark(&mut state);
        answer(&mut state, &sent, Ok(()));
        assert_eq!(marks(&state)[3], Some(ViewedState::Viewed));
        assert_eq!(
            viewed_in_detail(&state, "new.rs"),
            Some(ViewedState::Viewed)
        );
        assert_eq!(
            state.review.detail.as_ref().unwrap().viewed.len(),
            before + 1
        );
    }

    #[test]
    fn a_failure_keeps_the_row_and_says_why() {
        let mut state = ready();
        select(&mut state, 2);
        let sent = mark(&mut state);
        let busy = state.busy_count();
        let error = CreateError::Refused("Could not resolve to a node".into());
        answer(&mut state, &sent, Err(error));
        assert_eq!(state.busy_count(), busy - 1);
        assert_eq!(marks(&state)[2], Some(ViewedState::Unviewed));
        assert_eq!(files(&state).marking, None);
        assert_eq!(
            viewed_in_detail(&state, "docs/café.md"),
            Some(ViewedState::Unviewed)
        );
        assert_eq!(state.notice.as_deref(), Some("Could not resolve to a node"));
        // The next `v` may ask again.
        mark(&mut state);

        let mut state = ready();
        let sent = mark(&mut state);
        answer(
            &mut state,
            &sent,
            Err(CreateError::Status(ForgeStatus::NoGh)),
        );
        assert_eq!(
            state.notice.as_deref(),
            Some(CreateError::Status(ForgeStatus::NoGh).message().as_str())
        );
    }

    #[test]
    fn a_rate_limit_is_remembered_so_the_next_v_asks_nobody() {
        let mut state = ready();
        let sent = mark(&mut state);
        answer(
            &mut state,
            &sent,
            Err(CreateError::RateLimited { until: NOW + 600 }),
        );
        assert_eq!(state.forge.limited.get("github.com"), Some(&(NOW + 600)));
        assert_eq!(marks(&state)[0], Some(ViewedState::Viewed));
        assert_eq!(type_char(&mut state, 'v'), []);
        assert_eq!(state.notice.as_deref(), Some(REFRESH_LIMITED));
    }

    #[test]
    fn a_rate_limit_is_recorded_against_the_host_the_mark_went_to() {
        let mut state = ready();
        let sent = mark(&mut state);
        // The repository's remote changes host while the call runs.
        state.forge.repo = Some(kika_forge::ForgeRepo {
            host: "gitlab.example".into(),
            ..repo()
        });
        answer_from(
            &mut state,
            &github_host(),
            &sent,
            Err(CreateError::RateLimited { until: NOW + 600 }),
        );
        assert_eq!(state.forge.limited.get("github.com"), Some(&(NOW + 600)));
        assert_eq!(state.forge.limited.get("gitlab.example"), None);
    }

    #[test]
    fn stale_answers_change_nothing_but_still_end_their_busy_mark() {
        type Stale = fn(&mut AppState, &mut (Tag, u64, String, bool));
        let cases: [Stale; 4] = [
            |_, sent| sent.0.seq += 100,
            |_, sent| sent.0.generation += 1,
            |_, sent| sent.1 = 13,
            // The screen is left before the answer.
            |state, _| {
                press(state, KeyCode::Esc);
            },
        ];
        for (i, stale) in cases.into_iter().enumerate() {
            let mut state = ready();
            select(&mut state, 2);
            let mut sent = mark(&mut state);
            stale(&mut state, &mut sent);
            let busy = state.busy_count();
            let detail = state.review.detail.clone();
            answer(&mut state, &sent, Ok(()));
            assert_eq!(state.busy_count(), busy - 1, "case {i}");
            assert_eq!(state.review.detail, detail, "case {i}");
            if let Some(files) = &state.review.files {
                assert_eq!(
                    files.rows[2].viewed,
                    Some(ViewedState::Unviewed),
                    "case {i}"
                );
                assert!(files.marking.is_some(), "case {i}");
            }
        }
    }

    #[test]
    fn v_says_why_and_asks_nothing_when_it_cannot_mark() {
        let reason = |state: &AppState| review_viewed_disabled(state).map(String::from);
        let refused = |state: &mut AppState, want: &str| {
            assert_eq!(reason(state).as_deref(), Some(want));
            assert_eq!(
                state.update(AppIntent::Review(ReviewIntent::ToggleViewed)),
                []
            );
            assert_eq!(state.notice.as_deref(), Some(want));
            assert_eq!(files(state).marking, None);
        };
        assert_eq!(reason(&ready()), None);

        let mut state = ready_with(vec![row(b"caf\xe9.rs")]);
        refused(&mut state, NOT_UTF8);

        let mut state = ready();
        state.review.detail.as_mut().unwrap().node_id = None;
        refused(&mut state, NO_NODE_ID);

        let mut state = ready();
        state.forge.limited.insert("github.com".into(), NOW + 60);
        refused(&mut state, REFRESH_LIMITED);
        // A limit that has passed no longer stops it.
        state.forge.limited.insert("github.com".into(), NOW - 1);
        mark(&mut state);

        let mut state = ready();
        state.forge.hosts.clear();
        refused(&mut state, NO_FORGE);

        let mut state = ready();
        state.forge.hosts = vec![
            ForgeHost::parse("github.com")
                .unwrap()
                .with_allow(vec![AllowRule::parse("other").unwrap()]),
        ];
        refused(&mut state, NO_FORGE);

        let mut state = ready();
        state.forge.repo = Some(kika_forge::ForgeRepo {
            host: "gitlab.example".into(),
            ..repo()
        });
        refused(&mut state, NO_FORGE);

        let mut state = ready_with(Vec::new());
        refused(&mut state, NO_FILE);

        // Not off the files screen either.
        let mut state = ready();
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.review.screen, ReviewScreen::Detail(12));
        assert_eq!(reason(&state).as_deref(), Some(NO_FILE));
        assert_eq!(
            state.update(AppIntent::Review(ReviewIntent::ToggleViewed)),
            []
        );
    }

    #[test]
    fn v_from_the_focused_diff_marks_the_file_shown() {
        let mut state = ready();
        select(&mut state, 2);
        press(&mut state, KeyCode::Tab);
        let (_, _, path, _) = mark(&mut state);
        assert_eq!(path, "docs/café.md");
    }

    fn terminal(state: &AppState, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| crate::view::render(state, frame))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn draw_at(state: &AppState, width: u16, height: u16) -> Vec<String> {
        crate::view::buffer_rows(&terminal(state, width, height))
    }

    fn draw(state: &AppState) -> Vec<String> {
        draw_at(state, 80, 24)
    }

    /// The style of the first cell where `text` starts on screen.
    fn style_at(state: &AppState, width: u16, height: u16, text: &str) -> ratatui::style::Style {
        let buffer = terminal(state, width, height);
        for y in 0..height {
            for x in 0..width {
                let from: String = (x..width).map(|x| buffer[(x, y)].symbol()).collect();
                if from.starts_with(text) {
                    return buffer[(x, y)].style();
                }
            }
        }
        panic!("{text:?} not drawn");
    }

    /// The drawn row of `path`.
    fn file_row(rows: &[String], path: &str) -> String {
        rows.iter()
            .find(|r| r.contains(path) && r.contains("+1"))
            .cloned()
            .unwrap_or_else(|| panic!("{path:?} not in {rows:#?}"))
    }

    #[test]
    fn the_bar_offers_v_and_the_row_shows_the_mark_github_took() {
        let mut state = ready();
        state.size = (80, 24);
        select(&mut state, 2);
        let rows = draw(&state);
        let bar = rows.last().unwrap();
        assert!(bar.contains("v viewed"), "{bar:?}");
        let dimmed = Some(ratatui::style::Color::DarkGray);
        assert_ne!(style_at(&state, 80, 24, "v viewed").fg, dimmed);
        assert!(file_row(&rows, "docs/café.md").contains("M   docs/café.md"));
        assert!(rows.iter().any(|r| r.contains("files (1/4)")), "{rows:#?}");
        let sent = mark(&mut state);
        answer(&mut state, &sent, Ok(()));
        let rows = draw(&state);
        assert!(file_row(&rows, "docs/café.md").contains("M ✓ docs/café.md"));
        assert!(rows.iter().any(|r| r.contains("files (2/4)")), "{rows:#?}");
        assert!(
            rows.last().unwrap().contains("Marked docs/café.md viewed"),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_mark_in_flight_shows_a_dim_pending_glyph_in_the_mark_column() {
        let mut state = ready();
        state.size = (80, 24);
        select(&mut state, 2);
        mark(&mut state);
        let rows = draw(&state);
        assert!(file_row(&rows, "docs/café.md").contains("M … docs/café.md"));
        // One column, so the paths stay lined up with the other rows.
        let col = |path: &str| {
            let row = file_row(&rows, path);
            row[..row.find(path).unwrap()].chars().count()
        };
        assert_eq!(col("docs/café.md"), col("src/lib.rs"));
        let pending = style_at(&state, 80, 24, "… docs/café.md");
        assert!(pending.add_modifier.contains(ratatui::style::Modifier::DIM));
        // Only the file asked about.
        assert!(file_row(&rows, "src/lib.rs").contains("M ~ src/lib.rs"));
        assert!(file_row(&rows, "src/cache.rs").contains("M ✓ src/cache.rs"));
    }

    #[test]
    fn an_unmark_clears_the_tick_and_lowers_the_count() {
        let mut state = ready();
        state.size = (80, 24);
        let sent = mark(&mut state);
        assert!(file_row(&draw(&state), "src/cache.rs").contains("M … src/cache.rs"));
        answer(&mut state, &sent, Ok(()));
        let rows = draw(&state);
        assert!(file_row(&rows, "src/cache.rs").contains("M   src/cache.rs"));
        assert!(rows.iter().any(|r| r.contains("files (0/4)")), "{rows:#?}");
        assert!(
            rows.last()
                .unwrap()
                .contains("Marked src/cache.rs not viewed"),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_failed_mark_draws_the_old_mark_and_the_reason() {
        let mut state = ready();
        state.size = (80, 24);
        select(&mut state, 2);
        let sent = mark(&mut state);
        let error = CreateError::Refused("Could not resolve to a node".into());
        answer(&mut state, &sent, Err(error));
        let rows = draw(&state);
        assert!(file_row(&rows, "docs/café.md").contains("M   docs/café.md"));
        assert!(rows.iter().any(|r| r.contains("files (1/4)")), "{rows:#?}");
        assert!(
            rows.last().unwrap().contains("Could not resolve to a node"),
            "{rows:#?}"
        );
    }

    #[test]
    fn on_a_narrow_bar_v_viewed_shows_whole_or_not_at_all() {
        for (width, height) in [(60, 15), (50, 15), (40, 12), (30, 10), (20, 8)] {
            let mut state = ready();
            state.size = (width, height);
            let rows = draw_at(&state, width, height);
            let bar = rows.last().unwrap();
            assert_eq!(bar.chars().count(), usize::from(width), "{bar:?}");
            if bar.contains("v view") {
                assert!(bar.contains("v viewed"), "clipped at {width}: {bar:?}");
            }
            if width >= 50 {
                assert!(bar.contains("v viewed"), "dropped at {width}: {bar:?}");
            }
            // Drawing with a mark in flight never panics either.
            mark(&mut state);
            state.notice = None;
            draw_at(&state, width, height);
        }
    }

    #[test]
    fn a_disabled_v_stays_on_the_bar_dimmed_like_the_other_disabled_keys() {
        let mut state = ready();
        state.size = (80, 24);
        state.forge.limited.insert("github.com".into(), NOW + 60);
        assert!(review_viewed_disabled(&state).is_some());
        let rows = draw(&state);
        assert!(rows.last().unwrap().contains("v viewed"), "{rows:#?}");
        let style = style_at(&state, 80, 24, "v viewed");
        assert_eq!(style.fg, Some(ratatui::style::Color::DarkGray));
        // An enabled key beside it is not dimmed.
        assert_ne!(style_at(&state, 80, 24, "Tab list/diff").fg, style.fg);
    }
}
