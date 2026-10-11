//! Reading, replacing and creating one file in the worktree by its repo
//! path, for the conflict view and `.gitignore`. Blocking; called from
//! tasks.
//!
//! The path comes from the repository, which may be hostile, so no call
//! follows a symlink anywhere along it or leaves the worktree. A replace
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
    let fail = |e: std::io::Error| format!("Could not read {}: {e}", path.display());
    let mut bytes = Vec::new();
    File::open(&file)
        .and_then(|f| f.take(max.saturating_add(1)).read_to_end(&mut bytes))
        .map_err(fail)?;
    if bytes.len() as u64 > max {
        return Err(format!(
            "The file is larger than {} MB, too large to resolve here.",
            max / (1024 * 1024)
        ));
    }
    Ok(bytes)
}

/// Write `content` over `path` under `root`, only if it still holds
/// `expected`. The file keeps its permissions, exec bit included.
pub fn replace(
    root: &Path,
    path: &RepoPath,
    expected: &[u8],
    content: &[u8],
) -> Result<(), String> {
    let file = checked(root, path)?;
    let fail = |e: std::io::Error| format!("Could not write {}: {e}", path.display());
    if fs::read(&file).map_err(fail)? != expected {
        return Err(CHANGED_ON_DISK.to_string());
    }
    let permissions = fs::symlink_metadata(&file).map_err(fail)?.permissions();
    let (temp, mut handle) = create_temp(&file).map_err(fail)?;
    let written = (|| {
        handle.set_permissions(permissions)?;
        handle.write_all(content)?;
        handle.sync_all()?;
        drop(handle);
        fs::rename(&temp, &file)
    })();
    if let Err(e) = written {
        remove_temp(&temp);
        return Err(fail(e));
    }
    Ok(())
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
    let parts = components(path).ok_or_else(|| refuse("the path is not a plain repo path"))?;
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

/// The `/`-separated parts of a relative path, from its raw bytes. `None`
/// for an absolute path or one with an empty, `.` or `..` part, which a
/// path from git never has. Each part must be one plain name to the
/// platform too: on Windows a `\` separates and a `:` makes a drive
/// prefix (`D:x`) or a stream name, either of which would leave the
/// worktree.
fn components(path: &RepoPath) -> Option<Vec<OsString>> {
    let raw = raw_bytes(path)?;
    if cfg!(windows) && raw.iter().any(|&b| b == b'\\' || b == b':') {
        return None;
    }
    let parts: Vec<OsString> = raw
        .split(|&b| b == b'/')
        .map(crate::git::path::os_string)
        .collect();
    let plain = |part: &OsString| {
        let mut inside = Path::new(part).components();
        matches!(
            (inside.next(), inside.next()),
            (Some(std::path::Component::Normal(name)), None) if name == part.as_os_str()
        )
    };
    parts.iter().all(plain).then_some(parts)
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
        for name in ["a", "dir/a.txt", "a.", "...", "a\\b", "c:d"] {
            assert!(components(&RepoPath::from(name)).is_some(), "{name:?}");
        }
        for name in ["", "/a", "a/", "a//b", "./a", "a/.", "../a", "a/../b"] {
            assert_eq!(components(&RepoPath::from(name)), None, "{name:?}");
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
            assert_eq!(components(&RepoPath::from(name)), None, "{name:?}");
        }
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
