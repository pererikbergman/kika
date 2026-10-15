//! Fitting a menu row's disabled reason into the room beside its label,
//! or a notice into the bar. A reason that ends in a path keeps the file
//! name: the path is cut from the left and the words before it give way
//! first. A reason with marked advice keeps that the same way: the words
//! before it are cut first. Other reasons are cut from the right.

use crate::keymap::Reason;

use crate::view::width_of;

/// The fewest cells the words before a path keep, `…` included; with less
/// room they are dropped and the path has the row.
const LEAD_MIN: usize = 8;

/// `reason` in at most `room` cells. Whole when it fits; a reason with a
/// marked path keeps `…/name` before anything else, then as much of the
/// words before it as fit, cut from the right; one with marked advice
/// keeps the advice, cut from the right only when it alone is too wide,
/// then as much of the words before it as fit; any other reason is cut
/// from the right.
pub(crate) fn fit_reason(reason: &Reason, room: usize) -> String {
    if width_of(reason) <= room {
        return reason.to_string();
    }
    if let Some((lead, advice)) = reason.advice_split() {
        return fit_advice(lead, advice, room);
    }
    let Some((lead, path)) = reason.path_split() else {
        return cut_right(reason, room);
    };
    let min_path = match path.rfind('/') {
        None => width_of(path),
        Some(slash) => width_of(path).min(2 + width_of(&path[slash + 1..])),
    };
    if room <= min_path {
        return cut_left(path, room);
    }
    let shown_path = cut_left(path, room.saturating_sub(width_of(lead)).max(min_path));
    let lead_room = room - width_of(&shown_path);
    if width_of(lead) <= lead_room {
        format!("{lead}{shown_path}")
    } else if lead_room >= LEAD_MIN {
        format!("{} {shown_path}", cut_right(lead.trim_end(), lead_room - 1))
    } else {
        cut_left(path, room)
    }
}

/// `lead` then `advice` in `room` cells, too few for both whole. The
/// advice stays whole while it fits; the lead keeps at least `LEAD_MIN`
/// cells or is dropped.
fn fit_advice(lead: &str, advice: &str, room: usize) -> String {
    let lead_room = room.saturating_sub(width_of(advice));
    if lead_room >= LEAD_MIN {
        format!("{} {advice}", cut_right(lead.trim_end(), lead_room - 1))
    } else {
        cut_right(advice, room)
    }
}

/// `text` in at most `n` cells: its start, without trailing spaces, then
/// `…`. A glyph is never split.
fn cut_right(text: &str, n: usize) -> String {
    if width_of(text) <= n {
        return text.to_string();
    }
    if n == 0 {
        return String::new();
    }
    let kept = bytes_within(glyphs(text), n - 1);
    format!("{}…", text[..kept].trim_end_matches(' '))
}

/// `text` in at most `n` cells: `…`, then its end. A glyph is never split.
fn cut_left(text: &str, n: usize) -> String {
    if width_of(text) <= n {
        return text.to_string();
    }
    if n == 0 {
        return String::new();
    }
    let kept = bytes_within(glyphs(text).into_iter().rev(), n - 1);
    format!("…{}", &text[text.len() - kept..])
}

/// The byte length of the leading `glyphs` that fit in `room` cells.
fn bytes_within<'a>(glyphs: impl IntoIterator<Item = &'a str>, room: usize) -> usize {
    let mut used = 0;
    glyphs
        .into_iter()
        .take_while(|g| {
            used += width_of(g);
            used <= room
        })
        .map(str::len)
        .sum()
}

/// `text` split into the pieces a cut may not split: each sanitised
/// `<U+XXXX>` marker, as `push_safe` writes it, and each other char.
fn glyphs(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        let len = marker_len(rest).unwrap_or(c.len_utf8());
        let (glyph, tail) = rest.split_at(len);
        out.push(glyph);
        rest = tail;
    }
    out
}

/// The byte length of the marker `text` starts with: `<U+`, four to six
/// uppercase hex digits and `>`.
fn marker_len(text: &str) -> Option<usize> {
    let hex = text.strip_prefix("<U+")?;
    let digits = (hex.bytes())
        .take_while(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(b))
        .count();
    ((4..=6).contains(&digits) && hex.as_bytes().get(digits) == Some(&b'>')).then_some(digits + 4)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notice::Notice;
    use crate::view::elide_right;

    const LEAD: &str = "A file now sits at the renamed file's old path: ";
    const PATH: &str = "src/features/stash/push.rs";

    fn marked(path: &str) -> Reason {
        Notice::with_path(LEAD, path)
    }

    #[test]
    fn a_reason_that_fits_is_unchanged() {
        let reason = marked(PATH);
        let whole = format!("{LEAD}{PATH}");
        for room in [whole.len(), whole.len() + 10] {
            assert_eq!(fit_reason(&reason, room), whole);
        }
        let plain = Notice::fixed("Nothing to commit");
        assert_eq!(fit_reason(&plain, 17), "Nothing to commit");
    }

    #[test]
    fn a_reason_without_a_path_is_cut_from_the_right() {
        let plain = Notice::fixed("This rename only changes case and cannot be stashed by file");
        for room in [1, 5, 13, 30] {
            let fitted = fit_reason(&plain, room);
            assert_eq!(fitted, cut_right(&plain, room), "{room}");
        }
        // The same as the bar's cut where no space sits before it; a
        // space there is trimmed.
        assert_eq!(fit_reason(&plain, 12), elide_right(&plain, 12));
        assert_eq!(fit_reason(&plain, 12), "This rename…");
        assert_eq!(fit_reason(&plain, 13), "This rename…");
        assert_eq!(fit_reason(&plain, 0), "");
    }

    #[test]
    fn an_advised_reason_keeps_its_advice_and_cuts_the_words_first() {
        use crate::features::stash::keys::{STASH_RENAME_CASE, stash_rename_case_reason};
        let reason = stash_rename_case_reason();
        let advice = "use Stash all changes… and apply it on this commit";
        assert_eq!(width_of(advice), 50);
        assert_eq!(fit_reason(&reason, 200), STASH_RENAME_CASE);
        assert_eq!(
            fit_reason(&reason, width_of(STASH_RENAME_CASE)),
            STASH_RENAME_CASE
        );
        assert_eq!(
            fit_reason(&reason, 70),
            format!("This rename only c… {advice}")
        );
        // Below `LEAD_MIN` cells for the words, the advice has the row.
        assert_eq!(fit_reason(&reason, 58), format!("This r… {advice}"));
        assert_eq!(fit_reason(&reason, 57), advice);
        assert_eq!(fit_reason(&reason, 50), advice);
        // Only then is the advice itself cut, from the right.
        assert_eq!(fit_reason(&reason, 22), "use Stash all changes…");
        assert_eq!(fit_reason(&reason, 1), "…");
        assert_eq!(fit_reason(&reason, 0), "");
        // Unmarked, the same text is cut from the right as before.
        let plain = Notice::fixed(STASH_RENAME_CASE);
        assert_eq!(fit_reason(&plain, 70), cut_right(&plain, 70));
    }

    #[test]
    fn a_path_reason_keeps_the_file_name_and_cuts_the_words_first() {
        let reason = marked(PATH);
        assert_eq!(
            fit_reason(&reason, 53),
            "A file now sits at the renamed file's old… …/push.rs"
        );
        assert_eq!(fit_reason(&reason, 13), "…tash/push.rs");
        assert_eq!(fit_reason(&reason, 5), "…h.rs");
        assert_eq!(fit_reason(&reason, 9), "…/push.rs");
        assert_eq!(fit_reason(&reason, 1), "…");
        assert_eq!(fit_reason(&reason, 0), "");
    }

    #[test]
    fn a_path_with_no_directory_keeps_the_lead_while_it_can() {
        let reason = marked("old.rs");
        assert_eq!(fit_reason(&reason, 20), "A file now s… old.rs");
        assert_eq!(fit_reason(&reason, 14), "A file… old.rs");
        // Below `LEAD_MIN` cells for the words, only the path is left.
        assert_eq!(fit_reason(&reason, 13), "old.rs");
        assert_eq!(fit_reason(&reason, 6), "old.rs");
        assert_eq!(fit_reason(&reason, 4), "….rs");
    }

    /// Each `<U+` in `text` starts a whole `<U+XXXX>` marker.
    fn markers_whole(text: &str) -> bool {
        text.match_indices("<U+").all(|(i, _)| {
            let rest = &text[i + 3..];
            let hex = rest.bytes().take_while(u8::is_ascii_hexdigit).count();
            (4..=6).contains(&hex) && rest.as_bytes().get(hex) == Some(&b'>')
        })
    }

    #[test]
    fn no_room_panics_overflows_or_splits_a_marker() {
        let reasons = [
            marked(PATH),
            marked("old.rs"),
            marked("日本/語.rs"),
            marked("a\u{202E}b/c\u{200B}d.rs"),
            Notice::fixed("This rename only changes case and cannot be stashed by file"),
            Notice::advised("x<U+202E>y; z<U+200B> 日本語 advice"),
            crate::features::stash::keys::stash_rename_case_reason(),
            Notice::owned("x\u{202E}y\u{200B}z 日本語".to_string()),
        ];
        assert!(reasons[3].contains("a<U+202E>b/c<U+200B>d.rs"));
        for reason in &reasons {
            for room in 0..=80 {
                let fitted = fit_reason(reason, room);
                assert!(width_of(&fitted) <= room, "{room}: {fitted:?}");
                assert!(markers_whole(&fitted), "{room}: {fitted:?}");
            }
        }
    }

    #[test]
    fn a_marker_or_wide_char_is_never_split() {
        let reason = marked("a\u{202E}b/c\u{200B}d.rs");
        // `…` + `d.rs` is 5; one more cell cannot hold `<U+200B>`.
        assert_eq!(fit_reason(&reason, 6), "…d.rs");
        assert_eq!(fit_reason(&reason, 13), "…<U+200B>d.rs");
        assert_eq!(fit_reason(&reason, 14), "…c<U+200B>d.rs");
        let wide = marked("日本/語.rs");
        assert_eq!(fit_reason(&wide, 6), "…語.rs");
        assert_eq!(fit_reason(&wide, 5), "….rs");
    }
}
