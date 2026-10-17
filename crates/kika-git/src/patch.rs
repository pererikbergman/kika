//! The patch engine: parse one file's plain unified diff into hunks, and
//! build a patch from a hunk or a set of changed lines for
//! `git apply --cached`, forward or `--reverse`. Pure,
//! and bytes throughout: patch content is never decoded, so CRLF, invalid
//! UTF-8 and missing final newlines round-trip exactly. Only the view
//! sanitizes, when it draws.

use std::collections::BTreeSet;
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

/// Which side of the diff a changed line is numbered on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LineSide {
    Old,
    New,
}

/// A changed line, by identity rather than position in the hunk: a removed
/// line by its old line number, an added line by its new one. The path and
/// diff side are those of the `FilePatch` it was taken from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LineId {
    pub side: LineSide,
    pub line: u32,
}

impl LineId {
    /// A removed line, by its old line number.
    pub fn removed(line: u32) -> Self {
        Self {
            side: LineSide::Old,
            line,
        }
    }

    /// An added line, by its new line number.
    pub fn added(line: u32) -> Self {
        Self {
            side: LineSide::New,
            line,
        }
    }
}

/// Which part of a file's diff to put in a patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchSelection {
    Hunk(usize),
    /// Changed lines, from any hunks.
    Lines(BTreeSet<LineId>),
}

/// Which way the patch goes into the index. `Unstage` patches are applied
/// with `git apply --cached --reverse`, so git searches their new side.
/// `Unstage` also serves worktree discards (`git apply --reverse`), whose
/// new side is the worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Stage,
    Unstage,
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
    EmptySelection,
    /// A selected id is not a changed line of this diff.
    NoSuchLine,
    /// A partial hunk would be written without a context line to anchor it.
    NoContext,
    /// A line without a newline would stop being the last line of a side.
    NoNewlineSplit,
    PartialNewFile,
    PartialDeletedFile,
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
            PatchError::EmptySelection => "Nothing selected",
            PatchError::NoSuchLine => "The selected lines are not in this diff",
            PatchError::NoContext => {
                "No unchanged lines around the selection: stage the whole hunk"
            }
            PatchError::NoNewlineSplit => {
                "The last line has no newline: select it with the line it replaces"
            }
            PatchError::PartialNewFile => "New file: press Esc, then Space for the whole file",
            PatchError::PartialDeletedFile => {
                "Deleted file: press Esc, then Space for the whole file"
            }
        }
    }
}

impl PatchError {
    /// The sentence for a commit's change, where the Files panel cannot help: these
    /// files are refused by the custom patch, not staged elsewhere.
    pub fn in_commit_message(self) -> &'static str {
        match self {
            PatchError::Binary => "Binary file: it cannot go into the custom patch",
            PatchError::Renamed => "Renamed file: it cannot go into the custom patch",
            PatchError::Copied => "Copied file: it cannot go into the custom patch",
            PatchError::Unsupported => "This change cannot go into the custom patch",
            other => other.message(),
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
    let mut hunks: Vec<Hunk> = Vec::new();
    while let Some(line) = lines.next() {
        if line.starts_with(b"diff ") {
            return Err(PatchError::MultipleFiles);
        }
        let mut hunk = parse_hunk_header(line)?;
        // A line id names one line of the file, so hunks must be in order
        // and never overlap.
        if let Some(prev) = hunks.last()
            && (first_line(hunk.old_start, hunk.old_len) < end_line(prev.old_start, prev.old_len)
                || first_line(hunk.new_start, hunk.new_len)
                    < end_line(prev.new_start, prev.new_len))
        {
            return Err(PatchError::Malformed);
        }
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
    // Every line, and the position after the last, must have a `u32` number.
    if end_line(old_start, old_len) > i64::from(u32::MAX)
        || end_line(new_start, new_len) > i64::from(u32::MAX)
    {
        return Err(PatchError::Malformed);
    }
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

impl Hunk {
    /// The id of each line, `None` for context. Numbers count from the
    /// `@@` starts; a side of length 0 starts after its "line before".
    /// Counting is wide, so a hunk built by hand past `u32::MAX` gives no id
    /// rather than a wrapped one (`parse` refuses such headers).
    pub fn line_ids(&self) -> Vec<Option<LineId>> {
        let mut old = first_line(self.old_start, self.old_len);
        let mut new = first_line(self.new_start, self.new_len);
        let id = |n: i64, make: fn(u32) -> LineId| u32::try_from(n).ok().map(make);
        self.lines
            .iter()
            .map(|line| match line.kind {
                PatchLineKind::Context => {
                    old += 1;
                    new += 1;
                    None
                }
                PatchLineKind::Removed => {
                    old += 1;
                    id(old - 1, LineId::removed)
                }
                PatchLineKind::Added => {
                    new += 1;
                    id(new - 1, LineId::added)
                }
            })
            .collect()
    }
}

/// Where a side's first line is. Git writes a side of length 0 with the
/// line before it as the start.
fn first_line(start: u32, len: u32) -> i64 {
    i64::from(start) + i64::from(len == 0)
}

/// The position just after a side's last line.
fn end_line(start: u32, len: u32) -> i64 {
    first_line(start, len) + i64::from(len)
}

/// Every changed line of the file.
fn all_changes(patch: &FilePatch) -> BTreeSet<LineId> {
    patch
        .hunks
        .iter()
        .flat_map(Hunk::line_ids)
        .flatten()
        .collect()
}

/// Whether applying the selection would take the whole file's change, so
/// staging the file itself does the same with less risk.
pub fn selects_everything(patch: &FilePatch, selection: &PatchSelection) -> bool {
    match selection {
        PatchSelection::Hunk(index) => patch.hunks.len() == 1 && *index == 0,
        PatchSelection::Lines(ids) => !ids.is_empty() && *ids == all_changes(patch),
    }
}

/// The patch for `selection`: the `diff --git`, `---` and `+++` lines
/// verbatim, then each hunk with a selected line, its counts and starts
/// recomputed. Mode, index and similarity lines are left out, so a hunk
/// never carries a mode change with it.
///
/// Unselected changes are turned into what the index already holds: for
/// `Stage`, an unselected removal stays as context and an unselected
/// addition is dropped; for `Unstage`, the mirror image.
pub fn build(
    patch: &FilePatch,
    selection: &PatchSelection,
    direction: Direction,
) -> Result<Vec<u8>, PatchError> {
    match patch.kind {
        FileKind::Binary => return Err(PatchError::Binary),
        FileKind::Renamed => return Err(PatchError::Renamed),
        FileKind::Copied => return Err(PatchError::Copied),
        FileKind::Unsupported => return Err(PatchError::Unsupported),
        FileKind::Modified | FileKind::Added | FileKind::Deleted => {}
    }
    let chosen = chosen_lines(patch, selection)?;
    let mut out = Vec::new();
    for line in &patch.header {
        if line.starts_with(b"diff --git ")
            || line.starts_with(b"--- ")
            || line.starts_with(b"+++ ")
        {
            out.extend_from_slice(line);
        }
    }
    // What git searches is the index, so only the other side moves: by
    // however many lines the earlier hunks' left-out changes would add.
    let mut shift: i64 = 0;
    for (hunk, chosen) in patch.hunks.iter().zip(&chosen) {
        let full = i64::from(hunk.new_len) - i64::from(hunk.old_len);
        if !chosen.contains(&true) {
            shift += full;
            continue;
        }
        let whole = hunk
            .lines
            .iter()
            .zip(chosen)
            .all(|(line, &c)| c || line.kind == PatchLineKind::Context);
        let lines = transform(hunk, chosen, direction);
        let (old_len, new_len) = side_lens(&lines);
        if !whole && old_len > 0 && !lines.iter().any(|(k, _)| *k == PatchLineKind::Context) {
            return Err(PatchError::NoContext);
        }
        check_no_newline(&lines)?;
        let mut old_pos = first_line(hunk.old_start, hunk.old_len);
        let mut new_pos = first_line(hunk.new_start, hunk.new_len);
        match direction {
            Direction::Stage => new_pos -= shift,
            Direction::Unstage => old_pos += shift,
        }
        write_hunk(
            &mut out,
            &hunk.heading,
            &lines,
            (start(old_pos, old_len), old_len),
            (start(new_pos, new_len), new_len),
        );
        shift += full - (i64::from(new_len) - i64::from(old_len));
    }
    Ok(out)
}

/// For each hunk, which of its lines are selected; context never is.
fn chosen_lines(
    patch: &FilePatch,
    selection: &PatchSelection,
) -> Result<Vec<Vec<bool>>, PatchError> {
    match selection {
        PatchSelection::Hunk(selected) => {
            if *selected >= patch.hunks.len() {
                return Err(PatchError::NoSuchHunk);
            }
            Ok(patch
                .hunks
                .iter()
                .enumerate()
                .map(|(index, hunk)| {
                    hunk.lines
                        .iter()
                        .map(|l| index == *selected && l.kind != PatchLineKind::Context)
                        .collect()
                })
                .collect())
        }
        PatchSelection::Lines(ids) => {
            if ids.is_empty() {
                return Err(PatchError::EmptySelection);
            }
            let all = all_changes(patch);
            if !ids.is_subset(&all) {
                return Err(PatchError::NoSuchLine);
            }
            // A partial patch with `/dev/null` headers does not round-trip.
            if *ids != all {
                match patch.kind {
                    FileKind::Added => return Err(PatchError::PartialNewFile),
                    FileKind::Deleted => return Err(PatchError::PartialDeletedFile),
                    _ => {}
                }
            }
            Ok(patch
                .hunks
                .iter()
                .map(|hunk| {
                    hunk.line_ids()
                        .into_iter()
                        .map(|id| id.is_some_and(|id| ids.contains(&id)))
                        .collect()
                })
                .collect())
        }
    }
}

/// The hunk's lines as they go into the patch, each with its written kind.
/// Text bytes are never touched, so a line turned into context keeps its
/// line ending exactly.
fn transform<'a>(
    hunk: &'a Hunk,
    chosen: &[bool],
    direction: Direction,
) -> Vec<(PatchLineKind, &'a PatchLine)> {
    use PatchLineKind::{Added, Context, Removed};
    hunk.lines
        .iter()
        .zip(chosen)
        .filter_map(|(line, &chosen)| {
            let kind = match (line.kind, chosen, direction) {
                (Context, _, _) => Context,
                (kind, true, _) => kind,
                (Removed, false, Direction::Stage) | (Added, false, Direction::Unstage) => Context,
                (Added, false, Direction::Stage) | (Removed, false, Direction::Unstage) => {
                    return None;
                }
            };
            Some((kind, line))
        })
        .collect()
}

/// How many lines each side of the written hunk has.
fn side_lens(lines: &[(PatchLineKind, &PatchLine)]) -> (u32, u32) {
    let count = |skip: PatchLineKind| {
        let n = lines.iter().filter(|(kind, _)| *kind != skip).count();
        u32::try_from(n).unwrap_or(u32::MAX)
    };
    (count(PatchLineKind::Added), count(PatchLineKind::Removed))
}

/// A line without a newline can only be the last line of its side; one that
/// would be followed by another line means staging a newline change the
/// user did not pick.
fn check_no_newline(lines: &[(PatchLineKind, &PatchLine)]) -> Result<(), PatchError> {
    for skip in [PatchLineKind::Added, PatchLineKind::Removed] {
        let side: Vec<&PatchLine> = lines
            .iter()
            .filter(|(kind, _)| *kind != skip)
            .map(|(_, line)| *line)
            .collect();
        if let Some((_, before_last)) = side.split_last()
            && before_last.iter().any(|line| line.no_newline)
        {
            return Err(PatchError::NoNewlineSplit);
        }
    }
    Ok(())
}

/// A side's `@@` start: its first line, or the line before when it is empty.
fn start(first: i64, len: u32) -> i64 {
    let start = if len == 0 { first - 1 } else { first };
    start.max(0)
}

fn write_hunk(
    out: &mut Vec<u8>,
    heading: &[u8],
    lines: &[(PatchLineKind, &PatchLine)],
    old: (i64, u32),
    new: (i64, u32),
) {
    out.extend_from_slice(
        format!("@@ -{} +{} @@", range(old.0, old.1), range(new.0, new.1)).as_bytes(),
    );
    out.extend_from_slice(heading);
    out.push(b'\n');
    for (kind, line) in lines {
        out.push(match kind {
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
        String::from_utf8(build(&patch, &PatchSelection::Hunk(hunk), Direction::Stage).unwrap())
            .unwrap()
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
        let mut whole = build(&patch, &PatchSelection::Hunk(0), Direction::Stage).unwrap();
        let second = build(&patch, &PatchSelection::Hunk(1), Direction::Stage).unwrap();
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
            let built = build(&patch, &PatchSelection::Hunk(0), Direction::Stage).unwrap();
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
            assert_eq!(
                build(&parse(diff).unwrap(), &hunk0, Direction::Stage),
                Err(error)
            );
        }
        let copy = parse(b"diff --git a/a b/b\ncopy from a\ncopy to b\n").unwrap();
        assert_eq!(
            build(&copy, &hunk0, Direction::Stage),
            Err(PatchError::Copied)
        );
        let patch = parse(TWO_HUNKS).unwrap();
        assert_eq!(
            build(&patch, &PatchSelection::Hunk(2), Direction::Stage),
            Err(PatchError::NoSuchHunk)
        );
        assert_eq!(
            build(&parse(b"").unwrap(), &hunk0, Direction::Stage),
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
    fn a_commits_change_names_the_custom_patch_not_the_files_panel() {
        assert_eq!(
            PatchError::Binary.in_commit_message(),
            "Binary file: it cannot go into the custom patch"
        );
        assert_eq!(
            PatchError::Renamed.in_commit_message(),
            "Renamed file: it cannot go into the custom patch"
        );
        assert_eq!(
            PatchError::Copied.in_commit_message(),
            "Copied file: it cannot go into the custom patch"
        );
        assert_eq!(
            PatchError::Unsupported.in_commit_message(),
            "This change cannot go into the custom patch"
        );
        assert_eq!(
            PatchError::NoSuchHunk.in_commit_message(),
            PatchError::NoSuchHunk.message()
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

    const MIXED: &[u8] = b"diff --git a/m b/m\n--- a/m\n+++ b/m\n\
        @@ -1,6 +1,6 @@ h\n a\n-b\n-c\n+B\n+C\n d\n-e\n+E\n f\n";
    /// A pure addition, then a pure deletion further down.
    const MULTI: &[u8] = b"diff --git a/m b/m\n--- a/m\n+++ b/m\n\
        @@ -1,4 +1,6 @@\n a\n+n1\n+n2\n b\n c\n d\n\
        @@ -10,8 +12,6 @@ g\n h\n i\n j\n-k\n-l\n m\n n\n o\n";
    /// An empty file that got two lines, and one that lost them.
    const FILLED: &[u8] = b"diff --git a/e b/e\n--- a/e\n+++ b/e\n@@ -0,0 +1,2 @@\n+x\n+y\n";
    const EMPTIED: &[u8] = b"diff --git a/e b/e\n--- a/e\n+++ b/e\n@@ -1,2 +0,0 @@\n-x\n-y\n";
    const HEAD_M: &str = "diff --git a/m b/m\n--- a/m\n+++ b/m\n";
    const HEAD_E: &str = "diff --git a/e b/e\n--- a/e\n+++ b/e\n";

    fn lines(diff: &[u8], ids: &[LineId], direction: Direction) -> Result<String, PatchError> {
        let selection = PatchSelection::Lines(ids.iter().copied().collect());
        build(&parse(diff).unwrap(), &selection, direction)
            .map(|bytes| String::from_utf8(bytes).unwrap())
    }

    /// The built patch for `ids` in both directions, without the header.
    fn both(diff: &[u8], ids: &[LineId]) -> (String, String) {
        let body = |direction| {
            let built = lines(diff, ids, direction).unwrap();
            built[built.find("@@").unwrap()..].to_string()
        };
        (body(Direction::Stage), body(Direction::Unstage))
    }

    use Direction::{Stage, Unstage};

    #[test]
    fn a_single_added_line() {
        let (stage, unstage) = both(MIXED, &[LineId::added(2)]);
        // The other removals stay as context; the other additions go.
        assert_eq!(stage, "@@ -1,6 +1,7 @@ h\n a\n b\n c\n+B\n d\n e\n f\n");
        // Mirrored: the other additions are already in the index.
        assert_eq!(unstage, "@@ -1,5 +1,6 @@ h\n a\n+B\n C\n d\n E\n f\n");
        assert!(
            lines(MIXED, &[LineId::added(2)], Stage)
                .unwrap()
                .starts_with(HEAD_M)
        );
    }

    #[test]
    fn a_single_removed_line() {
        let (stage, unstage) = both(MIXED, &[LineId::removed(5)]);
        assert_eq!(stage, "@@ -1,6 +1,5 @@ h\n a\n b\n c\n d\n-e\n f\n");
        assert_eq!(unstage, "@@ -1,7 +1,6 @@ h\n a\n B\n C\n d\n-e\n E\n f\n");
    }

    #[test]
    fn a_whole_mixed_hunk_but_one_line() {
        let ids = [
            LineId::removed(2),
            LineId::removed(3),
            LineId::added(2),
            LineId::removed(5),
            LineId::added(5),
        ];
        let (stage, unstage) = both(MIXED, &ids);
        assert_eq!(stage, "@@ -1,6 +1,5 @@ h\n a\n-b\n-c\n+B\n d\n-e\n+E\n f\n");
        assert_eq!(
            unstage,
            "@@ -1,7 +1,6 @@ h\n a\n-b\n-c\n+B\n C\n d\n-e\n+E\n f\n"
        );
    }

    #[test]
    fn the_first_and_last_change_lines_of_a_hunk() {
        let (stage, unstage) = both(MIXED, &[LineId::removed(2), LineId::added(5)]);
        assert_eq!(stage, "@@ -1,6 +1,6 @@ h\n a\n-b\n c\n d\n e\n+E\n f\n");
        assert_eq!(unstage, "@@ -1,6 +1,6 @@ h\n a\n-b\n B\n C\n d\n+E\n f\n");
    }

    #[test]
    fn a_pure_addition_partly_selected() {
        let (stage, unstage) = both(MULTI, &[LineId::added(2)]);
        assert_eq!(stage, "@@ -1,4 +1,5 @@\n a\n+n1\n b\n c\n d\n");
        assert_eq!(unstage, "@@ -1,5 +1,6 @@\n a\n+n1\n n2\n b\n c\n d\n");

        // An empty old side starts at the line before it, and stops doing so
        // once a line lands on it.
        let (stage, unstage) = both(FILLED, &[LineId::added(2)]);
        assert_eq!(stage, "@@ -0,0 +1 @@\n+y\n");
        assert_eq!(unstage, "@@ -1 +1,2 @@\n x\n+y\n");
    }

    #[test]
    fn a_pure_deletion_partly_selected() {
        // Hunk 0 is left out, so the index-side start moves by its whole delta.
        let (stage, unstage) = both(MULTI, &[LineId::removed(13)]);
        assert_eq!(
            stage,
            "@@ -10,8 +10,7 @@ g\n h\n i\n j\n-k\n l\n m\n n\n o\n"
        );
        assert_eq!(unstage, "@@ -12,7 +12,6 @@ g\n h\n i\n j\n-k\n m\n n\n o\n");

        let stage = lines(EMPTIED, &[LineId::removed(1)], Stage).unwrap();
        assert_eq!(stage, format!("{HEAD_E}@@ -1,2 +1 @@\n-x\n y\n"));
        // Mirrored, nothing is left around the line to anchor it.
        assert_eq!(
            lines(EMPTIED, &[LineId::removed(1)], Unstage),
            Err(PatchError::NoContext)
        );
    }

    #[test]
    fn a_later_hunk_shifts_by_what_earlier_hunks_actually_wrote() {
        let (stage, unstage) = both(MULTI, &[LineId::added(2), LineId::removed(13)]);
        assert_eq!(
            stage,
            "@@ -1,4 +1,5 @@\n a\n+n1\n b\n c\n d\n\
             @@ -10,8 +11,7 @@ g\n h\n i\n j\n-k\n l\n m\n n\n o\n"
        );
        assert_eq!(
            unstage,
            "@@ -1,5 +1,6 @@\n a\n+n1\n n2\n b\n c\n d\n\
             @@ -11,7 +12,6 @@ g\n h\n i\n j\n-k\n m\n n\n o\n"
        );
    }

    #[test]
    fn crlf_lines_keep_their_carriage_returns() {
        let (stage, unstage) = both(CRLF, &[LineId::added(1)]);
        assert_eq!(
            stage,
            "@@ -1,4 +1,5 @@\n row 1\r\n+row 1 changed\r\n row 2\r\n row 3\r\n row 4\r\n"
        );
        assert_eq!(
            unstage,
            "@@ -1,3 +1,4 @@\n+row 1 changed\r\n row 2\r\n row 3\r\n row 4\r\n"
        );
    }

    #[test]
    fn a_line_without_a_newline_must_stay_last_on_its_side() {
        const MARK: &str = "\\ No newline at end of file\n";
        let split = Err(PatchError::NoNewlineSplit);
        // Old side lacks the newline.
        assert_eq!(lines(NO_EOL_OLD, &[LineId::added(2)], Stage), split);
        assert_eq!(lines(NO_EOL_OLD, &[LineId::removed(2)], Unstage), split);
        let (stage, unstage) = (
            lines(NO_EOL_OLD, &[LineId::removed(2)], Stage).unwrap(),
            lines(NO_EOL_OLD, &[LineId::added(2)], Unstage).unwrap(),
        );
        assert!(stage.ends_with(&format!("@@ -1,2 +1 @@\n keep\n-last\n{MARK}")));
        assert!(unstage.ends_with("@@ -1 +1,2 @@\n keep\n+last\n"));

        // New side lacks it: every single line is fine.
        let (stage, unstage) = both(NO_EOL_NEW, &[LineId::added(2)]);
        assert_eq!(
            stage,
            format!("@@ -1,2 +1,3 @@\n keep\n last\n+last\n{MARK}")
        );
        assert_eq!(unstage, format!("@@ -1 +1,2 @@\n keep\n+last\n{MARK}"));
        let (stage, unstage) = both(NO_EOL_NEW, &[LineId::removed(2)]);
        assert_eq!(stage, "@@ -1,2 +1 @@\n keep\n-last\n");
        assert_eq!(
            unstage,
            format!("@@ -1,3 +1,2 @@\n keep\n-last\n last\n{MARK}")
        );

        // Both sides lack it.
        assert_eq!(lines(NO_EOL_BOTH, &[LineId::added(2)], Stage), split);
        assert_eq!(lines(NO_EOL_BOTH, &[LineId::removed(2)], Unstage), split);
        let stage = lines(NO_EOL_BOTH, &[LineId::removed(2)], Stage).unwrap();
        assert!(stage.ends_with(&format!("@@ -1,2 +1 @@\n keep\n-old\n{MARK}")));
        let unstage = lines(NO_EOL_BOTH, &[LineId::added(2)], Unstage).unwrap();
        assert!(unstage.ends_with(&format!("@@ -1 +1,2 @@\n keep\n+new\n{MARK}")));
        // Both lines together are the whole hunk, which always builds.
        let whole = [LineId::removed(2), LineId::added(2)];
        assert!(lines(NO_EOL_BOTH, &whole, Stage).is_ok());
        assert!(lines(NO_EOL_BOTH, &whole, Unstage).is_ok());
    }

    #[test]
    fn unstaging_a_later_hunk_keeps_its_index_start_and_shifts_the_other() {
        let patch = parse(TWO_HUNKS).unwrap();
        let unstage = build(&patch, &PatchSelection::Hunk(1), Unstage).unwrap();
        let unstage = String::from_utf8(unstage).unwrap();
        assert!(unstage.contains("@@ -18,4 +18,4 @@ line 16\n"), "{unstage}");
        // Stage output is what #12 wrote.
        assert!(built(TWO_HUNKS, 1).contains("@@ -17,4 +17,4 @@ line 16\n"));
        // Hunk 0 has no hunk before it: the same either way.
        let first = build(&patch, &PatchSelection::Hunk(0), Unstage).unwrap();
        assert!(
            String::from_utf8(first)
                .unwrap()
                .contains("@@ -1,4 +1,5 @@\n")
        );
    }

    #[test]
    fn line_selections_on_what_cannot_be_staged_by_line_are_refused() {
        let any = [LineId::added(1)];
        for (diff, error) in [
            (RENAME, PatchError::Renamed),
            (BINARY, PatchError::Binary),
            (SUBMODULE, PatchError::Unsupported),
        ] {
            for direction in [Stage, Unstage] {
                assert_eq!(lines(diff, &any, direction), Err(error));
            }
        }

        assert_eq!(
            lines(NEW_FILE, &[LineId::added(1)], Stage),
            Err(PatchError::PartialNewFile)
        );
        let deleted = b"diff --git a/g b/g\ndeleted file mode 100644\n--- a/g\n+++ /dev/null\n\
            @@ -1,2 +0,0 @@\n-x\n-y\n";
        assert_eq!(
            lines(deleted, &[LineId::removed(2)], Unstage),
            Err(PatchError::PartialDeletedFile)
        );
        // Whole files build, and say they are the whole file.
        for (diff, ids) in [
            (NEW_FILE, &[LineId::added(1), LineId::added(2)][..]),
            (DELETED, &[LineId::removed(1)][..]),
            (deleted, &[LineId::removed(1), LineId::removed(2)][..]),
        ] {
            assert!(lines(diff, ids, Stage).is_ok());
            let selection = PatchSelection::Lines(ids.iter().copied().collect());
            assert!(selects_everything(&parse(diff).unwrap(), &selection));
        }
    }

    #[test]
    fn empty_unknown_and_unanchored_selections_are_refused() {
        assert_eq!(lines(MIXED, &[], Stage), Err(PatchError::EmptySelection));
        // Line 4 is context, and nothing is numbered 99.
        for id in [LineId::removed(4), LineId::added(4), LineId::added(99)] {
            assert_eq!(lines(MIXED, &[id], Stage), Err(PatchError::NoSuchLine));
        }
        // A whole-file rewrite has no context; part of it has nothing to anchor to.
        let rewrite = b"diff --git a/r b/r\n--- a/r\n+++ b/r\n@@ -1 +1,2 @@\n-a\n+b\n+c\n";
        let part = [LineId::removed(1), LineId::added(1)];
        assert_eq!(lines(rewrite, &part, Stage), Err(PatchError::NoContext));
        // The whole of it is the hunk as git wrote it.
        let all = [LineId::removed(1), LineId::added(1), LineId::added(2)];
        assert!(
            lines(rewrite, &all, Stage)
                .unwrap()
                .ends_with("@@ -1 +1,2 @@\n-a\n+b\n+c\n")
        );
        assert_eq!(
            PatchError::NoContext.to_string(),
            "No unchanged lines around the selection: stage the whole hunk"
        );
    }

    #[test]
    fn line_ids_number_changes_from_the_hunk_starts() {
        let patch = parse(MULTI).unwrap();
        assert_eq!(
            patch.hunks[0].line_ids(),
            [
                None,
                Some(LineId::added(2)),
                Some(LineId::added(3)),
                None,
                None,
                None
            ]
        );
        assert_eq!(
            patch.hunks[1].line_ids()[3..5],
            [Some(LineId::removed(13)), Some(LineId::removed(14))]
        );
        assert_eq!(
            parse(FILLED).unwrap().hunks[0].line_ids()[0],
            Some(LineId::added(1))
        );
    }

    #[test]
    fn a_lines_selection_selects_everything_only_when_it_is_every_change() {
        let patch = parse(TWO_HUNKS).unwrap();
        let every: BTreeSet<LineId> = patch
            .hunks
            .iter()
            .flat_map(Hunk::line_ids)
            .flatten()
            .collect();
        assert_eq!(every.len(), 5);
        assert!(selects_everything(
            &patch,
            &PatchSelection::Lines(every.clone())
        ));
        let mut part = every;
        part.pop_last();
        assert!(!selects_everything(&patch, &PatchSelection::Lines(part)));
        assert!(!selects_everything(
            &patch,
            &PatchSelection::Lines(BTreeSet::new())
        ));
    }

    #[test]
    fn hunk_headers_whose_lines_would_overflow_are_refused() {
        let head = "diff --git a/x b/x\n--- a/x\n+++ b/x\n";
        for body in [
            "@@ -4294967295,2 +4294967295,2 @@\n a\n-b\n+c\n",
            "@@ -4294967295 +4294967295 @@\n-a\n+b\n",
            "@@ -4294967295,0 +1 @@\n+a\n",
            "@@ -1 +4294967295,0 @@\n-a\n",
        ] {
            let diff = format!("{head}{body}");
            assert_eq!(
                parse(diff.as_bytes()),
                Err(PatchError::Malformed),
                "{body:?}"
            );
        }
        // The largest start that still fits parses.
        let fits = format!("{head}@@ -4294967294 +4294967294 @@\n-a\n+b\n");
        assert!(parse(fits.as_bytes()).is_ok());
    }

    #[test]
    fn a_hand_built_hunk_at_the_top_of_the_range_numbers_without_panicking() {
        let hunk = Hunk {
            old_start: u32::MAX,
            old_len: 2,
            new_start: u32::MAX,
            new_len: 2,
            heading: Vec::new(),
            lines: vec![
                line(Removed, b"a\n", false),
                line(Removed, b"b\n", false),
                line(Added, b"c\n", false),
                line(Added, b"d\n", false),
            ],
        };
        // Only lines with a number that fits get an id.
        assert_eq!(
            hunk.line_ids(),
            [
                Some(LineId::removed(u32::MAX)),
                None,
                Some(LineId::added(u32::MAX)),
                None
            ]
        );
        let patch = FilePatch {
            header: Vec::new(),
            kind: FileKind::Modified,
            hunks: vec![hunk],
        };
        let selection = PatchSelection::Lines([LineId::removed(u32::MAX)].into());
        assert!(build(&patch, &selection, Direction::Stage).is_ok());
    }

    #[test]
    fn hunks_that_overlap_or_go_backwards_are_refused() {
        let head = "diff --git a/x b/x\n--- a/x\n+++ b/x\n";
        for body in [
            // Out of order.
            "@@ -10 +10 @@\n-a\n+b\n@@ -1 +1 @@\n-c\n+d\n",
            // Overlapping on both sides.
            "@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n@@ -3 +3 @@\n-c\n+C\n",
            // Overlapping on the new side only.
            "@@ -1,2 +1,4 @@\n a\n+x\n+y\n b\n@@ -5 +3 @@\n-e\n+E\n",
        ] {
            let diff = format!("{head}{body}");
            assert_eq!(
                parse(diff.as_bytes()),
                Err(PatchError::Malformed),
                "{body:?}"
            );
        }
        // Hunks that only touch are fine, as are empty sides between them.
        let touching = format!("{head}@@ -1 +1 @@\n-a\n+b\n@@ -2 +2 @@\n-c\n+d\n");
        assert_eq!(parse(touching.as_bytes()).unwrap().hunks.len(), 2);
    }
}
