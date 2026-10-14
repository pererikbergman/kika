//! Turns a diff renderer's ANSI output into styled rows that are safe to
//! draw. Only SGR colour and attribute sequences are honoured; every other
//! escape is consumed and dropped, and the text left over goes through the
//! same body-text sanitiser as diff lines, so renderer bytes never drive the
//! terminal. Headers and body cannot be told apart here, so all of it is body.

use ratatui::style::{Color, Modifier, Style};

use kika_git::diff::{EmojiZwj, sanitize_body};

/// One line of renderer output: styled runs of terminal-safe text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StyledRow {
    pub runs: Vec<(Style, String)>,
}

impl StyledRow {
    /// The row's text without its styles.
    pub fn text(&self) -> String {
        self.runs.iter().map(|(_, text)| text.as_str()).collect()
    }
}

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;

/// The most rows kept. Output of mostly newlines would otherwise cost a
/// row, and a map entry, per byte.
pub const MAX_ROWS: usize = 200_000;

/// Split `bytes` into rows on `\n`, dropping a trailing `\r`, and keep only
/// the styles of SGR sequences. Rows past [`MAX_ROWS`] are dropped.
pub fn parse(bytes: &[u8]) -> Vec<StyledRow> {
    parse_capped(bytes, MAX_ROWS)
}

fn parse_capped(bytes: &[u8], max_rows: usize) -> Vec<StyledRow> {
    let mut parser = Parser::default();
    let mut at = 0;
    while at < bytes.len() && parser.rows.len() < max_rows {
        match bytes[at] {
            ESC => at = parser.escape(bytes, at),
            b'\n' => {
                parser.end_row();
                at += 1;
            }
            byte => {
                parser.text.push(byte);
                at += 1;
            }
        }
    }
    if parser.rows.len() < max_rows && (!parser.text.is_empty() || !parser.row.runs.is_empty()) {
        parser.end_row();
    }
    parser.rows
}

#[derive(Default)]
struct Parser {
    rows: Vec<StyledRow>,
    row: StyledRow,
    style: Style,
    /// Raw bytes of the run being built, in `style`.
    text: Vec<u8>,
}

impl Parser {
    fn flush(&mut self) {
        if !self.text.is_empty() {
            let text = sanitize_body(&std::mem::take(&mut self.text), EmojiZwj::Escape);
            match self.row.runs.last_mut() {
                Some((style, run)) if *style == self.style => run.push_str(&text),
                _ => self.row.runs.push((self.style, text)),
            }
        }
    }

    fn end_row(&mut self) {
        if self.text.last() == Some(&b'\r') {
            self.text.pop();
        }
        self.flush();
        self.rows.push(std::mem::take(&mut self.row));
    }

    /// Consume the escape at `bytes[at]` and return where text resumes.
    fn escape(&mut self, bytes: &[u8], at: usize) -> usize {
        let Some(&kind) = bytes.get(at + 1) else {
            return at + 1;
        };
        match kind {
            b'[' => self.csi(bytes, at + 2),
            // OSC, DCS, SOS, PM and APC: a string up to BEL or ST.
            b']' | b'P' | b'X' | b'^' | b'_' => skip_string(bytes, at + 2),
            // nF: intermediates, then one final byte.
            0x20..=0x2f => {
                let mut end = at + 1;
                while bytes.get(end).is_some_and(|b| (0x20..=0x2f).contains(b)) {
                    end += 1;
                }
                if bytes.get(end).is_some_and(|b| (0x30..=0x7e).contains(b)) {
                    end += 1;
                }
                end
            }
            // Fp, Fe and Fs: one byte after ESC.
            0x30..=0x7e => at + 2,
            // A lone ESC.
            _ => at + 1,
        }
    }

    /// A CSI sequence whose parameters start at `start`. Only `m` with
    /// digit and `;` parameters changes anything.
    fn csi(&mut self, bytes: &[u8], start: usize) -> usize {
        let mut end = start;
        while bytes.get(end).is_some_and(|b| (0x30..=0x3f).contains(b)) {
            end += 1;
        }
        let params = &bytes[start..end];
        while bytes.get(end).is_some_and(|b| (0x20..=0x2f).contains(b)) {
            end += 1;
        }
        match bytes.get(end) {
            Some(b'm') if end == start + params.len() => {
                if params.iter().all(|b| b.is_ascii_digit() || *b == b';') {
                    self.flush();
                    self.style = sgr(self.style, params);
                }
                end + 1
            }
            Some(0x40..=0x7e) => end + 1,
            // Cut short: drop what was read and go on with the next byte.
            _ => end,
        }
    }
}

/// Skip a control string's body up to and including BEL or ST. One that
/// never ends takes the rest of the output with it.
fn skip_string(bytes: &[u8], start: usize) -> usize {
    let mut at = start;
    while at < bytes.len() {
        match bytes[at] {
            BEL => return at + 1,
            ESC if bytes.get(at + 1) == Some(&b'\\') => return at + 2,
            _ => at += 1,
        }
    }
    at
}

/// `style` after the SGR parameters `params`. Unknown ones are ignored.
fn sgr(mut style: Style, params: &[u8]) -> Style {
    let codes: Vec<u32> = params
        .split(|&b| b == b';')
        .map(|p| {
            std::str::from_utf8(p)
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0)
        })
        .collect();
    let mut codes = codes.into_iter();
    while let Some(code) = codes.next() {
        match code {
            0 => style = Style::default(),
            1 => style.add_modifier.insert(Modifier::BOLD),
            2 => style.add_modifier.insert(Modifier::DIM),
            3 => style.add_modifier.insert(Modifier::ITALIC),
            4 => style.add_modifier.insert(Modifier::UNDERLINED),
            7 => style.add_modifier.insert(Modifier::REVERSED),
            9 => style.add_modifier.insert(Modifier::CROSSED_OUT),
            22 => style.add_modifier.remove(Modifier::BOLD | Modifier::DIM),
            23 => style.add_modifier.remove(Modifier::ITALIC),
            24 => style.add_modifier.remove(Modifier::UNDERLINED),
            25 => style
                .add_modifier
                .remove(Modifier::SLOW_BLINK | Modifier::RAPID_BLINK),
            27 => style.add_modifier.remove(Modifier::REVERSED),
            28 => style.add_modifier.remove(Modifier::HIDDEN),
            29 => style.add_modifier.remove(Modifier::CROSSED_OUT),
            30..=37 => style.fg = Some(basic(code - 30)),
            39 => style.fg = None,
            40..=47 => style.bg = Some(basic(code - 40)),
            49 => style.bg = None,
            90..=97 => style.fg = Some(basic(code - 90 + 8)),
            100..=107 => style.bg = Some(basic(code - 100 + 8)),
            38 => {
                if let Some(color) = extended(&mut codes) {
                    style.fg = Some(color);
                }
            }
            48 => {
                if let Some(color) = extended(&mut codes) {
                    style.bg = Some(color);
                }
            }
            _ => {}
        }
    }
    style
}

/// The colour after a `38` or `48`: `5;n` or `2;r;g;b`.
fn extended(codes: &mut impl Iterator<Item = u32>) -> Option<Color> {
    let byte = |c: u32| u8::try_from(c).ok();
    match codes.next()? {
        5 => Some(Color::Indexed(byte(codes.next()?)?)),
        2 => {
            let (r, g, b) = (codes.next()?, codes.next()?, codes.next()?);
            Some(Color::Rgb(byte(r)?, byte(g)?, byte(b)?))
        }
        _ => None,
    }
}

/// One of the 16 standard colours, by its index.
fn basic(index: u32) -> Color {
    match index {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        7 => Color::Gray,
        8 => Color::DarkGray,
        9 => Color::LightRed,
        10 => Color::LightGreen,
        11 => Color::LightYellow,
        12 => Color::LightBlue,
        13 => Color::LightMagenta,
        14 => Color::LightCyan,
        _ => Color::White,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(rows: &[StyledRow]) -> Vec<String> {
        rows.iter().map(StyledRow::text).collect()
    }

    fn no_controls(rows: &[StyledRow]) {
        for (_, text) in rows.iter().flat_map(|r| &r.runs) {
            assert!(!text.contains(char::is_control), "{text:?}");
        }
    }

    #[test]
    fn rows_split_on_newlines_and_lose_a_trailing_cr() {
        let rows = parse(b"one\r\ntwo\n\nthree");
        assert_eq!(texts(&rows), ["one", "two", "", "three"]);
        assert_eq!(parse(b"a\n").len(), 1);
        assert!(parse(b"").is_empty());
    }

    #[test]
    fn sixteen_colour_codes_set_fg_bg_and_reset() {
        let rows = parse(b"\x1b[31mred\x1b[1;42mbold\x1b[39;49mplain\x1b[0mx\x1b[91;104my");
        let runs = &rows[0].runs;
        assert_eq!(runs[0], (Style::new().fg(Color::Red), "red".into()));
        assert_eq!(
            runs[1],
            (
                Style::new()
                    .fg(Color::Red)
                    .bg(Color::Green)
                    .add_modifier(Modifier::BOLD),
                "bold".into()
            )
        );
        assert_eq!(
            runs[2],
            (Style::new().add_modifier(Modifier::BOLD), "plain".into())
        );
        assert_eq!(runs[3], (Style::new(), "x".into()));
        assert_eq!(
            runs[4],
            (
                Style::new().fg(Color::LightRed).bg(Color::LightBlue),
                "y".into()
            )
        );
    }

    #[test]
    fn indexed_and_truecolor_codes_become_their_colours() {
        let rows = parse(b"\x1b[38;5;208ma\x1b[48;2;1;2;3mb\x1b[m c");
        let runs = &rows[0].runs;
        assert_eq!(runs[0].0, Style::new().fg(Color::Indexed(208)));
        assert_eq!(
            runs[1].0,
            Style::new().fg(Color::Indexed(208)).bg(Color::Rgb(1, 2, 3))
        );
        assert_eq!(runs[2], (Style::new(), " c".into()));
    }

    #[test]
    fn attributes_are_set_and_cleared() {
        let rows = parse(b"\x1b[2;3;4;7;9ma\x1b[22;23;24;27;29mb");
        let all = Modifier::DIM
            | Modifier::ITALIC
            | Modifier::UNDERLINED
            | Modifier::REVERSED
            | Modifier::CROSSED_OUT;
        assert_eq!(rows[0].runs[0].0, Style::new().add_modifier(all));
        assert_eq!(rows[0].runs[1].0, Style::new());
    }

    #[test]
    fn unknown_or_malformed_sgr_is_ignored() {
        let rows = parse(b"\x1b[5;31ma\x1b[38;9mb\x1b[38;2;300;0;0mc");
        let red = Style::new().fg(Color::Red);
        assert_eq!(rows[0].runs, [(red, "abc".to_string())]);
    }

    #[test]
    fn other_escapes_are_dropped_and_leave_no_controls() {
        let rows = parse(
            b"\x1b]8;;https://x.example\x1b\\link\x1b]8;;\x1b\\ \
              \x1b[2Jclear \x1b]0;title\x07set \x07bell \x1bxlone \x1b\
              \x1bP1$r\x1b\\dcs \x1b(Bcs \x1b[?25lhide\n",
        );
        no_controls(&rows);
        assert_eq!(
            texts(&rows),
            ["link clear set \u{FFFD}bell lone dcs cs hide"]
        );
    }

    #[test]
    fn an_escape_at_the_end_or_cut_short_is_dropped() {
        let rows = parse(b"a\x1b");
        assert_eq!(texts(&rows), ["a"]);
        let rows = parse(b"a\x1b[12\nb\x1b]unterminated\nc");
        no_controls(&rows);
        assert_eq!(texts(&rows), ["a", "b"]);
    }

    #[test]
    fn rows_past_the_cap_are_dropped() {
        assert_eq!(texts(&parse_capped(b"a\nb\nc\nd", 2)), ["a", "b"]);
        assert_eq!(texts(&parse_capped(b"a\nb", 2)), ["a", "b"]);
        assert_eq!(parse(&vec![b'\n'; MAX_ROWS + 10]).len(), MAX_ROWS);
    }

    #[test]
    fn tabs_expand_and_raw_controls_are_replaced() {
        let rows = parse(b"\ta\x00b\x9b");
        no_controls(&rows);
        assert_eq!(texts(&rows), ["    a\u{FFFD}b\u{FFFD}"]);
    }

    #[test]
    fn body_text_keeps_a_joined_word_and_escapes_bidi_and_emoji_joiners() {
        let rows = parse("\x1b[32m+می\u{200C}خواهم é\u{202E}ü 👨\u{200D}👩\x1b[0m".as_bytes());
        assert_eq!(texts(&rows), ["+می\u{200C}خواهم é<U+202E>ü 👨<U+200D>👩"]);
    }
}
