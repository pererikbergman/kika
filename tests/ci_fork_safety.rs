//! Guards the CI workflow against running fork PRs on the owner's self-hosted Mac.
//!
//! Once the repository is public, anyone can open a fork PR. Only pushes and PRs from
//! branches in this repository may reach the `[self-hosted, mac]` runner. The workflow
//! is read as text (no YAML parser), so every guard spells the "own" expression out
//! literally. See `docs/ci.md`.

use std::path::Path;

/// True for pushes and for PRs whose head branch lives in this repository. A deleted
/// fork has a null `head.repo`, so it counts as a fork.
const OWN: &str = "github.event_name != 'pull_request' || github.event.pull_request.head.repo.full_name == github.repository";

fn workflow() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/ci.yml");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Lines that are not YAML comments, so a comment cannot satisfy or trip a check.
fn code_lines(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect()
}

#[test]
fn workflow_never_uses_pull_request_target() {
    assert!(
        !workflow().contains("pull_request_target"),
        "pull_request_target runs with repository secrets on fork code"
    );
}

#[test]
fn every_self_hosted_runs_on_is_guarded_by_the_own_expression() {
    let text = workflow();
    let runs_on: Vec<&str> = code_lines(&text)
        .into_iter()
        .filter(|line| line.trim_start().starts_with("runs-on:"))
        .collect();
    assert!(!runs_on.is_empty(), "no runs-on lines found");
    for line in runs_on {
        if line.contains("self-hosted") {
            assert!(
                line.contains(OWN),
                "runs-on reaches the self-hosted runner without the own guard: {line}"
            );
        }
    }
}

#[test]
fn self_hosted_matrix_leg_is_guarded_by_the_own_expression() {
    let text = workflow();
    let matrix_lines: Vec<&str> = code_lines(&text)
        .into_iter()
        .filter(|line| line.contains("macos-self-hosted"))
        .filter(|line| !line.trim_start().starts_with("runs-on:"))
        .filter(|line| !line.trim_start().starts_with("name:"))
        .collect();
    assert!(
        !matrix_lines.is_empty(),
        "no macos-self-hosted matrix leg found"
    );
    for line in matrix_lines {
        assert!(
            line.trim_start().starts_with("os:"),
            "macos-self-hosted appears outside the os matrix: {line}"
        );
        assert!(
            line.contains(OWN),
            "the os matrix offers macos-self-hosted without the own guard: {line}"
        );
        assert!(
            line.contains("macos-latest"),
            "the os matrix has no hosted macOS leg for fork PRs: {line}"
        );
    }
}

#[test]
fn self_hosted_labels_never_appear_as_a_bare_yaml_list() {
    let text = workflow();
    let lines = code_lines(&text);
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let Some(value) = trimmed.strip_prefix("runs-on:") else {
            continue;
        };
        let value = value.trim();
        assert!(
            !value.starts_with('['),
            "runs-on is a bare flow list, so it has no own guard: {line}"
        );
        if value.is_empty() {
            // Block list form: `runs-on:` followed by `- label` lines.
            let next = lines.get(i + 1).map(|l| l.trim_start()).unwrap_or("");
            assert!(
                !next.starts_with('-'),
                "runs-on is a bare block list, so it has no own guard"
            );
        }
    }
}
