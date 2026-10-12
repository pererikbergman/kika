//! The I/O behind the renderers' effects: finding the renderers the
//! global config and git's global config name, and running one on a diff,
//! each off the event loop.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use super::external_diff::{discover, read_diff, run_renderer};
use super::{ExternalRenderer, RendererKind, ansi, diff_tool};
use crate::effect::Tag;
use crate::git::GitRunner;
use crate::git::diff::{DiffRequest, terminal_safe};
use crate::intent::AppIntent;

/// Find the renderers, `pager` and `tool` from the global config first,
/// and report them as `RenderersFound`.
pub(crate) fn find(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    pager: Option<String>,
    tool: Option<String>,
) {
    tokio::spawn(async move {
        // A panic finds nothing; the built-in view still works.
        let found = tokio::task::spawn_blocking(move || discover(runner.as_ref(), pager, tool))
            .await
            .unwrap_or_default();
        let _ = tx.send(AppIntent::RenderersFound(found));
    });
}

/// Run `renderer` on `request` in `workdir`, `cols` wide, and report its
/// rows as `DiffRendered`.
pub(crate) fn render(
    runner: Arc<dyn GitRunner>,
    tx: UnboundedSender<AppIntent>,
    workdir: PathBuf,
    tag: Tag,
    request: DiffRequest,
    renderer: ExternalRenderer,
    cols: u16,
) {
    let label = renderer.label.clone();
    tokio::spawn(async move {
        // A panic still reports back, or the busy counter would stay raised.
        let result = tokio::task::spawn_blocking(move || {
            let out = match renderer.kind {
                RendererKind::Pager => {
                    let diff = read_diff(runner.as_ref(), &request, &renderer.label)?;
                    run_renderer(&renderer, &workdir, &diff, cols)?
                }
                RendererKind::Tool => {
                    diff_tool::render(runner.as_ref(), &workdir, &request, &renderer, cols)?
                }
            };
            Ok(ansi::parse(&out))
        })
        .await
        .unwrap_or_else(|e| Err(terminal_safe(format!("{label} crashed: {e}").as_bytes())));
        let _ = tx.send(AppIntent::DiffRendered { tag, result });
    });
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::ffi::OsString;

    use tokio::sync::mpsc;

    use super::*;
    use crate::effect::{Effect, Runtime};
    #[cfg(unix)]
    use crate::git::diff::DiffSide;
    use crate::git::fake::{FakeRunner, ok};

    #[cfg(unix)]
    #[tokio::test]
    async fn render_diff_feeds_the_plain_diff_to_the_renderer_and_parses_it() {
        let runner = Arc::new(FakeRunner::new().expect(
            &["diff", "--no-color", "--no-ext-diff", "-U3", "--", "a.rs"],
            ok(b"\x1b[31m-a\x1b[0m\n+b\n"),
        ));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx);
        let tag = Tag {
            generation: 1,
            seq: 8,
            user: true,
        };

        runtime.spawn(Effect::RenderDiff {
            tag,
            request: DiffRequest {
                path: "a.rs".into(),
                orig_path: None,
                side: DiffSide::Worktree,
                options: Default::default(),
                commit: None,
            },
            renderer: ExternalRenderer {
                label: "cat".into(),
                argv: vec!["cat".into()],
                kind: crate::features::diff::renderers::RendererKind::Pager,
            },
            cols: 40,
        });

        match rx.recv().await {
            Some(AppIntent::DiffRendered {
                tag: got,
                result: Ok(rows),
            }) => {
                assert_eq!(got, tag);
                let texts: Vec<String> = rows.iter().map(|r| r.text()).collect();
                assert_eq!(texts, ["-a", "+b"]);
            }
            other => panic!("expected rendered rows, got {other:?}"),
        }
        runner.assert_done();
    }

    /// Run `RenderDiff` with tool `argv` on `request` and take its rows.
    #[cfg(unix)]
    async fn tool_rows(
        runner: Arc<dyn GitRunner>,
        request: DiffRequest,
        argv: &[&str],
    ) -> Vec<String> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner, tx);
        let tag = Tag {
            generation: 1,
            seq: 3,
            user: true,
        };
        runtime.spawn(Effect::RenderDiff {
            tag,
            request,
            renderer: ExternalRenderer {
                label: argv[0].into(),
                argv: argv.iter().map(OsString::from).collect(),
                kind: crate::features::diff::renderers::RendererKind::Tool,
            },
            cols: 40,
        });
        match rx.recv().await {
            Some(AppIntent::DiffRendered {
                tag: got,
                result: Ok(rows),
            }) => {
                assert_eq!(got, tag);
                assert!(rx.try_recv().is_err(), "one answer only");
                rows.iter().map(|r| r.text()).collect()
            }
            other => panic!("expected rendered rows, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn render_diff_runs_a_tool_on_both_sides_of_a_worktree_change() {
        use crate::git::fake::{TempDir, git};
        let tmp = TempDir::new("effect-tool");
        let dir = tmp.path();
        git(dir, &["init", "-q"]);
        std::fs::write(dir.join("a.txt"), "old \x1b]0;title\x07one\n").unwrap();
        git(dir, &["add", "a.txt"]);
        std::fs::write(dir.join("a.txt"), "new \x1b[2J\x08two\n").unwrap();
        let runner: Arc<dyn GitRunner> = Arc::new(crate::git::CliRunner::isolated(dir));
        let request = DiffRequest {
            path: "a.txt".into(),
            orig_path: None,
            side: DiffSide::Worktree,
            options: Default::default(),
            commit: None,
        };

        let rows = tool_rows(Arc::clone(&runner), request.clone(), &["cat"]).await;
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(
            rows[0].starts_with("old ") && rows[0].ends_with("one"),
            "{rows:?}"
        );
        assert!(
            rows[1].starts_with("new ") && rows[1].ends_with("two"),
            "{rows:?}"
        );
        for row in &rows {
            assert!(!row.chars().any(char::is_control), "{row:?}");
        }

        // The tool's arguments name the temp files, gone once it is done.
        let rows = tool_rows(runner, request, &["echo"]).await;
        let args: Vec<&str> = rows[0].split(' ').collect();
        assert_eq!(args.len(), 3, "{rows:?}");
        assert_eq!(args[0], "--");
        let old = std::path::Path::new(args[1]);
        assert_eq!(old.file_name().unwrap(), "old.txt");
        assert!(!old.parent().unwrap().exists(), "{old:?}");
    }

    #[tokio::test]
    async fn find_renderers_offers_the_configured_pager_then_the_global_git_ones() {
        let runner = Arc::new(
            FakeRunner::new()
                .expect(
                    &["config", "--global", "--get", "pager.diff"],
                    ok(b"delta\n"),
                )
                .expect(&["config", "--global", "--get", "core.pager"], ok(b"")),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runtime = Runtime::new(runner.clone(), tx).with_diff_pager(Some("riff".into()));

        runtime.spawn(Effect::FindRenderers);

        match rx.recv().await {
            Some(AppIntent::RenderersFound(found)) => {
                let labels: Vec<&str> = found.iter().map(|r| r.label.as_str()).collect();
                assert_eq!(labels, ["riff", "delta"]);
            }
            other => panic!("expected renderers, got {other:?}"),
        }
        runner.assert_done();
    }
}
