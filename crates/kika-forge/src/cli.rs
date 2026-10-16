//! Whether a forge's CLI (`gh`, `glab`) is installed and usable, judged
//! from its version command alone: local, argv only, no network and no
//! token. The login is never checked here; the first real call says.
//!
//! [`CliGate`] keeps each forge kind's last answer, shared by every clone
//! of the [`ForgeContext`](crate::task::ForgeContext), so no call is made
//! for a kind whose CLI is known to be missing or broken.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::github::{GhCall, GhCommand, GhOutput, logged};
use super::gitlab::{GlabCall, GlabCommand};
use super::url::ForgeKind;
use super::{github::Gh, gitlab::Glab};
use kika_git::recording::CommandOutcome;

/// How long a version command may take: it does no network work.
pub const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

/// A CLI's version, as `major.minor.patch`; a missing patch is 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CliVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl fmt::Display for CliVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// What a forge's version command came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliStatus {
    Ready,
    /// The program is not on `PATH`.
    NotInstalled,
    /// It exits non-zero, times out, or prints no version Kika can read.
    Unusable,
    /// Older than the adapter's minimum for a flag it needs.
    TooOld {
        found: CliVersion,
        needed: CliVersion,
    },
}

/// The status a version command's `output` gives, with `min` the oldest
/// version the adapter accepts. Nothing the CLI printed is kept but the
/// parsed numbers.
pub fn judge(output: io::Result<GhOutput>, min: Option<CliVersion>) -> CliStatus {
    let output = match output {
        Ok(output) => output,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return CliStatus::NotInstalled,
        Err(_) => return CliStatus::Unusable,
    };
    if output.timed_out || output.exit != Some(0) {
        return CliStatus::Unusable;
    }
    let Some(found) = version_of(&output.stdout) else {
        return CliStatus::Unusable;
    };
    match min {
        Some(needed) if found < needed => CliStatus::TooOld { found, needed },
        _ => CliStatus::Ready,
    }
}

/// The first token of the first line shaped `N.N` or `N.N.N`, with an
/// optional leading `v`: `gh version 2.40.1 (2023-12-13)` and
/// `glab 1.36.0 (…)` both read.
fn version_of(stdout: &[u8]) -> Option<CliVersion> {
    let text = String::from_utf8_lossy(stdout);
    let line = text.lines().next()?;
    line.split_whitespace().find_map(|token| {
        let token = token.strip_prefix('v').unwrap_or(token);
        let parts: Vec<&str> = token.split('.').collect();
        if !(2..=3).contains(&parts.len()) {
            return None;
        }
        let number = |part: &str| {
            (!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
                .then(|| part.parse::<u32>().ok())
                .flatten()
        };
        Some(CliVersion {
            major: number(parts[0])?,
            minor: number(parts[1])?,
            patch: match parts.get(2) {
                Some(part) => number(part)?,
                None => 0,
            },
        })
    })
}

/// The Command log's outcome of a version run.
fn outcome(output: &io::Result<GhOutput>) -> CommandOutcome {
    match output {
        Ok(output) if output.exit == Some(0) && !output.timed_out => CommandOutcome::Ok,
        Err(e) if e.kind() == io::ErrorKind::NotFound => CommandOutcome::NotStarted,
        _ => CommandOutcome::Failed(None),
    }
}

impl Gh<'_> {
    /// Run `gh --version`, logged: no host, no stdin.
    pub(crate) fn run_version(&self) -> io::Result<GhOutput> {
        let cmd = GhCommand {
            host: self.host.as_str().to_string(),
            kind: GhCall::Version,
            cwd: self.cwd.clone(),
            timeout: VERSION_TIMEOUT,
        };
        logged(
            self.sink,
            cmd.display_line(),
            || self.runner.run(&cmd),
            outcome,
        )
    }
}

impl Glab<'_> {
    /// Run `glab --version`, logged: no host, no stdin.
    pub(crate) fn run_version(&self) -> io::Result<GhOutput> {
        let cmd = GlabCommand {
            host: self.host.as_str().to_string(),
            kind: GlabCall::Version,
            cwd: self.cwd.clone(),
            timeout: VERSION_TIMEOUT,
        };
        logged(
            self.sink,
            cmd.display_line(),
            || self.runner.run(&cmd),
            outcome,
        )
    }
}

/// What the user is told about `status` where no forge kind is at hand,
/// such as a popup: words that name no forge.
pub fn problem_text(status: CliStatus) -> String {
    match status {
        CliStatus::Ready => "The forge CLI is ready.".into(),
        CliStatus::NotInstalled => "The forge CLI is not installed.".into(),
        CliStatus::Unusable => {
            "The forge CLI is not working; check that its --version runs.".into()
        }
        CliStatus::TooOld { found, needed } => {
            format!("The forge CLI {found} is too old; update to {needed} or newer.")
        }
    }
}

/// Each forge kind's last check, shared by every clone. A kind with no
/// entry yet counts as open: until the first check answers, only the
/// held-back refresh keeps calls from running.
#[derive(Debug, Clone, Default)]
pub struct CliGate(Arc<Mutex<HashMap<ForgeKind, CliStatus>>>);

impl CliGate {
    /// Keep `status` as `kind`'s answer.
    pub fn set(&self, kind: ForgeKind, status: CliStatus) {
        self.lock().insert(kind, status);
    }

    /// Why `kind` is shut: its last check, when that was not `Ready`.
    pub fn shut(&self, kind: ForgeKind) -> Option<CliStatus> {
        self.lock()
            .get(&kind)
            .copied()
            .filter(|status| *status != CliStatus::Ready)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ForgeKind, CliStatus>> {
        // A panic while holding it leaves a map still worth reading.
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn printed(stdout: &str) -> io::Result<GhOutput> {
        Ok(GhOutput {
            exit: Some(0),
            stdout: stdout.as_bytes().to_vec(),
            ..GhOutput::default()
        })
    }

    fn v(major: u32, minor: u32, patch: u32) -> CliVersion {
        CliVersion {
            major,
            minor,
            patch,
        }
    }

    #[test]
    fn both_sample_version_lines_parse() {
        let gh =
            "gh version 2.40.1 (2023-12-13)\nhttps://github.com/cli/cli/releases/tag/v2.40.1\n";
        assert_eq!(version_of(gh.as_bytes()), Some(v(2, 40, 1)));
        let glab = "glab 1.36.0 (f8b8d1b2)\n";
        assert_eq!(version_of(glab.as_bytes()), Some(v(1, 36, 0)));
        assert_eq!(version_of(b"glab version v1.5\n"), Some(v(1, 5, 0)));
        assert_eq!(judge(printed(gh), None), CliStatus::Ready);
        assert_eq!(judge(printed(glab), Some(v(1, 36, 0))), CliStatus::Ready);
    }

    #[test]
    fn only_the_first_line_and_a_plain_number_count() {
        for stdout in [
            "",
            "gh\n2.40.1\n",
            "gh version 2\n",
            "gh version 2.40.1.7\n",
            "gh version 2.x.1\n",
            "gh version +2.40.1\n",
            "gh version 99999999999.1.0\n",
            "gh version .1.2\n",
        ] {
            assert_eq!(
                judge(printed(stdout), None),
                CliStatus::Unusable,
                "{stdout:?}"
            );
        }
    }

    #[test]
    fn a_missing_program_is_not_installed_and_any_other_failure_unusable() {
        let missing: io::Result<GhOutput> = Err(io::ErrorKind::NotFound.into());
        assert_eq!(judge(missing, None), CliStatus::NotInstalled);
        let denied: io::Result<GhOutput> = Err(io::ErrorKind::PermissionDenied.into());
        assert_eq!(judge(denied, None), CliStatus::Unusable);
        let failed = Ok(GhOutput {
            exit: Some(1),
            stdout: b"gh version 2.40.1\n".to_vec(),
            ..GhOutput::default()
        });
        assert_eq!(judge(failed, None), CliStatus::Unusable);
        let killed = Ok(GhOutput {
            exit: None,
            stdout: b"gh version 2.40.1\n".to_vec(),
            ..GhOutput::default()
        });
        assert_eq!(judge(killed, None), CliStatus::Unusable);
        let timed_out = Ok(GhOutput {
            exit: Some(0),
            stdout: b"gh version 2.40.1\n".to_vec(),
            timed_out: true,
            ..GhOutput::default()
        });
        assert_eq!(judge(timed_out, None), CliStatus::Unusable);
    }

    #[test]
    fn a_version_below_the_minimum_is_too_old() {
        let line = || printed("glab 1.2.0 (abc)\n");
        assert_eq!(
            judge(line(), Some(v(1, 5, 0))),
            CliStatus::TooOld {
                found: v(1, 2, 0),
                needed: v(1, 5, 0)
            }
        );
        assert_eq!(judge(line(), Some(v(1, 2, 0))), CliStatus::Ready);
        assert_eq!(judge(line(), Some(v(1, 1, 9))), CliStatus::Ready);
        assert_eq!(v(1, 2, 0).to_string(), "1.2.0");
        assert!(v(1, 10, 0) > v(1, 9, 9));
    }

    #[test]
    fn the_gate_is_open_until_a_check_shuts_it_and_clones_share_it() {
        let gate = CliGate::default();
        let clone = gate.clone();
        assert_eq!(gate.shut(ForgeKind::GitLab), None);
        clone.set(ForgeKind::GitLab, CliStatus::NotInstalled);
        assert_eq!(gate.shut(ForgeKind::GitLab), Some(CliStatus::NotInstalled));
        assert_eq!(gate.shut(ForgeKind::GitHub), None);
        gate.set(ForgeKind::GitLab, CliStatus::Ready);
        assert_eq!(clone.shut(ForgeKind::GitLab), None);
    }

    #[test]
    fn the_text_without_a_forge_kind_names_no_forge() {
        for status in [
            CliStatus::NotInstalled,
            CliStatus::Unusable,
            CliStatus::TooOld {
                found: v(1, 2, 0),
                needed: v(1, 5, 0),
            },
        ] {
            let text = problem_text(status);
            for name in ["gh", "glab", "GitHub", "GitLab"] {
                assert!(!text.contains(name), "{text}");
            }
        }
        assert_eq!(
            problem_text(CliStatus::TooOld {
                found: v(1, 2, 0),
                needed: v(1, 5, 0)
            }),
            "The forge CLI 1.2.0 is too old; update to 1.5.0 or newer."
        );
    }
}
