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

fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

/// `text` as lines at most `width` columns wide, except fenced code,
/// which is kept as written.
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
                out.push(Line::from(Span::styled(format!("{INDENT}{raw}"), dim())));
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
            let depth = (line.len() - trimmed.len()) / 2;
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
/// `text (url)`.
fn inline(text: &str, base: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut plain = String::new();
    let mut rest = text;
    let flush = |plain: &mut String, spans: &mut Vec<Span<'static>>| {
        if !plain.is_empty() {
            spans.push(Span::styled(std::mem::take(plain), base));
        }
    };
    while let Some(c) = rest.chars().next() {
        let styled = match c {
            '`' => rest[1..].find('`').map(|end| {
                let code = &rest[1..1 + end];
                (
                    code.to_string(),
                    base.add_modifier(Modifier::REVERSED),
                    2 + end,
                )
            }),
            '*' if rest.starts_with("**") => rest[2..].find("**").filter(|&e| e > 0).map(|end| {
                let inner = &rest[2..2 + end];
                (
                    inner.to_string(),
                    base.add_modifier(Modifier::BOLD),
                    4 + end,
                )
            }),
            '_' if word_start(text, rest) => closing_underscore(&rest[1..]).map(|end| {
                let inner = &rest[1..1 + end];
                (
                    inner.to_string(),
                    base.add_modifier(Modifier::ITALIC),
                    2 + end,
                )
            }),
            '[' => link(rest).map(|(label, url, len)| (format!("{label} ({url})"), base, len)),
            _ => None,
        };
        match styled {
            Some((content, style, len)) => {
                flush(&mut plain, &mut spans);
                spans.push(Span::styled(content, style));
                rest = &rest[len..];
            }
            None => {
                plain.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    flush(&mut plain, &mut spans);
    spans
}

/// Whether `rest`, a tail of `text`, starts a word: emphasis inside a
/// word, as in `snake_case_name`, is not emphasis.
fn word_start(text: &str, rest: &str) -> bool {
    let before = &text[..text.len() - rest.len()];
    before
        .chars()
        .next_back()
        .is_none_or(|c| !c.is_alphanumeric())
}

/// Where the `_` that closes emphasis is in `after`: not first, and not
/// followed by a letter or digit.
fn closing_underscore(after: &str) -> Option<usize> {
    after.char_indices().find_map(|(i, c)| {
        let next = after[i + 1..].chars().next();
        (c == '_' && i > 0 && next.is_none_or(|n| !n.is_alphanumeric())).then_some(i)
    })
}

/// `[label](url)` at the start of `rest`: its label, its url and its length.
fn link(rest: &str) -> Option<(&str, &str, usize)> {
    let close = rest.find("](")?;
    let label = &rest[1..close];
    if label.contains('[') {
        return None;
    }
    let after = &rest[close + 2..];
    let end = after.find(')')?;
    let url = &after[..end];
    if url.is_empty() || url.contains(' ') {
        return None;
    }
    Some((label, url, close + 2 + end + 1))
}

/// Word-wrap `spans` into lines of at most `width` columns, the first
/// starting with `first` and the rest with `hang`. A word longer than a
/// line is cut.
fn wrap(spans: Vec<Span<'static>>, width: usize, first: &str, hang: &str) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<Span<'static>> = vec![Span::raw(first.to_string())];
    let mut used = Span::raw(first).width();
    let room = |prefix: &str| width.saturating_sub(Span::raw(prefix).width()).max(1);
    let mut pending_space = false;
    for word in words(spans) {
        let Some((text, style)) = word else {
            pending_space = true;
            continue;
        };
        let mut text = text;
        loop {
            let w = Span::raw(text.as_str()).width();
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
                used = Span::raw(hang).width();
                pending_space = false;
                continue;
            }
            // Longer than a whole line: cut it.
            let (head, tail) = split_at_width(&text, room(hang).min(width - used));
            current.push(Span::styled(head, style));
            lines.push(Line::from(std::mem::take(&mut current)));
            current = vec![Span::raw(hang.to_string())];
            used = Span::raw(hang).width();
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

/// `text` cut after at most `cols` columns, and what is left.
fn split_at_width(text: &str, cols: usize) -> (String, String) {
    let mut used = 0;
    for (i, c) in text.char_indices() {
        let w = Span::raw(c.to_string()).width();
        if used + w > cols && i > 0 {
            return (text[..i].to_string(), text[i..].to_string());
        }
        used += w;
    }
    (text.to_string(), String::new())
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
    fn fenced_code_is_kept_indented_dim_and_unwrapped() {
        let long = "let x = a_very_long_call(with, many, arguments, past, forty);";
        let lines = render(&format!("```rust\n{long}\n  **not bold**\n```\nafter"), 40);
        assert_eq!(
            texts(&lines),
            [
                format!("  {long}"),
                "    **not bold**".into(),
                "after".into()
            ]
        );
        assert!(lines[0].spans[0].style.add_modifier.contains(Modifier::DIM));
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
}
