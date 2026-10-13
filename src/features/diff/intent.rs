//! The Diff pane's intents. Core wraps them in one variant,
//! `AppIntent::Diff`, and hands them to this feature's `update`, which
//! passes the child `renderers`' own enum on to its `update`
//! (see `docs/adr/0002-nested-intents.md`).

use super::renderers::RenderersIntent;
use crate::effect::Tag;
use crate::git::diff::DiffChunk;
use crate::git::patch::FilePatch;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DiffIntent {
    /// A chunk of the diff stream for `tag`. A stream reports more than once.
    Loaded {
        tag: Tag,
        result: Result<DiffChunk, String>,
    },
    /// The diff for `tag` has taken long enough to show "Loading…".
    Slow {
        tag: Tag,
    },
    /// Move the hunk cursor down or up.
    NextHunk,
    PrevHunk,
    /// Stage the selected hunk, or unstage it in the staged view.
    ToggleHunkStage,
    /// Start or stop picking changed lines inside the selected hunk.
    EnterLineMode,
    ExitLineMode,
    /// Move the line cursor down or up; with `extend`, grow the range.
    NextLine {
        extend: bool,
    },
    PrevLine {
        extend: bool,
    },
    /// Stage the selected lines, or unstage them in the staged view.
    ToggleLinesStage,
    /// Switch the focused Diff pane between unstaged and staged changes.
    ToggleSide,
    /// Show one more unchanged line around each change in the diff.
    MoreContext,
    /// Show one fewer unchanged line around each change in the diff.
    LessContext,
    /// Hide or show changes that only touch whitespace.
    ToggleWhitespace,
    /// A whole-file patch load finished; `tag` says which one.
    PatchLoaded {
        tag: Tag,
        result: Result<FilePatch, String>,
    },
    /// Edit the selected hunk in the user's editor, then stage the result.
    EditHunk,
    /// The editor opened by `OpenInEditor` has closed.
    EditorClosed {
        result: Result<(), String>,
    },
    /// The hunk edit finished: the patch to stage, `None` if the user
    /// cancelled.
    HunkEdited {
        result: Result<Option<Vec<u8>>, String>,
    },
    /// Everything the renderers do; their own `update` handles it.
    Renderers(RenderersIntent),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent::AppIntent;

    #[test]
    fn a_child_intent_reaches_app_intent_through_its_parent() {
        assert_eq!(
            AppIntent::from(DiffIntent::from(RenderersIntent::Cycle)),
            AppIntent::Diff(DiffIntent::Renderers(RenderersIntent::Cycle))
        );
    }
}
