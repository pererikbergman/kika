//! The one line of text on the bottom bar that says why a key did nothing,
//! or what just happened. Text built at run time often names a branch, a
//! commit or a forge message, so this is the one place it is made safe for
//! the terminal and kept to a sane length.

use std::borrow::Cow;
use std::fmt;
use std::ops::Deref;

use kika_git::safe_text::sanitize;

/// The most chars run-time text keeps. The bar elides to its width anyway;
/// this only bounds what is stored.
pub const MAX_NOTICE_CHARS: usize = 200;

/// A notice or a disabled-key reason. Literals stay as written; owned text
/// is sanitised and capped when it is built, so nothing else has to.
#[derive(Debug, Clone)]
pub struct Notice {
    text: Cow<'static, str>,
    /// Where a path at the end of `text` starts, as a byte offset, so a
    /// narrow row can cut the path from the left and keep the file name.
    path_at: Option<usize>,
    /// Where the clause saying what to do starts, as a byte offset, so a
    /// narrow row cuts the words before it first and keeps the advice.
    advice_at: Option<usize>,
}

impl Notice {
    /// A trusted literal, kept as written.
    pub const fn fixed(text: &'static str) -> Self {
        Self {
            text: Cow::Borrowed(text),
            path_at: None,
            advice_at: None,
        }
    }

    /// A trusted literal of the form `<what>; <what to do>`, with the
    /// clause after the first `; ` marked as the part to keep. With no
    /// `; ` it is [`Notice::fixed`].
    pub fn advised(text: &'static str) -> Self {
        Self {
            advice_at: text.find("; ").map(|at| at + 2),
            ..Self::fixed(text)
        }
    }

    /// Text made at run time: control and invisible characters are escaped
    /// and anything past `MAX_NOTICE_CHARS` is cut with `…`.
    pub fn owned(text: String) -> Self {
        let safe = sanitize(text.as_bytes());
        Self {
            text: Cow::Owned(cap(safe)),
            path_at: None,
            advice_at: None,
        }
    }

    /// `lead` then `path`, stored as `owned` would store them joined, with
    /// the path's start marked. Sanitising is per char, so the two halves
    /// sanitise to the joined text. Capped text keeps no mark: its path
    /// end is gone.
    pub fn with_path(lead: &str, path: &str) -> Self {
        let lead = sanitize(lead.as_bytes());
        let at = lead.len();
        let safe = lead + &sanitize(path.as_bytes());
        let cut = safe.chars().nth(MAX_NOTICE_CHARS).is_some();
        Self {
            text: Cow::Owned(cap(safe)),
            path_at: (!cut).then_some(at),
            advice_at: None,
        }
    }

    /// The literal, when the notice is one. Lets tests and callers tell
    /// fixed reasons from built ones.
    pub fn as_fixed(&self) -> Option<&'static str> {
        match self.text {
            Cow::Borrowed(text) => Some(text),
            Cow::Owned(_) => None,
        }
    }

    /// The text before the path and the path, when the notice ends in one.
    pub fn path_split(&self) -> Option<(&str, &str)> {
        self.path_at.map(|at| self.text.split_at(at))
    }

    /// The words before the advice, `; ` included, and the advice, when
    /// the notice marks one.
    pub fn advice_split(&self) -> Option<(&str, &str)> {
        self.advice_at.map(|at| self.text.split_at(at))
    }
}

/// By the text only: the path mark is a display hint, not content.
impl PartialEq for Notice {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
    }
}

impl Eq for Notice {}

/// Keeps the start of `text`, ending in `…` when it had to cut.
fn cap(text: String) -> String {
    match text.char_indices().nth(MAX_NOTICE_CHARS) {
        None => text,
        Some(_) => {
            let keep: String = text.chars().take(MAX_NOTICE_CHARS - 1).collect();
            keep + "…"
        }
    }
}

impl From<&'static str> for Notice {
    fn from(text: &'static str) -> Self {
        Self::fixed(text)
    }
}

impl From<String> for Notice {
    fn from(text: String) -> Self {
        Self::owned(text)
    }
}

impl From<Notice> for String {
    fn from(notice: Notice) -> Self {
        notice.text.into_owned()
    }
}

impl PartialEq<str> for Notice {
    fn eq(&self, other: &str) -> bool {
        *self.text == *other
    }
}

impl PartialEq<&str> for Notice {
    fn eq(&self, other: &&str) -> bool {
        *self.text == **other
    }
}

impl Deref for Notice {
    type Target = str;

    fn deref(&self) -> &str {
        &self.text
    }
}

impl fmt::Display for Notice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_text_escapes_controls_tabs_and_invisible_characters() {
        let notice = Notice::from("a\x1b[31mb\tc\u{202E}d\u{200B}e".to_string());
        assert_eq!(&*notice, "a\u{FFFD}[31mb    c<U+202E>d<U+200B>e");
    }

    #[test]
    fn sanitising_twice_looks_the_same() {
        let once = Notice::from("x\x07y\u{2066}".to_string());
        let twice = Notice::from(once.to_string());
        assert_eq!(once, twice);
    }

    #[test]
    fn long_owned_text_is_capped_with_an_ellipsis() {
        let notice = Notice::from("é".repeat(MAX_NOTICE_CHARS + 5));
        assert_eq!(notice.chars().count(), MAX_NOTICE_CHARS);
        assert!(notice.ends_with('…'), "{notice}");
        assert!(notice.starts_with("éé"), "{notice}");
    }

    #[test]
    fn text_at_or_under_the_cap_is_unchanged() {
        let exact = "x".repeat(MAX_NOTICE_CHARS);
        assert_eq!(&*Notice::from(exact.clone()), exact);
        assert_eq!(&*Notice::from("Copied hash".to_string()), "Copied hash");
    }

    #[test]
    fn a_literal_is_kept_as_written_and_given_back() {
        let notice = Notice::from("Nothing\tto do");
        assert_eq!(&*notice, "Nothing\tto do");
        assert_eq!(notice.as_fixed(), Some("Nothing\tto do"));
        assert_eq!(Notice::fixed("x"), Notice::from("x"));
    }

    #[test]
    fn a_path_notice_stores_what_the_joined_text_would() {
        let lead = "Old path: ";
        for path in ["src/a.rs", "a\tb/c.rs", "x\u{202E}y/z.rs", "d\x07e/f.rs"] {
            let marked = Notice::with_path(lead, path);
            let plain = Notice::owned(format!("{lead}{path}"));
            assert_eq!(&*marked, &*plain, "{path:?}");
            assert_eq!(marked, plain, "{path:?}");
            let safe = sanitize(path.as_bytes());
            assert_eq!(marked.path_split(), Some((lead, safe.as_str())), "{path:?}");
            assert_eq!(plain.path_split(), None);
        }
    }

    #[test]
    fn an_advised_notice_marks_the_clause_after_its_first_semicolon() {
        let advised = Notice::advised("Not here; do this; then that");
        assert_eq!(
            advised.advice_split(),
            Some(("Not here; ", "do this; then that"))
        );
        assert_eq!(advised, Notice::fixed("Not here; do this; then that"));
        assert_eq!(advised.as_fixed(), Some("Not here; do this; then that"));
        assert_eq!(Notice::advised("Nothing to do").advice_split(), None);
        assert_eq!(Notice::fixed("Not here; do this").advice_split(), None);
    }

    #[test]
    fn a_path_notice_sanitises_its_lead_too() {
        let marked = Notice::with_path("a\u{200B}: ", "p");
        assert_eq!(marked.path_split(), Some(("a<U+200B>: ", "p")));
    }

    #[test]
    fn a_capped_path_notice_loses_its_mark_and_keeps_todays_text() {
        let path = "d/".repeat(MAX_NOTICE_CHARS);
        let marked = Notice::with_path("Lead: ", &path);
        assert_eq!(&*marked, &*Notice::owned(format!("Lead: {path}")));
        assert_eq!(marked.path_split(), None);
    }

    #[test]
    fn literals_and_owned_text_have_no_path() {
        assert_eq!(Notice::fixed("a/b").path_split(), None);
        assert_eq!(Notice::from("a/b".to_string()).path_split(), None);
    }

    #[test]
    fn built_text_is_not_fixed() {
        assert_eq!(Notice::from("x".to_string()).as_fixed(), None);
    }
}
