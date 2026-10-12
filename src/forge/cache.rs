//! The last pull requests seen per repository, on disk, so the badges show
//! at once on startup. Machine-written state in the user's cache
//! directory, private to them. It holds badges, the heads asked about
//! (each branch with the owner and ref name read from local git config),
//! whether the repository is a fork, the probe's validated ETag and the
//! key they belong to: no token, title, URL userinfo or anything else `gh`
//! printed.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::github::{MAX_BRANCHES, valid_etag};
use super::head::Head;
use super::{CiState, ForgeRepo, PrBadge, PrState};
use crate::git::branches::BranchName;

/// The file format; another version is a miss.
const VERSION: u32 = 3;
/// Far more than 500 badges take; anything larger is not ours.
const MAX_FILE: u64 = 1024 * 1024;

/// Whose badges a file holds. Stored in the file and compared on read, so
/// a file written for one repository is never shown for another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheKey {
    pub host: String,
    pub owner: String,
    pub repo: String,
    /// The repository's canonical top level: two clones of one remote
    /// keep apart.
    pub top: String,
}

impl CacheKey {
    /// `None` for a top level that is not UTF-8, which is then not cached.
    pub fn new(repo: &ForgeRepo, top: &Path) -> Option<Self> {
        Some(Self {
            host: repo.host.to_ascii_lowercase(),
            owner: repo.owner.clone(),
            repo: repo.name.clone(),
            top: top.to_str()?.to_string(),
        })
    }

    /// The file's name: the FNV-1a 64 hash of the key, in hex.
    pub fn file_name(&self) -> String {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for part in [&self.host, &self.owner, &self.repo, &self.top] {
            // A separator no part holds, so parts cannot shift into each other.
            for &byte in part.as_bytes().iter().chain(&[0u8]) {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0100_0000_01b3);
            }
        }
        format!("{hash:016x}.json")
    }
}

/// Where per-user caches go, which differs by platform.
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

/// The cache directory for this platform, from the environment `env`
/// gives. `None` when nothing usable is set.
pub fn default_dir(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    dir_on(Platform::CURRENT, env)
}

fn dir_on(platform: Platform, env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    // An empty or relative value counts as unset, as the XDG spec says.
    let dir = |key: &str| env(key).map(PathBuf::from).filter(|p| p.is_absolute());
    let base = match (dir("XDG_CACHE_HOME"), platform) {
        (Some(cache), _) => cache.join("kika"),
        (None, Platform::Mac) => dir("HOME")?.join("Library").join("Caches").join("kika"),
        (None, Platform::Unix) => dir("HOME")?.join(".cache").join("kika"),
        (None, Platform::Windows) => dir("LOCALAPPDATA")?.join("kika").join("cache"),
    };
    Some(base.join("forge"))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheFile {
    version: u32,
    key: CacheKey,
    fetched_at: i64,
    /// The pull-request list's ETag, for the next conditional probe.
    etag: Option<String>,
    /// The heads the badges were asked for, as (branch, owner, ref name);
    /// another set needs a new ask.
    queried: Vec<(String, String, String)>,
    /// GitHub named a parent, so the probe, which only sees this
    /// repository's own pull requests, is skipped.
    forked: bool,
    prs: HashMap<String, StoredBadge>,
}

/// What one cache file holds for a repository.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cached {
    pub badges: HashMap<BranchName, PrBadge>,
    /// Validated on read and write; an invalid one is dropped.
    pub etag: Option<String>,
    pub queried: Vec<Head>,
    /// The repository is a fork: its own pull-request list says nothing
    /// about the parent's, so no probe can vouch for the badges.
    pub forked: bool,
    /// When GitHub last answered with these badges, in seconds since the
    /// epoch. A 304 does not move it, so its age bounds how long a
    /// probe alone may vouch for them.
    pub fetched_at: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredBadge {
    number: u64,
    state: StoredState,
    ci: Option<StoredCi>,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum StoredState {
    Open,
    Draft,
    Merged,
    Closed,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum StoredCi {
    Success,
    Failure,
    Pending,
}

impl From<PrBadge> for StoredBadge {
    fn from(badge: PrBadge) -> Self {
        Self {
            number: badge.number,
            state: match badge.state {
                PrState::Open => StoredState::Open,
                PrState::Draft => StoredState::Draft,
                PrState::Merged => StoredState::Merged,
                PrState::Closed => StoredState::Closed,
            },
            ci: badge.ci.map(|ci| match ci {
                CiState::Success => StoredCi::Success,
                CiState::Failure => StoredCi::Failure,
                CiState::Pending => StoredCi::Pending,
            }),
        }
    }
}

impl From<&StoredBadge> for PrBadge {
    fn from(stored: &StoredBadge) -> Self {
        Self {
            number: stored.number,
            state: match stored.state {
                StoredState::Open => PrState::Open,
                StoredState::Draft => PrState::Draft,
                StoredState::Merged => PrState::Merged,
                StoredState::Closed => PrState::Closed,
            },
            ci: stored.ci.map(|ci| match ci {
                StoredCi::Success => CiState::Success,
                StoredCi::Failure => CiState::Failure,
                StoredCi::Pending => CiState::Pending,
            }),
        }
    }
}

/// What is stored for `key` in `dir`, or `None` for a miss: no file, a
/// link, a file too large or malformed, of another version, or one
/// written for another key.
pub fn read(dir: &Path, key: &CacheKey) -> Option<Cached> {
    if is_link(dir) {
        return None;
    }
    let path = dir.join(key.file_name());
    let meta = fs::symlink_metadata(&path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let file = File::open(&path).ok()?;
    // It may have been swapped since the check; ask the open handle.
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_FILE {
        return None;
    }
    let stored: CacheFile = serde_json::from_slice(&bytes).ok()?;
    if stored.version != VERSION
        || stored.key != *key
        || stored.prs.len() > MAX_BRANCHES
        || stored.queried.len() > MAX_BRANCHES
    {
        return None;
    }
    Some(Cached {
        badges: stored
            .prs
            .iter()
            .map(|(name, badge)| (BranchName::from(name.as_str()), PrBadge::from(badge)))
            .collect(),
        etag: stored.etag.as_deref().and_then(valid_etag),
        queried: stored
            .queried
            .into_iter()
            .map(|(branch, owner, name)| Head {
                branch: BranchName::from(branch.as_str()),
                owner,
                name,
            })
            .collect(),
        forked: stored.forked,
        fetched_at: stored.fetched_at,
    })
}

/// Store `cached` for `key` in `dir`, replacing the file in one rename.
/// Names that are not UTF-8 are left out; they are never fetched. An
/// invalid ETag is not stored.
pub fn write(dir: &Path, key: &CacheKey, cached: &Cached) -> io::Result<()> {
    ensure_dir(dir)?;
    let path = dir.join(key.file_name());
    if is_link(&path) {
        return Err(io::Error::other("the cache file is a symbolic link"));
    }
    let utf8 = |name: &BranchName| std::str::from_utf8(&name.0).ok().map(str::to_string);
    let prs = cached
        .badges
        .iter()
        .filter_map(|(name, badge)| Some((utf8(name)?, StoredBadge::from(*badge))))
        .collect();
    let file = CacheFile {
        version: VERSION,
        key: key.clone(),
        fetched_at: cached.fetched_at,
        etag: cached.etag.as_deref().and_then(valid_etag),
        queried: cached
            .queried
            .iter()
            .filter_map(|head| Some((utf8(&head.branch)?, head.owner.clone(), head.name.clone())))
            .collect(),
        forked: cached.forked,
        prs,
    };
    let bytes = serde_json::to_vec(&file).map_err(io::Error::other)?;
    let (temp, mut out) = create_temp(&path)?;
    let written = out
        .write_all(&bytes)
        .and_then(|()| fs::rename(&temp, &path));
    if written.is_err() {
        // Only ever a name this call created.
        let _ = fs::remove_file(&temp);
    }
    written
}

/// Make `dir` if it is missing, owner-only, and refuse it as a link. A
/// looser mode on one already there is tightened.
pub fn ensure_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)?;
    let meta = fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(io::Error::other(
            "the cache directory is not a plain directory",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

fn is_link(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

/// How many temp names [`create_temp`] tries before giving up.
const TEMP_ATTEMPTS: u32 = 16;

/// A new owner-only temp file beside `path`, created exclusively so an
/// entry already at a name is skipped, never followed or overwritten.
fn create_temp(path: &Path) -> io::Result<(PathBuf, File)> {
    let file_name = path.file_name().unwrap_or_default();
    let pid = std::process::id();
    let mut last = io::Error::from(io::ErrorKind::AlreadyExists);
    for n in 0..TEMP_ATTEMPTS {
        let mut name = file_name.to_os_string();
        name.push(format!(".{pid}.{n}.tmp"));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::fake::TempDir;

    fn key(top: &str) -> CacheKey {
        CacheKey {
            host: "github.com".into(),
            owner: "acme".into(),
            repo: "widgets".into(),
            top: top.into(),
        }
    }

    fn badges() -> HashMap<BranchName, PrBadge> {
        HashMap::from([
            (
                BranchName::from("main"),
                PrBadge {
                    number: 7,
                    state: PrState::Draft,
                    ci: Some(CiState::Pending),
                },
            ),
            (
                BranchName::from("feature/x"),
                PrBadge {
                    number: 9,
                    state: PrState::Merged,
                    ci: None,
                },
            ),
        ])
    }

    fn head(branch: &str, owner: &str, name: &str) -> Head {
        Head {
            branch: branch.into(),
            owner: owner.into(),
            name: name.into(),
        }
    }

    fn cached() -> Cached {
        Cached {
            badges: badges(),
            etag: Some("W/\"abc\"".into()),
            queried: vec![
                head("main", "acme", "main"),
                head("feature/x", "me", "feature/x"),
                head("no-pr", "acme", "renamed"),
            ],
            forked: true,
            fetched_at: 1_700_000_000,
        }
    }

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |key| {
            vars.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[test]
    fn a_written_entry_reads_back() {
        let tmp = TempDir::new("forge-cache");
        let dir = tmp.path().join("kika").join("forge");
        assert_eq!(read(&dir, &key("/r")), None);
        write(&dir, &key("/r"), &cached()).unwrap();
        assert_eq!(read(&dir, &key("/r")), Some(cached()));
        // Only the file is left, no temp.
        let names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, [OsString::from(key("/r").file_name())]);
        let text = fs::read_to_string(dir.join(key("/r").file_name())).unwrap();
        assert!(text.contains("\"fetched_at\":1700000000"), "{text}");
    }

    #[test]
    fn two_clones_of_one_remote_get_different_files() {
        let tmp = TempDir::new("forge-cache");
        assert_ne!(key("/a").file_name(), key("/b").file_name());
        // Parts never run into each other.
        let shifted = CacheKey {
            owner: "acm".into(),
            repo: "ewidgets".into(),
            ..key("/a")
        };
        assert_ne!(key("/a").file_name(), shifted.file_name());
        assert_eq!(key("/a").file_name().len(), 16 + ".json".len());

        write(tmp.path(), &key("/a"), &cached()).unwrap();
        assert_eq!(read(tmp.path(), &key("/b")), None);
    }

    #[test]
    fn a_stored_key_that_differs_is_a_miss() {
        let tmp = TempDir::new("forge-cache");
        write(tmp.path(), &key("/a"), &cached()).unwrap();
        // Another repository's entry planted under this one's name.
        fs::rename(
            tmp.path().join(key("/a").file_name()),
            tmp.path().join(key("/b").file_name()),
        )
        .unwrap();
        assert_eq!(read(tmp.path(), &key("/b")), None);
    }

    #[test]
    fn malformed_or_oversized_files_are_misses() {
        let tmp = TempDir::new("forge-cache");
        let path = tmp.path().join(key("/a").file_name());
        let valid = |extra: &str| {
            format!(
                r#"{{"version":3,"key":{{"host":"github.com","owner":"acme","repo":"widgets","top":"/a"}},"fetched_at":0,"etag":null,"queried":[["main","acme","main"]],"forked":false,"prs":{{"main":{{"number":1,"state":"open","ci":null}}}}{extra}}}"#
            )
        };
        fs::write(&path, valid("")).unwrap();
        assert!(read(tmp.path(), &key("/a")).is_some());
        for bad in [
            "{".to_string(),
            valid(r#","token":"x""#),
            valid("").replace("\"version\":3", "\"version\":4"),
            valid("").replace("[\"main\",\"acme\",\"main\"]", "\"main\""),
            valid("").replace("\"open\"", "\"weird\""),
            valid("").replace("\"number\":1", "\"number\":-1"),
        ] {
            fs::write(&path, bad.as_bytes()).unwrap();
            assert_eq!(read(tmp.path(), &key("/a")), None, "{bad}");
        }
        let mut big = valid("").into_bytes();
        big.extend(std::iter::repeat_n(b' ', 1024 * 1024));
        fs::write(&path, big).unwrap();
        assert_eq!(read(tmp.path(), &key("/a")), None);
    }

    #[test]
    fn a_version_1_file_is_a_miss() {
        let tmp = TempDir::new("forge-cache");
        let v1 = r#"{"version":1,"key":{"host":"github.com","owner":"acme","repo":"widgets","top":"/a"},"fetched_at":0,"prs":{"main":{"number":1,"state":"open","ci":null}}}"#;
        fs::write(tmp.path().join(key("/a").file_name()), v1).unwrap();
        assert_eq!(read(tmp.path(), &key("/a")), None);
    }

    #[test]
    fn a_version_2_file_is_a_miss() {
        let tmp = TempDir::new("forge-cache");
        let v2 = r#"{"version":2,"key":{"host":"github.com","owner":"acme","repo":"widgets","top":"/a"},"fetched_at":0,"etag":null,"queried":["main"],"prs":{"main":{"number":1,"state":"open","ci":null}}}"#;
        fs::write(tmp.path().join(key("/a").file_name()), v2).unwrap();
        assert_eq!(read(tmp.path(), &key("/a")), None);
    }

    #[test]
    fn heads_and_forked_round_trip() {
        let tmp = TempDir::new("forge-cache");
        for forked in [true, false] {
            let cached = Cached { forked, ..cached() };
            write(tmp.path(), &key("/a"), &cached).unwrap();
            let back = read(tmp.path(), &key("/a")).unwrap();
            assert_eq!((back.queried, back.forked), (cached.queried, forked));
        }
    }

    #[test]
    fn the_etag_and_queried_branches_round_trip_and_a_bad_etag_is_dropped() {
        let tmp = TempDir::new("forge-cache");
        write(tmp.path(), &key("/a"), &cached()).unwrap();
        let back = read(tmp.path(), &key("/a")).unwrap();
        assert_eq!(back.etag.as_deref(), Some("W/\"abc\""));
        assert_eq!(back.queried, cached().queried);

        let long = format!("\"{}\"", "a".repeat(300));
        for bad in ["\"a\u{1b}b\"", long.as_str(), "abc"] {
            let with_bad = Cached {
                etag: Some(bad.into()),
                ..cached()
            };
            write(tmp.path(), &key("/a"), &with_bad).unwrap();
            let text = fs::read_to_string(tmp.path().join(key("/a").file_name())).unwrap();
            assert!(text.contains("\"etag\":null"), "{text}");
            assert_eq!(read(tmp.path(), &key("/a")).unwrap().etag, None);
        }
        // One planted in the file is not handed back either.
        let path = tmp.path().join(key("/a").file_name());
        let planted = fs::read_to_string(&path)
            .unwrap()
            .replace("\"etag\":null", "\"etag\":\"abc\"");
        fs::write(&path, planted).unwrap();
        let back = read(tmp.path(), &key("/a")).unwrap();
        assert_eq!((back.etag, back.badges), (None, badges()));
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    #[test]
    fn the_dir_and_file_are_private_and_looser_modes_are_repaired() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new("forge-cache");
        let dir = tmp.path().join("forge");
        write(&dir, &key("/a"), &cached()).unwrap();
        let file = dir.join(key("/a").file_name());
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&file), 0o600);

        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        write(&dir, &key("/a"), &cached()).unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&file), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_files_and_dirs_are_refused() {
        let tmp = TempDir::new("forge-cache");
        let real = tmp.path().join("real");
        write(&real, &key("/a"), &cached()).unwrap();

        // A linked directory is neither read nor written.
        let linked = tmp.path().join("linked");
        std::os::unix::fs::symlink(&real, &linked).unwrap();
        assert_eq!(read(&linked, &key("/a")), None);
        assert!(write(&linked, &key("/a"), &cached()).is_err());

        // A linked file is a miss, and is never written through.
        let dir = tmp.path().join("dir");
        ensure_dir(&dir).unwrap();
        let file = dir.join(key("/a").file_name());
        std::os::unix::fs::symlink(real.join(key("/a").file_name()), &file).unwrap();
        assert_eq!(read(&dir, &key("/a")), None);
        assert!(write(&dir, &key("/a"), &Cached::default()).is_err());
        assert_eq!(read(&real, &key("/a")), Some(cached()));
    }

    #[cfg(unix)]
    #[test]
    fn each_platform_has_its_cache_dir_and_xdg_wins() {
        let vars = [("HOME", "/h"), ("LOCALAPPDATA", "/l")];
        assert_eq!(
            dir_on(Platform::Mac, env(&vars)),
            Some(PathBuf::from("/h/Library/Caches/kika/forge"))
        );
        assert_eq!(
            dir_on(Platform::Unix, env(&vars)),
            Some(PathBuf::from("/h/.cache/kika/forge"))
        );
        assert_eq!(
            dir_on(Platform::Windows, env(&vars)),
            Some(PathBuf::from("/l/kika/cache/forge"))
        );
        let xdg = [
            ("XDG_CACHE_HOME", "/x"),
            ("HOME", "/h"),
            ("LOCALAPPDATA", "/l"),
        ];
        for platform in [Platform::Mac, Platform::Unix, Platform::Windows] {
            assert_eq!(
                dir_on(platform, env(&xdg)),
                Some(PathBuf::from("/x/kika/forge")),
                "{platform:?}"
            );
        }
    }

    #[test]
    fn relative_or_empty_values_count_as_unset() {
        let vars = [
            ("XDG_CACHE_HOME", "rel"),
            ("HOME", ""),
            ("LOCALAPPDATA", "x"),
        ];
        for platform in [Platform::Mac, Platform::Unix, Platform::Windows] {
            assert_eq!(dir_on(platform, env(&vars)), None, "{platform:?}");
            assert_eq!(dir_on(platform, env(&[])), None);
        }
    }
}
