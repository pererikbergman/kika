//! The renderers' drawing: a renderer's rows in the Diff pane, and a diff
//! tool's title. Core `view.rs` picks which to draw for the pane.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};

use super::RenderedDiff;
use crate::hunk_view::HunkView;
use crate::view::{FOCUSED, PanelLook, elide_right, reversed, width_of};

/// A diff tool's title, ` [n] <head> <path><tail> · <label> (display only) `,
/// fitted so the note stays as long as it can: the label gives way first,
/// then the path, then all but the note. Only too narrow for that does
/// [`PanelLook::title`] cut the note itself.
pub(crate) fn tool_title(
    look: &PanelLook,
    head: &str,
    path: &str,
    tail: &str,
    label: &str,
) -> String {
    const NOTE: &str = " (display only)";
    let whole = format!("{head} {path}{tail} · {label}{NOTE}");
    if look.fits(&whole) {
        return look.title(&whole);
    }
    let without_label = format!("{head} {path}{tail} · {NOTE}");
    let used = width_of(&look.title(&without_label));
    if look.fits(&without_label) && used < look.title_room() {
        let label = elide_right(label, look.title_room() - used);
        return look.title(&format!("{head} {path}{tail} · {label}{NOTE}"));
    }
    let tail = format!("{tail} · …{NOTE}");
    if look.fits(&format!("{head} …{tail}")) {
        return look.path_title(head, path, &tail);
    }
    look.title(NOTE.trim_start())
}

/// A renderer's rows, from the scroll, behind a one-column gutter. With
/// the Diff pane focused (`view`), the selection's rows get a `▌`, and in
/// line mode are reversed too, like the built-in view's.
pub(crate) fn render_rendered(
    rendered: &RenderedDiff,
    block: Block<'static>,
    view: Option<&HunkView>,
    hint: &str,
    frame: &mut Frame,
    area: Rect,
) {
    let inner = block.inner(area);
    let (rows, width) = (usize::from(inner.height), usize::from(inner.width));
    let pane = if let Some(error) = &rendered.error {
        // The way out first, so a long stderr cannot push it off the pane.
        let mut lines = vec![Line::from(hint.to_string()), Line::from("")];
        lines.extend(error.lines().map(Line::from));
        Paragraph::new(lines).wrap(Wrap { trim: false })
    } else if rendered.rendering() {
        Paragraph::new("Rendering…")
    } else if rendered.rows.is_empty() {
        Paragraph::new("No diff")
    } else {
        let selected = view.map(|view| {
            let picked = view.picked();
            (
                rendered.mapped_rows(view.hunk, picked.as_deref()),
                view.lines.is_some(),
            )
        });
        let lines = rendered
            .rows
            .iter()
            .enumerate()
            .skip(rendered.scroll)
            .take(rows)
            .map(|(at, row)| {
                let marked = selected
                    .as_ref()
                    .is_some_and(|(rows, _)| rows.contains(&at));
                let gutter = if marked { "▌" } else { " " };
                let mut spans = vec![Span::styled(gutter, Style::new().fg(FOCUSED))];
                spans.extend(elide_runs(&row.runs, width.saturating_sub(1)));
                let line = Line::from(spans);
                let lines_mode = selected.as_ref().is_some_and(|(_, lines)| *lines);
                if marked && lines_mode {
                    reversed(line, width)
                } else {
                    line
                }
            })
            .collect::<Vec<_>>();
        Paragraph::new(lines)
    };
    frame.render_widget(pane.block(block), area);
}

/// `runs` fitted to `max` columns: a row too wide (a renderer that
/// ignores the width, or a pane that shrank) ends in `…`, never half a
/// wide glyph.
fn elide_runs(runs: &[(Style, String)], max: usize) -> Vec<Span<'_>> {
    let total: usize = runs.iter().map(|(_, text)| width_of(text)).sum();
    if total <= max {
        return runs
            .iter()
            .map(|(style, text)| Span::styled(text.as_str(), *style))
            .collect();
    }
    let mut spans = Vec::new();
    let mut used = 0;
    for (style, text) in runs {
        let room = max.saturating_sub(used);
        if width_of(text) < room {
            used += width_of(text);
            spans.push(Span::styled(text.as_str(), *style));
            continue;
        }
        // The row goes on past this run, so it ends here, in `…`.
        let mut kept = 0;
        let cut: String = text
            .chars()
            .take_while(|c| {
                kept += width_of(c.encode_utf8(&mut [0; 4]));
                kept < room
            })
            .chain(std::iter::once('…'))
            .collect();
        if max > 0 {
            spans.push(Span::styled(cut, *style));
        }
        break;
    }
    spans
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Modifier};

    use super::*;
    use crate::git::diff::{DiffSide, LineKind};
    use crate::layout::SidePanel;
    use crate::model::{AppState, Focus};
    use crate::test_support::screen::{
        TWO_HUNKS, contains, diff_line, draw, draw_buffer, find, hunks, with_diff,
    };
    use crate::view::buffer_rows;

    /// Show `text` as renderer `delta` drew the shown diff, mapped to the
    /// focused view's patch when there is one.
    fn through_renderer(state: &mut AppState, text: &str) {
        use crate::features::diff::renderers::RenderedDiff;
        use crate::features::diff::renderers::external_diff::{ExternalRenderer, map_rows};

        state.renderers = vec![ExternalRenderer {
            label: "delta".into(),
            argv: vec!["delta".into()],
            kind: crate::features::diff::renderers::RendererKind::Pager,
        }];
        state.renderer = Some(0);
        let side = match &state.focus {
            Focus::Diff(view) => view.side,
            _ => DiffSide::Worktree,
        };
        let mut rendered = RenderedDiff::new(1, "a.rs".into(), side);
        rendered.apply(
            1,
            Ok(crate::features::diff::renderers::ansi::parse(
                text.as_bytes(),
            )),
        );
        if let Focus::Diff(HunkView {
            patch: Some(patch), ..
        }) = &state.focus
        {
            rendered.map = map_rows(patch, &rendered.rows);
        }
        state.rendered = Some(rendered);
    }

    /// The view's `TWO_HUNKS`, decorated, with old line numbers in a gutter.
    const DECORATED: &str = "══ a.rs ══\n@1\n\x1b[31m1 │old one\x1b[0m\n\
        \x1b[32m  │new one\x1b[0m\n2 │same\n@9\n9 │old nine\n  │new nine\n";

    #[test]
    fn rendered_rows_replace_the_diff_until_cycled_back() {
        let mut state = with_diff(vec![diff_line(LineKind::Added, "+built in")]);
        through_renderer(&mut state, DECORATED);
        let buffer = draw_buffer(&state, 100, 20);
        let rows = buffer_rows(&buffer);
        assert!(contains(&rows, " Diff a.rs · delta "), "{rows:#?}");
        assert!(contains(&rows, "│ 1 │old one"), "{rows:#?}");
        assert!(!contains(&rows, "built in"), "{rows:#?}");
        let (x, y) = find(&buffer, "1 │old one");
        assert_eq!(buffer[(x, y)].style().fg, Some(Color::Red));

        state.update(crate::intent::AppIntent::CycleRenderer);
        let rows = draw(&state, 100, 20);
        assert!(contains(&rows, "+built in"), "{rows:#?}");
        assert!(contains(&rows, " Diff "), "{rows:#?}");
        assert!(!contains(&rows, "delta"), "{rows:#?}");
    }

    #[test]
    fn an_open_commits_file_shows_its_streamed_diff_not_a_stale_render() {
        use crate::commit_files::CommitFilesList;
        use crate::git::commit_files::{CommitId, CommitRange};

        // A commit's file of the same name as the rendered worktree file.
        let mut state = with_diff(vec![diff_line(LineKind::Added, "+in the commit")]);
        through_renderer(&mut state, DECORATED);
        state.focus = Focus::Commits;
        state.expanded = SidePanel::Commits;
        let range = CommitRange {
            oid: CommitId::parse("abc").unwrap(),
            parent: None,
        };
        state.commits.open = Some(CommitFilesList::begin(range, "abc".into(), 1));

        let rows = draw(&state, 100, 20);

        assert!(contains(&rows, "+in the commit"), "{rows:#?}");
        assert!(!contains(&rows, "old one"), "{rows:#?}");
        assert!(!contains(&rows, "delta"), "{rows:#?}");
    }

    #[test]
    fn the_line_cursor_marks_only_the_rendered_row_of_its_line() {
        let mut state = hunks(DiffSide::Worktree, TWO_HUNKS);
        if let Focus::Diff(view) = &mut state.focus {
            view.enter_lines();
            view.next_line(false);
        }
        through_renderer(&mut state, DECORATED);
        let buffer = draw_buffer(&state, 100, 20);
        let rows = buffer_rows(&buffer);
        assert!(
            contains(&rows, " Unstaged a.rs · lines · delta "),
            "{rows:#?}"
        );
        let marked: Vec<&String> = rows.iter().filter(|r| r.contains('▌')).collect();
        assert_eq!(marked.len(), 1, "{rows:#?}");
        assert!(marked[0].contains("▌  │new one"), "{rows:#?}");
        let (x, y) = find(&buffer, "│new one");
        assert!(
            buffer[(x, y)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn in_hunk_mode_the_hunks_rendered_rows_get_the_gutter() {
        let mut state = hunks(DiffSide::Worktree, TWO_HUNKS);
        through_renderer(&mut state, DECORATED);
        let rows = draw(&state, 100, 20);
        for marked in ["▌1 │old one", "▌  │new one", "▌2 │same"] {
            assert!(contains(&rows, marked), "{marked}: {rows:#?}");
        }
        assert!(contains(&rows, " 9 │old nine"), "{rows:#?}");
        assert!(contains(&rows, " @1"), "{rows:#?}");
    }

    #[test]
    fn a_side_by_side_rendering_falls_back_to_the_built_in_view() {
        let mut state = hunks(DiffSide::Worktree, TWO_HUNKS);
        if let Focus::Diff(view) = &mut state.focus {
            view.enter_lines();
        }
        // Each row ends with the new side, so `-old one` has no row.
        through_renderer(&mut state, "old one │ new one\nsame │ same\n");
        let rows = draw(&state, 100, 20);
        assert!(
            contains(&rows, " Unstaged a.rs · lines · delta (unmapped) "),
            "{rows:#?}"
        );
        assert!(contains(&rows, "▌-old one"), "{rows:#?}");
        assert!(!contains(&rows, "old one │ new one"), "{rows:#?}");
    }

    #[test]
    fn a_diff_tools_rows_are_drawn_display_only_without_a_cursor() {
        let mut state = hunks(DiffSide::Worktree, TWO_HUNKS);
        // Side by side, so a pager's rows here would be unmapped.
        through_renderer(&mut state, "old one │ new one\nsame │ same\n");
        state.renderers[0].label = "difft".into();
        state.renderers[0].kind = crate::features::diff::renderers::RendererKind::Tool;
        if let Some(rendered) = &mut state.rendered {
            rendered.map.clear();
        }
        let rows = draw(&state, 100, 20);
        assert!(
            contains(&rows, " Unstaged a.rs · difft (display only) "),
            "{rows:#?}"
        );
        assert!(contains(&rows, "old one │ new one"), "{rows:#?}");
        assert!(!contains(&rows, "-old one"), "{rows:#?}");
        assert!(!rows.iter().any(|r| r.contains('▌')), "{rows:#?}");
    }

    /// Show `text` as tool `label` drew the shown diff: rows that map to
    /// no patch line.
    fn through_tool(state: &mut AppState, label: &str, text: &str) {
        through_renderer(state, text);
        state.renderers[0].label = label.into();
        state.renderers[0].kind = crate::features::diff::renderers::RendererKind::Tool;
        if let Some(rendered) = &mut state.rendered {
            rendered.map.clear();
        }
    }

    /// A tool label too long for any pane's title at 80 columns.
    const LONG_LABEL: &str = "a-diff-tool-with-a-very-long-name-that-goes-on-and-on-and-on";

    /// The Diff pane's title row, from its key `[0]` on.
    fn diff_title(rows: &[String]) -> &str {
        rows.iter()
            .find_map(|r| r.find("[0]").map(|at| &r[at..]))
            .unwrap_or_else(|| panic!("no Diff pane title: {rows:#?}"))
    }

    #[test]
    fn a_long_tool_label_gives_way_to_the_display_only_note() {
        assert_eq!(LONG_LABEL.len(), 60);
        let mut state = hunks(DiffSide::Worktree, TWO_HUNKS);
        through_tool(&mut state, LONG_LABEL, "old one │ new one\n");
        let rows = draw(&state, 80, 24);
        let title = diff_title(&rows);
        assert!(title.contains(" (display only)"), "{rows:#?}");
        assert!(title.contains("Unstaged a.rs · a-diff-tool"), "{rows:#?}");
        assert!(title.contains("… (display only)"), "{rows:#?}");
        assert!(!title.contains(LONG_LABEL), "{rows:#?}");
    }

    #[test]
    fn a_narrow_tool_view_still_says_display_only() {
        let mut state = hunks(DiffSide::Worktree, TWO_HUNKS);
        through_tool(&mut state, LONG_LABEL, "old one │ new one\n");
        for (width, height) in [(40, 12), (30, 8)] {
            let rows = draw(&state, width, height);
            let title = diff_title(&rows);
            assert!(
                title.contains("(display only)"),
                "{width}x{height}: {rows:#?}"
            );
        }
        // Narrower, the usual title elision applies; it draws without a panic.
        for (width, height) in [(0, 0), (1, 1), (10, 4), (20, 6)] {
            draw(&state, width, height);
        }
    }

    #[test]
    fn a_tool_title_cuts_the_label_then_the_path_then_all_but_the_note() {
        let look = |width: usize| PanelLook {
            number: 0,
            focused: true,
            mode: String::new(),
            title_width: width,
            top_only: false,
            title_only: false,
        };
        let title = |width| tool_title(&look(width), "Diff", "src/a.rs", " · x", "difft");
        assert_eq!(title(46), " [0] Diff src/a.rs · x · difft (display only) ");
        assert_eq!(title(48), title(46));
        assert_eq!(title(45), " [0] Diff src/a.rs · x · dif… (display only) ");
        assert_eq!(title(42), " [0] Diff src/a.rs · x · … (display only) ");
        assert_eq!(title(40), " [0] Diff …/a.rs · x · … (display only) ");
        assert_eq!(title(35), " [0] Diff … · x · … (display only) ");
        assert_eq!(title(34), " [0] (display only) ");
        assert_eq!(title(19), "[0] (display only)");
        assert_eq!(title(10), "[0] (disp…");
    }

    #[test]
    fn a_side_panels_diff_title_says_a_tool_view_is_display_only() {
        for label in ["difft", LONG_LABEL] {
            let mut state = with_diff(vec![diff_line(LineKind::Added, "+built in")]);
            through_tool(&mut state, label, "old one │ new one\n");
            assert_eq!(state.focus, Focus::Files);
            let rows = draw(&state, 80, 24);
            let title = diff_title(&rows);
            assert!(title.contains("(display only)"), "{label}: {rows:#?}");
            assert!(title.contains(" Diff a.rs · "), "{label}: {rows:#?}");
            assert!(contains(&rows, "old one │ new one"), "{label}: {rows:#?}");
        }
        let mut state = with_diff(vec![diff_line(LineKind::Added, "+built in")]);
        through_renderer(&mut state, DECORATED);
        let rows = draw(&state, 80, 24);
        let title = diff_title(&rows);
        assert!(title.contains(" Diff a.rs · delta "), "{rows:#?}");
        assert!(!title.contains("(display only)"), "{rows:#?}");
    }

    #[test]
    fn a_renderer_error_or_a_render_in_flight_says_so() {
        let mut state = with_diff(vec![diff_line(LineKind::Added, "+built in")]);
        through_renderer(&mut state, "");
        if let Some(rendered) = &mut state.rendered {
            rendered.error = Some("delta: bad \u{FFFD} flag".into());
        }
        let rows = draw(&state, 100, 20);
        assert!(contains(&rows, "delta: bad \u{FFFD} flag"), "{rows:#?}");
        assert!(
            contains(&rows, "Press l, then R, for the built-in diff"),
            "{rows:#?}"
        );

        if let Some(rendered) = &mut state.rendered {
            rendered.error = None;
            rendered.seq = Some(2);
        }
        let rows = draw(&state, 100, 20);
        assert!(contains(&rows, "Rendering…"), "{rows:#?}");
    }

    #[test]
    fn a_row_wider_than_the_pane_ends_in_an_ellipsis() {
        let mut state = with_diff(vec![]);
        let wide = format!(
            "\x1b[31m{}\x1b[0m{}\n日本語日本語日本語日本語日本語\n",
            "a".repeat(20),
            "b".repeat(20)
        );
        through_renderer(&mut state, &wide);
        // 40 columns: the Diff pane's inside is 26 wide, 25 after the gutter.
        let rows = draw(&state, 40, 10);
        let expected = format!("│ {}{}…│", "a".repeat(20), "b".repeat(3));
        assert!(contains(&rows, &expected), "{rows:#?}");
        // No half glyph at the edge: 11 two-column glyphs, then `…`.
        assert!(contains(&rows, "│ 日本語日本語日本語日本… │"), "{rows:#?}");
        let wider = draw(&state, 100, 10);
        let whole = format!("{}{}", "a".repeat(20), "b".repeat(20));
        assert!(contains(&wider, &whole), "{wider:#?}");
    }

    #[test]
    fn the_way_out_of_an_error_stays_on_screen_and_names_the_next_step() {
        let mut state = with_diff(vec![]);
        through_renderer(&mut state, "");
        let long: Vec<String> = (0..60).map(|i| format!("stderr line {i}")).collect();
        if let Some(rendered) = &mut state.rendered {
            rendered.error = Some(long.join("\n"));
        }
        let rows = draw(&state, 80, 24);
        assert!(
            contains(&rows, "Press l, then R, for the built-in diff"),
            "{rows:#?}"
        );
        assert!(contains(&rows, "stderr line 0"), "{rows:#?}");

        state
            .renderers
            .push(crate::features::diff::renderers::ExternalRenderer {
                label: "riff".into(),
                argv: vec!["riff".into()],
                kind: crate::features::diff::renderers::RendererKind::Pager,
            });
        let rows = draw(&state, 80, 24);
        assert!(
            contains(&rows, "Press l, then R, for the next renderer"),
            "{rows:#?}"
        );
    }

    #[test]
    fn tiny_terminals_do_not_panic_with_a_renderer_active() {
        let text: String = (0..50)
            .map(|i| format!("\x1b[32m{i} │日本 one {}\x1b[0m\n", "x".repeat(i)))
            .collect();
        let mut files = with_diff(vec![]);
        through_renderer(&mut files, &text);
        let mut hunk = hunks(DiffSide::Worktree, TWO_HUNKS);
        through_renderer(&mut hunk, DECORATED);
        let mut lines = hunk.clone();
        if let Focus::Diff(view) = &mut lines.focus {
            view.enter_lines();
        }
        through_renderer(&mut lines, DECORATED);
        let mut fallback = lines.clone();
        through_renderer(&mut fallback, "old one │ new one\n");
        let mut error = files.clone();
        if let Some(rendered) = &mut error.rendered {
            rendered.error = Some("bad\nnews".into());
        }
        let mut rendering = files.clone();
        if let Some(rendered) = &mut rendering.rendered {
            rendered.rows.clear();
            rendered.seq = Some(9);
        }
        for mut state in [files, hunk, lines, fallback, error, rendering] {
            for scroll in [0, 3, 500] {
                if let Some(rendered) = &mut state.rendered {
                    rendered.scroll = scroll;
                }
                for (w, h) in [(0, 0), (1, 1), (10, 3), (2, 10), (3, 3), (7, 5), (80, 24)] {
                    draw(&state, w, h);
                }
            }
        }
    }
}
