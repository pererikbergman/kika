//! Guards that every process Kika starts goes through
//! [`kika_git::program`]: a scan of the sources for processes built by
//! hand, and a run of each spawn site from a directory full of decoys.
//! They read or run the whole tree, so they live in the main crate.

/// Marks a copy of the test binary started with a hostile `PATH` and a
/// working directory full of decoys; the hidden spawn-site tests run only
/// there.
#[cfg(unix)]
const DECOY_CHILD_ENV: &str = "KIKA_DECOY_CHILD";

/// Whether this test process is such a copy.
#[cfg(unix)]
pub(crate) fn in_decoy_child() -> bool {
    std::env::var_os(DECOY_CHILD_ENV).is_some()
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;

    #[cfg(unix)]
    mod unix {
        use super::super::*;
        use kika_git::fake::TempDir;
        use kika_git::program::NOT_FOUND;
        use std::io;
        use std::os::unix::fs::PermissionsExt;
        use std::path::{Path, PathBuf};

        /// An executable script at `dir/name`.
        fn decoy(dir: &Path, name: &str, body: &str) -> PathBuf {
            let file = dir.join(name);
            std::fs::write(&file, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
            file
        }

        /// The hidden tests that try every spawn site from a directory of
        /// decoys, by their full test names.
        const DECOY_CHILDREN: [&str; 3] = [
            "spawn_guard::tests::unix::decoy_child_public_sites",
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
            use kika_forge::github::{CliGh, GhCall, GhCommand, GhRunner};
            use kika_git::runner::{CliRunner, GitCommand, GitRunner};

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
            let refused = kika_git::version::check(&runner).unwrap_err();
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
    }

    /// No production code builds a process by itself: outside
    /// `kika_git::program` and test-only code, `Command::new(` never appears.
    /// `GitCommand::new(` and the like are other types.
    #[test]
    fn every_spawn_site_goes_through_this_module() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let git = root.join("crates").join("kika-git").join("src");
        let forge = root.join("crates").join("kika-forge").join("src");
        let src = root.join("src");
        let skipped = [git.join("program.rs"), src.join("e2e"), git.join("fake.rs")];
        let mut offenders = Vec::new();
        let mut checked = 0;
        let mut dirs = vec![src, git, forge];
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
