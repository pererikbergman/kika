//! The colours views draw with, named by what they mean.
//!
//! A role names what a colour means (an added line, a warning, a pending
//! check), not which colour it is, so a theme can swap colours without
//! touching any view. Every view reads its colours from the [`Palette`] in
//! `AppState`; only this module names terminal colours. Colour is an extra
//! signal only: symbols and text carry the meaning in every palette.

use ratatui::style::Color;

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
    pub(crate) renamed: Color,
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
    pub(crate) copied: Color,
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
    /// The text of a search match.
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
        renamed: Color::Cyan,
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
        copied: Color::Cyan,
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
    fn the_default_palette_draws_todays_colours() {
        let p = Palette::DEFAULT;
        let expected = [
            (p.text, Color::Reset),
            (p.dim, Color::DarkGray),
            (p.focused, Color::Cyan),
            (p.added, Color::Green),
            (p.removed, Color::Red),
            (p.modified, Color::Yellow),
            (p.renamed, Color::Cyan),
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
            (p.copied, Color::Cyan),
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
