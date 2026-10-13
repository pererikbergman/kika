//! Copying text to the system clipboard with the OSC 52 terminal escape:
//! the terminal does the copy, so it needs no dependency and works over
//! SSH. Pure apart from the sink.
//!
//! Limits: some terminals do not support OSC 52 or turn it off by default,
//! and none replies to say whether the copy worked. Inside tmux it needs
//! `set -g set-clipboard on`. GNU screen is not supported. Payloads are
//! capped at [`MAX_COPY`], since several terminals drop larger ones
//! without a word. No DCS passthrough wrapping is sent.
//!
//! Where OSC 52 does not work, the global config's `[clipboard] command`
//! names a program to copy with instead; it gets [`command_payload`] on
//! stdin. It is off by default and never tried after OSC 52: with no
//! reply, a failed OSC 52 copy cannot be detected. Over SSH such a program
//! fills the remote machine's clipboard, not the user's.

use std::io::{self, Write};

use crate::git::safe_text::is_invisible_or_bidi;

/// What of a commit to copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyWhat {
    Hash,
    ShortHash,
    Subject,
    Message,
    /// The commit's page on the remote's forge.
    ForgeUrl,
}

impl CopyWhat {
    /// How the notice names it: "Copied <label>: …".
    pub fn label(self) -> &'static str {
        match self {
            CopyWhat::Hash => "hash",
            CopyWhat::ShortHash => "short hash",
            CopyWhat::Subject => "subject",
            CopyWhat::Message => "message",
            CopyWhat::ForgeUrl => "URL",
        }
    }
}

/// The most bytes copied at once, after cleaning.
pub const MAX_COPY: usize = 64 * 1024;

/// Why a copy over [`MAX_COPY`] is refused.
pub const TOO_LONG: &str = "Too long to copy (64 KB limit)";

/// `text` with nothing that could drive a terminal: every control
/// character but `\n` and `\t` is dropped (C0, DEL and C1, so a raw ESC
/// and the 8-bit CSI, OSC and ST), and so is every bidi or zero-width
/// character. `\r\n` becomes `\n`, and trailing newlines go.
pub fn clean(text: &str) -> String {
    let text = text.replace("\r\n", "\n");
    let kept: String = text
        .chars()
        .filter(|&c| c == '\n' || c == '\t' || !(c.is_control() || is_invisible_or_bidi(c)))
        .collect();
    kept.trim_end_matches('\n').to_string()
}

/// The OSC 52 sequence that sets the clipboard to `clean(text)`. The
/// payload is base64, whose alphabet cannot end the sequence early.
pub fn osc52(text: &str) -> Result<Vec<u8>, &'static str> {
    let text = command_payload(text)?;
    let mut out = b"\x1b]52;c;".to_vec();
    out.extend(base64(&text).bytes());
    out.push(0x07);
    Ok(out)
}

/// The bytes a clipboard program is given on stdin: `clean(text)`, so a
/// copy holds the same text whichever way it is made, within [`MAX_COPY`].
pub fn command_payload(text: &str) -> Result<Vec<u8>, &'static str> {
    let text = clean(text);
    if text.len() > MAX_COPY {
        return Err(TOO_LONG);
    }
    Ok(text.into_bytes())
}

/// The start of `text` as one line, for a notice: cleaned, with newlines
/// and tabs as spaces, cut to 40 characters.
pub fn preview(text: &str) -> String {
    let text: String = clean(text)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    match text.char_indices().nth(40) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text,
    }
}

/// Standard base64 (RFC 4648) with padding.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[(n >> (18 - 6 * i) & 0x3f) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Where an OSC 52 sequence goes. Only the loop thread calls it, between
/// draws, so the bytes never land inside a frame.
pub trait ClipboardSink: Send + Sync {
    fn copy(&self, bytes: &[u8]) -> io::Result<()>;
}

/// The terminal Kika draws on.
pub struct TerminalSink;

impl ClipboardSink for TerminalSink {
    fn copy(&self, bytes: &[u8]) -> io::Result<()> {
        let mut out = io::stdout().lock();
        out.write_all(bytes)?;
        out.flush()
    }
}

/// Keeps every sequence it is given, for tests.
#[cfg(test)]
#[derive(Default)]
pub struct RecordingSink(std::sync::Mutex<Vec<Vec<u8>>>);

#[cfg(test)]
impl RecordingSink {
    pub fn copies(&self) -> Vec<Vec<u8>> {
        self.0.lock().unwrap().clone()
    }
}

#[cfg(test)]
impl ClipboardSink for RecordingSink {
    fn copy(&self, bytes: &[u8]) -> io::Result<()> {
        self.0.lock().unwrap().push(bytes.to_vec());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleaning_keeps_only_printable_text_newlines_and_tabs() {
        let dirty = "a\x1b[31mb\x07c\u{9b}2Jd\u{9d}e\r\nf\tg\u{202E}h\u{200B}i\x7f\rj\n\n";
        assert_eq!(clean(dirty), "a[31mbc2Jde\nf\tghij");
    }

    #[test]
    fn osc52_wraps_the_base64_in_the_escape() {
        assert_eq!(osc52("abc"), Ok(b"\x1b]52;c;YWJj\x07".to_vec()));
        assert_eq!(osc52("a\x1bb"), Ok(b"\x1b]52;c;YWI=\x07".to_vec()));
    }

    #[test]
    fn base64_matches_the_rfc_4648_test_vectors() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(plain.as_bytes()), encoded, "{plain:?}");
        }
        assert_eq!(base64(&[0xfb, 0xff, 0xbf]), "+/+/");
    }

    #[test]
    fn more_than_64_kb_is_refused() {
        assert!(osc52(&"x".repeat(MAX_COPY)).is_ok());
        assert_eq!(osc52(&"x".repeat(MAX_COPY + 1)), Err(TOO_LONG));
        // Measured after cleaning: control characters do not count.
        assert!(osc52(&format!("{}\x1b", "x".repeat(MAX_COPY))).is_ok());
    }

    #[test]
    fn a_command_gets_the_same_cleaned_text_as_osc52() {
        assert_eq!(command_payload("hi\x1b\r\n"), Ok(b"hi".to_vec()));
        assert!(command_payload(&"x".repeat(MAX_COPY)).is_ok());
        assert_eq!(command_payload(&"x".repeat(MAX_COPY + 1)), Err(TOO_LONG));
    }

    #[test]
    fn a_preview_is_one_short_line() {
        assert_eq!(preview("subj\n\nbody\tx"), "subj  body x");
        assert_eq!(preview(&"é".repeat(41)), format!("{}…", "é".repeat(40)));
        assert_eq!(preview(&"é".repeat(40)), "é".repeat(40));
    }
}
