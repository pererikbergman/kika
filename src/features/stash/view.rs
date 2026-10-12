//! Draws the Stash panel: the stash list, or one entry's files.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use super::list::{StashFilesList, StashList};
use crate::git::stash::StashEntry;
use crate::rows::commit_file_line;
use crate::view::{
    PanelLook, elide_right, list_title, no_matches, range_row, reversed, scrolled, width_of,
};

/// The panel: a placeholder until the list loads, then one row per entry
/// the filter shows, scrolled so the cursor shows, the cursor and the
/// range reversed. Collapsed, the newest entry's row.
pub(crate) fn render_stash_panel(
    stash: &StashList,
    look: &PanelLook,
    expanded: bool,
    frame: &mut Frame,
    area: Rect,
) {
    if let Some(open) = stash.open.as_ref().filter(|_| expanded) {
        return render_stash_files(open, look, frame, area);
    }
    let title = if expanded {
        let range = stash.range();
        list_title(
            look,
            "Stash",
            &stash.filter,
            stash.visible().len(),
            stash.entries.len(),
            stash.range_anchor.is_some().then_some(range.len()),
        )
    } else {
        "Stash".to_string()
    };
    let block = look.block(look.title(&title));
    let inner = block.inner(area);
    let (height, width) = (usize::from(inner.height), usize::from(inner.width));
    let paragraph = match &stash.error {
        Some(error) => error_paragraph(error, Some("Press r to retry")),
        None if stash.entries.is_empty() && !stash.loaded => Paragraph::new("Loading…"),
        None if stash.entries.is_empty() => {
            Paragraph::new(Line::styled("No stashes", Style::new().fg(Color::DarkGray)))
        }
        None if !expanded => Paragraph::new(entry_line(&stash.entries[0], width)),
        None => {
            let shown = stash.visible();
            if shown.is_empty() {
                Paragraph::new(no_matches())
            } else {
                let range = stash.range();
                let selected = Some(stash.selected);
                Paragraph::new(
                    scrolled(&shown, selected, height.max(1))
                        .iter()
                        .map(|&i| {
                            let line = entry_line(&stash.entries[i], width);
                            range_row(line, i, selected, &range, width)
                        })
                        .collect::<Vec<_>>(),
                )
            }
        }
    };
    frame.render_widget(paragraph.block(block), area);
}

/// `stash@{N}  <subject>`, as the filter matches it, the subject cut to `width`.
fn entry_line(entry: &StashEntry, width: usize) -> Line<'static> {
    let name = entry.stash_ref().arg();
    let room = width.saturating_sub(width_of(&name) + 2);
    Line::from(vec![
        Span::styled(name, Style::new().fg(Color::Yellow)),
        Span::raw("  "),
        Span::raw(elide_right(&entry.subject, room)),
    ])
}

/// An error's lines with `! ` before the first, wrapped, and a hint after.
fn error_paragraph(error: &str, hint: Option<&str>) -> Paragraph<'static> {
    let mut lines: Vec<Line> = error.lines().map(|l| Line::from(l.to_string())).collect();
    if let Some(first) = lines.first_mut() {
        first.spans.insert(0, Span::raw("! "));
    }
    lines.extend(hint.map(|h| Line::from(h.to_string())));
    Paragraph::new(lines).wrap(Wrap { trim: false })
}

/// An entry's files in place of the list, as a commit's are: a
/// placeholder until they load, then one row per file. Untracked files
/// show as added.
fn render_stash_files(open: &StashFilesList, look: &PanelLook, frame: &mut Frame, area: Rect) {
    let block = look.block(look.title(&open.stash.arg()));
    let inner = block.inner(area);
    let (height, width) = (usize::from(inner.height), usize::from(inner.width));
    let paragraph = match &open.error {
        _ if !open.loaded => Paragraph::new("Loading…"),
        Some(error) => error_paragraph(error, None),
        None if open.files.is_empty() => Paragraph::new(Line::styled(
            "No files in this stash",
            Style::new().fg(Color::DarkGray),
        )),
        None => {
            let start = (open.selected + 1).saturating_sub(height.max(1));
            let end = (start + height).min(open.files.len());
            Paragraph::new(
                (start..end)
                    .map(|i| {
                        let line = commit_file_line(&open.files[i].file, width);
                        if i == open.selected {
                            reversed(line, width)
                        } else {
                            line
                        }
                    })
                    .collect::<Vec<_>>(),
            )
        }
    };
    frame.render_widget(paragraph.block(block), area);
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Modifier;

    use super::*;
    use crate::git::commit_files::{CommitFile, CommitId, CommitRange};
    use crate::git::diff::{DiffChunk, DiffLine, LineKind};
    use crate::git::stash::StashFile;
    use crate::layout::{ScreenMode, SidePanel};
    use crate::model::{AppState, Context, Focus};
    use crate::view::{buffer_rows, render};

    fn entry(index: usize, oid: &str, subject: &str) -> StashEntry {
        StashEntry {
            index,
            oid: CommitId::parse(oid).unwrap(),
            base: CommitId::parse("ba5e").unwrap(),
            untracked: None,
            subject: subject.into(),
        }
    }

    /// The Stash panel focused and zoomed, listing `entries`.
    fn showing(entries: Vec<StashEntry>) -> AppState {
        let mut state = AppState::new(Context::Workspace);
        state.files_loaded = true;
        state.focus = Focus::Stash;
        state.expanded = SidePanel::Stash;
        state.screen_mode = ScreenMode::Full;
        state.stash.begin(1);
        state.stash.loaded(1, Ok(entries));
        state
    }

    fn rows(state: &AppState, width: u16, height: u16) -> (ratatui::buffer::Buffer, Vec<String>) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(state, frame)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let rows = buffer_rows(&buffer);
        (buffer, rows)
    }

    fn row_of(rows: &[String], needle: &str) -> usize {
        rows.iter()
            .position(|r| r.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} not on screen:\n{}", rows.join("\n")))
    }

    #[test]
    fn the_panel_lists_each_stash_with_its_subject_and_marks_the_cursor() {
        let mut state = showing(vec![
            entry(0, "a1", "WIP on main: 1234 first"),
            entry(1, "a2", "On main: second"),
        ]);
        state.stash.move_by(1);
        let (buffer, rows) = rows(&state, 80, 12);
        let first = row_of(&rows, "stash@{0}  WIP on main: 1234 first");
        let second = row_of(&rows, "stash@{1}  On main: second");
        assert!(rows[0].contains("[5] Stash"), "{rows:#?}");
        let x = rows[second].find("On main").unwrap() as u16;
        let reversed = |y: usize| {
            buffer[(x, y as u16)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        };
        assert!(reversed(second));
        assert!(!reversed(first));
    }

    fn three() -> AppState {
        showing(vec![
            entry(0, "a0", "WIP on main: zero"),
            entry(1, "a1", "On main: one"),
            entry(2, "a2", "On main: two"),
        ])
    }

    #[test]
    fn a_filter_shows_only_matching_rows_and_names_itself_in_the_title() {
        let mut state = three();
        state.stash.set_filter("zero".into());
        let (_, rows) = rows(&state, 80, 12);
        row_of(&rows, "Stash · /zero 1 of 3");
        row_of(&rows, "stash@{0}  WIP on main: zero");
        assert!(!rows.iter().any(|r| r.contains("one")), "{rows:#?}");
    }

    #[test]
    fn a_hostile_filter_matches_nothing_and_draws_made_safe() {
        let mut state = three();
        state.stash.set_filter("x\x1b[2J\u{202E}\r\0".into());
        let (_, rows) = rows(&state, 80, 12);
        row_of(&rows, "No matches");
        row_of(&rows, "Stash · /");
        row_of(&rows, " 0 of 3");
        for row in &rows {
            assert!(
                !row.chars().any(|c| c.is_control() || c == '\u{202E}'),
                "{row:?}"
            );
        }
    }

    #[test]
    fn range_rows_are_drawn_reversed_and_counted_in_the_title() {
        let mut state = three();
        state.stash.toggle_range();
        state.stash.move_by(1);
        let (buffer, rows) = rows(&state, 80, 12);
        row_of(&rows, "Stash · 2 selected");
        let reversed = |needle: &str| {
            let y = row_of(&rows, needle);
            let x = rows[y].find(needle).unwrap() as u16;
            buffer[(x, y as u16)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        };
        assert!(reversed("WIP on main: zero"), "the anchor");
        assert!(reversed("On main: one"), "the cursor");
        assert!(!reversed("On main: two"));
    }

    #[test]
    fn tiny_terminals_do_not_panic_with_a_filter_or_a_range() {
        let mut filtered = three();
        filtered.stash.set_filter("on\u{202E}".into());
        let mut ranged = three();
        ranged.stash.toggle_range();
        ranged.stash.move_by(2);
        let mut hidden = three();
        hidden.stash.set_filter("zzz".into());
        for state in [&filtered, &ranged, &hidden] {
            for (w, h) in [(0, 0), (1, 1), (20, 5), (5, 20), (80, 2)] {
                rows(state, w, h);
            }
        }
    }

    #[test]
    fn an_empty_list_says_no_stashes_and_a_failed_one_says_why() {
        let (_, empty) = rows(&showing(vec![]), 80, 12);
        row_of(&empty, "No stashes");

        let mut state = showing(vec![]);
        state.stash.begin(2);
        state.stash.loaded(2, Err("fatal: bad reflog".into()));
        let (_, failed) = rows(&state, 80, 12);
        row_of(&failed, "! fatal: bad reflog");
        row_of(&failed, "Press r to retry");

        let mut loading = AppState::new(Context::Workspace);
        loading.focus = Focus::Stash;
        loading.expanded = SidePanel::Stash;
        let (_, rows) = rows(&loading, 80, 24);
        row_of(&rows, "Loading…");
    }

    #[test]
    fn the_list_scrolls_to_keep_the_cursor_on_screen() {
        let entries = (0..30)
            .map(|i| entry(i, "a1", &format!("subject {i:02}")))
            .collect();
        let mut state = showing(entries);
        state.stash.move_by(25);
        let (_, rows) = rows(&state, 80, 12);
        row_of(&rows, "stash@{25}  subject 25");
        assert!(!rows.iter().any(|r| r.contains("subject 00")), "{rows:#?}");
    }

    #[test]
    fn at_80x24_the_collapsed_panel_is_its_title_and_the_newest_stash() {
        let mut state = showing(vec![entry(0, "a1", "newest"), entry(1, "a2", "older")]);
        state.screen_mode = ScreenMode::Normal;
        state.focus = Focus::Files;
        state.expanded = SidePanel::Files;
        let (_, rows) = rows(&state, 80, 24);
        let title = row_of(&rows, "[5] Stash");
        assert!(rows[title].starts_with("┌ [5] Stash ─"), "{rows:#?}");
        assert!(
            rows[title + 1].starts_with("│stash@{0}  newest"),
            "{rows:#?}"
        );
        assert!(!rows.iter().any(|r| r.contains("older")), "{rows:#?}");
    }

    #[test]
    fn collapsed_the_panel_shows_the_newest_stash() {
        let mut state = showing(vec![entry(0, "a1", "newest"), entry(1, "a2", "older")]);
        state.screen_mode = ScreenMode::Normal;
        state.focus = Focus::Files;
        state.expanded = SidePanel::Files;
        let (_, rows) = rows(&state, 100, 30);
        row_of(&rows, "stash@{0}  newest");
        assert!(!rows.iter().any(|r| r.contains("older")), "{rows:#?}");
    }

    fn stash_file(status: char, path: &str, untracked: bool) -> StashFile {
        StashFile {
            range: CommitRange {
                oid: CommitId::parse(if untracked { "dd" } else { "a1" }).unwrap(),
                parent: (!untracked).then(|| CommitId::parse("ba5e").unwrap()),
            },
            file: CommitFile {
                status,
                path: path.into(),
                orig_path: None,
            },
        }
    }

    fn opened(result: Option<Result<Vec<StashFile>, String>>) -> AppState {
        let mut state = showing(vec![entry(0, "a1", "first"), entry(1, "a2", "second")]);
        state.stash.move_by(1);
        let at = state.stash.selected_entry().unwrap().stash_ref();
        let mut open = StashFilesList::begin(at, 3);
        if let Some(result) = result {
            open.loaded(3, result);
        }
        state.stash.open = Some(open);
        state
    }

    #[test]
    fn an_open_stash_lists_its_files_under_its_name_with_untracked_ones_added() {
        let files = vec![
            stash_file('M', "src/lib.rs", false),
            stash_file('A', "notes.txt", true),
        ];
        let (_, rows) = rows(&opened(Some(Ok(files))), 80, 12);
        assert!(rows[0].contains("[5] stash@{1}"), "{rows:#?}");
        row_of(&rows, "M src/lib.rs");
        row_of(&rows, "A notes.txt");
        assert!(!rows.iter().any(|r| r.contains("second")), "{rows:#?}");
    }

    #[test]
    fn an_open_stash_says_loading_failed_or_empty() {
        for (result, needle) in [
            (None, "Loading…"),
            (
                Some(Err("fatal: bad object".to_string())),
                "! fatal: bad object",
            ),
            (Some(Ok(vec![])), "No files in this stash"),
        ] {
            let (_, rows) = rows(&opened(result), 80, 12);
            row_of(&rows, needle);
            row_of(&rows, "stash@{1}");
        }
    }

    #[test]
    fn the_diff_pane_draws_the_open_stashs_file_diff() {
        let mut state = opened(Some(Ok(vec![stash_file('M', "src/lib.rs", false)])));
        state.screen_mode = ScreenMode::Normal;
        state.diff.begin(9, &"src/lib.rs".into(), 20);
        state.diff.apply(
            9,
            Ok(DiffChunk {
                lines: vec![DiffLine {
                    kind: LineKind::Added,
                    text: "+put aside".into(),
                }],
                done: true,
            }),
        );
        let (_, rows) = rows(&state, 120, 20);
        row_of(&rows, "+put aside");
        row_of(&rows, "stash@{1}");
    }

    #[test]
    fn tiny_terminals_do_not_panic_with_the_stash_list_or_an_open_stash() {
        let list = showing(vec![entry(0, "a1", "first"), entry(1, "a2", "second")]);
        let open = opened(Some(Ok(vec![stash_file('M', "src/lib.rs", false)])));
        for state in [&list, &open] {
            for (w, h) in [(0, 0), (1, 1), (20, 5), (5, 20), (80, 2)] {
                rows(state, w, h);
            }
        }
    }

    #[test]
    fn a_wide_subject_is_cut_on_a_character_boundary_within_the_panel() {
        let mut state = showing(vec![entry(0, "a1", "日本語の変更を一時的に退避した作業")]);
        state.screen_mode = ScreenMode::Normal;
        let (_, rows) = rows(&state, 60, 24);
        let row = rows[row_of(&rows, "stash@{0}")].clone();
        assert!(row.contains("stash@{0}  日"), "{row:?}");
        assert!(row.contains('…'), "{row:?}");
        assert!(!row.contains('\u{FFFD}'), "{row:?}");
        // The cut row fills the panel exactly: its right border stays put.
        let title = &rows[row_of(&rows, "[5] Stash")];
        let panel = |line: &str, left: char, right: char| {
            let from = line.find(left).unwrap();
            let to = line[from + left.len_utf8()..].find(right).unwrap() + from;
            width_of(&line[from..to])
        };
        assert_eq!(panel(&row, '┃', '┃'), panel(title, '┏', '┓'), "{row:?}");
    }
}
