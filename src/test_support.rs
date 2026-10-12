//! Shared `#[cfg(test)]` builders for core and feature tests: the helpers
//! that tests in more than one area use. A helper only one feature's tests
//! use stays with that feature.

use crate::commit_editor::CommitEditor;
use crate::effect::{Effect, Tag};
use crate::forge::ForgeHost;
use crate::git::FileEntry;
use crate::git::branches::{Branch, BranchName, Track};
use crate::git::commit_files::{CommitFile, CommitId, CommitRange};
use crate::git::diff::{DiffLine, DiffRequest, LineKind};
use crate::git::log::{CommitRow, LogPage};
use crate::git::patch::{self, FilePatch};
use crate::git::path::RepoPath;
use crate::git::rebase_plan;
use crate::git::remote::{RemoteFailure, RemoteOp};
use crate::git::remotes::{Remote, RemoteBranch, RemoteName};
use crate::git::repo_state::Operation;
use crate::git::stash::{StashEntry, StashFile};
use crate::git::status::{BranchStatus, EntryKind, Head, RepoStatus};
use crate::git::submodule::{Submodule, SubmoduleState};
use crate::handoff::HandOff;
use crate::hunk_view::HunkView;
use crate::intent::AppIntent;
use crate::keymap::{
    Binding, KeyPress, KeyScope, Keymap, KeysTable, Resolution, Trust, registry, resolve,
};
use crate::layout::{self, SidePanel};
use crate::model::*;
use crate::op::{GitOp, ResetMode};
use crate::view::{buffer_rows, render};
use crate::watch::WatchEvent;
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

pub(crate) fn untracked(path: &str) -> FileEntry {
    FileEntry {
        path: path.into(),
        orig_path: None,
        index: '?',
        worktree: '?',
        kind: EntryKind::Untracked,
    }
}

pub(crate) fn on_branch(name: &str) -> BranchStatus {
    BranchStatus {
        head: Head::Branch(name.into()),
        upstream: None,
        ahead_behind: None,
        in_progress: vec![],
        bisect: None,
    }
}

/// A status load of `files` on branch `main`.
pub(crate) fn repo(files: Vec<FileEntry>) -> RepoStatus {
    RepoStatus {
        branch: on_branch("main"),
        files,
    }
}

pub(crate) fn loaded(tag: Tag, paths: &[&str]) -> AppIntent {
    AppIntent::StatusLoaded {
        tag,
        result: Ok(repo(paths.iter().map(|p| untracked(p)).collect())),
    }
}

/// Refresh, as `r` does, and return the status load's tag. The
/// branch list it also reloads is answered at once with nothing, so
/// tests about the status see only that.
pub(crate) fn refresh_tag(state: &mut AppState) -> Tag {
    match state.update(AppIntent::RefreshStatus).as_slice() {
        [
            Effect::LoadStatus { tag },
            Effect::LoadBranches { tag: list },
            Effect::LoadRemotes { tag: remotes },
            Effect::LoadTags { tag: tags },
            Effect::LoadCommits { skip: 0, .. },
            Effect::LoadMarkers { .. },
            Effect::LoadBranchTips { .. },
            Effect::LoadStashes { .. },
        ] => {
            let (tag, list, remotes, tags) = (*tag, *list, *remotes, *tags);
            state.update(AppIntent::BranchesLoaded {
                tag: list,
                result: Ok(vec![]),
            });
            state.update(AppIntent::RemotesLoaded {
                tag: remotes,
                result: Ok(vec![]),
            });
            state.update(AppIntent::TagsLoaded {
                tag: tags,
                result: Ok(vec![]),
            });
            tag
        }
        other => {
            panic!("expected LoadStatus, LoadBranches, LoadRemotes and the commits, got {other:?}")
        }
    }
}

pub(crate) fn changed(path: &str, index: char, worktree: char) -> FileEntry {
    FileEntry {
        path: path.into(),
        orig_path: None,
        index,
        worktree,
        kind: EntryKind::Changed,
    }
}

/// A workspace of `height` rows whose status load listed `files`, with
/// the first file's diff already requested. Returns that request's effects.
pub(crate) fn with_files(files: Vec<FileEntry>, height: u16) -> (AppState, Vec<Effect>) {
    let mut state = AppState::new(Context::Workspace);
    state.update(AppIntent::Resize { width: 80, height });
    let tag = refresh_tag(&mut state);
    let effects = state.update(AppIntent::StatusLoaded {
        tag,
        result: Ok(repo(files)),
    });
    (state, effects)
}

pub(crate) fn three_files() -> Vec<FileEntry> {
    vec![
        changed("a.rs", '.', 'M'),
        changed("b.rs", 'M', '.'),
        untracked("c.txt"),
    ]
}

pub(crate) fn diff_tag(effects: &[Effect]) -> Tag {
    match effects {
        [
            Effect::LoadDiff { tag, .. },
            Effect::DiffLoadingDelay { tag: delay },
        ] => {
            assert_eq!(tag, delay);
            *tag
        }
        other => panic!("expected LoadDiff + DiffLoadingDelay, got {other:?}"),
    }
}

pub(crate) fn line(text: &str) -> DiffLine {
    DiffLine {
        kind: LineKind::Context,
        text: text.into(),
    }
}

pub(crate) fn press(state: &mut AppState, code: KeyCode) -> Vec<Effect> {
    state.update(AppIntent::Key(KeyPress::new(code, KeyModifiers::NONE)))
}

pub(crate) fn op_of(effects: &[Effect]) -> (Tag, GitOp) {
    match effects {
        [Effect::RunOp { tag, op }] => (*tag, op.clone()),
        other => panic!("expected one RunOp, got {other:?}"),
    }
}

/// The commit a submit asked to probe before running it.
pub(crate) fn probe_of(effects: &[Effect]) -> (Tag, GitOp) {
    match effects {
        [Effect::ProbeCommit { tag, op }] => (*tag, op.clone()),
        other => panic!("expected one ProbeCommit, got {other:?}"),
    }
}

pub(crate) fn select_path(state: &mut AppState, path: &str) {
    while state.files[state.selected.unwrap()].path != path.into() {
        state.update(AppIntent::SelectNext);
    }
}

pub(crate) fn finished(tag: Tag, op: GitOp, result: Result<(), String>) -> AppIntent {
    AppIntent::OpFinished { tag, op, result }
}

pub(crate) fn ctrl(state: &mut AppState, c: char) -> Vec<Effect> {
    state.update(AppIntent::Key(KeyPress::new(
        KeyCode::Char(c),
        KeyModifiers::CONTROL,
    )))
}

pub(crate) fn type_text(state: &mut AppState, text: &str) {
    for c in text.chars() {
        let code = if c == '\n' {
            KeyCode::Enter
        } else {
            KeyCode::Char(c)
        };
        press(state, code);
    }
}

pub(crate) fn editor(state: &AppState) -> &CommitEditor {
    match &state.commit {
        CommitFlow::Editing(editor) => editor,
        other => panic!("expected Editing, got {other:?}"),
    }
}

/// A workspace with a staged file, the commit popup open on it.
pub(crate) fn editing() -> AppState {
    let (mut state, _) = with_files(three_files(), 24);
    press(&mut state, KeyCode::Char('c'));
    editor(&state);
    state
}

pub(crate) const TWO_HUNKS: &[u8] = b"diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n\
    @@ -1,2 +1,3 @@\n-a\n+b\n+c\n d\n\
    @@ -9 +10 @@\n-e\n+f\n";

pub(crate) fn patch_of(bytes: &[u8]) -> FilePatch {
    patch::parse(bytes).unwrap()
}

/// The one `LoadPatch` among `effects`.
pub(crate) fn patch_load(effects: &[Effect]) -> (Tag, DiffRequest) {
    let loads: Vec<_> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::LoadPatch { tag, request } => Some((*tag, request.clone())),
            _ => None,
        })
        .collect();
    match loads.as_slice() {
        [load] => load.clone(),
        other => panic!("expected one LoadPatch in {effects:?}, got {other:?}"),
    }
}

pub(crate) fn view(state: &AppState) -> &HunkView {
    match &state.focus {
        Focus::Diff(view) => view,
        other => panic!("expected diff focus, got {other:?}"),
    }
}

/// `files` loaded, the first selected, Enter pressed and its patch
/// answered with `bytes`.
pub(crate) fn focused(files: Vec<FileEntry>, bytes: &[u8]) -> AppState {
    let (mut state, _) = with_files(files, 24);
    let (tag, _) = patch_load(&press(&mut state, KeyCode::Enter));
    state.update(AppIntent::PatchLoaded {
        tag,
        result: Ok(patch_of(bytes)),
    });
    state
}

pub(crate) const HEAD: &str = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n";

/// The `LoadDiff` request among `effects`, checking it comes with its delay.
pub(crate) fn diff_request(effects: &[Effect]) -> DiffRequest {
    diff_tag(&effects[..2]);
    match &effects[0] {
        Effect::LoadDiff { request, .. } => request.clone(),
        other => panic!("expected LoadDiff, got {other:?}"),
    }
}

pub(crate) fn press_char(state: &mut AppState, c: char) -> Vec<Effect> {
    press(state, KeyCode::Char(c))
}

pub(crate) fn blame_line(oid: &str, line_no: usize) -> crate::git::blame::BlameLine {
    crate::git::blame::BlameLine {
        oid: oid.into(),
        author: "Ann".into(),
        date: "2024-01-02".into(),
        line_no,
        text: format!("line {line_no}"),
    }
}

pub(crate) const OID: &str = "abcdef1234567890abcdef1234567890abcdef12";

/// Press `b` on a tracked, modified `a.rs` in a `height`-row terminal.
pub(crate) fn blaming(height: u16) -> (AppState, Tag) {
    let (mut state, _) = with_files(vec![changed("a.rs", '.', 'M')], height);
    let effects = press(&mut state, KeyCode::Char('b'));
    match effects.as_slice() {
        [Effect::LoadBlame { tag, path }] => {
            assert_eq!(path, &RepoPath::from("a.rs"));
            assert!(tag.user);
            let tag = *tag;
            (state, tag)
        }
        other => panic!("expected one LoadBlame, got {other:?}"),
    }
}

/// A blame of `n` lines, all by commit `OID` but the uncommitted last.
pub(crate) fn blamed(n: usize, height: u16) -> AppState {
    let (mut state, tag) = blaming(height);
    let zero = "0".repeat(40);
    let lines = (1..=n)
        .map(|i| blame_line(if i == n { &zero } else { OID }, i))
        .collect();
    state.update(AppIntent::BlameLoaded {
        tag,
        result: Ok(lines),
    });
    state
}

pub(crate) fn blame(state: &AppState) -> &crate::features::blame::BlameView {
    match &state.focus {
        Focus::Blame(view) => view,
        other => panic!("expected the blame focused, got {other:?}"),
    }
}

pub(crate) fn submodule(path: &str, state: SubmoduleState) -> Submodule {
    Submodule {
        path: path.into(),
        oid: "1234567890abcdef".into(),
        state,
    }
}

pub(crate) fn submodules_tag(effects: &[Effect]) -> Tag {
    match effects {
        [Effect::LoadSubmodules { tag }] => *tag,
        other => panic!("expected one LoadSubmodules, got {other:?}"),
    }
}

/// Three files with `b.rs` selected, and the Submodules popup open
/// listing `subs`.
pub(crate) fn with_submodules(subs: Vec<Submodule>) -> AppState {
    let (state, _) = with_files(three_files(), 24);
    let mut state = state.with_location(Some(TOP.into()), TOP.into());
    state.update(AppIntent::SelectNext);
    let tag = submodules_tag(&press_char(&mut state, 'S'));
    state.update(AppIntent::SubmodulesLoaded {
        tag,
        result: Ok(subs),
    });
    state
}

/// The top level the submodule tests run in.
pub(crate) const TOP: &str = "/r/top";

pub(crate) fn outside(launch: &str) -> AppState {
    let mut state = AppState::new(Context::Workspace).with_location(None, launch.into());
    state.update(AppIntent::Resize {
        width: 80,
        height: 24,
    });
    state
}

/// `n` commits named `c<from>`, `c<from+1>`, …, each the parent's child.
pub(crate) fn page(from: usize, n: usize, done: bool) -> LogPage {
    let rows = (from..from + n)
        .map(|i| CommitRow {
            oid: format!("c{i}"),
            short_oid: format!("c{i}"),
            parents: vec![format!("c{}", i + 1)],
            author: "Ann".into(),
            time: 0,
            subject: format!("commit {i}"),
            subject_altered: false,
        })
        .collect();
    LogPage { rows, done }
}

/// The one page load in `effects`: its tag, tip and skip.
pub(crate) fn commits_load(effects: &[Effect]) -> (Tag, Option<String>, usize) {
    let loads: Vec<_> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::LoadCommits { tag, tip, skip, .. } => Some((*tag, tip.clone(), *skip)),
            _ => None,
        })
        .collect();
    match loads.as_slice() {
        [load] => load.clone(),
        other => panic!("expected one LoadCommits, got {other:?} in {effects:?}"),
    }
}

pub(crate) fn commits_loaded(tag: Tag, skip: usize, page: LogPage) -> AppIntent {
    AppIntent::CommitsLoaded {
        tag,
        skip,
        result: Ok((page, 1_000)),
    }
}

/// Started in a repository with the first page of `first` commits in.
pub(crate) fn with_commits(first: usize, done: bool) -> AppState {
    let mut state = AppState::new(Context::Workspace).with_location(Some(TOP.into()), TOP.into());
    let effects = state.update(AppIntent::Start);
    let (tag, _, _) = commits_load(&effects);
    state.update(commits_loaded(tag, 0, page(0, first, done)));
    state.focus = Focus::Commits;
    state.expanded = SidePanel::Commits;
    state
}

/// `three_files` in the worktree, eight commits in the log, and the
/// Commits panel focused with the cursor on `c5`.
pub(crate) fn on_commit_five() -> AppState {
    let (mut state, _) = with_files(three_files(), 24);
    state.commits.begin_page(900, 0);
    state
        .commits
        .page_loaded(900, 0, Ok((page(0, 8, true), 1_000)));
    state.focus = Focus::Commits;
    state.expanded = SidePanel::Commits;
    state.commits.selected = 5;
    state
}

pub(crate) fn files_load(effects: &[Effect]) -> (Tag, CommitRange) {
    match effects {
        [Effect::LoadCommitFiles { tag, range }] => (*tag, range.clone()),
        other => panic!("expected one LoadCommitFiles, got {other:?}"),
    }
}

pub(crate) fn range_of(oid: &str, parent: &str) -> CommitRange {
    CommitRange {
        oid: CommitId::parse(oid).unwrap(),
        parent: CommitId::parse(parent),
    }
}

pub(crate) fn commit_file(status: char, path: &str, orig: Option<&str>) -> CommitFile {
    CommitFile {
        status,
        path: path.into(),
        orig_path: orig.map(Into::into),
    }
}

pub(crate) fn two_commit_files() -> Vec<CommitFile> {
    vec![
        commit_file('M', "src/lib.rs", None),
        commit_file('R', "new.rs", Some("old.rs")),
    ]
}

/// Open `c5` and answer with `files`; returns the effects of the answer.
pub(crate) fn opened(state: &mut AppState, files: Vec<CommitFile>) -> Vec<Effect> {
    let (tag, _) = files_load(&press(state, KeyCode::Enter));
    state.update(AppIntent::CommitFilesLoaded {
        tag,
        result: Ok(files),
    })
}

pub(crate) fn watch(state: &mut AppState, event: WatchEvent) -> Vec<Effect> {
    state.update(AppIntent::Watch(event))
}

/// The tag of the one background status load in `effects`.
pub(crate) fn background_status_tag(effects: &[Effect]) -> Tag {
    match effects {
        [Effect::LoadStatus { tag }] => {
            assert!(!tag.user, "background loads stay out of the busy count");
            *tag
        }
        other => panic!("expected one background LoadStatus, got {other:?}"),
    }
}

/// An idle workspace showing `three_files` with its diff settled.
pub(crate) fn idle_with_files() -> AppState {
    let (state, _) = with_files(three_files(), 24);
    let state = state.with_location(Some("/r/a".into()), "/r/a".into());
    assert_eq!(state.busy, 0);
    state
}

pub(crate) fn git_branch(name: &str, head: bool, merged: bool) -> Branch {
    Branch {
        name: name.into(),
        head,
        upstream: None,
        track: Track::None,
        merged,
    }
}

/// `main` (current), `topic` (merged) and `wip` (not merged).
pub(crate) fn three_branches() -> Vec<Branch> {
    vec![
        git_branch("main", true, true),
        git_branch("topic", false, true),
        git_branch("wip", false, false),
    ]
}

/// The tag of the one `LoadBranches` in `effects`.
pub(crate) fn branches_tag(effects: &[Effect]) -> Tag {
    let found = effects.iter().find_map(|e| match e {
        Effect::LoadBranches { tag } => Some(*tag),
        _ => None,
    });
    found.unwrap_or_else(|| panic!("expected LoadBranches in {effects:?}"))
}

/// The `LoadBranchLog` in `effects`, if any, with the full ref it reads.
pub(crate) fn log_load(effects: &[Effect]) -> Option<(Tag, Vec<u8>)> {
    effects.iter().find_map(|e| match e {
        Effect::LoadBranchLog { tag, refname } => Some((*tag, refname.clone())),
        _ => None,
    })
}

pub(crate) fn branches_loaded(tag: Tag, branches: Vec<Branch>) -> AppIntent {
    AppIntent::BranchesLoaded {
        tag,
        result: Ok(branches),
    }
}

/// A workspace with `three_branches` loaded and the Branches panel
/// focused, its first log load answered.
pub(crate) fn with_branches() -> AppState {
    let mut state = AppState::new(Context::Workspace);
    state.update(AppIntent::Resize {
        width: 80,
        height: 24,
    });
    let tag = branches_tag(&state.update(AppIntent::RefreshStatus));
    let effects = state.update(branches_loaded(tag, three_branches()));
    let (tag, refname) = log_load(&effects).expect("the selection's log loads");
    state.update(AppIntent::BranchLogLoaded {
        tag,
        refname,
        result: Ok(vec![]),
    });
    state.update(AppIntent::FocusPanel(layout::Panel::Side(
        SidePanel::Branches,
    )));
    state
}

pub(crate) fn selected_name(state: &AppState) -> String {
    state.branches.selected_branch().unwrap().name.display()
}

pub(crate) fn git_remote(name: &str, configured: bool, branches: &[&str]) -> Remote {
    Remote {
        name: name.into(),
        url: configured.then(|| format!("/srv/{name}")),
        configured,
        branches: branches
            .iter()
            .map(|short| RemoteBranch {
                remote: RemoteName::from(name),
                short: short.as_bytes().to_vec(),
                refname: format!("refs/remotes/{name}/{short}").into_bytes(),
            })
            .collect(),
    }
}

/// The tag of the one `LoadRemotes` in `effects`.
pub(crate) fn remotes_tag(effects: &[Effect]) -> Tag {
    let found = effects.iter().find_map(|e| match e {
        Effect::LoadRemotes { tag } => Some(*tag),
        _ => None,
    });
    found.unwrap_or_else(|| panic!("expected LoadRemotes in {effects:?}"))
}

/// Reload both lists, answering with `remotes`.
pub(crate) fn reload_remotes(state: &mut AppState, remotes: Vec<Remote>) -> Vec<Effect> {
    let tag = remotes_tag(&state.update(AppIntent::RefreshStatus));
    state.update(AppIntent::RemotesLoaded {
        tag,
        result: Ok(remotes),
    })
}

/// The one attached hand-off in `effects`.
pub(crate) fn attached_of(effects: &[Effect]) -> (Tag, GitOp) {
    match effects {
        [Effect::HandOff(HandOff::Attached { tag, op })] => (*tag, op.clone()),
        other => panic!("expected one attached hand-off, got {other:?}"),
    }
}

/// `with_branches`, with `ops` in progress.
pub(crate) fn in_progress(ops: &[Operation]) -> AppState {
    let mut state = with_branches();
    let mut branch = on_branch("main");
    branch.in_progress = ops.to_vec();
    state.branch = Some(branch);
    state
}

/// `in_progress`, with the Status panel focused.
pub(crate) fn status_focused(ops: &[Operation]) -> AppState {
    let mut state = in_progress(ops);
    state.update(AppIntent::FocusPanel(layout::Panel::Side(
        SidePanel::Status,
    )));
    assert_eq!(state.key_focus(), KeyScope::Status);
    state
}

pub(crate) fn conflicted(path: &str) -> FileEntry {
    FileEntry {
        path: path.into(),
        orig_path: None,
        index: 'U',
        worktree: 'U',
        kind: EntryKind::Conflicted,
    }
}

pub(crate) fn cid(text: &str) -> CommitId {
    CommitId::parse(text).unwrap()
}

/// `with_commits`, on the second commit, `c1`.
pub(crate) fn on_commit() -> AppState {
    let mut state = with_commits(3, true);
    press_char(&mut state, 'j');
    assert_eq!(state.key_focus(), KeyScope::Commits);
    state
}

/// `on_commit`, with `HEAD` detached on `c0`.
pub(crate) fn detached_on_commit() -> AppState {
    let mut state = on_commit();
    state.branch = Some(BranchStatus {
        head: Head::Detached("c0".into()),
        upstream: None,
        ahead_behind: None,
        in_progress: vec![],
        bisect: None,
    });
    state
}

/// The one left-behind check in `effects`: its tag and planned op,
/// after checking it reads from `c0` to `c1`.
pub(crate) fn check_of(effects: &[Effect]) -> (Tag, GitOp) {
    match effects {
        [
            Effect::CheckLeftBehind {
                tag,
                op,
                head,
                target,
            },
        ] => {
            assert_eq!((head, target), (&cid("c0"), &cid("c1")));
            (*tag, op.clone())
        }
        other => panic!("expected one CheckLeftBehind, got {other:?}"),
    }
}

pub(crate) fn reset_op(mode: ResetMode) -> GitOp {
    GitOp::Reset {
        to: cid("c1"),
        mode,
    }
}

/// Each move a detached `HEAD` checks first, as its intent and op.
pub(crate) fn moves() -> [(AppIntent, GitOp); 4] {
    [
        (AppIntent::CheckoutCommit, GitOp::CheckoutCommit(cid("c1"))),
        (
            AppIntent::ResetTo(ResetMode::Soft),
            reset_op(ResetMode::Soft),
        ),
        (
            AppIntent::ResetTo(ResetMode::Mixed),
            reset_op(ResetMode::Mixed),
        ),
        (
            AppIntent::ResetTo(ResetMode::Hard),
            reset_op(ResetMode::Hard),
        ),
    ]
}

/// `detached_on_commit` after `intent`'s check came back with `result`.
pub(crate) fn checked(
    intent: AppIntent,
    result: Result<Vec<CommitId>, String>,
) -> (AppState, Vec<Effect>) {
    let mut state = detached_on_commit();
    let (tag, op) = check_of(&state.update(intent));
    let effects = state.update(AppIntent::LeftBehindChecked {
        tag,
        op,
        head: cid("c0"),
        result,
    });
    (state, effects)
}

pub(crate) fn ids(n: usize) -> Vec<CommitId> {
    (0..n).map(|i| cid(&format!("a{i}"))).collect()
}

pub(crate) const COPIED_OID: &str = "1111111111111111111111111111111111111111";

/// `on_commit`, on `main` with a clean tree, the selected row a commit
/// with a full id and a short one.
pub(crate) fn copy_ready() -> AppState {
    let mut state = on_commit();
    let row = &mut state.commits.rows[1];
    row.oid = COPIED_OID.into();
    // Not what the notice names: that is made from the full id.
    row.short_oid = "111".into();
    state.branch = Some(on_branch("main"));
    state
}

/// A repository with `three_branches`, on `main`, nothing running.
pub(crate) fn remote_ready() -> AppState {
    let mut state = with_branches().with_location(Some("/r".into()), "/r".into());
    state.branch = Some(on_branch("main"));
    state.busy = 0;
    state.status_seq = None;
    state
}

/// The tag and op of the one `Remote` effect in `effects`.
pub(crate) fn remote_of(effects: &[Effect]) -> (Tag, RemoteOp) {
    match effects {
        [Effect::Remote { tag, op }] => (*tag, op.clone()),
        other => panic!("expected one Remote effect, got {other:?}"),
    }
}

pub(crate) fn shift(state: &mut AppState, c: char) -> Vec<Effect> {
    state.update(AppIntent::Key(KeyPress::new(
        KeyCode::Char(c),
        KeyModifiers::SHIFT,
    )))
}

pub(crate) fn remote_finished(
    tag: Tag,
    op: RemoteOp,
    result: Result<(), RemoteFailure>,
) -> AppIntent {
    AppIntent::RemoteFinished { tag, op, result }
}

/// `with_commits(3)` with full ids on one line, `aaa…` at `HEAD`, the
/// last loaded commit on `ddd…`, and `bbb…` selected.
pub(crate) fn on_linear_log() -> AppState {
    let mut state = on_commit();
    let ids = ['a', 'b', 'c', 'd'].map(|d| d.to_string().repeat(40));
    for (i, row) in state.commits.rows.iter_mut().enumerate() {
        row.oid = ids[i].clone();
        row.parents = vec![ids[i + 1].clone()];
    }
    // No other branch; an unknown git, so no `--update-refs`.
    state.commits.tips = Some(vec![]);
    state
}

pub(crate) fn rebase_step(action: rebase_plan::TodoAction, digit: char) -> rebase_plan::Step {
    rebase_plan::Step {
        action,
        commit: cid(&digit.to_string().repeat(40)),
        break_after: false,
    }
}

pub(crate) fn press_with(state: &mut AppState, c: char, modifiers: KeyModifiers) -> Vec<Effect> {
    state.update(AppIntent::Key(KeyPress::new(KeyCode::Char(c), modifiers)))
}

/// `state` as git leaves it once a bisect has begun.
pub(crate) fn start_bisecting(state: &mut AppState) {
    let branch = state.branch.get_or_insert_with(|| on_branch("main"));
    branch.in_progress.push(Operation::Bisect);
}

pub(crate) fn stash_entry(index: usize, oid: &str, subject: &str) -> StashEntry {
    StashEntry {
        index,
        oid: cid(oid),
        base: cid("ba5e"),
        untracked: (oid == "a1").then(|| cid("d1")),
        subject: subject.into(),
    }
}

pub(crate) fn three_stashes() -> Vec<StashEntry> {
    vec![
        stash_entry(0, "a0", "WIP on main: 1234 zero"),
        stash_entry(1, "a1", "On main: one"),
        stash_entry(2, "a2", "On main: two"),
    ]
}

/// Reload the stash list and answer with `entries`.
pub(crate) fn load_stashes(state: &mut AppState, entries: Vec<StashEntry>) {
    let tag = match state.reload_stashes().as_slice() {
        [Effect::LoadStashes { tag }] => *tag,
        other => panic!("expected one LoadStashes, got {other:?}"),
    };
    state.update(AppIntent::StashesLoaded {
        tag,
        result: Ok(entries),
    });
}

/// `three_files` in the worktree, `three_stashes` in the list, the
/// Stash panel focused with the cursor on `stash@{1}`.
pub(crate) fn on_stash_one() -> AppState {
    let (mut state, _) = with_files(three_files(), 24);
    load_stashes(&mut state, three_stashes());
    press_char(&mut state, '5');
    press_char(&mut state, 'j');
    assert_eq!(state.key_focus(), KeyScope::Stash);
    state
}

pub(crate) fn stash_files_load(effects: &[Effect]) -> (Tag, StashEntry) {
    match effects {
        [Effect::LoadStashFiles { tag, entry }] => (*tag, entry.clone()),
        other => panic!("expected one LoadStashFiles, got {other:?}"),
    }
}

pub(crate) fn tracked_and_untracked() -> Vec<StashFile> {
    vec![
        StashFile {
            range: range_of("a1", "ba5e"),
            file: commit_file('M', "src/lib.rs", None),
        },
        StashFile {
            range: CommitRange {
                oid: cid("d1"),
                parent: None,
            },
            file: commit_file('A', "notes.txt", None),
        },
    ]
}

/// Open `stash@{1}` and answer with its two files.
pub(crate) fn stash_opened(state: &mut AppState) -> Vec<Effect> {
    let (tag, _) = stash_files_load(&press(state, KeyCode::Enter));
    state.update(AppIntent::StashFilesLoaded {
        tag,
        result: Ok(tracked_and_untracked()),
    })
}

/// The Branches panel drawn on its own, focused, for checks that only
/// care what it shows.
pub(crate) fn draw_branches(state: &AppState, width: u16, height: u16) -> Vec<String> {
    let mut state = state.clone();
    state.files_loaded = true;
    state.focus = Focus::Branches;
    state.expanded = SidePanel::Branches;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| render(&state, frame)).unwrap();
    buffer_rows(terminal.backend().buffer())
}

/// Whether the row holding `needle` is drawn reversed where it starts.
pub(crate) fn reversed_at(buffer: &ratatui::buffer::Buffer, needle: &str) -> bool {
    let rows = buffer_rows(buffer);
    let y = rows
        .iter()
        .position(|r| r.contains(needle))
        .unwrap_or_else(|| panic!("{needle} in {rows:#?}"));
    let x = rows[y].find(needle).map(|b| rows[y][..b].chars().count());
    buffer[(x.unwrap() as u16, y as u16)]
        .style()
        .add_modifier
        .contains(ratatui::style::Modifier::REVERSED)
}

/// A workspace on github.com with `branches` listed, the Branches
/// panel shown, and no load started.
pub(crate) fn forge_state(branches: &[BranchName]) -> AppState {
    let mut state = AppState::new(Context::Workspace)
        .with_forge_hosts(vec![ForgeHost::parse("github.com").unwrap()]);
    let listed = branches
        .iter()
        .map(|b| {
            let name = String::from_utf8_lossy(&b.0);
            git_branch(&name, name == "main", true)
        })
        .collect();
    state.branches.loaded(Ok(listed));
    state.forge.set_branches(branches);
    state.focus = Focus::Branches;
    state.expanded = SidePanel::Branches;
    state.files_loaded = true;
    state
}

pub(crate) fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyPress {
    KeyPress::new(code, modifiers)
}

pub(crate) fn plain(c: char) -> KeyPress {
    key(KeyCode::Char(c), KeyModifiers::NONE)
}

pub(crate) fn layer(entries: &[(&str, &[&str])]) -> KeysTable {
    entries
        .iter()
        .map(|(id, keys)| (id.to_string(), keys.iter().map(|k| k.to_string()).collect()))
        .collect()
}

pub(crate) fn build(
    entries: &[(&str, &[&str])],
    trust: Trust,
) -> Result<(Keymap, Vec<String>), String> {
    Keymap::build(&Keymap::default(), &layer(entries), trust)
}

pub(crate) fn binding(id: &str) -> &'static Binding {
    registry().iter().find(|b| b.id == id).expect("known id")
}

pub(crate) fn from(focus: KeyScope, key: KeyPress) -> Option<AppIntent> {
    // Every binding is enabled on a workspace with a stageable file
    // selected, a commit in the log and a stash in the list.
    let mut state = AppState::new(Context::Workspace);
    state.stash.entries = vec![crate::git::stash::StashEntry {
        index: 0,
        oid: crate::git::commit_files::CommitId::parse("a0").unwrap(),
        base: crate::git::commit_files::CommitId::parse("b0").unwrap(),
        untracked: None,
        subject: "On main: wip".into(),
    }];
    state.files = vec![crate::git::FileEntry {
        path: "a.rs".into(),
        orig_path: None,
        index: '.',
        worktree: 'M',
        kind: EntryKind::Changed,
    }];
    state.selected = Some(0);
    state.commits.rows = vec![crate::git::log::CommitRow {
        oid: "abc".into(),
        short_oid: "abc".into(),
        parents: vec![],
        author: "Ann".into(),
        time: 0,
        subject: "s".into(),
        subject_altered: false,
    }];
    match resolve(registry(), &Keymap::default(), focus, &state, key) {
        Resolution::Run(binding) => Some(binding.intent.clone()),
        Resolution::Disabled(reason) => panic!("{key:?} disabled: {reason}"),
        Resolution::Unbound | Resolution::Command(_) => None,
    }
}

/// Screen helpers for view tests: draw a state into a test terminal and
/// find text in it, plus the states the view tests draw. Apart from the
/// model-test builders above, as some share their names.
#[cfg(test)]
pub(crate) mod screen {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::features::blame::BlameView;
    use crate::git::FileEntry;
    use crate::git::blame::BlameLine;
    use crate::git::diff::{DiffLine, LineKind};
    use crate::git::status::{BranchStatus, EntryKind, Head};
    use crate::model::{AppState, Context, Focus};
    use crate::view::{buffer_rows, render};

    /// Draw into a `width` x `height` buffer and return its rows as text.
    pub(crate) fn draw(state: &AppState, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(state, frame)).unwrap();
        buffer_rows(terminal.backend().buffer())
    }

    pub(crate) fn contains(rows: &[String], needle: &str) -> bool {
        rows.iter().any(|row| row.contains(needle))
    }

    pub(crate) fn file(path: &str) -> FileEntry {
        FileEntry {
            path: path.into(),
            orig_path: None,
            index: '.',
            worktree: 'M',
            kind: EntryKind::Changed,
        }
    }

    /// A workspace after a status load of `files` on branch `main`.
    pub(crate) fn loaded(files: Vec<FileEntry>) -> AppState {
        let mut state = AppState::new(Context::Workspace);
        state.files_loaded = true;
        state.files = files;
        state.branch = Some(branch(Head::Branch("main".into())));
        state
    }

    pub(crate) fn branch(head: Head) -> BranchStatus {
        BranchStatus {
            head,
            upstream: None,
            ahead_behind: None,
            in_progress: vec![],
            bisect: None,
        }
    }

    pub(crate) fn draw_buffer(
        state: &AppState,
        width: u16,
        height: u16,
    ) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(state, frame)).unwrap();
        terminal.backend().buffer().clone()
    }

    pub(crate) fn diff_line(kind: LineKind, text: &str) -> DiffLine {
        DiffLine {
            kind,
            text: text.into(),
        }
    }

    /// Where `needle` starts on screen, by cell.
    pub(crate) fn find(buffer: &ratatui::buffer::Buffer, needle: &str) -> (u16, u16) {
        let first = needle.chars().next().unwrap().to_string();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                let rest: String = (x..buffer.area.width)
                    .take(needle.chars().count())
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect();
                if buffer[(x, y)].symbol() == first && rest == needle {
                    return (x, y);
                }
            }
        }
        panic!("{needle:?} not on screen");
    }

    /// No cell holds part of an emoji sequence: each holds all of `family`
    /// or none of it, and no cell starts with a joiner or a virama.
    pub(crate) fn assert_no_split_cluster(
        buffer: &ratatui::buffer::Buffer,
        family: &str,
        at: &str,
    ) {
        let parts: Vec<char> = family.chars().filter(|&c| c != '\u{200D}').collect();
        for cell in buffer.content() {
            let symbol = cell.symbol();
            let lead = symbol.chars().next();
            assert!(
                !matches!(lead, Some('\u{200C}' | '\u{200D}' | '\u{094D}')),
                "{at}: a cell starts with {lead:?}"
            );
            if symbol.chars().any(|c| parts.contains(&c)) {
                assert_eq!(symbol, family, "{at}: a split sequence");
            }
        }
    }

    pub(crate) fn blame_line(oid: &str, author: &str, line_no: usize, text: &str) -> BlameLine {
        BlameLine {
            oid: oid.into(),
            author: author.into(),
            date: "2024-01-02".into(),
            line_no,
            text: text.into(),
        }
    }

    /// A workspace showing a loaded blame of `a.rs` with `lines`.
    pub(crate) fn blamed(lines: Vec<BlameLine>, cursor: usize) -> AppState {
        let mut state = loaded(vec![file("a.rs")]);
        state.selected = Some(0);
        let mut view = BlameView::new("a.rs".into(), 0);
        view.loaded(Ok(lines));
        view.cursor = cursor;
        state.focus = Focus::Blame(view);
        state
    }
}
