//! Review mode: GitHub's pull request queues for the remote's repository,
//! and one pull request's detail. GitHub is only asked when the user
//! enters review mode, opens a pull request or refreshes, and only on a
//! host the global config lists.

use super::files::PrFiles;
use crate::effect::{Effect, Tag};
use crate::forge::merge::MergeMethod;
use crate::forge::remote::{allowed, matches};
use crate::forge::review::{CheckStatus, PrDetail, PrSummary, QueueKind, Queues, ReviewError};
use crate::forge::{ForgeHost, ForgeRepo, ForgeStatus};
use crate::git::commit_files::CommitId;
use crate::intent::AppIntent;
use crate::keymap::Reason;
use crate::list_filter;
use crate::model::{AppState, Context};
use crate::pick_list::{PickList, PickPurpose};
use crate::popup::{MergePrConfirm, Popup};
use ratatui::text::Line;

pub const NO_FORGE: &str =
    "The remote is on no forge host in the global config, or not in [forge.allow]";
const NO_BROWSER: &str = "Set forge.browser in the global config";
const NO_FAILING_CHECK: &str = "No failing check has a details page";
const NO_PR_OPEN: &str = "No pull request is open";
const NO_PR_SELECTED: &str = "No pull request selected";
const BACK_TO_QUEUE: &str = "Go back to the queue first (esc)";
const LOADING: &str = "Already loading";

/// Which screen review mode shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewScreen {
    Queue,
    /// Pull request `number`, loaded or loading.
    Detail(u64),
    /// Pull request `number`'s changed files and their diff.
    Files(u64),
}

/// Why a screen has nothing to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewProblem {
    /// The remote is on no `[forge] github` host; nobody was asked.
    NoForgeHost,
    /// GitHub refuses calls until this second; none is made before it.
    Limited(i64),
    Status(ForgeStatus),
    /// GitHub has no pull request with this number.
    Missing(u64),
}

impl From<ReviewError> for ReviewProblem {
    fn from(error: ReviewError) -> Self {
        match error {
            ReviewError::NoForgeHost => ReviewProblem::NoForgeHost,
            ReviewError::Limited { until } => ReviewProblem::Limited(until),
            ReviewError::Status(status) => ReviewProblem::Status(status),
        }
    }
}

/// One kind of load: the latest asked for, whether it runs, and why the
/// last one gave nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewFetch {
    /// Answers to any other load are dropped.
    pub seq: Option<u64>,
    pub loading: bool,
    pub error: Option<ReviewProblem>,
}

/// Everything review mode shows. Started afresh each time it is entered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewState {
    pub screen: ReviewScreen,
    pub queue: QueueKind,
    pub queues: Option<Queues>,
    /// Index into the shown queue, among the rows the filter keeps.
    pub cursor: usize,
    /// Narrows the queue by number, title and author; empty keeps all.
    pub filter: String,
    pub detail: Option<PrDetail>,
    /// The first line of the detail shown.
    pub detail_scroll: usize,
    pub queues_fetch: ReviewFetch,
    pub detail_fetch: ReviewFetch,
    /// The repository a load found, so the next one need not read the
    /// remote again.
    pub repo: Option<ForgeRepo>,
    /// The pages the open "Check details" list offers, by row.
    pub check_urls: Vec<String>,
    /// The open pull request's description, rendered once for the
    /// panel's width rather than on every frame and key.
    pub body: Option<RenderedBody>,
    /// The files screen, while it is shown.
    pub files: Option<PrFiles>,
    /// The merge question waiting for its method, and the methods the
    /// open "Merge method" list offers, by row.
    pub merge_pick: Option<(MergePrConfirm, Vec<MergeMethod>)>,
    /// Each pull request head fetched this session, so none is fetched
    /// twice. Kept when review mode is left and entered again.
    pub fetched_heads: Vec<(u64, CommitId)>,
}

/// A description as drawn at `width`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedBody {
    pub width: usize,
    pub lines: Vec<Line<'static>>,
}

impl Default for ReviewState {
    fn default() -> Self {
        Self {
            screen: ReviewScreen::Queue,
            queue: QueueKind::Assigned,
            queues: None,
            cursor: 0,
            filter: String::new(),
            detail: None,
            detail_scroll: 0,
            queues_fetch: ReviewFetch::default(),
            detail_fetch: ReviewFetch::default(),
            repo: None,
            check_urls: Vec::new(),
            body: None,
            files: None,
            merge_pick: None,
            fetched_heads: Vec::new(),
        }
    }
}

impl ReviewState {
    /// A session started on one pull request shows it from the first frame.
    pub fn for_context(context: &Context) -> Self {
        let screen = match context {
            Context::Review {
                number: Some(number),
            } => ReviewScreen::Detail(*number),
            _ => ReviewScreen::Queue,
        };
        Self {
            screen,
            ..Self::default()
        }
    }

    /// The shown queue's pull requests, all of them.
    pub fn rows(&self) -> &[PrSummary] {
        self.queues.as_ref().map_or(&[], |q| q.get(self.queue))
    }

    /// The indexes of the rows the filter keeps, in order.
    pub fn visible(&self) -> Vec<usize> {
        let texts: Vec<String> = self.rows().iter().map(row_text).collect();
        list_filter::visible(&self.filter, texts.iter().map(String::as_str))
    }

    /// Started afresh, but still knowing which heads were fetched.
    pub(crate) fn restart(&mut self) {
        let fetched_heads = std::mem::take(&mut self.fetched_heads);
        *self = Self {
            fetched_heads,
            ..Self::default()
        };
    }

    /// A load is in flight on any screen.
    #[cfg(test)]
    pub fn loading(&self) -> bool {
        self.queues_fetch.loading
            || self.detail_fetch.loading
            || self.files.as_ref().is_some_and(PrFiles::loading)
    }

    /// The pull request under the cursor, if the filter shows it.
    pub fn selected(&self) -> Option<&PrSummary> {
        self.visible()
            .contains(&self.cursor)
            .then(|| self.rows().get(self.cursor))
            .flatten()
    }

    /// Put a cursor the filter hides, or one past the end, on the first
    /// row shown.
    pub(crate) fn reconcile(&mut self) {
        let visible = self.visible();
        if !visible.contains(&self.cursor) {
            self.cursor = visible.first().copied().unwrap_or(0);
        }
    }

    /// The failing checks of the open pull request that have a page.
    fn failing_pages(&self) -> Vec<(String, String)> {
        let Some(detail) = &self.detail else {
            return Vec::new();
        };
        detail
            .checks
            .iter()
            .filter(|c| c.status == CheckStatus::Failure)
            .filter_map(|c| Some((c.name.clone(), c.details_url.clone()?)))
            .collect()
    }
}

/// What the filter matches a row against.
fn row_text(pr: &PrSummary) -> String {
    format!("#{} {} {}", pr.number, pr.title, pr.author)
}

/// A pull request is open, on its detail or its files.
fn detail_shown(state: &AppState) -> bool {
    matches!(
        state.review.screen,
        ReviewScreen::Detail(_) | ReviewScreen::Files(_)
    )
}

/// Why Enter cannot open a pull request.
pub(crate) fn open_pr_disabled(state: &AppState) -> Option<Reason> {
    if detail_shown(state) {
        return Some(NO_PR_SELECTED.into());
    }
    state
        .review
        .selected()
        .is_none()
        .then_some(NO_PR_SELECTED.into())
}

/// The queue's keys wait for the way back from a pull request.
pub(crate) fn queue_keys_disabled(state: &AppState) -> Option<Reason> {
    detail_shown(state).then_some(BACK_TO_QUEUE.into())
}

/// A refresh waits for the load it would repeat.
pub(crate) fn refresh_review_disabled(state: &AppState) -> Option<Reason> {
    if let Some(reason) = super::files::refresh_in_files(state) {
        return Some(reason);
    }
    let fetch = match state.review.screen {
        ReviewScreen::Queue => &state.review.queues_fetch,
        ReviewScreen::Detail(_) | ReviewScreen::Files(_) => &state.review.detail_fetch,
    };
    fetch.loading.then_some(LOADING.into())
}

/// Why `o` cannot open a check's page: no pull request open, no browser
/// configured, or no failing check with a page.
pub(crate) fn open_check_disabled(state: &AppState) -> Option<Reason> {
    if !detail_shown(state) || state.review.detail.is_none() {
        return Some(NO_PR_OPEN.into());
    }
    if state.browser.is_none() {
        return Some(NO_BROWSER.into());
    }
    state
        .review
        .failing_pages()
        .is_empty()
        .then_some(NO_FAILING_CHECK.into())
}

impl AppState {
    /// Leave the workspace for review mode, started afresh, and load its
    /// queues: entering is the user's say-so to ask GitHub.
    pub(crate) fn enter_review(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        self.context = Context::Review { number: None };
        self.review.restart();
        self.input = None;
        self.load_review_queues()
    }

    /// The first load of a session started in review mode: the pull
    /// request it names, else the queues.
    pub(crate) fn start_review(&mut self, number: Option<u64>) -> Vec<Effect> {
        self.review = ReviewState::for_context(&self.context);
        match number {
            Some(number) => self.open_review_detail(number),
            None => self.load_review_queues(),
        }
    }

    /// Where a load may go: the configured hosts and, when known, the
    /// repository. Refused when the remote is known to be on none of the
    /// hosts or left out by its allow-list, or its host is rate limited
    /// past now.
    fn review_target(&self) -> Result<(Vec<ForgeHost>, Option<ForgeRepo>), ReviewProblem> {
        let hosts = &self.forge.hosts;
        if hosts.is_empty() || self.forge.unmatched {
            return Err(ReviewProblem::NoForgeHost);
        }
        let repo = self.forge.repo.clone().or_else(|| self.review.repo.clone());
        if let Some(repo) = &repo {
            let host = allowed(repo, hosts).ok_or(ReviewProblem::NoForgeHost)?;
            if let Some(&until) = self.forge.limited.get(host.as_str())
                && until > self.now
            {
                return Err(ReviewProblem::Limited(until));
            }
        }
        Ok((hosts.clone(), repo))
    }

    /// The limits the task checks again by its own clock.
    fn review_limits(&self) -> Vec<(ForgeHost, i64)> {
        self.forge
            .hosts
            .iter()
            .filter_map(|host| Some((host.clone(), *self.forge.limited.get(host.as_str())?)))
            .collect()
    }

    fn review_tag(&mut self) -> Tag {
        Tag {
            generation: self.generation,
            seq: self.take_seq(),
            user: false,
        }
    }

    fn load_review_queues(&mut self) -> Vec<Effect> {
        let (hosts, repo) = match self.review_target() {
            Ok(target) => target,
            Err(problem) => {
                self.review.queues_fetch = ReviewFetch {
                    seq: None,
                    loading: false,
                    error: Some(problem),
                };
                return vec![];
            }
        };
        let tag = self.review_tag();
        self.review.queues_fetch = ReviewFetch {
            seq: Some(tag.seq),
            loading: true,
            error: None,
        };
        vec![Effect::LoadReviewQueues {
            tag,
            hosts,
            repo,
            limited: self.review_limits(),
        }]
    }

    fn load_review_detail(&mut self, number: u64) -> Vec<Effect> {
        let (hosts, repo) = match self.review_target() {
            Ok(target) => target,
            Err(problem) => {
                self.review.detail_fetch = ReviewFetch {
                    seq: None,
                    loading: false,
                    error: Some(problem),
                };
                return vec![];
            }
        };
        let tag = self.review_tag();
        self.review.detail_fetch = ReviewFetch {
            seq: Some(tag.seq),
            loading: true,
            error: None,
        };
        vec![Effect::LoadPrDetail {
            tag,
            hosts,
            repo,
            number,
            limited: self.review_limits(),
        }]
    }

    /// Show pull request `number`, empty until it loads.
    fn open_review_detail(&mut self, number: u64) -> Vec<Effect> {
        self.review.screen = ReviewScreen::Detail(number);
        self.review.detail = None;
        self.review.body = None;
        self.review.detail_scroll = 0;
        self.load_review_detail(number)
    }

    /// Whether an answer under `tag` is the one `fetch` waits for.
    fn review_current(&self, tag: Tag, fetch: &ReviewFetch) -> bool {
        tag.generation == self.generation && fetch.seq == Some(tag.seq)
    }

    /// Remember what an answer says about the repository and its host's
    /// rate limit, which badge loads share.
    fn review_answered(&mut self, repo: Option<ForgeRepo>, limited: Option<i64>) {
        if let Some(repo) = repo {
            if let Some(host) = matches(&repo, &self.forge.hosts) {
                let host = host.as_str().to_string();
                match limited {
                    Some(until) => {
                        self.forge.limited.insert(host, until);
                    }
                    None => {
                        self.forge.limited.remove(&host);
                    }
                }
            }
            self.review.repo = Some(repo);
        }
    }

    /// Keep what a failed answer tells: the repository it found and, on a
    /// rate limit, until when its host is limited.
    fn review_failed(&mut self, repo: Option<ForgeRepo>, error: ReviewError) -> ReviewProblem {
        match error {
            ReviewError::Limited { until } => self.review_answered(repo, Some(until)),
            _ => self.review.repo = repo.or(self.review.repo.take()),
        }
        error.into()
    }

    pub(crate) fn review_queues_loaded(
        &mut self,
        tag: Tag,
        repo: Option<ForgeRepo>,
        result: Result<Queues, ReviewError>,
    ) {
        if !self.review_current(tag, &self.review.queues_fetch) {
            return;
        }
        self.review.queues_fetch.loading = false;
        match result {
            Ok(queues) => {
                self.review_answered(repo, None);
                self.review.queues = Some(queues);
                self.review.queues_fetch.error = None;
                self.review.reconcile();
            }
            Err(error) => self.review.queues_fetch.error = Some(self.review_failed(repo, error)),
        }
    }

    pub(crate) fn pr_detail_loaded(
        &mut self,
        tag: Tag,
        number: u64,
        repo: Option<ForgeRepo>,
        result: Result<Option<PrDetail>, ReviewError>,
    ) {
        if !self.review_current(tag, &self.review.detail_fetch)
            || self.review.screen != ReviewScreen::Detail(number)
        {
            return;
        }
        self.review.detail_fetch.loading = false;
        match result {
            Ok(found) => {
                self.review_answered(repo, None);
                self.review.detail_fetch.error =
                    found.is_none().then_some(ReviewProblem::Missing(number));
                self.review.detail = found;
                self.review.body = None;
                self.render_review_body();
            }
            Err(error) => self.review.detail_fetch.error = Some(self.review_failed(repo, error)),
        }
    }

    /// `j`/`k`: the queue's cursor, the detail's scroll, or the file.
    pub(crate) fn review_move(&mut self, delta: isize) -> Vec<Effect> {
        match self.review.screen {
            ReviewScreen::Queue => {
                let visible = self.review.visible();
                if let Some(at) = list_filter::step(&visible, self.review.cursor, delta) {
                    self.review.cursor = at;
                }
            }
            ReviewScreen::Detail(_) => {
                let max = self.review_max_scroll();
                self.review.detail_scroll = self
                    .review
                    .detail_scroll
                    .saturating_add_signed(delta)
                    .min(max);
            }
            ReviewScreen::Files(_) => return self.move_review_file(delta),
        }
        vec![]
    }

    /// Where the detail is drawn, at the current screen size.
    fn review_area(&self) -> ratatui::layout::Rect {
        let screen = ratatui::layout::Rect::new(0, 0, self.size.0, self.size.1);
        let [main, _] = crate::layout::screen_layout(screen);
        super::view::detail_area(main)
    }

    /// Render the open description for the panel's width, once: on load
    /// and when the screen is resized.
    pub(crate) fn render_review_body(&mut self) {
        let width = usize::from(self.review_area().width);
        if self.review.body.as_ref().is_some_and(|b| b.width == width) {
            return;
        }
        self.review.body = self.review.detail.as_ref().map(|detail| RenderedBody {
            width,
            lines: super::markdown::render(&detail.body, width),
        });
    }

    /// The furthest the detail scrolls, with its last line at the bottom.
    fn review_max_scroll(&self) -> usize {
        match (&self.review.detail, &self.review.body) {
            (Some(detail), Some(body)) => {
                super::view::detail_max_scroll(detail, &body.lines, self.review_area())
            }
            _ => 0,
        }
    }

    pub(crate) fn step_review_queue(&mut self, back: bool) {
        if detail_shown(self) {
            return;
        }
        self.review.queue = self.review.queue.step(back);
        self.review.cursor = 0;
        self.review.reconcile();
    }

    pub(crate) fn open_pr(&mut self) -> Vec<Effect> {
        if let Some(reason) = open_pr_disabled(self) {
            self.notice = Some(reason);
            return vec![];
        }
        match self.review.selected().map(|pr| pr.number) {
            Some(number) => self.open_review_detail(number),
            None => vec![],
        }
    }

    /// Esc: from a pull request back to the same queue and row; in the
    /// queue, clear the filter, else go back to the workspace.
    pub(crate) fn close_review(&mut self) -> Vec<Effect> {
        match self.review.screen {
            ReviewScreen::Files(_) => self.close_review_files(),
            ReviewScreen::Detail(_) => {
                self.review.screen = ReviewScreen::Queue;
                self.review.detail = None;
                self.review.body = None;
                self.review.detail_fetch = ReviewFetch::default();
                // A session started on one pull request loads its queues
                // only now.
                if self.review.queues.is_none() && !self.review.queues_fetch.loading {
                    return self.load_review_queues();
                }
                vec![]
            }
            ReviewScreen::Queue if !self.review.filter.is_empty() => {
                self.review.filter.clear();
                self.review.reconcile();
                vec![]
            }
            ReviewScreen::Queue => {
                self.context = Context::Workspace;
                self.review.restart();
                if self.workspace_started {
                    let mut effects = self.update(AppIntent::RefreshStatus);
                    // The review may have streamed its own diff into the
                    // pane; a commit's or stash's open file gets it back.
                    effects.extend(self.reload_open_file_diff());
                    effects
                } else {
                    self.start()
                }
            }
        }
    }

    pub(crate) fn refresh_review(&mut self) -> Vec<Effect> {
        if let Some(reason) = refresh_review_disabled(self) {
            self.notice = Some(reason);
            return vec![];
        }
        match self.review.screen {
            ReviewScreen::Queue => self.load_review_queues(),
            ReviewScreen::Detail(number) | ReviewScreen::Files(number) => {
                self.load_review_detail(number)
            }
        }
    }

    /// `o`: open the one failing check's page, or pick among several.
    pub(crate) fn open_check_details(&mut self) -> Vec<Effect> {
        if let Some(reason) = open_check_disabled(self) {
            self.notice = Some(reason);
            return vec![];
        }
        let pages = self.review.failing_pages();
        if let [(_, url)] = pages.as_slice() {
            return self.open_check_url(url.clone());
        }
        let rows = pages.iter().map(|(name, _)| name.clone()).collect();
        self.review.check_urls = pages.into_iter().map(|(_, url)| url).collect();
        self.popups.push(Popup::PickList(PickList::new(
            PickPurpose::CheckDetails,
            rows,
            0,
        )));
        vec![]
    }

    /// The check picked from the "Check details" list.
    pub(crate) fn pick_check(&mut self, row: usize) -> Vec<Effect> {
        let urls = std::mem::take(&mut self.review.check_urls);
        match urls.get(row) {
            Some(url) => self.open_check_url(url.clone()),
            None => vec![],
        }
    }

    fn open_check_url(&mut self, url: String) -> Vec<Effect> {
        let Some(mut argv) = self.browser.clone() else {
            self.notice = Some(NO_BROWSER.into());
            return vec![];
        };
        // Its own argument, last: never joined into another one.
        argv.push(url.clone().into());
        self.notice_text = Some(format!("Opening {url}"));
        vec![Effect::OpenUrl { argv }]
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::ffi::OsString;

    use crossterm::event::{KeyCode, KeyModifiers};

    use super::*;
    use crate::forge::ForgeView;
    use crate::forge::review::Check;
    use crate::forge::review::fixtures::{DETAIL, QUEUES};
    use crate::keymap::KeyPress;
    use crate::list_filter::InputTarget;

    const NOW: i64 = 1_700_000_000;

    pub(crate) fn github() -> Vec<ForgeHost> {
        vec![ForgeHost::parse("github.com").unwrap()]
    }

    pub(crate) fn repo() -> ForgeRepo {
        ForgeRepo {
            host: "github.com".into(),
            owner: "acme".into(),
            name: "widgets".into(),
        }
    }

    fn browser() -> Vec<OsString> {
        vec!["firefox".into(), "--new-tab".into()]
    }

    /// A workspace on github.com, its repository known from the badges.
    pub(crate) fn workspace() -> AppState {
        let mut state = AppState::new(Context::Workspace)
            .with_location(Some("/r".into()), "/r".into())
            .with_forge_hosts(github())
            .with_forge_links(Vec::new(), Some(browser()));
        state.forge.repo = Some(repo());
        state.now = NOW;
        state.size = (80, 30);
        state.workspace_started = true;
        state
    }

    pub(crate) fn press(state: &mut AppState, code: KeyCode) -> Vec<Effect> {
        state.update(AppIntent::Key(KeyPress::new(code, KeyModifiers::NONE)))
    }

    pub(crate) fn type_char(state: &mut AppState, c: char) -> Vec<Effect> {
        press(state, KeyCode::Char(c))
    }

    fn queue_tag(effects: &[Effect]) -> Tag {
        match effects {
            [Effect::LoadReviewQueues { tag, .. }] => *tag,
            other => panic!("expected one queue load, got {other:?}"),
        }
    }

    pub(crate) fn detail_load(effects: &[Effect]) -> (Tag, u64) {
        match effects {
            [Effect::LoadPrDetail { tag, number, .. }] => (*tag, *number),
            other => panic!("expected one detail load, got {other:?}"),
        }
    }

    pub(crate) fn fixture_queues() -> Queues {
        let runner =
            crate::forge::github::FakeGh::new().reply(crate::forge::github::answer(QUEUES));
        let host = github()[0].clone();
        let sink = crate::git::recording::CommandSink::new();
        crate::forge::github::Gh::fixture(&runner, &sink, &host, NOW)
            .fetch_queues(&repo())
            .unwrap()
    }

    pub(crate) fn fixture_detail() -> PrDetail {
        detail_from(DETAIL)
    }

    pub(crate) fn detail_from(answer: &str) -> PrDetail {
        let runner =
            crate::forge::github::FakeGh::new().reply(crate::forge::github::answer(answer));
        let host = github()[0].clone();
        let sink = crate::git::recording::CommandSink::new();
        crate::forge::github::Gh::fixture(&runner, &sink, &host, NOW)
            .fetch_detail(&repo(), 42)
            .unwrap()
            .unwrap()
    }

    /// In review mode with the fixture's queues shown.
    pub(crate) fn in_queues() -> AppState {
        let mut state = workspace();
        let tag = queue_tag(&state.update(AppIntent::EnterReview));
        state.update(AppIntent::ReviewQueuesLoaded {
            tag,
            repo: Some(repo()),
            result: Ok(fixture_queues()),
        });
        state
    }

    /// The fixture's detail open, with `checks` in place of its own.
    pub(crate) fn in_detail(checks: Vec<Check>) -> AppState {
        let mut state = in_queues();
        let (tag, number) = detail_load(&press(&mut state, KeyCode::Enter));
        let mut detail = fixture_detail();
        detail.checks = checks;
        state.update(AppIntent::PrDetailLoaded {
            tag,
            number,
            repo: Some(repo()),
            result: Ok(Some(detail)),
        });
        state
    }

    fn failing(name: &str) -> Check {
        Check {
            name: name.into(),
            status: CheckStatus::Failure,
            details_url: Some(format!("https://ci.example.com/{name}")),
        }
    }

    #[test]
    fn a_session_on_one_pull_request_loads_only_it_then_the_queues_on_esc() {
        let mut state = AppState::new(Context::Review { number: Some(42) })
            .with_location(Some("/r".into()), "/r".into())
            .with_forge_hosts(github());
        let effects = state.update(AppIntent::Start);
        let loads: Vec<_> = effects
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    Effect::LoadPrDetail { .. } | Effect::LoadReviewQueues { .. }
                )
            })
            .collect();
        match loads.as_slice() {
            [
                Effect::LoadPrDetail {
                    number: 42,
                    repo: None,
                    hosts,
                    ..
                },
            ] => assert_eq!(*hosts, github()),
            other => panic!("{other:?}"),
        }
        assert_eq!(state.review.screen, ReviewScreen::Detail(42));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::LoadStatus { .. })),
            "{effects:?}"
        );

        // Esc in the detail goes to the queue, which loads only now.
        let effects = press(&mut state, KeyCode::Esc);
        queue_tag(&effects);
        assert_eq!(state.review.screen, ReviewScreen::Queue);
        // Esc in the queue goes to the workspace, which starts its first load.
        let effects = press(&mut state, KeyCode::Esc);
        assert_eq!(state.context, Context::Workspace);
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::LoadStatus { .. })),
            "{effects:?}"
        );
    }

    #[test]
    fn with_no_host_or_another_host_nothing_is_asked() {
        // No `[forge] github` hosts at all.
        let mut state = AppState::new(Context::Workspace);
        assert_eq!(state.update(AppIntent::EnterReview), []);
        assert_eq!(
            state.review.queues_fetch.error,
            Some(ReviewProblem::NoForgeHost)
        );
        let mut state = AppState::new(Context::Review { number: Some(42) });
        assert_eq!(state.update(AppIntent::Start), []);
        assert_eq!(
            state.review.detail_fetch.error,
            Some(ReviewProblem::NoForgeHost)
        );

        // The badge load found the remote on no configured host.
        let mut state = workspace();
        state.forge.repo = None;
        state.forge.unmatched = true;
        assert_eq!(state.update(AppIntent::EnterReview), []);
        assert_eq!(
            state.review.queues_fetch.error,
            Some(ReviewProblem::NoForgeHost)
        );
        let mut state =
            AppState::new(Context::Review { number: Some(42) }).with_forge_hosts(github());
        state.forge.unmatched = true;
        assert_eq!(state.update(AppIntent::Start), []);
        assert_eq!(
            state.review.detail_fetch.error,
            Some(ReviewProblem::NoForgeHost)
        );

        // The known repository is one `[forge.allow]` leaves out.
        let mut state = workspace();
        let rule = crate::forge::allow::AllowRule::parse("someone-else").unwrap();
        state.forge.hosts = vec![state.forge.hosts[0].clone().with_allow(vec![rule])];
        assert!(state.forge.repo.is_some());
        assert_eq!(state.update(AppIntent::EnterReview), []);
        assert_eq!(
            state.review.queues_fetch.error,
            Some(ReviewProblem::NoForgeHost)
        );
    }

    #[test]
    fn a_host_limited_past_now_gets_no_load_until_its_reset() {
        let mut state = workspace();
        state.forge.limited.insert("github.com".into(), NOW + 60);
        assert_eq!(state.update(AppIntent::EnterReview), []);
        assert_eq!(
            state.review.queues_fetch.error,
            Some(ReviewProblem::Limited(NOW + 60))
        );
        // At the reset it asks, and hands the limit to the task as well.
        state.now = NOW + 60;
        let effects = state.update(AppIntent::RefreshReview);
        match effects.as_slice() {
            [Effect::LoadReviewQueues { limited, repo, .. }] => {
                assert_eq!(*limited, [(github()[0].clone(), NOW + 60)]);
                assert_eq!(*repo, Some(self::repo()));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_rate_limited_answer_is_remembered_and_an_answer_clears_it() {
        let mut state = workspace();
        let tag = queue_tag(&state.update(AppIntent::EnterReview));
        state.update(AppIntent::ReviewQueuesLoaded {
            tag,
            repo: Some(repo()),
            result: Err(ReviewError::Limited { until: NOW + 90 }),
        });
        assert_eq!(state.forge.limited.get("github.com"), Some(&(NOW + 90)));
        assert_eq!(
            state.review.queues_fetch.error,
            Some(ReviewProblem::Limited(NOW + 90))
        );
        assert_eq!(state.update(AppIntent::RefreshReview), []);
        state.now = NOW + 90;
        let tag = queue_tag(&state.update(AppIntent::RefreshReview));
        state.update(AppIntent::ReviewQueuesLoaded {
            tag,
            repo: Some(repo()),
            result: Ok(fixture_queues()),
        });
        assert!(state.forge.limited.is_empty());
        assert_eq!(state.forge.status, ForgeView::Off);
    }

    #[test]
    fn the_queue_keys_cycle_filter_open_and_come_back_to_the_same_row() {
        let mut state = in_queues();
        assert_eq!(state.panel_focus(), crate::keymap::KeyScope::Review);
        let numbers = |state: &AppState| -> Vec<u64> {
            let rows = state.review.rows();
            state
                .review
                .visible()
                .iter()
                .map(|&i| rows[i].number)
                .collect()
        };
        assert_eq!(numbers(&state), [12, 7]);
        for want in [
            QueueKind::ReviewRequested,
            QueueKind::Authored,
            QueueKind::AllOpen,
            QueueKind::Assigned,
        ] {
            press(&mut state, KeyCode::Tab);
            assert_eq!(state.review.queue, want);
        }
        press(&mut state, KeyCode::BackTab);
        assert_eq!(state.review.queue, QueueKind::AllOpen);
        assert_eq!(numbers(&state), [15, 12, 9, 7]);

        type_char(&mut state, '/');
        assert_eq!(
            state.input.as_ref().map(|i| i.target),
            Some(InputTarget::ReviewFilter)
        );
        for c in "widget".chars() {
            type_char(&mut state, c);
        }
        press(&mut state, KeyCode::Enter);
        assert_eq!(state.input, None);
        assert_eq!(numbers(&state), [12, 7]);
        // By author and number too.
        state.review.filter = "dependabot".into();
        assert_eq!(numbers(&state), [9]);
        state.review.filter = "#15".into();
        assert_eq!(numbers(&state), [15]);
        state.review.filter = "widget".into();
        state.review.reconcile();

        type_char(&mut state, 'j');
        assert_eq!(state.review.cursor, 3);
        let (_, number) = detail_load(&press(&mut state, KeyCode::Enter));
        assert_eq!(number, 7);
        assert_eq!(state.review.screen, ReviewScreen::Detail(7));
        assert_eq!(press(&mut state, KeyCode::Esc), []);
        assert_eq!(state.review.screen, ReviewScreen::Queue);
        assert_eq!(state.review.queue, QueueKind::AllOpen);
        assert_eq!(state.review.cursor, 3);
        assert_eq!(state.review.filter, "widget");
        // Esc clears the filter, then leaves review mode.
        assert_eq!(press(&mut state, KeyCode::Esc), []);
        assert_eq!(state.review.filter, "");
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.context, Context::Workspace);
    }

    #[test]
    fn stale_answers_are_dropped() {
        let mut state = workspace();
        let old = queue_tag(&state.update(AppIntent::EnterReview));
        // A refresh waits for a load in flight; one that got past it
        // still wins over the first.
        assert_eq!(state.update(AppIntent::RefreshReview), []);
        let new = queue_tag(&state.load_review_queues());
        state.update(AppIntent::ReviewQueuesLoaded {
            tag: old,
            repo: Some(repo()),
            result: Ok(fixture_queues()),
        });
        assert_eq!(state.review.queues, None);
        assert!(state.review.queues_fetch.loading);
        state.update(AppIntent::ReviewQueuesLoaded {
            tag: new,
            repo: Some(repo()),
            result: Ok(fixture_queues()),
        });
        assert!(state.review.queues.is_some());

        // A detail answer for a pull request no longer open is dropped too.
        let (tag, number) = detail_load(&press(&mut state, KeyCode::Enter));
        press(&mut state, KeyCode::Esc);
        state.update(AppIntent::PrDetailLoaded {
            tag,
            number,
            repo: Some(repo()),
            result: Ok(Some(fixture_detail())),
        });
        assert_eq!(state.review.detail, None);
        assert_eq!(state.review.screen, ReviewScreen::Queue);
    }

    #[test]
    fn a_missing_pull_request_says_so() {
        let mut state = in_queues();
        let (tag, number) = detail_load(&press(&mut state, KeyCode::Enter));
        state.update(AppIntent::PrDetailLoaded {
            tag,
            number,
            repo: Some(repo()),
            result: Ok(None),
        });
        assert_eq!(
            state.review.detail_fetch.error,
            Some(ReviewProblem::Missing(12))
        );
    }

    #[test]
    fn o_opens_the_one_failing_checks_page_as_the_last_argument() {
        let mut state = in_detail(fixture_detail().checks);
        let effects = type_char(&mut state, 'o');
        assert_eq!(
            effects,
            [Effect::OpenUrl {
                argv: vec![
                    "firefox".into(),
                    "--new-tab".into(),
                    "https://ci.example.com/build/1".into(),
                ]
            }]
        );
    }

    #[test]
    fn o_with_two_failing_checks_picks_one() {
        let mut state = in_detail(vec![failing("build"), failing("lint")]);
        assert_eq!(type_char(&mut state, 'o'), []);
        match state.popups.last() {
            Some(Popup::PickList(list)) => {
                assert_eq!(list.purpose, PickPurpose::CheckDetails);
                assert_eq!(list.purpose.title(), "Check details");
                assert_eq!(list.rows, ["build", "lint"]);
            }
            other => panic!("{other:?}"),
        }
        type_char(&mut state, 'j');
        let effects = press(&mut state, KeyCode::Enter);
        assert_eq!(
            effects,
            [Effect::OpenUrl {
                argv: vec![
                    "firefox".into(),
                    "--new-tab".into(),
                    "https://ci.example.com/lint".into(),
                ]
            }]
        );
        assert!(state.popups.is_empty());
    }

    #[test]
    fn o_is_off_with_no_failing_check_or_no_browser() {
        let passing = Check {
            status: CheckStatus::Success,
            ..failing("ok")
        };
        let no_page = Check {
            details_url: None,
            ..failing("bad")
        };
        let mut state = in_detail(vec![passing, no_page]);
        assert_eq!(type_char(&mut state, 'o'), []);
        assert_eq!(state.notice.as_deref(), Some(NO_FAILING_CHECK));
        assert!(state.popups.is_empty());

        let mut state = in_detail(vec![failing("build")]);
        state.browser = None;
        assert_eq!(type_char(&mut state, 'o'), []);
        assert_eq!(state.notice.as_deref(), Some(NO_BROWSER));

        // Nor in the queue.
        let mut state = in_queues();
        assert_eq!(type_char(&mut state, 'o'), []);
        assert_eq!(state.notice.as_deref(), Some(NO_PR_OPEN));
    }

    #[test]
    fn j_and_k_scroll_the_detail_within_its_lines() {
        let mut state = in_detail(Vec::new());
        type_char(&mut state, 'k');
        assert_eq!(state.review.detail_scroll, 0);
        for _ in 0..200 {
            type_char(&mut state, 'j');
        }
        let max = state.review_max_scroll();
        assert_eq!(state.review.detail_scroll, max);
        // The fixture is short: it fits an 80x30 screen.
        assert_eq!(max, 0);
        state.size = (80, 8);
        for _ in 0..200 {
            type_char(&mut state, 'j');
        }
        assert!(state.review.detail_scroll > 0);
    }

    #[test]
    fn the_description_is_rendered_once_per_width() {
        let mut state = in_detail(Vec::new());
        let body = state.review.body.clone().expect("rendered on load");
        assert_eq!(body.width, 78);
        assert!(!body.lines.is_empty());
        // Keys and frames reuse it.
        type_char(&mut state, 'j');
        assert_eq!(state.review.body.as_ref(), Some(&body));
        // A resize renders it for the new width.
        state.update(AppIntent::Resize {
            width: 50,
            height: 20,
        });
        assert_eq!(state.review.body.as_ref().map(|b| b.width), Some(48));
        // Leaving the pull request drops it.
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.review.body, None);
    }
}
