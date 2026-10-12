//! File names as Git reports them. A name is bytes, not text: decoding it
//! lossily for display would let `x\xff` and a real `x\u{FFFD}` collide, so
//! argv and identity always use the raw bytes.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use super::diff::sanitize;

/// A repo file name: the exact bytes git gave us, plus a terminal-safe form
/// for display. Equality is on the bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoPath {
    raw: OsString,
    display: String,
}

impl RepoPath {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            raw: os_string(bytes),
            display: sanitize(bytes),
        }
    }

    /// The exact name, for argv. Never decoded.
    pub fn as_os_str(&self) -> &OsStr {
        &self.raw
    }

    /// The exact name as bytes, for content that holds names, such as a
    /// `.gitignore` line. Never decoded.
    pub fn as_bytes(&self) -> &[u8] {
        self.raw.as_encoded_bytes()
    }

    /// For drawing only: invalid UTF-8 and control characters are replaced, and
    /// bidi or zero-width characters are shown as `<U+XXXX>` escapes.
    pub fn display(&self) -> &str {
        &self.display
    }
}

/// By the raw bytes, like equality, so names can key a sorted map.
impl Ord for RepoPath {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.raw.cmp(&other.raw)
    }
}

impl PartialOrd for RepoPath {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl From<&str> for RepoPath {
    fn from(name: &str) -> Self {
        Self::from_bytes(name.as_bytes())
    }
}

/// A path git printed on its own line, such as from `rev-parse`: exactly one
/// trailing newline dropped, and the raw bytes kept so a directory name that
/// is not UTF-8 survives.
pub fn path_from_stdout(stdout: &[u8]) -> PathBuf {
    let bytes = stdout.strip_suffix(b"\n").unwrap_or(stdout);
    PathBuf::from(os_string(bytes))
}

#[cfg(unix)]
pub(crate) fn os_string(bytes: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(bytes.to_vec())
}

/// Elsewhere an `OsString` cannot hold arbitrary bytes; git for those
/// platforms reports names as UTF-8 anyway.
#[cfg(not(unix))]
pub(crate) fn os_string(bytes: &[u8]) -> OsString {
    String::from_utf8_lossy(bytes).into_owned().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_utf8_name_is_its_own_display() {
        let path = RepoPath::from("src/café.rs");
        assert_eq!(path.display(), "src/café.rs");
        assert_eq!(path.as_os_str(), "src/café.rs");
    }

    #[test]
    fn control_characters_never_reach_the_display() {
        let path = RepoPath::from_bytes(b"a\x1b[2J\tb");
        assert_eq!(path.display(), "a\u{FFFD}[2J    b");
    }

    #[cfg(unix)]
    #[test]
    fn invalid_utf8_keeps_its_bytes_and_differs_from_its_look_alike() {
        use std::os::unix::ffi::OsStrExt;
        let raw = RepoPath::from_bytes(b"x\xff");
        let look_alike = RepoPath::from("x\u{FFFD}");
        assert_eq!(raw.as_os_str().as_bytes(), b"x\xff");
        assert_eq!(raw.display(), look_alike.display());
        assert_ne!(raw, look_alike);
    }

    #[test]
    fn names_that_differ_only_by_an_invisible_character_display_differently() {
        let pairs = [("ab", "a\u{200B}b"), ("a\u{200B}b", "a\u{200C}b")];
        for (left, right) in pairs {
            let (l, r) = (RepoPath::from(left), RepoPath::from(right));
            assert_ne!(l.display(), r.display(), "{left:?} vs {right:?}");
            assert_eq!(l.as_os_str(), left);
            assert_eq!(r.as_os_str(), right);
        }
    }
}
