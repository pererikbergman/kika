//! `o` on the files screen: open the line in view on GitHub's page of the
//! pull request, at its side and number. The review diff has no line
//! cursor, so the line is the selected thread's when its row is in view,
//! else the first numbered row at the top. Nothing is asked of GitHub:
//! the URL is built locally and handed to the configured browser.

use super::files::{FilesStage, PrFiles, ReviewView, files_screen};
use super::state::{NO_BROWSER, NO_FORGE};
use crate::effect::Effect;
use crate::keymap::Reason;
use crate::model::AppState;
use kika_forge::anchor::{self, Placement};
use kika_forge::url::pr_line_url;
use kika_git::patch::LineId;

const NOT_READY: &str = "Open the files first";
const NOT_UTF8: &str = "GitHub cannot be pointed at a file whose name is not UTF-8";
const NO_LINE: &str = "No diff line in view";
const NOT_LINKABLE: &str = "This repository's address cannot be linked";

/// The line to point at in the selected file's diff: the selected line
/// thread's end when its row is in view, else the first row in view with
/// a number. `None` while the pane loads or shows another file.
fn target_line(state: &AppState, files: &PrFiles) -> Option<LineId> {
    let path = &files.selected()?.file.path;
    let diff = &state.diff;
    if diff.pending || diff.path.as_ref() != Some(path) {
        return None;
    }
    let window = diff.scroll..diff.scroll.saturating_add(state.review_diff_rows());
    // A row's number depends only on the rows above it, so the rows up to
    // the window's end are enough, however long the diff.
    let numbers = anchor::row_numbers(&diff.lines[..window.end.min(diff.lines.len())]);
    let thread_line = files.thread.and_then(|t| {
        let discussion = state.review.discussion.as_ref()?;
        if discussion.threads.get(t)?.path.as_bytes() != path.as_bytes() {
            return None;
        }
        let scope = super::threads::ThreadScope::of(files, discussion);
        let Some(Placement::Line { end, .. }) = scope.placement(t) else {
            return None;
        };
        window
            .contains(&anchor::row_in(&numbers, end)?)
            .then_some(end)
    });
    thread_line.or_else(|| {
        numbers
            .get(window.start.min(numbers.len())..)?
            .iter()
            .find_map(|&(old, new)| new.map(LineId::added).or(old.map(LineId::removed)))
    })
}

/// The URL `o` opens, or why there is none, in the order the guard says.
fn line_url(state: &AppState) -> Result<String, Reason> {
    let files = files_screen(state)
        .filter(|f| f.stage == FilesStage::Ready && state.review_files_shown())
        .ok_or(NOT_READY)?;
    if state.browser.is_none() {
        return Err(NO_BROWSER.into());
    }
    let repo = state.review_repo().ok_or(NO_FORGE)?;
    let path = match files.selected() {
        Some(row) => Some(std::str::from_utf8(row.file.path.as_bytes()).map_err(|_| NOT_UTF8)?),
        None => None,
    };
    let (Some(path), Some(line)) = (path, target_line(state, files)) else {
        return Err(NO_LINE.into());
    };
    let commit = match files.view {
        ReviewView::Whole => None,
        ReviewView::Commit(i) => Some(
            &files
                .commits
                .as_ref()
                .and_then(|(commits, _)| commits.get(i))
                .ok_or(NOT_LINKABLE)?
                .range
                .oid,
        ),
    };
    pr_line_url(
        &repo,
        files.number,
        commit,
        path,
        anchor::from_line_id(line),
    )
    .ok_or_else(|| NOT_LINKABLE.into())
}

/// Why `o` cannot open the line on GitHub: the files are not shown, no
/// browser is configured, no forge repository is known, the file's name
/// is not UTF-8, no numbered line is in view, or the address will not
/// make a URL.
pub(crate) fn open_line_disabled(state: &AppState) -> Option<Reason> {
    line_url(state).err()
}

impl AppState {
    /// `o` on the files screen: start the browser on the line in view.
    pub(crate) fn open_review_line(&mut self) -> Vec<Effect> {
        let url = match line_url(self) {
            Ok(url) => url,
            Err(reason) => {
                self.notice = Some(reason);
                return vec![];
            }
        };
        let Some(mut argv) = self.browser.clone() else {
            return vec![];
        };
        // Its own argument, last: never joined into another one. Every
        // part of it was checked, so the notice is safe to draw.
        argv.push(url.clone().into());
        self.notice = Some(format!("Opening {url}").into());
        vec![Effect::OpenUrl { argv }]
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;

    use super::*;
    use crate::effect::Tag;
    use crate::features::review::state::tests::{press, type_char};
    use crate::features::review::threads::tests::{cache_diff, files_with, fixture_discussion};
    use crate::features::review::{PrCommit, ReviewIntent};
    use crate::intent::AppIntent;
    use crate::test_support::diff_chunk;
    use kika_forge::discussion::PrDiscussion;
    use kika_git::commit_files::{CommitId, CommitRange};
    use kika_git::diff::{Classifier, DiffChunk, DiffLine, EmojiZwj};

    const MAIN: &str = "https://github.com/acme/widgets/pull/12/files#diff-42cb6807ad74b3e201c5a7ca98b911c5fa08380e942be6e4ac5807f8377f87fc";

    fn lines(texts: &[&str]) -> Vec<DiffLine> {
        let mut classifier = Classifier::new(EmojiZwj::Escape);
        texts
            .iter()
            .map(|t| classifier.classify(t.as_bytes()))
            .collect()
    }

    /// `src/main.rs`'s diff: an added new line 12 first, then a removed
    /// old line 10 and a context line (old 11, new 13).
    fn main_diff() -> Vec<DiffLine> {
        lines(&[
            "diff --git a/src/main.rs b/src/main.rs",
            "--- a/src/main.rs",
            "+++ b/src/main.rs",
            "@@ -10,2 +12,2 @@",
            "+fn added() {}",
            "-fn removed() {}",
            " fn kept() {}",
        ])
    }

    fn chunk(state: &mut AppState, lines: Vec<DiffLine>) {
        let tag = Tag {
            generation: state.generation,
            seq: state.diff.seq.unwrap(),
            user: false,
        };
        state.update(diff_chunk(tag, DiffChunk { lines, done: true }));
    }

    /// The files screen on `paths`, with `browser = ["open"]`, the first
    /// file's diff streamed in as `diff`.
    fn shown(paths: &[&str], discussion: PrDiscussion, diff: Vec<DiffLine>) -> AppState {
        let mut state = files_with(paths, discussion);
        state.browser = Some(vec!["open".into()]);
        chunk(&mut state, diff);
        state
    }

    fn opened(state: &mut AppState) -> String {
        let effects = state.update(AppIntent::Review(ReviewIntent::OpenLineOnForge));
        match effects.as_slice() {
            [Effect::OpenUrl { argv }] => {
                assert_eq!(argv.len(), 2, "{argv:?}");
                assert_eq!(argv[0], "open");
                argv[1].to_str().unwrap().to_string()
            }
            other => panic!("expected one OpenUrl, got {other:?}"),
        }
    }

    fn refused(state: &mut AppState, reason: &str) {
        assert_eq!(open_line_disabled(state).as_deref(), Some(reason));
        let effects = state.update(AppIntent::Review(ReviewIntent::OpenLineOnForge));
        assert_eq!(effects, []);
        assert_eq!(state.notice.as_deref(), Some(reason));
    }

    #[test]
    fn o_opens_the_top_added_line_as_the_browsers_last_argument() {
        let mut state = shown(&["src/main.rs"], fixture_discussion(), main_diff());
        let url = format!("{MAIN}R12");
        assert_eq!(
            type_char(&mut state, 'o'),
            [Effect::OpenUrl {
                argv: vec!["open".into(), url.clone().into()]
            }]
        );
        assert_eq!(
            state.notice.as_deref(),
            Some(format!("Opening {url}").as_str())
        );
        // With the keys on the diff too.
        press(&mut state, KeyCode::Tab);
        assert_eq!(opened(&mut state), url);
    }

    #[test]
    fn a_removed_row_on_top_gives_the_old_side_and_a_context_row_the_new() {
        let mut state = shown(&["src/main.rs"], fixture_discussion(), main_diff());
        state.diff.scroll = 5;
        assert_eq!(opened(&mut state), format!("{MAIN}L10"));
        state.diff.scroll = 6;
        assert_eq!(opened(&mut state), format!("{MAIN}R13"));
        // Header and hunk rows at the top are skipped.
        state.diff.scroll = 1;
        assert_eq!(opened(&mut state), format!("{MAIN}R12"));
    }

    #[test]
    fn the_selected_thread_is_pointed_at_while_its_row_is_in_view() {
        let mut state = shown(&["src/cache.rs"], fixture_discussion(), cache_diff());
        let cache = "https://github.com/acme/widgets/pull/12/files#diff-";
        // `]` goes to the thread on new line 3, three rows below the top,
        // whose row has line 1.
        type_char(&mut state, ']');
        assert_eq!(state.review.files.as_ref().unwrap().thread, Some(0));
        assert_eq!(state.diff.scroll, 4);
        let url = opened(&mut state);
        assert!(url.starts_with(cache) && url.ends_with("R3"), "{url}");
        // Scrolled past its row, the top numbered row again.
        state.diff.scroll = 8;
        let url = opened(&mut state);
        assert!(url.ends_with("R4"), "{url}");
        // A thread on another file is not this file's line.
        state.diff.scroll = 4;
        state.review.discussion.as_mut().unwrap().threads[0].path = "src/lib.rs".into();
        assert!(opened(&mut state).ends_with("R1"));
    }

    #[test]
    fn a_commit_view_points_at_that_commits_page() {
        let mut state = shown(&["src/main.rs"], fixture_discussion(), main_diff());
        let id = "ab".repeat(20);
        let files = state.review.files.as_mut().unwrap();
        let commit = |oid: &str| PrCommit {
            range: CommitRange {
                oid: CommitId::parse(oid).unwrap(),
                parent: None,
            },
            subject: "s".into(),
        };
        files.commits = Some((vec![commit(&"cd".repeat(20)), commit(&id)], false));
        files.view = ReviewView::Commit(1);
        assert_eq!(
            opened(&mut state),
            format!(
                "https://github.com/acme/widgets/pull/12/commits/{id}#diff-42cb6807ad74b3e201c5a7ca98b911c5fa08380e942be6e4ac5807f8377f87fcR12"
            )
        );
    }

    #[test]
    fn each_reason_it_is_off_is_said_and_nothing_runs() {
        // Back on the detail, the files are not shown.
        let mut state = shown(&["src/main.rs"], fixture_discussion(), main_diff());
        press(&mut state, KeyCode::Esc);
        refused(&mut state, NOT_READY);

        let mut state = shown(&["src/main.rs"], fixture_discussion(), main_diff());
        state.browser = None;
        refused(&mut state, NO_BROWSER);

        let mut state = shown(&["src/main.rs"], fixture_discussion(), main_diff());
        state.forge.repo = None;
        state.review.repo = None;
        refused(&mut state, NO_FORGE);

        // Scrolled past every numbered row.
        let mut state = shown(&["src/main.rs"], fixture_discussion(), main_diff());
        state.diff.scroll = 7;
        refused(&mut state, NO_LINE);

        // A diff still loading, then an empty one.
        let mut state = files_with(&["src/main.rs"], fixture_discussion());
        assert!(state.diff.pending);
        refused(&mut state, NO_LINE);
        chunk(&mut state, Vec::new());
        refused(&mut state, NO_LINE);

        // A diff of header rows only.
        let mut state = shown(&["src/main.rs"], fixture_discussion(), main_diff());
        state.diff.lines.truncate(4);
        refused(&mut state, NO_LINE);

        // A repository address that will not make a URL.
        let mut state = shown(&["src/main.rs"], fixture_discussion(), main_diff());
        state.forge.repo.as_mut().unwrap().owner = "a/b".into();
        refused(&mut state, NOT_LINKABLE);
    }

    #[cfg(unix)]
    #[test]
    fn a_file_name_that_is_not_utf8_cannot_be_linked() {
        let mut state = shown(&["src/main.rs"], fixture_discussion(), main_diff());
        let odd = kika_git::path::RepoPath::from_bytes(b"src/\xff.rs");
        state.review.files.as_mut().unwrap().rows[0].file.path = odd.clone();
        state.diff.path = Some(odd);
        refused(&mut state, NOT_UTF8);
    }
}
