//! The Custom commands feature's key binding: `ctrl-x` stops a running
//! captured command. `CHUNKS` in `keymap.rs` places the chunk, so its
//! position there is behaviour. The user's own command keys are resolved by
//! the command engine in `keymap.rs`.

use crate::features::custom_commands::CustomCommandsIntent;
use crate::intent::AppIntent;
use crate::keymap::{Binding, KeyPattern, KeyScope, Reason};
use crate::model::AppState;
use crossterm::event::{KeyCode, KeyModifiers};

fn no_command_running(state: &AppState) -> Option<Reason> {
    (!state.command_running).then_some("No command is running".into())
}

// Chunk 8: custom_commands
#[rustfmt::skip]
pub(crate) static CUSTOM_COMMANDS_WORKSPACE: &[Binding] = &[
    Binding {
        id: "workspace.cancel_command",
        keys: &[KeyPattern::exact(KeyCode::Char('x'), KeyModifiers::CONTROL)],
        // Not `esc` or `ctrl-c`, which quit; the bar shows it only while a
        // command runs.
        scope: KeyScope::Workspace,
        intent: AppIntent::CustomCommands(CustomCommandsIntent::Cancel),
        description: "Stop the running command",
        label: "cancel command",
        bar: false,
        disabled: no_command_running,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{Keymap, Resolution, registry, resolve};
    use crate::model::Context;
    use crate::test_support::key;

    #[test]
    fn ctrl_x_cancels_a_running_command_from_every_command_panel() {
        let ctrl_x = key(KeyCode::Char('x'), KeyModifiers::CONTROL);
        let mut state = AppState::new(Context::Workspace);
        for scope in [KeyScope::Files, KeyScope::Diff, KeyScope::Blame] {
            let at =
                |state: &AppState| resolve(registry(), &Keymap::default(), scope, state, ctrl_x);
            state.command_running = false;
            assert!(
                matches!(at(&state), Resolution::Disabled(ref reason) if reason == "No command is running"),
                "{scope:?}"
            );
            state.command_running = true;
            assert!(
                matches!(at(&state), Resolution::Run(b) if b.intent == AppIntent::CustomCommands(CustomCommandsIntent::Cancel)),
                "{scope:?}"
            );
        }
    }
}
