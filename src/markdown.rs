//! A small markdown renderer for pull request descriptions: enough to
//! read one in a terminal, not a full CommonMark parser. Pure; the text
//! comes in already terminal-safe. Syntax it does not know is shown as
//! written.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

/// How a quote's bar is drawn, before the quoted text.
const QUOTE_BAR: &str = "│ ";
/// How deep code and nested lists are indented per level.
const INDENT: &str = "  ";
/// The deepest a nested list is drawn.
const MAX_DEPTH: usize = 8;

fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

/// `text` as lines at most `width` columns wide (at least 8). Fenced code
/// is not wrapped: a line too wide is cut and ends in a dim `…`.
pub fn render(text: &str, width: usize) -> Vec<Line<'static>> {
    let width = width.max(8);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut fence: Option<&str> = None;
    let mut comment = false;
    for raw in text.split('\n') {
        if let Some(marker) = fence {
            if raw.trim_start().starts_with(marker) {
                fence = None;
            } else {
                out.push(code_line(raw, width));
            }
            continue;
        }
        let line = strip_comments(raw, &mut comment);
        let Some(line) = line else { continue };
        let trimmed = line.trim_start();
        if let Some(marker) = ["```", "~~~"].into_iter().find(|m| trimmed.starts_with(m)) {
            fence = Some(marker);
            continue;
        }
        if trimmed.is_empty() {
            // One blank line between blocks is enough.
            if out.last().is_some_and(|l| l.width() > 0) {
                out.push(Line::default());
            }
            continue;
        }
        if let Some(heading) = heading(trimmed) {
            let spans = inline(heading, bold());
            out.extend(wrap(spans, width, "", ""));
        } else if let Some(quoted) = trimmed.strip_prefix('>') {
            let quoted = quoted.strip_prefix(' ').unwrap_or(quoted);
            let spans = inline(quoted, Style::new());
            out.extend(
                wrap(spans, width, QUOTE_BAR, QUOTE_BAR)
                    .into_iter()
                    .map(dim_bar),
            );
        } else if let Some((marker, rest)) = list_item(trimmed) {
            // Depth comes from untrusted indentation: capped, and kept to
            // half the width so the text always has room.
            let marker_cols = width_of(&marker) + 1;
            let mut depth = (indent_cols(&line) / 2).min(MAX_DEPTH);
            while depth > 0 && depth * INDENT.len() + marker_cols > width / 2 {
                depth -= 1;
            }
            let first = format!("{}{marker} ", INDENT.repeat(depth));
            let hang = " ".repeat(first.chars().count());
            out.extend(wrap(inline(rest, Style::new()), width, &first, &hang));
        } else {
            out.extend(wrap(inline(trimmed, Style::new()), width, "", ""));
        }
    }
    while out.last().is_some_and(|l| l.width() == 0) {
        out.pop();
    }
    out
}

/// The line with HTML comments taken out; `None` when nothing is left of
/// a line that held one. `comment` carries an open comment to the next line.
fn strip_comments(raw: &str, comment: &mut bool) -> Option<String> {
    let mut kept = String::new();
    let mut rest = raw;
    let mut had = false;
    loop {
        if *comment {
            had = true;
            match rest.find("-->") {
                Some(end) => {
                    rest = &rest[end + 3..];
                    *comment = false;
                }
                None => break,
            }
        } else {
            match rest.find("<!--") {
                Some(start) => {
                    kept.push_str(&rest[..start]);
                    rest = &rest[start + 4..];
                    *comment = true;
                }
                None => {
                    kept.push_str(rest);
                    break;
                }
            }
        }
    }
    if had && kept.trim().is_empty() {
        None
    } else {
        Some(kept)
    }
}

/// An ATX heading's text: one to six `#` and a space.
fn heading(line: &str) -> Option<&str> {
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = &line[hashes..];
    if rest.is_empty() {
        return Some("");
    }
    let text = rest.strip_prefix(' ')?.trim();
    // A closing run of `#` is not part of the text.
    Some(text.trim_end_matches('#').trim_end())
}

/// A list item's marker as drawn, and its text: `-`, `*` or `+` become a
/// bullet; `1.` stays as written.
fn list_item(line: &str) -> Option<(String, &str)> {
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(bullet) {
            return Some(("•".to_string(), rest));
        }
    }
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    if (1..=9).contains(&digits) {
        let rest = &line[digits..];
        if let Some(text) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            return Some((line[..digits + 1].to_string(), text));
        }
    }
    None
}

/// The bar of a quote line in the dim style, the text as it was.
fn dim_bar(line: Line<'static>) -> Line<'static> {
    let mut spans = line.spans;
    if let Some(first) = spans.first_mut()
        && first.content == QUOTE_BAR
    {
        first.style = dim();
    }
    Line::from(spans)
}

/// `text`'s inline markup as styled spans, on top of `base`: code spans,
/// `**bold**` and `_em_` lose their markers, and `[text](url)` reads
/// `text (url)`. One pass over the chars: each closing marker is looked
/// up through a [`Finder`], so a line full of openers costs no more than
/// its length.
fn inline(text: &str, base: Style) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let mut finders = Finders::default();
    let mut spans = Vec::new();
    let mut plain = String::new();
    let mut i = 0;
    while i < chars.len() {
        match finders.markup(&chars, i, base) {
            Some((content, style, next)) => {
                if !plain.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut plain), base));
                }
                spans.push(Span::styled(content, style));
                i = next;
            }
            None => {
                plain.push(chars[i]);
                i += 1;
            }
        }
    }
    if !plain.is_empty() {
        spans.push(Span::styled(plain, base));
    }
    spans
}

/// The next place at or after a start where a fixed test holds, cached.
/// Starts only grow during a pass, so each char is tested at most once:
/// a found place answers every start up to it, and "none from here"
/// answers every later start.
#[derive(Default)]
struct Finder {
    /// The start of the last search, and what it found.
    last: Option<(usize, Option<usize>)>,
}

impl Finder {
    fn find(
        &mut self,
        chars: &[char],
        start: usize,
        test: impl Fn(&[char], usize) -> bool,
    ) -> Option<usize> {
        if let Some((from, found)) = self.last
            && start >= from
        {
            match found {
                Some(at) if at >= start => return Some(at),
                None => return None,
                Some(_) => {}
            }
        }
        let found = (start..chars.len()).find(|&k| test(chars, k));
        self.last = Some((start, found));
        found
    }
}

/// One finder per marker the inline pass closes.
#[derive(Default)]
struct Finders {
    tick: Finder,
    stars: Finder,
    underscore: Finder,
    label_end: Finder,
    bracket: Finder,
    paren: Finder,
    space: Finder,
}

impl Finders {
    /// The markup starting at `chars[i]`: its shown text, its style and
    /// the index after it. `None` when `chars[i]` is just a character.
    fn markup(&mut self, chars: &[char], i: usize, base: Style) -> Option<(String, Style, usize)> {
        let text = |from: usize, to: usize| chars[from..to].iter().collect::<String>();
        match chars[i] {
            '`' => {
                let end = self.tick.find(chars, i + 1, |cs, k| cs[k] == '`')?;
                Some((
                    text(i + 1, end),
                    base.add_modifier(Modifier::REVERSED),
                    end + 1,
                ))
            }
            '*' if chars.get(i + 1) == Some(&'*') => {
                let end = self.stars.find(chars, i + 2, |cs, k| {
                    cs[k] == '*' && cs.get(k + 1) == Some(&'*')
                })?;
                (end > i + 2)
                    .then(|| (text(i + 2, end), base.add_modifier(Modifier::BOLD), end + 2))
            }
            // Inside a word, as in `snake_case_name`, it is not emphasis.
            '_' if i == 0 || !chars[i - 1].is_alphanumeric() => {
                let end = self.underscore.find(chars, i + 2, |cs, k| {
                    cs[k] == '_' && cs.get(k + 1).is_none_or(|n| !n.is_alphanumeric())
                })?;
                Some((
                    text(i + 1, end),
                    base.add_modifier(Modifier::ITALIC),
                    end + 1,
                ))
            }
            '[' => {
                let close = self.label_end.find(chars, i + 1, |cs, k| {
                    cs[k] == ']' && cs.get(k + 1) == Some(&'(')
                })?;
                let nested = self.bracket.find(chars, i + 1, |cs, k| cs[k] == '[');
                if nested.is_some_and(|at| at < close) {
                    return None;
                }
                let end = self.paren.find(chars, close + 2, |cs, k| cs[k] == ')')?;
                let spaced = self.space.find(chars, close + 2, |cs, k| cs[k] == ' ');
                if end == close + 2 || spaced.is_some_and(|at| at < end) {
                    return None;
                }
                let shown = format!("{} ({})", text(i + 1, close), text(close + 2, end));
                Some((shown, base, end + 1))
            }
            _ => None,
        }
    }
}

/// Word-wrap `spans` into lines of at most `width` columns, the first
/// starting with `first` and the rest with `hang`. A word longer than a
/// line is cut.
fn wrap(spans: Vec<Span<'static>>, width: usize, first: &str, hang: &str) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<Span<'static>> = vec![Span::raw(first.to_string())];
    let mut used = width_of(first);
    let mut pending_space = false;
    for word in words(spans) {
        let Some((text, style)) = word else {
            pending_space = true;
            continue;
        };
        let mut text = text;
        loop {
            let w = width_of(&text);
            let space = usize::from(pending_space && used > prefix_width(&current));
            if used + space + w <= width {
                if space == 1 {
                    current.push(Span::raw(" "));
                }
                used += space + w;
                current.push(Span::styled(text, style));
                break;
            }
            if used > prefix_width(&current) {
                lines.push(Line::from(std::mem::take(&mut current)));
                current = vec![Span::raw(hang.to_string())];
                used = width_of(hang);
                pending_space = false;
                continue;
            }
            // Longer than a whole line: cut it, at least a char a line.
            let (head, tail) = split_at_width(&text, width.saturating_sub(used).max(1));
            current.push(Span::styled(head, style));
            lines.push(Line::from(std::mem::take(&mut current)));
            current = vec![Span::raw(hang.to_string())];
            used = width_of(hang);
            text = tail;
            if text.is_empty() {
                break;
            }
        }
        pending_space = false;
    }
    if used > prefix_width(&current) || lines.is_empty() {
        lines.push(Line::from(current));
    }
    lines
}

/// `text` in `style`, word-wrapped to `width`, for one-line fields that
/// may be long.
pub fn wrap_text(text: &str, style: Style, width: usize) -> Vec<Line<'static>> {
    wrap(
        vec![Span::styled(text.to_string(), style)],
        width.max(1),
        "",
        "",
    )
}

fn width_of(text: &str) -> usize {
    Span::raw(text).width()
}

fn prefix_width(current: &[Span<'_>]) -> usize {
    current.first().map_or(0, Span::width)
}

/// The words of `spans`, each with its style; `None` stands for space.
fn words(spans: Vec<Span<'static>>) -> Vec<Option<(String, Style)>> {
    let mut out = Vec::new();
    for span in spans {
        let style = span.style;
        let mut word = String::new();
        for c in span.content.chars() {
            if c == ' ' {
                if !word.is_empty() {
                    out.push(Some((std::mem::take(&mut word), style)));
                }
                out.push(None);
            } else {
                word.push(c);
            }
        }
        if !word.is_empty() {
            out.push(Some((word, style)));
        }
    }
    out
}

/// `text` cut after at most `cols` columns (at least one char), and what
/// is left.
fn split_at_width(text: &str, cols: usize) -> (String, String) {
    let mut used = 0;
    for (i, c) in text.char_indices() {
        let w = width_of(c.encode_utf8(&mut [0; 4]));
        if used + w > cols && i > 0 {
            return (text[..i].to_string(), text[i..].to_string());
        }
        used += w;
    }
    (text.to_string(), String::new())
}

/// A fenced code line, indented and dim, cut to `width` with a dim `…`
/// when it does not fit, so the reader knows text is missing.
fn code_line(raw: &str, width: usize) -> Line<'static> {
    let text = format!("{INDENT}{raw}");
    if width_of(&text) <= width {
        return Line::from(Span::styled(text, dim()));
    }
    let (head, _) = split_at_width(&text, width.saturating_sub(1).max(1));
    Line::from(vec![Span::styled(head, dim()), Span::styled("…", dim())])
}

/// Columns of indentation before `line`'s text, a tab counting four.
fn indent_cols(line: &str) -> usize {
    line.chars()
        .take_while(|c| c.is_whitespace())
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn texts(lines: &[Line]) -> Vec<String> {
        lines.iter().map(text).collect()
    }

    fn span<'a>(line: &'a Line, content: &str) -> &'a Span<'a> {
        line.spans
            .iter()
            .find(|s| s.content == content)
            .unwrap_or_else(|| panic!("no span {content:?} in {line:?}"))
    }

    #[test]
    fn a_heading_is_bold_without_its_hashes() {
        let lines = render("## Summary ##", 40);
        assert_eq!(texts(&lines), ["Summary"]);
        assert!(
            span(&lines[0], "Summary")
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        // No space after the hashes: not a heading.
        assert_eq!(texts(&render("#3 is fixed", 40)), ["#3 is fixed"]);
    }

    #[test]
    fn a_nested_list_hangs_its_wrapped_lines_under_the_text() {
        let md = "- first item\n  - nested item that runs on long enough to wrap\n1. one";
        let lines = render(md, 40);
        assert_eq!(
            texts(&lines),
            [
                "• first item",
                "  • nested item that runs on long enough",
                "    to wrap",
                "1. one",
            ]
        );
    }

    #[test]
    fn a_quote_has_a_bar_on_each_line() {
        let lines = render("> Note: needs a migration and a very long tail to wrap", 40);
        assert_eq!(
            texts(&lines),
            [
                "│ Note: needs a migration and a very",
                "│ long tail to wrap"
            ]
        );
        assert!(lines[1].spans[0].style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn fenced_code_is_kept_indented_dim_and_cut_with_a_mark() {
        let long = "let x = a_very_long_call(with, many, arguments, past, forty);";
        let lines = render(&format!("```rust\n{long}\n  **not bold**\n```\nafter"), 40);
        assert_eq!(
            texts(&lines),
            [
                format!("  {}…", &long[..37]),
                "    **not bold**".into(),
                "after".into()
            ]
        );
        assert_eq!(lines[0].width(), 40);
        for span in &lines[0].spans {
            assert!(span.style.add_modifier.contains(Modifier::DIM), "{span:?}");
        }
    }

    #[test]
    fn inline_markup_loses_its_markers() {
        let lines = render(
            "Run `cargo test` for **all** of _it_, see [docs](https://x.io/d)",
            80,
        );
        assert_eq!(
            texts(&lines),
            ["Run cargo test for all of it, see docs (https://x.io/d)"]
        );
        let line = &lines[0];
        assert!(
            span(line, "cargo")
                .style
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        assert!(
            span(line, "all")
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert!(
            span(line, "it")
                .style
                .add_modifier
                .contains(Modifier::ITALIC)
        );
        // Inside a word, an underscore is just a character.
        assert_eq!(texts(&render("snake_case_name", 40)), ["snake_case_name"]);
    }

    #[test]
    fn html_comments_are_dropped_even_over_lines() {
        let md = "a <!-- hidden --> b\n<!-- one\ntwo -->\nc";
        assert_eq!(texts(&render(md, 40)), ["a b", "c"]);
    }

    #[test]
    fn long_paragraphs_wrap_and_no_line_is_wider_than_the_width() {
        let md = "## A heading that is long enough to wrap at forty\n\n\
                  Some paragraph text that goes on and on, well past forty columns, \
                  with a supercalifragilisticexpialidociouslylongword inside.\n\n\
                  - a list item that also goes on past the edge of the pane\n\
                  > a quote that also goes on past the edge of the pane";
        let lines = render(md, 40);
        assert!(lines.len() > 6, "{:#?}", texts(&lines));
        for line in &lines {
            assert!(
                line.width() <= 40,
                "{:?} is {} wide",
                text(line),
                line.width()
            );
        }
    }

    #[test]
    fn unknown_syntax_passes_through_as_text() {
        let md = "| a | b |\n|---|---|\n<img src=x>\n[not a link] (x)";
        assert_eq!(
            texts(&render(md, 40)),
            ["| a | b |", "|---|---|", "<img src=x>", "[not a link] (x)"]
        );
    }

    #[test]
    fn markers_next_to_multibyte_text_never_split_a_char() {
        for md in [
            "see _é_ here",
            "_über_ and _日本",
            "a _e\u{301}_ b",
            "_👍_ _👍",
            "**é** **日本**x",
            "`é` `日",
            "[é](日) [日本](https://x.io/é) [é",
            "_",
            "é_",
            "**",
            "[](",
        ] {
            let lines = render(md, 40);
            assert!(!lines.is_empty() || md.trim().is_empty(), "{md:?}");
        }
        assert_eq!(texts(&render("see _é_ here", 40)), ["see é here"]);
    }

    /// Random text from markup characters and multibyte, combining and
    /// wide ones renders without a panic and within the width.
    #[test]
    fn random_unicode_renders_within_the_width() {
        const ALPHABET: &[char] = &[
            '_', '*', '`', '[', ']', '(', ')', '#', '>', '-', '1', '.', ' ', ' ', '\n', '\t', 'a',
            'é', '\u{301}', '日', '👍', '\u{200D}', '<', '!', '~', 'x', ':',
        ];
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..500 {
            let len = (next() % 120) as usize;
            let text: String = (0..len)
                .map(|_| ALPHABET[(next() % ALPHABET.len() as u64) as usize])
                .collect();
            let width = 8 + (next() % 50) as usize;
            for line in render(&text, width) {
                assert!(line.width() <= width, "{text:?} at {width}: {line:?}");
            }
        }
    }

    #[test]
    fn deep_indentation_is_capped_and_stays_within_the_width() {
        let md = format!("{}- {}", " ".repeat(200), "y".repeat(300));
        for width in [8, 40] {
            let lines = render(&md, width);
            for line in &lines {
                assert!(line.width() <= width, "{width}: {:?}", text(line));
            }
            assert!(text(&lines[0]).contains('•'), "{:?}", texts(&lines));
        }
        let lines = render(&format!("{}- deep", " ".repeat(200)), 80);
        assert_eq!(texts(&lines), [format!("{}• deep", "  ".repeat(MAX_DEPTH))]);
    }

    #[test]
    fn tab_indentation_nests_a_list() {
        let lines = render("- top\n\t- under a tab\n\t\t- two tabs", 40);
        assert_eq!(
            texts(&lines),
            ["• top", "    • under a tab", "        • two tabs"]
        );
    }

    /// Lines of openers that never close, at the 64 KiB description cap,
    /// render in linear time.
    #[test]
    fn hostile_descriptions_render_within_a_time_budget() {
        let size = 64 * 1024;
        for unit in ["[", "_a ", "**a ", "`", "[a](", "[x]( ", "*"] {
            let text = unit.repeat(size / unit.len());
            let start = std::time::Instant::now();
            render(&text, 78);
            let took = start.elapsed();
            assert!(
                took < std::time::Duration::from_secs(2),
                "{unit:?} took {took:?}"
            );
        }
    }
}
