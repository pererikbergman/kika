//! The commit popup's rendering: the stage-all, loading and discard
//! questions, the message editor with its Subject and Body boxes, the
//! running commit's footer, and a failed commit's output.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, Paragraph};

use super::CommitEditor;
use super::editor::Field;
use crate::model::CommitFlow;
use crate::theme::Palette;
use crate::view::{COMMIT_WIDTH, centered, clip, render_question, tail, width_of, wrap};

/// The footer of a running commit: where Ctrl+C goes, which depends on
/// whether git has the terminal.
const DETACHED_FOOTER: &str = " waiting for git · ctrl-c quits Kika ";
const ON_TERMINAL_FOOTER: &str = " git has the terminal · ctrl-c stops git ";

/// The commit popup for the current step, over `area`. `on_terminal` says
/// a running commit was handed the terminal.
pub(crate) fn render_commit(
    palette: &Palette,
    commit: &CommitFlow,
    on_terminal: bool,
    frame: &mut Frame,
    area: Rect,
) {
    match commit {
        CommitFlow::Closed => {}
        CommitFlow::ConfirmStageAll => render_question(
            " Commit ",
            "Nothing is staged. Stage all changes and commit? (y/n)",
            frame,
            area,
        ),
        CommitFlow::LoadingAmend { .. } => render_question(
            " Amend last commit ",
            "Loading the last commit…",
            frame,
            area,
        ),
        CommitFlow::ConfirmDiscard(_) => render_question(
            " Discard message ",
            "Discard the typed message? (y/n)",
            frame,
            area,
        ),
        CommitFlow::Editing(editor) => render_commit_editor(palette, editor, None, frame, area),
        CommitFlow::Running(editor) => {
            let footer = if on_terminal {
                ON_TERMINAL_FOOTER
            } else {
                DETACHED_FOOTER
            };
            render_commit_editor(palette, editor, Some(footer), frame, area)
        }
        CommitFlow::Failed { output, .. } => render_commit_failed(palette, output, frame, area),
    }
}

/// The message popup: a one-line Subject box over a Body box. While
/// editing, the focused box is marked and holds the terminal cursor; a
/// `running` commit shows that footer instead. When there is no room for
/// both boxes, only the focused one is drawn.
fn render_commit_editor(
    palette: &Palette,
    editor: &CommitEditor,
    running: Option<&str>,
    frame: &mut Frame,
    area: Rect,
) {
    let editing = running.is_none();
    // Two outer borders, a three-row Subject box and a Body box of ten text rows.
    let popup = centered(area, COMMIT_WIDTH, 2 + 3 + 12);
    frame.render_widget(Clear, popup);
    let block = match (running, editor.amend.is_some()) {
        (None, true) => Block::bordered().title(" Amend last commit "),
        (None, false) => Block::bordered().title(" Commit message "),
        (Some(footer), _) => Block::bordered()
            .title(" Committing… ")
            .title_bottom(footer),
    };
    // Skipping hooks skips the user's own checks, so it is never quiet.
    let block = if editor.no_verify {
        block.title(
            Line::styled(" hooks skipped ", Style::new().fg(palette.warning)).right_aligned(),
        )
    } else {
        block
    };
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let focus = editing.then_some(editor.field);
    let subject = TextBox {
        palette,
        title: "Subject",
        lines: std::slice::from_ref(&editor.subject),
        cursor: (focus == Some(Field::Subject)).then_some((0, editor.col)),
    };
    let body = TextBox {
        palette,
        title: "Body",
        lines: &editor.body,
        cursor: (focus == Some(Field::Body)).then_some((editor.row, editor.col)),
    };
    // Each box needs three rows to show one line of text.
    let cursor = if inner.height >= 6 {
        let [top, bottom] =
            Layout::vertical([Constraint::Length(3), Constraint::Fill(1)]).areas(inner);
        subject.render(frame, top).or(body.render(frame, bottom))
    } else if focus == Some(Field::Body) {
        body.render(frame, inner)
    } else {
        subject.render(frame, inner)
    };
    if let Some(position) = cursor {
        frame.set_cursor_position(position);
    }
}

/// A bordered box of text lines. With a `(row, col)` cursor it is focused:
/// marked, and scrolled so the cursor is in view. Without one it shows its
/// start. Text cut off at either side is marked with `…`.
struct TextBox<'a> {
    palette: &'a Palette,
    title: &'a str,
    lines: &'a [String],
    cursor: Option<(usize, usize)>,
}

impl TextBox<'_> {
    /// Draw into `area`; return the cursor's screen position if it shows.
    fn render(&self, frame: &mut Frame, area: Rect) -> Option<(u16, u16)> {
        let block = match self.cursor {
            Some(_) => Block::bordered()
                .title(format!(" ▸ {} ", self.title))
                .title_style(Style::new().add_modifier(Modifier::BOLD))
                .border_style(Style::new().fg(self.palette.focused)),
            None => Block::bordered().title(format!(" {} ", self.title)),
        };
        let inner = block.inner(area);
        if inner.is_empty() {
            frame.render_widget(block, area);
            return None;
        }
        let (width, height) = (usize::from(inner.width), usize::from(inner.height));
        let (row, col) = self.cursor.unwrap_or_default();
        let line = self.lines.get(row).map_or("", String::as_str);
        let before: String = line.chars().take(col).collect();
        let x = width_of(&before);
        // Scroll just enough that the cursor's row is inside, and its column
        // is inside with one column to spare for a `…` after it.
        let top = (row + 1).saturating_sub(height);
        let left = (x + 2).saturating_sub(width.max(2));
        let text: Vec<Line> = self
            .lines
            .iter()
            .skip(top)
            .take(height)
            .map(|line| Line::raw(clip(line, left, width)))
            .collect();
        frame.render_widget(Paragraph::new(text).block(block), area);
        let at = |offset: usize, start: u16| start + u16::try_from(offset).unwrap_or(u16::MAX);
        self.cursor
            .map(|_| (at(x - left, inner.x), at(row - top, inner.y)))
    }
}

/// The output of a failed commit, wrapped. When it does not fit, the end is
/// shown, since that is where hooks usually say what went wrong, and the
/// first row says how much is cut. The way back is always on the last row.
fn render_commit_failed(palette: &Palette, output: &str, frame: &mut Frame, area: Rect) {
    let width = COMMIT_WIDTH.min(area.width);
    let rows = wrap(output, usize::from(width.saturating_sub(2)).max(1));
    let height = u16::try_from(rows.len() + 3).unwrap_or(u16::MAX);
    let popup = centered(area, width, height);
    frame.render_widget(Clear, popup);
    let block = Block::bordered().title(" Commit failed ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let [text, hint] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(inner);
    frame.render_widget(
        Paragraph::new(tail(palette, &rows, usize::from(text.height))),
        text,
    );
    frame.render_widget(
        Line::styled("Esc: back to the message", Style::new().fg(palette.warning)),
        hint,
    );
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::model::AppState;
    use crate::test_support::screen::{contains, draw, draw_with_cursor, file, find, loaded};
    use crate::view::{buffer_rows, render};

    fn commit_editor(subject: &str, body: &[&str]) -> CommitEditor {
        let mut editor = CommitEditor::new(false);
        editor.subject = subject.into();
        editor.body = body.iter().map(|line| line.to_string()).collect();
        editor.col = subject.chars().count();
        editor
    }

    fn with_commit(commit: CommitFlow) -> AppState {
        let mut state = loaded(vec![file("a.rs")]);
        state.files.selected = 0;
        state.commit = commit;
        state
    }

    #[test]
    fn the_stage_all_confirm_asks_and_the_bar_offers_its_keys() {
        let rows = draw(&with_commit(CommitFlow::ConfirmStageAll), 80, 12);
        assert!(contains(&rows, " Commit "), "{rows:#?}");
        assert!(
            contains(
                &rows,
                "Nothing is staged. Stage all changes and commit? (y/n)"
            ),
            "{rows:#?}"
        );
        assert!(rows[11].starts_with(" y stage all  n cancel"), "{rows:#?}");
    }

    #[test]
    fn the_editor_shows_subject_and_body_with_the_cursor_in_the_subject() {
        let editor = commit_editor("Fix the thing", &["line 1", "line 2"]);
        let state = with_commit(CommitFlow::Editing(editor));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| render(&state, frame)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let rows = buffer_rows(&buffer);

        assert!(contains(&rows, " Commit message "), "{rows:#?}");
        assert!(contains(&rows, " Subject "), "{rows:#?}");
        assert!(contains(&rows, " Body "), "{rows:#?}");
        let (x, y) = find(&buffer, "Fix the thing");
        assert!(contains(&rows, "│line 1 "), "{rows:#?}");
        assert!(contains(&rows, "│line 2 "), "{rows:#?}");
        let line1 = find(&buffer, "line 1");
        assert!(line1.1 > y, "the body is below the subject");
        // The cursor sits after the subject, and its box is highlighted.
        let cursor = terminal.get_cursor_position().unwrap();
        assert_eq!((cursor.x, cursor.y), (x + 13, y));
        assert_eq!(
            buffer[(x - 1, y)].style().fg,
            Some(Palette::DEFAULT.focused)
        );
        assert_ne!(
            buffer[(line1.0 - 1, line1.1)].style().fg,
            Some(Palette::DEFAULT.focused)
        );
        assert!(
            rows[23].starts_with(
                " q and ? are typed  ctrl-s commit  Tab subject/body  esc cancel  ctrl-c stop"
            ),
            "{rows:#?}"
        );
    }

    #[test]
    fn the_cursor_follows_the_body_row_and_col() {
        let mut editor = commit_editor("s", &["ab", "日本x", "cd"]);
        editor.field = Field::Body;
        editor.row = 1;
        editor.col = 2;
        let state = with_commit(CommitFlow::Editing(editor));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| render(&state, frame)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        // `find` reads one cell per char, so locate the wide line by its neighbour.
        let (x, below) = find(&buffer, "cd");
        let y = below - 1;
        assert_eq!(buffer[(x, y)].symbol(), "日");
        let cursor = terminal.get_cursor_position().unwrap();
        // Two wide glyphs before the cursor: four columns.
        assert_eq!((cursor.x, cursor.y), (x + 4, y));
        assert_eq!(
            buffer[(x - 1, y)].style().fg,
            Some(Palette::DEFAULT.focused)
        );
    }

    #[test]
    fn a_long_body_scrolls_to_keep_the_cursor_row_visible() {
        let lines: Vec<String> = (0..40).map(|i| format!("body {i}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut editor = commit_editor("s", &refs);
        editor.field = Field::Body;
        editor.row = 39;
        editor.col = 0;
        let state = with_commit(CommitFlow::Editing(editor));
        let rows = draw(&state, 80, 24);
        assert!(contains(&rows, "│body 39 "), "{rows:#?}");
        assert!(!contains(&rows, "│body 0 "), "{rows:#?}");
    }

    #[test]
    fn only_the_focused_box_scrolls_to_the_cursor() {
        let long = |start: &str| format!("{start}{}", "x".repeat(100));
        let editor = commit_editor(&long("subject"), &[&long("body")]);
        let rows = draw(&with_commit(CommitFlow::Editing(editor)), 80, 24);
        assert!(contains(&rows, "│bodyxxx"), "{rows:#?}");
        assert!(!contains(&rows, "│subjectxxx"), "{rows:#?}");

        let mut editor = commit_editor(&long("subject"), &[&long("body")]);
        editor.field = Field::Body;
        editor.col = 104;
        let rows = draw(&with_commit(CommitFlow::Editing(editor)), 80, 24);
        assert!(contains(&rows, "│subjectxxx"), "{rows:#?}");
        assert!(!contains(&rows, "│bodyxxx"), "{rows:#?}");
    }

    #[test]
    fn a_running_commit_says_committing() {
        let state = with_commit(CommitFlow::Running(commit_editor("subj", &[""])));
        let rows = draw(&state, 80, 24);
        assert!(contains(&rows, " Committing… "), "{rows:#?}");
        assert!(contains(&rows, "subj"), "{rows:#?}");
    }

    #[test]
    fn an_amend_popup_is_titled_amend_last_commit() {
        let state = with_commit(CommitFlow::Editing(CommitEditor::amend("subj\n\nbody")));
        let rows = draw(&state, 80, 24);
        assert!(contains(&rows, " Amend last commit "), "{rows:#?}");
        assert!(!contains(&rows, " Commit message "), "{rows:#?}");
        assert!(contains(&rows, "subj"), "{rows:#?}");
        assert!(contains(&rows, "│body "), "{rows:#?}");
    }

    #[test]
    fn hooks_skipped_shows_only_when_skipping_hooks() {
        let mut editor = commit_editor("subj", &[""]);
        let rows = draw(&with_commit(CommitFlow::Editing(editor.clone())), 80, 24);
        assert!(!contains(&rows, "hooks skipped"), "{rows:#?}");

        editor.no_verify = true;
        let rows = draw(&with_commit(CommitFlow::Editing(editor.clone())), 80, 24);
        assert!(contains(&rows, " hooks skipped "), "{rows:#?}");
        // On the popup's title row, next to its title.
        assert_eq!(
            find_row(&rows, " hooks skipped "),
            find_row(&rows, " Commit message ")
        );
        let rows = draw(&with_commit(CommitFlow::Running(editor)), 80, 24);
        assert!(contains(&rows, " hooks skipped "), "{rows:#?}");
    }

    fn find_row(rows: &[String], needle: &str) -> Option<usize> {
        rows.iter().position(|row| row.contains(needle))
    }

    #[test]
    fn loading_the_last_commit_draws_a_small_box() {
        let state = with_commit(CommitFlow::LoadingAmend { seq: 3 });
        let rows = draw(&state, 80, 24);
        assert!(contains(&rows, "Loading the last commit…"), "{rows:#?}");
        assert!(
            rows[23].starts_with(" esc cancel  ctrl-c quit"),
            "{rows:#?}"
        );
    }

    #[test]
    fn tiny_terminals_do_not_panic_with_an_amend_popup() {
        let mut editor = CommitEditor::amend("subj");
        editor.no_verify = true;
        for commit in [
            CommitFlow::LoadingAmend { seq: 0 },
            CommitFlow::Editing(editor),
        ] {
            let state = with_commit(commit);
            for (w, h) in [(1, 1), (5, 3), (12, 4), (30, 6)] {
                draw(&state, w, h);
            }
        }
    }

    #[test]
    fn a_failed_commit_shows_the_output_and_how_to_go_back() {
        let state = with_commit(CommitFlow::Failed {
            editor: commit_editor("subj", &[""]),
            output: "lint failed\nsrc/a.rs:3: unused import".into(),
        });
        let rows = draw(&state, 80, 24);
        assert!(contains(&rows, " Commit failed "), "{rows:#?}");
        assert!(contains(&rows, "lint failed"), "{rows:#?}");
        assert!(contains(&rows, "src/a.rs:3: unused import"), "{rows:#?}");
        assert!(contains(&rows, "Esc: back to the message"), "{rows:#?}");
    }

    #[test]
    fn long_failure_output_shows_its_tail_and_says_what_is_cut() {
        let output: Vec<String> = (0..100).map(|i| format!("error {i}")).collect();
        let state = with_commit(CommitFlow::Failed {
            editor: commit_editor("subj", &[""]),
            output: output.join("\n"),
        });
        let rows = draw(&state, 80, 24);
        // 23 rows: two borders and the hint leave 20, one of them the cut line.
        assert!(contains(&rows, "│… 81 earlier lines "), "{rows:#?}");
        assert!(contains(&rows, "│error 81 "), "{rows:#?}");
        assert!(contains(&rows, "│error 99 "), "{rows:#?}");
        assert!(!contains(&rows, "│error 80 "), "{rows:#?}");
        assert!(contains(&rows, "Esc: back to the message"), "{rows:#?}");
    }

    #[test]
    fn failure_lines_wider_than_the_popup_wrap_and_count_as_rows() {
        let state = with_commit(CommitFlow::Failed {
            editor: commit_editor("subj", &[""]),
            output: format!("{}\nlast", "w".repeat(100)),
        });
        let rows = draw(&state, 40, 12);
        assert!(
            contains(&rows, &format!("│{}│", "w".repeat(38))),
            "{rows:#?}"
        );
        assert!(contains(&rows, "│last "), "{rows:#?}");
        assert!(!contains(&rows, "earlier"), "{rows:#?}");
    }

    #[test]
    fn the_running_popup_says_how_to_get_out() {
        let state = with_commit(CommitFlow::Running(commit_editor("subj", &[""])));
        let rows = draw(&state, 80, 24);
        assert!(contains(&rows, "ctrl-c quits Kika"), "{rows:#?}");
    }

    #[test]
    fn a_handed_over_commit_says_ctrl_c_goes_to_git() {
        let mut state = with_commit(CommitFlow::Running(commit_editor("subj", &[""])));
        state.commit_on_terminal = true;
        let rows = draw(&state, 80, 24);
        assert!(contains(&rows, "ctrl-c stops git"), "{rows:#?}");
        assert!(!contains(&rows, "quits Kika"), "{rows:#?}");
    }

    #[test]
    fn the_discard_confirm_asks_before_throwing_the_message_away() {
        let state = with_commit(CommitFlow::ConfirmDiscard(commit_editor("subj", &[""])));
        let rows = draw(&state, 80, 12);
        assert!(
            contains(&rows, "Discard the typed message? (y/n)"),
            "{rows:#?}"
        );
        assert!(rows[11].starts_with(" y discard  n keep"), "{rows:#?}");
    }

    #[test]
    fn a_narrow_confirm_wraps_and_keeps_y_n_visible() {
        let rows = draw(&with_commit(CommitFlow::ConfirmStageAll), 40, 12);
        assert!(contains(&rows, "(y/n)"), "{rows:#?}");
        assert!(contains(&rows, "Nothing is staged."), "{rows:#?}");
    }

    #[test]
    fn the_focused_box_is_marked_in_text_not_only_colour() {
        let state = with_commit(CommitFlow::Editing(commit_editor("s", &[""])));
        let rows = draw(&state, 80, 24);
        assert!(contains(&rows, "▸ Subject "), "{rows:#?}");
        assert!(!contains(&rows, "▸ Body "), "{rows:#?}");
    }

    #[test]
    fn a_short_terminal_still_shows_the_focused_body_and_its_cursor() {
        let mut editor = commit_editor("subj", &["body text"]);
        editor.field = Field::Body;
        editor.col = 4;
        let state = with_commit(CommitFlow::Editing(editor));
        for height in [6, 8, 9] {
            let (buffer, cursor) = draw_with_cursor(&state, 80, height);
            let rows = buffer_rows(&buffer);
            assert!(contains(&rows, "▸ Body "), "{height}: {rows:#?}");
            let (x, y) = find(&buffer, "body text");
            assert_eq!(cursor, (x + 4, y), "{height}: {rows:#?}");
        }
    }

    #[test]
    fn a_long_subject_scrolls_with_a_marker_and_the_cursor_stays_inside() {
        let editor = commit_editor(&format!("start{}", "a".repeat(95)), &[""]);
        let state = with_commit(CommitFlow::Editing(editor));
        let (buffer, cursor) = draw_with_cursor(&state, 80, 24);
        let rows = buffer_rows(&buffer);
        // The popup is 72 wide, so the Subject box has 68 text columns.
        let (x, y) = find(&buffer, "▸ Subject");
        // The title is " ▸ Subject ", so the box's inside starts one left of "▸".
        let inner_x = x - 1;
        assert_eq!(buffer[(inner_x, y + 1)].symbol(), "…", "{rows:#?}");
        assert!(!contains(&rows, "start"), "{rows:#?}");
        // The cursor is after the last char, one column short of the edge.
        assert_eq!(cursor, (inner_x + 66, y + 1));
        assert_eq!(buffer[(inner_x + 65, y + 1)].symbol(), "a");
    }

    #[test]
    fn a_wide_glyph_cut_by_the_scroll_edge_does_not_shift_the_cursor() {
        // A 71-column screen makes the Subject box 67 wide: an odd scroll
        // offset cuts a two-column glyph in half.
        let editor = commit_editor(&"日".repeat(60), &[""]);
        let state = with_commit(CommitFlow::Editing(editor));
        let (buffer, cursor) = draw_with_cursor(&state, 71, 24);
        let (x, y) = find(&buffer, "▸ Subject");
        // The title is " ▸ Subject ", so the box's inside starts one left of "▸".
        let inner_x = x - 1;
        assert_eq!(buffer[(inner_x, y + 1)].symbol(), "…");
        assert_eq!(cursor, (inner_x + 65, y + 1));
        assert_eq!(buffer[(inner_x + 63, y + 1)].symbol(), "日");
    }

    #[test]
    fn an_unfocused_line_cut_at_the_right_ends_in_a_marker() {
        let editor = commit_editor("s", &[&"b".repeat(100)]);
        let state = with_commit(CommitFlow::Editing(editor));
        let rows = draw(&state, 80, 24);
        assert!(
            contains(&rows, &format!("│{}…│", "b".repeat(67))),
            "{rows:#?}"
        );
    }

    #[test]
    fn tiny_terminals_do_not_panic_with_a_commit_popup() {
        let mut editor = commit_editor("a long subject line", &["one", "two", "three"]);
        editor.field = Field::Body;
        editor.row = 2;
        editor.col = 5;
        let flows = [
            CommitFlow::ConfirmStageAll,
            CommitFlow::Editing(editor.clone()),
            CommitFlow::Running(editor.clone()),
            CommitFlow::Failed {
                editor,
                output: "a long hook failure line that wraps\nand more".into(),
            },
        ];
        for flow in flows {
            let state = with_commit(flow);
            for (w, h) in [(0, 0), (1, 1), (10, 2), (2, 10), (3, 3), (5, 4), (20, 6)] {
                draw(&state, w, h);
            }
        }
    }

    #[test]
    fn a_20_by_6_screen_still_shows_the_typed_subject() {
        let state = with_commit(CommitFlow::Editing(commit_editor("subj", &[""])));
        let rows = draw(&state, 20, 6);
        assert!(contains(&rows, "subj"), "{rows:#?}");
    }
}
