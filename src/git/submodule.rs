//! The repository's submodules and where each one stands, as
//! `git submodule status` reports them.

use super::path::RepoPath;
use super::runner::{GitCommand, GitRunner};
use super::safe_text::sanitize;

/// One submodule: its path from the top level, the commit git reports for
/// it, and how its checkout compares to what the parent records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submodule {
    pub path: RepoPath,
    pub oid: String,
    pub state: SubmoduleState,
}

/// The status line's first character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmoduleState {
    /// `-`: not cloned or checked out yet.
    Uninitialized,
    /// ` `: checked out at the commit the parent records.
    UpToDate,
    /// `+`: checked out at another commit.
    OutOfDate,
    /// `U`: the parent has a merge conflict on it.
    Conflicted,
}

impl SubmoduleState {
    fn from_marker(marker: u8) -> Option<Self> {
        match marker {
            b'-' => Some(Self::Uninitialized),
            b' ' => Some(Self::UpToDate),
            b'+' => Some(Self::OutOfDate),
            b'U' => Some(Self::Conflicted),
            _ => None,
        }
    }

    /// The character git uses, for the list.
    pub fn marker(self) -> char {
        match self {
            Self::Uninitialized => '-',
            Self::UpToDate => ' ',
            Self::OutOfDate => '+',
            Self::Conflicted => 'U',
        }
    }

    /// The marker in words, since a blank one says nothing on its own.
    pub fn word(self) -> &'static str {
        match self {
            Self::Uninitialized => "not initialized",
            Self::UpToDate => "up to date",
            Self::OutOfDate => "out of date",
            Self::Conflicted => "conflicted",
        }
    }
}

/// A read: no optional locks, and English output so the format is fixed.
/// It takes no pathspec, so literal pathspecs change nothing (a real-git
/// test pins that).
pub fn status_command() -> GitCommand {
    GitCommand::new("submodule").arg("status").standard_env()
}

/// Parse `git submodule status`: per line, the state character, the hex
/// oid, a space, then the path as raw bytes. The command has no `-z`, so a
/// trailing ` (<describe>)` is dropped only when the line ends in `)`; a
/// path that itself ends in ` (…)` is cut, which is accepted.
pub fn parse_status(stdout: &[u8]) -> Result<Vec<Submodule>, String> {
    stdout
        .split(|&b| b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| parse_line(line).ok_or_else(|| bad_line(line)))
        .collect()
}

fn parse_line(line: &[u8]) -> Option<Submodule> {
    let (&marker, rest) = line.split_first()?;
    let state = SubmoduleState::from_marker(marker)?;
    let space = rest.iter().position(|&b| b == b' ')?;
    let (oid, path) = (&rest[..space], &rest[space + 1..]);
    if oid.is_empty() || !oid.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let path = match path.strip_suffix(b")") {
        Some(_) => {
            let open = path.windows(2).rposition(|w| w == b" (")?;
            &path[..open]
        }
        None => path,
    };
    if path.is_empty() {
        return None;
    }
    Some(Submodule {
        path: RepoPath::from_bytes(path),
        oid: String::from_utf8_lossy(oid).into_owned(),
        state,
    })
}

fn bad_line(line: &[u8]) -> String {
    format!("Unexpected submodule status line: {}", sanitize(line))
}

/// Every submodule of the repository, in git's order. A failure carries
/// git's message, made safe to draw.
pub fn read_submodules(runner: &dyn GitRunner) -> Result<Vec<Submodule>, String> {
    let output = runner
        .run_checked(&status_command())
        .map_err(|e| e.message_or("git submodule status failed"))?;
    parse_status(&output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::fake::{FakeRunner, fail, ok};
    use crate::git::runner::EnvProfile;
    use crate::test_support::{STANDARD_ENV, env_list};

    const OID: &str = "1234567890abcdef1234567890abcdef12345678";

    fn sub(path: &str, state: SubmoduleState) -> Submodule {
        Submodule {
            path: path.into(),
            oid: OID.into(),
            state,
        }
    }

    #[test]
    fn every_state_character_is_read() {
        let out =
            format!("-{OID} a\n {OID} b (heads/main)\n+{OID} c (v1.0-2-g1234567)\nU{OID} d\n");
        assert_eq!(
            parse_status(out.as_bytes()),
            Ok(vec![
                sub("a", SubmoduleState::Uninitialized),
                sub("b", SubmoduleState::UpToDate),
                sub("c", SubmoduleState::OutOfDate),
                sub("d", SubmoduleState::Conflicted),
            ])
        );
    }

    #[test]
    fn a_path_with_spaces_keeps_them_and_loses_only_the_describe() {
        let out = format!(" {OID} libs/my lib (heads/main)\n-{OID} other dir\n");
        assert_eq!(
            parse_status(out.as_bytes()),
            Ok(vec![
                sub("libs/my lib", SubmoduleState::UpToDate),
                sub("other dir", SubmoduleState::Uninitialized),
            ])
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_utf8_keeps_its_bytes() {
        use std::os::unix::ffi::OsStrExt;

        let mut out = format!(" {OID} x").into_bytes();
        out.extend(b"\xff (heads/main)\n");
        let subs = parse_status(&out).unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].path.as_os_str().as_bytes(), b"x\xff");
        assert_eq!(subs[0].path.display(), "x\u{FFFD}");
    }

    #[test]
    fn empty_output_is_no_submodules() {
        assert_eq!(parse_status(b""), Ok(vec![]));
    }

    #[test]
    fn a_malformed_line_is_an_error_safe_to_draw() {
        let error = parse_status(b"?zz \x1b[2Jpath\n").unwrap_err();
        assert!(
            error.starts_with("Unexpected submodule status line"),
            "{error}"
        );
        assert!(!error.contains(char::is_control), "{error:?}");
        assert!(parse_status(format!(" {OID}\n").as_bytes()).is_err());
        assert!(parse_status(b" nothex path\n").is_err());
    }

    #[test]
    fn the_command_is_a_read_with_fixed_output() {
        let cmd = status_command();
        assert_eq!(cmd.args(), ["submodule", "status"]);
        assert_eq!(cmd.env_profile(), Some(EnvProfile::Standard));
        assert_eq!(cmd.process_env(), env_list(&STANDARD_ENV));
    }

    /// `submodule status` walks the index for submodules itself, so this
    /// pins that literal pathspecs leave it listing what it lists without.
    #[test]
    fn the_standard_profile_still_lists_a_real_submodule() {
        use crate::git::CliRunner;
        use crate::git::fake::{TempDir, git};

        let tmp = TempDir::new("submodule-profile");
        let (inner, parent) = (tmp.path().join("inner"), tmp.path().join("parent"));
        for dir in [&inner, &parent] {
            std::fs::create_dir(dir).unwrap();
            git(dir, &["init", "-q"]);
            git(dir, &["commit", "-q", "--allow-empty", "-m", "init"]);
        }
        let source = inner.to_str().expect("a UTF-8 temp dir");
        git(
            &parent,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                source,
                "sub",
            ],
        );
        let runner = CliRunner::isolated(&parent).with_env("GIT_LITERAL_PATHSPECS", "0");

        let listed = read_submodules(&runner).unwrap();
        let plain = runner.run(&status_command().user_env()).unwrap();

        assert_eq!(
            listed.iter().map(|s| s.path.display()).collect::<Vec<_>>(),
            ["sub"]
        );
        assert_eq!(parse_status(&plain.stdout).unwrap(), listed);
    }

    #[test]
    fn read_submodules_parses_what_git_prints() {
        let out = format!("+{OID} sub (heads/main)\n");
        let runner = FakeRunner::new().expect(&["submodule", "status"], ok(out.as_bytes()));
        assert_eq!(
            read_submodules(&runner),
            Ok(vec![sub("sub", SubmoduleState::OutOfDate)])
        );
        runner.assert_done();
    }

    #[test]
    fn read_submodules_returns_gits_message_made_safe() {
        let runner =
            FakeRunner::new().expect(&["submodule", "status"], fail("fatal: no \x1b[31mrepo\n"));
        assert_eq!(
            read_submodules(&runner),
            Err("fatal: no \u{FFFD}[31mrepo".into())
        );
        let runner = FakeRunner::new().expect(&["submodule", "status"], fail(""));
        assert_eq!(
            read_submodules(&runner),
            Err("git submodule status failed".into())
        );
    }
}
