//! The renderers' intents. They nest in `DiffIntent::Renderers`, so core's
//! one `AppIntent::Diff` arm reaches this feature's `update` through
//! the diff feature (see `docs/adr/0002-nested-intents.md`).

use super::{ExternalRenderer, StyledRow};
use crate::effect::Tag;
use crate::features::diff::DiffIntent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RenderersIntent {
    /// Show the diff through the next configured renderer, or the
    /// built-in view after the last.
    Cycle,
    /// The renderers configured for this session, found at boot.
    Found(Vec<ExternalRenderer>),
    /// A renderer's rows for the shown diff; `tag` says which render.
    Rendered {
        tag: Tag,
        result: Result<Vec<StyledRow>, String>,
    },
}

impl From<RenderersIntent> for DiffIntent {
    fn from(intent: RenderersIntent) -> Self {
        Self::Renderers(intent)
    }
}
