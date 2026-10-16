//! The colours views draw with, named by what they mean.
//!
//! A role names what a colour means (an added line, a warning, a pending
//! check), not which colour it is, so a theme can swap colours without
//! touching any view. Every view reads its colours from the [`Palette`] in
//! `AppState`; only this module names terminal colours. Colour is an extra
//! signal only: symbols and text carry the meaning in every palette.

use ratatui::style::Color;
use serde::Deserialize;

/// One colour per semantic role. Some roles share a colour today
/// (`pending` and `warning`, `neutral` and `dim`); they are separate so a
/// theme can tell them apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Palette {
    /// Plain text that still takes a colour slot: a plain status letter,
    /// the rebase `pick` action.
    pub(crate) text: Color,
    /// Unfocused borders, hints, empty states, dates and disabled rows.
    pub(crate) dim: Color,
    /// The focused panel's border and the Diff pane's cursor gutter.
    pub(crate) focused: Color,
    /// Added lines and counts, and the status `A`.
    pub(crate) added: Color,
    /// Removed lines and counts, and the status `D`.
    pub(crate) removed: Color,
    /// The status `M`.
    pub(crate) modified: Color,
    /// The status `R` and `C`.
    pub(crate) status_renamed: Color,
    /// Hunk headers in the diff and hunk views.
    pub(crate) hunk_header: Color,
    /// Notices and things to look at: the bar notice, next steps, markers.
    pub(crate) warning: Color,
    /// Failures: op errors, conflicted files, load errors.
    pub(crate) error: Color,
    /// Done and fine: viewed, check passed, mergeable.
    pub(crate) success: Color,
    /// A check still running.
    pub(crate) pending: Color,
    /// A check neutral or skipped; mergeability unknown.
    pub(crate) neutral: Color,
    /// A commit's short hash.
    pub(crate) hash: Color,
    /// A stash name or reflog selector.
    pub(crate) ref_name: Color,
    /// A reflog entry's label.
    pub(crate) ref_label: Color,
    /// A commit's author.
    pub(crate) author: Color,
    /// The commit graph's lanes.
    pub(crate) graph: Color,
    /// The diff mark and the custom-patch mark.
    pub(crate) mark: Color,
    /// Copied commits and the bar's copied note.
    pub(crate) copied_commit: Color,
    /// Where a review comment sits.
    pub(crate) location: Color,
    /// The "ours" side of a conflict.
    pub(crate) conflict_ours: Color,
    /// The "theirs" side of a conflict.
    pub(crate) conflict_theirs: Color,
    /// A review thread still open.
    pub(crate) thread_open: Color,
    /// A review thread addressed.
    pub(crate) thread_addressed: Color,
    /// The PR badge: open.
    pub(crate) pr_open: Color,
    /// The PR badge: draft.
    pub(crate) pr_draft: Color,
    /// The PR badge: merged.
    pub(crate) pr_merged: Color,
    /// The PR badge: closed.
    pub(crate) pr_closed: Color,
    /// The text of every search match, the current one included: it is
    /// drawn on `match_bg`, or on `current_match_bg` for the current match.
    pub(crate) match_fg: Color,
    /// The background of a search match.
    pub(crate) match_bg: Color,
    /// The background of the current search match.
    pub(crate) current_match_bg: Color,
}

impl Palette {
    /// The built-in default: the colours Kika has always drawn.
    pub(crate) const DEFAULT: Palette = Palette {
        text: Color::Reset,
        dim: Color::DarkGray,
        focused: Color::Cyan,
        added: Color::Green,
        removed: Color::Red,
        modified: Color::Yellow,
        status_renamed: Color::Cyan,
        hunk_header: Color::Cyan,
        warning: Color::Yellow,
        error: Color::Red,
        success: Color::Green,
        pending: Color::Yellow,
        neutral: Color::DarkGray,
        hash: Color::Cyan,
        ref_name: Color::Yellow,
        ref_label: Color::Cyan,
        author: Color::Green,
        graph: Color::Magenta,
        mark: Color::Magenta,
        copied_commit: Color::Cyan,
        location: Color::Cyan,
        conflict_ours: Color::Green,
        conflict_theirs: Color::Cyan,
        thread_open: Color::Yellow,
        thread_addressed: Color::Green,
        pr_open: Color::Green,
        pr_draft: Color::DarkGray,
        pr_merged: Color::Magenta,
        pr_closed: Color::Red,
        match_fg: Color::Black,
        match_bg: Color::Yellow,
        current_match_bg: Color::LightRed,
    };

    /// For light backgrounds. Every role is set, none to the terminal's
    /// own foreground or a weak 16-colour yellow, cyan or grey: 256-colour
    /// indexes read better on white.
    pub(crate) const LIGHT: Palette = Palette {
        text: Color::Black,
        dim: Color::Indexed(240),
        focused: Color::Indexed(25),
        added: Color::Indexed(28),
        removed: Color::Indexed(124),
        modified: Color::Indexed(130),
        status_renamed: Color::Indexed(25),
        hunk_header: Color::Indexed(25),
        warning: Color::Indexed(130),
        error: Color::Indexed(124),
        success: Color::Indexed(28),
        pending: Color::Indexed(130),
        neutral: Color::Indexed(240),
        hash: Color::Indexed(25),
        ref_name: Color::Indexed(130),
        ref_label: Color::Indexed(25),
        author: Color::Indexed(28),
        graph: Color::Indexed(90),
        mark: Color::Indexed(90),
        copied_commit: Color::Indexed(25),
        location: Color::Indexed(25),
        conflict_ours: Color::Indexed(28),
        conflict_theirs: Color::Indexed(25),
        thread_open: Color::Indexed(130),
        thread_addressed: Color::Indexed(28),
        pr_open: Color::Indexed(28),
        pr_draft: Color::Indexed(240),
        pr_merged: Color::Indexed(90),
        pr_closed: Color::Indexed(124),
        match_fg: Color::Black,
        match_bg: Color::Indexed(229),
        current_match_bg: Color::Indexed(217),
    };

    /// For dark backgrounds: each default colour in its bright form, and
    /// plain text white, so nothing is drawn dim.
    pub(crate) const HIGH_CONTRAST: Palette = Palette {
        text: Color::White,
        dim: Color::Gray,
        focused: Color::LightCyan,
        added: Color::LightGreen,
        removed: Color::LightRed,
        modified: Color::LightYellow,
        status_renamed: Color::LightCyan,
        hunk_header: Color::LightCyan,
        warning: Color::LightYellow,
        error: Color::LightRed,
        success: Color::LightGreen,
        pending: Color::LightYellow,
        neutral: Color::Gray,
        hash: Color::LightCyan,
        ref_name: Color::LightYellow,
        ref_label: Color::LightCyan,
        author: Color::LightGreen,
        graph: Color::LightMagenta,
        mark: Color::LightMagenta,
        copied_commit: Color::LightCyan,
        location: Color::LightCyan,
        conflict_ours: Color::LightGreen,
        conflict_theirs: Color::LightCyan,
        thread_open: Color::LightYellow,
        thread_addressed: Color::LightGreen,
        pr_open: Color::LightGreen,
        pr_draft: Color::Gray,
        pr_merged: Color::LightMagenta,
        pr_closed: Color::LightRed,
        match_fg: Color::Black,
        match_bg: Color::LightYellow,
        current_match_bg: Color::LightRed,
    };

    /// No colour at all, for a non-empty `NO_COLOR` (no-color.org). Every
    /// role is the terminal's own colour; marks, words and the bold,
    /// underline and reversed modifiers still carry the meaning.
    pub(crate) const NO_COLOR: Palette = Palette {
        text: Color::Reset,
        dim: Color::Reset,
        focused: Color::Reset,
        added: Color::Reset,
        removed: Color::Reset,
        modified: Color::Reset,
        status_renamed: Color::Reset,
        hunk_header: Color::Reset,
        warning: Color::Reset,
        error: Color::Reset,
        success: Color::Reset,
        pending: Color::Reset,
        neutral: Color::Reset,
        hash: Color::Reset,
        ref_name: Color::Reset,
        ref_label: Color::Reset,
        author: Color::Reset,
        graph: Color::Reset,
        mark: Color::Reset,
        copied_commit: Color::Reset,
        location: Color::Reset,
        conflict_ours: Color::Reset,
        conflict_theirs: Color::Reset,
        thread_open: Color::Reset,
        thread_addressed: Color::Reset,
        pr_open: Color::Reset,
        pr_draft: Color::Reset,
        pr_merged: Color::Reset,
        pr_closed: Color::Reset,
        match_fg: Color::Reset,
        match_bg: Color::Reset,
        current_match_bg: Color::Reset,
    };
}

/// `ROLES`, and `Palette::set` and `Palette::get` over them, from one list
/// so the three cannot drift apart.
macro_rules! roles {
    ($($role:ident),* $(,)?) => {
        /// Every role's config key, its field name, in field order.
        pub(crate) const ROLES: [&str; 32] = [$(stringify!($role)),*];

        impl Palette {
            /// Gives `role` the colour `colour`; `Err` when no role has
            /// that name.
            pub(crate) fn set(&mut self, role: &str, colour: Color) -> Result<(), ()> {
                match role {
                    $(stringify!($role) => self.$role = colour,)*
                    _ => return Err(()),
                }
                Ok(())
            }

            /// The colour of `role`, if there is one by that name.
            #[cfg(test)]
            pub(crate) fn get(&self, role: &str) -> Option<Color> {
                match role {
                    $(stringify!($role) => Some(self.$role),)*
                    _ => None,
                }
            }
        }
    };
}

roles!(
    text,
    dim,
    focused,
    added,
    removed,
    modified,
    status_renamed,
    hunk_header,
    warning,
    error,
    success,
    pending,
    neutral,
    hash,
    ref_name,
    ref_label,
    author,
    graph,
    mark,
    copied_commit,
    location,
    conflict_ours,
    conflict_theirs,
    thread_open,
    thread_addressed,
    pr_open,
    pr_draft,
    pr_merged,
    pr_closed,
    match_fg,
    match_bg,
    current_match_bg,
);

/// The built-in themes' names, as `[theme] name` takes them.
pub(crate) const THEMES: [&str; 3] = ["default", "light", "high-contrast"];

/// The built-in theme called `name`.
pub(crate) fn builtin(name: &str) -> Option<Palette> {
    match name {
        "default" => Some(Palette::DEFAULT),
        "light" => Some(Palette::LIGHT),
        "high-contrast" => Some(Palette::HIGH_CONTRAST),
        _ => None,
    }
}

/// A colour as a config file writes it, before it is checked. Anything
/// that is neither a number nor a string lands in `Other`, so it gets the
/// same plain message as a bad name instead of a parser error.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum ColourValue {
    Index(i64),
    Text(String),
    Other(toml::Value),
}

/// The colour names a config file may use, each the ratatui colour of the
/// same name. One spelling each, so a typo is an error, not a guess.
const NAMES: [(&str, Color); 16] = [
    ("black", Color::Black),
    ("red", Color::Red),
    ("green", Color::Green),
    ("yellow", Color::Yellow),
    ("blue", Color::Blue),
    ("magenta", Color::Magenta),
    ("cyan", Color::Cyan),
    ("gray", Color::Gray),
    ("dark_gray", Color::DarkGray),
    ("light_red", Color::LightRed),
    ("light_green", Color::LightGreen),
    ("light_yellow", Color::LightYellow),
    ("light_blue", Color::LightBlue),
    ("light_magenta", Color::LightMagenta),
    ("light_cyan", Color::LightCyan),
    ("white", Color::White),
];

/// `value` as a colour: one of the 16 names, `reset` for the terminal's
/// own colour, an index 0-255 (a number or its decimal string), or
/// `#rrggbb`. The error says what to write.
pub(crate) fn parse_colour(value: &ColourValue) -> Result<Color, String> {
    let colour = match value {
        ColourValue::Index(n) => index(*n),
        ColourValue::Text(text) if text == "reset" => Some(Color::Reset),
        ColourValue::Text(text) => named(text).or_else(|| decimal(text)).or_else(|| rgb(text)),
        ColourValue::Other(_) => None,
    };
    colour.ok_or_else(|| {
        let shown = match value {
            ColourValue::Index(n) => n.to_string(),
            ColourValue::Text(text) => format!("{text:?}"),
            ColourValue::Other(other) => other.to_string(),
        };
        format!(
            "{shown} is not a colour; use a colour name such as \"green\" or \"light_red\", \"reset\", a number 0-255, or \"#rrggbb\""
        )
    })
}

fn named(text: &str) -> Option<Color> {
    NAMES.iter().find(|(n, _)| *n == text).map(|(_, c)| *c)
}

fn index(n: i64) -> Option<Color> {
    u8::try_from(n).ok().map(Color::Indexed)
}

/// An index written as a string: ASCII digits only, so no sign or space.
fn decimal(text: &str) -> Option<Color> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok().map(Color::Indexed)
}

fn rgb(text: &str) -> Option<Color> {
    let hex = text.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some(Color::Rgb(byte(0)?, byte(2)?, byte(4)?))
}

/// Whether a `NO_COLOR` value turns colour off: any non-empty value does,
/// and an empty one counts as unset (no-color.org). Takes the value, not
/// the env, so callers read the env once and tests never touch it.
pub(crate) fn no_color(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|v| !v.is_empty())
}

/// Whether a role draws in the terminal's own colour, so it marks nothing
/// by colour and a view must mark it some other way. `const`, since `==` on
/// `Color` is not.
pub(crate) const fn is_reset(colour: Color) -> bool {
    matches!(colour, Color::Reset)
}

/// The palette a session starts with: `NO_COLOR` wins over the configured
/// theme and its colour overrides.
pub(crate) fn startup_palette(configured: Palette, no_color: bool) -> Palette {
    if no_color {
        Palette::NO_COLOR
    } else {
        configured
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn no_color_is_set_only_by_a_non_empty_value() {
        use std::ffi::OsStr;
        assert!(!no_color(None));
        assert!(!no_color(Some(OsStr::new(""))));
        assert!(no_color(Some(OsStr::new("1"))));
        // Any non-empty value counts, even one that reads as "off".
        assert!(no_color(Some(OsStr::new("0"))));
    }

    #[test]
    fn the_no_color_palette_sets_every_role_to_the_terminals_own_colour() {
        for role in ROLES {
            assert_eq!(Palette::NO_COLOR.get(role), Some(Color::Reset), "{role}");
        }
    }

    #[test]
    fn no_color_wins_over_a_theme_and_its_colour_overrides() {
        assert_eq!(startup_palette(Palette::LIGHT, true), Palette::NO_COLOR);
        let mut overridden = Palette::HIGH_CONTRAST;
        overridden.set("added", Color::Indexed(17)).unwrap();
        assert_eq!(startup_palette(overridden, true), Palette::NO_COLOR);
    }

    #[test]
    fn without_no_color_the_configured_palette_is_kept() {
        let mut overridden = Palette::LIGHT;
        overridden.set("added", Color::Indexed(17)).unwrap();
        for p in [Palette::DEFAULT, Palette::LIGHT, overridden] {
            assert_eq!(startup_palette(p, false), p);
        }
    }

    #[test]
    fn the_default_palette_draws_todays_colours() {
        let p = Palette::DEFAULT;
        let expected = [
            (p.text, Color::Reset),
            (p.dim, Color::DarkGray),
            (p.focused, Color::Cyan),
            (p.added, Color::Green),
            (p.removed, Color::Red),
            (p.modified, Color::Yellow),
            (p.status_renamed, Color::Cyan),
            (p.hunk_header, Color::Cyan),
            (p.warning, Color::Yellow),
            (p.error, Color::Red),
            (p.success, Color::Green),
            (p.pending, Color::Yellow),
            (p.neutral, Color::DarkGray),
            (p.hash, Color::Cyan),
            (p.ref_name, Color::Yellow),
            (p.ref_label, Color::Cyan),
            (p.author, Color::Green),
            (p.graph, Color::Magenta),
            (p.mark, Color::Magenta),
            (p.copied_commit, Color::Cyan),
            (p.location, Color::Cyan),
            (p.conflict_ours, Color::Green),
            (p.conflict_theirs, Color::Cyan),
            (p.thread_open, Color::Yellow),
            (p.thread_addressed, Color::Green),
            (p.pr_open, Color::Green),
            (p.pr_draft, Color::DarkGray),
            (p.pr_merged, Color::Magenta),
            (p.pr_closed, Color::Red),
            (p.match_fg, Color::Black),
            (p.match_bg, Color::Yellow),
            (p.current_match_bg, Color::LightRed),
        ];
        for (i, (got, want)) in expected.into_iter().enumerate() {
            assert_eq!(got, want, "role #{i}");
        }
        assert_eq!(Palette::default(), Palette::DEFAULT);
    }

    /// Every field, in declaration order. Destructured whole, so a new
    /// field fails to compile here until `roles!` lists it too.
    fn fields(p: &Palette) -> [Color; 32] {
        let Palette {
            text,
            dim,
            focused,
            added,
            removed,
            modified,
            status_renamed,
            hunk_header,
            warning,
            error,
            success,
            pending,
            neutral,
            hash,
            ref_name,
            ref_label,
            author,
            graph,
            mark,
            copied_commit,
            location,
            conflict_ours,
            conflict_theirs,
            thread_open,
            thread_addressed,
            pr_open,
            pr_draft,
            pr_merged,
            pr_closed,
            match_fg,
            match_bg,
            current_match_bg,
        } = *p;
        [
            text,
            dim,
            focused,
            added,
            removed,
            modified,
            status_renamed,
            hunk_header,
            warning,
            error,
            success,
            pending,
            neutral,
            hash,
            ref_name,
            ref_label,
            author,
            graph,
            mark,
            copied_commit,
            location,
            conflict_ours,
            conflict_theirs,
            thread_open,
            thread_addressed,
            pr_open,
            pr_draft,
            pr_merged,
            pr_closed,
            match_fg,
            match_bg,
            current_match_bg,
        ]
    }

    #[test]
    fn roles_set_and_get_name_every_field_in_order() {
        let colour = Color::Rgb(1, 2, 3);
        for (i, role) in ROLES.into_iter().enumerate() {
            let mut p = Palette::DEFAULT;
            assert_eq!(p.set(role, colour), Ok(()), "{role}");
            assert_eq!(p.get(role), Some(colour), "{role}");
            let mut want = fields(&Palette::DEFAULT);
            want[i] = colour;
            assert_eq!(fields(&p), want, "{role} set another field");
        }
        let mut p = Palette::DEFAULT;
        assert_eq!(p.set("addded", colour), Err(()));
        assert_eq!(p, Palette::DEFAULT);
        assert_eq!(p.get("addded"), None);
    }

    #[test]
    fn every_theme_name_gives_its_palette() {
        assert_eq!(builtin("default"), Some(Palette::DEFAULT));
        assert_eq!(builtin("light"), Some(Palette::LIGHT));
        assert_eq!(builtin("high-contrast"), Some(Palette::HIGH_CONTRAST));
        for name in THEMES {
            assert!(builtin(name).is_some(), "{name}");
        }
        for name in ["Light", "solarized", "", "high_contrast"] {
            assert_eq!(builtin(name), None, "{name}");
        }
    }

    #[test]
    fn the_light_theme_never_leans_on_the_terminal_foreground_or_weak_colours() {
        let light = Palette::LIGHT;
        for (role, colour) in [
            ("text", light.text),
            ("dim", light.dim),
            ("neutral", light.neutral),
            ("pr_draft", light.pr_draft),
        ] {
            assert!(
                !matches!(colour, Color::Reset | Color::DarkGray),
                "{role} is {colour:?}"
            );
        }
        for (role, colour) in ROLES.into_iter().zip(fields(&light)) {
            assert!(
                !matches!(
                    colour,
                    Color::Reset | Color::Yellow | Color::Cyan | Color::DarkGray
                ),
                "{role} is {colour:?}"
            );
        }
        for name in THEMES.into_iter().filter(|n| *n != "default") {
            assert_ne!(builtin(name), Some(Palette::DEFAULT), "{name}");
        }
    }

    #[test]
    fn high_contrast_brightens_every_default_colour() {
        let bright = |c: Color| match c {
            Color::Green => Color::LightGreen,
            Color::Red => Color::LightRed,
            Color::Yellow => Color::LightYellow,
            Color::Cyan => Color::LightCyan,
            Color::Magenta => Color::LightMagenta,
            Color::Reset => Color::White,
            Color::DarkGray => Color::Gray,
            other => other,
        };
        let p = Palette::HIGH_CONTRAST;
        let skip = ["match_fg", "match_bg", "current_match_bg"];
        for ((role, default), got) in ROLES
            .into_iter()
            .zip(fields(&Palette::DEFAULT))
            .zip(fields(&p))
        {
            if !skip.contains(&role) {
                assert_eq!(got, bright(default), "{role}");
            }
        }
        assert_eq!(
            (p.match_fg, p.match_bg, p.current_match_bg),
            (Color::Black, Color::LightYellow, Color::LightRed)
        );
    }

    fn text(s: &str) -> ColourValue {
        ColourValue::Text(s.into())
    }

    #[test]
    fn a_colour_is_a_name_an_index_or_hex() {
        for (name, colour) in NAMES {
            assert_eq!(parse_colour(&text(name)), Ok(colour), "{name}");
        }
        assert_eq!(NAMES.len(), 16);
        assert_eq!(parse_colour(&text("dark_gray")), Ok(Color::DarkGray));
        assert_eq!(parse_colour(&text("light_red")), Ok(Color::LightRed));
        assert_eq!(parse_colour(&ColourValue::Index(0)), Ok(Color::Indexed(0)));
        assert_eq!(
            parse_colour(&ColourValue::Index(255)),
            Ok(Color::Indexed(255))
        );
        assert_eq!(parse_colour(&text("17")), Ok(Color::Indexed(17)));
        assert_eq!(parse_colour(&text("reset")), Ok(Color::Reset));
        assert_eq!(
            parse_colour(&text("#3a3a3a")),
            Ok(Color::Rgb(0x3a, 0x3a, 0x3a))
        );
        assert_eq!(
            parse_colour(&text("#AbCdEf")),
            Ok(Color::Rgb(0xab, 0xcd, 0xef))
        );
    }

    #[test]
    fn any_other_spelling_is_refused_with_what_to_use() {
        let hint = r##"is not a colour; use a colour name such as "green" or "light_red", "reset", a number 0-255, or "#rrggbb""##;
        let refused = [
            (text("grene"), r#""grene""#),
            (text("Green"), r#""Green""#),
            (text("Reset"), r#""Reset""#),
            (text("RESET"), r#""RESET""#),
            (text("default"), r#""default""#),
            (text("none"), r#""none""#),
            (text("darkgray"), r#""darkgray""#),
            (text("256"), r#""256""#),
            (text("-1"), r#""-1""#),
            (text("+1"), r#""+1""#),
            (text(" 1"), r#"" 1""#),
            (text(""), r#""""#),
            (text("#fff"), r##""#fff""##),
            (text("#12345g"), r##""#12345g""##),
            (text("#1234567"), r##""#1234567""##),
            (text("rgb(1,2,3)"), r#""rgb(1,2,3)""#),
            (ColourValue::Index(256), "256"),
            (ColourValue::Index(-1), "-1"),
            (ColourValue::Other(toml::Value::Boolean(true)), "true"),
        ];
        for (value, shown) in refused {
            assert_eq!(
                parse_colour(&value),
                Err(format!("{shown} {hint}")),
                "{value:?}"
            );
        }
    }

    /// Files that may name terminal colours outside their tests: this
    /// palette, and the pager's own SGR codes, which are diff content.
    const EXEMPT: [&str; 2] = ["theme.rs", "features/diff/renderers/ansi.rs"];

    fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read src") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// The folder a file's child modules live in.
    fn module_dir(path: &Path) -> PathBuf {
        let parent = path.parent().expect("in a folder");
        match path.file_stem().and_then(|s| s.to_str()) {
            Some("main" | "lib" | "mod") => parent.to_path_buf(),
            Some(stem) => parent.join(stem),
            None => parent.to_path_buf(),
        }
    }

    /// The name in a `mod name;` or `mod name {` line, and whether it is
    /// inline.
    fn module_decl(line: &str) -> Option<(&str, bool)> {
        let rest = line.trim_start();
        let rest = rest.strip_prefix("pub(crate) ").unwrap_or(rest);
        let rest = rest.strip_prefix("pub ").unwrap_or(rest);
        let rest = rest.strip_prefix("mod ")?;
        let name_end = rest.find(|c: char| !(c.is_alphanumeric() || c == '_'))?;
        let (name, tail) = rest.split_at(name_end);
        match tail.trim() {
            ";" => Some((name, false)),
            "{" => Some((name, true)),
            _ => None,
        }
    }

    /// The non-test lines of a file, numbered from 1: everything outside
    /// inline `#[cfg(test)] mod … { }` blocks, which run to the file's end
    /// by convention. Also returns the out-of-line modules declared under
    /// `#[cfg(test)]`, which are test code as whole files.
    fn non_test_lines(text: &str) -> (Vec<(usize, &str)>, Vec<String>) {
        let mut lines = Vec::new();
        let mut test_mods = Vec::new();
        let mut after_cfg_test = false;
        for (n, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if after_cfg_test {
                if trimmed.starts_with("#[") {
                    continue;
                }
                after_cfg_test = false;
                match module_decl(line) {
                    Some((_, true)) => break,
                    Some((name, false)) => {
                        test_mods.push(name.to_string());
                        continue;
                    }
                    None => {}
                }
            }
            if trimmed.starts_with("#[cfg(test)]") {
                after_cfg_test = true;
                continue;
            }
            lines.push((n + 1, line));
        }
        (lines, test_mods)
    }

    #[test]
    fn no_view_names_a_terminal_colour() {
        let src = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
        let mut files = Vec::new();
        rust_files(src, &mut files);
        let mut checked = Vec::new();
        let mut test_dirs = Vec::new();
        for path in &files {
            let text = std::fs::read_to_string(path).expect("read file");
            let (lines, test_mods) = non_test_lines(&text);
            let dir = module_dir(path);
            test_dirs.extend(test_mods.iter().map(|m| dir.join(m)));
            let hits: Vec<(usize, String)> = lines
                .into_iter()
                .filter(|(_, line)| line.contains("Color::"))
                .map(|(n, line)| (n, line.trim().to_string()))
                .collect();
            checked.push((path.clone(), hits));
        }
        let is_test_file = |path: &Path| {
            test_dirs
                .iter()
                .any(|dir| path.starts_with(dir) || path == dir.with_extension("rs"))
        };
        let mut found = Vec::new();
        for (path, hits) in checked {
            let relative = path
                .strip_prefix(src)
                .expect("under src")
                .to_string_lossy()
                .replace('\\', "/");
            if EXEMPT.contains(&relative.as_str()) || is_test_file(&path) {
                continue;
            }
            for (n, line) in hits {
                found.push(format!("src/{relative}:{n}: {line}"));
            }
        }
        assert!(
            found.is_empty(),
            "views must take colours from the Palette (src/theme.rs):\n{}",
            found.join("\n")
        );
    }

    #[test]
    fn the_colour_check_skips_only_test_code() {
        let text = "use x;\n#[cfg(test)]\nmod helpers;\nlet a = Color::Red;\n#[cfg(test)]\n#[allow(dead_code)]\nfn f() {}\n#[cfg(test)]\nmod tests {\n    Color::Blue\n}\n";
        let (lines, test_mods) = non_test_lines(text);
        assert_eq!(test_mods, ["helpers"]);
        let kept: Vec<usize> = lines.iter().map(|(n, _)| *n).collect();
        assert_eq!(kept, [1, 4, 7]);
    }
}
