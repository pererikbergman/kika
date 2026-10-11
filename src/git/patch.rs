//! The patch engine: parse one file's plain unified diff into hunks, and
//! build a patch from a selection of them for `git apply --cached`. Pure,
//! and bytes throughout: patch content is never decoded, so CRLF, invalid
//! UTF-8 and missing final newlines round-trip exactly. Only the view
//! sanitizes, when it draws.

use std::fmt;

/// One file's diff, split into the lines before the first hunk and the hunks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePatch {
    /// The raw lines before the first `@@`, each with its line ending.
    pub header: Vec<Vec<u8>>,
    pub kind: FileKind,
    pub hunks: Vec<Hunk>,
}

/// What the header says the change is, which decides whether hunks of it
/// can be staged on their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    Binary,
    /// Combined (conflict) diffs and submodules.
    Unsupported,
}

/// One `@@` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: u32,
    pub old_len: u32,
    pub new_start: u32,
    pub new_len: u32,
    /// The bytes after the second `@@`, without the line ending.
    pub heading: Vec<u8>,
    pub lines: Vec<PatchLine>,
}

/// One line of a hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchLine {
    pub kind: PatchLineKind,
    /// The bytes after the prefix, with the line ending (any `\r` too).
    /// No `\n` when `no_newline` is set.
    pub text: Vec<u8>,
    /// The file has no newline after this line (git's `\` marker).
    pub no_newline: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchLineKind {
    Context,
    Added,
    Removed,
}

/// Which part of a file's diff to put in a patch. One variant for now;
/// line selection adds another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchSelection {
    Hunk(usize),
}

/// Why a diff could not be parsed, or a patch not built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchError {
    /// The diff holds more than one file.
    MultipleFiles,
    /// A hunk's lines do not match its `@@` counts, or a line is not diff.
    Malformed,
    Binary,
    Renamed,
    Copied,
    Unsupported,
    /// The selection names a hunk that does not exist.
    NoSuchHunk,
}

impl PatchError {
    /// A short sentence for the bottom bar.
    pub fn message(self) -> &'static str {
        match self {
            PatchError::MultipleFiles => "The diff holds more than one file",
            PatchError::Malformed => "The diff could not be read",
            PatchError::Binary => "Binary file: stage it from the Files panel",
            PatchError::Renamed => "Renamed file: stage it from the Files panel",
            PatchError::Copied => "Copied file: stage it from the Files panel",
            PatchError::Unsupported => "This change can only be staged from the Files panel",
            PatchError::NoSuchHunk => "No such hunk",
        }
    }
}

impl fmt::Display for PatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for PatchError {}

/// Git writes this after a line with no newline; the text after `\` is
/// localised, so parsing only looks at the `\`.
const NO_NEWLINE: &[u8] = b"\\ No newline at end of file\n";

/// Parse the diff of exactly one file. Empty input is a file with no changes.
pub fn parse(bytes: &[u8]) -> Result<FilePatch, PatchError> {
    let mut lines = bytes.split_inclusive(|&b| b == b'\n').peekable();
    let mut header = Vec::new();
    while let Some(line) = lines.next_if(|line| !line.starts_with(b"@@")) {
        header.push(line.to_vec());
    }
    let files = header
        .iter()
        .filter(|line| line.starts_with(b"diff "))
        .count();
    if files > 1 {
        return Err(PatchError::MultipleFiles);
    }
    let kind = kind_of(&header);
    if kind == FileKind::Unsupported && header.iter().any(|l| is_combined(l)) {
        // `@@@` hunks have a prefix column per parent; they are never staged
        // by hunk, so they stay unparsed.
        header.extend(lines.map(<[u8]>::to_vec));
        return Ok(FilePatch {
            header,
            kind,
            hunks: Vec::new(),
        });
    }
    let mut hunks = Vec::new();
    while let Some(line) = lines.next() {
        if line.starts_with(b"diff ") {
            return Err(PatchError::MultipleFiles);
        }
        let mut hunk = parse_hunk_header(line)?;
        let (mut old, mut new) = (hunk.old_len, hunk.new_len);
        while old > 0 || new > 0 {
            let line = lines.next().ok_or(PatchError::Malformed)?;
            let (kind, text) = match line.split_first() {
                Some((b' ', text)) => (PatchLineKind::Context, text),
                Some((b'-', text)) => (PatchLineKind::Removed, text),
                Some((b'+', text)) => (PatchLineKind::Added, text),
                // `diff.suppressBlankEmpty` writes a blank context line bare.
                Some((b'\n', _)) => (PatchLineKind::Context, line),
                Some((b'\\', _)) => {
                    mark_no_newline(&mut hunk)?;
                    continue;
                }
                _ => return Err(PatchError::Malformed),
            };
            let (takes_old, takes_new) = match kind {
                PatchLineKind::Context => (true, true),
                PatchLineKind::Removed => (true, false),
                PatchLineKind::Added => (false, true),
            };
            if (takes_old && old == 0) || (takes_new && new == 0) {
                return Err(PatchError::Malformed);
            }
            old -= u32::from(takes_old);
            new -= u32::from(takes_new);
            hunk.lines.push(PatchLine {
                kind,
                text: text.to_vec(),
                no_newline: false,
            });
        }
        while lines.next_if(|line| line.starts_with(b"\\")).is_some() {
            mark_no_newline(&mut hunk)?;
        }
        hunks.push(hunk);
    }
    Ok(FilePatch {
        header,
        kind,
        hunks,
    })
}

/// The `\` marker belongs to the line before it: its `\n` was only diff framing.
fn mark_no_newline(hunk: &mut Hunk) -> Result<(), PatchError> {
    let last = hunk.lines.last_mut().ok_or(PatchError::Malformed)?;
    if last.no_newline {
        return Err(PatchError::Malformed);
    }
    if last.text.last() == Some(&b'\n') {
        last.text.pop();
    }
    last.no_newline = true;
    Ok(())
}

fn is_combined(line: &[u8]) -> bool {
    line.starts_with(b"diff --cc ") || line.starts_with(b"diff --combined ")
}

/// Classify from the extended header lines. A submodule shows as mode
/// `160000` on the `index` or mode lines.
fn kind_of(header: &[Vec<u8>]) -> FileKind {
    let has = |prefix: &[u8]| header.iter().any(|line| line.starts_with(prefix));
    let submodule = header.iter().any(|line| {
        let line = line.trim_ascii_end();
        (line.starts_with(b"index ")
            || line.starts_with(b"new file mode ")
            || line.starts_with(b"deleted file mode ")
            || line.starts_with(b"old mode ")
            || line.starts_with(b"new mode "))
            && line.ends_with(b" 160000")
    });
    if header.iter().any(|l| is_combined(l)) || submodule {
        FileKind::Unsupported
    } else if has(b"GIT binary patch")
        || header
            .iter()
            .any(|l| l.starts_with(b"Binary files ") && l.trim_ascii_end().ends_with(b" differ"))
    {
        FileKind::Binary
    } else if has(b"rename from ") {
        FileKind::Renamed
    } else if has(b"copy from ") {
        FileKind::Copied
    } else if has(b"new file mode ") {
        FileKind::Added
    } else if has(b"deleted file mode ") {
        FileKind::Deleted
    } else {
        FileKind::Modified
    }
}

/// `@@ -a[,b] +c[,d] @@heading`.
fn parse_hunk_header(line: &[u8]) -> Result<Hunk, PatchError> {
    let rest = line.strip_prefix(b"@@ -").ok_or(PatchError::Malformed)?;
    let end = find(rest, b" @@").ok_or(PatchError::Malformed)?;
    let (ranges, after) = (&rest[..end], &rest[end + 3..]);
    let mut parts = ranges.split(|&b| b == b' ');
    let old = parts.next().ok_or(PatchError::Malformed)?;
    let new = parts
        .next()
        .and_then(|p| p.strip_prefix(b"+"))
        .ok_or(PatchError::Malformed)?;
    if parts.next().is_some() {
        return Err(PatchError::Malformed);
    }
    let (old_start, old_len) = parse_range(old)?;
    let (new_start, new_len) = parse_range(new)?;
    let heading = after.strip_suffix(b"\n").unwrap_or(after).to_vec();
    Ok(Hunk {
        old_start,
        old_len,
        new_start,
        new_len,
        heading,
        lines: Vec::new(),
    })
}

/// `start[,len]`; a missing length means one line.
fn parse_range(range: &[u8]) -> Result<(u32, u32), PatchError> {
    let number = |digits: &[u8]| -> Result<u32, PatchError> {
        std::str::from_utf8(digits)
            .ok()
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|s| s.parse().ok())
            .ok_or(PatchError::Malformed)
    };
    match range.iter().position(|&b| b == b',') {
        Some(comma) => Ok((number(&range[..comma])?, number(&range[comma + 1..])?)),
        None => Ok((number(range)?, 1)),
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Whether applying the selection would take the whole file's change, so
/// staging the file itself does the same with less risk.
pub fn selects_everything(patch: &FilePatch, selection: &PatchSelection) -> bool {
    match selection {
        PatchSelection::Hunk(index) => patch.hunks.len() == 1 && *index == 0,
    }
}

/// The patch for `selection`: the `diff --git`, `---` and `+++` lines
/// verbatim, then the selected hunks with their counts recomputed. Mode,
/// index and similarity lines are left out, so a hunk never carries a mode
/// change with it.
pub fn build(patch: &FilePatch, selection: &PatchSelection) -> Result<Vec<u8>, PatchError> {
    match patch.kind {
        FileKind::Binary => return Err(PatchError::Binary),
        FileKind::Renamed => return Err(PatchError::Renamed),
        FileKind::Copied => return Err(PatchError::Copied),
        FileKind::Unsupported => return Err(PatchError::Unsupported),
        FileKind::Modified | FileKind::Added | FileKind::Deleted => {}
    }
    let PatchSelection::Hunk(selected) = *selection;
    if selected >= patch.hunks.len() {
        return Err(PatchError::NoSuchHunk);
    }
    let mut out = Vec::new();
    for line in &patch.header {
        if line.starts_with(b"diff --git ")
            || line.starts_with(b"--- ")
            || line.starts_with(b"+++ ")
        {
            out.extend_from_slice(line);
        }
    }
    // Hunks left out never reach the index, so later hunks land earlier
    // (or later) by however many lines each one would have added.
    let mut shift: i64 = 0;
    for (index, hunk) in patch.hunks.iter().enumerate() {
        if index != selected {
            shift += i64::from(hunk.new_len) - i64::from(hunk.old_len);
            continue;
        }
        write_hunk(&mut out, hunk, shift);
    }
    Ok(out)
}

fn write_hunk(out: &mut Vec<u8>, hunk: &Hunk, shift: i64) {
    let count = |wanted: fn(PatchLineKind) -> bool| {
        let n = hunk.lines.iter().filter(|l| wanted(l.kind)).count();
        u32::try_from(n).unwrap_or(u32::MAX)
    };
    let old_len = count(|k| k != PatchLineKind::Added);
    let new_len = count(|k| k != PatchLineKind::Removed);
    let new_start = (i64::from(hunk.new_start) - shift).max(0);
    out.extend_from_slice(
        format!(
            "@@ -{} +{} @@",
            range(i64::from(hunk.old_start), old_len),
            range(new_start, new_len)
        )
        .as_bytes(),
    );
    out.extend_from_slice(&hunk.heading);
    out.push(b'\n');
    for line in &hunk.lines {
        out.push(match line.kind {
            PatchLineKind::Context => b' ',
            PatchLineKind::Added => b'+',
            PatchLineKind::Removed => b'-',
        });
        out.extend_from_slice(&line.text);
        if line.no_newline {
            out.push(b'\n');
            out.extend_from_slice(NO_NEWLINE);
        }
    }
}

/// A side of an `@@` line, `start[,len]`. Git leaves out a length of one.
pub fn range(start: i64, len: u32) -> String {
    if len == 1 {
        start.to_string()
    } else {
        format!("{start},{len}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use PatchLineKind::{Added, Context, Removed};

    const TWO_HUNKS: &[u8] = include_bytes!("fixtures/patch/two_hunks.diff");
    const NEW_FILE: &[u8] = include_bytes!("fixtures/patch/new_file.diff");
    const DELETED: &[u8] = include_bytes!("fixtures/patch/deleted.diff");
    const RENAME: &[u8] = include_bytes!("fixtures/patch/rename.diff");
    const BINARY: &[u8] = include_bytes!("fixtures/patch/binary.diff");
    const NO_EOL_OLD: &[u8] = include_bytes!("fixtures/patch/no_eol_old.diff");
    const NO_EOL_NEW: &[u8] = include_bytes!("fixtures/patch/no_eol_new.diff");
    const NO_EOL_BOTH: &[u8] = include_bytes!("fixtures/patch/no_eol_both.diff");
    const CRLF: &[u8] = include_bytes!("fixtures/patch/crlf.diff");
    const BLANK_CONTEXT: &[u8] = include_bytes!("fixtures/patch/blank_context.diff");
    const QUOTED_PATH: &[u8] = include_bytes!("fixtures/patch/quoted_path.diff");
    const SUBMODULE: &[u8] = include_bytes!("fixtures/patch/submodule.diff");
    const COMBINED: &[u8] = include_bytes!("fixtures/patch/combined.diff");

    /// `(old_start, old_len, new_start, new_len)` of each hunk.
    fn ranges(patch: &FilePatch) -> Vec<(u32, u32, u32, u32)> {
        patch
            .hunks
            .iter()
            .map(|h| (h.old_start, h.old_len, h.new_start, h.new_len))
            .collect()
    }

    fn kinds(hunk: &Hunk) -> Vec<PatchLineKind> {
        hunk.lines.iter().map(|l| l.kind).collect()
    }

    fn texts(hunk: &Hunk) -> Vec<&[u8]> {
        hunk.lines.iter().map(|l| l.text.as_slice()).collect()
    }

    fn line(kind: PatchLineKind, text: &[u8], no_newline: bool) -> PatchLine {
        PatchLine {
            kind,
            text: text.to_vec(),
            no_newline,
        }
    }

    #[test]
    fn a_two_hunk_file_with_a_mode_change() {
        let patch = parse(TWO_HUNKS).unwrap();
        assert_eq!(patch.kind, FileKind::Modified);
        assert_eq!(patch.header.len(), 6);
        assert_eq!(patch.header[1], b"old mode 100644\n");
        assert_eq!(ranges(&patch), [(1, 4, 1, 5), (17, 4, 18, 4)]);
        assert_eq!(
            kinds(&patch.hunks[0]),
            [Removed, Added, Added, Context, Context, Context]
        );
        assert_eq!(
            texts(&patch.hunks[0])[..3],
            [&b"line 1\n"[..], b"line 1a\n", b"line 1b\n"]
        );
        assert_eq!(patch.hunks[0].heading, b"");
        assert_eq!(patch.hunks[1].heading, b" line 16");
        assert_eq!(
            kinds(&patch.hunks[1]),
            [Context, Context, Context, Removed, Added]
        );
    }

    #[test]
    fn a_new_file() {
        let patch = parse(NEW_FILE).unwrap();
        assert_eq!(patch.kind, FileKind::Added);
        assert_eq!(ranges(&patch), [(0, 0, 1, 2)]);
        assert_eq!(kinds(&patch.hunks[0]), [Added, Added]);
    }

    #[test]
    fn a_deleted_file() {
        let patch = parse(DELETED).unwrap();
        assert_eq!(patch.kind, FileKind::Deleted);
        assert_eq!(ranges(&patch), [(1, 1, 0, 0)]);
        assert_eq!(patch.hunks[0].lines, [line(Removed, b"gone\n", false)]);
    }

    #[test]
    fn a_rename() {
        let patch = parse(RENAME).unwrap();
        assert_eq!(patch.kind, FileKind::Renamed);
        assert_eq!(ranges(&patch), [(2, 4, 2, 4)]);
        assert_eq!(patch.hunks[0].heading, b" one");
    }

    #[test]
    fn a_copy() {
        let diff = b"diff --git a/a b/b\nsimilarity index 100%\ncopy from a\ncopy to b\n";
        assert_eq!(parse(diff).unwrap().kind, FileKind::Copied);
    }

    #[test]
    fn a_binary_file_in_either_form() {
        let patch = parse(BINARY).unwrap();
        assert_eq!(patch.kind, FileKind::Binary);
        assert!(patch.hunks.is_empty());
        let literal = b"diff --git a/x b/x\nindex 1..2 100644\nGIT binary patch\nliteral 5\nMcmZ\n\nliteral 0\nKcmV+b0001\n\n";
        // A binary patch has no `@@`, so its base85 body stays in the header.
        let patch = parse(literal).unwrap();
        assert_eq!(patch.kind, FileKind::Binary);
        assert!(patch.hunks.is_empty());
    }

    #[test]
    fn no_newline_on_the_old_side() {
        let patch = parse(NO_EOL_OLD).unwrap();
        assert_eq!(
            patch.hunks[0].lines,
            [
                line(Context, b"keep\n", false),
                line(Removed, b"last", true),
                line(Added, b"last\n", false),
            ]
        );
    }

    #[test]
    fn no_newline_on_the_new_side() {
        let patch = parse(NO_EOL_NEW).unwrap();
        assert_eq!(
            patch.hunks[0].lines,
            [
                line(Context, b"keep\n", false),
                line(Removed, b"last\n", false),
                line(Added, b"last", true),
            ]
        );
    }

    #[test]
    fn no_newline_on_both_sides() {
        let patch = parse(NO_EOL_BOTH).unwrap();
        assert_eq!(
            patch.hunks[0].lines,
            [
                line(Context, b"keep\n", false),
                line(Removed, b"old", true),
                line(Added, b"new", true),
            ]
        );
    }

    #[test]
    fn crlf_content_keeps_its_carriage_returns() {
        let patch = parse(CRLF).unwrap();
        assert_eq!(ranges(&patch), [(1, 4, 1, 4), (17, 4, 17, 4)]);
        assert_eq!(
            texts(&patch.hunks[0])[..2],
            [&b"row 1\r\n"[..], b"row 1 changed\r\n"]
        );
        assert_eq!(patch.hunks[1].heading, b" row 16");
    }

    #[test]
    fn a_bare_blank_line_inside_a_hunk_is_context() {
        let patch = parse(BLANK_CONTEXT).unwrap();
        assert_eq!(ranges(&patch), [(1, 5, 1, 5)]);
        assert_eq!(
            kinds(&patch.hunks[0]),
            [Context, Context, Removed, Added, Context, Context]
        );
        assert_eq!(texts(&patch.hunks[0])[1], b"\n");
    }

    #[test]
    fn a_quoted_path_stays_in_the_header_as_git_wrote_it() {
        let patch = parse(QUOTED_PATH).unwrap();
        assert_eq!(patch.kind, FileKind::Modified);
        assert_eq!(
            patch.header[0],
            b"diff --git \"a/tab\\there.txt\" \"b/tab\\there.txt\"\n"
        );
        assert_eq!(ranges(&patch), [(1, 3, 1, 3)]);
    }

    #[test]
    fn submodules_and_combined_diffs_are_unsupported() {
        let patch = parse(SUBMODULE).unwrap();
        assert_eq!(patch.kind, FileKind::Unsupported);
        assert_eq!(ranges(&patch), [(1, 1, 1, 1)]);

        let patch = parse(COMBINED).unwrap();
        assert_eq!(patch.kind, FileKind::Unsupported);
        assert!(patch.hunks.is_empty());
        assert_eq!(patch.header.concat(), COMBINED);
    }

    #[test]
    fn empty_input_is_a_file_with_no_hunks() {
        let patch = parse(b"").unwrap();
        assert!(patch.header.is_empty());
        assert!(patch.hunks.is_empty());
        assert_eq!(patch.kind, FileKind::Modified);
    }

    #[test]
    fn a_mode_change_alone_has_no_hunks() {
        let patch = parse(b"diff --git a/x b/x\nold mode 100644\nnew mode 100755\n").unwrap();
        assert_eq!(patch.kind, FileKind::Modified);
        assert!(patch.hunks.is_empty());
    }

    #[test]
    fn more_than_one_file_is_refused() {
        let two = [TWO_HUNKS, NEW_FILE].concat();
        assert_eq!(parse(&two), Err(PatchError::MultipleFiles));
        let headers_only = b"diff --git a/x b/x\nold mode 100644\nnew mode 100755\ndiff --git a/y b/y\nold mode 100644\nnew mode 100755\n";
        assert_eq!(parse(headers_only), Err(PatchError::MultipleFiles));
    }

    #[test]
    fn counts_that_do_not_match_the_lines_are_refused() {
        let head = "diff --git a/x b/x\n--- a/x\n+++ b/x\n";
        for body in [
            // Too few lines.
            "@@ -1,2 +1,2 @@\n a\n",
            // Too many lines: the extra one is not a hunk header.
            "@@ -1 +1 @@\n-a\n+b\n c\n",
            // An added line where only old lines are left.
            "@@ -1,2 +1,1 @@\n a\n+b\n",
            // Not a diff line at all.
            "@@ -1 +1 @@\n-a\n?b\n",
            // A marker with no line before it.
            "@@ -1 +1 @@\n\\ No newline at end of file\n-a\n+b\n",
            // A broken header.
            "@@ -x +1 @@\n a\n",
        ] {
            let diff = format!("{head}{body}");
            assert_eq!(
                parse(diff.as_bytes()),
                Err(PatchError::Malformed),
                "{body:?}"
            );
        }
    }

    #[test]
    fn a_hunk_header_without_lengths_means_one_line() {
        let patch =
            parse(b"diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -3 +3 @@ f\n-a\n+b\n").unwrap();
        assert_eq!(ranges(&patch), [(3, 1, 3, 1)]);
        assert_eq!(patch.hunks[0].heading, b" f");
    }

    fn built(diff: &[u8], hunk: usize) -> String {
        let patch = parse(diff).unwrap();
        String::from_utf8(build(&patch, &PatchSelection::Hunk(hunk)).unwrap()).unwrap()
    }

    #[test]
    fn building_the_first_hunk_drops_the_mode_lines() {
        assert_eq!(
            built(TWO_HUNKS, 0),
            "diff --git a/two.txt b/two.txt\n\
             --- a/two.txt\n\
             +++ b/two.txt\n\
             @@ -1,4 +1,5 @@\n\
             -line 1\n\
             +line 1a\n\
             +line 1b\n \
             line 2\n \
             line 3\n \
             line 4\n"
        );
    }

    #[test]
    fn building_the_second_hunk_shifts_its_new_start_back() {
        // Hunk 0 added a line; without it, hunk 1 starts at 17 in the index.
        assert_eq!(
            built(TWO_HUNKS, 1),
            "diff --git a/two.txt b/two.txt\n\
             --- a/two.txt\n\
             +++ b/two.txt\n\
             @@ -17,4 +17,4 @@ line 16\n \
             line 17\n \
             line 18\n \
             line 19\n\
             -line 20\n\
             +line twenty\n"
        );
    }

    #[test]
    fn a_shift_can_go_either_way() {
        let diff = b"diff --git a/x b/x\n--- a/x\n+++ b/x\n\
            @@ -1,3 +1 @@\n-a\n-b\n c\n\
            @@ -10,2 +8,3 @@\n d\n+e\n f\n";
        assert_eq!(
            built(diff, 1),
            "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -10,2 +10,3 @@\n d\n+e\n f\n"
        );
    }

    #[test]
    fn building_keeps_zero_length_starts() {
        // Deleting lines 1-2 only: git writes the empty new side as `+0,0`.
        let diff = b"diff --git a/x b/x\n--- a/x\n+++ b/x\n\
            @@ -1,2 +0,0 @@\n-a\n-b\n\
            @@ -9 +7,2 @@\n-c\n+d\n+e\n";
        assert_eq!(
            built(diff, 0),
            "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,2 +0,0 @@\n-a\n-b\n"
        );
        assert_eq!(
            built(diff, 1),
            "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -9 +9,2 @@\n-c\n+d\n+e\n"
        );
    }

    #[test]
    fn crlf_and_markers_come_back_byte_for_byte() {
        let patch = parse(CRLF).unwrap();
        let mut whole = build(&patch, &PatchSelection::Hunk(0)).unwrap();
        let second = build(&patch, &PatchSelection::Hunk(1)).unwrap();
        // The second hunk has no shift to apply, so both hunks together are
        // the fixture without its index line.
        whole.extend_from_slice(&second[second.iter().position(|&b| b == b'@').unwrap()..]);
        let without_index: Vec<u8> = CRLF
            .split_inclusive(|&b| b == b'\n')
            .filter(|l| !l.starts_with(b"index "))
            .flatten()
            .copied()
            .collect();
        assert_eq!(whole, without_index);

        for fixture in [NO_EOL_OLD, NO_EOL_NEW, NO_EOL_BOTH] {
            let patch = parse(fixture).unwrap();
            let built = build(&patch, &PatchSelection::Hunk(0)).unwrap();
            let expected: Vec<u8> = fixture
                .split_inclusive(|&b| b == b'\n')
                .filter(|l| !l.starts_with(b"index "))
                .flatten()
                .copied()
                .collect();
            assert_eq!(
                String::from_utf8_lossy(&built),
                String::from_utf8_lossy(&expected)
            );
        }
    }

    #[test]
    fn a_blank_context_line_is_written_with_its_prefix() {
        let built = built(BLANK_CONTEXT, 0);
        assert!(
            built.contains("@@ -1,5 +1,5 @@\n a\n \n-b\n+B\n"),
            "{built:?}"
        );
    }

    #[test]
    fn a_quoted_path_header_is_copied_verbatim() {
        let built = built(QUOTED_PATH, 0);
        assert!(
            built.starts_with(
                "diff --git \"a/tab\\there.txt\" \"b/tab\\there.txt\"\n\
                 --- \"a/tab\\there.txt\"\n\
                 +++ \"b/tab\\there.txt\"\n@@ -1,3 +1,3 @@\n"
            ),
            "{built:?}"
        );
    }

    #[test]
    fn build_refuses_what_cannot_be_staged_by_hunk() {
        let hunk0 = PatchSelection::Hunk(0);
        let cases = [
            (BINARY, PatchError::Binary),
            (RENAME, PatchError::Renamed),
            (SUBMODULE, PatchError::Unsupported),
            (COMBINED, PatchError::Unsupported),
        ];
        for (diff, error) in cases {
            assert_eq!(build(&parse(diff).unwrap(), &hunk0), Err(error));
        }
        let copy = parse(b"diff --git a/a b/b\ncopy from a\ncopy to b\n").unwrap();
        assert_eq!(build(&copy, &hunk0), Err(PatchError::Copied));
        let patch = parse(TWO_HUNKS).unwrap();
        assert_eq!(
            build(&patch, &PatchSelection::Hunk(2)),
            Err(PatchError::NoSuchHunk)
        );
        assert_eq!(
            build(&parse(b"").unwrap(), &hunk0),
            Err(PatchError::NoSuchHunk)
        );
    }

    #[test]
    fn every_error_has_a_short_message() {
        assert_eq!(
            PatchError::Binary.to_string(),
            "Binary file: stage it from the Files panel"
        );
        assert_eq!(
            PatchError::Renamed.to_string(),
            "Renamed file: stage it from the Files panel"
        );
    }

    #[test]
    fn only_a_single_hunk_selection_selects_everything() {
        let one = parse(QUOTED_PATH).unwrap();
        assert!(selects_everything(&one, &PatchSelection::Hunk(0)));
        let two = parse(TWO_HUNKS).unwrap();
        assert!(!selects_everything(&two, &PatchSelection::Hunk(0)));
        assert!(!selects_everything(&two, &PatchSelection::Hunk(1)));
        let new_file = parse(NEW_FILE).unwrap();
        assert!(selects_everything(&new_file, &PatchSelection::Hunk(0)));
    }
}
