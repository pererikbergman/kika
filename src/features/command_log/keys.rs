//! The Command log's key bindings: `@` in the workspace opens the log, and
//! the log's own scope moves, pages, filters and closes it. `CHUNKS` in
//! `keymap.rs` places both chunks, so their position there is behaviour.

use crate::intent::AppIntent;
use crate::keymap::{Binding, KeyPattern, KeyScope, NONE, never};
use crossterm::event::{KeyCode, KeyModifiers};

// Chunk 25: command_log
#[rustfmt::skip]
pub(crate) static COMMAND_LOG_WORKSPACE: &[Binding] = &[
    // `@` arrives as itself whatever it takes to type it: Shift on most
    // layouts, AltGr (reported as Ctrl+Alt on Windows) or Option on
    // others, so it matches with any modifiers.
    Binding {
        id: "workspace.command_log",
        keys: &[KeyPattern::any(KeyCode::Char('@'))],
        scope: KeyScope::Workspace,
        intent: AppIntent::OpenCommandLog,
        description: "Show the git commands Kika ran",
        label: "command log",
        bar: false,
        disabled: never,
    },
];

// Chunk 68: command_log
pub(crate) static COMMAND_LOG_KEYS: &[Binding] = &[
    Binding {
        id: "command_log.next",
        keys: &[
            KeyPattern::exact(KeyCode::Char('j'), NONE),
            KeyPattern::any(KeyCode::Down),
        ],
        scope: KeyScope::CommandLog,
        intent: AppIntent::CommandLogNext,
        description: "Select the next command",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "command_log.prev",
        keys: &[
            KeyPattern::exact(KeyCode::Char('k'), NONE),
            KeyPattern::any(KeyCode::Up),
        ],
        scope: KeyScope::CommandLog,
        intent: AppIntent::CommandLogPrev,
        description: "Select the previous command",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "command_log.page_down",
        keys: &[KeyPattern::any(KeyCode::PageDown)],
        scope: KeyScope::CommandLog,
        intent: AppIntent::CommandLogPageDown,
        description: "Move a page down",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "command_log.page_up",
        keys: &[KeyPattern::any(KeyCode::PageUp)],
        scope: KeyScope::CommandLog,
        intent: AppIntent::CommandLogPageUp,
        description: "Move a page up",
        label: "",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "command_log.filter",
        keys: &[KeyPattern::any(KeyCode::Char('/'))],
        scope: KeyScope::CommandLog,
        intent: AppIntent::OpenFilter,
        description: "Filter the commands shown",
        label: "filter",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "command_log.escape",
        keys: &[KeyPattern::any(KeyCode::Esc)],
        scope: KeyScope::CommandLog,
        intent: AppIntent::ListEscape,
        description: "Clear the filter, else close the log",
        label: "clear",
        bar: false,
        disabled: never,
    },
    Binding {
        id: "command_log.close",
        keys: &[KeyPattern::exact(KeyCode::Char('q'), NONE)],
        scope: KeyScope::CommandLog,
        intent: AppIntent::PopPopup,
        description: "Close the log",
        label: "close",
        bar: true,
        disabled: never,
    },
    Binding {
        id: "command_log.quit",
        keys: &[KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL)],
        scope: KeyScope::CommandLog,
        intent: AppIntent::Quit,
        description: "Quit Kika",
        label: "quit",
        bar: true,
        disabled: never,
    },
];

#[cfg(test)]
mod tests {
    use crate::intent::AppIntent;
    use crate::keymap::{
        KeyPattern, KeyScope, Keymap, REPO_LOCKED, Resolution, Trust, parse_key, registry, resolve,
    };
    use crate::model::{AppState, Context};
    use crate::test_support::{binding, build, key, plain};
    use crossterm::event::{KeyCode, KeyModifiers};

    /// `@` is shifted on most layouts and AltGr (Ctrl+Alt on Windows) or
    /// Option on others; crossterm reports the character itself with
    /// whatever modifiers made it, so each of those must open the log.
    #[test]
    fn at_opens_the_command_log_however_it_is_typed_and_clashes_with_nothing() {
        use AppIntent::*;

        let at = KeyCode::Char('@');
        let binding = registry()
            .iter()
            .find(|b| b.id == "workspace.command_log")
            .unwrap();
        assert_eq!(binding.scope, KeyScope::Workspace);
        assert_eq!(binding.intent, OpenCommandLog);
        assert_eq!(binding.keys, [KeyPattern::any(at)]);
        assert_eq!(parse_key("@"), Ok(KeyPattern::any(at)));

        // Nothing in the workspace, its panels or the global scope takes it.
        for other in registry() {
            let reaches_workspace = other.scope.chain().any(|s| s == KeyScope::Workspace)
                || other.scope == KeyScope::Global;
            if other.id == binding.id || !reaches_workspace {
                continue;
            }
            for key in other.keys {
                assert!(!key.overlaps(&KeyPattern::any(at)), "{} takes @", other.id);
            }
        }

        let workspace = AppState::new(Context::Workspace);
        let alt_gr = KeyModifiers::CONTROL | KeyModifiers::ALT;
        let mods = [
            KeyModifiers::NONE,
            KeyModifiers::SHIFT,
            alt_gr,
            KeyModifiers::ALT,
        ];
        for scope in [
            KeyScope::Files,
            KeyScope::Branches,
            KeyScope::Remotes,
            KeyScope::Tags,
            KeyScope::Commits,
            KeyScope::CommitFiles,
            KeyScope::Reflog,
            KeyScope::Status,
            KeyScope::Diff,
            KeyScope::Lines,
            KeyScope::Blame,
            KeyScope::BlameCommit,
            KeyScope::Conflict,
            KeyScope::Workspace,
        ] {
            for mods in mods {
                let found = resolve(
                    registry(),
                    &Keymap::default(),
                    scope,
                    &workspace,
                    key(at, mods),
                );
                assert!(
                    matches!(found, Resolution::Run(b) if b.intent == OpenCommandLog),
                    "{scope:?} {mods:?}"
                );
            }
        }
    }

    #[test]
    fn the_command_log_has_its_own_keys_and_never_reaches_global_ones() {
        use AppIntent::*;

        assert_eq!(KeyScope::CommandLog.parent(), None);
        assert_eq!(KeyScope::CommandLog.title(), "Command log");
        let state = AppState::new(Context::Workspace);
        let none = KeyModifiers::NONE;
        let run = |k| match resolve(
            registry(),
            &Keymap::default(),
            KeyScope::CommandLog,
            &state,
            k,
        ) {
            Resolution::Run(binding) => Some(binding.intent.clone()),
            Resolution::Disabled(reason) => panic!("{k:?} disabled: {reason}"),
            Resolution::Unbound | Resolution::Command(_) => None,
        };
        assert_eq!(run(plain('j')), Some(CommandLogNext));
        assert_eq!(run(key(KeyCode::Down, none)), Some(CommandLogNext));
        assert_eq!(run(plain('k')), Some(CommandLogPrev));
        assert_eq!(run(key(KeyCode::Up, none)), Some(CommandLogPrev));
        assert_eq!(run(key(KeyCode::PageDown, none)), Some(CommandLogPageDown));
        assert_eq!(run(key(KeyCode::PageUp, none)), Some(CommandLogPageUp));
        assert_eq!(run(plain('/')), Some(OpenFilter));
        assert_eq!(run(key(KeyCode::Esc, none)), Some(ListEscape));
        assert_eq!(run(plain('q')), Some(PopPopup));
        assert_eq!(
            run(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Quit)
        );
        for k in [
            plain('?'),
            plain('@'),
            plain('r'),
            key(KeyCode::Enter, none),
        ] {
            assert_eq!(run(k), None, "{k:?}");
        }
        assert!(REPO_LOCKED.contains(&"command_log.quit"));
    }

    #[test]
    fn the_command_log_filter_keys_clash_with_nothing_the_log_reaches() {
        // The log has no parent scope, so only its own keys are reachable.
        assert_eq!(KeyScope::CommandLog.parent(), None);
        for id in ["command_log.filter", "command_log.escape"] {
            let new = binding(id);
            for other in registry()
                .iter()
                .filter(|b| b.id != id && b.scope == KeyScope::CommandLog)
            {
                for a in new.keys {
                    for b in other.keys {
                        assert!(!a.overlaps(b), "{id} {a:?} clashes with {}", other.id);
                    }
                }
            }
        }
    }

    #[test]
    fn the_command_log_escape_is_locked_and_its_filter_is_not() {
        assert!(REPO_LOCKED.contains(&"command_log.escape"));
        assert!(build(&[("command_log.escape", &["Z"])], Trust::Repo).is_err());
        assert!(!REPO_LOCKED.contains(&"command_log.filter"));
        assert!(build(&[("command_log.filter", &["Z"])], Trust::Repo).is_ok());
    }

    #[test]
    fn the_command_log_key_is_typed_into_the_input_and_needs_none_of_the_new_keys() {
        let state = AppState::new(crate::model::Context::Workspace);
        for c in ['@', 'q', '?', '/', 'V', 'n', 'N'] {
            assert!(
                matches!(
                    resolve(
                        registry(),
                        &Keymap::default(),
                        KeyScope::Input,
                        &state,
                        plain(c)
                    ),
                    Resolution::Unbound
                ),
                "{c} is typed"
            );
        }
        let log = registry()
            .iter()
            .filter(|b| b.scope == KeyScope::CommandLog);
        for b in log {
            // `/` filters the log; there is no range or separate search.
            if b.id == "command_log.filter" {
                continue;
            }
            for c in ['/', 'V', 'n', 'N'] {
                assert!(
                    !b.keys.iter().any(|k| k.matches(plain(c))),
                    "{} takes {c}",
                    b.id
                );
            }
        }
    }
}
