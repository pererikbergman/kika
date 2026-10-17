//! Draws the Worktrees popup. Reads state only.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

use super::git::Worktree;
use super::list::{WorktreeList, is_current, label};
use crate::model::AppState;
use crate::popup_view::{ListBox, ListPopup, draw_list_popup};
use crate::view::{centered, clip, elide_left, elide_right, width_of, wrap};
use kika_git::safe_text::sanitize;

/// One row per worktree the filter shows: `*` for the one Kika is in, the
/// branch (or what stands for one), the path and its tags. A long path
/// loses its start to `…`. The cursor row is reversed while the list is on
/// top. Until a load lands, and when it fails, that is said instead. A
/// filter is named in the title, sanitised, with how many rows it shows.
pub fn render_worktrees(
    state: &AppState,
    list: &WorktreeList,
    top: bool,
    frame: &mut Frame,
    area: Rect,
) {
    draw_list_popup(worktrees_popup(state, list, top, area), false, frame);
}

/// The Worktrees popup laid out in `area`: what it draws and where a
/// click finds its rows.
pub(crate) fn worktrees_popup(
    state: &AppState,
    list: &WorktreeList,
    top: bool,
    area: Rect,
) -> ListPopup {
    let room = usize::from(area.width.saturating_sub(2));
    let visible = list.list.visible();
    let items = &list.list.rows;
    let listed = list.list.loaded && list.list.error.is_none() && !visible.is_empty();
    let rows: Vec<String> = match (list.list.loaded, &list.list.error) {
        (_, Some(error)) => {
            let safe: Vec<String> = error.lines().map(|l| sanitize(l.as_bytes())).collect();
            wrap(&safe.join("\n"), room.saturating_sub(2).max(1))
                .into_iter()
                .map(|row| format!(" {row} "))
                .collect()
        }
        (false, None) => vec![" Loading… ".into()],
        (true, None) if items.is_empty() => vec![" No worktrees ".into()],
        (true, None) if visible.is_empty() => vec![" No matches ".into()],
        (true, None) => visible
            .iter()
            .map(|&i| row(&items[i], is_current(state, &items[i]), room))
            .collect(),
    };
    // The cursor's place among the rows drawn.
    let cursor = (top && list.selected().is_some())
        .then(|| visible.iter().position(|&i| i == list.list.selected))
        .flatten();
    // As `centered` will cut it: the screen may be shorter than the list.
    let height = (rows.len() + 2).min(usize::from(area.height));
    let shown = height.saturating_sub(2).max(1);
    let total = items.len();
    let title = title(&list.list.filter, visible.len(), total, cursor, shown, room);
    let width = rows
        .iter()
        .map(|row| width_of(row))
        .chain([width_of(&title)])
        .max()
        .unwrap_or(0)
        .min(room);
    let lines: Vec<Line<'static>> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let text = clip(row, 0, width);
            let style = if Some(i) == cursor {
                Style::new().add_modifier(Modifier::REVERSED)
            } else {
                Style::new()
            };
            Line::styled(text, style)
        })
        .collect();
    let rect = centered(
        area,
        u16::try_from(width + 2).unwrap_or(u16::MAX),
        u16::try_from(height).unwrap_or(u16::MAX),
    );
    let first = cursor.map_or(0, |cursor| cursor.saturating_sub(shown - 1));
    ListPopup {
        list: ListBox {
            rect,
            first,
            header: 0,
            len: if listed { visible.len() } else { 0 },
            cursor,
        },
        title,
        lines,
    }
}

/// ` Worktrees `, or with a filter ` Worktrees · /<filter> <n> of <total> `.
/// A list cut by a short screen adds where the cursor is in it. The filter
/// is drawn sanitised and loses its end so the title fits `room` columns.
fn title(
    filter: &str,
    matched: usize,
    total: usize,
    cursor: Option<usize>,
    shown: usize,
    room: usize,
) -> String {
    let place = match cursor {
        Some(cursor) if shown < matched => Some(format!("{}/{matched}", cursor + 1)),
        _ => None,
    };
    if filter.is_empty() {
        return match place {
            Some(place) => format!(" Worktrees {place} "),
            None => " Worktrees ".to_string(),
        };
    }
    let place = place.map(|p| format!(" · {p}")).unwrap_or_default();
    let fixed = format!(" Worktrees · / {matched} of {total}{place} ");
    let query = elide_right(
        &sanitize(filter.as_bytes()),
        room.saturating_sub(width_of(&fixed)),
    );
    format!(" Worktrees · /{query} {matched} of {total}{place} ")
}

/// Columns of path kept, when room is short, before the branch is cut.
const MIN_PATH: usize = 10;

/// `<marker> <branch>  <path>  <tags>`, fitted to `room` columns. The
/// marker and the tags always stay, since they say which worktree is here,
/// locked, missing or unsure. The path loses its start first, down to
/// [`MIN_PATH`] columns; then the branch loses its end. Both cuts show `…`.
fn row(worktree: &Worktree, current: bool, room: usize) -> String {
    let marker = if current { '*' } else { ' ' };
    let label = label(worktree);
    let tags: Vec<&str> = [
        (worktree.main, "main"),
        (worktree.locked, "locked"),
        (worktree.prunable, "missing"),
        (worktree.unsure, "?"),
    ]
    .into_iter()
    .filter_map(|(on, tag)| on.then_some(tag))
    .collect();
    let head = format!(" {marker} ");
    let tail = if tags.is_empty() {
        " ".to_string()
    } else {
        format!("  {} ", tags.join(" "))
    };
    let path = worktree.path.display();
    // Room for the branch, the two-column gap and the path.
    let avail = room.saturating_sub(width_of(&head) + width_of(&tail));
    let (label_w, path_w) = (width_of(&label), width_of(path));
    let (label_room, path_room) = if label_w + 2 + path_w <= avail {
        (label_w, path_w)
    } else if label_w + 2 + MIN_PATH.min(path_w) <= avail {
        (label_w, avail - label_w - 2)
    } else {
        let path_room = MIN_PATH.min(path_w).min(avail.saturating_sub(2 + 4));
        let gap = if path_room > 0 { 2 } else { 0 };
        (avail.saturating_sub(path_room + gap), path_room)
    };
    let gap = if path_room > 0 { "  " } else { "" };
    format!(
        "{head}{}{gap}{}{tail}",
        elide_right(&label, label_room),
        elide_left(path, path_room)
    )
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::features::worktrees::git::parse_list;
    use crate::list_filter::{InputTarget, ListInput};
    use crate::model::Context;
    use crate::popup::{LineInput, Popup};
    use crate::test_support::load_list;
    use crate::view::{buffer_rows, render};

    const OID: &str = "abcdef1234567890abcdef1234567890abcdef12";

    fn state_with(out: &[u8], repo: &str) -> AppState {
        let mut list = WorktreeList::loading(0);
        load_list(&mut list.list, Ok(parse_list(out, 0)));
        let mut state =
            AppState::new(Context::Workspace).with_location(Some(repo.into()), "/".into());
        state.popups = vec![Popup::Worktrees(list)];
        state
    }

    fn draw(state: &AppState) -> (ratatui::buffer::Buffer, Vec<String>) {
        draw_sized(state, 100, 20)
    }

    fn draw_sized(
        state: &AppState,
        width: u16,
        height: u16,
    ) -> (ratatui::buffer::Buffer, Vec<String>) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(state, frame)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let rows = buffer_rows(&buffer);
        (buffer, rows)
    }

    fn line_with<'a>(rows: &'a [String], needle: &str) -> (usize, &'a String) {
        rows.iter()
            .enumerate()
            .find(|(_, row)| row.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} not on screen: {rows:#?}"))
    }

    #[test]
    fn rows_show_branch_path_tags_and_the_current_marker_with_the_cursor_reversed() {
        let out = format!(
            "worktree /r/main\0HEAD {OID}\0branch refs/heads/main\0\0\
             worktree /r/wt-feature\0HEAD {OID}\0branch refs/heads/feature\0locked why\0\0\
             worktree /r/gone\0HEAD {OID}\0detached\0prunable\0\0"
        );
        let mut state = state_with(out.as_bytes(), "/r/wt-feature");
        if let Some(Popup::Worktrees(list)) = state.popups.last_mut() {
            list.list.move_by(2);
        }
        let (buffer, rows) = draw(&state);
        line_with(&rows, " Worktrees ");
        let (_, main) = line_with(&rows, "/r/main");
        assert!(main.contains("   main  /r/main  main"), "{main}");
        let (_, feature) = line_with(&rows, "/r/wt-feature");
        assert!(
            feature.contains(" * feature  /r/wt-feature  locked"),
            "{feature}"
        );
        let (y, gone) = line_with(&rows, "/r/gone");
        assert!(
            gone.contains("(detached) abcdef1  /r/gone  missing"),
            "{gone}"
        );

        let x = gone.find("(detached)").unwrap();
        let x = gone[..x].chars().count() as u16;
        let cell = &buffer[(x, y as u16)];
        assert!(cell.style().add_modifier.contains(Modifier::REVERSED));
        let (my, main) = line_with(&rows, "/r/main");
        let mx = main[..main.find("/r/main").unwrap()].chars().count() as u16;
        assert!(
            !buffer[(mx, my as u16)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn a_hostile_path_or_branch_draws_no_escape_byte() {
        let out = b"worktree /r/\x1b[31mevil\0branch refs/heads/x\x1b]0;t\x07\0\0";
        let state = state_with(out, "/r");
        let (_, rows) = draw(&state);
        line_with(&rows, "[31mevil");
        assert!(
            rows.iter()
                .all(|row| !row.contains('\x1b') && !row.contains('\x07'))
        );
    }

    #[test]
    fn loading_empty_and_failed_lists_say_so() {
        let mut state = AppState::new(Context::Workspace);
        state.popups = vec![Popup::Worktrees(WorktreeList::loading(0))];
        line_with(&draw(&state).1, "Loading…");
        let mut list = WorktreeList::loading(0);
        load_list(&mut list.list, Err("fatal: \x1b[2Jbroken".into()));
        state.popups = vec![Popup::Worktrees(list)];
        let (_, rows) = draw(&state);
        line_with(&rows, "broken");
        assert!(rows.iter().all(|row| !row.contains('\x1b')));
    }

    #[test]
    fn a_long_path_loses_its_start_and_the_row_fits_the_screen() {
        let long = "/very/long/".repeat(20) + "tail-dir";
        let out = format!("worktree {long}\0bare\0\0");
        let state = state_with(out.as_bytes(), "/r");
        let (_, rows) = draw(&state);
        let (_, row) = line_with(&rows, "tail-dir");
        assert!(row.contains('…'), "{row}");
        assert!(row.contains("(bare)"), "{row}");
    }

    /// `n` linked worktrees after the main one, each on its own branch,
    /// the cursor on the last.
    fn many(n: usize) -> AppState {
        let mut out = format!("worktree /r/main\0HEAD {OID}\0branch refs/heads/main\0\0");
        for i in 1..=n {
            out.push_str(&format!(
                "worktree /r/wt-{i}\0HEAD {OID}\0branch refs/heads/b{i}\0\0"
            ));
        }
        let mut state = state_with(out.as_bytes(), "/r/main");
        if let Some(Popup::Worktrees(list)) = state.popups.last_mut() {
            list.list.move_by(n as isize);
        }
        state
    }

    /// The row drawn reversed, if any.
    fn reversed_row(buffer: &ratatui::buffer::Buffer, rows: &[String]) -> Option<String> {
        (0..buffer.area.height)
            .find(|&y| {
                (0..buffer.area.width).any(|x| {
                    buffer[(x, y)]
                        .style()
                        .add_modifier
                        .contains(Modifier::REVERSED)
                        && buffer[(x, y)].symbol() != " "
                })
            })
            .map(|y| rows[usize::from(y)].clone())
    }

    #[test]
    fn at_80x24_every_row_fits_with_its_branch_and_path() {
        let state = many(5);
        let (buffer, rows) = draw_sized(&state, 80, 24);
        for i in 1..=5 {
            let (_, row) = line_with(&rows, &format!("/r/wt-{i}"));
            assert!(row.contains(&format!("b{i}  /r/wt-{i}")), "{row}");
        }
        line_with(&rows, " Worktrees ");
        assert!(!rows.iter().any(|r| r.contains("6/6")), "nothing is cut");
        let cursor = reversed_row(&buffer, &rows).expect("a cursor row");
        assert!(cursor.contains("/r/wt-5"), "{cursor}");
    }

    #[test]
    fn a_narrow_screen_keeps_the_tags_and_marks_what_it_cut() {
        let out = format!(
            "worktree /r/some/deeply/nested/place/wt\0HEAD {OID}\0\
             branch refs/heads/a-rather-long-branch-name\0locked\0prunable\0\0"
        );
        let state = state_with(out.as_bytes(), "/r");
        let (_, rows) = draw_sized(&state, 50, 10);
        let (_, row) = line_with(&rows, "locked missing");
        assert!(row.contains('…'), "{row}");
        assert!(row.contains("/wt"), "the path's end stays: {row}");
        assert!(row.contains("a-rather"), "the branch's start stays: {row}");
    }

    #[test]
    fn a_short_screen_scrolls_to_the_cursor_and_counts_in_the_title() {
        let state = many(11);
        let (buffer, rows) = draw_sized(&state, 40, 8);
        let cursor = reversed_row(&buffer, &rows).expect("the cursor row is in view");
        assert!(cursor.contains("/r/wt-11"), "{cursor}");
        line_with(&rows, "12/12");
        assert!(!rows.iter().any(|r| r.contains("/r/main")), "{rows:#?}");
    }

    #[test]
    fn tiny_screens_draw_without_panicking() {
        let state = many(11);
        for (width, height) in [(20, 5), (3, 3), (1, 1)] {
            draw_sized(&state, width, height);
        }
        let (buffer, rows) = draw_sized(&state, 20, 5);
        assert!(reversed_row(&buffer, &rows).is_some(), "{rows:#?}");
    }

    /// [`many`] with `filter` set on the list and typed into the open
    /// input, as `/` then the keys leave it.
    fn filtered(n: usize, filter: &str) -> AppState {
        let mut state = many(n);
        if let Some(Popup::Worktrees(list)) = state.popups.last_mut() {
            list.list.set_filter(filter.into());
        }
        state.input = Some(ListInput {
            target: InputTarget::WorktreesFilter,
            line: LineInput::with_text(filter),
        });
        state
    }

    #[test]
    fn a_filter_draws_only_its_rows_and_says_so_safely_in_the_title() {
        // A control char never matches a row, so the bar holds the hostile
        // text while the list is narrowed by the part that can match.
        let mut state = filtered(5, "wt-\x1b[2J\x07");
        if let Some(Popup::Worktrees(list)) = state.popups.last_mut() {
            list.list.set_filter("wt-2".into());
        }
        let (buffer, rows) = draw(&state);
        assert!(
            rows.iter()
                .all(|row| !row.contains('\x1b') && !row.contains('\x07')),
            "{rows:#?}"
        );
        let bar = rows.last().unwrap();
        assert!(bar.contains(" /wt-"), "the bar draws the input: {bar}");
        assert!(bar.contains("[2J"), "sanitised: {bar}");
        line_with(&rows, "┌ Worktrees · /wt-2 1 of 6 ┐");
        line_with(&rows, "b2  /r/wt-2");
        for gone in ["/r/main", "/r/wt-1", "/r/wt-5"] {
            assert!(!rows.iter().any(|r| r.contains(gone)), "{gone} drawn");
        }
        let cursor = reversed_row(&buffer, &rows).expect("a cursor row");
        assert!(cursor.contains("/r/wt-2"), "{cursor}");

        // A hostile filter is drawn, never run, in the title.
        if let Some(Popup::Worktrees(list)) = state.popups.last_mut() {
            list.list.set_filter("x\x1b[31m\x07".into());
        }
        let (buffer, rows) = draw(&state);
        assert!(
            rows.iter()
                .all(|row| !row.contains('\x1b') && !row.contains('\x07')),
            "{rows:#?}"
        );
        let (_, title) = line_with(&rows, " Worktrees · /x");
        assert!(title.contains("[31m"), "{title}");
        assert!(title.contains(" 0 of 6 "), "{title}");
        line_with(&rows, " No matches ");
        assert_eq!(reversed_row(&buffer, &rows), None);
    }

    #[test]
    fn without_a_filter_the_title_is_unchanged() {
        let mut state = many(3);
        state.input = None;
        let (_, rows) = draw(&state);
        line_with(&rows, "┌ Worktrees ─");
        assert!(!rows.iter().any(|r| r.contains(" of ")), "{rows:#?}");
    }

    #[test]
    fn a_long_filter_on_a_small_screen_fits_the_title_inside_the_border() {
        let long = "w".repeat(200);
        let state = filtered(11, &long);
        let (_, rows) = draw_sized(&state, 40, 8);
        let (_, title) = line_with(&rows, " Worktrees · /");
        assert!(title.contains('…'), "{title}");
        assert!(title.contains(" 0 of 12 "), "{title}");
        let title = title.trim_end();
        assert!(title.ends_with('┐'), "the right corner stays: {title}");

        // A filter matching more rows than fit counts the cursor's place.
        // Every term must match, so many short ones make it long.
        let state = filtered(11, &format!("wt{}", " w".repeat(100)));
        let (buffer, rows) = draw_sized(&state, 40, 8);
        let (_, title) = line_with(&rows, " Worktrees · /");
        assert!(title.contains(" 11 of 12 · 11/11 "), "{title}");
        assert!(title.trim_end().ends_with('┐'), "{title}");
        let cursor = reversed_row(&buffer, &rows).expect("the cursor row is in view");
        assert!(cursor.contains("/r/wt-11"), "{cursor}");
    }

    #[test]
    fn a_wide_character_path_is_elided_by_display_width() {
        let out = "worktree /r/日本語/とても長いディレクトリの名前/作業ツリー\0bare\0locked\0\0";
        let state = state_with(out.as_bytes(), "/r");
        let (buffer, rows) = draw_sized(&state, 40, 8);
        let (y, row) = line_with(&rows, "locked");
        assert!(row.contains('…'), "{row}");
        assert!(row.contains("ツリー"), "the path's end stays: {row}");
        // The row ends at the popup's right border, inside the screen.
        let right = (0..buffer.area.width)
            .rev()
            .find(|&x| buffer[(x, y as u16)].symbol() == "│")
            .expect("a right border");
        assert!(right < buffer.area.width);
    }
}
