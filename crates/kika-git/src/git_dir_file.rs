//! Opening a file git keeps in the git dir. A repo the user opens can hold
//! anything at those paths, so only a regular file is opened, never
//! through a symlink: a FIFO would block the read and a link would read
//! another file.

use std::fs::File;
use std::path::Path;

/// Why a git-dir file was not opened.
#[derive(Debug)]
pub(crate) enum OpenError {
    /// Missing, or the check or the open failed.
    Unreadable(std::io::Error),
    /// The path is there but is not a regular file.
    NotRegular,
}

/// Open `path` for reading only when it is a regular file. On unix the
/// open itself refuses a link and does not wait on a FIFO swapped in
/// after the check, and the open handle is checked again.
pub(crate) fn open_regular(path: &Path) -> Result<File, OpenError> {
    let meta = std::fs::symlink_metadata(path).map_err(OpenError::Unreadable)?;
    if !meta.is_file() {
        return Err(OpenError::NotRegular);
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(OpenError::Unreadable)?;
    // It may have been swapped since the check; ask the open handle.
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return Err(OpenError::NotRegular);
    }
    Ok(file)
}

/// Make a FIFO at `path`, for tests that put one where a file should be.
#[cfg(all(test, unix))]
pub(crate) fn mkfifo(path: &Path) {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: `c` is a valid NUL-terminated path for the call.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;
    use crate::fake::TempDir;

    #[test]
    fn a_regular_file_opens() {
        let tmp = TempDir::new("git-dir-file");
        let file = tmp.path().join("f");
        std::fs::write(&file, "text").unwrap();
        let mut text = String::new();
        open_regular(&file)
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "text");
    }

    #[test]
    fn a_missing_file_is_unreadable_and_a_directory_is_not_regular() {
        let tmp = TempDir::new("git-dir-file-dir");
        let missing = open_regular(&tmp.path().join("missing"));
        assert!(
            matches!(&missing, Err(OpenError::Unreadable(e)) if e.kind() == std::io::ErrorKind::NotFound),
            "{missing:?}"
        );
        assert!(matches!(
            open_regular(tmp.path()),
            Err(OpenError::NotRegular)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_or_a_fifo_is_not_regular_and_never_blocks() {
        let tmp = TempDir::new("git-dir-file-special");
        let dir = tmp.path();
        std::fs::write(dir.join("target"), "secret").unwrap();
        std::os::unix::fs::symlink(dir.join("target"), dir.join("link")).unwrap();
        mkfifo(&dir.join("fifo"));
        for name in ["link", "fifo"] {
            assert!(
                matches!(open_regular(&dir.join(name)), Err(OpenError::NotRegular)),
                "{name}"
            );
        }
    }
}
