//! Draws the Status panel: the branch, its upstream and any operation in
//! progress, with the bisect's rows from core under them.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::model::AppState;
use crate::view::{PanelLook, bisect_lines, elide_right, fit, short_hash, width_of};
use kika_git::repo_state::Operation;
use kika_git::status::Head;

/// Where the user is: the branch, its upstream, and any operation in
/// progress. Names too wide for the panel lose their start to `…`, so the
/// counts and markers after them stay visible. Blank when no load has
/// succeeded yet, since the Files panel shows why; dimmed when the last
/// refresh failed, since it may be out of date. Collapsed, one row shows
/// the branch and any operation in progress; with only a title row, the
/// title does.
pub(crate) fn render_status_panel(
    state: &AppState,
    look: &PanelLook,
    expanded: bool,
    frame: &mut Frame,
    area: Rect,
) {
    if area.is_empty() {
        return;
    }
    let stale = state.files.error.is_some() && state.branch.is_some();
    let stale_note = if stale { " (stale)" } else { "" };
    // Inside a submodule, say which one: the paths from the launch
    // repository. A long one loses its start, so the innermost name shows.
    let location: Vec<&str> = state.repo_stack.iter().map(|p| p.path.display()).collect();
    let operations: Vec<&str> = state
        .branch
        .iter()
        .flat_map(|b| b.in_progress.iter().map(|&op| doing(op)))
        .collect();
    let operations = operations.join(", ");
    let title = if let (true, Some(branch)) = (look.title_only, &state.branch) {
        let ops = if operations.is_empty() {
            String::new()
        } else {
            format!(" · {operations}")
        };
        look.path_title(
            "Status ·",
            &head_name(&branch.head),
            &format!("{ops}{stale_note}"),
        )
    } else if location.is_empty() {
        look.title(&format!("Status{stale_note}"))
    } else {
        look.path_title("Status ·", &location.join("/"), stale_note)
    };
    let block = look.block(title);
    let block = if stale {
        block.style(Style::new().fg(Color::DarkGray))
    } else {
        block
    };
    let width = usize::from(block.inner(area).width);
    let Some(branch) = &state.branch else {
        let text = if state.files.loaded { "" } else { "Loading…" };
        frame.render_widget(Paragraph::new(text).block(block), area);
        return;
    };
    let marker = Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    if !expanded {
        // One row: the branch, and what the repository is in the middle of.
        let ops = if operations.is_empty() {
            String::new()
        } else {
            format!(" {operations}")
        };
        let ops = elide_right(&ops, width.saturating_sub(2));
        let head = head_line(&branch.head, width.saturating_sub(width_of(&ops)));
        let line = Line::from(vec![Span::raw(head), Span::styled(ops, marker)]);
        frame.render_widget(Paragraph::new(line).block(block), area);
        return;
    }
    let head = head_line(&branch.head, width);
    let upstream = match (&branch.upstream, branch.ahead_behind) {
        (None, _) => fit("no upstream", "", width),
        (Some(name), Some((ahead, behind))) => fit(name, &format!(" ↑{ahead} ↓{behind}"), width),
        (Some(name), None) => fit(name, " (gone)", width),
    };
    let mut lines = vec![
        Line::raw(head),
        Line::raw(upstream),
        Line::styled(elide_right(&operations, width), marker),
    ];
    if let Some(bisect) = &branch.bisect {
        lines.extend(bisect_lines(state, &branch.head, bisect, width));
    }
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// The branch row, fitted to `width` columns.
fn head_line(head: &Head, width: usize) -> String {
    match head {
        Head::Branch(name) => fit(name, "", width),
        Head::Detached(oid) => fit("detached", &format!(" {}", short_hash(oid)), width),
        Head::Unborn(name) => fit(name, " (no commits)", width),
    }
}

/// The branch in a few words, for a title.
fn head_name(head: &Head) -> String {
    match head {
        Head::Branch(name) | Head::Unborn(name) => name.clone(),
        Head::Detached(oid) => format!("detached {}", short_hash(oid)),
    }
}

/// What the repository is in the middle of, as the panel names it.
fn doing(op: Operation) -> &'static str {
    match op {
        Operation::Rebase => "rebasing",
        Operation::Merge => "merging",
        Operation::CherryPick => "cherry-picking",
        Operation::Revert => "reverting",
        Operation::Bisect => "bisecting",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Context;
    use crate::test_support::screen::{
        branch, contains, draw, draw_buffer, find, on, status_focused, thin,
    };
    use crate::view::buffer_rows;

    /// The three text rows inside the expanded Status panel on an 80x24 screen.
    fn panel_rows(state: &AppState) -> Vec<String> {
        let rows = thin(draw(&status_focused(state), 80, 24));
        assert!(rows[0].contains(" [1] Status "), "{rows:#?}");
        rows[1..4].to_vec()
    }

    #[test]
    fn the_panel_shows_the_branch_and_its_upstream_counts() {
        let mut main = branch(Head::Branch("main".into()));
        main.upstream = Some("origin/main".into());
        main.ahead_behind = Some((2, 1));
        let rows = panel_rows(&on(main));
        assert!(rows[0].starts_with("│main "), "{rows:#?}");
        assert!(rows[1].starts_with("│origin/main ↑2 ↓1 "), "{rows:#?}");
        assert!(rows[2].starts_with("│ "), "{rows:#?}");
    }

    #[test]
    fn the_panel_says_when_there_is_no_upstream() {
        let rows = panel_rows(&on(branch(Head::Branch("topic".into()))));
        assert!(rows[0].starts_with("│topic "), "{rows:#?}");
        assert!(rows[1].starts_with("│no upstream "), "{rows:#?}");
    }

    #[test]
    fn the_panel_marks_an_upstream_that_is_gone() {
        let mut main = branch(Head::Branch("main".into()));
        main.upstream = Some("origin/main".into());
        let rows = panel_rows(&on(main));
        assert!(rows[1].starts_with("│origin/main (gone) "), "{rows:#?}");
    }

    #[test]
    fn a_detached_head_shows_the_short_hash() {
        let rows = panel_rows(&on(branch(Head::Detached("abc1234def5678".into()))));
        assert!(rows[0].starts_with("│detached abc1234 "), "{rows:#?}");
    }

    #[test]
    fn an_unborn_branch_says_it_has_no_commits() {
        let rows = panel_rows(&on(branch(Head::Unborn("main".into()))));
        assert!(rows[0].starts_with("│main (no commits) "), "{rows:#?}");
    }

    #[test]
    fn operations_in_progress_are_named_in_yellow() {
        let mut main = branch(Head::Branch("main".into()));
        main.in_progress = vec![Operation::Merge];
        let state = on(main.clone());
        assert!(panel_rows(&state)[2].starts_with("│merging "));
        let buffer = draw_buffer(&status_focused(&state), 80, 24);
        let (x, y) = find(&buffer, "merging");
        assert_eq!(buffer[(x, y)].style().fg, Some(Color::Yellow));
        // Bold too, so it stands out where yellow reads poorly.
        assert!(buffer[(x, y)].style().add_modifier.contains(Modifier::BOLD));

        main.in_progress = vec![Operation::Rebase];
        assert!(panel_rows(&on(main.clone()))[2].starts_with("│rebasing "));

        main.in_progress = vec![
            Operation::Rebase,
            Operation::Merge,
            Operation::CherryPick,
            Operation::Bisect,
        ];
        let rows = thin(draw(&status_focused(&on(main)), 200, 24));
        assert!(
            contains(&rows, "│rebasing, merging, cherry-picking, bisecting"),
            "{rows:#?}"
        );
    }

    #[test]
    fn the_panel_says_loading_before_the_first_load() {
        let rows = panel_rows(&AppState::new(Context::Workspace));
        assert!(rows[0].starts_with("│Loading… "), "{rows:#?}");
    }

    #[test]
    fn a_failed_first_load_leaves_the_panel_blank_not_loading() {
        let mut state = AppState::new(Context::Workspace);
        state.files.loaded = true;
        state.files.error = Some("fatal: not a git repository".into());
        let rows = panel_rows(&state);
        assert!(!contains(&rows, "Loading"), "{rows:#?}");
    }

    #[test]
    fn a_long_branch_name_is_elided_with_a_marker() {
        let rows = panel_rows(&on(branch(Head::Branch(format!("{}end", "b".repeat(100))))));
        // 22 columns inside the panel: the start gives way, the end stays.
        assert!(
            rows[0].starts_with(&format!("│…{}end││", "b".repeat(21))),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_long_upstream_keeps_its_counts_on_screen() {
        let mut main = branch(Head::Branch("main".into()));
        main.upstream = Some("origin/feature/a-really-long-status-panel".into());
        main.ahead_behind = Some((2, 1));
        let rows = panel_rows(&on(main.clone()));
        assert!(
            rows[1].starts_with("│…-long-status-panel ↑2 ↓1││"),
            "{rows:#?}"
        );

        main.ahead_behind = None;
        let rows = panel_rows(&on(main));
        assert!(
            rows[1].starts_with("│…") && rows[1].contains(" (gone)"),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_long_unborn_branch_keeps_no_commits_on_screen() {
        let rows = panel_rows(&on(branch(Head::Unborn("x".repeat(40)))));
        assert!(
            rows[0].starts_with("│…xxx") && rows[0].contains(" (no commits)"),
            "{rows:#?}"
        );
    }

    #[test]
    fn an_overflowing_operations_row_ends_in_a_marker() {
        let mut main = branch(Head::Branch("main".into()));
        main.in_progress = vec![Operation::Rebase, Operation::Merge, Operation::CherryPick];
        let rows = panel_rows(&on(main));
        assert!(
            rows[2].starts_with("│rebasing, merging, cherr…││"),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_failed_refresh_marks_the_panel_stale() {
        let mut state = on(branch(Head::Branch("main".into())));
        state.files.error = Some("fatal: index file corrupt".into());
        let buffer = draw_buffer(&state, 80, 24);
        let rows = buffer_rows(&buffer);
        assert!(rows[0].contains(" Status (stale) "), "{rows:#?}");
        let (x, y) = find(&buffer, "main");
        assert_eq!(buffer[(x, y)].style().fg, Some(Color::DarkGray));
    }

    fn parent(path: &str) -> crate::model::ParentRepo {
        crate::model::ParentRepo {
            path: path.into(),
            files: vec![],
            files_loaded: true,
            branch: None,
            selected: None,
        }
    }

    #[test]
    fn inside_a_submodule_the_status_title_names_it() {
        let mut state = on(branch(Head::Branch("main".into())));
        state.repo_stack = vec![parent("sub")];
        let rows = draw(&state, 80, 24);
        assert!(rows[0].contains(" Status · sub "), "{rows:#?}");

        // Nested, and with a name that must not drive the terminal.
        state.repo_stack = vec![parent("libs/a"), parent("in\x1bner")];
        state.files.error = Some("boom".into());
        let rows = draw(&state, 160, 24);
        assert!(
            rows[0].contains(" Status · libs/a/in\u{FFFD}ner (stale) "),
            "{rows:#?}"
        );
        // A narrow panel keeps the innermost name.
        let rows = draw(&state, 80, 24);
        assert!(rows[0].contains(" [1] Status · …r (stale) "), "{rows:#?}");
    }

    #[test]
    fn the_launch_repository_has_a_plain_status_title() {
        let state = on(branch(Head::Branch("main".into())));
        let rows = draw(&state, 80, 24);
        assert!(rows[0].starts_with("┌ [1] Status ─"), "{rows:#?}");
    }

    #[test]
    fn tiny_terminals_do_not_panic_with_a_status_panel() {
        let mut main = branch(Head::Branch("a-long-branch-name".into()));
        main.upstream = Some("origin/a-long-branch-name".into());
        main.ahead_behind = Some((12, 345));
        main.in_progress = vec![Operation::Rebase, Operation::Merge];
        let state = on(main);
        for (w, h) in [
            (0, 0),
            (1, 1),
            (10, 2),
            (2, 10),
            (3, 3),
            (5, 4),
            (20, 6),
            (80, 24),
        ] {
            draw(&state, w, h);
        }
    }

    #[test]
    fn a_collapsed_status_still_shows_an_operation_in_progress() {
        let mut main = branch(Head::Branch("main".into()));
        main.in_progress = vec![Operation::Rebase];
        let state = on(main);
        let buffer = draw_buffer(&state, 80, 24);
        let rows = buffer_rows(&buffer);
        assert!(rows[1].starts_with("│main rebasing "), "{rows:#?}");
        let (x, y) = find(&buffer, "rebasing");
        assert_eq!(buffer[(x, y)].style().fg, Some(Color::Yellow));
    }
}
