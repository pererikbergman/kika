//! Keeps `docs/configuration.md` honest: its binding table must list every
//! binding in the keymap with its default keys and whether a repository
//! file may change it. A new or re-keyed binding fails here until the page
//! says so, and the failure prints the row to paste. The Commit message
//! rows must also say what the help menu says. Its `[theme]` section
//! must name every colour role and built-in theme, with each theme's colour
//! for each role. The README's Quick start keys must be real default keys,
//! and its Configuration example must load without errors.

use std::collections::{BTreeMap, BTreeSet};

use crossterm::event::KeyCode;

use crate::keymap::{Binding, KeyPattern, KeyScope, REPO_LOCKED, parse_key, registry};
use crate::theme::{self, ColourValue, ROLES, THEMES};

const PAGE: &str = include_str!("../docs/configuration.md");
const README: &str = include_str!("../README.md");

/// One row of the binding table, cells trimmed and backticks kept.
#[derive(Debug)]
struct Row<'a> {
    id: &'a str,
    keys: &'a str,
    action: &'a str,
    lock: &'a str,
}

/// The trimmed cells of a Markdown table row, or `None` for any other line.
fn cells(line: &str) -> Option<Vec<&str>> {
    let inner = line.trim().strip_prefix('|')?.strip_suffix('|')?;
    Some(inner.split('|').map(str::trim).collect())
}

/// Every table row in `page` whose first cell is a backticked id. Other
/// tables and rows, and the headings around them, are ignored.
fn rows(page: &str) -> Vec<Row<'_>> {
    page.lines()
        .filter_map(|line| {
            let cells = cells(line)?;
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

/// Where the page's Action text differs from the help menu's sentence for a
/// binding in `scope`. Checked per scope, because a stale Action row tells a
/// reader the wrong thing even when its keys and lock are right.
fn action_mismatches(page: &str, scope: KeyScope) -> Vec<String> {
    let rows = rows(page);
    registry()
        .iter()
        .filter(|binding| binding.scope == scope)
        .filter_map(|binding| {
            let row = rows.iter().find(|row| row.id == binding.id)?;
            (row.action != binding.description).then(|| {
                format!(
                    "`{}` action says \"{}\"; use this row:\n{}",
                    binding.id,
                    row.action,
                    expected_row(binding)
                )
            })
        })
        .collect()
}

#[test]
fn the_commit_message_rows_say_what_their_bindings_do() {
    let problems = action_mismatches(PAGE, KeyScope::CommitEditor);
    assert!(
        problems.is_empty(),
        "docs/configuration.md's Commit message rows differ from the help menu:\n{}",
        problems.join("\n")
    );
}

#[test]
fn a_stale_commit_message_action_fails_with_the_row_to_paste() {
    let page = page_in_sync();
    assert_eq!(
        action_mismatches(&page, KeyScope::CommitEditor),
        Vec::<String>::new()
    );
    let quit = registry()
        .iter()
        .find(|b| b.id == "commit_editor.quit")
        .unwrap();
    let stale = page.replace(
        &expected_row(quit),
        "| `commit_editor.quit` | `ctrl-c` | Quit Kika, stopping a commit that is running | locked |",
    );
    assert_ne!(stale, page);
    let problems = action_mismatches(&stale, KeyScope::CommitEditor);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].ends_with(&expected_row(quit)),
        "{}",
        problems[0]
    );
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
        "gitlab_ask = [",
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

#[test]
fn the_page_documents_the_clipboard_command() {
    for needle in ["[clipboard]", "command = [", "clipboard.command"] {
        assert!(
            PAGE.contains(needle),
            "docs/configuration.md lacks {needle:?}"
        );
    }
}

/// The page's `[theme]` section: from its heading to the next `## `.
fn theme_section(page: &str) -> &str {
    let start = page
        .find("\n## `[theme]`")
        .expect("docs/configuration.md has a `[theme]` section");
    let rest = &page[start + 1..];
    let end = rest[3..].find("\n## ").map_or(rest.len(), |i| i + 3);
    &rest[..end]
}

/// Every role and theme name the `[theme]` section of `page` does not
/// name as a backticked word, and every role-table cell that does not
/// match its theme.
fn theme_mismatches(page: &str) -> Vec<String> {
    let section = theme_section(page);
    let mut problems: Vec<String> = ROLES
        .iter()
        .chain(THEMES.iter())
        .filter(|name| !section.contains(&format!("`{name}`")))
        .map(|name| format!("`{name}` is not named in the [theme] section"))
        .collect();
    let palettes = THEMES.map(|name| theme::builtin(name).expect("a built-in theme"));
    for line in section.lines() {
        let Some(cells) = cells(line) else {
            continue;
        };
        let [role, _, colours @ ..] = cells.as_slice() else {
            continue;
        };
        let Some(role) = role.strip_prefix('`').and_then(|r| r.strip_suffix('`')) else {
            continue;
        };
        if !ROLES.contains(&role) || colours.len() != THEMES.len() {
            continue;
        }
        for ((cell, palette), name) in colours.iter().zip(&palettes).zip(THEMES) {
            let want = palette.get(role).expect("a role");
            let text = cell.trim_matches('`');
            let got = theme::parse_colour(&ColourValue::Text(text.into()));
            if got != Ok(want) {
                problems.push(format!(
                    "`{role}` in `{name}` is {want:?} but the page says {cell}"
                ));
            }
        }
    }
    problems
}

#[test]
fn the_theme_section_names_every_role_and_theme_with_its_colours() {
    let problems = theme_mismatches(PAGE);
    assert!(
        problems.is_empty(),
        "docs/configuration.md's [theme] section is out of date:\n{}",
        problems.join("\n")
    );
}

#[test]
fn a_missing_role_or_a_wrong_colour_fails() {
    let page = "# x\n\n## `[theme]` (global file only)\n\n`default` `light` `high-contrast`\n| `focused` | f | `cyan` | `25` | `light_cyan` |\n| `added` | a | `red` | `28` | `light_green` |\n\n## `[clipboard]`\n`dim`\n";
    let problems = theme_mismatches(page);
    assert!(
        problems.contains(&"`dim` is not named in the [theme] section".to_string()),
        "{problems:?}"
    );
    assert!(
        !problems.iter().any(|p| p.starts_with("`focused`")),
        "{problems:?}"
    );
    assert!(
        problems.contains(&"`added` in `default` is Green but the page says `red`".to_string()),
        "{problems:?}"
    );
}

/// The text of the `## {title}` section of `page`, up to the next `## `.
fn section<'a>(page: &'a str, title: &str) -> &'a str {
    let heading = format!("## {title}\n");
    let Some(start) = page.find(&heading) else {
        return "";
    };
    let body = &page[start + heading.len()..];
    let end = body.find("\n## ").map_or(body.len(), |i| i + 1);
    &body[..end]
}

/// Every way the Quick start table in `readme` differs from the registry:
/// a row whose binding is unknown, or whose key is not one of its defaults,
/// and a key the section should name but does not.
fn quick_start_mismatches(readme: &str) -> Vec<String> {
    let quick = section(readme, "Quick start");
    let mut problems = Vec::new();
    let mut named = BTreeSet::new();
    for line in quick.lines() {
        let Some(cells) = cells(line) else {
            continue;
        };
        let [key, _, id] = cells.as_slice() else {
            continue;
        };
        let Some(id) = id.strip_prefix('`').and_then(|i| i.strip_suffix('`')) else {
            continue;
        };
        let key = key.trim_matches('`');
        let Some(binding) = registry().iter().find(|b| b.id == id) else {
            problems.push(format!("`{id}` is not a binding: {line}"));
            continue;
        };
        // Compared in the page's spelling: a default such as `q` takes any
        // modifiers, while `parse_key("q")` takes none, yet both are `q`.
        match parse_key(key) {
            Ok(pattern) if binding.keys.iter().any(|k| doc_key(k) == doc_key(&pattern)) => {
                named.insert(key.to_string());
            }
            Ok(_) => problems.push(format!(
                "`{key}` is not a default key of `{id}` (it has {}): {line}",
                keys_cell(binding.keys)
            )),
            Err(err) => problems.push(format!("{err}: {line}")),
        }
    }
    for key in [
        "1", "2", "3", "4", "5", "?", "space", "c", "P", "p", "G", "q",
    ] {
        if !named.contains(key) {
            problems.push(format!("the Quick start table does not list `{key}`"));
        }
    }
    problems
}

#[test]
fn readme_quick_start_keys_are_default_keys_of_their_bindings() {
    let problems = quick_start_mismatches(README);
    assert!(
        problems.is_empty(),
        "README.md's Quick start is out of date:\n{}",
        problems.join("\n")
    );
}

#[test]
fn a_wrong_or_missing_quick_start_key_fails() {
    let readme = "## Quick start\n\n| Key | Does | Binding |\n|---|---|---|\n| `U` | Push | `workspace.push` |\n| `x` | Nothing | `no.such` |\n\n## Next\n| `q` | Quit | `global.quit` |\n";
    let problems = quick_start_mismatches(readme);
    assert!(
        problems
            .iter()
            .any(|p| p.starts_with("`U` is not a default key of `workspace.push`")),
        "{problems:?}"
    );
    assert!(
        problems
            .iter()
            .any(|p| p.starts_with("`no.such` is not a binding")),
        "{problems:?}"
    );
    // A row after the section does not count.
    assert!(
        problems.contains(&"the Quick start table does not list `q`".to_string()),
        "{problems:?}"
    );
}

/// The first ```toml block under `## Configuration` in `readme`.
fn config_example(readme: &str) -> Option<&str> {
    let body = section(readme, "Configuration");
    let start = body.find("```toml\n")? + "```toml\n".len();
    let len = body[start..].find("```")?;
    Some(&body[start..start + len])
}

#[test]
fn readme_config_example_loads_without_errors() {
    let example = config_example(README).expect("a ```toml block under ## Configuration");
    let dir = std::env::temp_dir().join(format!("kika-readme-config-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let global = dir.join("config.toml");
    std::fs::write(&global, example).unwrap();
    let loaded = crate::config::load(&crate::config::ConfigPaths {
        global: Some(global),
        repo: None,
    });
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        loaded.errors.is_empty(),
        "README.md's Configuration example does not load:\n{:?}\n{example}",
        loaded.errors
    );
}
