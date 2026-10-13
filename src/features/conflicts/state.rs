//! A file git left with conflict markers, read into blocks the user can
//! pick a side of, and written back with the picks applied. Pure: the
//! file is read and written by effects.
//!
//! Every style git writes is understood: plain merge markers, and diff3 or
//! zdiff3 with the common base between `|||||||` and `=======`. Bytes
//! outside the markers, line endings included, are kept exactly, so a file
//! with no picks renders back to what was read.

use crate::git::path::RepoPath;
use crate::git::safe_text::sanitize;
use crate::intent::Scroll;

/// The marker size git uses unless a `conflict-marker-size` attribute
/// says otherwise.
pub const DEFAULT_MARKER_SIZE: usize = 7;

/// The largest file the view reads. A conflicted file is held in memory
/// and its rows drawn from it; one far past this is better left to the
/// merge tool.
pub const MAX_CONFLICT_FILE: u64 = 8 * 1024 * 1024;

/// A conflicted file: text git merged cleanly, and the blocks it could not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictFile {
    segments: Vec<Segment>,
}

/// One stretch of a conflicted file, in file order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// Lines outside any block, with their own line endings.
    Text(Vec<u8>),
    Block(Block),
}

/// One conflict: both sides, the base when git wrote one, and the exact
/// bytes of the whole block so an unpicked one is written back unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub ours: Vec<u8>,
    /// Only in the diff3 and zdiff3 styles.
    pub base: Option<Vec<u8>>,
    pub theirs: Vec<u8>,
    /// What git wrote after `<<<<<<<`, safe to draw.
    pub ours_label: String,
    /// What git wrote after `>>>>>>>`, safe to draw.
    pub theirs_label: String,
    /// The block as read, marker lines included.
    pub raw: Vec<u8>,
}

/// Which side of a block to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Ours,
    Theirs,
    /// Ours, then theirs.
    Both,
}

impl Choice {
    pub fn name(self) -> &'static str {
        match self {
            Choice::Ours => "ours",
            Choice::Theirs => "theirs",
            Choice::Both => "both",
        }
    }
}

/// Why a file's markers cannot be read without guessing. Kika refuses
/// such a file rather than write back something the user did not mean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// A NUL byte: not a text file.
    Binary,
    /// No `<<<<<<<` anywhere.
    NoConflicts,
    /// A marker where none can be, on this 1-based line.
    Misplaced { line: usize, marker: char },
    /// A second `=======` in one block: either could split the sides.
    TwoSeparators { line: usize },
    /// The file ends inside a block.
    Unterminated,
}

impl ParseError {
    /// For the user, who will resolve the file elsewhere.
    pub fn message(&self) -> String {
        match self {
            ParseError::Binary => "This is a binary file.".to_string(),
            ParseError::NoConflicts => "No conflict markers were found in this file.".to_string(),
            ParseError::Misplaced { line, marker } => {
                let run = marker.to_string().repeat(DEFAULT_MARKER_SIZE);
                format!("Line {line}: a {run} marker where none belongs.")
            }
            ParseError::TwoSeparators { line } => format!(
                "Line {line}: a second ======= in one conflict, so its sides are ambiguous."
            ),
            ParseError::Unterminated => {
                "The file ends inside a conflict, before its >>>>>>> marker.".to_string()
            }
        }
    }
}

/// A marker line: which of the four, and the label after it, if any.
struct Marker<'a> {
    kind: u8,
    label: &'a [u8],
}

/// `line` without its `\n`, and a `\r` before it.
fn content(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// Whether `line` is a marker of exactly `size` characters. A longer run
/// of the same character is content: git uses longer markers for a
/// conflict inside a conflict.
fn marker(line: &[u8], size: usize) -> Option<Marker<'_>> {
    let text = content(line);
    let &kind = text.first()?;
    if !matches!(kind, b'<' | b'|' | b'=' | b'>') || text.len() < size {
        return None;
    }
    let (run, rest) = text.split_at(size);
    if run.iter().any(|&b| b != kind) {
        return None;
    }
    match rest {
        [] => Some(Marker { kind, label: rest }),
        [b' ', label @ ..] if kind != b'=' => Some(Marker { kind, label }),
        _ => None,
    }
}

/// Where the parser is inside a block.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Ours,
    Base,
    Theirs,
}

/// Read `bytes` as a conflicted file whose markers are `marker_size`
/// characters long.
pub fn parse(bytes: &[u8], marker_size: usize) -> Result<ConflictFile, ParseError> {
    if bytes.contains(&0) {
        return Err(ParseError::Binary);
    }
    let mut segments = Vec::new();
    let mut text = Vec::new();
    let mut open: Option<(Side, Block)> = None;
    for (index, line) in bytes.split_inclusive(|&b| b == b'\n').enumerate() {
        let number = index + 1;
        let misplaced = |marker: u8| ParseError::Misplaced {
            line: number,
            marker: char::from(marker),
        };
        let found = marker(line, marker_size);
        let Some((side, block)) = &mut open else {
            match found {
                Some(Marker { kind: b'<', label }) => {
                    if !text.is_empty() {
                        segments.push(Segment::Text(std::mem::take(&mut text)));
                    }
                    let block = Block {
                        ours: Vec::new(),
                        base: None,
                        theirs: Vec::new(),
                        ours_label: sanitize(label),
                        theirs_label: String::new(),
                        raw: line.to_vec(),
                    };
                    open = Some((Side::Ours, block));
                }
                Some(Marker { kind, .. }) => return Err(misplaced(kind)),
                None => text.extend_from_slice(line),
            }
            continue;
        };
        block.raw.extend_from_slice(line);
        match (found, *side) {
            (None, Side::Ours) => block.ours.extend_from_slice(line),
            (None, Side::Base) => block.base.get_or_insert_default().extend_from_slice(line),
            (None, Side::Theirs) => block.theirs.extend_from_slice(line),
            (Some(Marker { kind: b'|', .. }), Side::Ours) => {
                block.base = Some(Vec::new());
                *side = Side::Base;
            }
            (Some(Marker { kind: b'=', .. }), Side::Ours | Side::Base) => *side = Side::Theirs,
            (Some(Marker { kind: b'=', .. }), Side::Theirs) => {
                return Err(ParseError::TwoSeparators { line: number });
            }
            (Some(Marker { kind: b'>', label }), Side::Theirs) => {
                block.theirs_label = sanitize(label);
                if let Some((_, block)) = open.take() {
                    segments.push(Segment::Block(block));
                }
            }
            (Some(Marker { kind, .. }), _) => return Err(misplaced(kind)),
        }
    }
    if open.is_some() {
        return Err(ParseError::Unterminated);
    }
    if !text.is_empty() {
        segments.push(Segment::Text(text));
    }
    if !segments.iter().any(|s| matches!(s, Segment::Block(_))) {
        return Err(ParseError::NoConflicts);
    }
    Ok(ConflictFile { segments })
}

impl ConflictFile {
    #[cfg(test)]
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    pub fn blocks(&self) -> impl Iterator<Item = &Block> {
        self.segments.iter().filter_map(|s| match s {
            Segment::Block(block) => Some(block),
            Segment::Text(_) => None,
        })
    }

    pub fn block_count(&self) -> usize {
        self.blocks().count()
    }

    /// The file with block `i` as `choices[i]` says: its side or sides
    /// when picked, its exact markers when not. Text is kept verbatim.
    pub fn render(&self, choices: &[Option<Choice>]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut index = 0;
        for segment in &self.segments {
            match segment {
                Segment::Text(text) => out.extend_from_slice(text),
                Segment::Block(block) => {
                    match choices.get(index).copied().flatten() {
                        Some(Choice::Ours) => out.extend_from_slice(&block.ours),
                        Some(Choice::Theirs) => out.extend_from_slice(&block.theirs),
                        Some(Choice::Both) => {
                            out.extend_from_slice(&block.ours);
                            out.extend_from_slice(&block.theirs);
                        }
                        None => out.extend_from_slice(&block.raw),
                    }
                    index += 1;
                }
            }
        }
        out
    }

    /// The rows the conflict view draws, given the picks so far: text
    /// lines; an unpicked block as its markers and sides; a picked one as
    /// a `✓` row and the lines kept.
    pub fn rows(&self, choices: &[Option<Choice>]) -> Vec<Row<'_>> {
        let mut rows = Vec::new();
        let mut index = 0;
        for segment in &self.segments {
            match segment {
                Segment::Text(text) => lines(&mut rows, text, RowKind::Text, None),
                Segment::Block(block) => {
                    let at = Some(index);
                    let marker = |kind| Row {
                        kind,
                        block: at,
                        line: &[],
                    };
                    match choices.get(index).copied().flatten() {
                        Some(choice) => {
                            rows.push(marker(RowKind::Picked(choice)));
                            if choice != Choice::Theirs {
                                lines(&mut rows, &block.ours, RowKind::Ours, at);
                            }
                            if choice != Choice::Ours {
                                lines(&mut rows, &block.theirs, RowKind::Theirs, at);
                            }
                        }
                        None => {
                            rows.push(marker(RowKind::OursMarker));
                            lines(&mut rows, &block.ours, RowKind::Ours, at);
                            if let Some(base) = &block.base {
                                rows.push(marker(RowKind::BaseMarker));
                                lines(&mut rows, base, RowKind::Base, at);
                            }
                            rows.push(marker(RowKind::Separator));
                            lines(&mut rows, &block.theirs, RowKind::Theirs, at);
                            rows.push(marker(RowKind::TheirsMarker));
                        }
                    }
                    index += 1;
                }
            }
        }
        rows
    }
}

/// Push one row per line of `bytes`.
fn lines<'a>(rows: &mut Vec<Row<'a>>, bytes: &'a [u8], kind: RowKind, block: Option<usize>) {
    for line in bytes.split_inclusive(|&b| b == b'\n') {
        rows.push(Row {
            kind,
            block,
            line: content(line),
        });
    }
}

/// What a row of the conflict view shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Text,
    /// `<<<<<<<`: the block's first row while unpicked.
    OursMarker,
    Ours,
    /// `|||||||`, before the base in diff3 and zdiff3.
    BaseMarker,
    Base,
    /// `=======`.
    Separator,
    Theirs,
    /// `>>>>>>>`.
    TheirsMarker,
    /// `✓ <choice>`: the block's first row once picked.
    Picked(Choice),
}

impl RowKind {
    /// The first row of a block, which the cursor highlights.
    pub fn is_header(self) -> bool {
        matches!(self, RowKind::OursMarker | RowKind::Picked(_))
    }
}

/// One row: its kind, the block it belongs to, and the file's bytes for
/// a line row, without the line ending. Marker rows have no bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row<'a> {
    pub kind: RowKind,
    pub block: Option<usize>,
    pub line: &'a [u8],
}

/// A loaded conflicted file: the bytes on disk, which a write must still
/// find there, and what they parse to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    pub on_disk: Vec<u8>,
    pub file: ConflictFile,
}

/// A pick being written: the block, and its choice before, to put back
/// if the write fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingPick {
    pub block: usize,
    pub previous: Option<Choice>,
}

/// The conflict view, in the Diff pane's place: one conflicted file's
/// blocks, a cursor on one, and the side picked for each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictView {
    pub path: RepoPath,
    /// The load in flight, if any; only its result is taken.
    pub seq: Option<u64>,
    pub loaded: Option<Loaded>,
    /// One per block once loaded.
    pub choices: Vec<Option<Choice>>,
    /// Index of the selected block.
    pub block: usize,
    /// Index of the top visible row.
    pub scroll: usize,
    /// Why the file cannot be resolved here.
    pub error: Option<String>,
    /// The pick whose write is in flight.
    pub pending: Option<PendingPick>,
}

impl ConflictView {
    /// A view of `path` whose load is request `seq`.
    pub fn new(path: RepoPath, seq: u64) -> Self {
        Self {
            path,
            seq: Some(seq),
            loaded: None,
            choices: Vec::new(),
            block: 0,
            scroll: 0,
            error: None,
            pending: None,
        }
    }

    /// Take the load: `bytes` read with markers of `marker_size`, or why
    /// it failed. The cursor starts on the first block.
    pub fn loaded(&mut self, result: Result<(Vec<u8>, usize), String>, rows: usize) {
        self.seq = None;
        let parsed = result.and_then(|(bytes, size)| {
            parse(&bytes, size)
                .map(|file| (bytes, file))
                .map_err(|e| e.message())
        });
        match parsed {
            Ok((on_disk, file)) => {
                self.choices = vec![None; file.block_count()];
                self.loaded = Some(Loaded { on_disk, file });
                self.error = None;
                self.block = 0;
                self.scroll = 0;
                self.scroll_to_block(rows);
            }
            Err(message) => {
                self.loaded = None;
                self.choices.clear();
                self.error = Some(message);
            }
        }
    }

    pub fn block_count(&self) -> usize {
        self.choices.len()
    }

    /// Every block has a side picked.
    pub fn resolved(&self) -> bool {
        self.loaded.is_some() && self.choices.iter().all(Option::is_some)
    }

    /// The rows the view draws now.
    pub fn rows(&self) -> Vec<Row<'_>> {
        self.loaded
            .as_ref()
            .map(|l| l.file.rows(&self.choices))
            .unwrap_or_default()
    }

    /// Move the cursor `delta` blocks, clamped, bringing it into a pane
    /// of `rows`.
    pub fn move_by(&mut self, delta: isize, rows: usize) {
        let last = self.block_count().saturating_sub(1);
        self.block = self.block.saturating_add_signed(delta).min(last);
        self.scroll_to_block(rows);
    }

    /// Move to the next unpicked block after the cursor, else the first
    /// unpicked one before it; stay when every block is picked.
    pub fn next_unpicked(&mut self, rows: usize) {
        let count = self.block_count();
        let found = (1..count)
            .map(|step| (self.block + step) % count)
            .find(|&i| self.choices[i].is_none());
        if let Some(i) = found {
            self.block = i;
        }
        self.scroll_to_block(rows);
    }

    /// Scroll the rows by a line or a page, never past the last screenful.
    pub fn scroll(&mut self, by: Scroll, rows: usize) {
        let total = self.rows().len();
        let scroll = match by {
            Scroll::LineDown => self.scroll.saturating_add(1),
            Scroll::LineUp => self.scroll.saturating_sub(1),
            Scroll::PageDown => self.scroll.saturating_add(rows),
            Scroll::PageUp => self.scroll.saturating_sub(rows),
        };
        self.scroll = scroll.min(total.saturating_sub(rows));
    }

    /// Scroll so the selected block's first row is on screen, with a
    /// little of what comes before it when it was off screen.
    pub fn scroll_to_block(&mut self, rows: usize) {
        let rows = rows.max(1);
        let header = self
            .rows()
            .iter()
            .position(|r| r.block == Some(self.block) && r.kind.is_header());
        let Some(header) = header else {
            return;
        };
        if header < self.scroll || header >= self.scroll + rows {
            self.scroll = header.saturating_sub(2.min(rows - 1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MERGE: &[u8] = b"top\n\
<<<<<<< HEAD\n\
ours 1\n\
ours 2\n\
=======\n\
theirs\n\
>>>>>>> feature\n\
bottom\n";

    fn only_block(file: &ConflictFile) -> &Block {
        let blocks: Vec<_> = file.blocks().collect();
        assert_eq!(blocks.len(), 1, "{file:?}");
        blocks[0]
    }

    #[test]
    fn the_merge_style_gives_both_sides_and_their_labels() {
        let file = parse(MERGE, 7).unwrap();
        let block = only_block(&file);
        assert_eq!(block.ours, b"ours 1\nours 2\n");
        assert_eq!(block.base, None);
        assert_eq!(block.theirs, b"theirs\n");
        assert_eq!(block.ours_label, "HEAD");
        assert_eq!(block.theirs_label, "feature");
        assert_eq!(
            file.segments()[0],
            Segment::Text(b"top\n".to_vec()),
            "text before the block"
        );
        assert_eq!(file.segments()[2], Segment::Text(b"bottom\n".to_vec()));
        assert_eq!(
            block.raw,
            &MERGE[b"top\n".len()..MERGE.len() - b"bottom\n".len()]
        );
    }

    #[test]
    fn the_diff3_style_gives_the_base_too() {
        // As git writes it with merge.conflictStyle=diff3.
        let bytes = b"<<<<<<< HEAD\nours\n||||||| 1a2b3c4\nbase\n=======\ntheirs\n>>>>>>> other\n";
        let file = parse(bytes, 7).unwrap();
        let block = only_block(&file);
        assert_eq!(block.ours, b"ours\n");
        assert_eq!(block.base.as_deref(), Some(&b"base\n"[..]));
        assert_eq!(block.theirs, b"theirs\n");
    }

    #[test]
    fn the_zdiff3_style_keeps_common_lines_outside_and_an_empty_base() {
        // zdiff3 (git 2.35+) moves lines both sides share out of the block,
        // which can leave the base empty.
        let bytes = b"same\n<<<<<<< HEAD\nours\n||||||| merged common ancestors\n=======\ntheirs\n>>>>>>> other\nsame\n";
        let file = parse(bytes, 7).unwrap();
        let block = only_block(&file);
        assert_eq!(block.ours, b"ours\n");
        assert_eq!(block.base.as_deref(), Some(&b""[..]));
        assert_eq!(block.theirs, b"theirs\n");
        assert_eq!(file.render(&[None]), bytes);
    }

    /// What `git merge-file` writes with `style` (`-q` for the default
    /// merge style), or `None` when this git does not know the style
    /// (zdiff3 needs 2.35).
    fn merged_by_git(style: &str) -> Option<Vec<u8>> {
        let tmp = crate::git::fake::TempDir::new("merge-file");
        let dir = tmp.path();
        for (name, text) in [
            ("ours", "a\nB1\nX\nd\n"),
            ("base", "a\nb\nc\nd\n"),
            ("theirs", "a\nB2\nX\nd\n"),
        ] {
            std::fs::write(dir.join(name), text).unwrap();
        }
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .envs(crate::git::runner::isolation_env())
            .args(["merge-file", "-p", style, "ours", "base", "theirs"])
            .output()
            .unwrap();
        // The exit code counts the conflicts; one is expected.
        (out.status.code() == Some(1)).then_some(out.stdout)
    }

    #[test]
    fn every_style_real_git_writes_parses_and_round_trips() {
        let cases = [
            ("-q", "B1\n", None, "B2\n"),
            ("--diff3", "B1\nX\n", Some("b\nc\n"), "B2\nX\n"),
            ("--zdiff3", "B1\n", Some("b\nc\n"), "B2\n"),
        ];
        for (style, ours, base, theirs) in cases {
            let Some(bytes) = merged_by_git(style) else {
                assert_eq!(style, "--zdiff3", "only zdiff3 may be missing");
                continue;
            };
            let file = parse(&bytes, 7).unwrap_or_else(|e| panic!("{style}: {e:?}"));
            let block = only_block(&file);
            assert_eq!(block.ours, ours.as_bytes(), "{style}");
            assert_eq!(block.base.as_deref(), base.map(str::as_bytes), "{style}");
            assert_eq!(block.theirs, theirs.as_bytes(), "{style}");
            assert_eq!(file.render(&[None]), bytes, "{style}");
        }
    }

    #[test]
    fn an_empty_side_is_kept_empty() {
        let bytes = b"<<<<<<< HEAD\n=======\ntheirs\n>>>>>>> other\n";
        let block = parse(bytes, 7).unwrap().blocks().next().unwrap().clone();
        assert_eq!(block.ours, b"");
        assert_eq!(block.theirs, b"theirs\n");
    }

    #[test]
    fn a_custom_marker_size_reads_its_markers_and_leaves_shorter_ones_as_content() {
        let bytes =
            b"<<<<<<<<<<<< HEAD\n<<<<<<< not a marker\n============\n=======\n>>>>>>>>>>>> other\n";
        let file = parse(bytes, 12).unwrap();
        let block = only_block(&file);
        assert_eq!(block.ours, b"<<<<<<< not a marker\n");
        assert_eq!(block.theirs, b"=======\n");
        assert_eq!(block.theirs_label, "other");
    }

    #[test]
    fn a_longer_run_than_the_marker_size_is_content() {
        let bytes =
            b"<<<<<<< HEAD\n<<<<<<<<< inner\n=========\n=======\n>>>>>>>>> inner\n>>>>>>> other\n";
        let file = parse(bytes, 7).unwrap();
        let block = only_block(&file);
        assert_eq!(block.ours, b"<<<<<<<<< inner\n=========\n");
        assert_eq!(block.theirs, b">>>>>>>>> inner\n");
    }

    #[test]
    fn a_marker_needs_a_space_before_its_label_and_none_after_equals() {
        let bytes = b"<<<<<<<x\n<<<<<<< HEAD\n======= x\n=======\n>>>>>>>\n";
        let file = parse(bytes, 7).unwrap();
        let block = only_block(&file);
        assert_eq!(file.segments()[0], Segment::Text(b"<<<<<<<x\n".to_vec()));
        assert_eq!(block.ours, b"======= x\n");
        assert_eq!(block.theirs_label, "");
    }

    #[test]
    fn crlf_and_a_missing_final_newline_round_trip_byte_for_byte() {
        let cases: [&[u8]; 3] = [
            b"a\r\n<<<<<<< HEAD\r\nours\r\n=======\r\ntheirs\r\n>>>>>>> other\r\nz\r\n",
            b"a\n<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> other",
            b"a\r\n<<<<<<< HEAD\nours\r\n=======\ntheirs\r\n>>>>>>> other\r\nno newline",
        ];
        for bytes in cases {
            let file = parse(bytes, 7).unwrap();
            assert_eq!(
                file.render(&[None]),
                bytes,
                "{:?}",
                String::from_utf8_lossy(bytes)
            );
        }
        let crlf = parse(cases[0], 7).unwrap();
        assert_eq!(only_block(&crlf).ours, b"ours\r\n");
        assert_eq!(only_block(&crlf).ours_label, "HEAD");
    }

    #[test]
    fn labels_are_made_safe_to_draw() {
        let bytes = b"<<<<<<< a\x1b[2Jb\n=======\n>>>>>>> c\n";
        let block = parse(bytes, 7).unwrap().blocks().next().unwrap().clone();
        assert_eq!(block.ours_label, "a\u{FFFD}[2Jb");
    }

    #[test]
    fn unreadable_markers_are_refused() {
        use ParseError::*;
        let cases: [(&[u8], ParseError); 11] = [
            (b"a\0b\n<<<<<<< x\n=======\n>>>>>>> y\n", Binary),
            (b"no markers\n", NoConflicts),
            (b"", NoConflicts),
            (
                b"<<<<<<< x\n<<<<<<< y\n=======\n>>>>>>> z\n",
                Misplaced {
                    line: 2,
                    marker: '<',
                },
            ),
            (
                b"a\n=======\n",
                Misplaced {
                    line: 2,
                    marker: '=',
                },
            ),
            (
                b"||||||| base\n",
                Misplaced {
                    line: 1,
                    marker: '|',
                },
            ),
            (
                b"<<<<<<< x\n=======\n||||||| base\n>>>>>>> y\n",
                Misplaced {
                    line: 3,
                    marker: '|',
                },
            ),
            (
                b"<<<<<<< x\n||||||| a\n||||||| b\n=======\n>>>>>>> y\n",
                Misplaced {
                    line: 3,
                    marker: '|',
                },
            ),
            (
                b"<<<<<<< x\nours\n>>>>>>> y\n",
                Misplaced {
                    line: 3,
                    marker: '>',
                },
            ),
            (b"<<<<<<< x\nours\n=======\ntheirs\n", Unterminated),
            (
                b"<<<<<<< x\nours\n=======\nmiddle\n=======\ntheirs\n>>>>>>> y\n",
                TwoSeparators { line: 5 },
            ),
        ];
        for (bytes, error) in cases {
            assert_eq!(
                parse(bytes, 7),
                Err(error),
                "{:?}",
                String::from_utf8_lossy(bytes)
            );
        }
        assert_eq!(
            parse(b">>>>>>> y\n", 7),
            Err(Misplaced {
                line: 1,
                marker: '>'
            })
        );
    }

    const TWO: &[u8] = b"a\n<<<<<<< HEAD\no1\n=======\nt1\n>>>>>>> x\nb\n<<<<<<< HEAD\no2\n||||||| base\nb2\n=======\nt2\n>>>>>>> x\nc\n";

    #[test]
    fn render_writes_each_pick_and_leaves_unpicked_blocks_as_they_were() {
        use Choice::*;
        let file = parse(TWO, 7).unwrap();
        assert_eq!(
            file.render(&[Some(Ours), Some(Ours)]),
            b"a\no1\nb\no2\nc\n".to_vec()
        );
        assert_eq!(
            file.render(&[Some(Theirs), Some(Theirs)]),
            b"a\nt1\nb\nt2\nc\n".to_vec()
        );
        assert_eq!(
            file.render(&[Some(Both), Some(Both)]),
            b"a\no1\nt1\nb\no2\nt2\nc\n".to_vec()
        );
        assert_eq!(
            file.render(&[None, Some(Theirs)]),
            b"a\n<<<<<<< HEAD\no1\n=======\nt1\n>>>>>>> x\nb\nt2\nc\n".to_vec()
        );
        assert_eq!(file.render(&[]), TWO, "no picks is the file as read");
    }

    #[test]
    fn rows_show_markers_until_a_block_is_picked() {
        let file = parse(TWO, 7).unwrap();
        let kinds = |choices: &[Option<Choice>]| -> Vec<(RowKind, Option<usize>)> {
            file.rows(choices)
                .iter()
                .map(|r| (r.kind, r.block))
                .collect()
        };
        use RowKind::*;
        assert_eq!(
            kinds(&[None, Some(Choice::Theirs)]),
            [
                (Text, None),
                (OursMarker, Some(0)),
                (Ours, Some(0)),
                (Separator, Some(0)),
                (Theirs, Some(0)),
                (TheirsMarker, Some(0)),
                (Text, None),
                (Picked(Choice::Theirs), Some(1)),
                (Theirs, Some(1)),
                (Text, None),
            ]
        );
        let rows = file.rows(&[None, None]);
        assert_eq!(rows[2].line, b"o1");
        assert!(rows.iter().any(|r| r.kind == BaseMarker));
    }

    fn loaded_view(rows: usize) -> ConflictView {
        let mut view = ConflictView::new("a.txt".into(), 1);
        view.loaded(Ok((TWO.to_vec(), 7)), rows);
        view
    }

    #[test]
    fn the_next_unpicked_block_wraps_around_and_stays_when_all_are_picked() {
        let mut view = loaded_view(20);
        view.next_unpicked(20);
        assert_eq!(view.block, 1);
        view.choices[1] = Some(Choice::Ours);
        view.next_unpicked(20);
        assert_eq!(view.block, 0);
        view.choices[0] = Some(Choice::Ours);
        view.next_unpicked(20);
        assert_eq!(view.block, 0);
        assert!(view.resolved());
    }

    #[test]
    fn moving_to_a_block_off_screen_scrolls_its_header_into_view() {
        let mut view = loaded_view(3);
        assert_eq!(view.scroll, 0);
        view.move_by(1, 3);
        // Block 1's header is row 7 of 15.
        assert_eq!(view.scroll, 5);
        view.move_by(-1, 3);
        assert_eq!(view.scroll, 0);
    }

    #[test]
    fn an_unparsable_load_keeps_the_reason() {
        let mut view = ConflictView::new("a.txt".into(), 1);
        view.loaded(Ok((b"plain\n".to_vec(), 7)), 10);
        assert_eq!(view.loaded, None);
        assert_eq!(view.error, Some(ParseError::NoConflicts.message()));
        assert!(!view.resolved());
        view.loaded(Err("cannot read".into()), 10);
        assert_eq!(view.error.as_deref(), Some("cannot read"));
    }
}
