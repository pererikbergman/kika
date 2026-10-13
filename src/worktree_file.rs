//! Reading, replacing and creating one file in the worktree by its repo
//! path, for the conflict view and `.gitignore`, or one file by its full
//! path, for git's own files. Blocking; called from tasks.
//!
//! The path comes from the repository, which may be hostile, so no call
//! follows a symlink anywhere along it or leaves the worktree. Nor does
//! a repo path with a `.git` part, or a spelling some filesystem reads as
//! `.git` (a dotless `ı`, an invisible code point, on Windows a short name
//! such as `GIT~2`), or on Windows a device name or a part ending in a dot
//! or space, reach the filesystem: those would land in git's own directory
//! or a device instead of a file. Errors show paths terminal-safe. A replace
//! is atomic: a temp file beside the target is written, synced and renamed
//! over it, so a crash never leaves half a file. A create links its temp
//! file into place, which fails on anything already there.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::git::path::RepoPath;

/// Why a replace was refused: someone else wrote the file since it was read.
pub const CHANGED_ON_DISK: &str = "The file changed on disk; reopen it";

/// The bytes of `path` under `root`, refused when there are more than
/// `max`: they are read into memory and drawn row by row.
pub fn read(root: &Path, path: &RepoPath, max: u64) -> Result<Vec<u8>, String> {
    let file = checked(root, path)?;
    match read_capped(&file, max) {
        Ok(Some(bytes)) => Ok(bytes),
        Ok(None) => Err(format!(
            "The file is larger than {} MB, too large to resolve here.",
            max / (1024 * 1024)
        )),
        Err(e) => Err(format!("Could not read {}: {e}", path.display())),
    }
}

/// Write `content` over `path` under `root`, only if it still holds
/// `expected`. The file keeps its permissions, exec bit included.
pub fn replace(
    root: &Path,
    path: &RepoPath,
    expected: &[u8],
    content: &[u8],
) -> Result<(), String> {
    replace_file(&checked(root, path)?, expected, content)
}

/// The bytes of `file`, a regular file in a real directory, refused when
/// there are more than `max`. For a file outside the worktree, such as
/// git's own todo list, which no repo path reaches.
pub fn read_file(file: &Path, max: u64) -> Result<Vec<u8>, String> {
    plain_file(file)?;
    match read_capped(file, max) {
        Ok(Some(bytes)) => Ok(bytes),
        Ok(None) => Err(format!("{} is larger than {} KB", shown(file), max / 1024)),
        Err(e) => Err(format!("Could not read {}: {e}", shown(file))),
    }
}

/// Write `content` over `file`, a regular file in a real directory, only
/// if it still holds `expected`. Atomic: a temp file beside it is
/// written, synced and renamed over it, keeping its permissions.
pub fn replace_file(file: &Path, expected: &[u8], content: &[u8]) -> Result<(), String> {
    plain_file(file)?;
    let fail = |e: std::io::Error| format!("Could not write {}: {e}", shown(file));
    // One byte past `expected` tells a longer file apart, so a file that
    // grew huge, or never ends, is not read into memory.
    match read_capped(file, expected.len() as u64).map_err(fail)? {
        Some(held) if held == expected => {}
        _ => return Err(CHANGED_ON_DISK.to_string()),
    }
    let permissions = fs::symlink_metadata(file).map_err(fail)?.permissions();
    let (temp, mut handle) = create_temp(file).map_err(fail)?;
    let written = (|| {
        handle.set_permissions(permissions)?;
        handle.write_all(content)?;
        handle.sync_all()?;
        drop(handle);
        fs::rename(&temp, file)
    })();
    if let Err(e) = written {
        remove_temp(&temp);
        return Err(fail(e));
    }
    Ok(())
}

/// Refuse `file` unless it is a regular file, not a symlink, in a
/// directory that is not a symlink either, so no write lands elsewhere.
fn plain_file(file: &Path) -> Result<(), String> {
    let refuse = |why: &str| format!("Kika will not touch {}: {why}", shown(file));
    let meta = fs::symlink_metadata(file).map_err(|e| refuse(&e.to_string()))?;
    if meta.file_type().is_symlink() {
        return Err(refuse("it is a symlink"));
    }
    if !meta.is_file() {
        return Err(refuse("it is not a regular file"));
    }
    let parent = match file.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let dir = fs::symlink_metadata(parent).map_err(|e| refuse(&e.to_string()))?;
    if dir.file_type().is_symlink() || !dir.is_dir() {
        return Err(refuse("its directory is a symlink"));
    }
    Ok(())
}

/// `file` as text safe to draw: a hostile name could carry ESC or a
/// newline, which `Path::display` would pass to the terminal.
fn shown(file: &Path) -> String {
    crate::git::safe_text::sanitize(file.as_os_str().as_encoded_bytes())
}

/// The bytes of `file`, or `None` when there are more than `max`.
fn read_capped(file: &Path, max: u64) -> std::io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    open_plain(file)?
        .take(max.saturating_add(1))
        .read_to_end(&mut bytes)?;
    Ok((bytes.len() as u64 <= max).then_some(bytes))
}

/// Open `file` for reading only if it is a regular file, checked on the
/// open handle. The earlier path checks give the friendly messages, but a
/// local process could swap in a symlink or a FIFO after them; this open
/// neither follows the one nor waits on the other.
fn open_plain(file: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // O_NONBLOCK keeps a FIFO from stalling the open; it changes
        // nothing for reads of a regular file.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_OPEN_REPARSE_POINT: open a symlink itself, not its target.
        options.custom_flags(0x0020_0000);
    }
    let handle = options.open(file).map_err(open_error)?;
    let meta = handle.metadata()?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    Ok(handle)
}

/// O_NOFOLLOW refuses a symlink with ELOOP; say what the type check in
/// [`open_plain`] would, not "Too many levels of symbolic links".
#[cfg(unix)]
fn open_error(e: std::io::Error) -> std::io::Error {
    if e.raw_os_error() == Some(libc::ELOOP) {
        std::io::Error::other("not a regular file")
    } else {
        e
    }
}

/// Off unix a symlink opens as itself and the type check refuses it.
#[cfg(not(unix))]
fn open_error(e: std::io::Error) -> std::io::Error {
    e
}

/// Write `content` to `path` under `root`, which must not exist yet;
/// its directories must. The temp file is hard-linked into place, which
/// fails on anything there, a symlink included, so a file that appeared
/// meanwhile is left alone and reported as [`CHANGED_ON_DISK`]. Where hard
/// links are unsupported, the target is opened with `create_new` instead.
pub fn create(root: &Path, path: &RepoPath, content: &[u8]) -> Result<(), String> {
    let (file, exists) = located(root, path)?;
    if exists {
        return Err(CHANGED_ON_DISK.to_string());
    }
    let fail = |e: std::io::Error| format!("Could not write {}: {e}", path.display());
    let (temp, mut handle) = create_temp(&file).map_err(fail)?;
    let written = (|| {
        handle.write_all(content)?;
        handle.sync_all()
    })();
    drop(handle);
    if let Err(e) = written {
        remove_temp(&temp);
        return Err(fail(e));
    }
    let linked = fs::hard_link(&temp, &file);
    remove_temp(&temp);
    match linked {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Err(CHANGED_ON_DISK.to_string()),
        Err(_) => create_in_place(&file, content).map_err(|e| match e.kind() {
            ErrorKind::AlreadyExists => CHANGED_ON_DISK.to_string(),
            _ => fail(e),
        }),
    }
}

/// `create` without a hard link: `create_new` still refuses anything
/// already there, but a crash can leave the file short.
fn create_in_place(file: &Path, content: &[u8]) -> std::io::Result<()> {
    let mut handle = OpenOptions::new().write(true).create_new(true).open(file)?;
    handle.write_all(content)?;
    handle.sync_all()
}

/// Change `path` under `root` with `change`, which gets the current
/// bytes (`None` when there is no file) and returns the new ones, or
/// `None` to leave it. An existing file is replaced only while it still
/// holds what was read, and a missing one is created; when someone else
/// writes in between, it is read and changed once more, and a second race
/// is [`CHANGED_ON_DISK`]. Returns whether anything was written.
pub fn update(
    root: &Path,
    path: &RepoPath,
    max: u64,
    mut change: impl FnMut(Option<&[u8]>) -> Option<Vec<u8>>,
) -> Result<bool, String> {
    for _ in 0..2 {
        let old = match located(root, path)? {
            (_, true) => Some(read(root, path, max)?),
            (_, false) => None,
        };
        let Some(new) = change(old.as_deref()) else {
            return Ok(false);
        };
        let written = match &old {
            Some(old) => replace(root, path, old, &new),
            None => create(root, path, &new),
        };
        match written {
            Err(e) if e == CHANGED_ON_DISK => continue,
            written => return written.map(|()| true),
        }
    }
    Err(CHANGED_ON_DISK.to_string())
}

/// Remove a temp file left by a failed replace. It may carry a read-only
/// target's permissions, which on Windows keep it from being removed.
fn remove_temp(temp: &Path) {
    if fs::remove_file(temp).is_err() && clear_read_only(temp) {
        let _ = fs::remove_file(temp);
    }
}

/// Clear Windows' read-only attribute, so the file can be removed.
#[cfg(windows)]
fn clear_read_only(file: &Path) -> bool {
    let Ok(meta) = fs::metadata(file) else {
        return false;
    };
    let mut permissions = meta.permissions();
    // On Windows this clears one attribute; it grants nobody anything.
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    fs::set_permissions(file, permissions).is_ok()
}

/// Elsewhere the directory's permissions decide removal, not the file's.
#[cfg(not(windows))]
fn clear_read_only(_: &Path) -> bool {
    false
}

/// A new file beside `file`, named `.<name>.kika-<pid>-<n>`. `create_new`
/// fails on anything already there, a symlink included, so it never
/// writes through one.
fn create_temp(file: &Path) -> std::io::Result<(PathBuf, File)> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = file.parent().unwrap_or(Path::new("."));
    let name = file.file_name().unwrap_or_default();
    loop {
        let mut temp = OsString::from(".");
        temp.push(name);
        temp.push(format!(
            ".kika-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let temp = dir.join(temp);
        match OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(handle) => return Ok((temp, handle)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

/// The file `path` names under `root`, once it is sure to be a regular
/// file reached through real directories inside `root`.
fn checked(root: &Path, path: &RepoPath) -> Result<PathBuf, String> {
    match located(root, path)? {
        (at, true) => Ok(at),
        (_, false) => Err(refusal(path, "it does not exist")),
    }
}

/// Why `path` is not touched.
fn refusal(path: &RepoPath, why: &str) -> String {
    format!("Kika will not touch {}: {why}", path.display())
}

/// The file `path` names under `root`, reached through real directories
/// inside `root`, and whether it exists. Anything there but a regular
/// file, a symlink included, is an error.
fn located(root: &Path, path: &RepoPath) -> Result<(PathBuf, bool), String> {
    let refuse = |why: &str| refusal(path, why);
    let parts = components(path).map_err(refuse)?;
    let Some((name, dirs)) = parts.split_last() else {
        return Err(refuse("the path is empty"));
    };
    let mut at = root.to_path_buf();
    for dir in dirs {
        at.push(dir);
        let meta = fs::symlink_metadata(&at).map_err(|e| refuse(&e.to_string()))?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(refuse(
                "a directory on its path is a symlink or not a directory",
            ));
        }
    }
    let parent = at.clone();
    at.push(name);
    // Pushing a part with a prefix would replace the path, not extend it;
    // `components` refuses those, and this makes sure of it.
    if at.parent() != Some(parent.as_path()) || !at.starts_with(root) {
        return Err(refuse("it is outside the repository"));
    }
    let meta = match fs::symlink_metadata(&at) {
        Ok(meta) => Some(meta),
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) => return Err(refuse(&e.to_string())),
    };
    if let Some(meta) = &meta {
        if meta.file_type().is_symlink() {
            return Err(refuse("it is a symlink"));
        }
        if !meta.is_file() {
            return Err(refuse("it is not a regular file"));
        }
    }
    let inside = match (fs::canonicalize(root), fs::canonicalize(&parent)) {
        (Ok(root), Ok(parent)) => parent.starts_with(root),
        _ => false,
    };
    if !inside {
        return Err(refuse("it is outside the repository"));
    }
    let exists = meta.is_some();
    Ok((at, exists))
}

/// The `/`-separated parts of a relative path, from its raw bytes, or why
/// it is refused. An absolute path or one with an empty, `.` or `..`
/// part, which a path from git never has, is refused. Each part must be
/// one plain name to the platform too: on Windows a `\` separates and a
/// `:` makes a drive prefix (`D:x`) or a stream name, either of which
/// would leave the worktree. A `.git` part would reach git's own
/// directory, not a worktree file; see [`refused_name`] for the rest.
fn components(path: &RepoPath) -> Result<Vec<OsString>, &'static str> {
    const NOT_PLAIN: &str = "the path is not a plain repo path";
    let raw = raw_bytes(path).ok_or(NOT_PLAIN)?;
    if cfg!(windows) && raw.iter().any(|&b| b == b'\\' || b == b':') {
        return Err(NOT_PLAIN);
    }
    let plain = |part: &OsString| {
        let mut inside = Path::new(part).components();
        matches!(
            (inside.next(), inside.next()),
            (Some(std::path::Component::Normal(name)), None) if name == part.as_os_str()
        )
    };
    raw.split(|&b| b == b'/')
        .map(|bytes| {
            let part = crate::git::path::os_string(bytes);
            if !plain(&part) {
                return Err(NOT_PLAIN);
            }
            refused_name(bytes).map_or(Ok(part), Err)
        })
        .collect()
}

/// Why the plain name `part` is refused, if it is. `.git` in any case,
/// or a spelling of it with `ı`, `İ` or an ignorable code point, would
/// reach git's own directory. On Windows, which drops a trailing dot or
/// space and opens a device for a reserved name in any folder, these
/// would reach another file or a device, and `GIT~N` or `GI250A~N` can
/// be the short name of `.git`.
fn refused_name(part: &[u8]) -> Option<&'static str> {
    const DOT_GIT: &str = "a part is named .git";
    if dot_git_alias(part) {
        return Some(DOT_GIT);
    }
    if !cfg!(windows) {
        return None;
    }
    if matches!(part.last(), Some(b'.' | b' ')) {
        Some("a part ends in a dot or space")
    } else if dot_git_short_name(part) {
        Some(DOT_GIT)
    } else if device_name(part) {
        Some("a part is a Windows device name")
    } else {
        None
    }
}

/// Whether some filesystem may open git's own directory for `part`: it
/// is `.git`, ignoring ASCII case, once the code points HFS+ ignores are
/// dropped and `ı` and `İ` read as `i`, as case-folding tables may. A part
/// that is not UTF-8 is no alias. Compiled everywhere so it is tested
/// everywhere.
fn dot_git_alias(part: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(part) else {
        return false;
    };
    let folded: String = text
        .chars()
        .filter(|&c| !hfs_ignored(c))
        .map(|c| match c {
            '\u{130}' | '\u{131}' => 'i',
            c => c,
        })
        .collect();
    folded.eq_ignore_ascii_case(".git")
}

/// The joiners, direction marks and byte-order mark HFS+ leaves out of
/// a name when it compares two.
fn hfs_ignored(c: char) -> bool {
    matches!(
        c,
        '\u{200C}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{206A}'..='\u{206F}' | '\u{FEFF}'
    )
}

/// Whether `part` can be the 8.3 short name NTFS gives `.git`: `GIT~N`,
/// or the hashed `GI250A~N` it falls back to after many clashes, in any
/// case, with `N` a number not starting with `0`. Compiled everywhere so
/// it is tested everywhere.
fn dot_git_short_name(part: &[u8]) -> bool {
    let part = part.to_ascii_lowercase();
    let number = part
        .strip_prefix(b"git~")
        .or_else(|| part.strip_prefix(b"gi250a~"));
    matches!(number, Some([b'1'..=b'9', rest @ ..]) if rest.iter().all(u8::is_ascii_digit))
}

/// Whether Windows opens a device, not a file, for `part`: its base
/// before the first `.`, less trailing spaces, is a reserved name in any
/// case. Compiled everywhere so it is tested everywhere.
fn device_name(part: &[u8]) -> bool {
    let base = part.split(|&b| b == b'.').next().unwrap_or_default();
    let end = base.iter().rposition(|&b| b != b' ').map_or(0, |i| i + 1);
    let base = base[..end].to_ascii_uppercase();
    // `1`–`9`, or a UTF-8 superscript `¹`, `²` or `³`.
    let digit = |rest: &[u8]| matches!(rest, [b'1'..=b'9'] | [0xC2, 0xB9 | 0xB2 | 0xB3]);
    match base.as_slice() {
        b"CON" | b"PRN" | b"AUX" | b"NUL" | b"CONIN$" | b"CONOUT$" => true,
        [b'C', b'O', b'M', rest @ ..] | [b'L', b'P', b'T', rest @ ..] => digit(rest),
        _ => false,
    }
}

#[cfg(unix)]
fn raw_bytes(path: &RepoPath) -> Option<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;
    Some(path.as_os_str().as_bytes().to_vec())
}

/// Elsewhere names are UTF-8, as `RepoPath` keeps them.
#[cfg(not(unix))]
fn raw_bytes(path: &RepoPath) -> Option<Vec<u8>> {
    path.as_os_str().to_str().map(|s| s.as_bytes().to_vec())
}

/// Name rules that need no filesystem, run on every platform.
#[cfg(test)]
mod name_tests {
    use super::*;

    #[test]
    fn a_part_named_dot_git_is_refused_in_any_case() {
        for name in [".git", ".GIT", ".Git/config", "sub/.git/hooks/x", "a/.git"] {
            assert_eq!(
                components(&RepoPath::from(name)),
                Err("a part is named .git"),
                "{name:?}"
            );
        }
        for name in [".gitignore", ".git2", "x.git", "git", "dir/.gitattributes"] {
            assert!(components(&RepoPath::from(name)).is_ok(), "{name:?}");
        }
    }

    #[test]
    fn unicode_spellings_of_dot_git_are_refused_on_every_platform() {
        for name in [
            ".g\u{131}t",
            ".G\u{130}T",
            ".g\u{200C}it",
            "\u{FEFF}.git",
            ".gi\u{200D}t/config",
            "dir/.g\u{206A}it",
            ".\u{202A}git\u{200F}",
        ] {
            assert_eq!(
                components(&RepoPath::from(name)),
                Err("a part is named .git"),
                "{name:?}"
            );
        }
        for name in [
            ".gitignore",
            ".g\u{131}tignore",
            "g\u{131}t",
            ".g\u{200C}itx",
            ".g\u{2010}it",
        ] {
            assert!(components(&RepoPath::from(name)).is_ok(), "{name:?}");
        }
        assert!(!dot_git_alias(b".g\xffit"));
        assert!(!dot_git_alias(b".git\xff"));
    }

    #[test]
    fn short_names_of_dot_git_match_any_number_from_one() {
        for name in ["GIT~1", "git~2", "Git~10", "GI250A~1", "gi250a~9"] {
            assert!(dot_git_short_name(name.as_bytes()), "{name:?}");
        }
        for name in [
            "git~0", "git~", "git~1a", "git~01", "gi250b~1", "xgit~1", "git~1.", "git1",
        ] {
            assert!(!dot_git_short_name(name.as_bytes()), "{name:?}");
        }
    }

    #[test]
    fn device_names_match_ignoring_case_extension_and_trailing_spaces() {
        for name in [
            "CON",
            "con",
            "Prn",
            "aux",
            "NUL",
            "nul.txt",
            "COM1",
            "com9.tar.gz",
            "LPT1",
            "lpt9",
            "COM¹",
            "LPT³",
            "CONIN$",
            "conout$",
            "AUX .c",
        ] {
            assert!(device_name(name.as_bytes()), "{name:?}");
        }
        for name in [
            "CONSOLE", "COM0", "COM10", "LPT", "xcon", "con_x", "nul-file", "a.con",
        ] {
            assert!(!device_name(name.as_bytes()), "{name:?}");
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::*;
    use crate::git::fake::TempDir;

    /// The names in `dir` that are Kika's temp files.
    fn temps(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".kika-"))
            .collect()
    }

    #[test]
    fn replace_writes_the_content_and_keeps_the_mode() {
        let tmp = TempDir::new("worktree-file");
        let root = tmp.path();
        fs::create_dir(root.join("dir")).unwrap();
        let file = root.join("dir/a.sh");
        fs::write(&file, "old\n").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        let path = RepoPath::from("dir/a.sh");

        assert_eq!(read(root, &path, 1024), Ok(b"old\n".to_vec()));
        assert_eq!(replace(root, &path, b"old\n", b"new\r\n"), Ok(()));

        assert_eq!(fs::read(&file).unwrap(), b"new\r\n");
        let mode = fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
        assert_eq!(temps(&root.join("dir")), Vec::<String>::new());
    }

    #[test]
    fn a_file_over_the_limit_is_refused() {
        let tmp = TempDir::new("worktree-file");
        let root = tmp.path();
        fs::write(root.join("big.txt"), vec![b'x'; 2 * 1024 * 1024 + 1]).unwrap();
        let path = RepoPath::from("big.txt");
        assert_eq!(
            read(root, &path, 2 * 1024 * 1024),
            Err("The file is larger than 2 MB, too large to resolve here.".to_string())
        );
        assert_eq!(
            read(root, &path, 3 * 1024 * 1024).map(|b| b.len()),
            Ok(2 * 1024 * 1024 + 1)
        );
    }

    #[test]
    fn each_part_must_be_one_plain_name() {
        // Windows' rules for dots, spaces, devices and short names stay
        // there: here these are ordinary names.
        for name in [
            "a",
            "dir/a.txt",
            "a.",
            "...",
            "a\\b",
            "c:d",
            "a ",
            "CON",
            "nul.txt",
            "GIT~1",
            "GIT~2",
            "GI250A~1",
        ] {
            assert!(components(&RepoPath::from(name)).is_ok(), "{name:?}");
        }
        for name in ["", "/a", "a/", "a//b", "./a", "a/.", "../a", "a/../b"] {
            assert!(components(&RepoPath::from(name)).is_err(), "{name:?}");
        }
    }

    #[test]
    fn a_file_that_changed_since_it_was_read_is_left_untouched() {
        let tmp = TempDir::new("worktree-file");
        let root = tmp.path();
        fs::write(root.join("a.txt"), "edited elsewhere\n").unwrap();
        let path = RepoPath::from("a.txt");

        assert_eq!(
            replace(root, &path, b"as read\n", b"resolved\n"),
            Err(CHANGED_ON_DISK.to_string())
        );
        assert_eq!(fs::read(root.join("a.txt")).unwrap(), b"edited elsewhere\n");
        assert_eq!(temps(root), Vec::<String>::new());
    }

    #[test]
    fn a_file_that_grew_shrank_or_differs_is_changed_on_disk_and_left_untouched() {
        let tmp = TempDir::new("worktree-file");
        let root = tmp.path();
        let grown = [b"old\n".to_vec(), vec![b'x'; 1024 * 1024]].concat();
        for (name, held) in [
            ("grown", grown),
            ("short", b"old".to_vec()),
            ("differs", b"odd\n".to_vec()),
        ] {
            let file = root.join(name);
            fs::write(&file, &held).unwrap();
            assert_eq!(
                replace(root, &RepoPath::from(name), b"old\n", b"new\n"),
                Err(CHANGED_ON_DISK.to_string()),
                "replace {name}"
            );
            assert_eq!(
                replace_file(&file, b"old\n", b"new\n"),
                Err(CHANGED_ON_DISK.to_string()),
                "replace_file {name}"
            );
            assert!(fs::read(&file).unwrap() == held, "{name} untouched");
        }
        assert_eq!(temps(root), Vec::<String>::new());
    }

    #[test]
    fn symlinks_and_paths_leaving_the_worktree_are_refused() {
        let tmp = TempDir::new("worktree-file");
        let root = tmp.path().join("repo");
        let outside = tmp.path().join("outside");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("x"), "secret\n").unwrap();
        fs::write(tmp.path().join("x"), "secret\n").unwrap();
        symlink(outside.join("x"), root.join("link.txt")).unwrap();
        symlink(&outside, root.join("dir")).unwrap();
        fs::create_dir(root.join("sub")).unwrap();

        for name in [
            "link.txt",
            "dir/x",
            "../x",
            "/etc/hosts",
            "sub/../../x",
            "./link.txt",
            "sub//x",
            "sub",
            "",
        ] {
            let path = RepoPath::from(name);
            assert!(read(&root, &path, 1024).is_err(), "read {name:?}");
            let result = replace(&root, &path, b"secret\n", b"pwned\n");
            assert!(result.is_err(), "replace {name:?}");
        }
        assert_eq!(fs::read(outside.join("x")).unwrap(), b"secret\n");
        assert_eq!(fs::read(tmp.path().join("x")).unwrap(), b"secret\n");
        assert_eq!(temps(&root), Vec::<String>::new());
        assert_eq!(temps(&outside), Vec::<String>::new());
    }

    #[test]
    fn replace_file_writes_atomically_and_keeps_the_mode() {
        let tmp = TempDir::new("worktree-file");
        let file = tmp.path().join("todo");
        fs::write(&file, "old\n").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();

        assert_eq!(read_file(&file, 1024), Ok(b"old\n".to_vec()));
        assert_eq!(replace_file(&file, b"old\n", b"new\n"), Ok(()));

        assert_eq!(fs::read(&file).unwrap(), b"new\n");
        let mode = fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(temps(tmp.path()), Vec::<String>::new());
    }

    #[test]
    fn replace_file_refuses_a_file_that_changed_and_leaves_it() {
        let tmp = TempDir::new("worktree-file");
        let file = tmp.path().join("todo");
        fs::write(&file, "edited elsewhere\n").unwrap();
        assert_eq!(
            replace_file(&file, b"as read\n", b"new\n"),
            Err(CHANGED_ON_DISK.to_string())
        );
        assert_eq!(fs::read(&file).unwrap(), b"edited elsewhere\n");
        assert_eq!(temps(tmp.path()), Vec::<String>::new());
    }

    #[test]
    fn replace_file_and_read_file_refuse_symlinks_and_their_dirs() {
        let tmp = TempDir::new("worktree-file");
        let real = tmp.path().join("real");
        fs::create_dir(&real).unwrap();
        fs::write(real.join("todo"), "secret\n").unwrap();
        symlink(real.join("todo"), tmp.path().join("link")).unwrap();
        symlink(&real, tmp.path().join("dir")).unwrap();

        for file in [
            tmp.path().join("link"),
            tmp.path().join("dir/todo"),
            real.clone(),
        ] {
            assert!(read_file(&file, 1024).is_err(), "{}", file.display());
            let result = replace_file(&file, b"secret\n", b"pwned\n");
            assert!(result.is_err(), "{}", file.display());
        }
        assert_eq!(fs::read(real.join("todo")).unwrap(), b"secret\n");
        assert_eq!(temps(&real), Vec::<String>::new());
        assert_eq!(temps(tmp.path()), Vec::<String>::new());
    }

    #[test]
    fn read_file_refuses_a_file_over_the_limit() {
        let tmp = TempDir::new("worktree-file");
        let file = tmp.path().join("big");
        fs::write(&file, vec![b'x'; 2049]).unwrap();
        assert!(read_file(&file, 2048).is_err());
        assert_eq!(read_file(&file, 4096).map(|b| b.len()), Ok(2049));
    }

    #[test]
    fn a_failed_replace_file_leaves_no_temp_file() {
        let tmp = TempDir::new("worktree-file");
        let dir = tmp.path().join("ro");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("todo"), "old\n").unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();

        let result = replace_file(&dir.join("todo"), b"old\n", b"new\n");

        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        // Root can write anywhere; the point is only that nothing is left.
        if result.is_err() {
            assert_eq!(fs::read(dir.join("todo")).unwrap(), b"old\n");
        }
        assert_eq!(temps(&dir), Vec::<String>::new());
    }

    #[test]
    fn create_writes_a_new_file_and_refuses_one_already_there() {
        let tmp = TempDir::new("worktree-file");
        let root = tmp.path();
        let path = RepoPath::from(".gitignore");

        assert_eq!(create(root, &path, b"/a\n"), Ok(()));
        assert_eq!(fs::read(root.join(".gitignore")).unwrap(), b"/a\n");
        assert_eq!(
            create(root, &path, b"/b\n"),
            Err(CHANGED_ON_DISK.to_string())
        );
        assert_eq!(fs::read(root.join(".gitignore")).unwrap(), b"/a\n");
        assert_eq!(temps(root), Vec::<String>::new());
    }

    #[test]
    fn update_creates_a_missing_file_and_appends_to_one_there() {
        let tmp = TempDir::new("worktree-file");
        let root = tmp.path();
        let path = RepoPath::from(".gitignore");
        let add = |old: Option<&[u8]>| {
            let mut new = old.unwrap_or_default().to_vec();
            new.extend_from_slice(b"x\n");
            Some(new)
        };

        assert_eq!(update(root, &path, 1024, add), Ok(true));
        assert_eq!(update(root, &path, 1024, add), Ok(true));
        assert_eq!(update(root, &path, 1024, |_| None), Ok(false));
        assert_eq!(fs::read(root.join(".gitignore")).unwrap(), b"x\nx\n");
        assert_eq!(temps(root), Vec::<String>::new());
    }

    #[test]
    fn update_refuses_a_symlink_or_a_symlinked_directory_and_leaves_the_target() {
        let tmp = TempDir::new("worktree-file");
        let root = tmp.path().join("repo");
        let outside = tmp.path().join("outside");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join(".gitignore"), "secret\n").unwrap();
        symlink(outside.join(".gitignore"), root.join(".gitignore")).unwrap();
        symlink(&outside, root.join("dir")).unwrap();
        symlink(outside.join("missing"), root.join("dangling")).unwrap();
        let add = |_: Option<&[u8]>| Some(b"pwned\n".to_vec());

        for name in [".gitignore", "dir/.gitignore", "dir/new", "dangling"] {
            let path = RepoPath::from(name);
            assert!(update(&root, &path, 1024, add).is_err(), "update {name:?}");
            assert!(create(&root, &path, b"pwned\n").is_err(), "create {name:?}");
        }
        assert_eq!(fs::read(outside.join(".gitignore")).unwrap(), b"secret\n");
        assert!(!outside.join("new").exists());
        assert!(!outside.join("missing").exists());
        assert_eq!(temps(&root), Vec::<String>::new());
        assert_eq!(temps(&outside), Vec::<String>::new());
    }

    #[test]
    fn update_reads_again_after_a_race_and_gives_up_after_a_second() {
        let tmp = TempDir::new("worktree-file");
        let root = tmp.path();
        let file = root.join(".gitignore");
        let path = RepoPath::from(".gitignore");

        // Someone writes once between the read and the replace: the
        // retry sees their line and keeps it.
        fs::write(&file, "a\n").unwrap();
        let mut calls = 0;
        let result = update(root, &path, 1024, |old| {
            calls += 1;
            if calls == 1 {
                fs::write(&file, "a\nb\n").unwrap();
            }
            Some([old.unwrap_or_default(), b"c\n"].concat())
        });
        assert_eq!(result, Ok(true));
        assert_eq!(calls, 2);
        assert_eq!(fs::read(&file).unwrap(), b"a\nb\nc\n");

        // Every time: the second race is reported, and their write stays.
        let mut n = 0;
        let result = update(root, &path, 1024, |old| {
            n += 1;
            fs::write(&file, format!("theirs {n}\n")).unwrap();
            Some([old.unwrap_or_default(), b"c\n"].concat())
        });
        assert_eq!(result, Err(CHANGED_ON_DISK.to_string()));
        assert_eq!(n, 2);
        assert_eq!(fs::read(&file).unwrap(), b"theirs 2\n");

        // A file that appears before a create is a race too, so the
        // retry reads it and appends to it.
        fs::remove_file(&file).unwrap();
        let mut created = false;
        let result = update(root, &path, 1024, |old| {
            if !created {
                created = true;
                fs::write(&file, "theirs\n").unwrap();
            }
            Some([old.unwrap_or_default(), b"mine\n"].concat())
        });
        assert_eq!(result, Ok(true));
        assert_eq!(fs::read(&file).unwrap(), b"theirs\nmine\n");
        assert_eq!(temps(root), Vec::<String>::new());
    }

    #[test]
    fn a_failed_write_leaves_no_temp_file() {
        let tmp = TempDir::new("worktree-file");
        let root = tmp.path();
        fs::create_dir(root.join("ro")).unwrap();
        fs::write(root.join("ro/a.txt"), "old\n").unwrap();
        fs::set_permissions(root.join("ro"), fs::Permissions::from_mode(0o555)).unwrap();
        let path = RepoPath::from("ro/a.txt");

        let result = replace(root, &path, b"old\n", b"new\n");

        fs::set_permissions(root.join("ro"), fs::Permissions::from_mode(0o755)).unwrap();
        // Root can write anywhere; the point is only that nothing is left.
        if result.is_err() {
            assert_eq!(fs::read(root.join("ro/a.txt")).unwrap(), b"old\n");
        }
        assert_eq!(temps(&root.join("ro")), Vec::<String>::new());
    }

    #[test]
    fn errors_for_a_name_with_control_characters_draw_none_of_them() {
        let tmp = TempDir::new("worktree-file");
        let dir = tmp.path();
        let name = "a\x1b[31m\nb";
        let file = dir.join(name);
        fs::write(&file, vec![b'x'; 2049]).unwrap();
        let link = dir.join(format!("{name}-link"));
        symlink(&file, &link).unwrap();
        let missing = dir.join(format!("{name}-missing"));
        let clean = |err: &str| !err.bytes().any(|b| b < 0x20 || b == 0x7F);

        let touch = "Kika will not touch".to_string();
        let mut errors = vec![
            (touch.clone(), replace_file(&link, b"", b"x").unwrap_err()),
            (touch, read_file(&missing, 1024).unwrap_err()),
            (
                format!("{} is larger than 1 KB", shown(&file)),
                read_file(&file, 1024).unwrap_err(),
            ),
        ];
        // A directory nobody may write to and a file nobody may read;
        // root ignores both, so those errors only come up for anyone else.
        let locked = dir.join(format!("{name}-locked"));
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("f"), "old\n").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
        if let Err(e) = replace_file(&locked.join("f"), b"old\n", b"new\n") {
            errors.push(("Could not write".to_string(), e));
        }
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        let unreadable = dir.join(format!("{name}-unreadable"));
        fs::write(&unreadable, "x").unwrap();
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();
        if let Err(e) = read_file(&unreadable, 1024) {
            errors.push(("Could not read".to_string(), e));
        }

        for (prefix, err) in errors {
            assert!(err.starts_with(&prefix), "{err:?} starts with {prefix:?}");
            assert!(clean(&err), "{err:?}");
        }
    }

    #[test]
    fn the_open_itself_refuses_a_symlink_swapped_in_after_the_check() {
        let tmp = TempDir::new("worktree-file");
        let root = tmp.path();
        fs::write(root.join("secret"), "secret\n").unwrap();
        symlink(root.join("secret"), root.join("link")).unwrap();

        // Straight to the open, as if the link appeared after `plain_file`:
        // the refusal reads like any other, not like the OS's loop error.
        let link = root.join("link");
        let opened = open_plain(&link).map(|_| ()).map_err(|e| e.to_string());
        assert_eq!(opened, Err("not a regular file".to_string()));
        let read = read_capped(&link, 1024).map_err(|e| e.to_string());
        assert_eq!(read, Err("not a regular file".to_string()));
        assert!(!read.unwrap_err().contains("symbolic links"));
        assert!(open_plain(&root.join("secret")).is_ok());
    }

    #[test]
    fn the_open_refuses_a_fifo_at_once_instead_of_blocking() {
        let tmp = TempDir::new("worktree-file");
        let fifo = tmp.path().join("fifo");
        let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: `c` is a valid NUL-terminated path for the call.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(read_capped(&fifo, 1024).map_err(|e| e.to_string()));
        });
        let result = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("opening a FIFO must not block");
        assert_eq!(result, Err("not a regular file".to_string()));
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use crate::git::fake::TempDir;

    #[test]
    fn drive_prefixes_streams_and_backslashes_are_refused() {
        for name in [
            "D:x",
            "C:",
            "dir/D:x",
            "a:b",
            "a.txt:stream",
            "a\\b",
            "..\\x",
        ] {
            assert!(components(&RepoPath::from(name)).is_err(), "{name:?}");
        }
    }

    #[test]
    fn devices_trailing_dots_and_spaces_and_the_short_dot_git_name_are_refused() {
        let refused = |name: &str| components(&RepoPath::from(name)).err();
        for name in ["CON", "dir/nul.txt", "COM1/x"] {
            assert_eq!(
                refused(name),
                Some("a part is a Windows device name"),
                "{name:?}"
            );
        }
        for name in ["a.", "a ", "dir./x", "x/b "] {
            assert_eq!(
                refused(name),
                Some("a part ends in a dot or space"),
                "{name:?}"
            );
        }
        for name in [
            "GIT~1",
            "git~1/config",
            ".git.",
            "GIT~2",
            "git~3/config",
            "GI250A~1",
        ] {
            let why = refused(name);
            assert!(
                matches!(
                    why,
                    Some("a part is named .git" | "a part ends in a dot or space")
                ),
                "{name:?}: {why:?}"
            );
        }
    }

    #[test]
    fn a_device_or_short_dot_git_path_is_never_read_or_written() {
        let tmp = TempDir::new("worktree-file");
        let root = tmp.path().join("repo");
        fs::create_dir(&root).unwrap();
        let add = |_: Option<&[u8]>| Some(b"pwned\n".to_vec());
        for name in ["NUL", "GIT~1/config", "GIT~2/config"] {
            let path = RepoPath::from(name);
            let results = [
                read(&root, &path, 1024).map(|_| ()),
                replace(&root, &path, b"", b"pwned\n"),
                create(&root, &path, b"pwned\n"),
                update(&root, &path, 1024, add).map(|_| ()),
            ];
            for result in results {
                let err = result.expect_err(name);
                assert!(err.starts_with("Kika will not touch"), "{name:?}: {err}");
            }
        }
        let left: Vec<_> = fs::read_dir(&root).unwrap().collect();
        assert_eq!(left.len(), 0);
    }

    #[test]
    fn a_drive_relative_name_never_reaches_outside_the_worktree() {
        let tmp = TempDir::new("worktree-file");
        let root = tmp.path().join("repo");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("a.txt"), "inside\n").unwrap();
        for name in ["D:x", "C:a.txt", "sub/C:a.txt"] {
            let path = RepoPath::from(name);
            assert!(read(&root, &path, 1024).is_err(), "read {name:?}");
            assert!(
                replace(&root, &path, b"inside\n", b"pwned\n").is_err(),
                "replace {name:?}"
            );
        }
        assert_eq!(fs::read(root.join("a.txt")).unwrap(), b"inside\n");
    }

    #[test]
    fn a_read_only_file_that_cannot_be_replaced_leaves_no_temp_file() {
        let tmp = TempDir::new("worktree-file");
        let root = tmp.path();
        let file = root.join("a.txt");
        fs::write(&file, "old\n").unwrap();
        let mut permissions = fs::metadata(&file).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&file, permissions).unwrap();

        let _ = replace(root, &RepoPath::from("a.txt"), b"old\n", b"new\n");

        let left: Vec<_> = fs::read_dir(root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".kika-"))
            .collect();
        assert_eq!(left, Vec::<String>::new());
        let mut permissions = fs::metadata(&file).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        fs::set_permissions(&file, permissions).unwrap();
    }

    #[test]
    fn a_plain_file_is_replaced() {
        let tmp = TempDir::new("worktree-file");
        let root = tmp.path();
        fs::write(root.join("a.txt"), "old\r\n").unwrap();
        let path = RepoPath::from("a.txt");
        assert_eq!(replace(root, &path, b"old\r\n", b"new\r\n"), Ok(()));
        assert_eq!(fs::read(root.join("a.txt")).unwrap(), b"new\r\n");
    }
}
