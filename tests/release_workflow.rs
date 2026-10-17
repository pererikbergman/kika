//! Guards on the release pipeline: `release.yml` only runs on GitHub-hosted
//! runners, builds every target, publishes only after all builds, and touches
//! no secret but the tap token; the Homebrew formula template renders to valid
//! Ruby. Plain text checks, so no YAML parser is needed.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const TARGETS: [&str; 5] = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
];

const HOSTED_RUNNERS: [&str; 6] = [
    "macos-latest",
    "ubuntu-latest",
    "ubuntu-22.04",
    "ubuntu-22.04-arm",
    "windows-latest",
    "${{ matrix.runs-on }}",
];

const PLACEHOLDERS: [&str; 5] = [
    "@VERSION@",
    "@SHA256_MACOS_ARM@",
    "@SHA256_MACOS_INTEL@",
    "@SHA256_LINUX_ARM@",
    "@SHA256_LINUX_INTEL@",
];

fn repo_file(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn workflow() -> String {
    fs::read_to_string(repo_file(".github/workflows/release.yml")).unwrap()
}

fn template() -> String {
    fs::read_to_string(repo_file("packaging/homebrew/kika.rb.tmpl")).unwrap()
}

/// The lines of one top-level job: from `  <name>:` up to the next line at the
/// same or lower indent.
fn job_block(workflow: &str, name: &str) -> String {
    let header = format!("  {name}:");
    let mut lines = workflow.lines().skip_while(|l| l.trim_end() != header);
    let first = lines
        .next()
        .unwrap_or_else(|| panic!("no job `{name}` in release.yml"));
    let body = lines.take_while(|l| l.trim().is_empty() || l.starts_with("   "));
    std::iter::once(first)
        .chain(body)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn release_never_runs_on_self_hosted_or_on_pull_requests() {
    let wf = workflow();
    assert!(!wf.contains("self-hosted"), "release.yml names self-hosted");
    assert!(!wf.contains("pull_request"), "release.yml has a PR trigger");
}

#[test]
fn every_runs_on_is_a_github_hosted_runner() {
    let wf = workflow();
    let values: Vec<&str> = wf
        .lines()
        .filter_map(|l| l.trim().trim_start_matches("- ").strip_prefix("runs-on:"))
        .map(str::trim)
        .collect();
    assert!(!values.is_empty(), "no runs-on in release.yml");
    for value in values {
        assert!(
            HOSTED_RUNNERS.contains(&value),
            "runs-on `{value}` is not an allowed GitHub-hosted runner"
        );
    }
}

#[test]
fn every_target_is_built() {
    let build = job_block(&workflow(), "build");
    for target in TARGETS {
        assert!(build.contains(target), "build job misses {target}");
    }
}

#[test]
fn release_waits_for_every_build_and_build_waits_for_the_version_check() {
    let wf = workflow();
    let needs = |job: &str| {
        job_block(&wf, job)
            .lines()
            .find_map(|l| l.trim().strip_prefix("needs:").map(str::to_owned))
            .unwrap_or_else(|| panic!("job `{job}` has no needs"))
    };
    assert!(needs("release").contains("build"));
    assert!(needs("build").contains("version"));
    assert!(needs("tap").contains("release"));
}

#[test]
fn only_the_tap_job_uses_a_secret_and_it_is_the_tap_token() {
    let wf = workflow();
    let tap = job_block(&wf, "tap");
    let mut seen = 0;
    for (i, _) in wf.match_indices("secrets") {
        let rest = &wf[i..];
        assert!(
            rest.starts_with("secrets.TAP_GITHUB_TOKEN"),
            "unexpected secret reference: {}",
            rest.lines().next().unwrap()
        );
        seen += 1;
    }
    assert!(seen > 0, "the tap token is never used");
    assert_eq!(
        tap.matches("secrets.TAP_GITHUB_TOKEN").count(),
        seen,
        "the tap token is used outside the tap job"
    );
}

#[test]
fn only_one_job_may_write_contents() {
    let wf = workflow();
    assert_eq!(wf.matches("contents: write").count(), 1);
    assert!(job_block(&wf, "release").contains("contents: write"));
}

#[test]
fn template_covers_both_platforms_and_archs() {
    let tmpl = template();
    for needle in PLACEHOLDERS.iter().chain(&[
        "on_macos",
        "on_linux",
        "on_arm",
        "on_intel",
        "license \"MIT OR Apache-2.0\"",
        "depends_on \"git\"",
        "bin.install \"kika\"",
        "--version",
    ]) {
        assert!(tmpl.contains(needle), "template misses {needle}");
    }
}

#[cfg(unix)]
mod render {
    use super::*;
    use std::process::Output;

    const SHAS: [&str; 4] = [
        "1111111111111111111111111111111111111111111111111111111111111111",
        "2222222222222222222222222222222222222222222222222222222222222222",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    ];

    fn render(args: &[&str]) -> Output {
        Command::new("sh")
            .arg(repo_file("packaging/homebrew/render-formula.sh"))
            .args(args)
            .output()
            .unwrap()
    }

    #[test]
    fn renders_a_formula_with_every_placeholder_filled() {
        let out = render(&["1.2.3", SHAS[0], SHAS[1], SHAS[2], SHAS[3]]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let formula = String::from_utf8(out.stdout).unwrap();
        assert!(!formula.contains('@'), "placeholder left:\n{formula}");
        assert_eq!(
            formula
                .lines()
                .filter(|l| l.trim_start().starts_with("sha256 "))
                .count(),
            4
        );
        for sha in SHAS {
            assert!(formula.contains(sha));
        }
        assert!(formula.contains("version \"1.2.3\""));
        assert!(
            formula.contains("releases/download/v1.2.3/kika-1.2.3-aarch64-apple-darwin.tar.gz")
        );

        if Command::new("ruby").arg("--version").output().is_err() {
            eprintln!("note: ruby not on PATH; skipping `ruby -c` on the rendered formula");
            return;
        }
        let path = std::env::temp_dir().join(format!("kika-formula-{}.rb", std::process::id()));
        fs::write(&path, &formula).unwrap();
        let check = Command::new("ruby").arg("-c").arg(&path).output().unwrap();
        let _ = fs::remove_file(&path);
        assert!(
            check.status.success(),
            "ruby -c rejected the formula: {}",
            String::from_utf8_lossy(&check.stderr)
        );
    }

    #[test]
    fn refuses_a_bad_sha() {
        let out = render(&["1.2.3", "not-a-sha", SHAS[1], SHAS[2], SHAS[3]]);
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
    }

    #[test]
    fn refuses_a_bad_version_or_missing_arguments() {
        assert!(
            !render(&["v1.2.3", SHAS[0], SHAS[1], SHAS[2], SHAS[3]])
                .status
                .success()
        );
        assert!(!render(&["1.2.3", SHAS[0]]).status.success());
    }
}
