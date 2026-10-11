//! Where the workspace panels go. Pure geometry: the view draws into these
//! rectangles and the e2e harness reads them back, so no other code
//! computes a panel's place.

use ratatui::layout::{Constraint, Layout, Rect};

/// The panels of the left column, in screen order, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SidePanel {
    Status,
    Files,
    Branches,
    Commits,
    Stash,
}

impl SidePanel {
    pub const ALL: [SidePanel; 5] = [
        SidePanel::Status,
        SidePanel::Files,
        SidePanel::Branches,
        SidePanel::Commits,
        SidePanel::Stash,
    ];

    pub fn title(self) -> &'static str {
        match self {
            SidePanel::Status => "Status",
            SidePanel::Files => "Files",
            SidePanel::Branches => "Branches",
            SidePanel::Commits => "Commits",
            SidePanel::Stash => "Stash",
        }
    }

    /// The key that focuses it, shown in its title.
    pub fn number(self) -> u8 {
        self.index() as u8 + 1
    }

    /// Its place in the column, from the top.
    pub fn index(self) -> usize {
        match self {
            SidePanel::Status => 0,
            SidePanel::Files => 1,
            SidePanel::Branches => 2,
            SidePanel::Commits => 3,
            SidePanel::Stash => 4,
        }
    }
}

/// Any panel that can have focus: one of the left column, or the diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Panel {
    Side(SidePanel),
    Diff,
}

impl Panel {
    /// The key that focuses it, shown in its title.
    pub fn number(self) -> u8 {
        match self {
            Panel::Side(side) => side.number(),
            Panel::Diff => 0,
        }
    }
}

/// How much of the screen the focused panel takes. Global: it stays as it
/// is while focus moves, so a zoomed view can be walked panel by panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScreenMode {
    #[default]
    Normal,
    Half,
    Full,
}

impl ScreenMode {
    pub fn next(self) -> Self {
        match self {
            ScreenMode::Normal => ScreenMode::Half,
            ScreenMode::Half => ScreenMode::Full,
            ScreenMode::Full => ScreenMode::Normal,
        }
    }

    /// The word the focused title ends with, when not Normal.
    pub fn note(self) -> Option<&'static str> {
        match self {
            ScreenMode::Normal => None,
            ScreenMode::Half => Some("half"),
            ScreenMode::Full => Some("full"),
        }
    }
}

/// Where each panel is drawn; `None` is a panel with no room this frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceLayout {
    side: [Option<Rect>; 5],
    diff: Option<Rect>,
}

impl WorkspaceLayout {
    pub fn rect(&self, panel: Panel) -> Option<Rect> {
        match panel {
            Panel::Side(side) => self.side[side.index()],
            Panel::Diff => self.diff,
        }
    }
}

/// The screen split into the panels above and the one-row bar below.
pub fn screen_layout(area: Rect) -> [Rect; 2] {
    Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area)
}

/// Rows a collapsed panel takes when there is room: its border and one line.
const COLLAPSED: u16 = 3;
/// Rows the expanded panel keeps before the collapsed ones get theirs.
const EXPANDED_MIN: u16 = 5;

/// A left column beside the diff. `focused` decides the split in Half and
/// Full; `expanded` is the side panel that fills the column. In Normal and
/// Half the expanded panel always has a rect, zero rows tall when the
/// column has none. In Full only the focused panel does: with the diff
/// focused, the expanded side panel is `None` too, as the mode asks.
pub fn workspace_layout(
    main: Rect,
    focused: Panel,
    expanded: SidePanel,
    mode: ScreenMode,
) -> WorkspaceLayout {
    let mut layout = WorkspaceLayout {
        side: [None; 5],
        diff: None,
    };
    let left_share = match (mode, focused) {
        (ScreenMode::Full, Panel::Diff) => {
            layout.diff = Some(main);
            return layout;
        }
        (ScreenMode::Full, Panel::Side(side)) => {
            layout.side[side.index()] = Some(main);
            return layout;
        }
        (ScreenMode::Normal, _) => Constraint::Ratio(1, 3),
        (ScreenMode::Half, Panel::Side(_)) => Constraint::Ratio(1, 2),
        (ScreenMode::Half, Panel::Diff) => Constraint::Ratio(1, 4),
    };
    let [left, diff] = Layout::horizontal([left_share, Constraint::Fill(1)]).areas(main);
    layout.diff = Some(diff);
    let heights = left_heights(left.height, expanded);
    let mut y = left.y;
    for (slot, height) in layout.side.iter_mut().zip(heights) {
        if height > 0 {
            *slot = Some(Rect::new(left.x, y, left.width, height));
            y += height;
        }
    }
    // The expanded panel is drawn even with no rows, so it is never `None`.
    if layout.side[expanded.index()].is_none() {
        layout.side[expanded.index()] = Some(Rect::new(left.x, left.y, left.width, 0));
    }
    layout
}

/// Rows for each side panel in a column `height` rows tall; 0 is hidden.
/// The expanded panel takes what the others leave. They collapse to 3 rows
/// (border and a line), then 2 (title and a line), then 1 (the title on the
/// top border), then the ones farthest from the expanded panel go. The
/// expanded one never goes.
pub fn left_heights(height: u16, expanded: SidePanel) -> [u16; 5] {
    let others = SidePanel::ALL.len() as u16 - 1;
    let mut heights = [0; 5];
    // Shrinking a row at a time would be smoother, but a collapsed panel
    // has three useful sizes: border and line, title and line, title.
    let collapsed = (1..=COLLAPSED)
        .rev()
        .find(|&rows| height >= others * rows + EXPANDED_MIN)
        .unwrap_or(1);
    // Nearest first; on a tie the one above stays, so Status outlasts the
    // panels below Files.
    let mut nearest: Vec<SidePanel> = SidePanel::ALL
        .into_iter()
        .filter(|&p| p != expanded)
        .collect();
    nearest.sort_by_key(|p| (p.index().abs_diff(expanded.index()), p.index()));
    let shown = usize::from(height.saturating_sub(EXPANDED_MIN) / collapsed).min(nearest.len());
    for panel in &nearest[..shown] {
        heights[panel.index()] = collapsed;
    }
    let used: u16 = heights.iter().sum();
    heights[expanded.index()] = height - used;
    heights
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_PANELS: [Panel; 6] = [
        Panel::Side(SidePanel::Status),
        Panel::Side(SidePanel::Files),
        Panel::Side(SidePanel::Branches),
        Panel::Side(SidePanel::Commits),
        Panel::Side(SidePanel::Stash),
        Panel::Diff,
    ];

    #[test]
    fn side_panels_are_numbered_one_to_five_and_the_diff_zero() {
        let numbers: Vec<u8> = ALL_PANELS.iter().map(|p| p.number()).collect();
        assert_eq!(numbers, [1, 2, 3, 4, 5, 0]);
        assert_eq!(SidePanel::Branches.title(), "Branches");
    }

    #[test]
    fn screen_modes_cycle_normal_half_full() {
        assert_eq!(ScreenMode::default(), ScreenMode::Normal);
        assert_eq!(ScreenMode::Normal.next(), ScreenMode::Half);
        assert_eq!(ScreenMode::Half.next(), ScreenMode::Full);
        assert_eq!(ScreenMode::Full.next(), ScreenMode::Normal);
    }

    #[test]
    fn a_tall_column_collapses_the_others_to_three_rows() {
        assert_eq!(left_heights(23, SidePanel::Files), [3, 11, 3, 3, 3]);
        assert_eq!(left_heights(17, SidePanel::Stash), [3, 3, 3, 3, 5]);
    }

    #[test]
    fn a_short_column_collapses_the_others_to_a_title_and_a_line_then_a_title() {
        assert_eq!(left_heights(12, SidePanel::Files), [1, 8, 1, 1, 1]);
        assert_eq!(left_heights(16, SidePanel::Status), [8, 2, 2, 2, 2]);
        assert_eq!(left_heights(13, SidePanel::Files), [2, 5, 2, 2, 2]);
    }

    #[test]
    fn a_tiny_column_hides_the_farthest_panels_first() {
        // Room for the expanded panel and one title row: the nearest stays,
        // the one above on a tie.
        assert_eq!(left_heights(6, SidePanel::Files), [1, 5, 0, 0, 0]);
        assert_eq!(left_heights(7, SidePanel::Commits), [0, 0, 1, 5, 1]);
        assert_eq!(left_heights(6, SidePanel::Stash), [0, 0, 0, 1, 5]);
        assert_eq!(left_heights(3, SidePanel::Files), [0, 3, 0, 0, 0]);
        assert_eq!(left_heights(1, SidePanel::Branches), [0, 0, 1, 0, 0]);
        assert_eq!(left_heights(0, SidePanel::Files), [0; 5]);
    }

    #[test]
    fn heights_always_add_up_to_the_column() {
        for height in 0..60 {
            for expanded in SidePanel::ALL {
                let heights = left_heights(height, expanded);
                assert_eq!(heights.iter().sum::<u16>(), height, "{height} {expanded:?}");
                assert!(heights[expanded.index()] > 0 || height == 0);
            }
        }
    }

    fn layout(focused: Panel, mode: ScreenMode) -> (Rect, WorkspaceLayout) {
        let main = Rect::new(0, 0, 120, 23);
        let expanded = match focused {
            Panel::Side(side) => side,
            Panel::Diff => SidePanel::Commits,
        };
        (main, workspace_layout(main, focused, expanded, mode))
    }

    fn left_width(layout: &WorkspaceLayout) -> u16 {
        layout.rect(Panel::Side(SidePanel::Status)).unwrap().width
    }

    #[test]
    fn normal_gives_the_left_column_a_third() {
        for focused in ALL_PANELS {
            let (_, layout) = layout(focused, ScreenMode::Normal);
            assert_eq!(left_width(&layout), 40, "{focused:?}");
            assert_eq!(layout.rect(Panel::Diff).unwrap().width, 80);
            assert_eq!(layout.rect(Panel::Diff).unwrap().x, 40);
        }
    }

    #[test]
    fn half_widens_the_column_for_a_side_panel_and_narrows_it_for_the_diff() {
        let (_, side) = layout(Panel::Side(SidePanel::Files), ScreenMode::Half);
        assert_eq!(left_width(&side), 60);
        let (_, diff) = layout(Panel::Diff, ScreenMode::Half);
        assert_eq!(left_width(&diff), 30);
        assert_eq!(diff.rect(Panel::Diff).unwrap().width, 90);
    }

    #[test]
    fn full_gives_the_focused_panel_everything() {
        for focused in ALL_PANELS {
            let (main, layout) = layout(focused, ScreenMode::Full);
            for panel in ALL_PANELS {
                let expected = (panel == focused).then_some(main);
                assert_eq!(layout.rect(panel), expected, "{focused:?} {panel:?}");
            }
        }
    }

    #[test]
    fn the_column_stacks_panels_in_order_with_the_expanded_one_filling() {
        let main = Rect::new(0, 0, 90, 23);
        let layout = workspace_layout(
            main,
            Panel::Side(SidePanel::Files),
            SidePanel::Files,
            ScreenMode::Normal,
        );
        let rect = |side| layout.rect(Panel::Side(side)).unwrap();
        assert_eq!(rect(SidePanel::Status), Rect::new(0, 0, 30, 3));
        assert_eq!(rect(SidePanel::Files), Rect::new(0, 3, 30, 11));
        assert_eq!(rect(SidePanel::Branches), Rect::new(0, 14, 30, 3));
        assert_eq!(rect(SidePanel::Stash), Rect::new(0, 20, 30, 3));
    }

    #[test]
    fn a_hidden_panel_is_none_and_the_expanded_one_never_is() {
        let main = Rect::new(0, 0, 80, 6);
        let layout = workspace_layout(main, Panel::Diff, SidePanel::Files, ScreenMode::Normal);
        assert!(layout.rect(Panel::Side(SidePanel::Status)).is_some());
        assert!(layout.rect(Panel::Side(SidePanel::Stash)).is_none());
        for height in [0, 1, 3] {
            let main = Rect::new(0, 0, 80, height);
            for expanded in SidePanel::ALL {
                for mode in [ScreenMode::Normal, ScreenMode::Half] {
                    for focused in [Panel::Side(expanded), Panel::Diff] {
                        let layout = workspace_layout(main, focused, expanded, mode);
                        assert!(layout.rect(Panel::Side(expanded)).is_some());
                    }
                }
            }
        }
    }

    #[test]
    fn no_rect_lies_outside_main() {
        for (w, h) in [(0, 0), (1, 1), (7, 3), (20, 5), (80, 23), (200, 59)] {
            let main = Rect::new(3, 2, w, h);
            for focused in ALL_PANELS {
                for expanded in SidePanel::ALL {
                    for mode in [ScreenMode::Normal, ScreenMode::Half, ScreenMode::Full] {
                        let layout = workspace_layout(main, focused, expanded, mode);
                        for panel in ALL_PANELS {
                            if let Some(rect) = layout.rect(panel) {
                                assert_eq!(main.union(rect), main, "{panel:?} {rect:?}");
                            }
                        }
                    }
                }
            }
        }
    }
}
