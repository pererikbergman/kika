//! Mouse input: what a click or a wheel notch lands on, and what it does
//! there. Pure: the panels' places come from the layout the view draws
//! with, and each list's rows from the window function its view shares.
//!
//! Nothing happens while anything is open over the workspace (a popup,
//! the commit popup, help or a text input), or in review mode, so a click
//! can never answer a question.

use ratatui::layout::{Position, Rect};

use crate::effect::Effect;
use crate::features::branches::BranchesIntent;
use crate::features::commits::CommitsIntent;
use crate::features::files::FilesIntent;
use crate::features::stash::StashIntent;
use crate::intent::{AppIntent, Scroll};
use crate::layout::{Panel, SidePanel, screen_layout, workspace_layout};
use crate::model::{AppState, Context};
use crate::view::PanelLook;

/// Rows one wheel notch moves a list or scrolls the diff.
const WHEEL_ROWS: isize = 3;

/// The panel under a screen cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MouseTarget {
    /// A side panel. `line` is the 0-based line inside its border, `None`
    /// on the border or title; `rows` is how many lines that inside has.
    Side {
        panel: SidePanel,
        line: Option<u16>,
        rows: u16,
    },
    Diff,
}

/// The panel drawn at (`column`, `row`) of the workspace laid out as the
/// view lays it out now; `None` off every panel, the bar included.
pub(crate) fn target(state: &AppState, column: u16, row: u16) -> Option<MouseTarget> {
    let (width, height) = state.size;
    let [main, _] = screen_layout(Rect::new(0, 0, width, height));
    let layout = workspace_layout(main, state.focus.panel(), state.expanded, state.screen_mode);
    let at = Position::new(column, row);
    for side in SidePanel::ALL {
        let panel = Panel::Side(side);
        let Some(area) = layout.rect(panel).filter(|area| area.contains(at)) else {
            continue;
        };
        // The frame the view draws, so its inside is the view's too.
        let inner = PanelLook::of(state, panel, area)
            .block(String::new())
            .inner(area);
        return Some(MouseTarget::Side {
            panel: side,
            line: inner.contains(at).then(|| row - inner.y),
            rows: inner.height,
        });
    }
    layout
        .rect(Panel::Diff)
        .filter(|area| area.contains(at))
        .map(|_| MouseTarget::Diff)
}

impl AppState {
    /// Whether mouse input is ignored: outside the workspace, or while
    /// anything over the panels takes the keys.
    fn mouse_ignored(&self) -> bool {
        self.context != Context::Workspace || self.key_focus() != self.panel_focus()
    }

    /// The move `delta` rows through `panel`'s list, when the panel lists
    /// rows a click or the wheel can move through.
    fn list_move(&self, panel: SidePanel, delta: isize) -> Option<AppIntent> {
        let intent = match panel {
            SidePanel::Files if self.files_list_shown() => FilesIntent::MoveBy(delta).into(),
            SidePanel::Branches if self.branches_list_shown() => {
                BranchesIntent::MoveBy(delta).into()
            }
            SidePanel::Commits if self.commits_list_shown() => CommitsIntent::MoveBy(delta).into(),
            SidePanel::Stash if self.stash_list_shown() => StashIntent::MoveBy(delta).into(),
            _ => return None,
        };
        Some(intent)
    }

    /// The move from the cursor to the row `panel` draws on `line` of its
    /// `rows` inner lines, if a row of its list is drawn there.
    fn step_to(&self, panel: SidePanel, line: u16, rows: u16) -> Option<isize> {
        let (line, rows) = (usize::from(line), usize::from(rows));
        match panel {
            SidePanel::Files => self.files_step_to(line, rows),
            SidePanel::Branches => self.branches_step_to(line, rows),
            SidePanel::Commits => self.commits_step_to(line, rows),
            SidePanel::Stash => self.stash_step_to(line, rows),
            SidePanel::Status => None,
        }
    }

    /// A left click: select the row under it in the expanded panel's
    /// list, then focus the panel clicked. Rows are found in the layout
    /// from before the click.
    pub(crate) fn click(&mut self, column: u16, row: u16) -> Vec<Effect> {
        if self.mouse_ignored() {
            return vec![];
        }
        match target(self, column, row) {
            Some(MouseTarget::Side { panel, line, rows }) => {
                let step = line.and_then(|line| self.step_to(panel, line, rows));
                let mut effects = match step.and_then(|delta| self.list_move(panel, delta)) {
                    Some(intent) => self.update(intent),
                    None => vec![],
                };
                if self.focus.panel() != Panel::Side(panel) {
                    effects.extend(self.update(AppIntent::FocusPanel(Panel::Side(panel))));
                }
                effects
            }
            // A blame or a conflict view already has the pane.
            Some(MouseTarget::Diff) if self.focus.is_side() => {
                self.update(AppIntent::FocusPanel(Panel::Diff))
            }
            Some(MouseTarget::Diff) | None => vec![],
        }
    }

    /// A wheel notch: scroll the diff, or move the cursor of the expanded
    /// panel's list, under the pointer. Focus stays where it is.
    pub(crate) fn wheel(&mut self, column: u16, row: u16, down: bool) -> Vec<Effect> {
        if self.mouse_ignored() {
            return vec![];
        }
        match target(self, column, row) {
            Some(MouseTarget::Diff) => {
                let by = if down {
                    Scroll::LineDown
                } else {
                    Scroll::LineUp
                };
                (0..WHEEL_ROWS)
                    .flat_map(|_| self.update(AppIntent::ScrollDiff(by)))
                    .collect()
            }
            Some(MouseTarget::Side { panel, .. }) => {
                let delta = if down { WHEEL_ROWS } else { -WHEEL_ROWS };
                match self.list_move(panel, delta) {
                    Some(intent) => self.update(intent),
                    None => vec![],
                }
            }
            None => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    use super::*;
    use crate::confirm_question::Answer;
    use crate::features::stash::StashEntry;
    use crate::layout::ScreenMode;
    use crate::menu::MenuId;
    use crate::model::{CommitFlow, Focus};
    use crate::popup::{ErrorPopup, Popup, PromptKind};
    use crate::test_support::screen::find;
    use crate::test_support::*;
    use crate::view::{buffer_rows, render};
    use kika_git::FileEntry;
    use kika_git::branches::Branch;

    const WIDTH: u16 = 80;
    const HEIGHT: u16 = 24;

    /// The screen drawn at the state's own size, as the hit-test sees it.
    fn drawn(state: &AppState) -> Buffer {
        let (width, height) = state.size;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(state, frame)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn click(state: &mut AppState, (column, row): (u16, u16)) -> Vec<Effect> {
        state.update(AppIntent::Click { column, row })
    }

    fn wheel(state: &mut AppState, (column, row): (u16, u16), down: bool) -> Vec<Effect> {
        state.update(AppIntent::Wheel { column, row, down })
    }

    /// The cell `text` starts at, failing with the screen when it is not
    /// drawn.
    fn at(buffer: &Buffer, text: &str) -> (u16, u16) {
        let rows = buffer_rows(buffer);
        assert!(
            rows.iter().any(|row| row.contains(text)),
            "{text:?} not drawn:\n{}",
            rows.join("\n")
        );
        find(buffer, text)
    }

    /// The panel's place on the screen as the view lays it out now.
    fn rect(state: &AppState, panel: Panel) -> Rect {
        let (width, height) = state.size;
        let [main, _] = screen_layout(Rect::new(0, 0, width, height));
        workspace_layout(main, state.focus.panel(), state.expanded, state.screen_mode)
            .rect(panel)
            .unwrap()
    }

    fn names(prefix: &str, n: usize) -> Vec<String> {
        (0..n).map(|i| format!("{prefix}{i:02}")).collect()
    }

    /// Thirty files, the cursor on `f20` so the window is scrolled, with
    /// the Diff pane focused and Files expanded beside it.
    fn many_files() -> AppState {
        let files: Vec<FileEntry> = names("f", 30)
            .iter()
            .map(|name| changed(name, '.', 'M'))
            .collect();
        let (mut state, _) = with_files(files, HEIGHT);
        state.update(AppIntent::Files(FilesIntent::MoveBy(20)));
        state.focus = Focus::Diff;
        state
    }

    fn selected_path(state: &AppState) -> String {
        state
            .files
            .selected_row()
            .unwrap()
            .path
            .display()
            .to_string()
    }

    #[test]
    fn a_click_on_a_scrolled_files_row_selects_that_file_and_focuses_files() {
        let mut state = many_files();
        let buffer = drawn(&state);
        assert!(!buffer_rows(&buffer).iter().any(|r| r.contains(" M f00")));
        let effects = click(&mut state, at(&buffer, " M f17"));
        assert_eq!(selected_path(&state), "f17");
        assert_eq!(state.focus, Focus::Files);
        // The diff preview follows the click, as it follows a key.
        assert!(
            effects.iter().any(|e| matches!(e, Effect::LoadDiff { .. })),
            "{effects:?}"
        );
    }

    /// Thirty local branches, the cursor on `b20`, with the Diff pane
    /// focused and Branches expanded beside it.
    fn many_branches() -> AppState {
        let mut state = AppState::new(Context::Workspace);
        state.update(AppIntent::Resize {
            width: WIDTH,
            height: HEIGHT,
        });
        let tag = branches_tag(&state.update(AppIntent::RefreshStatus));
        let branches: Vec<Branch> = names("b", 30)
            .iter()
            .map(|name| git_branch(name, name == "b00", false))
            .collect();
        state.update(branches_loaded(tag, branches));
        state.update(AppIntent::Branches(BranchesIntent::MoveBy(20)));
        state.expanded = SidePanel::Branches;
        state.focus = Focus::Diff;
        state
    }

    #[test]
    fn a_click_on_a_scrolled_branch_selects_it_and_focuses_branches() {
        let mut state = many_branches();
        let buffer = drawn(&state);
        let effects = click(&mut state, at(&buffer, "b17"));
        assert_eq!(selected_name(&state), "b17");
        assert_eq!(state.focus, Focus::Branches);
        assert!(log_load(&effects).is_some(), "{effects:?}");
    }

    /// Thirty commits, the cursor on `commit 20`, with the Diff pane
    /// focused and Commits expanded beside it.
    fn many_commits() -> AppState {
        let mut state = with_log(30);
        state.update(AppIntent::Resize {
            width: WIDTH,
            height: HEIGHT,
        });
        state.update(AppIntent::Commits(CommitsIntent::MoveBy(20)));
        assert_eq!(state.commits.list.selected, 20);
        state.focus = Focus::Diff;
        state
    }

    #[test]
    fn a_click_on_a_scrolled_commit_selects_it_and_focuses_commits() {
        let mut state = many_commits();
        let buffer = drawn(&state);
        click(&mut state, at(&buffer, "commit 17"));
        assert_eq!(state.commits.list.selected, 17);
        assert_eq!(state.focus, Focus::Commits);
    }

    fn many_stashes() -> AppState {
        let (mut state, _) = with_files(three_files(), HEIGHT);
        let entries: Vec<StashEntry> = (0..30)
            .map(|i| stash_entry(i, &format!("{:x}", 0xa00 + i), &format!("On main: s{i:02}")))
            .collect();
        load_list(&mut state.stash.list, Ok(entries));
        state.stash.list.move_by(20);
        state.expanded = SidePanel::Stash;
        state.focus = Focus::Diff;
        state
    }

    #[test]
    fn a_click_on_a_scrolled_stash_selects_it_and_focuses_stash() {
        let mut state = many_stashes();
        let buffer = drawn(&state);
        click(&mut state, at(&buffer, "stash@{17}"));
        assert_eq!(state.stash.list.selected, 17);
        assert_eq!(state.focus, Focus::Stash);
    }

    #[test]
    fn a_click_on_a_collapsed_panel_or_a_border_only_focuses_it() {
        let mut state = with_branches();
        state.update(AppIntent::FocusPanel(Panel::Side(SidePanel::Files)));
        let branches = rect(&state, Panel::Side(SidePanel::Branches));
        let before = selected_name(&state);
        // The collapsed panel's one line, then its title.
        click(&mut state, (branches.x + 3, branches.y + 1));
        assert_eq!(state.focus, Focus::Branches);
        assert_eq!(selected_name(&state), before);

        let mut state = many_files();
        let files = rect(&state, Panel::Side(SidePanel::Files));
        click(&mut state, (files.x + 3, files.y));
        assert_eq!(state.focus, Focus::Files);
        assert_eq!(selected_path(&state), "f20");
        state.focus = Focus::Diff;
        click(&mut state, (files.x, files.y + 2));
        assert_eq!(state.focus, Focus::Files);
        assert_eq!(selected_path(&state), "f20");
    }

    #[test]
    fn a_click_on_the_diff_pane_focuses_it_from_a_side_panel() {
        let (mut state, _) = with_files(three_files(), HEIGHT);
        assert_eq!(state.focus, Focus::Files);
        let diff = rect(&state, Panel::Diff);
        click(&mut state, (diff.x + 5, diff.y + 5));
        assert_eq!(state.focus, Focus::Diff);
        // Already there: nothing more.
        assert_eq!(click(&mut state, (diff.x + 5, diff.y + 5)), vec![]);
        assert_eq!(state.focus, Focus::Diff);
    }

    #[test]
    fn a_click_below_the_last_row_or_on_the_cut_note_only_focuses() {
        let (mut state, _) = with_files(three_files(), HEIGHT);
        state.update(AppIntent::Files(FilesIntent::Next));
        state.focus = Focus::Diff;
        let files = rect(&state, Panel::Side(SidePanel::Files));
        click(&mut state, (files.x + 2, files.y + 8));
        assert_eq!(state.focus, Focus::Files);
        assert_eq!(selected_path(&state), "b.rs");

        let mut state = many_files();
        state.update(AppIntent::Files(FilesIntent::MoveBy(-20)));
        state.focus = Focus::Diff;
        let buffer = drawn(&state);
        click(&mut state, at(&buffer, "more"));
        assert_eq!(state.focus, Focus::Files);
        assert_eq!(selected_path(&state), "f00");
    }

    #[test]
    fn in_full_screen_only_the_focused_panel_is_hit() {
        let (mut state, _) = with_files(three_files(), HEIGHT);
        state.screen_mode = ScreenMode::Full;
        // Where the Diff pane is in Normal: the Files panel takes it all.
        assert!(matches!(
            target(&state, 60, 10),
            Some(MouseTarget::Side {
                panel: SidePanel::Files,
                ..
            })
        ));
        click(&mut state, (60, 10));
        assert_eq!(state.focus, Focus::Files);
        let buffer = drawn(&state);
        click(&mut state, at(&buffer, "?? c.txt"));
        assert_eq!(selected_path(&state), "c.txt");
    }

    #[test]
    fn the_wheel_over_the_expanded_list_moves_its_cursor_three_rows_and_keeps_focus() {
        let (mut state, _) = with_files(
            (0..5)
                .map(|i| changed(&format!("f{i}"), '.', 'M'))
                .collect(),
            HEIGHT,
        );
        state.focus = Focus::Diff;
        let files = rect(&state, Panel::Side(SidePanel::Files));
        let inside = (files.x + 2, files.y + 2);
        let effects = wheel(&mut state, inside, true);
        assert_eq!(selected_path(&state), "f3");
        assert!(matches!(effects[0], Effect::LoadDiff { .. }), "{effects:?}");
        wheel(&mut state, inside, true);
        assert_eq!(selected_path(&state), "f4", "clamped at the end");
        wheel(&mut state, inside, false);
        assert_eq!(selected_path(&state), "f1");
        assert_eq!(state.focus, Focus::Diff);

        // A collapsed panel shows no list, so the wheel does nothing there.
        let branches = rect(&state, Panel::Side(SidePanel::Branches));
        let before = state.clone();
        assert_eq!(
            wheel(&mut state, (branches.x + 2, branches.y + 1), true),
            []
        );
        assert_eq!(state, before);
    }

    #[test]
    fn the_wheel_over_the_diff_scrolls_it_three_lines_and_keeps_focus() {
        let (mut state, effects) = with_files(three_files(), HEIGHT);
        state.update(chunk(diff_tag(&effects), 20, true));
        let diff = rect(&state, Panel::Diff);
        let over = (diff.x + 4, diff.y + 4);
        wheel(&mut state, over, true);
        assert_eq!(state.diff.scroll, 3);
        wheel(&mut state, over, true);
        wheel(&mut state, over, false);
        assert_eq!(state.diff.scroll, 3);
        assert_eq!(state.focus, Focus::Files);
    }

    /// The number drawn right after `prefix` in `text`, if any.
    fn number_after(text: &str, prefix: &str) -> Option<usize> {
        let rest = &text[text.find(prefix)? + prefix.len()..];
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    }

    /// At each size, a click on every line inside `panel` selects the row
    /// whose number `drawn_row` reads on that line, and leaves the cursor
    /// alone on a line with none (a cut note, or below the rows). Focus
    /// goes to the panel either way.
    fn every_line_selects_what_is_drawn(
        make: fn() -> AppState,
        panel: SidePanel,
        drawn_row: fn(&str) -> Option<usize>,
        cursor: fn(&AppState) -> usize,
        sizes: &[(u16, u16)],
    ) {
        for &(width, height) in sizes {
            let mut sized = make();
            sized.update(AppIntent::Resize { width, height });
            let area = rect(&sized, Panel::Side(panel));
            let rows = buffer_rows(&drawn(&sized));
            let mut hit = 0;
            for y in area.y..area.bottom() {
                let column = area.x + 1;
                let Some(MouseTarget::Side { line: Some(_), .. }) = target(&sized, column, y)
                else {
                    continue;
                };
                let drawn_line: String = rows[usize::from(y)]
                    .chars()
                    .skip(usize::from(area.x) + 1)
                    .take(usize::from(area.width.saturating_sub(2)))
                    .collect();
                let mut state = sized.clone();
                let before = cursor(&state);
                click(&mut state, (column, y));
                let why = format!("{width}x{height}, line {y}: {drawn_line:?}");
                match drawn_row(&drawn_line) {
                    Some(n) => {
                        assert_eq!(cursor(&state), n, "{why}");
                        hit += 1;
                    }
                    None => assert_eq!(cursor(&state), before, "{why}"),
                }
                assert_eq!(state.focus.panel(), Panel::Side(panel), "{why}");
            }
            assert!(
                hit > 0,
                "no row drawn at {width}x{height}:\n{}",
                rows.join("\n")
            );
        }
    }

    const SIZES: [(u16, u16); 3] = [(60, 14), (80, 24), (160, 50)];

    fn file_row(line: &str) -> Option<usize> {
        number_after(line, " M f")
    }

    fn file_number(state: &AppState) -> usize {
        number_after(&selected_path(state), "f").unwrap()
    }

    #[test]
    fn every_files_line_selects_the_file_drawn_on_it_at_any_size() {
        every_line_selects_what_is_drawn(
            many_files,
            SidePanel::Files,
            file_row,
            file_number,
            &SIZES,
        );
    }

    #[test]
    fn a_files_panel_one_or_two_lines_tall_still_selects_what_it_draws() {
        let tiny: Vec<(u16, u16)> = (4..=9).map(|height| (60, height)).collect();
        // The sizes reach the one- and two-line windows.
        let rows_at = |height| {
            let mut state = many_files();
            state.update(AppIntent::Resize { width: 60, height });
            let area = rect(&state, Panel::Side(SidePanel::Files));
            match target(&state, area.x + 1, area.y) {
                Some(MouseTarget::Side { rows, .. }) => rows,
                other => panic!("{other:?}"),
            }
        };
        let heights: Vec<u16> = tiny.iter().map(|&(_, h)| rows_at(h)).collect();
        assert!(heights.contains(&1) && heights.contains(&2), "{heights:?}");
        every_line_selects_what_is_drawn(
            many_files,
            SidePanel::Files,
            file_row,
            file_number,
            &tiny,
        );
    }

    #[test]
    fn every_branches_line_selects_the_branch_drawn_on_it_at_any_size() {
        let cursor = |state: &AppState| number_after(&selected_name(state), "b").unwrap();
        every_line_selects_what_is_drawn(
            many_branches,
            SidePanel::Branches,
            |line| number_after(line, "b"),
            cursor,
            &SIZES,
        );
    }

    #[test]
    fn every_commits_line_selects_the_commit_drawn_on_it_at_any_size() {
        let cursor = |state: &AppState| state.commits.list.selected;
        every_line_selects_what_is_drawn(
            many_commits,
            SidePanel::Commits,
            // The subject may be cut; the short hash is row `i` in hex.
            |line| {
                let hash = line
                    .split_whitespace()
                    .find(|word| word.len() == 7 && word.chars().all(|c| c.is_ascii_hexdigit()))?;
                usize::from_str_radix(hash, 16).ok()
            },
            cursor,
            &SIZES,
        );
    }

    #[test]
    fn every_stash_line_selects_the_entry_drawn_on_it_at_any_size() {
        let cursor = |state: &AppState| state.stash.list.selected;
        every_line_selects_what_is_drawn(
            many_stashes,
            SidePanel::Stash,
            |line| number_after(line, "stash@{"),
            cursor,
            &SIZES,
        );
    }

    #[test]
    fn clicks_and_the_wheel_on_a_tiny_terminal_never_panic() {
        let builders: [fn() -> AppState; 4] =
            [many_files, many_branches, many_commits, many_stashes];
        for make in builders {
            for (width, height) in [(0, 0), (1, 1), (3, 2), (10, 4), (12, 3)] {
                for focus_side in [true, false] {
                    let mut state = make();
                    state.update(AppIntent::Resize { width, height });
                    if focus_side {
                        state.focus = Focus::side(state.expanded);
                    }
                    for row in 0..=height {
                        for column in 0..=width {
                            click(&mut state, (column, row));
                            wheel(&mut state, (column, row), true);
                            wheel(&mut state, (column, row), false);
                        }
                    }
                }
            }
        }
    }

    /// Every cell of the screen, the popup's own included.
    fn every_cell() -> impl Iterator<Item = (u16, u16)> {
        (0..HEIGHT).flat_map(|row| (0..WIDTH).map(move |column| (column, row)))
    }

    /// Clicks and wheel notches anywhere change nothing and ask for nothing.
    fn assert_mouse_ignored(state: &mut AppState, why: &str) {
        let before = state.clone();
        for cell in every_cell().step_by(7) {
            assert_eq!(click(state, cell), vec![], "{why}: click at {cell:?}");
            for down in [true, false] {
                assert_eq!(wheel(state, cell, down), vec![], "{why}: wheel at {cell:?}");
            }
        }
        assert_eq!(*state, before, "{why}");
    }

    #[test]
    fn a_click_on_a_drawn_confirm_never_answers_it() {
        let (mut state, _) = with_files(three_files(), HEIGHT);
        state.update(AppIntent::Discard);
        assert!(matches!(
            state.popups.last(),
            Some(Popup::Question(q)) if q.answer == Answer::YOnly
        ));
        let buffer = drawn(&state);
        // The question's line, at the popup's centre column.
        let (_, row) = at(&buffer, "Discard all unstaged");
        let centre = (WIDTH / 2, row);
        let before = state.clone();
        assert_eq!(click(&mut state, centre), vec![]);
        assert_eq!(state, before);
        assert_mouse_ignored(&mut state, "question");
    }

    /// Opens something over the workspace.
    type Opener = fn(&mut AppState);

    #[test]
    fn anything_open_over_the_workspace_takes_the_mouse_away() {
        let open: [(&str, Opener); 6] = [
            ("menu", |s| {
                s.update(AppIntent::OpenMenu(MenuId::Actions));
            }),
            ("prompt", |s| {
                s.update(AppIntent::OpenPrompt(PromptKind::GoToFile));
            }),
            ("error", |s| s.push_error(ErrorPopup::new("t", "m", "n"))),
            ("commit popup", |s| s.commit = CommitFlow::ConfirmStageAll),
            ("help", |s| {
                s.update(AppIntent::OpenHelp);
            }),
            ("filter", |s| {
                s.focus = Focus::Files;
                s.update(AppIntent::OpenFilter);
            }),
        ];
        for (why, open) in open {
            let (mut state, _) = with_files(three_files(), HEIGHT);
            state.focus = Focus::Diff;
            open(&mut state);
            assert_ne!(state.key_focus(), state.panel_focus(), "{why} is open");
            assert_mouse_ignored(&mut state, why);
        }
    }

    #[test]
    fn review_mode_ignores_the_mouse() {
        let mut state = AppState::new(Context::Review { number: Some(3) });
        state.update(AppIntent::Resize {
            width: WIDTH,
            height: HEIGHT,
        });
        assert_mouse_ignored(&mut state, "review");
    }

    #[test]
    fn the_bar_and_cells_off_screen_hit_nothing() {
        let (state, _) = with_files(three_files(), HEIGHT);
        assert_eq!(target(&state, 3, HEIGHT - 1), None);
        assert_eq!(target(&state, WIDTH, 2), None);
        assert_eq!(
            target(&state, 1, 0),
            Some(MouseTarget::Side {
                panel: SidePanel::Status,
                line: None,
                rows: 1
            })
        );
    }
}
