//! The repos `update` handlers: the recent list, opening a repository
//! from it, `git init` outside one, and parking the view left behind.

use super::ReposIntent;
use std::path::PathBuf;

use super::{ParkedRepo, RecentList, RecentRow};
use crate::effect::{Effect, Tag};
use crate::git::diff::DiffRequest;
use crate::hunk_view::HunkView;
use crate::intent::AppIntent;
use crate::model::{AppState, Context, Focus};
use crate::popup::{ErrorPopup, Popup};

/// The repos feature's half of `update`: core's one `AppIntent::Repos` arm
/// lands here. A free function, so its name cannot clash with handlers.
pub(crate) fn update(state: &mut AppState, intent: ReposIntent) -> Vec<Effect> {
    match intent {
        ReposIntent::OpenRecent => return state.open_recent(),
        ReposIntent::Loaded { seq, list } => state.recent_loaded(seq, list),
        ReposIntent::Next => state.move_recent_cursor(1),
        ReposIntent::Prev => state.move_recent_cursor(-1),
        ReposIntent::Select => return state.recent_select(),
        ReposIntent::Init => return state.init_repo(),
        ReposIntent::Initialised { tag, result } => {
            return state.repo_initialised(tag, result);
        }
    }
    Vec::new()
}

/// Repos: the recent list, opening, initialising and parking repositories.
impl AppState {
    /// Show the recent list, offering `git init` outside a repository,
    /// and read the list afresh.
    pub(crate) fn open_recent(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        if !matches!(self.popups.last(), Some(Popup::Recent(_))) {
            let list = RecentList::new(self.repo.is_none());
            self.popups.push(Popup::Recent(list));
        }
        self.recent_seq += 1;
        vec![Effect::LoadRecent {
            seq: self.recent_seq,
        }]
    }

    /// Put `path` first in the recent list, as a new request.
    pub(crate) fn record_recent(&mut self, path: PathBuf) -> Effect {
        self.recent_seq += 1;
        Effect::RecordRecent {
            path,
            seq: self.recent_seq,
        }
    }

    /// Show the list a recent-list request answered, unless a newer one
    /// has answered already.
    pub(crate) fn recent_loaded(&mut self, seq: u64, list: Vec<PathBuf>) {
        if seq >= self.recent_shown {
            self.recent = list;
            self.recent_shown = seq;
            self.move_recent_cursor(0);
        }
    }

    /// Whether a recent-list request has yet to answer, so the test
    /// harness waits for it before calling the app idle.
    #[cfg(test)]
    pub(crate) fn recent_pending(&self) -> bool {
        self.recent_shown < self.recent_seq
    }

    /// The open recent list and its rows. The list can change under the
    /// cursor when it is read again, so the rows are worked out each time.
    fn recent_rows(&self) -> Option<(&RecentList, Vec<RecentRow<'_>>)> {
        match self.popups.last() {
            Some(Popup::Recent(list)) => {
                let rows = list.rows(&self.recent, self.repo.as_deref());
                Some((list, rows))
            }
            _ => None,
        }
    }

    pub(crate) fn move_recent_cursor(&mut self, delta: isize) {
        let Some((_, rows)) = self.recent_rows() else {
            return;
        };
        let len = rows.len();
        if let Some(Popup::Recent(list)) = self.popups.last_mut() {
            list.move_cursor(delta, len);
        }
    }

    pub(crate) fn recent_select(&mut self) -> Vec<Effect> {
        let Some((list, rows)) = self.recent_rows() else {
            return vec![];
        };
        match rows.get(list.cursor) {
            Some(RecentRow::Init) => self.init_repo(),
            Some(RecentRow::Repo(path)) => {
                let open = AppIntent::OpenRepo(path.to_path_buf());
                self.update(open)
            }
            None => vec![],
        }
    }

    /// Make the launch directory a repository. Only offered outside one,
    /// where git already runs in the launch directory.
    pub(crate) fn init_repo(&mut self) -> Vec<Effect> {
        if let Some(reason) = super::keys::init_repo_disabled(self) {
            self.notice = Some(reason);
            return vec![];
        }
        let seq = self.take_seq();
        vec![Effect::InitRepo {
            tag: self.user_tag(seq),
        }]
    }

    pub(crate) fn repo_initialised(
        &mut self,
        tag: Tag,
        result: Result<PathBuf, String>,
    ) -> Vec<Effect> {
        self.user_done(tag);
        if tag.generation != self.generation {
            return vec![];
        }
        match result {
            Ok(path) => self.open_repo(path),
            Err(message) => {
                self.push_error(ErrorPopup::new(
                    "Could not initialise a repository",
                    &message,
                    "Close this, fix the cause, then try again.",
                ));
                vec![]
            }
        }
    }

    /// Switch the whole workspace to the repository at `path`. The one
    /// left is parked, so coming back shows its files, selection and pane
    /// at once while a fresh status load catches up.
    pub(crate) fn open_repo(&mut self, path: PathBuf) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        if self.op_is_running() {
            self.notice = Some("Wait for the operation to finish".into());
            return vec![];
        }
        if self.repo.as_ref() == Some(&path) && self.repo_stack.is_empty() {
            self.popups
                .retain(|popup| !matches!(popup, Popup::Recent(_)));
            return vec![];
        }
        let parked = if self.repo.as_ref() == Some(&path) {
            // Inside one of its submodules: back to its top, as it was
            // when the first submodule was entered.
            let stack = std::mem::take(&mut self.repo_stack);
            stack.into_iter().next().map(|top| ParkedRepo {
                repo_stack: Vec::new(),
                files: top.files,
                files_loaded: top.files_loaded,
                branch: top.branch,
                selected: top.selected,
                diff_focused: false,
            })
        } else {
            if let Some(current) = self.repo.take() {
                let here = self.park();
                self.parked.insert(current, here);
            }
            self.parked.remove(&path)
        };
        self.switch_repo();
        self.repo = Some(path.clone());
        let parked = parked.unwrap_or(ParkedRepo {
            repo_stack: Vec::new(),
            files: Vec::new(),
            files_loaded: false,
            branch: None,
            selected: None,
            diff_focused: false,
        });
        self.repo_stack = parked.repo_stack;
        self.files = parked.files;
        self.files_loaded = parked.files_loaded;
        self.branch = parked.branch;
        let set_repo = self.set_repo(&path);
        let mut effects = vec![set_repo, self.record_recent(path)];
        if let Some(index) = parked.selected.filter(|&i| i < self.files.len()) {
            if parked.diff_focused {
                let request = DiffRequest::for_entry(&self.files[index], self.diff_options);
                self.focus_hunk_view(HunkView::new(request.path, request.side));
            }
            effects.extend(self.select(index));
        }
        effects.extend(self.reload_repo());
        // The keys follow the repository: its own file replaces the last one's.
        effects.push(Effect::LoadConfig {
            repo: self.repo.clone(),
        });
        effects
    }

    /// What the current repository shows, taken out of the state to keep.
    fn park(&mut self) -> ParkedRepo {
        ParkedRepo {
            repo_stack: std::mem::take(&mut self.repo_stack),
            files: std::mem::take(&mut self.files),
            files_loaded: self.files_loaded,
            branch: self.branch.take(),
            selected: self.selected.take(),
            diff_focused: self.focus == Focus::Diff,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::effect::{Effect, Tag};
    use crate::features::repos::{RecentList, ReposIntent};
    use crate::git::path::RepoPath;
    use crate::intent::AppIntent;
    use crate::keymap::{KeyPress, KeyScope};
    use crate::menu::{self, MenuId};
    use crate::model::{AppState, Context, Focus, ParentRepo};
    use crate::popup::Popup;
    use crate::test_support::{
        loaded, outside, press, press_char, refresh_tag, select_next, three_files, with_files,
    };
    use crossterm::event::{KeyCode, KeyModifiers};
    use std::path::{Path, PathBuf};

    // Startup and the recent repositories.

    /// Answer the latest recent-list request with `list`.
    fn recent_loaded(state: &mut AppState, list: Vec<PathBuf>) {
        let seq = state.recent_seq;
        state.update(AppIntent::Repos(ReposIntent::Loaded { seq, list }));
    }

    fn recent_list(state: &AppState) -> &RecentList {
        match state.popups.last() {
            Some(Popup::Recent(list)) => list,
            other => panic!("expected the Recent popup, got {other:?}"),
        }
    }

    #[test]
    fn start_in_a_repository_records_it_and_loads_its_status() {
        let mut state =
            AppState::new(Context::Workspace).with_location(Some("/r/a".into()), "/r/a/d".into());
        let effects = state.update(AppIntent::Start);
        match effects.as_slice() {
            [
                Effect::RecordRecent { path, .. },
                Effect::LoadStatus { .. },
                Effect::LoadBranches { .. },
                Effect::LoadRemotes { .. },
                Effect::LoadTags { .. },
                Effect::LoadCommits { skip: 0, .. },
                Effect::LoadMarkers { .. },
                Effect::LoadBranchTips { .. },
                Effect::LoadStashes { .. },
            ] => {
                assert_eq!(path, Path::new("/r/a"));
            }
            other => panic!("expected RecordRecent, LoadStatus and the commits, got {other:?}"),
        }
        assert_eq!(state.popups, []);
    }

    #[test]
    fn start_outside_a_repository_welcomes_with_the_init_row_and_no_status() {
        let mut state = outside("/w");
        let effects = state.update(AppIntent::Start);
        assert!(matches!(effects[..], [Effect::LoadRecent { .. }]));
        assert_eq!(*recent_list(&state), RecentList::new(true));
        assert_eq!(state.key_focus(), KeyScope::Recent);
        assert_eq!(state.busy_count(), 0);
    }

    #[test]
    fn closing_the_welcome_loads_the_status_to_show_gits_error() {
        let mut state = outside("/w");
        state.update(AppIntent::Start);
        let effects = press(&mut state, KeyCode::Esc);
        assert!(
            matches!(effects.as_slice(), [Effect::LoadStatus { .. }]),
            "{effects:?}"
        );
        assert_eq!(state.popups, []);
        assert!(!state.should_quit);
    }

    #[test]
    fn shift_o_opens_the_list_from_files_and_diff_without_the_init_row() {
        let (state, _) = with_files(three_files(), 24);
        let mut state = state.with_location(Some("/r/a".into()), "/r/a".into());
        let o = AppIntent::Key(KeyPress::new(KeyCode::Char('O'), KeyModifiers::SHIFT));
        assert!(matches!(
            state.update(o.clone())[..],
            [Effect::LoadRecent { .. }]
        ));
        assert_eq!(*recent_list(&state), RecentList::new(false));
        press(&mut state, KeyCode::Esc);
        // Closing it in a repository asks for nothing.
        assert_eq!(state.popups, []);

        press(&mut state, KeyCode::Enter);
        assert!(matches!(state.focus, Focus::Diff));
        assert!(matches!(state.update(o)[..], [Effect::LoadRecent { .. }]));
        assert_eq!(state.key_focus(), KeyScope::Recent);
    }

    #[test]
    fn the_actions_menu_opens_the_recent_list() {
        let (mut state, _) = with_files(three_files(), 24);
        state.update(AppIntent::OpenMenu(MenuId::Actions));
        let recent = menu::items(MenuId::Actions)
            .iter()
            .position(|item| item.intent == AppIntent::Repos(ReposIntent::OpenRecent))
            .unwrap();
        for _ in 0..recent {
            state.update(AppIntent::MenuNext);
        }
        assert!(matches!(
            state.update(AppIntent::MenuSelect)[..],
            [Effect::LoadRecent { .. }]
        ));
        assert!(matches!(state.popups[..], [Popup::Recent(_)]));
    }

    #[test]
    fn the_cursor_moves_over_the_rows_and_enter_opens_the_repository_under_it() {
        let mut state = outside("/w");
        state.update(AppIntent::Start);
        recent_loaded(&mut state, vec!["/r/a".into(), "/r/b".into()]);
        press_char(&mut state, 'k');
        for _ in 0..5 {
            press_char(&mut state, 'j');
        }
        assert_eq!(recent_list(&state).cursor, 2);
        press(&mut state, KeyCode::Up);

        let effects = press(&mut state, KeyCode::Enter);

        assert_eq!(state.repo, Some("/r/a".into()));
        assert_eq!(state.popups, []);
        assert_eq!(
            effects[0],
            Effect::SetRepo {
                root: "/r/a".into(),
                nested: vec![]
            }
        );
    }

    #[test]
    fn a_recent_list_answering_an_older_request_does_not_replace_a_newer_one() {
        let mut state =
            AppState::new(Context::Workspace).with_location(Some("/r/a".into()), "/r/a".into());
        let [Effect::RecordRecent { seq: recorded, .. }, ..] = state.update(AppIntent::Start)[..]
        else {
            panic!("expected RecordRecent first");
        };
        let [Effect::LoadRecent { seq: loaded }] =
            state.update(AppIntent::Repos(ReposIntent::OpenRecent))[..]
        else {
            panic!("expected LoadRecent");
        };
        assert!(recorded < loaded);

        let newer: Vec<PathBuf> = vec!["/r/a".into(), "/r/b".into()];
        state.update(AppIntent::Repos(ReposIntent::Loaded {
            seq: loaded,
            list: newer.clone(),
        }));
        state.update(AppIntent::Repos(ReposIntent::Loaded {
            seq: recorded,
            list: vec!["/r/a".into()],
        }));

        assert_eq!(state.recent, newer);
        assert!(!state.recent_pending());
    }

    #[test]
    fn a_shorter_list_pulls_the_cursor_in() {
        let mut state = outside("/w");
        state.update(AppIntent::Start);
        recent_loaded(&mut state, vec!["/r/a".into(), "/r/b".into()]);
        press_char(&mut state, 'j');
        press_char(&mut state, 'j');
        recent_loaded(&mut state, vec![]);
        assert_eq!(recent_list(&state).cursor, 0);
    }

    fn init_tag(effects: &[Effect]) -> Tag {
        match effects {
            [Effect::InitRepo { tag }] => *tag,
            other => panic!("expected one InitRepo, got {other:?}"),
        }
    }

    #[test]
    fn i_or_enter_on_the_init_row_runs_git_init_and_opens_the_result() {
        let mut state = outside("/w");
        state.update(AppIntent::Start);
        let tag = init_tag(&press_char(&mut state, 'i'));
        assert!(tag.user);
        assert_eq!(state.busy_count(), 1);
        // Enter on the init row asks the same, once git is done.
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.notice, Some("Loading…".into()));

        let effects = state.update(AppIntent::Repos(ReposIntent::Initialised {
            tag,
            result: Ok("/w".into()),
        }));

        assert_eq!(
            state.busy_count(),
            4,
            "only the status, branch, remotes and tags loads are left"
        );
        assert_eq!(state.repo, Some("/w".into()));
        assert_eq!(state.popups, []);
        match effects.as_slice() {
            [
                Effect::SetRepo { root, nested },
                Effect::RecordRecent { path, .. },
                Effect::LoadStatus { .. },
                Effect::LoadBranches { .. },
                Effect::LoadRemotes { .. },
                Effect::LoadTags { .. },
                Effect::LoadCommits { skip: 0, .. },
                Effect::LoadMarkers { .. },
                Effect::LoadBranchTips { .. },
                Effect::LoadStashes { .. },
                Effect::LoadConfig { repo },
            ] => {
                assert_eq!((root.as_path(), nested.len()), (Path::new("/w"), 0));
                assert_eq!(path, Path::new("/w"));
                assert_eq!(repo.as_deref(), Some(Path::new("/w")));
            }
            other => {
                panic!("expected SetRepo, RecordRecent, LoadStatus, LoadConfig, got {other:?}")
            }
        }
    }

    #[test]
    fn enter_on_the_init_row_inits_too() {
        let mut state = outside("/w");
        state.update(AppIntent::Start);
        init_tag(&press(&mut state, KeyCode::Enter));
    }

    #[test]
    fn a_failed_init_shows_gits_words_and_stays_outside() {
        let mut state = outside("/w");
        state.update(AppIntent::Start);
        let tag = init_tag(&press_char(&mut state, 'i'));
        let effects = state.update(AppIntent::Repos(ReposIntent::Initialised {
            tag,
            result: Err("fatal: cannot mkdir".into()),
        }));
        assert_eq!(effects, vec![]);
        assert_eq!(state.busy_count(), 0);
        assert_eq!(state.repo, None);
        match state.popups.last() {
            Some(Popup::Error(error)) => assert_eq!(error.message, "fatal: cannot mkdir"),
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[test]
    fn i_is_refused_inside_a_repository() {
        let (state, _) = with_files(three_files(), 24);
        let mut state = state.with_location(Some("/r/a".into()), "/r/a".into());
        state.update(AppIntent::Repos(ReposIntent::OpenRecent));
        assert_eq!(press_char(&mut state, 'i'), vec![]);
        assert_eq!(state.notice, Some("Already in a repository".into()));
    }

    /// Repository `/r/a` with three files loaded, `b.rs` selected.
    fn in_repo_a() -> AppState {
        let (state, _) = with_files(three_files(), 24);
        let mut state = state.with_location(Some("/r/a".into()), "/r/a".into());
        select_next(&mut state);
        state
    }

    #[test]
    fn open_repo_switches_to_a_fresh_repository() {
        let mut state = in_repo_a();
        let generation = state.generation;
        let effects = state.update(AppIntent::OpenRepo("/r/b".into()));

        assert_eq!(state.generation, generation + 1);
        assert_eq!(state.repo, Some("/r/b".into()));
        assert_eq!(state.files, []);
        assert!(!state.files_loaded);
        assert_eq!(state.selected, None);
        assert_eq!(state.focus, Focus::Files);
        match effects.as_slice() {
            [
                Effect::SetRepo { root, nested },
                Effect::RecordRecent { path, .. },
                Effect::LoadStatus { tag },
                Effect::LoadBranches { .. },
                Effect::LoadRemotes { .. },
                Effect::LoadTags { .. },
                Effect::LoadCommits { skip: 0, .. },
                Effect::LoadMarkers { .. },
                Effect::LoadBranchTips { .. },
                Effect::LoadStashes { .. },
                Effect::LoadConfig { repo },
            ] => {
                assert_eq!(repo.as_deref(), Some(Path::new("/r/b")), "its own keys");
                assert_eq!(root, Path::new("/r/b"));
                assert_eq!(nested, &[] as &[RepoPath]);
                assert_eq!(path, Path::new("/r/b"));
                assert_eq!(tag.generation, state.generation);
            }
            other => {
                panic!("expected SetRepo, RecordRecent, LoadStatus, LoadConfig, got {other:?}")
            }
        }
        assert!(state.parked.contains_key(Path::new("/r/a")));
    }

    #[test]
    fn switching_back_restores_the_files_selection_and_diff_focus() {
        let mut state = in_repo_a();
        let files = state.files.clone();
        let branch = state.branch.clone();
        press(&mut state, KeyCode::Enter);
        assert!(matches!(state.focus, Focus::Diff));

        state.update(AppIntent::OpenRepo("/r/b".into()));
        let tag = refresh_tag(&mut state);
        state.update(loaded(tag, &["b.txt"]));
        let effects = state.update(AppIntent::OpenRepo("/r/a".into()));

        // Back before the status lands.
        assert_eq!(state.files, files);
        assert!(state.files_loaded);
        assert_eq!(state.branch, branch);
        assert_eq!(state.selected, Some(1));
        match state.hunk_view() {
            Some(view) => assert_eq!(view.path, "b.rs".into()),
            None => panic!("expected the Diff pane, got {:?}", state.focus),
        }
        assert!(matches!(effects[0], Effect::SetRepo { .. }));
        assert!(matches!(effects[1], Effect::RecordRecent { .. }));
        assert!(
            effects.iter().any(|e| matches!(
                e,
                Effect::LoadPatch { request, .. } if request.path == "b.rs".into()
            )),
            "{effects:?}"
        );
        assert!(matches!(
            effects[effects.len() - 9..],
            [
                Effect::LoadStatus { .. },
                Effect::LoadBranches { .. },
                Effect::LoadRemotes { .. },
                Effect::LoadTags { .. },
                Effect::LoadCommits { skip: 0, .. },
                Effect::LoadMarkers { .. },
                Effect::LoadBranchTips { .. },
                Effect::LoadStashes { .. },
                Effect::LoadConfig { .. }
            ]
        ));
        assert!(!state.parked.contains_key(Path::new("/r/a")));
        assert!(state.parked.contains_key(Path::new("/r/b")));
    }

    #[test]
    fn switching_back_with_files_focused_keeps_the_files_panel() {
        let mut state = in_repo_a();
        state.update(AppIntent::OpenRepo("/r/b".into()));
        let effects = state.update(AppIntent::OpenRepo("/r/a".into()));
        assert_eq!(state.focus, Focus::Files);
        assert_eq!(state.selected, Some(1));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::LoadPatch { .. })),
            "{effects:?}"
        );
    }

    #[test]
    fn switching_back_into_a_submodule_restores_its_path() {
        let mut state = in_repo_a();
        state.repo_stack = vec![ParentRepo {
            path: "sub".into(),
            files: vec![],
            files_loaded: true,
            branch: None,
            selected: None,
        }];
        let stack = state.repo_stack.clone();

        state.update(AppIntent::OpenRepo("/r/b".into()));
        assert_eq!(state.repo_stack, []);
        let effects = state.update(AppIntent::OpenRepo("/r/a".into()));

        assert_eq!(state.repo_stack, stack);
        assert_eq!(
            effects[0],
            Effect::SetRepo {
                root: "/r/a".into(),
                nested: vec!["sub".into()],
            }
        );
    }

    #[test]
    fn opening_the_open_repository_only_closes_the_list() {
        let mut state = in_repo_a();
        state.update(AppIntent::Repos(ReposIntent::OpenRecent));
        let before = AppState {
            popups: vec![],
            ..state.clone()
        };
        assert_eq!(state.update(AppIntent::OpenRepo("/r/a".into())), vec![]);
        assert_eq!(state, before);
    }

    #[test]
    fn opening_the_open_repository_from_a_submodule_goes_back_to_its_top() {
        let mut state = in_repo_a();
        let files = state.files.clone();
        state.repo_stack = vec![ParentRepo {
            path: "sub".into(),
            files: files.clone(),
            files_loaded: true,
            branch: None,
            selected: Some(2),
        }];
        state.files = vec![];
        let effects = state.update(AppIntent::OpenRepo("/r/a".into()));
        assert_eq!(state.repo_stack, []);
        assert_eq!(state.files, files);
        assert_eq!(state.selected, Some(2));
        assert_eq!(
            effects[0],
            Effect::SetRepo {
                root: "/r/a".into(),
                nested: vec![],
            }
        );
    }

    #[test]
    fn open_repo_waits_for_a_running_op() {
        let mut state = in_repo_a();
        state.hold_op(9);
        assert_eq!(state.update(AppIntent::OpenRepo("/r/b".into())), vec![]);
        assert_eq!(
            state.notice,
            Some("Wait for the operation to finish".into())
        );
        assert_eq!(state.repo, Some("/r/a".into()));
    }

    #[test]
    fn a_late_init_result_for_another_generation_only_lowers_busy() {
        let mut state = outside("/w");
        state.update(AppIntent::Start);
        let tag = init_tag(&press_char(&mut state, 'i'));
        let stale = Tag {
            generation: tag.generation + 1,
            ..tag
        };
        let effects = state.update(AppIntent::Repos(ReposIntent::Initialised {
            tag: stale,
            result: Ok("/w".into()),
        }));
        assert_eq!(effects, vec![]);
        assert_eq!(state.busy_count(), 0);
        assert_eq!(state.repo, None);
    }
}
