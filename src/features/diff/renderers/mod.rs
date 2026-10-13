//! Diff renderers: the external pagers and the diff tool that draw the
//! Diff pane instead of the built-in view, cycled with `R`. It owns finding
//! them (`external_diff::discover`), running them argv-only on a diff
//! (`external_diff`, `diff_tool`), parsing and sanitising their output
//! (`ansi`), the rendered rows and their map to the patch
//! (`rendered_view`), and drawing them. Chunk 33 (`diff.renderer`) is its
//! key binding, and `Effect::FindRenderers` and `Effect::RenderDiff` are
//! its effects. The programs come only from the global config, which
//! core `config.rs` parses and checks with `external_diff::check_argv`.
//!
//! Its intents are nested (ADR-0002): they live in [`RenderersIntent`],
//! which nests in `DiffIntent::Renderers`, so core's one `AppIntent::Diff`
//! arm reaches [`update`] here through the diff feature's `update`.
//!
//! Its `pub(crate)` API:
//! - the intent enum [`RenderersIntent`] and the free function [`update`];
//! - the types `ExternalRenderer`, `RendererKind`, `RenderedDiff` and
//!   `StyledRow`, re-exported here; core reaches the rest by module path,
//!   such as `external_diff::{NO_RENDERER, kill_group, check_argv,
//!   parse_command, map_rows}` and `ansi::parse`;
//! - the queries `active_renderer`, `rendered_for_shown` and
//!   `rendered_shown`, and the handlers `renderers_found`,
//!   `cycle_renderer`, `render_shown`, `rerender_if_resized`,
//!   `diff_rendered` and `follow_cursor` (`update.rs`);
//! - `keys::RENDERERS_DIFF`, `view::{render_rendered, tool_title}` and
//!   `effects::{find, render}`.
//!
//! Kept cross-feature reads:
//! - `shown_request` reads `files`/`selected`, `commits.open`,
//!   `stash.open` and `diff_options`;
//! - `blame`'s `close_blame` calls `render_if_stale`, and `custom_patch`
//!   calls `remap` and `display_only`. So these three stay in core
//!   `model.rs`, where both features reach them without importing this
//!   one;
//! - `AppState` keeps the fields `renderers`, `renderer` and `rendered`,
//!   which core reads directly.
//!
//! What stays in core: `pane_cols`/`pane_rows`, the renderer branches of
//! the Diff pane's `match` in `view.rs`, the `Scroll`/`Resize`/focus arms
//! of `update`, the `display_only` keymap guard with `DISPLAY_ONLY`, and
//! the `Effect` variants with the `Runtime` fields `diff_pager`/`diff_tool`.

pub(crate) mod ansi;
pub(crate) mod diff_tool;
pub(crate) mod effects;
pub(crate) mod external_diff;
mod intent;
pub(crate) mod keys;
pub(crate) mod rendered_view;
mod update;
pub(crate) mod view;

pub(crate) use intent::RenderersIntent;
pub(crate) use update::update;

pub(crate) use ansi::StyledRow;
pub(crate) use external_diff::{ExternalRenderer, RendererKind};
pub(crate) use rendered_view::RenderedDiff;
