//! Reading and replacing one file in the worktree by its repo path, for
//! the conflict view. Blocking; called from tasks.
//!
//! The path comes from the repository, which may be hostile, so neither
//! call follows a symlink anywhere along it or leaves the worktree. A
//! replace is atomic: a temp file beside the target is written, synced
//! and renamed over it, so a crash never leaves half a file.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::git::path::RepoPath;

/// Why a replace was refused: someone else wrote the file since it was read.
pub const CHANGED_ON_DISK: &str = "The file changed on disk; reopen it";

/// The bytes of `path` under `root`.
pub fn read(root: &Path, path: &RepoPath) -> Result<Vec<u8>, String> {
    let file = checked(root, path)?;
    fs::read(&file).map_err(|e| format!("Could not read {}: {e}", path.display()))
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
        let _ = fs::remove_file(&temp);
        return Err(fail(e));
    }
    Ok(())
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
    let shown = path.display();
    let refuse = |why: &str| format!("Kika will not touch {shown}: {why}");
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
    let meta = fs::symlink_metadata(&at).map_err(|e| refuse(&e.to_string()))?;
    if meta.file_type().is_symlink() {
        return Err(refuse("it is a symlink"));
    }
    if !meta.is_file() {
        return Err(refuse("it is not a regular file"));
    }
    let inside = match (fs::canonicalize(root), fs::canonicalize(&parent)) {
        (Ok(root), Ok(parent)) => parent.starts_with(root),
        _ => false,
    };
    if !inside {
        return Err(refuse("it is outside the repository"));
    }
    Ok(at)
}

/// The `/`-separated parts of a relative path, from its raw bytes. `None`
/// for an absolute path or one with an empty, `.` or `..` part, which a
/// path from git never has.
fn components(path: &RepoPath) -> Option<Vec<OsString>> {
    let raw = raw_bytes(path)?;
    let parts: Vec<&[u8]> = raw.split(|&b| b == b'/').collect();
    let plain = |part: &&[u8]| !part.is_empty() && *part != b"." && *part != b"..";
    if !parts.iter().all(plain) || (cfg!(windows) && raw.contains(&b'\\')) {
        return None;
    }
    Some(parts.into_iter().map(crate::git::path::os_string).collect())
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

        assert_eq!(read(root, &path), Ok(b"old\n".to_vec()));
        assert_eq!(replace(root, &path, b"old\n", b"new\r\n"), Ok(()));

        assert_eq!(fs::read(&file).unwrap(), b"new\r\n");
        let mode = fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
        assert_eq!(temps(&root.join("dir")), Vec::<String>::new());
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
            assert!(read(&root, &path).is_err(), "read {name:?}");
            let result = replace(&root, &path, b"secret\n", b"pwned\n");
            assert!(result.is_err(), "replace {name:?}");
        }
        assert_eq!(fs::read(outside.join("x")).unwrap(), b"secret\n");
        assert_eq!(fs::read(tmp.path().join("x")).unwrap(), b"secret\n");
        assert_eq!(temps(&root), Vec::<String>::new());
        assert_eq!(temps(&outside), Vec::<String>::new());
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
