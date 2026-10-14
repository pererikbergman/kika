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
pub const NOT_FOUND: &str = "not found in an absolute PATH directory";

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
        use crate::fake::TempDir;
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

        #[test]
        fn resolve_reads_path_and_command_uses_the_absolute_program() {
            let found = resolve(OsStr::new("sh")).expect("sh is on PATH");
            assert!(found.is_absolute());
            let command = command(OsStr::new("sh")).unwrap();
            assert_eq!(command.get_program(), found.as_os_str());
        }
    }
}
