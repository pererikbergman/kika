//! Draws the streamed Diff pane and the focused pane's title: the file,
//! where the pane is in it, the diff options that differ from the
//! defaults and a search's matches. Reads state only.

use ratatui::Frame;
use ratatui::layout::Rect;

use crate::diff_pane::DiffPane;
use crate::diff_pane_view::{diff_pane_body, diff_position, found_note};
use crate::hunk_view::HunkView;
use crate::view::PanelLook;
use kika_git::diff::{DiffOptions, DiffSide};

/// The diff options that differ from the defaults, for the pane title, so
/// a changed setting is never mistaken for the file's real diff.
pub(crate) fn options_note(options: DiffOptions) -> String {
    let default = DiffOptions::default();
    let mut note = String::new();
    if options.context != default.context {
        note.push_str(&format!(" · {} lines context", options.context));
    }
    if options.ignore_whitespace {
        note.push_str(" · whitespace hidden");
    }
    note
}

/// The Diff pane: an error, a delayed "Loading…", "No diff", or the visible
/// lines. The title names the file and where in it the pane is: the first
/// line shown of the lines loaded, `+` while more are coming. A search's
/// matches are marked, and the title counts them, `+` while more lines
/// can bring more.
pub(crate) fn render_diff_pane(
    diff: &DiffPane,
    options: DiffOptions,
    look: &PanelLook,
    frame: &mut Frame,
    area: Rect,
) {
    let found = found_note(diff.search.as_ref(), !diff.done);
    let tail = format!("{}{found}{}", diff_position(diff), options_note(options));
    let title = match &diff.path {
        Some(path) => look.path_title("Diff", path.display(), &tail),
        None => look.title(&format!("Diff{tail}")),
    };
    let block = look.block(title);
    let inner = block.inner(area);
    let pane = diff_pane_body(&look.palette, diff, inner);
    frame.render_widget(pane.block(block), area);
}

/// The focused pane's title: the side, the path, the mode, the options
/// and `suffix` after a `·`, the path giving way to fit.
pub(crate) fn hunk_title(
    view: &HunkView,
    options: DiffOptions,
    suffix: &str,
    look: &PanelLook,
) -> String {
    let (side, tail) = hunk_title_parts(view, options);
    let suffix = if suffix.is_empty() {
        String::new()
    } else {
        format!(" · {suffix}")
    };
    look.path_title(side, view.path.display(), &format!("{tail}{suffix}"))
}

/// The focused pane's side word and what follows its path: the mode, the
/// options and the search.
pub(crate) fn hunk_title_parts(view: &HunkView, options: DiffOptions) -> (&'static str, String) {
    let side = match view.side {
        _ if view.commit.is_some() => "Commit",
        DiffSide::Index => "Staged",
        DiffSide::Worktree | DiffSide::Untracked => "Unstaged",
    };
    // The mode is in words too, for when the bar is cut off.
    let mode = match view.lines {
        Some(_) => " · lines",
        None => "",
    };
    let found = found_note(view.search.as_ref(), false);
    (side, format!("{mode}{}{found}", options_note(options)))
}

#[cfg(test)]
mod tests {
    use crate::test_support::screen::{
        TWO_HUNKS, contains, diff_line, draw, draw_buffer, find, hunks, with_diff,
    };
    use kika_git::diff::{DiffSide, LineKind};
    use ratatui::style::{Color, Modifier};

    #[test]
    fn diff_lines_are_coloured_inside_the_diff_block() {
        use LineKind::*;
        let state = with_diff(vec![
            diff_line(Header, "diff --git a/a.rs b/a.rs"),
            diff_line(Header, "--- a/a.rs"),
            diff_line(Header, "+++ b/a.rs"),
            diff_line(Hunk, "@@ -1 +1 @@"),
            diff_line(Removed, "-old"),
            diff_line(Added, "+new"),
            diff_line(Context, " same"),
        ]);
        let buffer = draw_buffer(&state, 80, 12);
        let (title_x, title_y) = find(&buffer, " [0] Diff ");
        let style = |needle| {
            let (x, y) = find(&buffer, needle);
            assert!(
                x >= title_x && y > title_y,
                "{needle:?} is inside the block"
            );
            buffer[(x, y)].style()
        };

        assert_eq!(style("-old").fg, Some(Color::Red));
        assert_eq!(style("+new").fg, Some(Color::Green));
        assert_eq!(style("@@ -1").fg, Some(Color::Cyan));
        let header = style("diff --git");
        assert!(header.add_modifier.contains(Modifier::BOLD));
        for needle in ["--- a/a.rs", "+++ b/a.rs"] {
            let header = style(needle);
            assert!(header.add_modifier.contains(Modifier::BOLD), "{needle}");
            assert!(
                matches!(header.fg, None | Some(Color::Reset)),
                "{needle} is not coloured as a change"
            );
        }
        assert!(matches!(style(" same").fg, None | Some(Color::Reset)));
    }

    #[test]
    fn only_the_scrolled_window_of_a_long_diff_is_drawn() {
        let mut state = with_diff(
            (0..50_000)
                .map(|i| diff_line(LineKind::Context, &format!("line {i}")))
                .collect(),
        );
        state.diff.scroll = 1_000;
        let rows = draw(&state, 80, 10);
        assert!(contains(&rows, "│line 1000 "), "{rows:#?}");
        assert!(contains(&rows, "│line 1006 "), "{rows:#?}");
        assert!(!contains(&rows, "line 999 "), "{rows:#?}");
        assert!(!contains(&rows, "line 1007 "), "{rows:#?}");
    }

    #[test]
    fn slow_diff_shows_loading_in_the_diff_pane() {
        let mut state = with_diff(vec![diff_line(LineKind::Added, "+old file")]);
        state.diff.pending = true;
        state.diff.done = false;
        state.diff.show_loading = true;
        let rows = draw(&state, 80, 8);
        assert!(contains(&rows, "│Loading…"), "{rows:#?}");
        assert!(!contains(&rows, "+old file"), "{rows:#?}");
    }

    #[test]
    fn a_pending_diff_keeps_showing_the_previous_lines() {
        let mut state = with_diff(vec![diff_line(LineKind::Added, "+old file")]);
        state.diff.pending = true;
        state.diff.done = false;
        let rows = draw(&state, 80, 8);
        assert!(contains(&rows, "│+old file "), "{rows:#?}");
        assert!(!contains(&rows, "Loading"), "{rows:#?}");
    }

    #[test]
    fn an_empty_finished_diff_says_no_diff() {
        let rows = draw(&with_diff(vec![]), 80, 8);
        assert!(contains(&rows, "│No diff"), "{rows:#?}");
    }

    #[test]
    fn a_diff_error_is_shown_in_the_diff_pane() {
        let mut state = with_diff(vec![]);
        state.diff.error = Some("fatal: bad object".into());
        let rows = draw(&state, 80, 8);
        assert!(contains(&rows, "│fatal: bad object"), "{rows:#?}");
        assert!(!contains(&rows, "No diff"), "{rows:#?}");
    }

    #[test]
    fn the_diff_title_names_the_diff_options_that_differ_from_the_defaults() {
        use kika_git::diff::DiffOptions;
        let mut state = with_diff(vec![diff_line(LineKind::Added, "+new")]);
        let rows = draw(&state, 120, 12);
        assert!(contains(&rows, "┌ [0] Diff a.rs · 1/1 ─"), "{rows:#?}");
        assert!(!contains(&rows, "context"), "{rows:#?}");

        state.diff_options = DiffOptions {
            context: 5,
            ignore_whitespace: false,
        };
        let rows = draw(&state, 120, 12);
        assert!(
            contains(&rows, "┌ [0] Diff a.rs · 1/1 · 5 lines context ─"),
            "{rows:#?}"
        );

        state.diff_options.ignore_whitespace = true;
        let rows = draw(&state, 120, 12);
        assert!(
            contains(
                &rows,
                "┌ [0] Diff a.rs · 1/1 · 5 lines context · whitespace hidden ─"
            ),
            "{rows:#?}"
        );

        state.diff_options.context = 3;
        let rows = draw(&state, 120, 12);
        assert!(
            contains(&rows, "┌ [0] Diff a.rs · 1/1 · whitespace hidden ─"),
            "{rows:#?}"
        );
    }

    #[test]
    fn the_focused_title_names_the_diff_options_too() {
        let mut state = hunks(DiffSide::Worktree, TWO_HUNKS);
        state.diff_options.context = 7;
        state.diff_options.ignore_whitespace = true;
        let rows = draw(&state, 100, 20);
        assert!(
            contains(
                &rows,
                " Unstaged a.rs · 7 lines context · whitespace hidden "
            ),
            "{rows:#?}"
        );
    }

    #[test]
    fn the_diff_title_shows_the_path_and_where_the_pane_is() {
        let mut state = with_diff(
            (0..50)
                .map(|i| diff_line(LineKind::Context, &format!("line {i}")))
                .collect(),
        );
        state.diff.path = Some("src/deeply/nested/module/file.rs".into());
        state.diff.scroll = 9;
        let rows = draw(&state, 120, 12);
        assert!(
            contains(
                &rows,
                "┌ [0] Diff src/deeply/nested/module/file.rs · 10/50 ─"
            ),
            "{rows:#?}"
        );
        // Still streaming: more lines are coming.
        state.diff.done = false;
        let rows = draw(&state, 120, 12);
        assert!(contains(&rows, " · 10/50+ ─"), "{rows:#?}");
        // A narrow pane loses the path's start, never the position.
        let rows = draw(&state, 60, 12);
        assert!(
            contains(&rows, "┌ [0] Diff …ed/module/file.rs · 10/50+ ┐"),
            "{rows:#?}"
        );
    }
}
