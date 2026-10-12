//! The renderers' key binding: `R` in the Diff pane steps through the
//! renderers, with its guard. `CHUNKS` in `keymap.rs` places the chunk, so
//! its position there is behaviour.

use super::external_diff::NO_RENDERER;
use crate::intent::AppIntent;
use crate::keymap::{Binding, KeyPattern, KeyScope, Reason};
use crate::model::AppState;
use crossterm::event::KeyCode;

/// With no renderer configured there is nothing to cycle.
fn renderer_disabled(state: &AppState) -> Option<Reason> {
    state.renderers.is_empty().then_some(NO_RENDERER.into())
}

// Chunk 33: renderers
#[rustfmt::skip]
pub(crate) static RENDERERS_DIFF: &[Binding] = &[
    Binding {
        id: "diff.renderer",
        keys: &[KeyPattern::any(KeyCode::Char('R'))],
        scope: KeyScope::Diff,
        intent: AppIntent::CycleRenderer,
        description: "Cycle diff renderer",
        label: "renderer",
        bar: false,
        disabled: renderer_disabled,
    },
];
