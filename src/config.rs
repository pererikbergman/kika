//! The user's settings, read from TOML files and laid over the built-in
//! defaults. Two files, both optional:
//!
//! - global: `$XDG_CONFIG_HOME/kika/config.toml`, else
//!   `~/.config/kika/config.toml` (Linux and macOS), or
//!   `%APPDATA%\kika\config.toml` on Windows;
//! - per repository: `.kika.toml` at the top level of the repository open
//!   now (an entered submodule keeps its parent's). It overrides the global
//!   file. Outside a repository there is none.
//!
//! ```toml
//! [keys]
//! "files.stage" = ["x", "ctrl-s"]   # replace this binding's keys
//! "files.discard" = []              # disable it
//! [watch]
//! refs = 2        # seconds
//! files = 5
//! fetch = 300     # absent = off. Setting it is the opt-in to network access.
//! [diff]
//! pager = "delta --paging=never"   # a renderer for the Diff pane
//! tool = "difft --color=always"    # a display-only tool, run on OLD NEW
//! [commit]
//! hooks_detached = true   # hooks never ask: a commit that only runs them stays detached
//! [forge]
//! github = ["github.com"]   # absent or empty = off. The opt-in to asking GitHub.
//! gitlab = ["gitlab.com"]   # URLs only: `gitlab` alone never contacts a host
//! gitlab_ask = ["gitlab.com"]  # the opt-in to asking GitLab, through glab
//! browser = ["open"]        # argv that opens a URL, given as its last argument
//! [forge.allow]
//! "github.com" = ["acme", "me/kika"]  # absent = every repository on the host
//! "gitlab.com" = ["acme"]             # a gitlab_ask host is limited the same way
//! [display]
//! emoji_zwj_terminals = ["WezTerm"]  # terminals that draw an emoji sequence as one picture
//! [clipboard]
//! command = ["pbcopy"]      # absent = off: copies use OSC 52. Gets the text on stdin.
//! ```
//!
//! `[watch]`, `[diff]`, `[commit]`, `[forge]`, `[display]` and
//! `[clipboard]` are read from the global file only, at startup.
//! `[display] emoji_zwj_terminals` is off (empty) by default;
//! [`emoji_zwj`] matches it against the running terminal once.
//! `[[commands]]` is read from the global file only. A repository file
//! arrives with a clone and is not trusted: it may hold `[keys]` and
//! nothing else, so it can never turn on fetching, name a program to run or
//! run one, and it cannot change the bindings in
//! [`REPO_LOCKED`](crate::keymap::REPO_LOCKED), and it must be a plain
//! file, not a symlink. Either file must be a regular file of at most
//! 64 KiB. A `[keys]` entry that binds a key twice in one scope is dropped
//! alone: its binding keeps the keys of the layer below, and the rest of the
//! file applies. A global `[[commands]]` entry on a key the same file's
//! `[keys]` gives a binding of its scope is dropped alone too, whatever its
//! other keys: the binding keeps the key. The commands left must pass every
//! other check. A file with any other error, a command left clashing with a
//! default key or another command included, is ignored as a whole, and the
//! layer below it applies.
//!
//! The user's guide to both files, with every binding id, is
//! `docs/configuration.md`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::features::custom_commands::CommandTable;
use crate::features::diff::renderers::external_diff::{check_argv, parse_command};
use crate::keymap::{Keymap, KeysTable, Trust};
use crate::watch::{DEFAULT_WATCH, WatchConfig};
use kika_forge::ForgeHost;
use kika_forge::allow::AllowRule;
use kika_git::diff::EmojiZwj;

/// Where the config files are. Either may be unknown.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigPaths {
    pub global: Option<PathBuf>,
    pub repo: Option<PathBuf>,
}

/// The per-repository file's name, at the repository's top level.
pub const REPO_FILE: &str = ".kika.toml";

impl ConfigPaths {
    /// The `global` file, and the file of the repository whose top level
    /// is `repo`. Outside a repository there is no repository file: a
    /// stray one in some directory is not a repository's.
    pub fn new(global: Option<PathBuf>, repo: Option<&Path>) -> Self {
        Self {
            global,
            repo: repo.map(|top| top.join(REPO_FILE)),
        }
    }
}

/// Where this user's global file is, from the environment.
pub fn global_path_from_env() -> Option<PathBuf> {
    global_path(|name| std::env::var_os(name), cfg!(windows))
}

/// `global_path_from_env` with the environment passed in, so tests set their own.
fn global_path(var: impl Fn(&str) -> Option<std::ffi::OsString>, windows: bool) -> Option<PathBuf> {
    // A relative base would depend on where Kika started; the XDG spec
    // says to ignore one.
    let absolute = |name| var(name).map(PathBuf::from).filter(|p| p.is_absolute());
    let base = if windows {
        absolute("APPDATA")
    } else {
        absolute("XDG_CONFIG_HOME").or_else(|| absolute("HOME").map(|h| h.join(".config")))
    };
    base.map(|b| b.join("kika").join("config.toml"))
}

/// A config file that was ignored, or one of its `[keys]` entries that
/// was dropped, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub path: String,
    pub message: String,
    /// Only one key change or command was dropped and the rest of the file applied;
    /// `false` means the whole file was ignored.
    pub dropped_binding: bool,
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
    /// The global `[diff] pager` command, checked by `parse_command`.
    pub diff_pager: Option<String>,
    /// The hosts Kika may contact: the global `[forge] github` hosts, then
    /// the `gitlab_ask` ones as GitLab hosts, each with its
    /// `[forge.allow]` list. Empty keeps the forge off.
    pub forge_hosts: Vec<ForgeHost>,
    /// The global `[forge] gitlab` hosts, used to build URLs. `gitlab`
    /// alone never contacts a host; `gitlab_ask` does.
    pub forge_gitlab_hosts: Vec<ForgeHost>,
    /// The global `[forge] browser` argv, checked by `check_argv`; `None`
    /// leaves opening a URL off.
    pub browser: Option<Vec<OsString>>,
    /// The global `[diff] tool` command, checked by `parse_command`.
    pub diff_tool: Option<String>,
    /// The global `[commit] hooks_detached`: hooks alone do not hand a
    /// commit the terminal.
    pub commit_hooks_detached: bool,
    /// The global `[display] emoji_zwj_terminals`: the terminals that draw
    /// an emoji ZWJ sequence as one two-column picture.
    pub emoji_zwj_terminals: Vec<String>,
    /// The global `[clipboard] command` argv, checked by `check_argv`;
    /// `None` keeps copying on OSC 52.
    pub clipboard_command: Option<Vec<OsString>>,
    pub errors: Vec<ConfigError>,
}

/// The global file's settings besides the keys.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GlobalSettings {
    keymap: Keymap,
    /// One message per `[keys]` entry dropped for a key clash.
    dropped: Vec<String>,
    watch: WatchConfig,
    diff_pager: Option<String>,
    forge_hosts: Vec<ForgeHost>,
    forge_gitlab_hosts: Vec<ForgeHost>,
    browser: Option<Vec<OsString>>,
    diff_tool: Option<String>,
    commit_hooks_detached: bool,
    emoji_zwj_terminals: Vec<String>,
    clipboard_command: Option<Vec<OsString>>,
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
    #[serde(default)]
    diff: DiffTable,
    #[serde(default)]
    display: DisplayTable,
    #[serde(default)]
    commands: Vec<CommandTable>,
    #[serde(default)]
    commit: CommitTable,
    #[serde(default)]
    forge: ForgeTable,
    #[serde(default)]
    clipboard: ClipboardTable,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommitTable {
    hooks_detached: Option<bool>,
}

/// The hosts Kika may ask about pull requests, by forge, the repositories
/// on them it may ask about, and the program that opens a forge page.
/// Only here, in the user's own file: a cloned repository can never make
/// Kika contact a host, widen what it asks about, or run a program.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForgeTable {
    #[serde(default)]
    github: Vec<String>,
    /// Hosts whose URLs Kika builds. Alone, none is ever contacted.
    #[serde(default)]
    gitlab: Vec<String>,
    /// The `gitlab` hosts `glab` may be asked about merge requests: the
    /// opt-in to contacting GitLab. Each must be in `gitlab` and not in
    /// `github`.
    #[serde(default)]
    gitlab_ask: Vec<String>,
    browser: Option<Vec<String>>,
    /// Per `github` or `gitlab_ask` host, the owners and `owner/repo`s its
    /// CLI may be asked about; a host without a key allows all of them.
    #[serde(default)]
    allow: BTreeMap<String, Vec<String>>,
}

/// The program that copies text, given it on stdin. Only here, in the
/// user's own file: a cloned repository can never name a program to run.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClipboardTable {
    command: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct DisplayTable {
    emoji_zwj_terminals: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiffTable {
    pager: Option<String>,
    tool: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct WatchTable {
    refs: Option<u64>,
    files: Option<u64>,
    fetch: Option<u64>,
}

/// A repository's file: keys only. A `[watch]`, `[diff]`, `[commit]`,
/// `[forge]`, `[display]` or `[clipboard]` table, or `[[commands]]`, here
/// is an unknown field, so a cloned repository can neither turn on
/// fetching, name or run a program, take the terminal away from its own hooks, make Kika contact
/// a forge, nor change how text is drawn.
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
        diff_pager: None,
        forge_hosts: Vec::new(),
        forge_gitlab_hosts: Vec::new(),
        browser: None,
        diff_tool: None,
        commit_hooks_detached: false,
        emoji_zwj_terminals: Vec::new(),
        clipboard_command: None,
        errors: Vec::new(),
    };
    let global = layer(&paths.global, Trust::Global, &mut loaded.errors, |text| {
        apply_global(text, &loaded.keymap, loaded.watch)
    });
    if let Some(global) = global {
        dropped(&paths.global, global.dropped, &mut loaded.errors);
        (
            loaded.keymap,
            loaded.watch,
            loaded.diff_pager,
            loaded.diff_tool,
            loaded.commit_hooks_detached,
        ) = (
            global.keymap,
            global.watch,
            global.diff_pager,
            global.diff_tool,
            global.commit_hooks_detached,
        );
        loaded.forge_hosts = global.forge_hosts;
        loaded.forge_gitlab_hosts = global.forge_gitlab_hosts;
        loaded.browser = global.browser;
        loaded.emoji_zwj_terminals = global.emoji_zwj_terminals;
        loaded.clipboard_command = global.clipboard_command;
    }
    let repo = layer(&paths.repo, Trust::Repo, &mut loaded.errors, |text| {
        apply_repo(text, &loaded.keymap)
    });
    if let Some((keymap, messages)) = repo {
        dropped(&paths.repo, messages, &mut loaded.errors);
        loaded.keymap = keymap;
    }
    loaded
}

/// Record each of `messages`, a `[keys]` entry or a command of the file
/// at `path` dropped while the rest of the file applied.
fn dropped(path: &Option<PathBuf>, messages: Vec<String>, errors: &mut Vec<ConfigError>) {
    let Some(path) = path.as_deref() else {
        return;
    };
    errors.extend(messages.into_iter().map(|message| ConfigError {
        path: path.display().to_string(),
        message,
        dropped_binding: true,
    }));
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
                dropped_binding: false,
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

/// `keymap` and `watch` with the global file's text applied, and its
/// pager and tool.
fn apply_global(text: &str, keymap: &Keymap, watch: WatchConfig) -> Result<GlobalSettings, String> {
    let file = parse_global(text)?;
    let (keymap, mut dropped, kept) = Keymap::build_layer(keymap, &file.keys, Trust::Global)?;
    let (keymap, commands_dropped) = keymap.with_commands(&file.commands, &kept)?;
    dropped.extend(commands_dropped);
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
    // Refused here, not when R is pressed, so the mistake is shown at once.
    if let Some(pager) = &file.diff.pager {
        parse_command(pager).map_err(|e| format!("diff.pager: {e}"))?;
    }
    if let Some(tool) = &file.diff.tool {
        parse_command(tool).map_err(|e| format!("diff.tool: {e}"))?;
    }
    let parsed = |names: &[String], key: &str, parse: fn(&str) -> Result<ForgeHost, String>| {
        names
            .iter()
            .map(|host| parse(host).map_err(|e| format!("forge.{key}: {e}")))
            .collect::<Result<Vec<_>, _>>()
    };
    let hosts = |names: &[String], key: &str| parsed(names, key, ForgeHost::parse);
    let github = hosts(&file.forge.github, "github")?;
    let forge_gitlab_hosts = hosts(&file.forge.gitlab, "gitlab")?;
    let gitlab_ask = parsed(
        &file.forge.gitlab_ask,
        "gitlab_ask",
        ForgeHost::parse_gitlab,
    )?;
    check_gitlab_ask(&gitlab_ask, &forge_gitlab_hosts, &github)
        .map_err(|e| format!("forge.gitlab_ask: {e}"))?;
    let forge_hosts = attach_allow(
        github.into_iter().chain(gitlab_ask).collect(),
        &file.forge.allow,
    )
    .map_err(|e| format!("forge.allow: {e}"))?;
    let browser = file
        .forge
        .browser
        .map(|argv| check_argv(argv).map_err(|e| format!("forge.browser: {e}")))
        .transpose()?;
    let clipboard_command = file
        .clipboard
        .command
        .map(|argv| check_argv(argv).map_err(|e| format!("clipboard.command: {e}")))
        .transpose()?;
    let emoji_zwj_terminals = file.display.emoji_zwj_terminals.unwrap_or_default();
    // An empty name would match a terminal that sets no name at all.
    if emoji_zwj_terminals.iter().any(String::is_empty) {
        return Err("display.emoji_zwj_terminals: a terminal name cannot be empty".into());
    }
    Ok(GlobalSettings {
        keymap,
        dropped,
        watch,
        diff_pager: file.diff.pager,
        diff_tool: file.diff.tool,
        forge_hosts,
        forge_gitlab_hosts,
        browser,
        commit_hooks_detached: file.commit.hooks_detached.unwrap_or(false),
        emoji_zwj_terminals,
        clipboard_command,
    })
}

/// Each `gitlab_ask` host must be a `gitlab` one, so asking is an
/// opt-in on top of the URLs, and none a `github` one, so a host has one
/// kind and only one CLI is ever asked about it.
fn check_gitlab_ask(
    ask: &[ForgeHost],
    gitlab: &[ForgeHost],
    github: &[ForgeHost],
) -> Result<(), String> {
    let among =
        |hosts: &[ForgeHost], host: &ForgeHost| hosts.iter().any(|h| h.as_str() == host.as_str());
    for host in ask {
        if !among(gitlab, host) {
            return Err(format!(
                "{:?} is not one of the forge.gitlab hosts",
                host.as_str()
            ));
        }
        if among(github, host) {
            return Err(format!("{:?} is also a forge.github host", host.as_str()));
        }
    }
    Ok(())
}

/// `hosts` (the `github` and `gitlab_ask` ones) with each `[forge.allow]`
/// list attached to its host, and to every copy of it should the host be
/// listed twice. A key must name one of `hosts`: a list for a host that
/// is never asked would look like a limit while it limits nothing.
fn attach_allow(
    hosts: Vec<ForgeHost>,
    allow: &BTreeMap<String, Vec<String>>,
) -> Result<Vec<ForgeHost>, String> {
    let mut lists = BTreeMap::new();
    for (key, entries) in allow {
        let name = ForgeHost::parse(key)?;
        if !hosts.iter().any(|h| h.as_str() == name.as_str()) {
            return Err(format!(
                "{key:?} is not one of the forge.github or forge.gitlab_ask hosts"
            ));
        }
        let rules = entries
            .iter()
            .map(|entry| AllowRule::parse(entry))
            .collect::<Result<Vec<_>, _>>()?;
        lists.insert(name.as_str().to_string(), rules);
    }
    Ok(hosts
        .into_iter()
        .map(|host| match lists.get(host.as_str()) {
            Some(rules) => host.with_allow(rules.clone()),
            None => host,
        })
        .collect())
}

/// Whether the running terminal is one of `terminals`, the ones the user
/// says draw an emoji ZWJ sequence as one picture. It is named by
/// `TERM_PROGRAM`, else, when that is unset or empty, by `TERM`; the match
/// is exact. Inside tmux `TERM_PROGRAM` is `tmux`, which is right: tmux
/// decides the width there. No match, the default, escapes.
pub fn emoji_zwj(terminals: &[String], term_program: Option<&str>, term: Option<&str>) -> EmojiZwj {
    let name = term_program.filter(|p| !p.is_empty()).or(term);
    match name {
        Some(name) if !name.is_empty() && terminals.iter().any(|t| t == name) => EmojiZwj::Keep,
        _ => EmojiZwj::Escape,
    }
}

/// `keymap` with a repository file's text applied, and one message per
/// `[keys]` entry dropped for a key clash.
fn apply_repo(text: &str, keymap: &Keymap) -> Result<(Keymap, Vec<String>), String> {
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
    use kika_forge::ForgeRepo;
    use kika_forge::allow::AllowRule;

    fn keys_of<'a>(keymap: &'a Keymap, id: &str) -> &'a [KeyPattern] {
        keymap.keys(registry().iter().find(|b| b.id == id).unwrap())
    }

    fn key(text: &str) -> KeyPattern {
        parse_key(text).unwrap()
    }

    fn global(text: &str) -> Result<(Keymap, WatchConfig), String> {
        apply_global(text, &Keymap::default(), DEFAULT_WATCH).map(|g| (g.keymap, g.watch))
    }

    fn repo(text: &str) -> Result<(Keymap, Vec<String>), String> {
        apply_repo(text, &Keymap::default())
    }

    /// An environment holding only `pairs`, for `global_path`.
    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |name: &str| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| OsString::from(v))
        }
    }

    // Unix paths such as /home/me are not absolute on Windows; the Windows
    // lookup has its own test below.
    #[test]
    #[cfg(not(windows))]
    fn the_global_file_sits_in_the_xdg_config_dir_and_the_repo_file_at_the_root() {
        let home = global_path(env(&[("HOME", "/home/me")]), false);
        assert_eq!(home, Some("/home/me/.config/kika/config.toml".into()));
        let xdg = env(&[("HOME", "/home/me"), ("XDG_CONFIG_HOME", "/xdg")]);
        assert_eq!(
            global_path(xdg, false),
            Some("/xdg/kika/config.toml".into())
        );
        let relative = env(&[("HOME", "/home/me"), ("XDG_CONFIG_HOME", "rel")]);
        assert_eq!(global_path(relative, false), home);
        assert_eq!(global_path(env(&[]), false), None);

        let paths = ConfigPaths::new(home.clone(), Some(Path::new("/work/repo")));
        assert_eq!(
            paths,
            ConfigPaths {
                global: home.clone(),
                repo: Some("/work/repo/.kika.toml".into()),
            }
        );
        assert_eq!(ConfigPaths::new(home, None).repo, None);
    }

    #[test]
    #[cfg(windows)]
    fn on_windows_the_global_file_sits_in_appdata() {
        let appdata = global_path(env(&[("APPDATA", r"C:\Users\me\AppData")]), true);
        assert_eq!(
            appdata,
            Some(r"C:\Users\me\AppData\kika\config.toml".into())
        );
        assert_eq!(global_path(env(&[("APPDATA", "AppData")]), true), None);
        let unix_vars = env(&[("HOME", r"C:\Users\me"), ("XDG_CONFIG_HOME", r"C:\xdg")]);
        assert_eq!(global_path(unix_vars, true), None);

        let paths = ConfigPaths::new(appdata.clone(), Some(Path::new(r"C:\work\repo")));
        assert_eq!(
            paths,
            ConfigPaths {
                global: appdata,
                repo: Some(r"C:\work\repo\.kika.toml".into()),
            }
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
        let keymap = repo("[keys]\n\"files.stage\" = [\"y\"]\n").unwrap().0;
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

    #[test]
    fn a_repository_file_cannot_remap_the_range_question_but_can_the_list_keys() {
        for id in ["delete_range.yes", "delete_range.no", "delete_range.quit"] {
            let locked = repo(&format!("[keys]\n\"{id}\" = [\"Z\"]\n")).unwrap_err();
            assert_eq!(
                locked,
                format!("\"{id}\" can only be changed in the global config file")
            );
        }
        for scope in ["branches", "remotes", "tags"] {
            let text =
                format!("[keys]\n\"{scope}.filter\" = [\"F\"]\n\"{scope}.range\" = [\"Z\"]\n");
            let keymap = repo(&text).unwrap().0;
            assert_eq!(keys_of(&keymap, &format!("{scope}.filter")), [key("F")]);
        }
    }

    #[test]
    fn a_repository_file_may_move_the_command_log_filter_but_not_its_escape() {
        let keymap = repo("[keys]\n\"command_log.filter\" = [\"f\"]\n")
            .unwrap()
            .0;
        assert_eq!(keys_of(&keymap, "command_log.filter"), [key("f")]);
        let locked = repo("[keys]\n\"command_log.escape\" = [\"x\"]\n").unwrap_err();
        assert_eq!(
            locked,
            r#""command_log.escape" can only be changed in the global config file"#
        );
        assert!(global("[keys]\n\"command_log.escape\" = [\"x\"]\n").is_ok());
    }

    #[test]
    fn a_repository_file_may_move_the_worktrees_filter_but_not_its_escape() {
        let keymap = repo("[keys]\n\"worktrees.filter\" = [\"f\"]\n").unwrap().0;
        assert_eq!(keys_of(&keymap, "worktrees.filter"), [key("f")]);
        let locked = repo("[keys]\n\"worktrees.escape\" = [\"x\"]\n").unwrap_err();
        assert_eq!(
            locked,
            r#""worktrees.escape" can only be changed in the global config file"#
        );
        assert!(global("[keys]\n\"worktrees.escape\" = [\"x\"]\n").is_ok());
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
                diff_pager: None,
                diff_tool: None,
                forge_hosts: vec![],
                forge_gitlab_hosts: vec![],
                browser: None,
                commit_hooks_detached: false,
                emoji_zwj_terminals: vec![],
                clipboard_command: None,
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
                dropped_binding: false,
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

    /// Every fenced `toml` block in the user page, so an example there
    /// cannot go stale.
    fn documented_examples() -> Vec<String> {
        let page = include_str!("../docs/configuration.md");
        let mut blocks = Vec::new();
        let mut open: Option<String> = None;
        for line in page.lines() {
            match (&mut open, line.trim_end()) {
                (None, "```toml") => open = Some(String::new()),
                (Some(_), "```") => blocks.extend(open.take()),
                (Some(block), text) => {
                    block.push_str(text);
                    block.push('\n');
                }
                (None, _) => {}
            }
        }
        blocks
    }

    #[test]
    fn documented_examples_load() {
        let examples = documented_examples();
        assert!(examples.len() >= 2, "{examples:?}");
        for example in examples {
            if let Err(e) = global(&example) {
                panic!("docs/configuration.md example does not load: {e}\n{example}");
            }
        }
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

    const COPY_COMMAND: &str = r#"
[[commands]]
context = "files"
keys = ["ctrl-t"]
description = "Copy the file to…"
command = ["cp", "--", "{path}", "{prompt1}"]
prompts = ["Copy to"]
output = "none"
"#;

    #[test]
    fn a_global_command_loads_into_the_keymap() {
        use crate::features::custom_commands::{OutputMode, Part};
        let files = Files::new("commands");
        files.global(COPY_COMMAND);
        let loaded = files.load();
        assert_eq!(loaded.errors, []);
        let [command] = loaded.keymap.commands.as_slice() else {
            panic!("expected one command: {:?}", loaded.keymap.commands);
        };
        assert_eq!(command.scope, crate::keymap::KeyScope::Files);
        assert_eq!(command.keys, [key("ctrl-t")]);
        assert_eq!(command.description, "Copy the file to…");
        assert_eq!(command.argv[2], [Part::Path]);
        assert_eq!(command.prompts, ["Copy to"]);
        assert_eq!(command.output, OutputMode::None);
    }

    #[test]
    fn a_repository_file_cannot_define_commands_and_the_global_ones_stay() {
        let files = Files::new("repo-commands");
        files.global(COPY_COMMAND).repo(&format!(
            "[keys]\n\"files.commit\" = [\"C\"]\n{}",
            COPY_COMMAND.replace("ctrl-t", "ctrl-y")
        ));
        let loaded = files.load();
        assert_eq!(loaded.errors.len(), 1, "{:?}", loaded.errors);
        assert!(
            loaded.errors[0]
                .message
                .contains("unknown field `commands`"),
            "{:?}",
            loaded.errors
        );
        // Ignored as a whole: its `[keys]` did not apply either.
        assert_eq!(keys_of(&loaded.keymap, "files.commit"), [key("c")]);
        assert_eq!(loaded.keymap.commands.len(), 1);
        assert_eq!(loaded.keymap.commands[0].keys, [key("ctrl-t")]);
    }

    #[test]
    fn a_bad_command_drops_the_global_file() {
        let err = |text: &str| global(text).unwrap_err();
        assert_eq!(
            err(&COPY_COMMAND.replace("ctrl-t", "q")),
            r#"command 1: key "q" would hide "global.quit""#
        );
        assert_eq!(
            err(&COPY_COMMAND.replace("ctrl-t", "c")),
            r#"key "c" is bound to both "files.commit" and command 1"#
        );
        let shell = err(&COPY_COMMAND.replace(
            r#"["cp", "--", "{path}", "{prompt1}"]"#,
            r#"["sh", "-c", "cp {path} x"]"#,
        ));
        assert!(shell.starts_with("command 1: "), "{shell}");
        let typo = err(&COPY_COMMAND.replace("output", "shell = true\noutput"));
        assert!(typo.contains("unknown field `shell`"), "{typo}");
    }

    /// The global `[keys]` lines that move `files.stage` onto `ctrl-t`,
    /// `COPY_COMMAND`'s key.
    const STAGE_ON_CTRL_T: &str =
        "[keys]\n\"files.stage\" = [\"ctrl-t\"]\n\"files.commit\" = [\"C\"]\n";

    /// `STAGE_ON_CTRL_T` without moving `files.commit` off `c`.
    const ONLY_STAGE_ON_CTRL_T: &str = "[keys]\n\"files.stage\" = [\"ctrl-t\"]\n";

    const COPY_DROPPED: &str = r#"key "ctrl-t" is bound to both "files.stage" and command 1 ("Copy the file to…"); command 1 was dropped"#;

    #[test]
    fn a_global_command_on_a_key_its_own_file_moved_a_binding_onto_is_dropped_alone() {
        let files = Files::new("global-onto-command");
        files.global(&format!(
            "{STAGE_ON_CTRL_T}[watch]\nfetch = 300\n{COPY_COMMAND}"
        ));
        let loaded = files.load();
        assert_eq!(
            loaded.errors,
            [ConfigError {
                path: files.dir.join("config.toml").display().to_string(),
                message: COPY_DROPPED.into(),
                dropped_binding: true,
            }]
        );
        assert_eq!(keys_of(&loaded.keymap, "files.stage"), [key("ctrl-t")]);
        assert_eq!(keys_of(&loaded.keymap, "files.commit"), [key("C")]);
        assert_eq!(loaded.watch.fetch, Some(Duration::from_secs(300)));
        assert_eq!(loaded.keymap.commands, []);
    }

    #[test]
    fn a_global_command_on_a_free_key_survives_its_neighbours_drop() {
        let files = Files::new("global-onto-command-free");
        files.global(&format!(
            "{STAGE_ON_CTRL_T}{COPY_COMMAND}{}",
            COPY_COMMAND.replace("ctrl-t", "ctrl-y")
        ));
        let loaded = files.load();
        let messages: Vec<&str> = loaded.errors.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(messages, [COPY_DROPPED]);
        assert_eq!(loaded.keymap.commands.len(), 1);
        assert_eq!(loaded.keymap.commands[0].keys, [key("ctrl-y")]);
        // It still runs, and the help menu lists it.
        use crate::keymap::{KeyPress, KeyScope, Resolution, available_commands, resolve};
        use crossterm::event::{KeyCode, KeyModifiers};
        let state = crate::model::AppState::new(crate::model::Context::Workspace);
        let ctrl_y = KeyPress::new(KeyCode::Char('y'), KeyModifiers::CONTROL);
        assert!(matches!(
            resolve(registry(), &loaded.keymap, KeyScope::Files, &state, ctrl_y),
            Resolution::Command(0)
        ));
        let listed = available_commands(&loaded.keymap, KeyScope::Files);
        assert_eq!(listed, [&loaded.keymap.commands[0]]);
    }

    #[test]
    fn a_repository_file_applies_over_a_global_file_with_a_dropped_command() {
        let files = Files::new("global-onto-command-repo");
        files
            .global(&format!(
                "{STAGE_ON_CTRL_T}{COPY_COMMAND}{}",
                COPY_COMMAND.replace("ctrl-t", "ctrl-y")
            ))
            .repo("[keys]\n\"files.commit\" = [\"K\"]\n");
        let loaded = files.load();
        let messages: Vec<&str> = loaded.errors.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(messages, [COPY_DROPPED]);
        assert_eq!(keys_of(&loaded.keymap, "files.stage"), [key("ctrl-t")]);
        assert_eq!(keys_of(&loaded.keymap, "files.commit"), [key("K")]);
        assert_eq!(loaded.keymap.commands.len(), 1);
        assert_eq!(loaded.keymap.commands[0].keys, [key("ctrl-y")]);
    }

    #[test]
    fn every_global_command_on_a_moved_key_is_dropped_with_its_own_message() {
        let files = Files::new("global-onto-commands");
        files.global(&format!("{STAGE_ON_CTRL_T}{COPY_COMMAND}{COPY_COMMAND}"));
        let loaded = files.load();
        let messages: Vec<&str> = loaded.errors.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                COPY_DROPPED,
                &COPY_DROPPED.replace("command 1", "command 2"),
            ]
        );
        assert!(loaded.errors.iter().all(|e| e.dropped_binding));
        assert_eq!(keys_of(&loaded.keymap, "files.stage"), [key("ctrl-t")]);
        assert_eq!(loaded.keymap.commands, []);
    }

    #[test]
    fn a_later_clash_after_a_drop_names_the_command_by_its_place_in_the_file() {
        let files = Files::new("global-onto-command-numbers");
        files.global(&format!(
            "{ONLY_STAGE_ON_CTRL_T}{COPY_COMMAND}{}{}",
            COPY_COMMAND.replace("ctrl-t", "ctrl-y"),
            COPY_COMMAND.replace("ctrl-t", "c")
        ));
        let loaded = files.load();
        assert_eq!(
            loaded.errors,
            [ConfigError {
                path: files.dir.join("config.toml").display().to_string(),
                message: r#"key "c" is bound to both "files.commit" and command 3"#.into(),
                dropped_binding: false,
            }]
        );
        assert_eq!(loaded.keymap, Keymap::default());
    }

    #[test]
    fn a_command_on_a_moved_key_and_a_default_key_is_dropped_and_the_file_loads() {
        let files = Files::new("global-onto-command-and-default");
        files.global(&format!(
            "{ONLY_STAGE_ON_CTRL_T}[watch]\nfetch = 300\n{}{}",
            COPY_COMMAND.replace(r#"["ctrl-t"]"#, r#"["ctrl-t", "c"]"#),
            COPY_COMMAND.replace("ctrl-t", "ctrl-y")
        ));
        let loaded = files.load();
        assert_eq!(
            loaded.errors,
            [ConfigError {
                path: files.dir.join("config.toml").display().to_string(),
                message: concat!(
                    r#"key "ctrl-t" is bound to both "files.stage" and command 1, "#,
                    r#"and key "c" is bound to both "files.commit" and command 1 "#,
                    r#"("Copy the file to…"); command 1 was dropped"#
                )
                .into(),
                dropped_binding: true,
            }]
        );
        assert_eq!(keys_of(&loaded.keymap, "files.stage"), [key("ctrl-t")]);
        assert_eq!(keys_of(&loaded.keymap, "files.commit"), [key("c")]);
        assert_eq!(loaded.watch.fetch, Some(Duration::from_secs(300)));
        assert_eq!(loaded.keymap.commands.len(), 1);
        assert_eq!(loaded.keymap.commands[0].keys, [key("ctrl-y")]);
    }

    #[test]
    fn a_droppable_command_does_not_save_a_file_with_another_error() {
        let err = |extra_keys: &str, extra: &str| {
            global(&format!(
                "{ONLY_STAGE_ON_CTRL_T}{extra_keys}{extra}{COPY_COMMAND}"
            ))
            .unwrap_err()
        };
        assert_eq!(
            err("", &COPY_COMMAND.replace("ctrl-t", "c")),
            r#"key "c" is bound to both "files.commit" and command 1"#
        );
        assert_eq!(
            err("", &COPY_COMMAND.replace("ctrl-t", "q")),
            r#"command 1: key "q" would hide "global.quit""#
        );
        assert_eq!(
            err("\"global.quit\" = []\n", ""),
            r#""global.quit" needs at least one key"#
        );
        assert_eq!(
            err("\"files.nope\" = [\"x\"]\n", ""),
            r#"unknown binding "files.nope""#
        );
    }

    #[test]
    fn a_repository_file_cannot_move_a_binding_onto_a_command_key() {
        let files = Files::new("repo-onto-command");
        files
            .global(COPY_COMMAND)
            .repo("[keys]\n\"files.stage\" = [\"ctrl-t\"]\n\"files.commit\" = [\"C\"]\n");
        let loaded = files.load();
        assert_eq!(
            loaded.errors,
            [ConfigError {
                path: files.dir.join(".kika.toml").display().to_string(),
                message: r#"key "ctrl-t" is bound to both "files.stage" and command 1; the change to "files.stage" was dropped"#.into(),
                dropped_binding: true,
            }]
        );
        assert_eq!(keys_of(&loaded.keymap, "files.stage"), [key("space")]);
        assert_eq!(keys_of(&loaded.keymap, "files.commit"), [key("C")]);
        assert_eq!(loaded.keymap.commands.len(), 1);
        assert_eq!(loaded.keymap.commands[0].keys, [key("ctrl-t")]);
    }

    #[test]
    fn a_clashing_global_entry_is_dropped_and_the_rest_of_the_file_applies() {
        let files = Files::new("global-clash");
        files.global(
            "[keys]\n\"files.stage\" = [\"s\"]\n\"files.commit\" = [\"C\"]\n[watch]\nfetch = 300\n",
        );
        let loaded = files.load();
        assert_eq!(keys_of(&loaded.keymap, "files.stage"), [key("space")]);
        assert_eq!(keys_of(&loaded.keymap, "files.stash"), [key("s")]);
        assert_eq!(keys_of(&loaded.keymap, "files.commit"), [key("C")]);
        assert_eq!(loaded.watch.fetch, Some(Duration::from_secs(300)));
        let [error] = loaded.errors.as_slice() else {
            panic!("expected one error, got {:?}", loaded.errors);
        };
        assert!(error.dropped_binding);
        assert_eq!(
            error.path,
            files.dir.join("config.toml").display().to_string()
        );
        for name in [r#""s""#, r#""files.stash""#, r#""files.stage""#] {
            assert!(error.message.contains(name), "{name}: {}", error.message);
        }
    }

    #[test]
    fn a_clashing_repository_entry_falls_back_to_the_global_keys() {
        let files = Files::new("repo-clash");
        files
            .global("[keys]\n\"files.stage\" = [\"x\"]\n")
            .repo("[keys]\n\"files.stage\" = [\"s\"]\n\"files.commit\" = [\"C\"]\n");
        let loaded = files.load();
        assert_eq!(keys_of(&loaded.keymap, "files.stage"), [key("x")]);
        assert_eq!(keys_of(&loaded.keymap, "files.stash"), [key("s")]);
        assert_eq!(keys_of(&loaded.keymap, "files.commit"), [key("C")]);
        let [error] = loaded.errors.as_slice() else {
            panic!("expected one error, got {:?}", loaded.errors);
        };
        assert!(error.dropped_binding);
        assert_eq!(
            error.path,
            files.dir.join(".kika.toml").display().to_string()
        );
        assert!(
            error
                .message
                .ends_with(r#"the change to "files.stage" was dropped"#)
        );
    }

    #[test]
    fn a_repository_file_with_a_clash_and_a_locked_id_is_still_ignored_whole() {
        let files = Files::new("repo-clash-locked");
        let path = files.dir.join(".kika.toml").display().to_string();
        let only_error = |files: &Files| {
            let loaded = files.load();
            assert_eq!(loaded.keymap, Keymap::default());
            match loaded.errors.as_slice() {
                [error] => {
                    assert_eq!(error.path, path);
                    assert!(!error.dropped_binding, "{error:?}");
                    error.message.clone()
                }
                other => panic!("expected one error, got {other:?}"),
            }
        };
        let clash = "[keys]\n\"files.stage\" = [\"s\"]\n\"files.commit\" = [\"C\"]\n";
        files.repo(&format!("{clash}\"files.open\" = [\"x\"]\n"));
        assert_eq!(
            only_error(&files),
            r#""files.open" can only be changed in the global config file"#
        );
        files.repo(&format!("{clash}\"files.blame\" = [\"q\"]\n"));
        assert_eq!(
            only_error(&files),
            r#""files.blame": key "q" would hide "global.quit""#
        );
        files.repo(&format!("{clash}\"global.quit\" = []\n"));
        assert_eq!(
            only_error(&files),
            r#""global.quit" can only be changed in the global config file"#
        );
    }

    #[test]
    fn the_global_file_names_a_diff_pager_and_a_bad_one_is_refused() {
        let pager = |text: &str| {
            apply_global(text, &Keymap::default(), DEFAULT_WATCH).map(|g| g.diff_pager)
        };
        assert_eq!(
            pager("[diff]\npager = \"delta --paging=never\"\n"),
            Ok(Some("delta --paging=never".into()))
        );
        assert_eq!(pager(""), Ok(None));
        assert_eq!(
            pager("[diff]\npager = \"delta | less\"\n"),
            Err("diff.pager: The command needs a shell, which is never used".into())
        );
        assert_eq!(
            pager("[diff]\npager = \"./delta\"\n"),
            Err("diff.pager: The program must be a bare name or an absolute path".into())
        );
        let typo = pager("[diff]\npagerr = \"delta\"\n").unwrap_err();
        assert!(typo.contains("unknown field `pagerr`"), "{typo}");
    }

    #[test]
    fn the_global_file_names_the_forge_hosts_and_a_bad_one_drops_the_file() {
        let hosts = |text: &str| {
            apply_global(text, &Keymap::default(), DEFAULT_WATCH).map(|g| g.forge_hosts)
        };
        assert_eq!(
            hosts("[forge]\ngithub = [\"github.com\"]\n"),
            Ok(vec![ForgeHost::parse("github.com").unwrap()])
        );
        assert_eq!(hosts(""), Ok(vec![]));
        assert_eq!(hosts("[forge]\ngithub = []\n"), Ok(vec![]));
        for bad in [
            "GitHub.com",
            "github.com:443",
            "user@github.com",
            "https://github.com",
            "*.github.com",
            "github.com.",
            "gіthub.com",
        ] {
            let err =
                hosts(&format!("[forge]\ngithub = [\"github.com\", \"{bad}\"]\n")).unwrap_err();
            assert!(err.starts_with("forge.github: "), "{bad}: {err}");
        }
        let typo = hosts("[forge]\ngitlub = [\"gitlab.com\"]\n").unwrap_err();
        assert!(typo.contains("unknown field `gitlub`"), "{typo}");

        let files = Files::new("forge-bad");
        files.global("[keys]\n\"files.stage\" = [\"x\"]\n[forge]\ngithub = [\"GitHub.com\"]\n");
        let loaded = files.load();
        assert_eq!(loaded.forge_hosts, []);
        assert_eq!(loaded.keymap, Keymap::default());
        assert_eq!(loaded.errors.len(), 1);
    }

    #[test]
    fn the_global_file_names_the_gitlab_hosts_and_the_browser() {
        let global = |text: &str| apply_global(text, &Keymap::default(), DEFAULT_WATCH);
        let settings = global(
            "[forge]\ngitlab = [\"gitlab.com\", \"git.example.org\"]\n\
             browser = [\"/usr/bin/open\", \"-a\", \"Safari Browser\"]\n",
        )
        .unwrap();
        assert_eq!(
            settings.forge_gitlab_hosts,
            [
                ForgeHost::parse("gitlab.com").unwrap(),
                ForgeHost::parse("git.example.org").unwrap()
            ]
        );
        // GitLab hosts never turn the pull-request load on.
        assert_eq!(settings.forge_hosts, []);
        assert_eq!(
            settings.browser,
            Some(vec![
                OsString::from("/usr/bin/open"),
                "-a".into(),
                "Safari Browser".into()
            ])
        );
        let bare = global("[forge]\nbrowser = [\"xdg-open\"]\n").unwrap();
        assert_eq!(bare.browser, Some(vec![OsString::from("xdg-open")]));
        assert_eq!(global("").unwrap().browser, None);

        let err = |text: &str| global(text).unwrap_err();
        assert_eq!(
            err("[forge]\nbrowser = [\"bin/open\"]\n"),
            "forge.browser: The program must be a bare name or an absolute path"
        );
        assert_eq!(
            err("[forge]\nbrowser = [\"./open\"]\n"),
            "forge.browser: The program must be a bare name or an absolute path"
        );
        assert_eq!(
            err("[forge]\nbrowser = []\n"),
            "forge.browser: The command is empty"
        );
        assert_eq!(
            err("[forge]\nbrowser = [\"open\", \"a\\u001bb\"]\n"),
            "forge.browser: The command contains a control character"
        );
        assert!(err("[forge]\nbrowser = \"open\"\n").contains("invalid type"));
        let bad_host = err("[forge]\ngitlab = [\"GitLab.com\"]\n");
        assert!(bad_host.starts_with("forge.gitlab: "), "{bad_host}");

        // A bad browser drops the whole file, as any error does.
        let files = Files::new("forge-browser-bad");
        files.global("[forge]\ngithub = [\"github.com\"]\nbrowser = [\"x/open\"]\n");
        let loaded = files.load();
        assert_eq!((loaded.forge_hosts, loaded.browser), (vec![], None));
        assert_eq!(loaded.errors.len(), 1);
    }

    #[test]
    fn a_gitlab_ask_host_turns_the_gitlab_load_on() {
        let global = |text: &str| apply_global(text, &Keymap::default(), DEFAULT_WATCH);
        let lab = || ForgeHost::parse_gitlab("gitlab.example.com").unwrap();
        let on = global(
            "[forge]\ngitlab = [\"gitlab.example.com\"]\n\
             gitlab_ask = [\"gitlab.example.com\"]\n",
        )
        .unwrap();
        let [host] = on.forge_hosts.try_into().unwrap();
        assert_eq!(host, lab());
        assert_eq!(host.kind(), lab().kind());
        assert_ne!(host.kind(), ForgeHost::parse("github.com").unwrap().kind());
        assert_eq!(
            on.forge_gitlab_hosts,
            [ForgeHost::parse("gitlab.example.com").unwrap()]
        );
        // `gitlab` alone only builds URLs.
        let off = global("[forge]\ngitlab = [\"gitlab.example.com\"]\n").unwrap();
        assert_eq!(off.forge_hosts, []);
        // GitHub's come first.
        let both = global(
            "[forge]\ngithub = [\"github.com\"]\ngitlab = [\"gitlab.example.com\"]\n\
             gitlab_ask = [\"gitlab.example.com\"]\n",
        )
        .unwrap();
        assert_eq!(
            both.forge_hosts,
            [ForgeHost::parse("github.com").unwrap(), lab()]
        );
    }

    #[test]
    fn a_bad_gitlab_ask_host_drops_the_global_file() {
        let err = |forge: &str| {
            apply_global(
                &format!("[forge]\n{forge}\n"),
                &Keymap::default(),
                DEFAULT_WATCH,
            )
            .map(|_| ())
            .unwrap_err()
        };
        assert_eq!(
            err("gitlab = [\"gitlab.com\"]\ngitlab_ask = [\"git.example.org\"]"),
            "forge.gitlab_ask: \"git.example.org\" is not one of the forge.gitlab hosts"
        );
        assert_eq!(
            err(
                "github = [\"git.example.org\"]\ngitlab = [\"git.example.org\"]\n\
                 gitlab_ask = [\"git.example.org\"]"
            ),
            "forge.gitlab_ask: \"git.example.org\" is also a forge.github host"
        );
        let malformed = err("gitlab = [\"gitlab.com\"]\ngitlab_ask = [\"GitLab.com\"]");
        assert!(malformed.starts_with("forge.gitlab_ask: "), "{malformed}");

        let files = Files::new("forge-gitlab-ask-bad");
        files.global(
            "[keys]\n\"files.stage\" = [\"x\"]\n[forge]\ngithub = [\"github.com\"]\n\
             gitlab_ask = [\"gitlab.com\"]\n",
        );
        let loaded = files.load();
        assert_eq!(loaded.forge_hosts, []);
        assert_eq!(loaded.keymap, Keymap::default());
        let [error] = loaded.errors.try_into().unwrap();
        assert!(error.message.starts_with("forge.gitlab_ask: "), "{error}");
    }

    #[test]
    fn an_allow_list_narrows_a_gitlab_ask_host_and_not_a_url_only_one() {
        let global = |text: &str| apply_global(text, &Keymap::default(), DEFAULT_WATCH);
        let [host] = global(
            "[forge]\ngitlab = [\"gitlab.example.com\"]\n\
             gitlab_ask = [\"gitlab.example.com\"]\n\
             [forge.allow]\n\"gitlab.example.com\" = [\"acme\"]\n",
        )
        .unwrap()
        .forge_hosts
        .try_into()
        .unwrap();
        assert_eq!(
            host,
            ForgeHost::parse_gitlab("gitlab.example.com")
                .unwrap()
                .with_allow(vec![AllowRule::parse("acme").unwrap()])
        );
        let repo = |owner: &str| ForgeRepo {
            host: "gitlab.example.com".into(),
            owner: owner.into(),
            name: "widgets".into(),
        };
        assert!(host.allows(&repo("acme")));
        assert!(!host.allows(&repo("other")));
        let only_urls = global(
            "[forge]\ngitlab = [\"gitlab.example.com\"]\n\
             [forge.allow]\n\"gitlab.example.com\" = [\"acme\"]\n",
        )
        .unwrap_err();
        assert_eq!(
            only_urls,
            "forge.allow: \"gitlab.example.com\" is not one of the forge.github or \
             forge.gitlab_ask hosts"
        );
    }

    #[test]
    fn only_the_global_file_turns_the_forge_on() {
        let refused = repo("[forge]\ngithub = [\"github.com\"]\n").unwrap_err();
        assert!(refused.contains("unknown field `forge`"), "{refused}");

        let files = Files::new("forge");
        files.repo("[forge]\ngithub = [\"github.com\"]\n");
        let loaded = files.load();
        assert_eq!(loaded.forge_hosts, []);
        assert_eq!(loaded.errors.len(), 1);

        // Nor can it name a program to open URLs with, or GitLab hosts.
        for text in [
            "[forge]\nbrowser = [\"open\"]\n",
            "[forge]\ngitlab = [\"gitlab.com\"]\n",
            "[forge]\ngitlab = [\"gitlab.com\"]\ngitlab_ask = [\"gitlab.com\"]\n",
        ] {
            let refused = repo(text).unwrap_err();
            assert!(refused.contains("unknown field `forge`"), "{refused}");
            files.repo(text);
            let loaded = files.load();
            assert_eq!(loaded.browser, None);
            assert_eq!(loaded.forge_hosts, []);
            assert_eq!(loaded.forge_gitlab_hosts, []);
            assert_eq!(loaded.errors.len(), 1);
        }

        files
            .global("[forge]\ngithub = [\"github.com\", \"github.example.com\"]\n")
            .repo("");
        let loaded = files.load();
        assert_eq!(loaded.errors, []);
        assert_eq!(
            loaded.forge_hosts,
            [
                ForgeHost::parse("github.com").unwrap(),
                ForgeHost::parse("github.example.com").unwrap()
            ]
        );
    }

    #[test]
    fn the_global_file_narrows_a_github_host_to_listed_owners() {
        let hosts = |text: &str| {
            apply_global(text, &Keymap::default(), DEFAULT_WATCH).map(|g| g.forge_hosts)
        };
        let github = || ForgeHost::parse("github.com").unwrap();
        let rule = |text: &str| AllowRule::parse(text).unwrap();
        // Unset: today's per-host behaviour.
        let [unset] = hosts("[forge]\ngithub = [\"github.com\"]\n")
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(unset, github());
        assert!(unset.allows(&ForgeRepo {
            host: "github.com".into(),
            owner: "anyone".into(),
            name: "anything".into(),
        }));
        // A host listed twice is narrowed in both places.
        let twice = hosts(
            "[forge]\ngithub = [\"github.com\", \"github.com\"]\n\
             [forge.allow]\n\"github.com\" = [\"acme\"]\n",
        )
        .unwrap();
        assert_eq!(twice, vec![github().with_allow(vec![rule("acme")]); 2]);
        assert_eq!(
            hosts(
                "[forge]\ngithub = [\"github.com\", \"github.example.com\"]\n\
                 [forge.allow]\n\"github.com\" = [\"acme\", \"pererikbergman/kika\"]\n\
                 \"github.example.com\" = []\n"
            ),
            Ok(vec![
                github().with_allow(vec![rule("acme"), rule("pererikbergman/kika")]),
                ForgeHost::parse("github.example.com")
                    .unwrap()
                    .with_allow(vec![]),
            ])
        );
    }

    #[test]
    fn a_bad_allow_entry_or_host_drops_the_global_file() {
        let err = |allow: &str| {
            apply_global(
                &format!(
                    "[forge]\ngithub = [\"github.com\"]\ngitlab = [\"gitlab.com\"]\n\
                     [forge.allow]\n{allow}\n"
                ),
                &Keymap::default(),
                DEFAULT_WATCH,
            )
            .map(|_| ())
            .unwrap_err()
        };
        for entry in [
            "*",
            "acme/*",
            "a/b/c",
            "",
            "acme/",
            "/kika",
            "acme/kika.git",
        ] {
            let message = err(&format!("\"github.com\" = [\"{entry}\"]"));
            assert!(message.starts_with("forge.allow: "), "{entry:?}: {message}");
        }
        for key in ["github.example.com", "gitlab.com", "GitHub.com"] {
            let message = err(&format!("\"{key}\" = [\"acme\"]"));
            assert!(message.starts_with("forge.allow: "), "{key:?}: {message}");
            assert!(message.contains(&format!("{key:?}")), "{message}");
        }
        // Control bytes in user text are escaped in the message.
        let message = err("\"github.com\" = [\"a\\u001bb\"]");
        assert!(message.contains("\\u{1b}"), "{message}");

        let files = Files::new("forge-allow-bad");
        files.global(
            "[keys]\n\"files.stage\" = [\"x\"]\n[forge]\ngithub = [\"github.com\"]\n\
             [forge.allow]\n\"github.com\" = [\"acme/*\"]\n",
        );
        let loaded = files.load();
        assert_eq!(loaded.forge_hosts, []);
        assert_eq!(loaded.keymap, Keymap::default());
        let [error] = loaded.errors.try_into().unwrap();
        assert!(error.message.starts_with("forge.allow: "), "{error}");
        assert!(!error.dropped_binding);
    }

    #[test]
    fn only_the_global_file_holds_an_allow_list() {
        let files = Files::new("forge-allow-repo");
        files.global(
            "[keys]\n\"files.stage\" = [\"x\"]\n[forge]\ngithub = [\"github.com\"]\n\
             [forge.allow]\n\"github.com\" = [\"acme\"]\n",
        );
        let want = vec![
            ForgeHost::parse("github.com")
                .unwrap()
                .with_allow(vec![AllowRule::parse("acme").unwrap()]),
        ];
        for text in [
            "[forge.allow]\n\"github.com\" = [\"evil\"]\n",
            "[keys]\n\"files.stage\" = [\"y\"]\n[forge.allow]\n\"github.com\" = []\n",
        ] {
            let refused = repo(text).unwrap_err();
            assert!(refused.contains("unknown field `forge`"), "{refused}");
            files.repo(text);
            let loaded = files.load();
            assert_eq!(loaded.forge_hosts, want);
            // Its keys do not apply either: the file is ignored whole.
            assert_eq!(keys_of(&loaded.keymap, "files.stage"), [key("x")]);
            let [error] = loaded.errors.try_into().unwrap();
            assert!(!error.dropped_binding);
        }
    }

    #[test]
    fn only_the_global_file_names_a_diff_pager() {
        let refused = repo("[diff]\npager = \"delta\"\n").unwrap_err();
        assert!(refused.contains("unknown field `diff`"), "{refused}");

        let files = Files::new("pager");
        files
            .global("[diff]\npager = \"delta\"\n")
            .repo("[diff]\npager = \"evil\"\n");
        let loaded = files.load();
        assert_eq!(loaded.diff_pager, Some("delta".into()));
        assert_eq!(loaded.errors.len(), 1);
        assert!(loaded.errors[0].message.contains("unknown field `diff`"));
    }

    #[test]
    fn the_global_file_keeps_hook_only_commits_detached() {
        let files = Files::new("commit");
        assert!(!files.load().commit_hooks_detached);
        files.global("");
        assert!(!files.load().commit_hooks_detached);
        files.global("[commit]\nhooks_detached = true\n");
        let loaded = files.load();
        assert_eq!(loaded.errors, []);
        assert!(loaded.commit_hooks_detached);

        let typo = global("[commit]\nbogus = 1\n").unwrap_err();
        assert!(typo.contains("unknown field `bogus`"), "{typo}");
        files.global("[commit]\nhooks_detached = true\nbogus = 1\n");
        assert!(
            !files.load().commit_hooks_detached,
            "a broken file is ignored"
        );
    }

    #[test]
    fn only_the_global_file_keeps_hook_only_commits_detached() {
        let files = Files::new("repo-commit");
        files.global("").repo("[commit]\nhooks_detached = true\n");
        let loaded = files.load();
        assert!(!loaded.commit_hooks_detached);
        let path = files.dir.join(".kika.toml").display().to_string();
        assert_eq!(loaded.errors.len(), 1, "{:?}", loaded.errors);
        assert_eq!(loaded.errors[0].path, path);
        assert!(loaded.errors[0].message.contains("unknown field `commit`"));

        files.global("[commit]\nhooks_detached = true\n");
        let loaded = files.load();
        assert!(loaded.commit_hooks_detached);
        assert_eq!(loaded.errors.len(), 1, "{:?}", loaded.errors);
        assert_eq!(loaded.errors[0].path, path);
    }

    #[test]
    fn the_global_file_names_a_diff_tool_and_a_bad_one_is_refused() {
        let tool =
            |text: &str| apply_global(text, &Keymap::default(), DEFAULT_WATCH).map(|g| g.diff_tool);
        assert_eq!(
            tool("[diff]\ntool = \"difft --color=always\"\n"),
            Ok(Some("difft --color=always".into()))
        );
        assert_eq!(tool(""), Ok(None));
        assert_eq!(
            tool("[diff]\ntool = \"difft | less\"\n"),
            Err("diff.tool: The command needs a shell, which is never used".into())
        );
        let files = Files::new("tool");
        files.global("[diff]\ntool = \"difft --color=always\"\n");
        assert_eq!(files.load().diff_tool, Some("difft --color=always".into()));
        files.global("[diff]\ntool = \"difft | less\"\n");
        let loaded = files.load();
        assert_eq!(loaded.diff_tool, None);
        assert_eq!(loaded.errors.len(), 1);
    }

    #[test]
    fn only_the_global_file_names_a_diff_tool() {
        let files = Files::new("repo-tool");
        files
            .global("[diff]\ntool = \"difft\"\n")
            .repo("[diff]\ntool = \"x\"\n");
        let loaded = files.load();
        assert_eq!(loaded.diff_tool, Some("difft".into()));
        assert_eq!(loaded.errors.len(), 1);
        assert!(loaded.errors[0].message.contains("unknown field `diff`"));

        files.global("").repo("[diff]\ntool = \"x\"\n");
        let loaded = files.load();
        assert_eq!(loaded.diff_tool, None);
        assert_eq!(loaded.errors.len(), 1);
    }

    #[test]
    fn the_global_file_lists_the_terminals_that_cluster_emoji() {
        let files = Files::new("display");
        files.global("[display]\nemoji_zwj_terminals = [\"WezTerm\", \"ghostty\"]\n");
        let loaded = files.load();
        assert_eq!(loaded.errors, []);
        assert_eq!(loaded.emoji_zwj_terminals, ["WezTerm", "ghostty"]);

        files.global("[keys]\n");
        assert!(files.load().emoji_zwj_terminals.is_empty());
        files.global("[display]\n");
        assert!(files.load().emoji_zwj_terminals.is_empty());
    }

    #[test]
    fn an_empty_terminal_name_or_an_unknown_display_key_is_refused() {
        let terminals = |text: &str| {
            apply_global(text, &Keymap::default(), DEFAULT_WATCH).map(|g| g.emoji_zwj_terminals)
        };
        assert_eq!(
            terminals("[display]\nemoji_zwj_terminals = [\"WezTerm\", \"\"]\n"),
            Err("display.emoji_zwj_terminals: a terminal name cannot be empty".into())
        );
        let typo = terminals("[display]\nemoji_zwj_terminal = [\"WezTerm\"]\n").unwrap_err();
        assert!(
            typo.contains("unknown field `emoji_zwj_terminal`"),
            "{typo}"
        );
        let wrong = terminals("[display]\nemoji_zwj_terminals = \"WezTerm\"\n").unwrap_err();
        assert!(wrong.starts_with("line 2"), "{wrong}");
    }

    #[test]
    fn only_the_global_file_lists_emoji_terminals() {
        let refused = repo("[display]\nemoji_zwj_terminals = [\"x\"]\n").unwrap_err();
        assert!(refused.contains("unknown field `display`"), "{refused}");

        let files = Files::new("repo-display");
        files
            .global("[display]\nemoji_zwj_terminals = [\"WezTerm\"]\n")
            .repo("[keys]\n\"files.commit\" = [\"C\"]\n[display]\nemoji_zwj_terminals = [\"x\"]\n");
        let loaded = files.load();
        assert_eq!(loaded.emoji_zwj_terminals, ["WezTerm"]);
        assert_eq!(loaded.errors.len(), 1, "{:?}", loaded.errors);
        assert!(loaded.errors[0].message.contains("unknown field `display`"));
        // Ignored as a whole: its `[keys]` did not apply either.
        assert_eq!(keys_of(&loaded.keymap, "files.commit"), [key("c")]);
    }

    #[test]
    fn the_running_terminal_keeps_emoji_joiners_only_when_listed() {
        let listed = ["WezTerm".to_string(), "xterm-kitty".to_string()];
        use EmojiZwj::{Escape, Keep};
        for (program, term, expected) in [
            // TERM_PROGRAM decides when set.
            (Some("WezTerm"), None, Keep),
            (Some("WezTerm"), Some("xterm-256color"), Keep),
            (Some("tmux"), Some("xterm-kitty"), Escape),
            // TERM only when TERM_PROGRAM is unset or empty.
            (None, Some("xterm-kitty"), Keep),
            (Some(""), Some("xterm-kitty"), Keep),
            // Exact and case-sensitive.
            (Some("wezterm"), None, Escape),
            (Some("WezTerm2"), None, Escape),
            (None, Some("xterm"), Escape),
            (None, None, Escape),
            (Some(""), Some(""), Escape),
        ] {
            assert_eq!(
                emoji_zwj(&listed, program, term),
                expected,
                "{program:?} {term:?}"
            );
        }
        assert_eq!(emoji_zwj(&[], Some("WezTerm"), Some("xterm-kitty")), Escape);
    }

    #[test]
    fn the_global_file_names_a_clipboard_command() {
        let global = |text: &str| apply_global(text, &Keymap::default(), DEFAULT_WATCH);
        assert_eq!(global("").unwrap().clipboard_command, None);
        let settings = global("[clipboard]\ncommand = [\"/usr/bin/pbcopy\"]\n").unwrap();
        assert_eq!(
            settings.clipboard_command,
            Some(vec![OsString::from("/usr/bin/pbcopy")])
        );
        let xclip =
            global("[clipboard]\ncommand = [\"xclip\", \"-selection\", \"clipboard\"]\n").unwrap();
        assert_eq!(
            xclip.clipboard_command,
            Some(vec![
                OsString::from("xclip"),
                "-selection".into(),
                "clipboard".into()
            ])
        );

        let err = |text: &str| global(text).unwrap_err();
        assert_eq!(
            err("[clipboard]\ncommand = []\n"),
            "clipboard.command: The command is empty"
        );
        assert_eq!(
            err("[clipboard]\ncommand = [\"bin/x\"]\n"),
            "clipboard.command: The program must be a bare name or an absolute path"
        );
        assert_eq!(
            err("[clipboard]\ncommand = [\"x\", \"a\\u001bb\"]\n"),
            "clipboard.command: The command contains a control character"
        );
        assert!(err("[clipboard]\ncommand = \"pbcopy\"\n").contains("invalid type"));
        let typo = err("[clipboard]\ncomand = [\"pbcopy\"]\n");
        assert!(typo.contains("unknown field `comand`"), "{typo}");

        // A bad command drops the whole file, as any error does.
        let files = Files::new("clipboard-bad");
        files.global("[keys]\n\"files.stage\" = [\"x\"]\n[clipboard]\ncommand = [\"x/copy\"]\n");
        let loaded = files.load();
        assert_eq!(loaded.clipboard_command, None);
        assert_eq!(loaded.keymap, Keymap::default());
        assert_eq!(loaded.errors.len(), 1);
        assert!(loaded.errors[0].message.starts_with("clipboard.command: "));

        files.global("[clipboard]\ncommand = [\"/usr/bin/pbcopy\"]\n");
        let loaded = files.load();
        assert_eq!(loaded.errors, []);
        assert_eq!(
            loaded.clipboard_command,
            Some(vec![OsString::from("/usr/bin/pbcopy")])
        );
    }

    #[test]
    fn only_the_global_file_names_a_clipboard_command() {
        let refused = repo("[clipboard]\ncommand = [\"x\"]\n").unwrap_err();
        assert!(refused.contains("unknown field `clipboard`"), "{refused}");

        let files = Files::new("repo-clipboard");
        files
            .global("[keys]\n\"files.stage\" = [\"x\"]\n")
            .repo("[keys]\n\"files.commit\" = [\"C\"]\n[clipboard]\ncommand = [\"x\"]\n");
        let loaded = files.load();
        assert_eq!(loaded.clipboard_command, None);
        assert_eq!(loaded.errors.len(), 1, "{:?}", loaded.errors);
        assert!(
            loaded.errors[0]
                .message
                .contains("unknown field `clipboard`")
        );
        // The global keys still apply; the repository file's do not.
        assert_eq!(keys_of(&loaded.keymap, "files.stage"), [key("x")]);
        assert_eq!(keys_of(&loaded.keymap, "files.commit"), [key("c")]);
    }
}
