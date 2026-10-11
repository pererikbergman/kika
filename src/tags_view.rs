//! Draws the Branches panel's Tags tab. The selected tag's log takes the
//! Diff pane's place through the branch log. Reads state only.

use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};

use crate::git::tags::TagRef;
use crate::tags::TagsList;
use crate::view::{elide_right, reversed};

/// What the tab shows when there are no tags.
const NO_TAGS: &str = "No tags";

/// Marks an annotated tag's row; a lightweight one has blanks there.
const ANNOTATED: &str = "◆ ";

/// The tab's body in `rows` by `width`: why the load failed, a
/// placeholder, or one row per tag, the selection reversed and scrolled
/// into view.
pub fn list_body(list: &TagsList, rows: usize, width: usize) -> Paragraph<'static> {
    if let Some(error) = &list.error {
        let mut lines: Vec<Line> = error.lines().map(|l| Line::from(l.to_string())).collect();
        lines.push(Line::from("Press r to retry"));
        return Paragraph::new(lines).wrap(Wrap { trim: false });
    }
    if !list.loaded {
        return Paragraph::new("Loading tags…");
    }
    if list.tags.is_empty() {
        return Paragraph::new(NO_TAGS);
    }
    let selected = list.selected.unwrap_or(0);
    // Scrolled just enough to keep the selection in view.
    let first = selected.saturating_sub(rows.saturating_sub(1));
    let lines = list
        .tags
        .iter()
        .enumerate()
        .skip(first)
        .take(rows)
        .map(|(i, tag)| {
            let line = Line::from(tag_row(tag, width));
            if Some(i) == list.selected {
                reversed(line, width)
            } else {
                line
            }
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
    use crate::branches::BranchesTab;
    use crate::git::commit_files::CommitId;
    use crate::layout::SidePanel;
    use crate::model::{AppState, Context, Focus};
    use crate::view::{buffer_rows, render};

    fn tag(name: &[u8], annotated: bool, oid: char, subject: &str) -> TagRef {
        let oid = oid.to_string().repeat(40);
        TagRef {
            name: crate::git::tags::TagName::from_bytes(name),
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
        state.files_loaded = true;
        state.focus = Focus::Branches;
        state.expanded = SidePanel::Branches;
        state.branches.loaded(Ok(vec![]));
        state.branches.tab = BranchesTab::Tags;
        state.branches.tags.loaded(tags);
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
        use crate::git::remote::RemoteOp;
        use crate::remote_run::RemoteRun;
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
