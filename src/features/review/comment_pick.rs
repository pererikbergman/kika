//! Picking the line or lines a comment goes on: `a` on the files screen
//! puts a cursor on the first numbered row of the diff in view, `j`/`k`
//! move it over numbered rows only, `v` anchors a range, and `enter`
//! opens the composer on them. The diff scrolls only to keep the cursor
//! in view. The pick ends when the file, the view or the diff changes.

use super::comment_editor::{Composer, ComposerKind};
use super::files::{FilesStage, ReviewView, files_screen};
use super::pending::{CommentTarget, comment_disabled};
use crate::diff_search::reveal;
use crate::effect::Effect;
use crate::model::AppState;
use kika_forge::anchor::{GhLine, GhSide, RowNumber, row_numbers, row_positions};
use kika_forge::pending::RowPos;
use kika_git::diff::LineKind;

pub(super) const ACROSS_HUNKS: &str = "A range must stay inside one hunk";

/// The rows picked, by index into the streamed diff: the cursor, and the
/// other end of a range once `v` set one. It belongs to one file's diff
/// stream, so it is dropped when another starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommentPick {
    pub cursor: usize,
    pub anchor: Option<usize>,
    /// The file list's cursor and the diff request it was picked on.
    pub file: usize,
    pub seq: Option<u64>,
}

impl CommentPick {
    /// The first and last row picked.
    pub fn span(&self) -> (usize, usize) {
        let other = self.anchor.unwrap_or(self.cursor);
        (other.min(self.cursor), other.max(self.cursor))
    }
}

/// The pick, while the diff it was made on is still the one shown.
pub(crate) fn live_pick(state: &AppState) -> Option<&CommentPick> {
    let files = files_screen(state).filter(|_| state.review_files_shown())?;
    let pick = files.pick.as_ref()?;
    let same = files.stage == FilesStage::Ready
        && files.view == ReviewView::Whole
        && pick.file == files.cursor
        && pick.seq.is_some()
        && pick.seq == state.diff.seq;
    same.then_some(pick)
}

/// Pick mode has the keys.
pub(crate) fn comment_pick_open(state: &AppState) -> bool {
    live_pick(state).is_some()
}

fn numbered((old, new): RowNumber) -> bool {
    old.is_some() || new.is_some()
}

/// GitHub's line for a numbered row: its new line when it has one (an
/// added or context row), else its old one (a removed row).
fn gh_line((old, new): RowNumber) -> Option<GhLine> {
    match (old, new) {
        (_, Some(line)) => Some(GhLine {
            side: GhSide::Right,
            line,
        }),
        (Some(line), None) => Some(GhLine {
            side: GhSide::Left,
            line,
        }),
        (None, None) => None,
    }
}

/// The first numbered row of the diff in view, once the shown file's
/// own lines have come.
pub(crate) fn numbered_in_view(state: &AppState) -> Option<usize> {
    let diff = &state.diff;
    if diff.pending || diff.error.is_some() {
        return None;
    }
    let numbers = row_numbers(&diff.lines);
    let rows = state.review_diff_rows();
    (diff.scroll..numbers.len())
        .take(rows)
        .find(|&row| numbered(numbers[row]))
}

impl AppState {
    /// `a`: start picking, on the first numbered row in view.
    pub(crate) fn start_comment_pick(&mut self) {
        if let Some(reason) = comment_disabled(self) {
            self.notice = Some(reason);
            return;
        }
        let Some(cursor) = numbered_in_view(self) else {
            return;
        };
        let seq = self.diff.seq;
        if let Some(files) = &mut self.review.files {
            files.pick = Some(CommentPick {
                cursor,
                anchor: None,
                file: files.cursor,
                seq,
            });
        }
    }

    /// `j`/`k`: the next or previous numbered row, the diff scrolled to
    /// keep it in view.
    pub(crate) fn step_comment_pick(&mut self, down: bool) -> Vec<Effect> {
        let Some(pick) = live_pick(self).copied() else {
            return self.end_comment_pick();
        };
        let numbers = row_numbers(&self.diff.lines);
        let next = if down {
            (pick.cursor + 1..numbers.len()).find(|&r| numbered(numbers[r]))
        } else {
            (0..pick.cursor).rev().find(|&r| numbered(numbers[r]))
        };
        let Some(cursor) = next else {
            return vec![];
        };
        if let Some(pick) = self.review.files.as_mut().and_then(|f| f.pick.as_mut()) {
            pick.cursor = cursor;
        }
        let rows = self.review_diff_rows();
        self.diff.scroll = reveal(cursor, self.diff.scroll, rows);
        self.diff.demand(rows)
    }

    /// `v`: anchor a range at the cursor, or drop the anchor.
    pub(crate) fn toggle_comment_range(&mut self) {
        if live_pick(self).is_none() {
            self.end_comment_pick();
            return;
        }
        if let Some(pick) = self.review.files.as_mut().and_then(|f| f.pick.as_mut()) {
            pick.anchor = match pick.anchor {
                Some(_) => None,
                None => Some(pick.cursor),
            };
        }
    }

    /// `enter`: the composer on the rows picked. A range that crosses a
    /// hunk header is refused, and the pick stays.
    pub(crate) fn confirm_comment_pick(&mut self) {
        let Some(target) = self.picked_target() else {
            return;
        };
        self.end_comment_pick();
        self.review.composer = Some(Composer::new(ComposerKind::Line {
            target,
            editing: None,
        }));
    }

    /// Where a comment on the rows picked goes, the pick left open; `None`
    /// when there is none. A pick whose diff has gone, or whose file has no
    /// UTF-8 name, is ended; a range across a hunk header says so and
    /// stays.
    pub(super) fn picked_target(&mut self) -> Option<CommentTarget> {
        let Some(pick) = live_pick(self).copied() else {
            self.end_comment_pick();
            return None;
        };
        let numbers = row_numbers(&self.diff.lines);
        let (first, last) = pick.span();
        let crosses = self.diff.lines[first..=last]
            .iter()
            .any(|line| matches!(line.kind, LineKind::Hunk | LineKind::Header));
        if crosses {
            self.notice = Some(ACROSS_HUNKS.into());
            return None;
        }
        let (start, end) = (gh_line(numbers[first])?, gh_line(numbers[last])?);
        let positions = row_positions(&self.diff.lines);
        let pos = |row: usize| {
            let (old, new) = positions[row]?;
            let (o, n) = numbers[row];
            Some(RowPos {
                old,
                new,
                both: o.is_some() && n.is_some(),
            })
        };
        let range = first != last;
        let target = self.review.files.as_ref().and_then(|files| {
            let path = std::str::from_utf8(files.selected()?.file.path.as_bytes()).ok()?;
            Some(CommentTarget {
                path: path.to_string(),
                start: range.then_some(start),
                end,
                head: files.head.as_str().to_string(),
                start_pos: pos(first).filter(|_| range),
                end_pos: pos(last),
            })
        });
        if target.is_none() {
            self.end_comment_pick();
        }
        target
    }

    /// `esc`, or a pick whose diff has gone: back to the files screen.
    pub(crate) fn end_comment_pick(&mut self) -> Vec<Effect> {
        if let Some(files) = &mut self.review.files {
            files.pick = None;
        }
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;

    use super::*;
    use crate::features::review::pending::tests::{
        commenting, ctrl, left, right, saved, type_text,
    };
    use crate::features::review::state::tests::{press, type_char};
    use crate::intent::AppIntent;
    use crate::keymap::KeyScope;

    fn cursor(state: &AppState) -> Option<(usize, Option<usize>)> {
        live_pick(state).map(|p| (p.cursor, p.anchor))
    }

    fn target(state: &AppState) -> (Option<GhLine>, GhLine) {
        let composer = state.review.composer.as_ref().expect("a composer");
        let ComposerKind::Line { target, .. } = &composer.kind else {
            panic!("{composer:?}");
        };
        (target.start, target.end)
    }

    #[test]
    fn a_starts_on_the_first_numbered_row_and_j_k_skip_header_and_hunk_rows() {
        let mut state = commenting();
        assert_eq!(type_char(&mut state, 'a'), []);
        assert_eq!(state.key_focus(), KeyScope::ReviewPick);
        assert_eq!(cursor(&state), Some((4, None)));
        assert_eq!(type_char(&mut state, 'k'), []);
        assert_eq!(cursor(&state), Some((4, None)), "nothing numbered above");
        for want in [5, 6, 7, 9, 10, 11, 11] {
            type_char(&mut state, 'j');
            assert_eq!(cursor(&state), Some((want, None)));
        }
        for want in [10, 9, 7] {
            press(&mut state, KeyCode::Up);
            assert_eq!(cursor(&state), Some((want, None)));
        }
        assert_eq!(press(&mut state, KeyCode::Esc), []);
        assert_eq!(state.review.files.as_ref().unwrap().pick, None);
        assert_eq!(state.key_focus(), KeyScope::ReviewFiles);
    }

    #[test]
    fn a_removed_row_is_on_the_old_side_and_a_context_row_on_the_new() {
        let mut state = commenting();
        type_char(&mut state, 'a');
        type_char(&mut state, 'j');
        press(&mut state, KeyCode::Enter);
        assert_eq!(target(&state), (None, right(2)), "context");
        press(&mut state, KeyCode::Esc);

        type_char(&mut state, 'a');
        type_char(&mut state, 'j');
        type_char(&mut state, 'j');
        type_char(&mut state, 'a');
        assert_eq!(target(&state), (None, left(2)), "removed");
        type_text(&mut state, "old");
        let (_, review) = saved(&ctrl(&mut state, 's'));
        assert_eq!(review.comments[0].end, left(2));
        assert_eq!(review.comments[0].start, None);
    }

    fn positions(state: &AppState) -> (Option<RowPos>, Option<RowPos>) {
        let composer = state.review.composer.as_ref().expect("a composer");
        let ComposerKind::Line { target, .. } = &composer.kind else {
            panic!("{composer:?}");
        };
        (target.start_pos, target.end_pos)
    }

    #[test]
    fn each_end_keeps_both_line_counters_of_its_row() {
        let pos = |old, new, both| Some(RowPos { old, new, both });
        // `+fn new() {}` alone: added, the old counter the next old line.
        let mut state = commenting();
        type_char(&mut state, 'a');
        press(&mut state, KeyCode::Enter);
        assert_eq!(positions(&state), (None, pos(1, 1, false)));
        // From the context row to `+fn newer() {}`.
        press(&mut state, KeyCode::Esc);
        type_char(&mut state, 'a');
        type_char(&mut state, 'j');
        type_char(&mut state, 'v');
        type_char(&mut state, 'j');
        type_char(&mut state, 'j');
        press(&mut state, KeyCode::Enter);
        assert_eq!(target(&state), (Some(right(2)), right(3)));
        assert_eq!(positions(&state), (pos(1, 2, true), pos(3, 3, false)));
        type_text(&mut state, "range");
        let (_, review) = saved(&ctrl(&mut state, 's'));
        assert_eq!(review.comments[0].start_pos, pos(1, 2, true));
        assert_eq!(review.comments[0].end_pos, pos(3, 3, false));
    }

    #[test]
    fn v_then_j_twice_makes_a_range_with_each_end_on_its_own_side() {
        let mut state = commenting();
        type_char(&mut state, 'a');
        assert_eq!(type_char(&mut state, 'v'), []);
        type_char(&mut state, 'j');
        type_char(&mut state, 'j');
        assert_eq!(cursor(&state), Some((6, Some(4))));
        press(&mut state, KeyCode::Enter);
        assert_eq!(target(&state), (Some(right(1)), left(2)));
        // `v` again drops the anchor.
        press(&mut state, KeyCode::Esc);
        type_char(&mut state, 'a');
        type_char(&mut state, 'v');
        type_char(&mut state, 'v');
        assert_eq!(cursor(&state), Some((4, None)));
    }

    #[test]
    fn a_range_across_a_hunk_header_is_refused_and_opens_nothing() {
        let mut state = commenting();
        type_char(&mut state, 'a');
        for _ in 0..3 {
            type_char(&mut state, 'j');
        }
        type_char(&mut state, 'v');
        type_char(&mut state, 'j');
        assert_eq!(cursor(&state), Some((9, Some(7))));
        assert_eq!(press(&mut state, KeyCode::Enter), []);
        assert_eq!(state.notice.as_deref(), Some(ACROSS_HUNKS));
        assert_eq!(state.review.composer, None);
        assert_eq!(state.key_focus(), KeyScope::ReviewPick);
    }

    #[test]
    fn the_cursor_keeps_in_view_and_the_pick_ends_when_the_diff_changes() {
        let mut state = commenting();
        // A screen with room for three diff rows only.
        state.size = (100, 12);
        let rows = state.review_diff_rows();
        type_char(&mut state, 'a');
        for _ in 0..6 {
            type_char(&mut state, 'j');
        }
        let (at, _) = cursor(&state).unwrap();
        assert_eq!(at, 11);
        assert!(state.diff.scroll <= at && at < state.diff.scroll + rows);
        // Another file: its diff is another stream, so the pick is gone.
        state.review.files.as_mut().unwrap().focus =
            crate::features::review::files::FilesFocus::List;
        state.review.files.as_mut().unwrap().pick = Some(CommentPick {
            seq: Some(u64::MAX),
            ..*live_pick(&state).unwrap()
        });
        assert!(!comment_pick_open(&state));
        assert_eq!(state.key_focus(), KeyScope::ReviewFiles);
        assert_eq!(type_char(&mut state, 'a'), []);
        // The scroll moved to keep row 11 in view; a new pick starts at
        // the first numbered row there.
        assert!(state.diff.scroll > 0);
        assert_eq!(cursor(&state).map(|(c, _)| c), numbered_in_view(&state));
        let effects = type_char(&mut state, 'k');
        assert!(
            effects
                .iter()
                .all(|e| matches!(e, Effect::DiffDemand { .. })),
            "{effects:?}"
        );
        state.update(AppIntent::Review(
            crate::features::review::ReviewIntent::CancelCommentPick,
        ));
        // Moving to another file ends a pick made there.
        type_char(&mut state, 'a');
        state.review.files.as_mut().unwrap().cursor = 1;
        assert!(!comment_pick_open(&state));
    }
}
