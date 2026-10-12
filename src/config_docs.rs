//! Keeps `docs/configuration.md` honest: its binding table must list every
//! binding in the keymap with its default keys and whether a repository
//! file may change it. A new or re-keyed binding fails here until the page
//! says so, and the failure prints the row to paste.

use std::collections::{BTreeMap, BTreeSet};

use crossterm::event::KeyCode;

use crate::keymap::{Binding, KeyPattern, REPO_LOCKED, parse_key, registry};

const PAGE: &str = include_str!("../docs/configuration.md");

/// One row of the binding table, cells trimmed and backticks kept.
#[derive(Debug)]
struct Row<'a> {
    id: &'a str,
    keys: &'a str,
    action: &'a str,
    lock: &'a str,
}

/// Every table row in `page` whose first cell is a backticked id. Other
/// tables and rows, and the headings around them, are ignored.
fn rows(page: &str) -> Vec<Row<'_>> {
    page.lines()
        .filter_map(|line| {
            let inner = line.trim().strip_prefix('|')?.strip_suffix('|')?;
            let cells: Vec<&str> = inner.split('|').map(str::trim).collect();
            let [first, keys, action, lock] = cells.as_slice() else {
                return None;
            };
            let id = first.strip_prefix('`')?.strip_suffix('`')?;
            Some(Row {
                id,
                keys,
                action,
                lock,
            })
        })
        .collect()
}

/// `key` as a config file would write it, so a reader can copy it.
fn doc_key(key: &KeyPattern) -> String {
    let label = key.label();
    let name = match key.code {
        KeyCode::Char(_) => return label,
        KeyCode::Up => "up".to_string(),
        KeyCode::Down => "down".to_string(),
        KeyCode::Left => "left".to_string(),
        KeyCode::Right => "right".to_string(),
        KeyCode::BackTab => "shift-tab".to_string(),
        _ => return label.to_lowercase(),
    };
    // The label ends in an arrow glyph; keep any `ctrl-` before it.
    match label.strip_suffix(|c: char| !c.is_ascii()) {
        Some(prefix) => format!("{prefix}{name}"),
        None => name,
    }
}

/// The keys cell for `keys`: each backticked, in registry order.
fn keys_cell(keys: &[KeyPattern]) -> String {
    if keys.is_empty() {
        return "(none)".to_string();
    }
    keys.iter()
        .map(|k| format!("`{}`", doc_key(k)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn lock_cell(binding: &Binding) -> &'static str {
    if REPO_LOCKED.contains(&binding.id) {
        "locked"
    } else {
        ""
    }
}

/// The row the page should have for `binding`, ready to paste.
fn expected_row(binding: &Binding) -> String {
    let lock = lock_cell(binding);
    let lock = if lock.is_empty() {
        String::new()
    } else {
        format!(" {lock}")
    };
    format!(
        "| `{}` | {} | {} |{lock} |",
        binding.id,
        keys_cell(binding.keys),
        binding.description
    )
}

/// Every way the page's table differs from the registry, one line each.
fn mismatches(page: &str) -> Vec<String> {
    let rows = rows(page);
    let mut problems = Vec::new();
    let mut documented: BTreeMap<&str, &Row> = BTreeMap::new();
    for row in &rows {
        if documented.insert(row.id, row).is_some() {
            problems.push(format!("`{}` is documented more than once", row.id));
        }
    }
    let known: BTreeSet<&str> = registry().iter().map(|b| b.id).collect();
    for id in documented.keys().filter(|id| !known.contains(*id)) {
        problems.push(format!("`{id}` is documented but is not a binding"));
    }
    for binding in registry() {
        let Some(row) = documented.get(binding.id) else {
            problems.push(format!(
                "`{}` is missing; add this row:\n{}",
                binding.id,
                expected_row(binding)
            ));
            continue;
        };
        let keys = keys_cell(binding.keys);
        if row.keys != keys {
            problems.push(format!(
                "`{}` keys are {} but the page says {}",
                binding.id, keys, row.keys
            ));
        }
        let lock = lock_cell(binding);
        if row.lock != lock {
            problems.push(format!(
                "`{}` lock cell should be \"{lock}\" but the page says \"{}\"",
                binding.id, row.lock
            ));
        }
        if row.action.is_empty() {
            problems.push(format!("`{}` has no action text", binding.id));
        }
    }
    problems
}

#[test]
fn every_binding_is_documented_with_its_default_keys_and_lock() {
    let problems = mismatches(PAGE);
    assert!(
        problems.is_empty(),
        "docs/configuration.md is out of date with the keymap:\n{}",
        problems.join("\n")
    );
}

/// A page that matches the registry, so the tests below see one mistake at
/// a time whatever state the real page is in.
fn page_in_sync() -> String {
    let rows: Vec<String> = registry().iter().map(expected_row).collect();
    format!(
        "## Binding reference\n\n| Binding id | Default keys | Action | Repo file |\n|---|---|---|---|\n{}\n",
        rows.join("\n")
    )
}

const QUIT_ROW: &str = "| `global.quit` | `q`, `esc`, `ctrl-c` | Quit Kika | locked |";

#[test]
fn a_missing_binding_fails_with_the_row_to_paste() {
    let page = page_in_sync();
    assert_eq!(mismatches(&page), Vec::<String>::new());
    let without = page.replace(&format!("{QUIT_ROW}\n"), "");
    assert_ne!(without, page);
    let problems = mismatches(&without);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].ends_with(QUIT_ROW), "{}", problems[0]);
}

#[test]
fn a_stray_id_a_changed_key_or_a_flipped_lock_fails() {
    let page = page_in_sync();
    let stray = format!("{page}| `files.nope` | `x` | Nothing | |\n");
    assert_eq!(
        mismatches(&stray),
        ["`files.nope` is documented but is not a binding"]
    );
    let twice = format!("{page}{QUIT_ROW}\n");
    assert_eq!(
        mismatches(&twice),
        ["`global.quit` is documented more than once"]
    );
    let rekeyed = page.replace(
        QUIT_ROW,
        "| `global.quit` | `q`, `ctrl-c` | Quit Kika | locked |",
    );
    assert_eq!(mismatches(&rekeyed).len(), 1);
    let unlocked = page.replace(
        QUIT_ROW,
        "| `global.quit` | `q`, `esc`, `ctrl-c` | Quit Kika | |",
    );
    assert_eq!(mismatches(&unlocked).len(), 1);
    let locked = page.replace(
        "| `global.help` | `?` | Show the keys for this panel | |",
        "| `global.help` | `?` | Show the keys for this panel | locked |",
    );
    assert_eq!(mismatches(&locked).len(), 1);
}

#[test]
fn expected_row_spells_keys_as_a_config_file_does() {
    let quit = registry().iter().find(|b| b.id == "global.quit").unwrap();
    assert_eq!(
        expected_row(quit),
        "| `global.quit` | `q`, `esc`, `ctrl-c` | Quit Kika | locked |"
    );
    let stage = registry().iter().find(|b| b.id == "files.stage").unwrap();
    assert!(
        expected_row(stage).ends_with(" | |"),
        "{}",
        expected_row(stage)
    );
    assert_eq!(keys_cell(&[]), "(none)");
}

#[test]
fn doc_key_writes_every_default_key_so_it_parses_back() {
    for binding in registry() {
        for key in binding.keys {
            let text = doc_key(key);
            let parsed = parse_key(&text)
                .unwrap_or_else(|e| panic!("{}: {text} does not parse: {e}", binding.id));
            assert!(
                parsed.overlaps(key),
                "{}: {text} parses to {parsed:?}, not {key:?}",
                binding.id
            );
        }
    }
}

#[test]
fn every_documented_key_parses_back_to_the_default_it_names() {
    for row in rows(PAGE) {
        let Some(binding) = registry().iter().find(|b| b.id == row.id) else {
            continue;
        };
        if row.keys == "(none)" {
            continue;
        }
        let texts: Vec<&str> = row.keys.split(", ").map(|k| k.trim_matches('`')).collect();
        for (text, default) in texts.iter().zip(binding.keys) {
            let parsed = parse_key(text)
                .unwrap_or_else(|e| panic!("{}: {text} does not parse: {e}", row.id));
            assert!(
                parsed.overlaps(default),
                "{}: {text} parses to {parsed:?}, not {default:?}",
                row.id
            );
        }
    }
}

#[test]
fn the_page_documents_every_forge_key_and_the_pull_request_menu() {
    for needle in [
        "github = [",
        "gitlab = [",
        "browser = [",
        "Open in browser",
        "Create pull request",
    ] {
        assert!(
            PAGE.contains(needle),
            "docs/configuration.md lacks {needle:?}"
        );
    }
}
