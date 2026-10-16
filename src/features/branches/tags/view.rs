//! Draws the Branches panel's Tags tab. The selected tag's log takes the
//! Diff pane's place through the branch log. Reads state only.

use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};

use super::TagsList;
use crate::features::branches::BranchesTab;
use crate::layout::SidePanel;
use crate::model::AppState;
use crate::theme::Palette;
use crate::view::{elide_right, no_matches, range_row, scrolled, scrolled_step_to};
use kika_git::tags::TagRef;

impl AppState {
    /// Whether the Branches panel lists its tags, expanded: what a click
    /// or the wheel can move through.
    pub(crate) fn tags_list_shown(&self) -> bool {
        let list = &self.branches.tags;
        self.expanded == SidePanel::Branches
            && self.branches.tab == BranchesTab::Tags
            && list.error.is_none()
            && list.loaded
            && !list.visible().is_empty()
    }

    /// The move from the cursor to the tag drawn on `line` of the listed
    /// panel's `rows` inner lines, as `list_body` scrolls them; `None`
    /// below the last tag or when the tab lists none.
    pub(crate) fn tags_step_to(&self, line: usize, rows: usize) -> Option<isize> {
        let list = &self.branches.tags;
        self.tags_list_shown()
            .then(|| scrolled_step_to(&list.visible(), list.selected, rows, line))
            .flatten()
    }
}

/// What the tab shows when there are no tags.
const NO_TAGS: &str = "No tags";

/// Marks an annotated tag's row; a lightweight one has blanks there.
const ANNOTATED: &str = "◆ ";

/// The tab's body in `rows` by `width`: why the load failed, a
/// placeholder, or one row per tag the filter shows, the selection and
/// the range reversed and scrolled into view.
pub fn list_body(
    palette: &Palette,
    list: &TagsList,
    rows: usize,
    width: usize,
) -> Paragraph<'static> {
    if let Some(error) = &list.error {
        let mut lines: Vec<Line> = error.lines().map(|l| Line::from(l.to_string())).collect();
        lines.push(Line::from("Press r to retry"));
        return Paragraph::new(lines).wrap(Wrap { trim: false });
    }
    if !list.loaded {
        return Paragraph::new("Loading tags…");
    }
    if list.rows.is_empty() {
        return Paragraph::new(NO_TAGS);
    }
    let shown = list.visible();
    if shown.is_empty() {
        return Paragraph::new(no_matches(palette));
    }
    let range = list.range();
    let selected = Some(list.selected);
    let lines = scrolled(&shown, selected, rows)
        .iter()
        .map(|&i| {
            let line = Line::from(tag_row(&list.rows[i], width));
            range_row(line, i, selected, &range, width)
        })
        .collect::<Vec<_>>();
    Paragraph::new(lines)
}

/// `◆ name  1234abc  2026-10-01  subject`, cut to `width`. The name is
/// drawn through `display`, so it holds no control characters; the other
/// fields were made safe when read.
fn tag_row(tag: &TagRef, width: usize) -> String {
    let marker = if tag.annotated { ANNOTATED } else { "  " };
    let text = format!(
        "{marker}{}  {}  {}  {}",
        tag.name.display(),
        tag.target_short,
        tag.date,
        tag.subject
    );
    elide_right(&text, width)
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Modifier;

    use super::*;
    use crate::features::branches::BranchesTab;
    use crate::layout::SidePanel;
    use crate::model::{AppState, Context, Focus};
    use crate::test_support::load_list;
    use crate::view::{buffer_rows, render};
    use kika_git::commit_files::CommitId;

    fn tag(name: &[u8], annotated: bool, oid: char, subject: &str) -> TagRef {
        let oid = oid.to_string().repeat(40);
        TagRef {
            name: kika_git::tags::TagName::from_bytes(name),
            target: CommitId::parse(&oid).unwrap(),
            target_short: oid[..7].into(),
            ref_oid: oid,
            annotated,
            date: "2026-10-01".into(),
            subject: subject.into(),
        }
    }

    /// The Branches panel focused on its Tags tab.
    fn focused(tags: Result<Vec<TagRef>, String>) -> AppState {
        let mut state = AppState::new(Context::Workspace);
        state.files.loaded = true;
        state.focus = Focus::Branches;
        state.expanded = SidePanel::Branches;
        load_list(&mut state.branches.list, Ok(vec![]));
        state.branches.tab = BranchesTab::Tags;
        load_list(&mut state.branches.tags, tags);
        state
    }

    fn draw(state: &AppState, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(state, frame)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn contains(rows: &[String], needle: &str) -> bool {
        rows.iter().any(|row| row.contains(needle))
    }

    #[test]
    fn the_tab_lists_tags_with_targets_and_marks_annotated_and_selected() {
        let mut state = focused(Ok(vec![
            tag(b"v2.0", true, 'a', "Second release"),
            tag(b"v1.0", false, 'b', "init"),
        ]));
        state.branches.tags.move_by(1);
        state.screen_mode = crate::layout::ScreenMode::Full;
        let buffer = draw(&state, 100, 24);
        let rows = buffer_rows(&buffer);
        assert!(contains(&rows, "Local Remotes [Tags]"), "{rows:#?}");
        assert!(
            contains(&rows, "◆ v2.0  aaaaaaa  2026-10-01  Second release"),
            "{rows:#?}"
        );
        assert!(
            contains(&rows, "  v1.0  bbbbbbb  2026-10-01  init"),
            "{rows:#?}"
        );
        let reversed_at = |needle: &str| {
            let y = rows.iter().position(|r| r.contains(needle)).unwrap();
            let x = rows[y].chars().position(|c| c == 'v').unwrap();
            buffer[(x as u16, y as u16)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        };
        assert!(reversed_at("v1.0  bbbbbbb"));
        assert!(!reversed_at("v2.0  aaaaaaa"));
    }

    #[test]
    fn a_name_with_an_escape_draws_without_the_escape_byte() {
        let state = focused(Ok(vec![tag(b"v\x1b[31mred", false, 'c', "s")]));
        let buffer = draw(&state, 100, 24);
        let rows = buffer_rows(&buffer);
        assert!(contains(&rows, "v\u{FFFD}[31mred"), "{rows:#?}");
        for cell in buffer.content() {
            assert!(!cell.symbol().contains('\x1b'), "{rows:#?}");
        }
    }

    #[test]
    fn a_filter_draws_safe_with_its_count_and_a_range_reversed() {
        let mut state = focused(Ok(vec![
            tag(b"v3", false, 'a', "s"),
            tag(b"x", false, 'a', "s"),
            tag(b"v2", false, 'a', "s"),
            tag(b"v1", false, 'a', "s"),
        ]));
        state.screen_mode = crate::layout::ScreenMode::Full;
        state.branches.tags.set_filter("v\x1b[31m".into());
        let buffer = draw(&state, 100, 24);
        let rows = buffer_rows(&buffer);
        assert!(
            buffer
                .content()
                .iter()
                .all(|c| !c.symbol().contains('\x1b'))
        );
        assert!(contains(&rows, "0 of 4"), "{rows:#?}");

        state.branches.tags.set_filter("v".into());
        state.branches.tags.toggle_range();
        state.branches.tags.move_by(1);
        let buffer = draw(&state, 100, 24);
        let rows = buffer_rows(&buffer);
        assert!(contains(&rows, "/v 3 of 4 · 2 selected"), "{rows:#?}");
        assert!(!contains(&rows, "  x  "), "{rows:#?}");
        let reversed = crate::test_support::reversed_at;
        assert!(reversed(&buffer, "v3  "));
        assert!(reversed(&buffer, "v2  "));
        assert!(!reversed(&buffer, "v1  "));
    }

    #[test]
    fn the_tab_says_loading_none_or_why_it_failed() {
        let mut state = focused(Ok(vec![]));
        assert!(contains(&buffer_rows(&draw(&state, 80, 24)), "No tags"));
        state.branches.tags = TagsList::default();
        assert!(contains(
            &buffer_rows(&draw(&state, 80, 24)),
            "Loading tags…"
        ));
        let state = focused(Err("fatal: broken".into()));
        let rows = buffer_rows(&draw(&state, 80, 24));
        assert!(contains(&rows, "fatal: broken"), "{rows:#?}");
    }

    #[test]
    fn a_tag_push_names_its_remote_in_the_bar() {
        use crate::remote_run::RemoteRun;
        use kika_git::remote::RemoteOp;
        let mut state = focused(Ok(vec![tag(b"v1", true, 'e', "s")]));
        let op = RemoteOp::PushTag {
            remote: "origin".into(),
            tag: "v1".into(),
        };
        state.remote = Some(RemoteRun::new(op, 1, None));
        let rows = buffer_rows(&draw(&state, 100, 24));
        assert!(
            rows.last().unwrap().starts_with(" pushing tag to origin… "),
            "{rows:#?}"
        );
    }

    #[test]
    fn the_shown_tab_stays_in_the_title_at_every_size() {
        use crate::layout::ScreenMode;
        for tab in [BranchesTab::Local, BranchesTab::Remotes, BranchesTab::Tags] {
            let marker = match tab {
                BranchesTab::Local => "[Local]",
                BranchesTab::Remotes => "[Remotes]",
                BranchesTab::Tags => "[Tags]",
            };
            for (w, h, mode) in [
                (80, 24, ScreenMode::Normal),
                (80, 24, ScreenMode::Half),
                (60, 20, ScreenMode::Normal),
                (40, 12, ScreenMode::Normal),
                (40, 12, ScreenMode::Half),
            ] {
                let mut state = focused(Ok(vec![tag(b"v1", true, 'e', "s")]));
                state.branches.tab = tab;
                state.screen_mode = mode;
                let rows = buffer_rows(&draw(&state, w, h));
                let title = rows.iter().find(|r| r.contains("[3]"));
                // At 40 columns a long tab name may shrink, never go.
                let needle = if w >= 60 { marker } else { &marker[..4] };
                assert!(
                    title.is_some_and(|t| t.contains(needle)),
                    "{tab:?} {w}x{h} {mode:?}: {rows:#?}"
                );
            }
        }
        // At 80 columns the other tabs shrink, and the padding stays.
        let state = focused(Ok(vec![]));
        let rows = buffer_rows(&draw(&state, 80, 24));
        assert!(contains(&rows, "┏ [3] Loc Rem [Tags] ━"), "{rows:#?}");
    }

    #[test]
    fn a_long_row_is_cut_to_the_width() {
        let row = tag_row(&tag(&b"v".repeat(50), true, 'd', "subject"), 20);
        assert_eq!(crate::view::width_of(&row), 20);
        assert!(row.starts_with("◆ vvv"), "{row}");
        // Narrow and tiny terminals draw without a panic.
        let state = focused(Ok(vec![tag(b"v1", true, 'e', "s")]));
        for (w, h) in [(1, 1), (2, 3), (10, 4), (20, 6), (0, 0)] {
            draw(&state, w, h);
        }
    }
}
