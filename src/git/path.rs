//! File names as Git reports them. A name is bytes, not text: decoding it
//! lossily for display would let `x\xff` and a real `x\u{FFFD}` collide, so
//! argv and identity always use the raw bytes.

use std::ffi::{OsStr, OsString};

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

    /// For drawing only: invalid UTF-8 and control characters are replaced.
    pub fn display(&self) -> &str {
        &self.display
    }
}

impl From<&str> for RepoPath {
    fn from(name: &str) -> Self {
        Self::from_bytes(name.as_bytes())
    }
}

#[cfg(unix)]
fn os_string(bytes: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(bytes.to_vec())
}

/// Elsewhere an `OsString` cannot hold arbitrary bytes; git for those
/// platforms reports names as UTF-8 anyway.
#[cfg(not(unix))]
fn os_string(bytes: &[u8]) -> OsString {
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
}
