//! The custom patch's mark in the Diff pane: which rows of a commit's
//! change the patch holds. Core `view.rs` draws the mark.

use super::CustomPatch;
use crate::hunk_view::HunkView;

/// `(hunk, line)` of each row whose changed line the patch holds.
pub(crate) fn picked_rows(
    view: &HunkView,
    patch: &CustomPatch,
) -> std::collections::HashSet<(usize, usize)> {
    let Some(file) = &view.patch else {
        return Default::default();
    };
    file.hunks
        .iter()
        .enumerate()
        .flat_map(|(h, hunk)| {
            hunk.line_ids()
                .into_iter()
                .enumerate()
                .filter_map(move |(l, id)| Some(((h, l), id?)))
        })
        .filter(|(_, id)| patch.contains_line(&view.path, *id))
        .map(|(at, _)| at)
        .collect()
}
