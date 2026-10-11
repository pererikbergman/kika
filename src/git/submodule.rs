//! The repository's submodules and where each one stands, as
//! `git submodule status` reports them.

use super::diff::{sanitize, terminal_safe};
use super::path::RepoPath;
use super::runner::{GitCommand, GitRunner};

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
pub fn status_command() -> GitCommand {
    GitCommand::new("submodule")
        .arg("status")
        .env("LC_ALL", "C")
        .env("GIT_OPTIONAL_LOCKS", "0")
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
        .run(&status_command())
        .map_err(|e| terminal_safe(e.to_string().as_bytes()))?;
    if !output.success {
        let message = terminal_safe(&output.stderr);
        return Err(if message.is_empty() {
            "git submodule status failed".into()
        } else {
            message
        });
    }
    parse_status(&output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::fake::{FakeRunner, fail, ok};

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
        assert_eq!(
            cmd.envs(),
            [
                ("LC_ALL".into(), "C".into()),
                ("GIT_OPTIONAL_LOCKS".into(), "0".into())
            ]
        );
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
