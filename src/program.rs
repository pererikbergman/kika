//! Finding the programs Kika starts. Every production spawn goes through
//! [`command`], which searches only absolute `PATH` entries, so a relative
//! or empty entry (`.`, `::`) can never run a file planted in a repository.
//! A program not found that way is not started at all.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Why a program was not found. Fixed text that never names the program,
/// so callers decide how (and how safely) to show that.
pub(crate) const NOT_FOUND: &str = "not found in an absolute PATH directory";

/// The process for `program`, located by [`resolve`]. The only way
/// production code builds a `Command`.
pub fn command(program: &OsStr) -> io::Result<Command> {
    Ok(Command::new(resolve(program)?))
}

/// The absolute path `program` runs from: itself when absolute (on
/// Windows, with `.exe` or `.com` added to a bare name), else the first
/// match in an absolute `PATH` entry. `NotFound` otherwise.
pub fn resolve(program: &OsStr) -> io::Result<PathBuf> {
    resolve_in(program, std::env::var_os("PATH").as_deref())
}

/// [`resolve`] against the given `PATH` value, so tests need not touch the
/// process environment.
fn resolve_in(program: &OsStr, path: Option<&OsStr>) -> io::Result<PathBuf> {
    let windows = cfg!(windows);
    let as_path = Path::new(program);
    if as_path.is_absolute() {
        // The same names as a search, in the program's own directory.
        let (Some(dir), Some(name)) = (as_path.parent(), as_path.file_name()) else {
            return Err(not_found());
        };
        return file_names(name, windows)
            .into_iter()
            .map(|name| dir.join(name))
            .find(|candidate| is_executable(candidate))
            .ok_or_else(not_found);
    }
    if program.is_empty() || has_separator(program, windows) {
        return Err(not_found());
    }
    let names = file_names(program, windows);
    path.into_iter()
        .flat_map(std::env::split_paths)
        .filter(|dir| dir.is_absolute())
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|candidate| is_executable(candidate))
        .ok_or_else(not_found)
}

fn not_found() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, NOT_FOUND)
}

/// A name with a directory part is a relative path, never a `PATH` lookup.
/// On Windows `\` separates too, and `:` names a drive (`C:tool`).
fn has_separator(program: &OsStr, windows: bool) -> bool {
    program
        .as_encoded_bytes()
        .iter()
        .any(|&b| b == b'/' || (windows && (b == b'\\' || b == b':')))
}

/// The file names `program` may have in a directory. Windows tries `.exe`
/// and `.com` for a bare name. `.bat` and `.cmd` would run through
/// `cmd.exe`, a shell, so they match nothing.
fn file_names(program: &OsStr, windows: bool) -> Vec<OsString> {
    if !windows {
        return vec![program.to_os_string()];
    }
    let extension = Path::new(program)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    match extension.as_deref() {
        Some("exe" | "com") => vec![program.to_os_string()],
        Some("bat" | "cmd") => Vec::new(),
        _ => [".exe", ".com"]
            .iter()
            .map(|ext| {
                let mut name = program.to_os_string();
                name.push(ext);
                name
            })
            .collect(),
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Marks a copy of the test binary started with a hostile `PATH` and a
/// working directory full of decoys; the hidden spawn-site tests run only
/// there.
#[cfg(all(test, unix))]
const DECOY_CHILD_ENV: &str = "KIKA_DECOY_CHILD";

/// Whether this test process is such a copy.
#[cfg(all(test, unix))]
pub(crate) fn in_decoy_child() -> bool {
    std::env::var_os(DECOY_CHILD_ENV).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(program: &str, windows: bool) -> Vec<OsString> {
        file_names(OsStr::new(program), windows)
    }

    #[test]
    fn windows_tries_exe_and_com_for_a_bare_name() {
        assert_eq!(names("git", true), ["git.exe", "git.com"]);
        assert_eq!(names("git.exe", true), ["git.exe"]);
        assert_eq!(names("git.COM", true), ["git.COM"]);
    }

    #[test]
    fn windows_never_matches_a_script_run_through_cmd() {
        assert!(names("x.bat", true).is_empty());
        assert!(names("x.CMD", true).is_empty());
    }

    #[test]
    fn unix_looks_for_the_name_itself() {
        assert_eq!(names("git", false), ["git"]);
        assert_eq!(names("x.bat", false), ["x.bat"]);
    }

    #[test]
    fn a_drive_or_backslash_is_a_separator_only_on_windows() {
        assert!(has_separator(OsStr::new("bin/tool"), false));
        assert!(!has_separator(OsStr::new("C:tool"), false));
        assert!(has_separator(OsStr::new("C:tool"), true));
        assert!(has_separator(OsStr::new("bin\\tool"), true));
        assert!(!has_separator(OsStr::new("tool"), true));
    }

    #[cfg(unix)]
    mod unix {
        use super::super::*;
        use crate::git::fake::TempDir;
        use std::os::unix::fs::PermissionsExt;

        /// An executable script at `dir/name`.
        fn decoy(dir: &Path, name: &str, body: &str) -> PathBuf {
            let file = dir.join(name);
            std::fs::write(&file, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
            file
        }

        /// `target` as a relative path from the current directory, made of
        /// `..` up to the root and back down.
        fn relative_from_cwd(target: &Path) -> PathBuf {
            let cwd = std::env::current_dir().unwrap();
            let mut relative: PathBuf = cwd.components().skip(1).map(|_| "..").collect();
            relative.push(target.strip_prefix("/").unwrap());
            relative
        }

        fn assert_not_found(result: io::Result<PathBuf>, program: &str) {
            let e = result.expect_err(program);
            assert_eq!(e.kind(), io::ErrorKind::NotFound, "{program}");
            assert_eq!(e.to_string(), NOT_FOUND);
            if !program.is_empty() {
                assert!(!e.to_string().contains(program), "{e}");
            }
        }

        #[test]
        fn relative_and_empty_path_entries_are_never_searched() {
            let tmp = TempDir::new("program-relative");
            let found = decoy(tmp.path(), "kika-decoy", "exit 0");
            let relative = relative_from_cwd(tmp.path());
            assert!(relative.is_relative());
            assert!(relative.join("kika-decoy").is_file(), "{relative:?}");

            let mut entries = vec![relative, PathBuf::from("."), PathBuf::new()];
            let path = std::env::join_paths(&entries).unwrap();
            let program = OsStr::new("kika-decoy");
            assert_not_found(resolve_in(program, Some(&path)), "kika-decoy");
            assert_not_found(resolve_in(program, Some(OsStr::new(".::"))), "kika-decoy");
            assert_not_found(resolve_in(program, None), "kika-decoy");

            entries.push(tmp.path().to_path_buf());
            let path = std::env::join_paths(&entries).unwrap();
            assert_eq!(resolve_in(program, Some(&path)).unwrap(), found);
        }

        #[test]
        fn the_first_absolute_entry_with_an_executable_file_wins() {
            let (one, two) = (TempDir::new("program-one"), TempDir::new("program-two"));
            std::fs::write(one.path().join("tool"), "not executable").unwrap();
            let found = decoy(two.path(), "tool", "exit 0");
            let path = std::env::join_paths([one.path(), two.path()]).unwrap();
            assert_eq!(resolve_in(OsStr::new("tool"), Some(&path)).unwrap(), found);
        }

        #[test]
        fn malformed_program_names_are_refused() {
            let tmp = TempDir::new("program-malformed");
            decoy(tmp.path(), "tool", "exit 0");
            std::fs::create_dir(tmp.path().join("bin")).unwrap();
            decoy(&tmp.path().join("bin"), "tool", "exit 0");
            let plain = tmp.path().join("plain");
            std::fs::write(&plain, "not executable").unwrap();
            let path = tmp.path().as_os_str();

            for program in ["./tool", "bin/tool", ""] {
                assert_not_found(resolve_in(OsStr::new(program), Some(path)), program);
            }
            let plain = plain.to_str().unwrap();
            assert_not_found(resolve_in(OsStr::new(plain), Some(path)), plain);
            let missing = tmp.path().join("missing");
            let missing = missing.to_str().unwrap();
            assert_not_found(resolve_in(OsStr::new(missing), Some(path)), missing);
        }

        #[test]
        fn an_absolute_executable_is_used_as_is() {
            let tmp = TempDir::new("program-absolute");
            let tool = decoy(tmp.path(), "tool", "exit 0");
            assert_eq!(resolve_in(tool.as_os_str(), None).unwrap(), tool);
        }

        /// The hidden tests that try every spawn site from a directory of
        /// decoys, by their full test names.
        const DECOY_CHILDREN: [&str; 3] = [
            "program::tests::unix::decoy_child_public_sites",
            "effect::tests::decoy_child_open_url_and_clipboard",
            "handoff::tests::decoy_child_run_command_and_editor",
        ];

        /// Each spawn site is tried in a copy of this test binary whose
        /// `PATH` is only a relative and empty entries, and whose working
        /// directory holds decoys under the names Kika starts. Each decoy
        /// leaves a mark when run, so none may have left one.
        #[test]
        fn no_spawn_site_runs_a_decoy_from_the_working_directory() {
            let (decoys, marks) = (TempDir::new("decoys"), TempDir::new("decoy-marks"));
            let names = ["git", "gh", "sh", "kika-decoy"];
            for name in names {
                // A redirection, not a program, so it needs no `PATH`.
                let mark = marks.path().join(name);
                decoy(decoys.path(), name, &format!(": > '{}'", mark.display()));
            }
            // The marks work: a decoy run on purpose leaves one.
            let status = std::process::Command::new(decoys.path().join("kika-decoy"))
                .env("PATH", ".::")
                .status()
                .unwrap();
            assert!(status.success());
            std::fs::remove_file(marks.path().join("kika-decoy")).unwrap();

            let exe = std::env::current_exe().unwrap();
            for child in DECOY_CHILDREN {
                let output = std::process::Command::new(&exe)
                    .args(["--exact", child, "--test-threads=1"])
                    .env(DECOY_CHILD_ENV, "1")
                    .env("PATH", ".::")
                    .current_dir(decoys.path())
                    .stdin(std::process::Stdio::null())
                    .output()
                    .unwrap();
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(
                    output.status.success() && stdout.contains(" 1 passed"),
                    "{child}:\n{stdout}\n{stderr}"
                );
            }
            let ran: Vec<_> = std::fs::read_dir(marks.path())
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            assert!(ran.is_empty(), "decoys ran: {ran:?}");
        }

        /// Hidden: the spawn sites other modules can reach, run by
        /// [`no_spawn_site_runs_a_decoy_from_the_working_directory`].
        #[tokio::test]
        async fn decoy_child_public_sites() {
            use crate::features::custom_commands::CommandFailure;
            use crate::features::custom_commands::run_captured;
            use crate::features::diff::renderers::external_diff::{
                ExternalRenderer, LIMITS, RendererKind,
            };
            use crate::forge::github::{CliGh, GhCall, GhCommand, GhRunner};
            use crate::git::runner::{CliRunner, GitCommand, GitRunner};

            if !in_decoy_child() {
                return;
            }
            let here = std::env::current_dir().unwrap();
            let runner = CliRunner::new(&here);
            let version = GitCommand::new("--version").standard_env();
            let not_found = |e: io::Error| e.kind() == io::ErrorKind::NotFound;
            assert!(runner.run(&version).is_err_and(not_found));
            assert!(runner.spawn(&version).is_err_and(not_found));
            assert!(runner.run_attached(&version).is_err_and(not_found));
            let refused = crate::git::version::check(&runner).unwrap_err();
            assert!(refused.contains("git was not found"), "{refused}");

            let gh = GhCommand {
                host: "github.com".into(),
                kind: GhCall::Probe {
                    path: "rate_limit".into(),
                    etag: None,
                },
                cwd: here.clone(),
                timeout: std::time::Duration::from_secs(5),
            };
            assert!(CliGh.run(&gh).is_err_and(not_found));

            match run_captured(vec!["kika-decoy".into()], here.clone()).await {
                Err(CommandFailure::Launch(e)) => assert!(e.contains(NOT_FOUND), "{e}"),
                other => panic!("{other:?}"),
            }

            let renderer = ExternalRenderer {
                label: "kika-decoy".into(),
                argv: vec!["kika-decoy".into()],
                kind: RendererKind::Pager,
            };
            let rendered = crate::features::diff::renderers::external_diff::run(
                &renderer,
                &here,
                &[],
                b"",
                80,
                LIMITS,
            );
            assert_eq!(rendered, Err("kika-decoy not found".into()));
        }

        #[test]
        fn resolve_reads_path_and_command_uses_the_absolute_program() {
            let found = resolve(OsStr::new("sh")).expect("sh is on PATH");
            assert!(found.is_absolute());
            let command = command(OsStr::new("sh")).unwrap();
            assert_eq!(command.get_program(), found.as_os_str());
        }
    }

    /// No production code builds a process by itself: outside this module
    /// and test-only code, `Command::new(` never appears. `GitCommand::new(`
    /// and the like are other types.
    #[test]
    fn every_spawn_site_goes_through_this_module() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let skipped = [
            src.join("program.rs"),
            src.join("e2e"),
            src.join("git").join("fake.rs"),
        ];
        let mut offenders = Vec::new();
        let mut checked = 0;
        let mut dirs = vec![src.clone()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if skipped.contains(&path) {
                    continue;
                }
                if path.is_dir() {
                    dirs.push(path);
                } else if path.extension() == Some(OsStr::new("rs")) {
                    let text = std::fs::read_to_string(&path).unwrap();
                    checked += 1;
                    if constructs_a_command(production_part(&text)) {
                        offenders.push(path);
                    }
                }
            }
        }
        assert!(checked > 50, "only {checked} files checked");
        assert!(offenders.is_empty(), "{offenders:?}");
    }

    /// The text before the file's test module.
    fn production_part(text: &str) -> &str {
        text.find("\nmod tests {").map_or(text, |end| &text[..end])
    }

    /// `Command::new(` with no identifier character right before it.
    fn constructs_a_command(text: &str) -> bool {
        text.match_indices("Command::new(").any(|(at, _)| {
            !text[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
        })
    }

    #[test]
    fn the_spawn_scan_tells_types_apart_and_stops_at_the_tests() {
        assert!(constructs_a_command("let c = Command::new(p);"));
        assert!(constructs_a_command("std::process::Command::new(p)"));
        assert!(!constructs_a_command("GitCommand::new(\"log\")"));
        assert!(!constructs_a_command("Gh_Command::new(x)"));
        let text = "fn a() {}\n#[cfg(test)]\nmod tests {\n Command::new(\"sh\") }";
        assert!(!constructs_a_command(production_part(text)));
    }
}
