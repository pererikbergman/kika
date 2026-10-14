//! A pending review: the line comments a reviewer has written on one pull
//! request and not yet submitted, kept on disk so they outlive Kika. It is
//! the user's own work, so it lives in their state directory, private to
//! them, and fails closed: a file that is not exactly what Kika writes is
//! never read, repaired or overwritten. Local files only: nothing here
//! reaches the network.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::ForgeRepo;
use super::anchor::{GhLine, GhSide};
use super::cache::{Platform, create_temp, ensure_dir, is_link};

/// The file format; another version is refused, not guessed at.
const VERSION: u32 = 1;
/// Far more than 500 comments of a usual length take.
const MAX_FILE: u64 = 1024 * 1024;
/// The most comments one pending review holds.
pub const MAX_COMMENTS: usize = 500;
/// The longest comment body, in bytes.
pub const MAX_BODY: usize = 64 * 1024;

/// Whose pending review a file holds: the repository, its clone and the
/// pull request. Stored in the file and compared on read, so comments
/// written for one pull request are never shown on another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingKey {
    pub host: String,
    pub owner: String,
    pub repo: String,
    pub number: u64,
    /// The clone's canonical top level, as for the badge cache.
    pub top: String,
}

impl PendingKey {
    /// `None` for a top level that is not UTF-8: nothing is kept for it.
    pub fn new(repo: &ForgeRepo, top: &Path, number: u64) -> Option<Self> {
        Some(Self {
            host: repo.host.to_ascii_lowercase(),
            owner: repo.owner.clone(),
            repo: repo.name.clone(),
            number,
            top: top.to_str()?.to_string(),
        })
    }

    /// The file's name: the FNV-1a 64 hash of the key, in hex.
    pub fn file_name(&self) -> String {
        let number = self.number.to_string();
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for part in [&self.host, &self.owner, &self.repo, &number, &self.top] {
            // A separator no part holds, so parts cannot shift into each other.
            for &byte in part.as_bytes().iter().chain(&[0u8]) {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0100_0000_01b3);
            }
        }
        format!("{hash:016x}.json")
    }
}

/// One comment waiting in the pending review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingComment {
    /// Unique within its review.
    pub id: u64,
    /// The file's name in the pull request, as GitHub names it.
    pub path: String,
    /// The first line of a range; `None` for a single line.
    pub start: Option<GhLine>,
    pub end: GhLine,
    /// Markdown, with no control character but `\n` and `\t`.
    pub body: String,
    /// The pull request head it was written against, as hex: the lines
    /// are that head's.
    pub head: String,
}

/// The comments not yet submitted, in the order they were written.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PendingReview {
    pub comments: Vec<PendingComment>,
}

/// Why a stored pending review was not read. The file is left as it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingError {
    Unreadable(&'static str),
}

impl std::fmt::Display for PendingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PendingError::Unreadable(why) => f.write_str(why),
        }
    }
}

/// `text` as it may be stored: CRLF as LF, and no control character but
/// `\n` and `\t`.
pub fn clean_body(text: &str) -> String {
    text.replace("\r\n", "\n")
        .chars()
        .filter(|&c| c == '\n' || c == '\t' || !c.is_control())
        .collect()
}

/// The state directory for this platform, from the environment `env`
/// gives. `None` when nothing usable is set.
pub fn default_dir(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    dir_on(Platform::CURRENT, env)
}

fn dir_on(platform: Platform, env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    // An empty or relative value counts as unset, as the XDG spec says.
    let dir = |key: &str| env(key).map(PathBuf::from).filter(|p| p.is_absolute());
    let base = match (dir("XDG_STATE_HOME"), platform) {
        (Some(state), _) => state.join("kika"),
        (None, Platform::Mac) => dir("HOME")?
            .join("Library")
            .join("Application Support")
            .join("kika"),
        (None, Platform::Unix) => dir("HOME")?.join(".local").join("state").join("kika"),
        (None, Platform::Windows) => dir("LOCALAPPDATA")?.join("kika"),
    };
    Some(base.join("reviews"))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewFile {
    version: u32,
    key: PendingKey,
    comments: Vec<StoredComment>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredComment {
    id: u64,
    path: String,
    start: Option<StoredLine>,
    end: StoredLine,
    body: String,
    head: String,
}

/// [`GhLine`] as stored, so the anchor types stay free of serde.
#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(deny_unknown_fields)]
struct StoredLine {
    side: StoredSide,
    line: u32,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum StoredSide {
    Left,
    Right,
}

impl From<GhLine> for StoredLine {
    fn from(line: GhLine) -> Self {
        Self {
            side: match line.side {
                GhSide::Left => StoredSide::Left,
                GhSide::Right => StoredSide::Right,
            },
            line: line.line,
        }
    }
}

impl From<StoredLine> for GhLine {
    fn from(line: StoredLine) -> Self {
        Self {
            side: match line.side {
                StoredSide::Left => GhSide::Left,
                StoredSide::Right => GhSide::Right,
            },
            line: line.line,
        }
    }
}

impl From<&PendingComment> for StoredComment {
    fn from(c: &PendingComment) -> Self {
        Self {
            id: c.id,
            path: c.path.clone(),
            start: c.start.map(StoredLine::from),
            end: c.end.into(),
            body: c.body.clone(),
            head: c.head.clone(),
        }
    }
}

impl From<StoredComment> for PendingComment {
    fn from(c: StoredComment) -> Self {
        Self {
            id: c.id,
            path: c.path,
            start: c.start.map(GhLine::from),
            end: c.end.into(),
            body: c.body,
            head: c.head,
        }
    }
}

const BAD: PendingError = PendingError::Unreadable("it is not a pending review Kika wrote");

/// Why `review` is not one Kika may store or read, if it is not.
fn check(review: &PendingReview) -> Result<(), PendingError> {
    if review.comments.len() > MAX_COMMENTS {
        return Err(PendingError::Unreadable("it holds too many comments"));
    }
    let mut ids: Vec<u64> = review.comments.iter().map(|c| c.id).collect();
    ids.sort_unstable();
    if ids.windows(2).any(|w| w[0] == w[1]) {
        return Err(PendingError::Unreadable("two comments share an id"));
    }
    for c in &review.comments {
        if c.path.is_empty() || c.path.contains('\0') || c.path.starts_with('/') {
            return Err(PendingError::Unreadable(
                "a comment's file name is not valid",
            ));
        }
        let lines = c.start.iter().chain([&c.end]);
        if lines.clone().any(|l| l.line == 0) {
            return Err(PendingError::Unreadable("a comment is on line 0"));
        }
        if let Some(start) = c.start
            && start.side == c.end.side
            && start.line >= c.end.line
        {
            return Err(PendingError::Unreadable("a comment's range runs backwards"));
        }
        let control = c
            .body
            .chars()
            .any(|ch| ch.is_control() && ch != '\n' && ch != '\t');
        if c.body.len() > MAX_BODY || control {
            return Err(PendingError::Unreadable("a comment's text is not valid"));
        }
        let hex = c.head.bytes().all(|b| b.is_ascii_hexdigit());
        if !hex || !matches!(c.head.len(), 40 | 64) {
            return Err(PendingError::Unreadable(
                "a comment's head is not a commit id",
            ));
        }
    }
    Ok(())
}

/// The pending review stored for `key` in `dir`: `Ok(None)` when there is
/// no file, and `Unreadable` for anything but a file Kika wrote for this
/// key: a link, a file too large or malformed, of another version, for
/// another pull request, or holding a comment Kika would not write.
pub fn read(dir: &Path, key: &PendingKey) -> Result<Option<PendingReview>, PendingError> {
    if is_link(dir) {
        return Err(PendingError::Unreadable("its directory is a symbolic link"));
    }
    let path = dir.join(key.file_name());
    let meta = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(PendingError::Unreadable("it cannot be opened")),
    };
    if meta.file_type().is_symlink() {
        return Err(PendingError::Unreadable("it is a symbolic link"));
    }
    if !meta.is_file() {
        return Err(PendingError::Unreadable("it is not a plain file"));
    }
    let file = File::open(&path).map_err(|_| PendingError::Unreadable("it cannot be opened"))?;
    // It may have been swapped since the check; ask the open handle.
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return Err(PendingError::Unreadable("it is not a plain file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| PendingError::Unreadable("it cannot be read"))?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(PendingError::Unreadable("it is too large"));
    }
    let stored: ReviewFile = serde_json::from_slice(&bytes).map_err(|_| BAD)?;
    if stored.version != VERSION {
        return Err(PendingError::Unreadable("it is of another version"));
    }
    if stored.key != *key {
        return Err(PendingError::Unreadable(
            "it was written for another pull request",
        ));
    }
    let review = PendingReview {
        comments: stored
            .comments
            .into_iter()
            .map(PendingComment::from)
            .collect(),
    };
    check(&review)?;
    Ok(Some(review))
}

/// Store `review` for `key` in `dir`, replacing the file in one rename; an
/// empty review removes the file. Refused, leaving the file untouched,
/// when the file there is not one [`read`] accepts, or `review` is not
/// one it would.
pub fn write(dir: &Path, key: &PendingKey, review: &PendingReview) -> io::Result<()> {
    let refused = |e: PendingError| io::Error::other(format!("the pending review {e}"));
    check(review).map_err(refused)?;
    ensure_dir(dir)?;
    // Someone else's file, or one Kika cannot vouch for, is left alone.
    read(dir, key).map_err(|e| {
        io::Error::other(format!(
            "the saved pending review could not be read ({e}); it was left as it is"
        ))
    })?;
    let path = dir.join(key.file_name());
    if review.comments.is_empty() {
        return match fs::remove_file(&path) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        };
    }
    let file = ReviewFile {
        version: VERSION,
        key: key.clone(),
        comments: review.comments.iter().map(StoredComment::from).collect(),
    };
    let bytes = serde_json::to_vec(&file).map_err(io::Error::other)?;
    let (temp, mut out) = create_temp(&path)?;
    let written = out
        .write_all(&bytes)
        .and_then(|()| out.sync_all())
        .and_then(|()| fs::rename(&temp, &path));
    if written.is_err() {
        // Only ever a name this call created.
        let _ = fs::remove_file(&temp);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;
    use kika_git::fake::TempDir;

    fn key(number: u64, top: &str) -> PendingKey {
        PendingKey {
            host: "github.com".into(),
            owner: "acme".into(),
            repo: "widgets".into(),
            number,
            top: top.into(),
        }
    }

    const HEAD: &str = "1111111111111111111111111111111111111111";

    fn comment(id: u64, start: Option<GhLine>, end: GhLine, body: &str) -> PendingComment {
        PendingComment {
            id,
            path: "src/lib.rs".into(),
            start,
            end,
            body: body.into(),
            head: HEAD.into(),
        }
    }

    fn right(line: u32) -> GhLine {
        GhLine {
            side: GhSide::Right,
            line,
        }
    }

    fn left(line: u32) -> GhLine {
        GhLine {
            side: GhSide::Left,
            line,
        }
    }

    fn review() -> PendingReview {
        PendingReview {
            comments: vec![
                comment(1, None, right(3), "hi"),
                comment(2, Some(left(4)), right(9), "a range\n\twith a tab"),
            ],
        }
    }

    fn file_of(dir: &Path) -> PathBuf {
        dir.join(key(12, "/r").file_name())
    }

    #[test]
    fn a_written_review_reads_back_and_an_empty_one_removes_the_file() {
        let tmp = TempDir::new("pending");
        let dir = tmp.path().join("kika").join("reviews");
        assert_eq!(read(&dir, &key(12, "/r")), Ok(None));
        write(&dir, &key(12, "/r"), &review()).unwrap();
        assert_eq!(read(&dir, &key(12, "/r")), Ok(Some(review())));
        // Only the file is left, no temp.
        let names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, [OsString::from(key(12, "/r").file_name())]);
        let text = fs::read_to_string(file_of(&dir)).unwrap();
        assert!(
            text.contains(r#""end":{"side":"right","line":3}"#),
            "{text}"
        );

        write(&dir, &key(12, "/r"), &PendingReview::default()).unwrap();
        assert!(!file_of(&dir).exists());
        assert_eq!(read(&dir, &key(12, "/r")), Ok(None));
        // Removing what is not there is no error.
        write(&dir, &key(12, "/r"), &PendingReview::default()).unwrap();
    }

    #[test]
    fn each_pull_request_and_clone_has_its_own_file() {
        let names = [
            key(12, "/r").file_name(),
            key(13, "/r").file_name(),
            key(12, "/s").file_name(),
        ];
        assert_ne!(names[0], names[1]);
        assert_ne!(names[0], names[2]);
        assert_eq!(names[0].len(), 16 + ".json".len());
        let repo = ForgeRepo {
            host: "GitHub.com".into(),
            owner: "acme".into(),
            name: "widgets".into(),
        };
        assert_eq!(
            PendingKey::new(&repo, Path::new("/r"), 12),
            Some(key(12, "/r"))
        );
    }

    /// A file with `text` where `key(12, "/r")` keeps it.
    fn planted(text: &str) -> TempDir {
        let tmp = TempDir::new("pending");
        fs::write(file_of(tmp.path()), text).unwrap();
        tmp
    }

    fn valid(comment: &str) -> String {
        format!(
            r#"{{"version":1,"key":{{"host":"github.com","owner":"acme","repo":"widgets","number":12,"top":"/r"}},"comments":[{comment}]}}"#
        )
    }

    const GOOD: &str = r#"{"id":1,"path":"a.rs","start":null,"end":{"side":"right","line":3},"body":"hi","head":"1111111111111111111111111111111111111111"}"#;

    /// `read` refuses the file, and `write` leaves it exactly as it was.
    fn refused(tmp: &TempDir, what: &str) {
        let before = fs::read(file_of(tmp.path())).ok();
        assert!(
            matches!(
                read(tmp.path(), &key(12, "/r")),
                Err(PendingError::Unreadable(_))
            ),
            "{what}"
        );
        assert!(
            write(tmp.path(), &key(12, "/r"), &review()).is_err(),
            "{what}"
        );
        assert!(
            write(tmp.path(), &key(12, "/r"), &PendingReview::default()).is_err(),
            "{what}"
        );
        assert_eq!(fs::read(file_of(tmp.path())).ok(), before, "{what}");
    }

    #[test]
    fn a_file_kika_did_not_write_is_refused_and_never_overwritten() {
        let tmp = planted(&valid(GOOD));
        assert!(matches!(read(tmp.path(), &key(12, "/r")), Ok(Some(_))));
        let good = |from: &str, to: &str| valid(&GOOD.replace(from, to));
        let two = format!("{GOOD},{GOOD}");
        let cases = [
            ("invalid JSON", "{".to_string()),
            (
                "unknown field",
                valid(GOOD).replace("\"comments\"", "\"x\":1,\"comments\""),
            ),
            (
                "unknown comment field",
                good("\"id\":1", "\"id\":1,\"x\":1"),
            ),
            (
                "wrong version",
                valid(GOOD).replace("\"version\":1", "\"version\":2"),
            ),
            (
                "another number",
                valid(GOOD).replace("\"number\":12", "\"number\":13"),
            ),
            ("another top", valid(GOOD).replace("\"/r\"", "\"/s\"")),
            ("ESC in a body", good("\"hi\"", "\"a\\u001b[31mb\"")),
            ("CR in a body", good("\"hi\"", "\"a\\rb\"")),
            ("line 0", good("\"line\":3", "\"line\":0")),
            ("duplicate ids", valid(&two)),
            ("empty path", good("\"a.rs\"", "\"\"")),
            ("absolute path", good("\"a.rs\"", "\"/etc/x\"")),
            ("NUL in a path", good("\"a.rs\"", "\"a\\u0000b\"")),
            ("short head", good(HEAD, "1111")),
            ("non-hex head", good(HEAD, &"g".repeat(40))),
            (
                "backwards range",
                good("\"start\":null", r#""start":{"side":"right","line":5}"#),
            ),
            (
                "one-line range",
                good("\"start\":null", r#""start":{"side":"right","line":3}"#),
            ),
            ("unknown side", good("\"right\"", "\"middle\"")),
        ];
        for (what, text) in cases {
            fs::write(file_of(tmp.path()), &text).unwrap();
            refused(&tmp, what);
        }
        let long = "a".repeat(MAX_BODY + 1);
        fs::write(file_of(tmp.path()), good("\"hi\"", &format!("\"{long}\""))).unwrap();
        refused(&tmp, "long body");
        let many: Vec<String> = (1..=MAX_COMMENTS as u64 + 1)
            .map(|id| GOOD.replace("\"id\":1", &format!("\"id\":{id}")))
            .collect();
        fs::write(file_of(tmp.path()), valid(&many.join(","))).unwrap();
        refused(&tmp, "too many comments");
        let mut big = valid(GOOD).into_bytes();
        big.extend(std::iter::repeat_n(b' ', 1024 * 1024));
        fs::write(file_of(tmp.path()), big).unwrap();
        refused(&tmp, "oversized");
        // A 64-hex head and a range across sides are fine.
        let sha256 = "a".repeat(64);
        let across = good("\"start\":null", r#""start":{"side":"left","line":9}"#);
        for text in [good(HEAD, &sha256), across] {
            fs::write(file_of(tmp.path()), text).unwrap();
            assert!(matches!(read(tmp.path(), &key(12, "/r")), Ok(Some(_))));
        }
    }

    #[test]
    fn a_review_kika_would_not_read_is_not_written() {
        let tmp = TempDir::new("pending");
        let mut bad = review();
        bad.comments[0].body = "a\u{1b}b".into();
        assert!(write(tmp.path(), &key(12, "/r"), &bad).is_err());
        let mut bad = review();
        bad.comments[1].id = 1;
        assert!(write(tmp.path(), &key(12, "/r"), &bad).is_err());
        assert!(!file_of(tmp.path()).exists());
    }

    #[test]
    fn a_directory_in_the_files_place_is_refused() {
        let tmp = TempDir::new("pending");
        fs::create_dir(file_of(tmp.path())).unwrap();
        assert!(matches!(
            read(tmp.path(), &key(12, "/r")),
            Err(PendingError::Unreadable(_))
        ));
        assert!(write(tmp.path(), &key(12, "/r"), &review()).is_err());
        assert!(file_of(tmp.path()).is_dir());
    }

    #[test]
    fn stored_text_keeps_newlines_and_tabs_only() {
        assert_eq!(clean_body("a\r\nb\rc\td\u{1b}[31me\u{7f}"), "a\nbc\td[31me");
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    #[test]
    fn the_dir_and_file_are_private() {
        let tmp = TempDir::new("pending");
        let dir = tmp.path().join("reviews");
        write(&dir, &key(12, "/r"), &review()).unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&file_of(&dir)), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_files_and_dirs_are_refused_and_never_written_through() {
        let tmp = TempDir::new("pending");
        let real = tmp.path().join("real");
        write(&real, &key(12, "/r"), &review()).unwrap();
        let original = fs::read(file_of(&real)).unwrap();

        // A linked directory is neither read nor written.
        let linked = tmp.path().join("linked");
        std::os::unix::fs::symlink(&real, &linked).unwrap();
        assert!(matches!(
            read(&linked, &key(12, "/r")),
            Err(PendingError::Unreadable(_))
        ));
        assert!(write(&linked, &key(12, "/r"), &PendingReview::default()).is_err());

        // A linked file is refused, and never written through.
        let dir = tmp.path().join("dir");
        ensure_dir(&dir).unwrap();
        std::os::unix::fs::symlink(file_of(&real), file_of(&dir)).unwrap();
        assert!(matches!(
            read(&dir, &key(12, "/r")),
            Err(PendingError::Unreadable(_))
        ));
        let one = PendingReview {
            comments: vec![comment(7, None, right(1), "other")],
        };
        assert!(write(&dir, &key(12, "/r"), &one).is_err());
        assert!(write(&dir, &key(12, "/r"), &PendingReview::default()).is_err());
        assert_eq!(fs::read(file_of(&real)).unwrap(), original);
        assert!(
            fs::symlink_metadata(file_of(&dir))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |key| {
            vars.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[cfg(unix)]
    #[test]
    fn each_platform_has_its_state_dir_and_xdg_wins() {
        let vars = [("HOME", "/h"), ("LOCALAPPDATA", "/l")];
        assert_eq!(
            dir_on(Platform::Mac, env(&vars)),
            Some(PathBuf::from("/h/Library/Application Support/kika/reviews"))
        );
        assert_eq!(
            dir_on(Platform::Unix, env(&vars)),
            Some(PathBuf::from("/h/.local/state/kika/reviews"))
        );
        assert_eq!(
            dir_on(Platform::Windows, env(&vars)),
            Some(PathBuf::from("/l/kika/reviews"))
        );
        let xdg = [("XDG_STATE_HOME", "/x"), ("HOME", "/h")];
        for platform in [Platform::Mac, Platform::Unix, Platform::Windows] {
            assert_eq!(
                dir_on(platform, env(&xdg)),
                Some(PathBuf::from("/x/kika/reviews")),
                "{platform:?}"
            );
        }
        let relative = [
            ("XDG_STATE_HOME", "rel"),
            ("HOME", ""),
            ("LOCALAPPDATA", "x"),
        ];
        for platform in [Platform::Mac, Platform::Unix, Platform::Windows] {
            assert_eq!(dir_on(platform, env(&relative)), None, "{platform:?}");
        }
    }
}
