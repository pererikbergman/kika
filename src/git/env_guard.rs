//! A guard on the env seam: the runner's profiles own the standard vars,
//! so no builder may set one by hand. It reads the sources, so a new
//! `.env(..)` or a `const` alias of a var's name fails here, wherever it is.

use std::path::{Path, PathBuf};

/// The vars only [`EnvProfile`](super::runner::EnvProfile) may set.
const STANDARD_VARS: [&str; 3] = ["LC_ALL", "GIT_OPTIONAL_LOCKS", "GIT_LITERAL_PATHSPECS"];

/// Files the guard does not read: end-to-end tests and shared test
/// builders spell the vars out to pin them, the runner owns them, and this
/// file names them to find them.
const SKIPPED: [&str; 4] = [
    "src/e2e/",
    "src/test_support.rs",
    "src/git/runner.rs",
    "src/git/env_guard.rs",
];

/// The only hand-set vars left, each with why a profile does not fit.
const ALLOWED: [(&str, &str, &str); 2] = [
    // MergeTool keeps the user's locale for the tool, yet takes paths.
    ("src/op.rs", "GIT_LITERAL_PATHSPECS", "merge tool"),
    // A fetch may auto-gc, so literal pathspecs are not provably harmless.
    (
        "src/features/review/git.rs",
        "GIT_OPTIONAL_LOCKS",
        "review fetch",
    ),
];

/// Each string literal naming a standard var in `text`'s production part,
/// as its 1-based line and the var. The production part ends at the first
/// `#[cfg(test)]` followed by a `mod tests`, private or not.
fn standard_var_hits(text: &str) -> Vec<(usize, &'static str)> {
    let lines: Vec<&str> = text.lines().collect();
    let end = lines
        .iter()
        .enumerate()
        .position(|(i, line)| {
            line.trim() == "#[cfg(test)]"
                && lines[i + 1..]
                    .iter()
                    .find(|next| !next.trim().is_empty())
                    .is_some_and(|next| is_tests_module(next))
        })
        .unwrap_or(lines.len());
    let mut hits = Vec::new();
    for (i, line) in lines[..end].iter().enumerate() {
        for var in STANDARD_VARS {
            let quoted = format!("\"{var}\"");
            hits.extend(std::iter::repeat_n(
                (i + 1, var),
                line.matches(&quoted).count(),
            ));
        }
    }
    hits
}

/// `mod tests {`, private or visible to the crate.
fn is_tests_module(line: &str) -> bool {
    let line = line.trim_start();
    let line = line.strip_prefix("pub(crate) ").unwrap_or(line);
    line.strip_prefix("pub ")
        .unwrap_or(line)
        .starts_with("mod tests")
}

/// Every `.rs` file under `dir`, with paths relative to the crate root.
fn rust_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(root, &path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path.strip_prefix(root).unwrap().to_path_buf());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_documented_commands_set_a_standard_var_by_hand() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        rust_files(root, &root.join("src"), &mut files);
        let mut found = Vec::new();
        for file in files {
            let name = file.to_string_lossy().replace('\\', "/");
            if SKIPPED.iter().any(|skip| name.starts_with(skip)) {
                continue;
            }
            let text = std::fs::read_to_string(root.join(&file)).unwrap();
            for (line, var) in standard_var_hits(&text) {
                found.push((name.clone(), line, var));
            }
        }
        let mut allowed: Vec<_> = ALLOWED.iter().map(|&(f, v, _)| (f, v)).collect();
        let mut unexpected = Vec::new();
        for (file, line, var) in &found {
            match allowed.iter().position(|&(f, v)| f == file && v == *var) {
                Some(at) => {
                    allowed.remove(at);
                }
                None => unexpected.push(format!("{file}:{line} sets {var}")),
            }
        }
        assert!(
            unexpected.is_empty(),
            "use an env profile instead:\n{}",
            unexpected.join("\n")
        );
        assert!(allowed.is_empty(), "allow-listed but gone: {allowed:?}");
    }

    #[test]
    fn a_hand_set_var_in_production_code_is_found() {
        let text = "fn f() -> GitCommand {\n    GitCommand::new(\"x\")\n        .env(\"LC_ALL\", \"C\")\n}\n";

        assert_eq!(standard_var_hits(text), [(3, "LC_ALL")]);
    }

    #[test]
    fn a_const_alias_is_found() {
        let text = "const LOCKS: &str = \"GIT_OPTIONAL_LOCKS\";\nfn f() {}\n";

        assert_eq!(standard_var_hits(text), [(1, "GIT_OPTIONAL_LOCKS")]);
    }

    #[test]
    fn the_test_module_is_not_read_but_other_test_items_are() {
        let text = "#[cfg(test)]\nconst A: &str = \"LC_ALL\";\n\n#[cfg(test)]\n\nmod tests {\n    const B: &str = \"GIT_LITERAL_PATHSPECS\";\n}\n";

        assert_eq!(standard_var_hits(text), [(2, "LC_ALL")]);
    }

    #[test]
    fn a_test_module_visible_to_the_crate_is_not_read_either() {
        let text =
            "fn f() {}\n#[cfg(test)]\npub(crate) mod tests {\n    const A: &str = \"LC_ALL\";\n}\n";

        assert!(standard_var_hits(text).is_empty());
    }
}
