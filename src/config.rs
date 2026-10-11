//! The user's settings, read from TOML files and laid over the built-in
//! defaults. Two files, both optional:
//!
//! - global: `$XDG_CONFIG_HOME/kika/config.toml`, else
//!   `~/.config/kika/config.toml` (Linux and macOS), or
//!   `%APPDATA%\kika\config.toml` on Windows;
//! - per repository: `.kika.toml` at the root of the repository Kika
//!   started in. It overrides the global file.
//!
//! ```toml
//! [keys]
//! "files.stage" = ["s", "ctrl-s"]   # replace this binding's keys
//! "files.discard" = []              # disable it
//! [watch]
//! refs = 2        # seconds
//! files = 5
//! fetch = 300     # absent = off. Setting it is the opt-in to network access.
//! ```
//!
//! `[watch]` is read from the global file only, at startup. A repository
//! file arrives with a clone and is not trusted: it may hold `[keys]` and
//! nothing else, so it can never turn on fetching, and it cannot change the
//! bindings in [`REPO_LOCKED`](crate::keymap::REPO_LOCKED), and it must be
//! a plain file, not a symlink. Either file must be a regular file of at most
//! 64 KiB. A file with any error is ignored as a whole, and the layer below
//! it applies.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::keymap::{Keymap, KeysTable, Trust};
use crate::watch::{DEFAULT_WATCH, WatchConfig};

/// Where the config files are. Either may be unknown.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigPaths {
    pub global: Option<PathBuf>,
    pub repo: Option<PathBuf>,
}

impl ConfigPaths {
    /// The files for a Kika started in the repository at `workdir`.
    pub fn from_env(workdir: &Path) -> Self {
        Self::locate(|name| std::env::var_os(name), cfg!(windows), workdir)
    }

    /// `from_env` with the environment passed in, so tests set their own.
    fn locate(
        var: impl Fn(&str) -> Option<std::ffi::OsString>,
        windows: bool,
        workdir: &Path,
    ) -> Self {
        // A relative base would depend on where Kika started; the XDG spec
        // says to ignore one.
        let absolute = |name| var(name).map(PathBuf::from).filter(|p| p.is_absolute());
        let base = if windows {
            absolute("APPDATA")
        } else {
            absolute("XDG_CONFIG_HOME").or_else(|| absolute("HOME").map(|h| h.join(".config")))
        };
        Self {
            global: base.map(|b| b.join("kika").join("config.toml")),
            repo: Some(workdir.join(".kika.toml")),
        }
    }
}

/// A config file that was ignored, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub path: String,
    pub message: String,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

/// The settings in force after every layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    pub keymap: Keymap,
    pub watch: WatchConfig,
    pub errors: Vec<ConfigError>,
}

/// The global file. Unknown fields are errors, so a typo is reported
/// instead of silently doing nothing.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct GlobalFile {
    #[serde(default)]
    keys: KeysTable,
    #[serde(default)]
    watch: WatchTable,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct WatchTable {
    refs: Option<u64>,
    files: Option<u64>,
    fetch: Option<u64>,
}

/// A repository's file: keys only. A `[watch]` table here is an unknown
/// field, so a cloned repository cannot turn on fetching.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RepoFile {
    #[serde(default)]
    keys: KeysTable,
}

/// Read both files over the defaults. Never fails: each file that cannot
/// be read, parsed or applied is skipped and reported in `errors`.
pub fn load(paths: &ConfigPaths) -> Loaded {
    let mut loaded = Loaded {
        keymap: Keymap::default(),
        watch: DEFAULT_WATCH,
        errors: Vec::new(),
    };
    let global = layer(&paths.global, Trust::Global, &mut loaded.errors, |text| {
        apply_global(text, &loaded.keymap, loaded.watch)
    });
    if let Some((keymap, watch)) = global {
        (loaded.keymap, loaded.watch) = (keymap, watch);
    }
    let repo = layer(&paths.repo, Trust::Repo, &mut loaded.errors, |text| {
        apply_repo(text, &loaded.keymap)
    });
    if let Some(keymap) = repo {
        loaded.keymap = keymap;
    }
    loaded
}

/// `apply` run on the file at `path`, if there is one. A failure is
/// recorded in `errors` and gives `None`, like a missing file.
fn layer<T>(
    path: &Option<PathBuf>,
    trust: Trust,
    errors: &mut Vec<ConfigError>,
    apply: impl FnOnce(&str) -> Result<T, String>,
) -> Option<T> {
    let path = path.as_deref()?;
    match read(path, trust).and_then(|text| text.as_deref().map(apply).transpose()) {
        Ok(applied) => applied,
        Err(message) => {
            errors.push(ConfigError {
                path: path.display().to_string(),
                message,
            });
            None
        }
    }
}

/// A config file holds a few hundred bytes; anything far larger is not one.
const MAX_FILE_BYTES: u64 = 64 * 1024;

/// The file's text, or `None` when there is no file: that is not an error.
/// Only a regular file is opened: a FIFO or a device such as `/dev/zero`
/// could block Kika or never end. A repository file must not be a symlink
/// either, since a clone controls where it points; the user's own global
/// file may be one, as dotfile managers make.
fn read(path: &Path, trust: Trust) -> Result<Option<String>, String> {
    let cannot = |e: io::Error| format!("cannot read the file: {e}");
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(cannot(e)),
    };
    let target = if meta.file_type().is_symlink() {
        if trust == Trust::Repo {
            return Err("a symbolic link; a repository's config file must be a plain file".into());
        }
        fs::metadata(path).map_err(cannot)?
    } else {
        meta
    };
    let not_regular = || "not a regular file".to_string();
    if !target.is_file() {
        return Err(not_regular());
    }
    let file = File::open(path).map_err(cannot)?;
    // It may have been swapped since the check; ask the open handle.
    if !file.metadata().map_err(cannot)?.is_file() {
        return Err(not_regular());
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(cannot)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(format!(
            "too large: a config file may hold at most {} KiB",
            MAX_FILE_BYTES / 1024
        ));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| "not UTF-8 text".to_string())
}

/// `keymap` and `watch` with the global file's text applied.
fn apply_global(
    text: &str,
    keymap: &Keymap,
    watch: WatchConfig,
) -> Result<(Keymap, WatchConfig), String> {
    let file = parse_global(text)?;
    let keymap = Keymap::build(keymap, &file.keys, Trust::Global)?;
    let seconds = |name: &str, value: Option<u64>| match value {
        // A zero interval would poll without pause.
        Some(0) => Err(format!("watch.{name} must be at least 1 second")),
        other => Ok(other.map(Duration::from_secs)),
    };
    let watch = WatchConfig {
        refs: seconds("refs", file.watch.refs)?.unwrap_or(watch.refs),
        files: seconds("files", file.watch.files)?.unwrap_or(watch.files),
        fetch: seconds("fetch", file.watch.fetch)?.or(watch.fetch),
    };
    Ok((keymap, watch))
}

/// `keymap` with a repository file's text applied.
fn apply_repo(text: &str, keymap: &Keymap) -> Result<Keymap, String> {
    let file = parse_repo(text)?;
    Keymap::build(keymap, &file.keys, Trust::Repo)
}

fn parse_global(text: &str) -> Result<GlobalFile, String> {
    toml::from_str(text).map_err(|e| toml_error(text, &e))
}

fn parse_repo(text: &str) -> Result<RepoFile, String> {
    toml::from_str(text).map_err(|e| toml_error(text, &e))
}

/// The parser's message, with the line and column it points at.
fn toml_error(text: &str, error: &toml::de::Error) -> String {
    let message = error.message().trim_end();
    match error.span() {
        Some(span) => {
            let before = &text[..span.start.min(text.len())];
            let line = before.matches('\n').count() + 1;
            let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
            format!("line {line}, column {column}: {message}")
        }
        None => message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;
    use crate::keymap::{KeyPattern, parse_key, registry};

    fn keys_of<'a>(keymap: &'a Keymap, id: &str) -> &'a [KeyPattern] {
        keymap.keys(registry().iter().find(|b| b.id == id).unwrap())
    }

    fn key(text: &str) -> KeyPattern {
        parse_key(text).unwrap()
    }

    fn global(text: &str) -> Result<(Keymap, WatchConfig), String> {
        apply_global(text, &Keymap::default(), DEFAULT_WATCH)
    }

    fn repo(text: &str) -> Result<Keymap, String> {
        apply_repo(text, &Keymap::default())
    }

    #[test]
    fn the_global_file_sits_in_the_xdg_config_dir_and_the_repo_file_at_the_root() {
        let env = |pairs: &'static [(&str, &str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| OsString::from(v))
            }
        };
        let root = Path::new("/work/repo");
        let paths = ConfigPaths::locate(env(&[("HOME", "/home/me")]), false, root);
        assert_eq!(
            paths,
            ConfigPaths {
                global: Some("/home/me/.config/kika/config.toml".into()),
                repo: Some("/work/repo/.kika.toml".into()),
            }
        );
        let xdg = env(&[("HOME", "/home/me"), ("XDG_CONFIG_HOME", "/xdg")]);
        let paths = ConfigPaths::locate(xdg, false, root);
        assert_eq!(paths.global, Some("/xdg/kika/config.toml".into()));
        let relative = env(&[("HOME", "/home/me"), ("XDG_CONFIG_HOME", "rel")]);
        let paths = ConfigPaths::locate(relative, false, root);
        assert_eq!(
            paths.global,
            Some("/home/me/.config/kika/config.toml".into())
        );
        assert_eq!(ConfigPaths::locate(env(&[]), false, root).global, None);
        #[cfg(windows)]
        assert_eq!(
            ConfigPaths::locate(env(&[("APPDATA", r"C:\Users\me\AppData")]), true, root).global,
            Some(r"C:\Users\me\AppData\kika\config.toml".into())
        );
    }

    #[test]
    fn the_global_file_remaps_keys_and_sets_the_watch() {
        let (keymap, watch) = global(
            r#"
            [keys]
            "files.stage" = ["x", "ctrl-x"]
            "files.discard" = []
            [watch]
            refs = 3
            fetch = 300
            "#,
        )
        .unwrap();
        assert_eq!(keys_of(&keymap, "files.stage"), [key("x"), key("ctrl-x")]);
        assert!(keys_of(&keymap, "files.discard").is_empty());
        assert_eq!(
            watch,
            WatchConfig {
                refs: Duration::from_secs(3),
                files: DEFAULT_WATCH.files,
                fetch: Some(Duration::from_secs(300)),
            }
        );
    }

    #[test]
    fn errors_name_the_problem_and_a_syntax_error_its_line() {
        let err = |text: &str| global(text).unwrap_err();
        let syntax = err("[keys]\n\"files.stage\" = [\"x\"\n");
        assert!(syntax.starts_with("line 2, column "), "{syntax}");
        assert_eq!(
            err("[keys]\n\"files.stag\" = [\"x\"]\n"),
            r#"unknown binding "files.stag""#
        );
        let bad = err("[keys]\n\"files.stage\" = [\"hyper-x\"]\n");
        assert!(
            bad.starts_with(r#""files.stage": bad key "hyper-x": use one character"#),
            "{bad}"
        );
        assert_eq!(
            err("[keys]\n\"files.stage\" = [\"c\"]\n"),
            r#"key "c" is bound to both "files.stage" and "files.commit""#
        );
        assert_eq!(
            err("[keys]\n\"global.quit\" = []\n"),
            r#""global.quit" needs at least one key"#
        );
        let typo = err("[watch]\nfetchh = 3\n");
        assert!(typo.contains("unknown field `fetchh`"), "{typo}");
        assert_eq!(
            err("[watch]\nfiles = 0\n"),
            "watch.files must be at least 1 second"
        );
        assert!(err("[theme]\n").contains("unknown field `theme`"));
    }

    #[test]
    fn a_repository_file_can_remap_keys_but_never_set_the_watch() {
        let keymap = repo("[keys]\n\"files.stage\" = [\"y\"]\n").unwrap();
        assert_eq!(keys_of(&keymap, "files.stage"), [key("y")]);
        let watch = repo("[watch]\nfetch = 60\n").unwrap_err();
        assert!(watch.contains("unknown field `watch`"), "{watch}");
        let locked = repo("[keys]\n\"files.open\" = [\"x\"]\n").unwrap_err();
        assert_eq!(
            locked,
            r#""files.open" can only be changed in the global config file"#
        );
        assert!(global("[keys]\n\"files.open\" = [\"x\"]\n").is_ok());
    }

    /// A temporary directory with `config.toml` and `.kika.toml` as given.
    struct Files {
        dir: PathBuf,
    }

    impl Files {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("kika-config-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }

        fn paths(&self) -> ConfigPaths {
            ConfigPaths {
                global: Some(self.dir.join("config.toml")),
                repo: Some(self.dir.join(".kika.toml")),
            }
        }

        fn global(&self, text: &str) -> &Self {
            std::fs::write(self.dir.join("config.toml"), text).unwrap();
            self
        }

        fn repo(&self, text: &str) -> &Self {
            std::fs::write(self.dir.join(".kika.toml"), text).unwrap();
            self
        }

        fn load(&self) -> Loaded {
            load(&self.paths())
        }
    }

    impl Drop for Files {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn missing_files_load_silently_as_the_defaults() {
        let files = Files::new("missing");
        assert_eq!(
            files.load(),
            Loaded {
                keymap: Keymap::default(),
                watch: DEFAULT_WATCH,
                errors: vec![],
            }
        );
        assert_eq!(load(&ConfigPaths::default()).errors, []);
    }

    #[test]
    fn the_repository_file_overrides_the_global_one() {
        let files = Files::new("layers");
        files
            .global("[keys]\n\"files.stage\" = [\"x\"]\n\"files.commit\" = [\"C\"]\n")
            .repo("[keys]\n\"files.stage\" = [\"y\"]\n");
        let loaded = files.load();
        assert_eq!(loaded.errors, []);
        assert_eq!(keys_of(&loaded.keymap, "files.stage"), [key("y")]);
        assert_eq!(keys_of(&loaded.keymap, "files.commit"), [key("C")]);
    }

    #[test]
    fn a_broken_file_is_dropped_whole_and_the_layer_below_applies() {
        let files = Files::new("broken");
        files
            .global("[keys]\n\"files.stage\" = [\"x\"]\n[watch]\nfetch = 300\n")
            .repo("[keys]\n\"files.commit\" = [\"C\"]\n\"files.nope\" = [\"y\"]\n");
        let loaded = files.load();
        assert_eq!(keys_of(&loaded.keymap, "files.stage"), [key("x")]);
        assert_eq!(keys_of(&loaded.keymap, "files.commit"), [key("c")]);
        let path = files.dir.join(".kika.toml").display().to_string();
        assert_eq!(
            loaded.errors,
            [ConfigError {
                path: path.clone(),
                message: r#"unknown binding "files.nope""#.into(),
            }]
        );
        assert_eq!(
            loaded.errors[0].to_string(),
            format!(r#"{path}: unknown binding "files.nope""#)
        );

        files.global("[keys\n");
        let loaded = files.load();
        assert_eq!(loaded.keymap, Keymap::default());
        assert_eq!(loaded.watch, DEFAULT_WATCH);
        assert_eq!(loaded.errors.len(), 2);
        assert!(loaded.errors[0].message.starts_with("line 1, column "));
    }

    #[test]
    fn only_a_regular_file_of_sane_size_is_read() {
        let files = Files::new("odd");
        let repo = files.dir.join(".kika.toml");
        let only_error = |files: &Files| {
            let loaded = files.load();
            assert_eq!(loaded.keymap, Keymap::default());
            match loaded.errors.as_slice() {
                [error] => error.message.clone(),
                other => panic!("expected one error, got {other:?}"),
            }
        };

        std::fs::create_dir(&repo).unwrap();
        assert_eq!(only_error(&files), "not a regular file");
        std::fs::remove_dir(&repo).unwrap();

        let big = format!(
            "[keys]\n\"files.stage\" = [\"x\"]\n#{}\n",
            "a".repeat(64 * 1024)
        );
        files.repo(&big);
        assert_eq!(
            only_error(&files),
            "too large: a config file may hold at most 64 KiB"
        );

        std::fs::write(&repo, b"[keys]\n\xff\n").unwrap();
        assert_eq!(only_error(&files), "not UTF-8 text");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_repository_file_is_refused_but_a_symlinked_global_one_is_read() {
        let files = Files::new("symlink");
        let target = files.dir.join("target.toml");
        std::fs::write(&target, "[keys]\n\"files.stage\" = [\"x\"]\n").unwrap();
        std::os::unix::fs::symlink(&target, files.dir.join(".kika.toml")).unwrap();
        let loaded = files.load();
        assert_eq!(loaded.keymap, Keymap::default());
        assert_eq!(loaded.errors.len(), 1);
        assert_eq!(
            loaded.errors[0].message,
            "a symbolic link; a repository's config file must be a plain file"
        );

        std::fs::remove_file(files.dir.join(".kika.toml")).unwrap();
        std::os::unix::fs::symlink(&target, files.dir.join("config.toml")).unwrap();
        let loaded = files.load();
        assert_eq!(loaded.errors, []);
        assert_eq!(keys_of(&loaded.keymap, "files.stage"), [key("x")]);

        // A link to a device is not a regular file, and is never opened.
        std::fs::remove_file(files.dir.join("config.toml")).unwrap();
        std::os::unix::fs::symlink("/dev/zero", files.dir.join("config.toml")).unwrap();
        let loaded = files.load();
        assert_eq!(loaded.errors.len(), 1);
        assert_eq!(loaded.errors[0].message, "not a regular file");
    }

    #[test]
    fn only_the_global_file_turns_fetching_on() {
        let files = Files::new("fetch");
        files.repo("[watch]\nfetch = 60\n");
        let loaded = files.load();
        assert_eq!(loaded.watch.fetch, None);
        assert_eq!(loaded.errors.len(), 1);
        assert!(loaded.errors[0].message.contains("unknown field `watch`"));

        files.global("[watch]\nfetch = 300\n").repo("");
        let loaded = files.load();
        assert_eq!(loaded.errors, []);
        assert_eq!(loaded.watch.fetch, Some(Duration::from_secs(300)));
    }
}
