//! Mouse input: what a click or a wheel notch lands on, and what it does
//! there. Pure: every place comes from the layout the view draws with,
//! and each list's rows from the window function its view shares.
//!
//! A click only ever moves a cursor or focus. In a popup's list it moves
//! the cursor and never runs, picks or opens anything; any other popup,
//! a question above all, the commit popup and a text input take no mouse
//! input at all, so a click can never answer a question.

use ratatui::layout::{Position, Rect};
use ratatui::widgets::Block;

use crate::effect::Effect;
use crate::features::branches::BranchesIntent;
use crate::features::branches::remotes::RemotesIntent;
use crate::features::branches::tags::TagsIntent;
use crate::features::command_log::CommandLogIntent;
use crate::features::commits::CommitsIntent;
use crate::features::commits::reflog::ReflogIntent;
use crate::features::files::FilesIntent;
use crate::features::pull_requests::PullRequestsIntent;
use crate::features::repos::ReposIntent;
use crate::features::review::view::{detail_area, files_screen_areas};
use crate::features::review::{ReviewIntent, ReviewScreen};
use crate::features::stash::StashIntent;
use crate::features::submodules::SubmodulesIntent;
use crate::features::worktrees::WorktreesIntent;
use crate::intent::{AppIntent, Scroll};
use crate::keymap::KeyScope;
use crate::layout::{Panel, SidePanel, screen_layout, workspace_layout};
use crate::model::{AppState, Context};
use crate::popup::Popup;
use crate::popup_view::top_list_box;
use crate::view::{PanelLook, help_box};

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
    let main = main_area(state);
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

/// Where everything above the bar is drawn, popups and help included.
fn main_area(state: &AppState) -> Rect {
    let (width, height) = state.size;
    let [main, _] = screen_layout(Rect::new(0, 0, width, height));
    main
}

/// The line of `area`'s inside, inside its border, that `at` is on.
fn line_inside(area: Rect, at: Position) -> Option<(usize, u16)> {
    let inner = Block::bordered().inner(area);
    inner
        .contains(at)
        .then(|| (usize::from(at.y - inner.y), inner.height))
}

/// The intents one wheel notch sends: `down` or up, a notch's rows.
fn notch(down: bool) -> isize {
    if down { WHEEL_ROWS } else { -WHEEL_ROWS }
}

/// `intent`, a notch's rows of times.
fn repeated(intent: AppIntent) -> impl Iterator<Item = AppIntent> {
    std::iter::repeat_n(intent, WHEEL_ROWS.unsigned_abs())
}

impl AppState {
    /// Whether the workspace's panels or review mode's screen have the
    /// mouse: nothing is open over them that takes the keys.
    fn panels_have_the_mouse(&self) -> bool {
        self.popups.is_empty() && !self.help_open && self.key_focus() == self.panel_focus()
    }

    /// The move of the top popup's cursor by `delta` rows, when that
    /// popup is a list and has the keys; not while its filter is typed.
    fn popup_move(&self, delta: isize) -> Option<AppIntent> {
        let (scope, intent): (KeyScope, AppIntent) = match self.popups.last()? {
            Popup::Menu { .. } => (KeyScope::Menu, AppIntent::MenuMoveBy(delta)),
            Popup::PickList(_) => (
                KeyScope::PickList,
                PullRequestsIntent::PickMoveBy(delta).into(),
            ),
            Popup::Submodules(_) => (KeyScope::Submodules, SubmodulesIntent::MoveBy(delta).into()),
            Popup::Worktrees(_) => (KeyScope::Worktrees, WorktreesIntent::MoveBy(delta).into()),
            Popup::Recent(_) => (KeyScope::Recent, ReposIntent::MoveBy(delta).into()),
            Popup::CommandLog(_) => (KeyScope::CommandLog, CommandLogIntent::MoveBy(delta).into()),
            _ => return None,
        };
        (self.key_focus() == scope).then_some(intent)
    }

    /// A click in the top popup: its cursor moves to the row drawn there.
    /// Nothing else, anywhere else, or in any other popup.
    fn popup_click(&mut self, at: Position) -> Vec<Effect> {
        if self.popup_move(0).is_none() {
            return vec![];
        }
        let step = top_list_box(self, main_area(self)).and_then(|list| {
            let row = list.row_at(at)?;
            Some(row as isize - list.cursor? as isize)
        });
        match step
            .filter(|&delta| delta != 0)
            .and_then(|d| self.popup_move(d))
        {
            Some(intent) => self.update(intent),
            None => vec![],
        }
    }

    /// A wheel notch in the top popup moves its cursor; outside it, or
    /// over any other popup, nothing happens.
    fn popup_wheel(&mut self, at: Position, down: bool) -> Vec<Effect> {
        let inside = top_list_box(self, main_area(self)).is_some_and(|list| list.rect.contains(at));
        match self.popup_move(notch(down)).filter(|_| inside) {
            Some(intent) => self.update(intent),
            None => vec![],
        }
    }

    /// The move `delta` rows through `panel`'s list, when the panel lists
    /// rows a click or the wheel can move through. Which list that is
    /// follows the tab and the opened entry, as the keys' scope does.
    fn list_move(&self, panel: SidePanel, delta: isize) -> Option<AppIntent> {
        let intent = match panel {
            SidePanel::Files if self.files_list_shown() => FilesIntent::MoveBy(delta).into(),
            SidePanel::Branches if self.branches_list_shown() => {
                BranchesIntent::MoveBy(delta).into()
            }
            SidePanel::Branches if self.remotes_list_shown() => {
                BranchesIntent::from(RemotesIntent::MoveBy(delta)).into()
            }
            SidePanel::Branches if self.tags_list_shown() => {
                BranchesIntent::from(TagsIntent::MoveBy(delta)).into()
            }
            SidePanel::Commits if self.commits_list_shown() => CommitsIntent::MoveBy(delta).into(),
            SidePanel::Commits if self.reflog_list_shown() => {
                CommitsIntent::from(ReflogIntent::MoveBy(delta)).into()
            }
            SidePanel::Commits if self.commit_files_shown() => {
                CommitsIntent::FileMoveBy(delta).into()
            }
            SidePanel::Stash if self.stash_list_shown() => StashIntent::MoveBy(delta).into(),
            SidePanel::Stash if self.stash_files_shown() => StashIntent::FileMoveBy(delta).into(),
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
            SidePanel::Branches => self
                .branches_step_to(line, rows)
                .or_else(|| self.remotes_step_to(line, rows))
                .or_else(|| self.tags_step_to(line, rows)),
            SidePanel::Commits => self
                .commits_step_to(line, rows)
                .or_else(|| self.reflog_step_to(line, rows))
                .or_else(|| self.commit_files_step_to(line, rows)),
            SidePanel::Stash => self
                .stash_step_to(line, rows)
                .or_else(|| self.stash_files_step_to(line, rows)),
            SidePanel::Status => None,
        }
    }

    /// A left click. In the top popup's list it moves the cursor there.
    /// In the workspace it selects the row under it in the expanded
    /// panel's list, then focuses the panel clicked; in review mode it
    /// selects a queue or file row, or gives the files screen's other
    /// pane the keys. Help, questions and anything else open take none.
    pub(crate) fn click(&mut self, column: u16, row: u16) -> Vec<Effect> {
        let at = Position::new(column, row);
        if self.help_open {
            return vec![];
        }
        if !self.popups.is_empty() {
            return self.popup_click(at);
        }
        if !self.panels_have_the_mouse() {
            return vec![];
        }
        match self.context {
            Context::Workspace => self.workspace_click(column, row),
            Context::Review { .. } => self.review_click(at),
        }
    }

    /// A click on the workspace. Rows are found in the layout from before
    /// the click.
    fn workspace_click(&mut self, column: u16, row: u16) -> Vec<Effect> {
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

    /// A click in review mode: a queue row, or on the files screen a file
    /// row or the pane that does not have the keys. Rows are found in the
    /// layout from before the click.
    fn review_click(&mut self, at: Position) -> Vec<Effect> {
        let scope = self.panel_focus();
        let main = main_area(self);
        match self.review.screen {
            ReviewScreen::Queue if scope == KeyScope::Review => {
                let inner = detail_area(main);
                let step = inner
                    .contains(at)
                    .then(|| self.review_queue_step_to(usize::from(at.y - inner.y), inner.height));
                self.review_move_by(step.flatten())
            }
            ReviewScreen::Files(_)
                if matches!(scope, KeyScope::ReviewFiles | KeyScope::ReviewDiff) =>
            {
                let [list, diff] = files_screen_areas(main);
                if list.contains(at) {
                    let step = line_inside(list, at)
                        .and_then(|(line, rows)| self.review_files_step_to(line, rows));
                    let mut effects = self.review_move_by(step);
                    if scope == KeyScope::ReviewDiff {
                        effects.extend(self.update(ReviewIntent::ToggleFilesFocus.into()));
                    }
                    effects
                } else if diff.contains(at) && scope == KeyScope::ReviewFiles {
                    self.update(ReviewIntent::ToggleFilesFocus.into())
                } else {
                    vec![]
                }
            }
            _ => vec![],
        }
    }

    /// Review mode's cursor moved by `step`, when there is a move.
    fn review_move_by(&mut self, step: Option<isize>) -> Vec<Effect> {
        match step.filter(|&delta| delta != 0) {
            Some(delta) => self.update(ReviewIntent::MoveBy(delta).into()),
            None => vec![],
        }
    }

    /// A wheel notch: scroll help, or move the top popup's cursor, under
    /// the pointer; in the workspace scroll the diff or move the expanded
    /// panel's list; in review mode move the queue or the file list, or
    /// scroll the diff, the detail or the conversation. Focus stays where
    /// it is.
    pub(crate) fn wheel(&mut self, column: u16, row: u16, down: bool) -> Vec<Effect> {
        let at = Position::new(column, row);
        if self.help_open {
            return self.help_wheel(at, down);
        }
        if !self.popups.is_empty() {
            return self.popup_wheel(at, down);
        }
        if !self.panels_have_the_mouse() {
            return vec![];
        }
        match self.context {
            Context::Workspace => self.workspace_wheel(column, row, down),
            Context::Review { .. } => self.review_wheel(at, down),
        }
    }

    /// Help scrolls under the wheel while it has the keys.
    fn help_wheel(&mut self, at: Position, down: bool) -> Vec<Effect> {
        let over =
            self.key_focus() == KeyScope::Help && help_box(self, main_area(self)).contains(at);
        if !over {
            return vec![];
        }
        let by = if down {
            Scroll::LineDown
        } else {
            Scroll::LineUp
        };
        self.run_all(repeated(AppIntent::ScrollHelp(by)))
    }

    fn workspace_wheel(&mut self, column: u16, row: u16, down: bool) -> Vec<Effect> {
        match target(self, column, row) {
            Some(MouseTarget::Diff) => self.scroll_diff_notch(down),
            Some(MouseTarget::Side { panel, .. }) => match self.list_move(panel, notch(down)) {
                Some(intent) => self.update(intent),
                None => vec![],
            },
            None => vec![],
        }
    }

    fn review_wheel(&mut self, at: Position, down: bool) -> Vec<Effect> {
        let scope = self.panel_focus();
        let main = main_area(self);
        match self.review.screen {
            ReviewScreen::Files(_)
                if matches!(scope, KeyScope::ReviewFiles | KeyScope::ReviewDiff) =>
            {
                let [list, diff] = files_screen_areas(main);
                if list.contains(at) {
                    self.update(ReviewIntent::MoveBy(notch(down)).into())
                } else if diff.contains(at) {
                    self.scroll_diff_notch(down)
                } else {
                    vec![]
                }
            }
            ReviewScreen::Files(_) => vec![],
            ReviewScreen::Queue if scope == KeyScope::Review && detail_area(main).contains(at) => {
                self.update(ReviewIntent::MoveBy(notch(down)).into())
            }
            ReviewScreen::Detail(_) | ReviewScreen::Conversation(_)
                if scope == KeyScope::Review && detail_area(main).contains(at) =>
            {
                let step = if down {
                    ReviewIntent::Next
                } else {
                    ReviewIntent::Prev
                };
                self.run_all(repeated(step.into()))
            }
            _ => vec![],
        }
    }

    /// The diff scrolled a notch, as `j`/`k` scroll it with the diff
    /// focused.
    fn scroll_diff_notch(&mut self, down: bool) -> Vec<Effect> {
        let by = if down {
            Scroll::LineDown
        } else {
            Scroll::LineUp
        };
        self.run_all(repeated(AppIntent::ScrollDiff(by)))
    }

    fn run_all(&mut self, intents: impl Iterator<Item = AppIntent>) -> Vec<Effect> {
        intents.flat_map(|intent| self.update(intent)).collect()
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    use super::*;
    use crate::confirm_question::Answer;
    use crate::confirm_question::quit_question;
    use crate::features::branches::BranchesTab;
    use crate::features::commits::CommitsTab;
    use crate::features::commits::reflog::Reflog;
    use crate::features::commits::reflog::git::parse_records;
    use crate::features::review::fixtures as review_fixtures;
    use crate::features::stash::StashEntry;
    use crate::features::stash::StashFile;
    use crate::features::worktrees::Worktree;
    use crate::layout::ScreenMode;
    use crate::menu::MenuId;
    use crate::model::{CommitFlow, Focus};
    use crate::pick_list::{PickList, PickPurpose};
    use crate::popup::{CredentialPrompt, Echo, SecretInput};
    use crate::popup::{ErrorPopup, Popup, PromptKind};
    use crate::test_support::screen::find;
    use crate::test_support::*;
    use crate::view::{buffer_rows, render};
    use crossterm::event::KeyCode;
    use kika_forge::pending::PendingReview;
    use kika_git::FileEntry;
    use kika_git::branches::Branch;
    use kika_git::commit_files::CommitFile;
    use kika_git::submodule::SubmoduleState;
    use std::path::PathBuf;

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

        let open: [(&str, Opener); 9] = [
            ("prompt", |s| {
                s.update(AppIntent::OpenPrompt(PromptKind::GoToFile));
            }),
            ("error", |s| s.push_error(ErrorPopup::new("t", "m", "n"))),
            ("password", |s| s.popups.push(credential(false))),
            ("ssh yes/no", |s| s.popups.push(credential(true))),
            ("commit flow", |s| s.commit = CommitFlow::ConfirmStageAll),
            ("worktrees filter", |s| {
                *s = many_worktrees(WIDTH, HEIGHT);
                press_char(s, '/');
                type_text(s, "w0");
            }),
            ("command log filter", |s| {
                *s = many_commands(WIDTH, HEIGHT);
                press_char(s, '/');
                type_text(s, "cmd1");
            }),
            ("question over its menu", |s| {
                s.update(AppIntent::OpenMenu(MenuId::Actions));
                s.popups.push(Popup::Question(quit_question()));
            }),
            ("discard question over a menu", |s| {
                s.update(AppIntent::OpenMenu(MenuId::Actions));
                s.update(AppIntent::Discard);
            }),
        ];
        for (why, open) in open {
            let (mut state, _) = with_files(three_files(), HEIGHT);
            open(&mut state);
            assert_ne!(state.key_focus(), state.panel_focus(), "{why} is open");
            if why.ends_with("filter") {
                assert_eq!(state.key_focus(), KeyScope::Input, "{why}");
            }
            assert_mouse_ignored(&mut state, why);
        }
    }

    /// Opens something over the workspace.
    type Opener = fn(&mut AppState);

    #[test]
    fn anything_open_over_the_workspace_takes_the_mouse_away() {
        let open: [(&str, Opener); 4] = [
            ("prompt", |s| {
                s.update(AppIntent::OpenPrompt(PromptKind::GoToFile));
            }),
            ("error", |s| s.push_error(ErrorPopup::new("t", "m", "n"))),
            ("commit popup", |s| s.commit = CommitFlow::ConfirmStageAll),
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

    // --- Popup lists ---

    /// A credential prompt: ssh's yes/no kind when `confirm`.
    fn credential(confirm: bool) -> Popup {
        Popup::Credential(CredentialPrompt {
            prompt: "Password for 'https://x':".into(),
            echo: Echo::Masked,
            confirm,
            input: SecretInput::default(),
        })
    }

    /// The workspace at `width` by `height`, three files listed.
    fn sized_files(width: u16, height: u16) -> AppState {
        let (mut state, _) = with_files(three_files(), HEIGHT);
        state.update(AppIntent::Resize { width, height });
        state
    }

    /// The Actions menu with its cursor on the last item, so a short
    /// screen scrolls it.
    fn actions_menu(width: u16, height: u16) -> AppState {
        let mut state = sized_files(width, height);
        state.update(AppIntent::OpenMenu(MenuId::Actions));
        state.update(AppIntent::MenuMoveBy(99));
        state
    }

    fn menu_cursor(state: &AppState) -> usize {
        match state.popups.last() {
            Some(Popup::Menu { cursor, .. }) => *cursor,
            other => panic!("expected a menu, got {other:?}"),
        }
    }

    /// The Actions item whose label starts `line`, the longest that does.
    fn menu_row(line: &str) -> Option<usize> {
        let text = line.trim_start();
        crate::menu::items(MenuId::Actions)
            .iter()
            .enumerate()
            .filter(|(_, item)| !item.label.is_empty() && text.starts_with(item.label))
            .max_by_key(|(_, item)| item.label.len())
            .map(|(i, _)| i)
    }

    fn many_picks(width: u16, height: u16) -> AppState {
        let mut state = sized_files(width, height);
        let rows = names("row", 30);
        state.popups.push(Popup::PickList(PickList::new(
            PickPurpose::TargetRemote,
            rows,
            20,
        )));
        state
    }

    fn pick_cursor(state: &AppState) -> usize {
        match state.popups.last() {
            Some(Popup::PickList(list)) => list.cursor,
            other => panic!("expected a pick list, got {other:?}"),
        }
    }

    fn many_submodules(width: u16, height: u16) -> AppState {
        let subs = names("sub", 30)
            .iter()
            .map(|path| submodule(path, SubmoduleState::UpToDate))
            .collect();
        let mut state = with_submodules(subs);
        state.update(AppIntent::Resize { width, height });
        state.update(SubmodulesIntent::MoveBy(20).into());
        state
    }

    fn submodule_cursor(state: &AppState) -> usize {
        match state.popups.last() {
            Some(Popup::Submodules(list)) => list.list.selected,
            other => panic!("expected the submodules, got {other:?}"),
        }
    }

    fn worktree_at(path: &str) -> Worktree {
        Worktree {
            path: kika_git::path::RepoPath::from_bytes(path.as_bytes()),
            head: None,
            branch: None,
            detached: true,
            bare: false,
            locked: false,
            prunable: false,
            main: path.ends_with("w00"),
            unsure: false,
        }
    }

    fn many_worktrees(width: u16, height: u16) -> AppState {
        let mut state = with_branches();
        state.update(AppIntent::Resize { width, height });
        state.repo = Some("/r/a".into());
        let effects = press_char(&mut state, 'W');
        let [Effect::LoadWorktrees { tag }] = effects[..] else {
            panic!("expected one LoadWorktrees, got {effects:?}");
        };
        let items = names("/r/w", 30).iter().map(|p| worktree_at(p)).collect();
        state.update(
            WorktreesIntent::Loaded {
                tag,
                result: Ok(items),
            }
            .into(),
        );
        state.update(WorktreesIntent::MoveBy(20).into());
        state
    }

    fn worktree_cursor(state: &AppState) -> usize {
        state.worktree_list().unwrap().list.selected
    }

    /// The Recent popup over thirty repositories `/r/pNN`, the cursor on
    /// the twentieth row; `outside` started outside a repository, so the
    /// popup says so above an "Initialise" row.
    fn many_recent(width: u16, height: u16, outside_repo: bool) -> AppState {
        let mut state = if outside_repo {
            let mut state = outside("/w");
            state.update(AppIntent::Start);
            state
        } else {
            let mut state =
                sized_files(width, height).with_location(Some("/r/a".into()), "/r/a".into());
            state.update(ReposIntent::OpenRecent.into());
            state
        };
        state.update(AppIntent::Resize { width, height });
        let seq = state.recent_seq;
        let list = names("/r/p", 30).iter().map(PathBuf::from).collect();
        state.update(ReposIntent::Loaded { seq, list }.into());
        state.update(ReposIntent::MoveBy(20).into());
        assert!(matches!(state.popups.last(), Some(Popup::Recent(_))));
        state
    }

    fn recent_cursor(state: &AppState) -> usize {
        match state.popups.last() {
            Some(Popup::Recent(list)) => list.cursor,
            other => panic!("expected the recent list, got {other:?}"),
        }
    }

    fn many_commands(width: u16, height: u16) -> AppState {
        let mut state = sized_files(width, height);
        for id in 0..30 {
            state.update(
                CommandLogIntent::Logged(kika_git::recording::CommandEvent::Started {
                    id,
                    line: format!("git cmd{id:02}"),
                    attached: false,
                })
                .into(),
            );
        }
        state.update(CommandLogIntent::Open.into());
        state.update(CommandLogIntent::MoveBy(-5).into());
        state
    }

    fn command_cursor(state: &AppState) -> usize {
        match state.popups.last() {
            Some(Popup::CommandLog(view)) => view.cursor,
            other => panic!("expected the command log, got {other:?}"),
        }
    }

    /// One list popup to test: how to open it scrolled at a size, which
    /// row a drawn line shows, and where its cursor is.
    struct PopupCase {
        name: &'static str,
        make: fn(u16, u16) -> AppState,
        row: fn(&str) -> Option<usize>,
        cursor: fn(&AppState) -> usize,
    }

    fn popup_cases() -> Vec<PopupCase> {
        vec![
            PopupCase {
                name: "menu",
                make: actions_menu,
                row: menu_row,
                cursor: menu_cursor,
            },
            PopupCase {
                name: "pick list",
                make: many_picks,
                row: |line| number_after(line, "row"),
                cursor: pick_cursor,
            },
            PopupCase {
                name: "submodules",
                make: many_submodules,
                row: |line| number_after(line, "sub"),
                cursor: submodule_cursor,
            },
            PopupCase {
                name: "worktrees",
                make: many_worktrees,
                row: |line| number_after(line, "/r/w"),
                cursor: worktree_cursor,
            },
            PopupCase {
                name: "recent, in a repository",
                make: |w, h| many_recent(w, h, false),
                row: |line| number_after(line, "/r/p"),
                cursor: recent_cursor,
            },
            PopupCase {
                name: "recent, outside one",
                make: |w, h| many_recent(w, h, true),
                // The "Initialise" row comes first.
                row: |line| {
                    if line.contains("Initialise a repository here") {
                        Some(0)
                    } else {
                        number_after(line, "/r/p").map(|n| n + 1)
                    }
                },
                cursor: recent_cursor,
            },
            PopupCase {
                name: "command log",
                make: many_commands,
                row: |line| number_after(line, "git cmd"),
                cursor: command_cursor,
            },
        ]
    }

    /// The top popup's kind and the stack's depth: what a click must
    /// never change.
    fn stack_shape(state: &AppState) -> (usize, std::mem::Discriminant<Popup>) {
        (
            state.popups.len(),
            std::mem::discriminant(state.popups.last().unwrap()),
        )
    }

    fn top_box(state: &AppState) -> crate::popup_view::ListBox {
        top_list_box(state, main_area(state)).expect("a list popup on top")
    }

    #[test]
    fn every_line_of_a_list_popup_selects_the_row_drawn_on_it_and_runs_nothing() {
        for case in popup_cases() {
            let mut scrolled = false;
            for (width, height) in [(80, 24), (60, 14), (60, 10)] {
                let state = (case.make)(width, height);
                let list = top_box(&state);
                scrolled |= list.first > 0;
                let rows = buffer_rows(&drawn(&state));
                let inner = Block::bordered().inner(list.rect);
                let mut hit = 0;
                for y in list.rect.y..list.rect.bottom() {
                    for column in [list.rect.x, inner.x + 1, list.rect.right() - 1] {
                        let line: String = rows[usize::from(y)]
                            .chars()
                            .skip(usize::from(inner.x))
                            .take(usize::from(inner.width))
                            .collect();
                        let on_border = column < inner.x
                            || column >= inner.right()
                            || y < inner.y
                            || y >= inner.bottom();
                        let why = format!(
                            "{} at {width}x{height}, ({column}, {y}): {line:?}",
                            case.name
                        );
                        let mut clicked = state.clone();
                        let effects = click(&mut clicked, (column, y));
                        assert_eq!(effects, vec![], "{why}");
                        assert_eq!(stack_shape(&clicked), stack_shape(&state), "{why}");
                        match (case.row)(&line).filter(|_| !on_border) {
                            Some(n) => {
                                assert_eq!((case.cursor)(&clicked), n, "{why}");
                                hit += 1;
                            }
                            None => assert_eq!(clicked, state, "{why}"),
                        }
                    }
                }
                assert!(hit > 0, "{}: no row drawn:\n{}", case.name, rows.join("\n"));
            }
            assert!(scrolled, "{} never scrolled", case.name);
        }
    }

    #[test]
    fn the_wheel_inside_a_list_popup_moves_its_cursor_three_rows_clamped() {
        for case in popup_cases() {
            let mut state = (case.make)(WIDTH, HEIGHT);
            let to_top = state.popup_move(-999).unwrap();
            state.update(to_top);
            let list = top_box(&state);
            let inside = (list.rect.x + 1, list.rect.y + 1);
            let start = (case.cursor)(&state);
            assert_eq!(wheel(&mut state, inside, true), vec![], "{}", case.name);
            assert_eq!((case.cursor)(&state), start + 3, "{}", case.name);
            for _ in 0..20 {
                wheel(&mut state, inside, true);
            }
            let end = (case.cursor)(&state);
            wheel(&mut state, inside, true);
            assert_eq!(
                (case.cursor)(&state),
                end,
                "{}: clamped at the end",
                case.name
            );
            wheel(&mut state, inside, false);
            assert_eq!((case.cursor)(&state), end - 3, "{}", case.name);

            // Outside the popup: the bar, and a corner it does not cover.
            let before = state.clone();
            for cell in [(0, HEIGHT - 1), (0, 0), (WIDTH - 1, HEIGHT - 2)] {
                if list.rect.contains(Position::new(cell.0, cell.1)) {
                    continue;
                }
                assert_eq!(wheel(&mut state, cell, true), vec![], "{}", case.name);
                assert_eq!(click(&mut state, cell), vec![], "{}", case.name);
            }
            assert_eq!(wheel(&mut state, (0, HEIGHT - 1), false), vec![]);
            assert_eq!(state, before, "{}", case.name);
        }
    }

    #[test]
    fn a_popup_below_the_top_one_takes_no_clicks_or_wheel() {
        let mut state = actions_menu(WIDTH, HEIGHT);
        state.update(AppIntent::MenuMoveBy(-99));
        let below = top_box(&state);
        state.popups.push(Popup::PickList(PickList::new(
            PickPurpose::TargetRemote,
            vec!["a".into(), "b".into()],
            0,
        )));
        let top = top_box(&state);
        let before = state.clone();
        let mut tried = 0;
        for y in below.rect.y..below.rect.bottom() {
            for x in below.rect.x..below.rect.right() {
                if top.rect.contains(Position::new(x, y)) {
                    continue;
                }
                tried += 1;
                assert_eq!(click(&mut state, (x, y)), vec![]);
                assert_eq!(wheel(&mut state, (x, y), true), vec![]);
            }
        }
        assert!(tried > 0);
        assert_eq!(state, before);
    }

    // --- Help ---

    fn help_on_files() -> AppState {
        let mut state = sized_files(WIDTH, HEIGHT);
        state.mouse = true;
        state.update(AppIntent::OpenHelp);
        state
    }

    #[test]
    fn the_wheel_over_help_scrolls_it_three_lines_clamped() {
        let mut state = help_on_files();
        let area = help_box(&state, main_area(&state));
        let inside = (area.x + 2, area.y + 2);
        assert_eq!(wheel(&mut state, inside, true), vec![]);
        assert_eq!(state.help_scroll, 3);
        let mut end = state.clone();
        for _ in 0..100 {
            end.update(AppIntent::ScrollHelp(Scroll::PageDown));
        }
        for _ in 0..100 {
            wheel(&mut state, inside, true);
        }
        assert_eq!(state.help_scroll, end.help_scroll, "clamped at the end");
        assert!(state.help_scroll > 3);
        wheel(&mut state, inside, false);
        assert_eq!(state.help_scroll, end.help_scroll - 3);
        // At the end the mouse's own line shows.
        for _ in 0..100 {
            wheel(&mut state, inside, true);
        }
        let rows = buffer_rows(&drawn(&state));
        assert!(
            rows.iter()
                .any(|row| row.contains("Mouse: click selects a row")),
            "{rows:#?}"
        );
    }

    #[test]
    fn outside_help_the_wheel_does_nothing_and_no_click_does_anything() {
        let mut state = help_on_files();
        let area = help_box(&state, main_area(&state));
        let before = state.clone();
        for cell in every_cell().step_by(5) {
            assert_eq!(click(&mut state, cell), vec![], "click at {cell:?}");
            if !area.contains(Position::new(cell.0, cell.1)) {
                assert_eq!(wheel(&mut state, cell, true), vec![], "wheel at {cell:?}");
            }
        }
        assert_eq!(state, before);
    }

    // --- Nested lists ---

    /// Three remotes `r0`..`r2`, each with ten branches `bNN`, on the
    /// Remotes tab with the cursor on row 25, the Diff pane focused and
    /// Branches expanded beside it.
    fn many_remotes() -> AppState {
        let mut state = many_branches();
        let remotes = (0..3)
            .map(|i| {
                let branches = names(&format!("{i}-b"), 10);
                let branches: Vec<&str> = branches.iter().map(String::as_str).collect();
                git_remote(&format!("r{i}"), true, &branches)
            })
            .collect();
        load_remotes(&mut state.branches.remotes.list, Ok(remotes));
        state.branches.tab = BranchesTab::Remotes;
        state.branches.remotes.list.move_by(25);
        state
    }

    /// The Remotes row a drawn line shows: `rK` heads eleven rows, its
    /// branches `K-bNN` follow it.
    fn remote_row(line: &str) -> Option<usize> {
        let text = line.trim_start();
        if let Some(name) = text.strip_prefix('r') {
            return number_after(name, "").map(|remote| remote * 11);
        }
        let remote = number_after(text, "")?;
        let branch = number_after(text, "-b")?;
        Some(remote * 11 + 1 + branch)
    }

    fn many_tags() -> AppState {
        let mut state = many_branches();
        let tags = names("tag", 30)
            .iter()
            .map(|name| tag_ref(name, 'a'))
            .collect();
        load_list(&mut state.branches.tags, Ok(tags));
        state.branches.tab = BranchesTab::Tags;
        state.branches.tags.move_by(20);
        state
    }

    /// Thirty reflog entries `entryNN`, newest first, the cursor on the
    /// twentieth, with the Diff pane focused and Commits expanded.
    fn many_reflog() -> AppState {
        let mut state = many_commits();
        let mut bytes = Vec::new();
        for i in 0..30 {
            // The short id, the first seven digits, is the row plus one.
            let oid = |n: usize| format!("{n:07x}{}", "a".repeat(33));
            bytes.extend(format!("{} {} commit: entry{i:02}\0", oid(i + 1), oid(i + 2)).bytes());
        }
        load_reflog_list(
            &mut state.commits.reflog,
            Ok(Reflog {
                entries: parse_records(&bytes),
                ..Reflog::default()
            }),
        );
        state.commits.tab = CommitsTab::Reflog;
        state.commits.reflog.list.move_by(20);
        state.expanded = SidePanel::Commits;
        state.focus = Focus::Diff;
        state
    }

    /// The reflog row a drawn line shows, read from its short id.
    fn reflog_row(line: &str) -> Option<usize> {
        let short = line.split_whitespace().next()?;
        usize::from_str_radix(short, 16).ok()?.checked_sub(1)
    }

    fn reflog_with_an_error() -> AppState {
        let mut state = many_reflog();
        state.commits.reflog.list.error = Some("could not read the reflog".into());
        state
    }

    fn file_names(n: usize) -> Vec<CommitFile> {
        names("file", n)
            .iter()
            .map(|name| commit_file('M', &format!("{name}.rs"), None))
            .collect()
    }

    /// A commit's thirty files `fileNN.rs`, the cursor on the twentieth,
    /// with the Diff pane focused and Commits expanded.
    fn many_commit_files() -> AppState {
        let mut state = on_commit_five();
        opened(&mut state, file_names(30));
        state.update(CommitsIntent::FileMoveBy(20).into());
        state.expanded = SidePanel::Commits;
        state.focus = Focus::Diff;
        state
    }

    /// As [`many_commit_files`], for a compare of two commits.
    fn many_compare_files() -> AppState {
        let mut state = many_commit_files();
        state.commits.open.as_mut().unwrap().against = Some("c9".into());
        state
    }

    fn many_stash_files() -> AppState {
        let mut state = on_stash_one();
        let (tag, _) = stash_files_load(&press(&mut state, KeyCode::Enter));
        let files = file_names(30)
            .into_iter()
            .map(|file| StashFile {
                range: range_of("a1", "ba5e"),
                file,
            })
            .collect();
        state.update(
            StashIntent::FilesLoaded {
                tag,
                result: Ok(files),
            }
            .into(),
        );
        state.update(StashIntent::FileMoveBy(20).into());
        state.expanded = SidePanel::Stash;
        state.focus = Focus::Diff;
        state
    }

    /// One nested list to test.
    struct NestedCase {
        name: &'static str,
        make: fn() -> AppState,
        panel: SidePanel,
        row: fn(&str) -> Option<usize>,
        cursor: fn(&AppState) -> usize,
        /// Text drawn on the row a scrolled click selects, and that row.
        pick: (&'static str, usize),
    }

    fn nested_cases() -> Vec<NestedCase> {
        let file_row = |line: &str| number_after(line, "file");
        let commit_file = |state: &AppState| state.commits.open.as_ref().unwrap().list.selected;
        vec![
            NestedCase {
                name: "remotes",
                make: many_remotes,
                panel: SidePanel::Branches,
                row: remote_row,
                cursor: |state| state.branches.remotes.list.selected,
                pick: ("2-b01", 24),
            },
            NestedCase {
                name: "a remote header",
                make: many_remotes,
                panel: SidePanel::Branches,
                row: remote_row,
                cursor: |state| state.branches.remotes.list.selected,
                pick: ("r2  /srv/r2", 22),
            },
            NestedCase {
                name: "tags",
                make: many_tags,
                panel: SidePanel::Branches,
                row: |line| number_after(line, "tag"),
                cursor: |state| state.branches.tags.selected,
                pick: ("tag17", 17),
            },
            NestedCase {
                name: "reflog",
                make: many_reflog,
                panel: SidePanel::Commits,
                row: reflog_row,
                cursor: |state| state.commits.reflog.list.selected,
                pick: ("entry17", 17),
            },
            NestedCase {
                name: "reflog under an error",
                make: reflog_with_an_error,
                panel: SidePanel::Commits,
                row: reflog_row,
                cursor: |state| state.commits.reflog.list.selected,
                pick: ("entry18", 18),
            },
            NestedCase {
                name: "commit files",
                make: many_commit_files,
                panel: SidePanel::Commits,
                row: file_row,
                cursor: commit_file,
                pick: ("file17.rs", 17),
            },
            NestedCase {
                name: "compare files",
                make: many_compare_files,
                panel: SidePanel::Commits,
                row: file_row,
                cursor: commit_file,
                pick: ("file17.rs", 17),
            },
            NestedCase {
                name: "stash files",
                make: many_stash_files,
                panel: SidePanel::Stash,
                row: file_row,
                cursor: |state| state.stash.open.as_ref().unwrap().list.selected,
                pick: ("file17.rs", 17),
            },
        ]
    }

    #[test]
    fn a_click_on_a_scrolled_nested_row_selects_it_and_focuses_its_panel() {
        for case in nested_cases() {
            let mut state = (case.make)();
            let buffer = drawn(&state);
            let rows = buffer_rows(&buffer);
            let (text, want) = case.pick;
            // Scrolled: the first row is not drawn.
            assert_ne!((case.cursor)(&state), want, "{}", case.name);
            click(&mut state, at(&buffer, text));
            assert_eq!(
                (case.cursor)(&state),
                want,
                "{}:\n{}",
                case.name,
                rows.join("\n")
            );
            assert_eq!(
                state.focus.panel(),
                Panel::Side(case.panel),
                "{}",
                case.name
            );
        }
    }

    #[test]
    fn every_nested_line_selects_the_row_drawn_on_it_at_any_size() {
        for case in nested_cases() {
            every_line_selects_what_is_drawn(case.make, case.panel, case.row, case.cursor, &SIZES);
        }
    }

    #[test]
    fn the_wheel_over_a_nested_list_moves_it_three_rows_and_keeps_focus() {
        for case in nested_cases() {
            let mut state = (case.make)();
            let area = rect(&state, Panel::Side(case.panel));
            let inside = (area.x + 2, area.y + 2);
            let start = (case.cursor)(&state);
            wheel(&mut state, inside, false);
            assert_eq!((case.cursor)(&state), start - 3, "{}", case.name);
            wheel(&mut state, inside, true);
            wheel(&mut state, inside, true);
            assert_eq!((case.cursor)(&state), start + 3, "{}", case.name);
            assert_eq!(state.focus, Focus::Diff, "{}", case.name);
        }
    }

    // --- Review mode ---

    fn review_sized(mut state: AppState) -> AppState {
        state.update(AppIntent::Resize {
            width: WIDTH,
            height: HEIGHT,
        });
        state
    }

    fn queue_number(state: &AppState) -> u64 {
        state.review.rows()[state.review.cursor].number
    }

    #[test]
    fn a_click_on_a_drawn_queue_row_moves_the_cursor_there_and_the_tabs_do_nothing() {
        let mut state = review_sized(review_fixtures::in_queues());
        assert_eq!(queue_number(&state), 12);
        let buffer = drawn(&state);
        let (_, y) = at(&buffer, "#7 ");
        click(&mut state, (10, y));
        assert_eq!(queue_number(&state), 7);
        assert_eq!(state.panel_focus(), KeyScope::Review);

        let inner = detail_area(main_area(&state));
        let before = state.clone();
        for x in inner.x..inner.right() {
            assert_eq!(click(&mut state, (x, inner.y)), vec![], "the tabs line");
        }
        // Below the last row.
        click(&mut state, (10, inner.bottom() - 1));
        assert_eq!(state, before);

        wheel(&mut state, (10, y), false);
        assert_eq!(queue_number(&state), 12);
    }

    fn file_cursor(state: &AppState) -> usize {
        state.review.files.as_ref().unwrap().cursor
    }

    fn many_review_files() -> AppState {
        let state =
            review_fixtures::large_ready(kika_forge::review::fixtures::files_page(100..150, None));
        let mut state = review_sized(state);
        state.update(ReviewIntent::MoveBy(60).into());
        state
    }

    #[test]
    fn a_click_on_a_review_file_selects_it_and_a_click_on_the_other_pane_moves_the_keys() {
        let mut state = many_review_files();
        assert_eq!(state.panel_focus(), KeyScope::ReviewFiles);
        let [list, diff] = files_screen_areas(main_area(&state));
        let inner = Block::bordered().inner(list);
        // The list keeps its cursor on its last line.
        let first = |state: &AppState| file_cursor(state) + 1 - usize::from(inner.height);
        let window = first(&state);
        let effects = click(&mut state, (inner.x + 2, inner.y + 1));
        assert_eq!(file_cursor(&state), window + 1);
        assert!(!effects.is_empty(), "the file's diff loads");
        assert_eq!(state.panel_focus(), KeyScope::ReviewFiles);

        // The diff takes the keys on a click, and gives them back.
        click(&mut state, (diff.x + 5, diff.y + 5));
        assert_eq!(state.panel_focus(), KeyScope::ReviewDiff);
        let window = first(&state);
        click(&mut state, (inner.x + 2, inner.y + 3));
        assert_eq!(file_cursor(&state), window + 3);
        assert_eq!(state.panel_focus(), KeyScope::ReviewFiles);

        // Below the files nothing is selected; the border is no row.
        let mut state = many_review_files();
        let cursor = file_cursor(&state);
        click(&mut state, (list.x, inner.y + 2));
        assert_eq!(file_cursor(&state), cursor);

        // The wheel over the list moves it; focus stays.
        wheel(&mut state, (inner.x + 2, inner.y + 2), true);
        assert_eq!(file_cursor(&state), cursor + 3);
        assert_eq!(state.panel_focus(), KeyScope::ReviewFiles);
    }

    #[test]
    fn the_wheel_over_the_review_diff_scrolls_it_three_lines() {
        let mut state = review_sized(review_fixtures::commenting());
        let [_, diff] = files_screen_areas(main_area(&state));
        let over = (diff.x + 5, diff.y + 3);
        let before = state.diff.scroll;
        click(&mut state, over);
        assert_eq!(state.panel_focus(), KeyScope::ReviewDiff);
        wheel(&mut state, over, true);
        assert_eq!(state.diff.scroll, before + 3);
        wheel(&mut state, over, false);
        assert_eq!(state.diff.scroll, before);
        assert_eq!(state.panel_focus(), KeyScope::ReviewDiff);
    }

    #[test]
    fn the_wheel_on_the_detail_scrolls_it() {
        let mut state = review_fixtures::in_detail(Vec::new());
        state.update(AppIntent::Resize {
            width: 60,
            height: 12,
        });
        let mut longest = state.clone();
        for _ in 0..100 {
            longest.update(ReviewIntent::Next.into());
        }
        assert!(
            longest.review.detail_scroll > 3,
            "the detail is long enough"
        );
        wheel(&mut state, (10, 5), true);
        assert_eq!(state.review.detail_scroll, 3);
        wheel(&mut state, (10, 5), false);
        assert_eq!(state.review.detail_scroll, 0);
        assert_eq!(click(&mut state, (10, 5)), vec![]);
        assert_eq!(state.review.detail_scroll, 0);
    }

    #[test]
    fn with_a_review_overlay_open_the_mouse_changes_nothing() {
        let open: [(&str, Opener, KeyScope); 2] = [
            (
                "pending list",
                |s| {
                    *s = review_sized(review_fixtures::with_review(PendingReview {
                        comments: vec![review_fixtures::comment(
                            1,
                            review_fixtures::right(1),
                            "one",
                        )],
                    }));
                    s.update(ReviewIntent::OpenPendingList.into());
                },
                KeyScope::ReviewPending,
            ),
            (
                "comment pick",
                |s| {
                    s.update(ReviewIntent::ToggleFilesFocus.into());
                    s.update(ReviewIntent::StartComment.into());
                },
                KeyScope::ReviewPick,
            ),
        ];
        for (why, open, scope) in open {
            let mut state = review_sized(review_fixtures::commenting());
            open(&mut state);
            assert_eq!(state.panel_focus(), scope, "{why}");
            assert_mouse_ignored(&mut state, why);
        }
    }

    #[test]
    fn clicks_and_the_wheel_on_a_tiny_terminal_never_panic_in_popups_help_and_review() {
        let mut builders: Vec<Box<dyn Fn() -> AppState>> = Vec::new();
        for case in popup_cases() {
            builders.push(Box::new(move || (case.make)(WIDTH, HEIGHT)));
        }
        for case in nested_cases() {
            builders.push(Box::new(case.make));
        }
        builders.push(Box::new(help_on_files));
        builders.push(Box::new(review_fixtures::in_queues));
        builders.push(Box::new(many_review_files));
        builders.push(Box::new(review_fixtures::commenting));
        for make in builders {
            for (width, height) in [(0, 0), (1, 1), (3, 2), (10, 4), (12, 3)] {
                let mut state = make();
                state.update(AppIntent::Resize { width, height });
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
