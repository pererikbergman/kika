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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice(Cow<'static, str>);

impl Notice {
    /// A trusted literal, kept as written.
    pub const fn fixed(text: &'static str) -> Self {
        Self(Cow::Borrowed(text))
    }

    /// Text made at run time: control and invisible characters are escaped
    /// and anything past `MAX_NOTICE_CHARS` is cut with `…`.
    pub fn owned(text: String) -> Self {
        let safe = sanitize(text.as_bytes());
        Self(Cow::Owned(cap(safe)))
    }

    /// The literal, when the notice is one. Lets tests and callers tell
    /// fixed reasons from built ones.
    pub fn as_fixed(&self) -> Option<&'static str> {
        match self.0 {
            Cow::Borrowed(text) => Some(text),
            Cow::Owned(_) => None,
        }
    }
}

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
        notice.0.into_owned()
    }
}

impl PartialEq<str> for Notice {
    fn eq(&self, other: &str) -> bool {
        *self.0 == *other
    }
}

impl PartialEq<&str> for Notice {
    fn eq(&self, other: &&str) -> bool {
        *self.0 == **other
    }
}

impl Deref for Notice {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Notice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
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
    fn built_text_is_not_fixed() {
        assert_eq!(Notice::from("x".to_string()).as_fixed(), None);
    }
}
