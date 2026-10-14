//! The recent-repositories list on disk: one absolute path per line,
//! newest first. It is machine-written state, not user config, so it lives
//! in the platform's state directory, apart from any config file.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// How many repositories the list keeps.
pub const MAX_RECENT: usize = 10;

/// Where per-user state goes, which differs by platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Platform {
    Mac,
    Unix,
    Windows,
}

impl Platform {
    const CURRENT: Platform = if cfg!(target_os = "macos") {
        Platform::Mac
    } else if cfg!(windows) {
        Platform::Windows
    } else {
        Platform::Unix
    };
}

/// The list's file for this platform, read from the environment `env`
/// gives, so tests need not touch the real one. `None` when nothing
/// usable is set.
pub fn default_path(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    path_on(Platform::CURRENT, env)
}

fn path_on(platform: Platform, env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    // An empty or relative value counts as unset, as the XDG spec says.
    let dir = |key: &str| {
        env(key)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    let base = match (dir("XDG_STATE_HOME"), platform) {
        (Some(state), _) => state,
        (None, Platform::Mac) => dir("HOME")?.join("Library").join("Application Support"),
        (None, Platform::Unix) => dir("HOME")?.join(".local").join("state"),
        (None, Platform::Windows) => dir("LOCALAPPDATA")?,
    };
    Some(base.join("kika").join("recent-repos"))
}

/// The recorded repositories, newest first, leaving out any that are no
/// longer directories. A missing or unreadable file is an empty list.
pub fn load(path: &Path) -> Vec<PathBuf> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    bytes
        .split(|&b| b == b'\n')
        .filter_map(decode)
        .filter(|repo| repo.is_absolute() && repo.is_dir())
        .take(MAX_RECENT)
        .collect()
}

/// Put `repo` first in the list at `path`, without duplicates and with at
/// most [`MAX_RECENT`] entries, and return the new list. The file is
/// replaced in one rename, so a crash never leaves half a list. A path
/// that cannot be written as one line is not recorded. The temp file is
/// new and owner-only, so a planted entry is never followed and others
/// cannot read the list.
pub fn record(path: &Path, repo: &Path) -> io::Result<Vec<PathBuf>> {
    let old = load(path);
    let Some(line) = encode(repo) else {
        return Ok(old);
    };
    let mut list = vec![repo.to_path_buf()];
    list.extend(old.into_iter().filter(|p| p != repo));
    list.truncate(MAX_RECENT);

    let mut contents = line;
    for other in &list[1..] {
        // Read back from this file, so it encodes.
        contents.extend(encode(other).unwrap_or_default());
    }
    if let Some(parent) = path.parent() {
        let mut dirs = std::fs::DirBuilder::new();
        dirs.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            dirs.mode(0o700);
        }
        dirs.create(parent)?;
    }
    let (temp, mut file) = create_temp(path)?;
    let written = file
        .write_all(&contents)
        .and_then(|()| std::fs::rename(&temp, path));
    if written.is_err() {
        // Only ever a name this call created.
        let _ = std::fs::remove_file(&temp);
    }
    written.map(|()| list)
}

/// How many temp names [`create_temp`] tries before giving up.
const TEMP_ATTEMPTS: u32 = 16;

/// A new temp file beside `path`, created exclusively so an entry already
/// at a name (a planted symlink, another process's file) is skipped, never
/// followed or overwritten.
fn create_temp(path: &Path) -> io::Result<(PathBuf, File)> {
    let file_name = path.file_name().unwrap_or_default();
    let pid = std::process::id();
    let mut last = io::Error::from(io::ErrorKind::AlreadyExists);
    for n in 0..TEMP_ATTEMPTS {
        let mut name = file_name.to_os_string();
        if n == 0 {
            name.push(format!(".{pid}.tmp"));
        } else {
            name.push(format!(".{pid}.{n}.tmp"));
        }
        let temp = path.with_file_name(name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&temp) {
            Ok(file) => return Ok((temp, file)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => last = err,
            Err(err) => return Err(err),
        }
    }
    Err(last)
}

/// One line of the file for `repo`, newline included. Raw bytes on Unix;
/// UTF-8 elsewhere. `None` for a path a line cannot hold.
fn encode(repo: &Path) -> Option<Vec<u8>> {
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt;
        repo.as_os_str().as_bytes().to_vec()
    };
    #[cfg(not(unix))]
    let bytes = repo.to_str()?.as_bytes().to_vec();
    if bytes.is_empty() || bytes.contains(&b'\n') {
        return None;
    }
    let mut line = bytes;
    line.push(b'\n');
    Some(line)
}

fn decode(line: &[u8]) -> Option<PathBuf> {
    if line.is_empty() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(line)))
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(line).ok().map(PathBuf::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kika_git::fake::TempDir;

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |key| {
            vars.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[cfg(unix)]
    #[test]
    fn xdg_state_home_wins_everywhere() {
        let vars = [
            ("XDG_STATE_HOME", "/s"),
            ("HOME", "/h"),
            ("LOCALAPPDATA", "/l"),
        ];
        for platform in [Platform::Mac, Platform::Unix, Platform::Windows] {
            assert_eq!(
                path_on(platform, env(&vars)),
                Some(PathBuf::from("/s/kika/recent-repos")),
                "{platform:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn without_xdg_each_platform_uses_its_own_state_dir() {
        let vars = [("HOME", "/h"), ("LOCALAPPDATA", "/l")];
        assert_eq!(
            path_on(Platform::Mac, env(&vars)),
            Some(PathBuf::from(
                "/h/Library/Application Support/kika/recent-repos"
            ))
        );
        assert_eq!(
            path_on(Platform::Unix, env(&vars)),
            Some(PathBuf::from("/h/.local/state/kika/recent-repos"))
        );
        assert_eq!(
            path_on(Platform::Windows, env(&vars)),
            Some(PathBuf::from("/l/kika/recent-repos"))
        );
    }

    #[test]
    fn nothing_usable_set_is_no_path() {
        for platform in [Platform::Mac, Platform::Unix, Platform::Windows] {
            assert_eq!(path_on(platform, env(&[])), None);
            // Empty and relative values count as unset.
            let vars = [
                ("XDG_STATE_HOME", ""),
                ("HOME", "rel"),
                ("LOCALAPPDATA", ""),
            ];
            assert_eq!(path_on(platform, env(&vars)), None, "{platform:?}");
        }
    }

    /// `n` real directories to record.
    fn repos(tmp: &TempDir, n: usize) -> Vec<PathBuf> {
        (0..n)
            .map(|i| {
                let dir = tmp.path().join(format!("repo{i}"));
                std::fs::create_dir(&dir).unwrap();
                dir
            })
            .collect()
    }

    #[test]
    fn record_puts_the_newest_first_without_duplicates() {
        let tmp = TempDir::new("recent");
        let file = tmp.path().join("state").join("kika").join("recent-repos");
        let r = repos(&tmp, 3);

        assert_eq!(record(&file, &r[0]).unwrap(), [r[0].clone()]);
        record(&file, &r[1]).unwrap();
        record(&file, &r[2]).unwrap();
        let list = record(&file, &r[0]).unwrap();

        assert_eq!(list, [r[0].clone(), r[2].clone(), r[1].clone()]);
        assert_eq!(load(&file), list);
        // Only the list is left behind, no temp file.
        let names: Vec<_> = std::fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["recent-repos"]);
    }

    #[test]
    fn record_keeps_at_most_ten() {
        let tmp = TempDir::new("recent");
        let file = tmp.path().join("recent-repos");
        let r = repos(&tmp, MAX_RECENT + 2);
        for repo in &r {
            record(&file, repo).unwrap();
        }
        let list = load(&file);
        assert_eq!(list.len(), MAX_RECENT);
        assert_eq!(list[0], r[MAX_RECENT + 1]);
        assert_eq!(list[MAX_RECENT - 1], r[2]);
    }

    #[test]
    fn load_drops_entries_that_are_no_longer_directories() {
        let tmp = TempDir::new("recent");
        let file = tmp.path().join("recent-repos");
        let r = repos(&tmp, 2);
        record(&file, &r[0]).unwrap();
        record(&file, &r[1]).unwrap();
        std::fs::remove_dir(&r[0]).unwrap();
        assert_eq!(load(&file), [r[1].clone()]);
        assert_eq!(load(&tmp.path().join("missing")), Vec::<PathBuf>::new());
    }

    #[test]
    #[cfg_attr(windows, ignore = "Windows forbids a newline in file names")]
    fn a_path_with_a_newline_is_never_recorded() {
        let tmp = TempDir::new("recent");
        let file = tmp.path().join("recent-repos");
        let r = repos(&tmp, 1);
        record(&file, &r[0]).unwrap();
        let odd = tmp.path().join("a\nb");
        std::fs::create_dir(&odd).unwrap();
        assert_eq!(record(&file, &odd).unwrap(), [r[0].clone()]);
        assert_eq!(load(&file), [r[0].clone()]);
    }

    /// The name `record` tries first for its temp file.
    fn first_temp_name(file: &Path) -> PathBuf {
        let mut name = file.file_name().unwrap().to_os_string();
        name.push(format!(".{}.tmp", std::process::id()));
        file.with_file_name(name)
    }

    #[cfg(unix)]
    #[test]
    fn a_planted_symlink_at_the_temp_name_is_never_followed() {
        let tmp = TempDir::new("recent");
        let file = tmp.path().join("state").join("recent-repos");
        std::fs::create_dir(file.parent().unwrap()).unwrap();
        let victim = tmp.path().join("victim");
        std::fs::write(&victim, b"precious").unwrap();
        let planted = first_temp_name(&file);
        std::os::unix::fs::symlink(&victim, &planted).unwrap();
        let r = repos(&tmp, 1);

        record(&file, &r[0]).unwrap();

        assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
        assert!(planted.symlink_metadata().unwrap().is_symlink());
        assert_eq!(load(&file), [r[0].clone()]);
    }

    #[test]
    fn a_file_already_at_the_temp_name_is_left_alone() {
        let tmp = TempDir::new("recent");
        let file = tmp.path().join("recent-repos");
        let squatter = first_temp_name(&file);
        std::fs::write(&squatter, b"not ours").unwrap();
        let r = repos(&tmp, 1);

        record(&file, &r[0]).unwrap();

        assert_eq!(std::fs::read(&squatter).unwrap(), b"not ours");
        assert_eq!(load(&file), [r[0].clone()]);
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    #[test]
    fn a_fresh_state_dir_and_list_are_private_to_the_user() {
        let tmp = TempDir::new("recent");
        let file = tmp.path().join("state").join("kika").join("recent-repos");
        let r = repos(&tmp, 1);

        record(&file, &r[0]).unwrap();

        assert_eq!(mode(file.parent().unwrap()), 0o700);
        assert_eq!(mode(&file), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn replacing_a_world_readable_list_makes_it_private() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new("recent");
        let file = tmp.path().join("recent-repos");
        std::fs::write(&file, b"").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        let r = repos(&tmp, 1);

        record(&file, &r[0]).unwrap();

        assert_eq!(mode(&file), 0o600);
        assert_eq!(load(&file), [r[0].clone()]);
    }

    #[cfg(unix)]
    #[test]
    fn a_line_keeps_the_raw_bytes_of_a_path() {
        use std::os::unix::ffi::OsStrExt;

        let odd = PathBuf::from(std::ffi::OsStr::from_bytes(b"/r/caf\xe9"));
        let line = encode(&odd).unwrap();
        assert_eq!(line, b"/r/caf\xe9\n");
        assert_eq!(decode(&line[..line.len() - 1]), Some(odd));
    }

    /// Through the file too, where the file system takes such a name.
    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_utf8_round_trips() {
        use std::os::unix::ffi::OsStrExt;

        let tmp = TempDir::new("recent");
        let file = tmp.path().join("recent-repos");
        let odd = tmp.path().join(std::ffi::OsStr::from_bytes(b"caf\xe9"));
        if std::fs::create_dir(&odd).is_err() {
            // Some file systems refuse names that are not UTF-8.
            return;
        }
        record(&file, &odd).unwrap();
        assert_eq!(load(&file), [odd]);
    }
}
