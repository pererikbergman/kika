//! Runs the app headless: the real model, effect runtime and git runner over
//! a [`TestRepo`], drawn on a `TestBackend`, with keys injected the way the
//! terminal would send them. Hand-offs run as the loop runs them, minus the
//! terminal, so a scripted editor can stand in for the user's; a failed
//! commit on the terminal does not wait for Enter.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Modifier;
use ratatui::widgets::Block;
use tokio::sync::mpsc::{self, UnboundedReceiver};

use crate::app;
use crate::clipboard::RecordingSink;
use crate::e2e::repo::TestRepo;
use crate::effect::Runtime;
use crate::features::diff::DiffIntent;
use crate::features::diff::renderers::ExternalRenderer;
use crate::features::diff::renderers::RenderersIntent;
use crate::handoff;
use crate::intent::AppIntent;
use crate::layout::{self, Panel, SidePanel};
use crate::model::{AppState, Context};
use crate::view;
use kika_forge::ForgeHost;
use kika_forge::github::FakeGh;
use kika_forge::gitlab::FakeGlab;
use kika_forge::task::ForgeContext;
use kika_git::recording::CommandSink;
use kika_git::repo_root::find_toplevel;
use kika_git::version;
use kika_git::{CliRunner, GitRunner};

/// How long `wait_idle` lets the app go without news before calling it a
/// hang. It bounds a failure; a passing test never waits this long.
const HANG_TIMEOUT: Duration = Duration::from_secs(10);

/// The app in a test. Every step that feeds it input returns only once it
/// is idle, so assertions never race the effects they caused.
pub struct Harness {
    state: AppState,
    rx: UnboundedReceiver<AppIntent>,
    runtime: Runtime,
    runner: Arc<CliRunner>,
    terminal: Terminal<TestBackend>,
    /// Boot's renderer discovery has reported. It is not a user effect,
    /// so the busy count does not cover it.
    renderers_found: bool,
    /// What git runs as the sequence editor of Kika's interactive rebase.
    /// The test binary is not Kika, so a script runs it as Kika's hidden
    /// mode instead (see [`use_test_binary_as_sequence_editor`]).
    sequence_editor: Option<std::path::PathBuf>,
    /// What the app sent to the clipboard, in place of the terminal.
    clipboard: Arc<RecordingSink>,
}

impl Harness {
    /// Start on an 80x24 screen and wait for the first load and diff.
    pub async fn start(repo: &TestRepo) -> Self {
        Self::start_sized(repo, 80, 24).await
    }

    pub async fn start_sized(repo: &TestRepo, width: u16, height: u16) -> Self {
        Self::start_with(repo.runner(), width, height).await
    }

    /// Start as if Kika were launched in `<repo>/<sub>`, on an 80x24 screen.
    pub async fn start_from(repo: &TestRepo, sub: &str) -> Self {
        Self::start_with(repo.runner_from(sub), 80, 24).await
    }

    /// Start over any runner, such as one resolved outside a repository.
    pub async fn start_with(runner: CliRunner, width: u16, height: u16) -> Self {
        Self::start_inner(runner, width, height, None).await
    }

    /// Start with `gh` standing in for GitHub on github.com. The host is
    /// configured only once the first load is done, so starting asks
    /// GitHub nothing, and no CLI check runs.
    pub async fn start_with_forge(repo: &TestRepo, gh: Arc<FakeGh>) -> Self {
        let github = vec![ForgeHost::parse("github.com").unwrap()];
        Self::start_with_forge_hosts(repo, gh, github).await
    }

    /// As [`start_with_forge`](Self::start_with_forge), with `hosts`
    /// configured instead: none means the forge is off.
    pub async fn start_with_forge_hosts(
        repo: &TestRepo,
        gh: Arc<FakeGh>,
        hosts: Vec<ForgeHost>,
    ) -> Self {
        Self::start_with_runners(repo, gh, Arc::new(FakeGlab::new()), hosts).await
    }

    /// Start with `glab` standing in for GitLab on `host`, a `gitlab_ask`
    /// host, configured as [`start_with_forge`](Self::start_with_forge)
    /// configures GitHub's.
    pub async fn start_with_glab(repo: &TestRepo, glab: Arc<FakeGlab>, host: &str) -> Self {
        let hosts = vec![ForgeHost::parse_gitlab(host).unwrap()];
        Self::start_with_runners(repo, Arc::new(FakeGh::new()), glab, hosts).await
    }

    async fn start_with_runners(
        repo: &TestRepo,
        gh: Arc<FakeGh>,
        glab: Arc<FakeGlab>,
        hosts: Vec<ForgeHost>,
    ) -> Self {
        let forge = ForgeContext {
            gh,
            glab,
            sink: CommandSink::new(),
            cache_dir: None,
            cli: kika_forge::cli::CliGate::default(),
        };
        let mut harness = Self::start_inner(repo.runner(), 120, 30, Some(forge)).await;
        harness.state.forge.hosts = hosts;
        // No check ran at start, as no host was configured then: count it
        // as answered with every kind open, so a refresh may run. `B`
        // still checks for real.
        harness.state.forge.cli_checked([]);
        harness
    }

    async fn start_inner(
        runner: CliRunner,
        width: u16,
        height: u16,
        forge: Option<ForgeContext>,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let runner = Arc::new(runner);
        let clipboard = Arc::new(RecordingSink::default());
        let runtime = Runtime::new(runner.clone(), tx)
            .with_clipboard(clipboard.clone())
            .with_forge(forge);
        // As `main` does, so submodule paths start from the right root.
        let launch = runner.workdir();
        let repo = find_toplevel(runner.as_ref());
        // As `main` does, so features that need a newer git are gated alike.
        let git_version = version::check(runner.as_ref()).ok().flatten();
        let mut harness = Self {
            state: AppState::new(Context::Workspace)
                .with_location(repo, launch)
                .with_git_version(git_version),
            rx,
            runtime,
            runner,
            terminal: Terminal::new(TestBackend::new(width, height)).unwrap(),
            renderers_found: false,
            sequence_editor: None,
            clipboard,
        };
        app::boot(&mut harness.state, &harness.runtime, width, height);
        harness.wait_idle().await;
        harness
    }

    pub async fn press(&mut self, code: KeyCode) {
        self.press_with(code, KeyModifiers::NONE).await;
    }

    /// Send a key through the same mapping real input takes, then wait for idle.
    pub async fn press_with(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        self.send(code, modifiers);
        self.wait_idle().await;
    }

    /// Send a key without waiting for idle.
    fn send(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        let event = Event::Key(KeyEvent::new(code, modifiers));
        if let Some(intent) = AppIntent::from_event(event) {
            self.dispatch(intent);
        }
    }

    /// Offer `renderers` instead of the ones the host has configured.
    // Only `#[cfg(unix)]` tests call it so far.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub async fn use_renderers(&mut self, renderers: Vec<ExternalRenderer>) {
        self.dispatch(AppIntent::Diff(DiffIntent::Renderers(
            RenderersIntent::Found(renderers),
        )));
        self.wait_idle().await;
    }

    /// Dispatch one intent, such as a timer tick, then wait for idle.
    pub async fn intent(&mut self, intent: AppIntent) {
        self.dispatch(intent);
        self.wait_idle().await;
    }

    /// Apply `intent` as the loop does, running any hand-off it asks for
    /// to the end before going on.
    fn dispatch(&mut self, intent: AppIntent) {
        if matches!(
            intent,
            AppIntent::Diff(DiffIntent::Renderers(RenderersIntent::Found(_)))
        ) {
            self.renderers_found = true;
        }
        let mut jobs = VecDeque::from(app::dispatch(&mut self.state, &self.runtime, intent));
        let editor = self.sequence_editor.clone();
        let exe = move || {
            editor.clone().ok_or_else(|| {
                std::io::Error::other("call use_test_binary_as_sequence_editor first")
            })
        };
        while let Some(job) = jobs.pop_front() {
            let workdir = self.runner.workdir();
            let intent = handoff::execute_as(job, self.runner.as_ref(), &workdir, &|| {}, &exe);
            jobs.extend(app::dispatch(&mut self.state, &self.runtime, intent));
        }
    }

    /// Let an interactive rebase run: git starts a script in `dir` that
    /// runs this test binary's [`hidden_sequence_editor`] test, which is
    /// Kika's hidden sequence-editor mode, on the todo file.
    #[cfg(unix)]
    pub fn use_test_binary_as_sequence_editor(&mut self, dir: &std::path::Path) {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::PermissionsExt;

        let exe = std::env::current_exe().unwrap();
        let quoted = crate::sequence_editor::editor_value(&exe);
        let mut body = b"#!/bin/sh\nKIKA_E2E_TODO=\"$1\" exec ".to_vec();
        body.extend_from_slice(quoted.as_bytes());
        body.extend_from_slice(
            b" --exact e2e::harness::hidden_sequence_editor --quiet --test-threads=1 >/dev/null\n",
        );
        let script = dir.join("kika-sequence-editor.sh");
        std::fs::write(&script, body).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        self.sequence_editor = Some(script);
    }

    /// Type `text` key by key, as a terminal sends it: uppercase with
    /// SHIFT, and `\n` as Enter.
    pub async fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            match c {
                '\n' => self.press(KeyCode::Enter).await,
                c if c.is_uppercase() => {
                    self.press_with(KeyCode::Char(c), KeyModifiers::SHIFT).await
                }
                c => self.press(KeyCode::Char(c)).await,
            }
        }
    }

    /// Draw the current state and return the screen, one string per row.
    pub fn screen(&mut self) -> Vec<String> {
        self.draw();
        view::buffer_rows(self.terminal.backend().buffer())
    }

    /// The trimmed text of the highlighted row in the Files panel, if any.
    pub fn selected_line(&mut self) -> Option<String> {
        self.draw();
        let buffer = self.terminal.backend().buffer();
        // The same split `view::render` makes, down to the panel's inside.
        let [main, _] = layout::screen_layout(buffer.area);
        let state = &self.state;
        let files =
            layout::workspace_layout(main, state.focus.panel(), state.expanded, state.screen_mode)
                .rect(Panel::Side(SidePanel::Files))?;
        let inner = Block::bordered().inner(files);
        (inner.top()..inner.bottom())
            .find(|&y| {
                buffer[(inner.x, y)]
                    .style()
                    .add_modifier
                    .contains(Modifier::REVERSED)
            })
            .map(|y| {
                view::row_text(buffer, y, inner.left()..inner.right())
                    .trim()
                    .to_string()
            })
    }

    /// Fail with the whole screen when `needle` is not on it.
    pub fn assert_screen_contains(&mut self, needle: &str) {
        let screen = self.screen();
        assert!(
            screen.iter().any(|row| row.contains(needle)),
            "{needle:?} not on screen:\n{}",
            screen.join("\n")
        );
    }

    /// Every OSC 52 sequence sent to the clipboard, oldest first.
    pub fn clipboard(&self) -> Vec<Vec<u8>> {
        self.clipboard.copies()
    }

    pub fn state(&self) -> &AppState {
        &self.state
    }

    fn draw(&mut self) {
        let state = &self.state;
        self.terminal
            .draw(|frame| view::render(state, frame))
            .unwrap();
    }

    /// Feed effect results back in until the app is idle. Idle is read from
    /// the state, never guessed from elapsed time.
    async fn wait_idle(&mut self) {
        loop {
            while let Ok(intent) = self.rx.try_recv() {
                self.dispatch(intent);
            }
            if is_idle(&self.state) && self.renderers_found {
                return;
            }
            match tokio::time::timeout(HANG_TIMEOUT, self.rx.recv()).await {
                Ok(Some(intent)) => self.dispatch(intent),
                // The runtime holds a sender, so the channel cannot close.
                Ok(None) => unreachable!("the runtime keeps the channel open"),
                Err(_) => self.hang(),
            }
        }
    }

    fn hang(&mut self) -> ! {
        let screen = self.screen().join("\n");
        let state = &self.state;
        let diff = &state.diff;
        panic!(
            "app not idle after {HANG_TIMEOUT:?}: busy {}, op_seq {:?}, diff seq {:?} \
             pending {} done {} requested {} lines {}\n{screen}",
            state.busy_count(),
            state.held_seq(),
            diff.seq,
            diff.pending,
            diff.done,
            diff.requested,
            diff.lines.len(),
        );
    }
}

/// Whether nothing more will change without input: no status load or Git
/// operation in flight, no background or recent-list load pending, the
/// diff settled, no commits page, markers, reflog or stash list on
/// their way, and no forge refresh or CLI check running.
/// Diff streams and background loads stay out of the busy count, so they are read
/// directly.
pub(crate) fn is_idle(state: &AppState) -> bool {
    let diff = &state.diff;
    // A stream that has sent all it was asked for is paused until the model
    // demands more, so nothing arrives on its own.
    let diff_settled =
        diff.seq.is_none() || (!diff.pending && (diff.done || diff.lines.len() >= diff.requested));
    !state.is_busy()
        && !state.op_is_running()
        && !state.watch.is_pending()
        && !state.recent_pending()
        && diff_settled
        && state.commits.list.seq.is_none()
        && state.commits.markers_seq.is_none()
        && state.commits.tips_seq.is_none()
        && state.commits.todo_seq.is_none()
        && state.stash.list.seq.is_none()
        && state.commits.reflog.list.seq.is_none()
        && !state.review.loading()
        && !state.forge.in_flight
        && !state.forge.checking
}

/// Kika's hidden sequence-editor mode, for the script
/// [`Harness::use_test_binary_as_sequence_editor`] writes. Run as part of
/// the suite it has no todo file and does nothing.
#[cfg(unix)]
#[test]
fn hidden_sequence_editor() {
    let (Some(todo), Some(_)) = (
        std::env::var_os("KIKA_E2E_TODO"),
        std::env::var_os(kika_git::rebase_plan::PLAN_ENV),
    ) else {
        return;
    };
    if crate::sequence_editor::dispatch_with(&[todo]) != std::process::ExitCode::SUCCESS {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::repos::ReposIntent;
    use kika_git::diff::{DiffLine, LineKind};

    /// A state with a diff stream for request 1 that has asked for 10 lines.
    fn streaming() -> AppState {
        let mut state = AppState::new(Context::Workspace);
        state.diff.seq = Some(1);
        state.diff.requested = 10;
        state
    }

    fn lines(n: usize) -> Vec<DiffLine> {
        let line = DiffLine {
            kind: LineKind::Context,
            text: "x".into(),
        };
        vec![line; n]
    }

    #[test]
    fn a_fresh_state_with_no_diff_requested_is_idle() {
        assert!(is_idle(&AppState::new(Context::Workspace)));
    }

    #[test]
    fn a_status_load_or_op_in_flight_is_not_idle() {
        let mut state = AppState::new(Context::Workspace);
        state.set_busy(1);
        assert!(!is_idle(&state));

        let mut state = AppState::new(Context::Workspace);
        state.hold_op(3);
        assert!(!is_idle(&state));
    }

    #[test]
    fn a_recent_list_still_on_its_way_is_not_idle() {
        let mut state = AppState::new(Context::Workspace);
        state.update(AppIntent::Repos(ReposIntent::OpenRecent));
        assert!(!is_idle(&state));

        let seq = state.recent_seq;
        state.update(AppIntent::Repos(ReposIntent::Loaded { seq, list: vec![] }));
        assert!(is_idle(&state));
    }

    #[test]
    fn a_pending_background_load_is_not_idle() {
        let mut state = AppState::new(Context::Workspace);
        state.watch.status_pending = true;
        assert!(!is_idle(&state));
    }

    #[test]
    fn a_diff_waiting_for_its_first_chunk_is_not_idle() {
        let mut state = streaming();
        state.diff.pending = true;
        assert!(!is_idle(&state));
    }

    #[test]
    fn a_stream_short_of_what_it_asked_for_is_not_idle() {
        let mut state = streaming();
        state.diff.lines = lines(4);
        assert!(!is_idle(&state));
    }

    #[test]
    fn a_finished_diff_is_idle() {
        let mut state = streaming();
        state.diff.lines = lines(4);
        state.diff.done = true;
        assert!(is_idle(&state));
    }

    #[test]
    fn a_stream_paused_at_its_demand_is_idle() {
        let mut state = streaming();
        state.diff.lines = lines(10);
        assert!(is_idle(&state));
    }
}
