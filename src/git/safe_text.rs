//! Making text from git, a forge or a file safe to draw. One home for the
//! sanitiser, so git, forge and features all escape the same way.

/// Decode lossily and make the text safe to draw: tabs become spaces and any
/// other control character becomes `U+FFFD`, so file bytes never drive the terminal.
/// Bidi controls and zero-width characters reorder or hide text, so a hostile
/// name could pass for another; they become a visible `<U+XXXX>` escape rather
/// than being dropped, so names that differ only by them still look different.
pub fn sanitize(raw: &[u8]) -> String {
    let mut text = String::with_capacity(raw.len());
    for c in String::from_utf8_lossy(raw).chars() {
        push_safe(&mut text, c);
    }
    text
}

/// The one escaping rule both sanitisers share, so they cannot drift apart.
pub(crate) fn push_safe(text: &mut String, c: char) {
    match c {
        '\t' => text.push_str("    "),
        c if is_invisible_or_bidi(c) => text.push_str(&format!("<U+{:04X}>", u32::from(c))),
        c if c.is_control() => text.push('\u{FFFD}'),
        c => text.push(c),
    }
}

/// Format characters (not `is_control`) that reorder text or draw nothing:
/// Unicode's Default_Ignorable_Code_Point set plus the interlinear annotation
/// characters. U+FE0E and U+FE0F are left alone because emoji such as "❤️"
/// need them; a name differing only by one of those is a narrow, accepted gap.
pub(crate) fn is_invisible_or_bidi(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{061C}'
            | '\u{115F}'..='\u{1160}'
            | '\u{17B4}'..='\u{17B5}'
            | '\u{180B}'..='\u{180F}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0D}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{FFF0}'..='\u{FFFB}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0000}'..='\u{E0FFF}'
    )
}

/// git's message, line by line through the same sanitiser as diff lines,
/// since it can quote repo-controlled paths.
pub fn terminal_safe(stderr: &[u8]) -> String {
    stderr
        .trim_ascii()
        .split(|&b| b == b'\n')
        .map(|line| sanitize(line.strip_suffix(b"\r").unwrap_or(line)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// git's words, safe to draw and with no credentials in them. Sanitised
/// first, so redaction sees the text as it will be drawn.
pub fn git_message(bytes: &[u8]) -> String {
    redact_userinfo(&terminal_safe(bytes))
}

/// `text` with the user part of every URL taken out: `scheme://user:pass@host`
/// becomes `scheme://host`. Git quotes remote URLs in its errors, and a URL
/// can carry a token. The authority ends only at the path or a line break,
/// and everything up to its last `@` goes. Not at `?`, `#`, a quote or a
/// space, so none of them inside a password can cut the redaction short;
/// text after a URL with no path (its query included) can lose words up to
/// a later `@`, which is the safe way to be wrong.
pub fn redact_userinfo(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let (before, after) = rest.split_at(at + 3);
        out.push_str(before);
        let end = after.find(['/', '\n', '\r']).unwrap_or(after.len());
        let authority = &after[..end];
        let host = authority.rfind('@').map_or(0, |i| i + 1);
        rest = &after[host..];
    }
    out.push_str(rest);
    out
}

/// Every class the strict sanitiser escapes, with sample code points.
#[cfg(test)]
pub(crate) const STRICT_CLASSES: [(&str, &[char]); 17] = [
    (
        "embedding/override",
        &['\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}'],
    ),
    ("isolate", &['\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}']),
    ("mark", &['\u{200E}', '\u{200F}', '\u{061C}']),
    ("zero-width", &['\u{200B}', '\u{200C}', '\u{200D}']),
    ("word joiner", &['\u{2060}']),
    ("byte order mark", &['\u{FEFF}']),
    ("soft hyphen", &['\u{00AD}']),
    ("combining grapheme joiner", &['\u{034F}']),
    (
        "hangul filler",
        &['\u{115F}', '\u{1160}', '\u{3164}', '\u{FFA0}'],
    ),
    ("khmer vowel inherent", &['\u{17B4}', '\u{17B5}']),
    (
        "mongolian format",
        &['\u{180B}', '\u{180C}', '\u{180D}', '\u{180E}', '\u{180F}'],
    ),
    (
        "invisible operator and deprecated format",
        &['\u{2061}', '\u{2064}', '\u{2065}', '\u{206A}', '\u{206F}'],
    ),
    ("variation selector", &['\u{FE00}', '\u{FE0D}']),
    (
        "unassigned and interlinear annotation",
        &['\u{FFF0}', '\u{FFF8}', '\u{FFF9}', '\u{FFFA}', '\u{FFFB}'],
    ),
    ("shorthand format", &['\u{1BCA0}', '\u{1BCA3}']),
    ("musical format", &['\u{1D173}', '\u{1D17A}']),
    (
        "tag",
        &[
            '\u{E0000}',
            '\u{E0001}',
            '\u{E0041}',
            '\u{E007F}',
            '\u{E0FFF}',
        ],
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bidi_and_invisible_characters_become_visible_escapes() {
        for (class, chars) in STRICT_CLASSES {
            for &c in chars {
                let raw = format!("a{c}b");
                let expected = format!("a<U+{:04X}>b", c as u32);
                assert_eq!(sanitize(raw.as_bytes()), expected, "{class} {c:?}");
            }
        }
        assert_eq!(sanitize("a\u{202E}b".as_bytes()), "a<U+202E>b");
        assert_eq!(sanitize("\u{061C}".as_bytes()), "<U+061C>");
    }

    #[test]
    fn ordinary_text_is_unchanged() {
        // U+FE0E/U+FE0F are kept so emoji presentation still works.
        for text in [
            "src/café.rs",
            "日本語",
            "🙂",
            "plain ascii",
            "❤\u{FE0F}",
            "\u{263A}\u{FE0E}",
        ] {
            assert_eq!(sanitize(text.as_bytes()), text);
        }
    }

    #[test]
    fn git_error_text_shows_bidi_escapes() {
        assert_eq!(
            terminal_safe("fatal: bad \u{2066}path\u{2069}\n".as_bytes()),
            "fatal: bad <U+2066>path<U+2069>"
        );
    }

    #[test]
    fn redact_userinfo_takes_the_user_part_out_of_every_url() {
        let redacted = redact_userinfo("fatal: unable to access 'https://u:tok@h/r/'");
        assert!(
            !redacted.contains("u:tok") && !redacted.contains("tok"),
            "{redacted}"
        );
        assert_eq!(redacted, "fatal: unable to access 'https://h/r/'");

        assert_eq!(
            redact_userinfo("from https://ghp_x@github.com/a and http://a:b@c:8080"),
            "from https://github.com/a and http://c:8080"
        );
        // A password with an `@` or a quote in it goes too.
        assert_eq!(redact_userinfo("ssh://u:p@ss@h/r"), "ssh://h/r");
        assert_eq!(
            redact_userinfo("unable to access 'https://u:to'k@h/r/'"),
            "unable to access 'https://h/r/'"
        );
        assert_eq!(redact_userinfo("\"https://u:p\"w@h?x\""), "\"https://h?x\"");
        // Whitespace inside a password, as typed or as a tab drawn as spaces.
        assert_eq!(redact_userinfo("https://u:my pass@h/r"), "https://h/r");
        assert_eq!(redact_userinfo("https://u:a\tb@h/r"), "https://h/r");
        assert_eq!(redact_userinfo("https://u:a    b@h"), "https://h");
        // A URL ends at a line break, so the next line keeps its `@`.
        assert_eq!(
            redact_userinfo("'https://h'\nhint: try git@x"),
            "'https://h'\nhint: try git@x"
        );
        // Nothing to take out.
        for text in ["git@github.com:a/b.git", "https://h/r", "no url", "a://"] {
            assert_eq!(redact_userinfo(text), text);
        }
    }

    #[test]
    fn redact_userinfo_removes_a_password_holding_url_syntax_whole() {
        for (url, password) in [
            ("https://u:p?ss@h/r", "p?ss"),
            ("https://u:p#ss@h/r", "p#ss"),
            ("https://u:my pass@h/r", "my pass"),
            ("https://u:to'k@h/r", "to'k"),
            ("https://u:to\"k@h/r", "to\"k"),
            ("ssh://u:p@ss@h/r", "p@ss"),
        ] {
            let redacted = redact_userinfo(url);
            let (scheme, rest) = redacted.split_once("://").unwrap();
            assert_eq!(redacted, format!("{scheme}://h/r"), "{url}");
            for part in password.split(['?', '#', ' ', '\'', '"', '@']) {
                assert!(!rest.contains(part), "{url} -> {redacted}");
            }
        }
    }
}
