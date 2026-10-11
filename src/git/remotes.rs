//! Configured remotes and their remote-tracking branches, for the
//! Branches panel's Remotes tab, and the checks a remote name or URL the
//! user typed must pass before it reaches git. Reads only: nothing here
//! reaches the network.

use std::ffi::OsString;

use super::branches::run;
use super::diff::terminal_safe;
use super::remote::redact_userinfo;
use super::runner::{GitCommand, GitRunner};

/// A remote's name as raw bytes, so a name that is not UTF-8 reaches argv
/// unchanged. Equality is on the bytes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RemoteName(pub Vec<u8>);

impl RemoteName {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(bytes.to_vec())
    }

    /// For drawing only: lossy, with control characters replaced.
    pub fn display(&self) -> String {
        terminal_safe(&self.0)
    }

    /// The name, for argv.
    pub fn as_os_string(&self) -> OsString {
        super::path::os_string(&self.0)
    }
}

impl From<&str> for RemoteName {
    fn from(name: &str) -> Self {
        Self::from_bytes(name.as_bytes())
    }
}

/// One remote-tracking branch, such as `refs/remotes/origin/main`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteBranch {
    pub remote: RemoteName,
    /// The name below the remote's, such as `main` or `feature/x`.
    pub short: Vec<u8>,
    /// The whole ref, as git listed it.
    pub refname: Vec<u8>,
}

impl RemoteBranch {
    /// The full ref, for argv: never taken for a tag, a local branch of the
    /// same name, or an option.
    pub fn full_ref(&self) -> OsString {
        super::path::os_string(&self.refname)
    }

    /// `remote/short`, terminal-safe: how git and the user write it.
    pub fn display(&self) -> String {
        let name = self
            .refname
            .strip_prefix(REMOTES_PREFIX)
            .unwrap_or(&self.refname);
        terminal_safe(name)
    }

    /// The short name, terminal-safe, as a local branch would be called.
    pub fn short_display(&self) -> String {
        terminal_safe(&self.short)
    }
}

/// A remote and its remote-tracking branches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    pub name: RemoteName,
    /// The first `remote.<name>.url`, terminal-safe and with any user part
    /// taken out, since a URL can carry a token. Never kept raw.
    pub url: Option<String>,
    /// Whether the config has a URL for it. Refs left under a name no
    /// remote has are grouped by their first path component instead.
    pub configured: bool,
    pub branches: Vec<RemoteBranch>,
}

const REMOTES_PREFIX: &[u8] = b"refs/remotes/";

/// Every remote with a URL in the config, plus groups for remote-tracking
/// refs no configured remote owns, each with its branches. Sorted by name
/// bytes, branches too.
pub fn list_remotes(runner: &dyn GitRunner) -> Result<Vec<Remote>, String> {
    let mut remotes = configured_remotes(runner)?;
    let refs = run(
        runner,
        GitCommand::new("for-each-ref")
            .arg("--format=%(refname)%00%(symref)")
            .arg("refs/remotes"),
    )?;
    for line in refs.split(|&b| b == b'\n') {
        let mut fields = line.split(|&b| b == 0);
        let (Some(refname), Some(symref)) = (fields.next(), fields.next()) else {
            continue;
        };
        // `<remote>/HEAD` points at a branch listed on its own.
        if !symref.is_empty() {
            continue;
        }
        let Some(rest) = refname.strip_prefix(REMOTES_PREFIX) else {
            continue;
        };
        let (index, short) = match owner(&remotes, rest) {
            Some(index) => {
                let short = &rest[remotes[index].name.0.len() + 1..];
                (index, short)
            }
            None => {
                let group = rest.split(|&b| b == b'/').next().unwrap_or(rest);
                let short = rest.get(group.len() + 1..).unwrap_or(rest);
                let index = match remotes.iter().position(|r| r.name.0 == group) {
                    Some(index) => index,
                    None => {
                        remotes.push(Remote {
                            name: RemoteName::from_bytes(group),
                            url: None,
                            configured: false,
                            branches: Vec::new(),
                        });
                        remotes.len() - 1
                    }
                };
                (index, short)
            }
        };
        let remote = remotes[index].name.clone();
        remotes[index].branches.push(RemoteBranch {
            remote,
            short: short.to_vec(),
            refname: refname.to_vec(),
        });
    }
    remotes.sort_by(|a, b| a.name.cmp(&b.name));
    for remote in &mut remotes {
        remote.branches.sort_by(|a, b| a.short.cmp(&b.short));
    }
    Ok(remotes)
}

/// The configured remote with the longest name that `rest` starts with,
/// followed by `/`. Names may contain `/`, so `a/b` owns `a/b/c` though
/// `a` also exists.
fn owner(remotes: &[Remote], rest: &[u8]) -> Option<usize> {
    remotes
        .iter()
        .enumerate()
        .filter(|(_, r)| r.configured)
        .filter(|(_, r)| {
            rest.strip_prefix(r.name.0.as_slice())
                .is_some_and(|tail| tail.starts_with(b"/"))
        })
        .max_by_key(|(_, r)| r.name.0.len())
        .map(|(i, _)| i)
}

/// The remotes with a `remote.<name>.url`, each with its first URL. A
/// name may contain dots, so it is everything between `remote.` and the
/// last `.url`.
fn configured_remotes(runner: &dyn GitRunner) -> Result<Vec<Remote>, String> {
    let cmd = GitCommand::new("config")
        .arg("-z")
        .arg("--get-regexp")
        .arg(r"^remote\..*\.url$")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C");
    let out = runner
        .run(&cmd)
        .map_err(|e| terminal_safe(e.to_string().as_bytes()))?;
    if !out.success {
        // Nothing matched: exit 1 with nothing on stderr.
        if out.stderr.is_empty() {
            return Ok(Vec::new());
        }
        return Err(terminal_safe(&out.stderr));
    }
    let out = out.stdout;
    let mut remotes: Vec<Remote> = Vec::new();
    for entry in out.split(|&b| b == 0) {
        let (key, value) = match entry.iter().position(|&b| b == b'\n') {
            Some(at) => (&entry[..at], &entry[at + 1..]),
            None => (entry, &b""[..]),
        };
        let Some(name) = key
            .strip_prefix(b"remote.")
            .and_then(|k| k.strip_suffix(b".url"))
        else {
            continue;
        };
        if name.is_empty() || remotes.iter().any(|r| r.name.0 == name) {
            continue;
        }
        remotes.push(Remote {
            name: RemoteName::from_bytes(name),
            url: Some(redact_userinfo(&terminal_safe(value))),
            configured: true,
            branches: Vec::new(),
        });
    }
    Ok(remotes)
}

/// Why `name` cannot be a new remote's name, if it cannot. Git checks a
/// name by whether `refs/remotes/<name>/x` is a valid ref; this applies
/// the same rules, and also refuses a name git would read as an option,
/// a bare `@`, and a trailing `.`.
pub fn validate_remote_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("A remote needs a name");
    }
    if name.starts_with('-') {
        return Err("A remote name cannot start with -");
    }
    if name == "@" {
        return Err("A remote cannot be called @");
    }
    if name
        .chars()
        .any(|c| c.is_ascii_control() || " ~^:?*[\\".contains(c))
    {
        return Err("A remote name cannot contain spaces, control characters or ~^:?*[\\");
    }
    if name.contains("..") || name.contains("@{") {
        return Err("A remote name cannot contain .. or @{");
    }
    if name.starts_with('/') || name.ends_with('/') || name.contains("//") {
        return Err("A remote name cannot start or end with / or contain //");
    }
    if name.ends_with('.') {
        return Err("A remote name cannot end with .");
    }
    let bad_part = |part: &str| part.starts_with('.') || part.ends_with(".lock");
    if name.split('/').any(bad_part) {
        return Err("No part of a remote name can start with . or end with .lock");
    }
    Ok(())
}

/// Why `url` cannot be a new remote's URL, if it cannot. Git would read
/// one starting with `-` as an option, and the `ext` and `fd`
/// transports run a command or use a file descriptor on every later
/// fetch, so Kika refuses them whatever `protocol.allow` says.
pub fn validate_remote_url(url: &str) -> Result<(), &'static str> {
    if url.is_empty() {
        return Err("A remote needs a URL");
    }
    if url.starts_with('-') {
        return Err("A remote URL cannot start with -");
    }
    if url.chars().any(char::is_control) {
        return Err("A remote URL cannot contain control characters");
    }
    // Both `ext::<command>` and `ext://...` reach the ext helper: git runs
    // `git remote-<scheme>` for a scheme it does not know. So the scheme
    // is everything before the first `:`, which leaves scp-like
    // `user@host:path` and `C:\path` alone.
    let scheme = url.split_once(':').map(|(s, _)| s.to_ascii_lowercase());
    if matches!(scheme.as_deref(), Some("ext" | "fd")) {
        return Err("Kika does not add ext or fd transport remotes; add it with git directly");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::CliRunner;
    use crate::git::fake::{FakeRunner, TempDir, fail, git, ok};

    const CONFIG: &[&str] = &["config", "-z", "--get-regexp", r"^remote\..*\.url$"];
    const REFS: &[&str] = &[
        "for-each-ref",
        "--format=%(refname)%00%(symref)",
        "refs/remotes",
    ];

    fn read_env() -> Vec<(OsString, OsString)> {
        vec![
            ("GIT_OPTIONAL_LOCKS".into(), "0".into()),
            ("LC_ALL".into(), "C".into()),
        ]
    }

    fn names(remotes: &[Remote]) -> Vec<String> {
        remotes.iter().map(|r| r.name.display()).collect()
    }

    fn branches(remote: &Remote) -> Vec<String> {
        remote.branches.iter().map(RemoteBranch::display).collect()
    }

    #[test]
    fn refs_are_grouped_under_the_longest_configured_name_and_head_is_skipped() {
        let config = b"remote.origin.url\nhttps://u:tok@h/r\0\
                       remote.my.fork.url\n/srv/fork\0\
                       remote.a.url\n/a\0\
                       remote.a/b.url\n/ab\0\
                       remote.origin.url\nsecond\0";
        let refs = b"refs/remotes/origin/HEAD\0refs/remotes/origin/main\n\
                     refs/remotes/origin/main\0\n\
                     refs/remotes/origin/feature/x\0\n\
                     refs/remotes/my.fork/topic\0\n\
                     refs/remotes/a/b/c\0\n\
                     refs/remotes/a/d\0\n\
                     refs/remotes/gone/old\0\n";
        let fake = FakeRunner::new()
            .expect(CONFIG, ok(config))
            .expect(REFS, ok(refs));

        let remotes = list_remotes(&fake).unwrap();

        fake.assert_done();
        assert_eq!(fake.envs(), [read_env(), read_env()]);
        assert_eq!(names(&remotes), ["a", "a/b", "gone", "my.fork", "origin"]);
        let by = |name: &str| remotes.iter().find(|r| r.name.display() == name).unwrap();
        assert_eq!(branches(by("a")), ["a/d"]);
        assert_eq!(branches(by("a/b")), ["a/b/c"]);
        assert_eq!(by("a/b").branches[0].short, b"c");
        assert_eq!(branches(by("my.fork")), ["my.fork/topic"]);
        assert_eq!(branches(by("origin")), ["origin/feature/x", "origin/main"]);
        assert_eq!(by("origin").url.as_deref(), Some("https://h/r"));
        assert!(by("origin").configured);
        let gone = by("gone");
        assert!(!gone.configured);
        assert_eq!(
            (gone.url.as_deref(), branches(gone)),
            (None, vec!["gone/old".into()])
        );
        assert_eq!(gone.branches[0].short, b"old");
        assert_eq!(
            by("origin").branches[1].full_ref(),
            OsString::from("refs/remotes/origin/main")
        );
    }

    #[test]
    fn no_remotes_is_an_empty_list_and_a_broken_config_an_error() {
        let fake = FakeRunner::new()
            .expect(CONFIG, fail(""))
            .expect(REFS, ok(b""));
        assert_eq!(list_remotes(&fake), Ok(vec![]));
        fake.assert_done();

        let fake = FakeRunner::new().expect(CONFIG, fail("fatal: bad \x1b[2Jconfig\n"));
        assert_eq!(
            list_remotes(&fake),
            Err("fatal: bad \u{FFFD}[2Jconfig".into())
        );
    }

    #[test]
    fn a_url_is_drawn_safe_and_without_credentials() {
        let fake = FakeRunner::new()
            .expect(CONFIG, ok(b"remote.o.url\nhttps://me:s3cret@h/\x1b[2Jr\0"))
            .expect(REFS, ok(b""));
        let remotes = list_remotes(&fake).unwrap();
        assert_eq!(remotes[0].url.as_deref(), Some("https://h/\u{FFFD}[2Jr"));
    }

    #[test]
    fn remotes_and_their_branches_list_in_real_git() {
        let tmp = TempDir::new("remotes");
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["commit", "-q", "--allow-empty", "-m", "init"]);
        git(dir, &["remote", "add", "origin", "https://u:tok@h/r"]);
        git(dir, &["remote", "add", "my.fork", "/srv/fork"]);
        for r in ["origin/main", "origin/topic", "my.fork/x", "stray/y"] {
            git(dir, &["update-ref", &format!("refs/remotes/{r}"), "HEAD"]);
        }
        git(
            dir,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );

        let remotes = list_remotes(&CliRunner::isolated(dir)).unwrap();

        assert_eq!(names(&remotes), ["my.fork", "origin", "stray"]);
        assert_eq!(branches(&remotes[0]), ["my.fork/x"]);
        assert_eq!(branches(&remotes[1]), ["origin/main", "origin/topic"]);
        assert_eq!(remotes[1].url.as_deref(), Some("https://h/r"));
        assert!(!remotes[2].configured);
    }

    #[test]
    fn a_name_is_refused_exactly_when_git_refuses_it_plus_a_few_more() {
        // Kika also refuses these, which git accepts in this position.
        const KIKA_ONLY: &[&str] = &["-a", "@", "foo."];
        // Control characters do not survive every platform's argv.
        #[cfg(unix)]
        const CONTROL: &[&str] = &["a\x01", "a\x7f", "a\tb"];
        #[cfg(not(unix))]
        const CONTROL: &[&str] = &[];
        let names = [
            "origin", "my.fork", "a/b", "é", "a@b", "@a", "lock", "a.lockx", "-a", "@", "foo.", "",
            "a.lock", "a.lock/b", ".a", "a/.b", "a..b", "a@{b", "a b", "a~", "a^", "a:b", "a?",
            "a*", "a[", "a\\b", "a//b", "/a", "a/", ".lock",
        ];
        for name in names.into_iter().chain(CONTROL.iter().copied()) {
            let ours = validate_remote_name(name).is_ok();
            let theirs = !name.is_empty()
                && crate::git::fake::try_git(
                    &std::env::temp_dir(),
                    &["check-ref-format", &format!("refs/remotes/{name}/x")],
                );
            if KIKA_ONLY.contains(&name) {
                assert!(theirs && !ours, "{name:?}");
            } else {
                assert_eq!(ours, theirs, "{name:?}");
            }
        }
    }

    #[test]
    fn a_url_that_runs_a_command_or_reads_as_an_option_is_refused() {
        for bad in [
            "ext::sh -c x",
            "EXT::sh -c x",
            "Ext::x",
            "fd::3",
            "FD::3,4",
            "ext://sh -c x",
            "EXT://x",
            "fd://3",
            "ext:x",
            "-oProxyCommand=x",
            "",
            "https://h/\x1b[2J",
            "https://h/\nx",
        ] {
            assert!(validate_remote_url(bad).is_err(), "{bad:?}");
        }
        let ext = validate_remote_url("ext::x").unwrap_err();
        assert!(ext.contains("git directly"), "{ext}");
        for good in [
            "https://github.com/me/kika.git",
            "https://u:tok@h/r",
            "ssh://git@h:22/r.git",
            "git@github.com:me/kika.git",
            "/srv/git/kika.git",
            "../kika",
            "file:///srv/kika",
            r"C:\repos\kika",
            "c:/repos/kika",
            "extra.host:repo.git",
            "ext.example.com:me/kika.git",
            "https://ext.example.com/fd",
        ] {
            assert_eq!(validate_remote_url(good), Ok(()), "{good:?}");
        }
    }
}
