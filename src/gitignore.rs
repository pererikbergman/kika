//! Lines for the worktree's `.gitignore`: the pattern that matches exactly
//! one file, and the file's content with a line added. Pure; the write
//! goes through [`worktree_file`](crate::worktree_file).

use crate::git::path::RepoPath;

/// The pattern the `.gitignore` prompt offers for a file: the lossy,
/// terminal-safe text it is filled with, and the exact bytes written when
/// it is accepted unchanged, so a name that is not UTF-8 is still matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnoreDraft {
    pub shown: String,
    pub bytes: Vec<u8>,
}

/// What an add did: `pattern` as shown, and whether it was written or
/// was already a line of the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnoreAdded {
    pub pattern: String,
    pub added: bool,
}

/// Why a name with a line break cannot be ignored by name.
pub const UNWRITABLE: &str = "This name cannot be written as a .gitignore line";

/// The pattern that matches exactly `path`, anchored to the top level.
pub fn escape(path: &RepoPath) -> Result<Vec<u8>, &'static str> {
    escape_bytes(path.as_bytes(), true)
}

/// The pattern for `name`, from its raw bytes. Glob characters and a
/// backslash are escaped, and so are trailing spaces, which git would
/// otherwise drop. Without the anchor, a leading `#` or `!` is escaped
/// too, or the line would be a comment or a negation. Bytes that are not
/// UTF-8 stay as they are: git matches bytes and has no byte escape.
fn escape_bytes(name: &[u8], anchored: bool) -> Result<Vec<u8>, &'static str> {
    if name.is_empty() || name.iter().any(|&b| b == b'\n' || b == b'\r') {
        return Err(UNWRITABLE);
    }
    let trailing = name.iter().rev().take_while(|&&b| b == b' ').count();
    let body = &name[..name.len() - trailing];
    let mut out = Vec::with_capacity(name.len() + 8);
    if anchored {
        out.push(b'/');
    } else if matches!(body.first(), Some(b'#' | b'!')) {
        out.push(b'\\');
    }
    for &b in body {
        if matches!(b, b'\\' | b'*' | b'?' | b'[') {
            out.push(b'\\');
        }
        out.push(b);
    }
    for _ in 0..trailing {
        out.extend_from_slice(b"\\ ");
    }
    Ok(out)
}

/// The `.gitignore` content `old` with `line` added at the end, or `None`
/// when `line` is already one of its lines. A last line without a newline
/// gets one first; the new line ends `\r\n` when the old last line did.
pub fn appended(old: Option<&[u8]>, line: &[u8]) -> Option<Vec<u8>> {
    let old = old.unwrap_or_default();
    let present = old
        .split(|&b| b == b'\n')
        .any(|l| l.strip_suffix(b"\r").unwrap_or(l) == line);
    if present {
        return None;
    }
    let crlf = old.ends_with(b"\r\n");
    let newline: &[u8] = if crlf { b"\r\n" } else { b"\n" };
    let mut out = old.to_vec();
    if !old.is_empty() && !old.ends_with(b"\n") {
        out.extend_from_slice(newline);
    }
    out.extend_from_slice(line);
    out.extend_from_slice(newline);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_characters_and_backslashes_are_escaped_under_the_anchor() {
        assert_eq!(
            escape(&RepoPath::from("a*b?[c]\\d")),
            Ok(b"/a\\*b\\?\\[c]\\\\d".to_vec())
        );
        assert_eq!(
            escape(&RepoPath::from("dir/build.log")),
            Ok(b"/dir/build.log".to_vec())
        );
    }

    #[test]
    fn trailing_spaces_are_escaped_and_inner_ones_kept() {
        assert_eq!(escape(&RepoPath::from("a b ")), Ok(b"/a b\\ ".to_vec()));
        assert_eq!(escape(&RepoPath::from("x  ")), Ok(b"/x\\ \\ ".to_vec()));
        // A tab is not a space git strips.
        assert_eq!(escape(&RepoPath::from("x\t")), Ok(b"/x\t".to_vec()));
    }

    #[test]
    fn a_leading_hash_or_bang_is_escaped_without_the_anchor() {
        assert_eq!(escape_bytes(b"#x", false), Ok(b"\\#x".to_vec()));
        assert_eq!(escape_bytes(b"!x", false), Ok(b"\\!x".to_vec()));
        assert_eq!(escape_bytes(b"x#!", false), Ok(b"x#!".to_vec()));
        assert_eq!(escape(&RepoPath::from("#x")), Ok(b"/#x".to_vec()));
    }

    #[cfg(unix)]
    #[test]
    fn bytes_that_are_not_utf8_are_kept_raw() {
        assert_eq!(
            escape(&RepoPath::from_bytes(b"x\xff")),
            Ok(b"/x\xff".to_vec())
        );
    }

    #[test]
    fn a_name_with_a_line_break_is_refused() {
        assert_eq!(escape(&RepoPath::from("a\nb")), Err(UNWRITABLE));
        assert_eq!(escape(&RepoPath::from("a\rb")), Err(UNWRITABLE));
    }

    #[test]
    fn a_line_is_appended_after_a_newline() {
        assert_eq!(appended(None, b"line"), Some(b"line\n".to_vec()));
        assert_eq!(appended(Some(b""), b"line"), Some(b"line\n".to_vec()));
        assert_eq!(appended(Some(b"a"), b"line"), Some(b"a\nline\n".to_vec()));
        assert_eq!(appended(Some(b"a\n"), b"line"), Some(b"a\nline\n".to_vec()));
    }

    #[test]
    fn a_crlf_file_gets_a_crlf_line() {
        assert_eq!(
            appended(Some(b"a\r\n"), b"line"),
            Some(b"a\r\nline\r\n".to_vec())
        );
    }

    #[test]
    fn a_line_already_there_is_not_added_again() {
        assert_eq!(appended(Some(b"a\nline\nb\n"), b"line"), None);
        assert_eq!(appended(Some(b"line"), b"line"), None);
        assert_eq!(appended(Some(b"x\r\nline\r\n"), b"line"), None);
        assert_eq!(
            appended(Some(b"lines\n"), b"line"),
            Some(b"lines\nline\n".to_vec())
        );
    }
}
