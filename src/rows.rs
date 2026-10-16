//! Row pieces that more than one panel draws.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::Palette;
use crate::view::elide_right;
use kika_git::commit_files::CommitFile;

/// `<status> <path>`, or `<status> <old> → <new>` for a rename or copy,
/// cut to `width`.
pub(crate) fn commit_file_line(
    palette: &Palette,
    file: &CommitFile,
    width: usize,
) -> Line<'static> {
    let colour = status_colour(palette, file.status);
    let path = drawn_path(file);
    Line::from(vec![
        Span::styled(file.status.to_string(), Style::new().fg(colour)),
        Span::raw(" "),
        Span::raw(elide_right(&path, width.saturating_sub(2))),
    ])
}

/// The colour of a changed file's status letter.
pub(crate) fn status_colour(palette: &Palette, status: char) -> Color {
    match status {
        'A' => palette.added,
        'D' => palette.removed,
        'M' => palette.modified,
        'R' | 'C' => palette.status_renamed,
        _ => palette.text,
    }
}

/// The diff mark's column, on a marked commit or branch row. Its own
/// glyph, not the current branch's `*`, so the two read apart without
/// colour.
pub(crate) const DIFF_MARK: &str = "◆ ";

pub(crate) fn diff_mark_style(palette: &Palette) -> Style {
    Style::new().fg(palette.mark).add_modifier(Modifier::BOLD)
}

/// A row's path as drawn, with the old name before it for a rename or
/// copy. The filter matches this and not the status letter, so a row
/// matches by what the user reads in it.
pub(crate) fn drawn_path(file: &CommitFile) -> String {
    match &file.orig_path {
        Some(orig) => format!("{} → {}", orig.display(), file.path.display()),
        None => file.path.display().to_string(),
    }
}

/// `HH:MM` of the UTC day `secs` (since the epoch) falls in, by plain
/// arithmetic: no clock is read and no time zone is known.
pub(crate) fn clock(secs: i64) -> String {
    let of_day = secs.rem_euclid(86_400);
    format!("{:02}:{:02}", of_day / 3600, of_day % 3600 / 60)
}
