//! Pull requests from the Branches panel: open the selected branch's
//! pull request (or GitLab merge requests) in the browser, copy its URL,
//! or create one on GitHub. Every URL is built here from the remotes; only
//! a create reaches the network, and only after a `y`.

use std::ffi::OsString;

use crate::clipboard::CopyWhat;
use crate::confirm_question::{Answer, ConfirmQuestion, Then};
use crate::effect::Effect;
use crate::features::pull_requests::PullRequestsIntent;
use crate::intent::AppIntent;
use crate::keymap::{KeyScope, Reason, no_commit_selected};
use crate::model::{AppState, Context};
use crate::pick_list::{PickList, PickPurpose};
use crate::popup::{ErrorPopup, LineInput, Popup, PromptKind};
use kika_forge::forge::{Capability, supports};
use kika_forge::remote::{allowed, matches};
use kika_forge::url::{ForgeKind, NoBranchPage, branch_page_url, commit_url, forge_for, repo_of};
use kika_forge::{
    CreateError, ForgeHost, ForgeRepo, PrBadge, PrState, REFRESH_LIMITED, refresh_limited,
    same_repo,
};
use kika_git::branches::BranchName;
use kika_git::commit_files::CommitId;
use kika_git::remotes::RemoteName;
use kika_git::safe_text::terminal_safe;

const NO_BRANCH: &str = "No branch selected";
const NO_FORGE: &str = "The remote is on no forge host in the global config";
const NO_BROWSER: &str = "Set forge.browser in the global config";
const NO_PR: &str = "No pull request known for this branch";
const NO_UPSTREAM: &str = "Push the branch first";
const RATE_LIMITED: &str = "GitHub is rate limited; fetch after the limit resets";
const CREATING: &str = "A pull request is being created";
const NOT_ALLOWED: &str = "No target repository is allowed by [forge.allow] in the global config";
const NOT_ASKED: &str =
    "The remote is on no forge host in the global config, or not in [forge.allow]";
const NO_HOSTS: &str = "Name GitHub hosts under [forge] in the global config";
const REFRESHING: &str = "Pull requests are already being refreshed";

/// A pull request being put together. Its parts are taken when each step
/// is answered, so a reload meanwhile cannot change what is created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrDraft {
    /// The local branch, which gets the badge.
    pub branch: BranchName,
    /// The remote the branch was pushed to, and its repository.
    pub head_remote: RemoteName,
    pub head_repo: ForgeRepo,
    /// The upstream's name on its remote, without the remote's prefix.
    pub head: String,
    /// The remotes on the head's GitHub host, any of which it may target.
    pub targets: Vec<(RemoteName, ForgeRepo)>,
    pub target: Option<(RemoteName, ForgeRepo)>,
    /// The target remote's branches it may merge into.
    pub bases: Vec<String>,
    pub base: Option<String>,
    pub title: String,
}

/// The create GitHub is asked for, kept until it answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrCreating {
    pub seq: u64,
    pub branch: BranchName,
    pub host: ForgeHost,
    pub target: ForgeRepo,
}

/// The forge repository the branches belong to, and its kind: `origin`,
/// else the only remote, on a host the global config lists.
fn forge_link(state: &AppState) -> Option<(ForgeKind, ForgeRepo)> {
    forge_for(
        &state
            .branches
            .remotes
            .remotes()
            .cloned()
            .collect::<Vec<_>>(),
        &state.forge.hosts,
        &state.forge_gitlab_hosts,
    )
}

/// The selected branch's pull request page on GitHub, or its merge
/// requests on GitLab, or why there is none.
fn branch_page(state: &AppState) -> Result<String, Reason> {
    let branch = state.branches.selected_branch().ok_or(NO_BRANCH)?;
    let (kind, repo) = forge_link(state).ok_or(NO_FORGE)?;
    let pr = state
        .forge
        .badges
        .get(&branch.name)
        .map(|badge| badge.number);
    branch_page_url(kind, &repo, &branch.name, pr).map_err(|gap| match gap {
        NoBranchPage::NoPull => NO_PR.into(),
        NoBranchPage::Unbuildable => NO_FORGE.into(),
    })
}

/// The selected commit's page on the remote's forge, or why there is none.
pub(crate) fn commit_page(state: &AppState, id: &CommitId) -> Result<String, Reason> {
    let (kind, repo) = forge_link(state).ok_or(NO_FORGE)?;
    commit_url(kind, &repo, id).ok_or(NO_FORGE.into())
}

/// Why "Open in browser" cannot run: no browser configured, or no page.
pub(crate) fn open_url_disabled(state: &AppState) -> Option<Reason> {
    if state.browser.is_none() {
        return Some(NO_BROWSER.into());
    }
    copy_url_disabled(state)
}

/// Why "Copy URL" cannot run: no forge repository, or on GitHub no pull
/// request known for the branch.
pub(crate) fn copy_url_disabled(state: &AppState) -> Option<Reason> {
    branch_page(state).err()
}

/// Why the Copy menu's "Forge URL" cannot run.
pub(crate) fn commit_url_disabled(state: &AppState) -> Option<Reason> {
    no_commit_selected(state).or_else(|| forge_link(state).is_none().then_some(NO_FORGE.into()))
}

/// Why no pull request can be created for the selected branch: only on
/// GitHub, one per branch, once it is pushed, and one at a time.
pub(crate) fn create_disabled(state: &AppState) -> Option<Reason> {
    let Some(branch) = state.branches.selected_branch() else {
        return Some(NO_BRANCH.into());
    };
    let Some((kind, repo)) = forge_link(state) else {
        return Some(NO_FORGE.into());
    };
    if let Err(unsupported) = supports(kind, Capability::CreatePull) {
        return Some(unsupported.message().into());
    }
    if let Some(badge) = state.forge.badges.get(&branch.name) {
        return Some(Reason::from(format!(
            "The branch already has pull request #{}",
            badge.number
        )));
    }
    if branch.upstream.is_none() {
        return Some(NO_UPSTREAM.into());
    }
    if state
        .forge
        .limited
        .contains_key(&repo.host.to_ascii_lowercase())
    {
        return Some(RATE_LIMITED.into());
    }
    state.pr_create.is_some().then_some(CREATING.into())
}

/// Why the badges cannot be asked for again now: no host opted in, the
/// remote on none of them or left out by `[forge.allow]`, a call already running, or the host rate
/// limited until a second still to come. Fixed text only, never the
/// forge's.
pub(crate) fn refresh_disabled(state: &AppState) -> Option<Reason> {
    let forge = &state.forge;
    if !forge.enabled() {
        return Some(NO_HOSTS.into());
    }
    let Some(repo) = &forge.repo else {
        return Some(NOT_ASKED.into());
    };
    if forge.in_flight {
        return Some(REFRESHING.into());
    }
    let limited = forge.limited.get(&repo.host.to_ascii_lowercase());
    limited.is_some_and(|until| *until > state.now).then(|| {
        forge
            .kind()
            .map_or_else(|| REFRESH_LIMITED.into(), refresh_limited)
            .into()
    })
}

/// The most of a title the question shows, in characters.
const QUESTION_TITLE: usize = 72;

/// The question before a create: what to press first, so a short screen
/// cuts the details rather than the answer, then where, what into what,
/// and the title, cut to [`QUESTION_TITLE`].
fn question(state: &AppState) -> Option<String> {
    let draft = state.pr_draft.as_ref()?;
    let (_, target) = draft.target.as_ref()?;
    let base = draft.base.as_ref()?;
    let safe = |text: &str| terminal_safe(text.as_bytes());
    let title = safe(&draft.title);
    let title = match title.char_indices().nth(QUESTION_TITLE) {
        Some((cut, _)) => format!("{}…", &title[..cut]),
        None => title,
    };
    Some(format!(
        "Press y to create it on GitHub, n or Esc to cancel\n{}/{}: {} → {}\n{title}",
        safe(&target.owner),
        safe(&target.name),
        safe(&head_ref(draft, target)),
        safe(base),
    ))
}

/// "Create the pull request?", asked once the draft has its title. It
/// reaches GitHub, so only `y` answers it: never Enter. Only
/// `PullRequestsIntent::ConfirmCreate` creates, from the draft as it is then.
pub(crate) fn create_question(state: &AppState) -> ConfirmQuestion {
    let text = question(state).unwrap_or_else(|| {
        // The fallback when the draft cannot be named.
        "Create the pull request on GitHub?\nPress y to create, n or Esc to cancel".into()
    });
    ConfirmQuestion {
        scope: KeyScope::CreatePullRequest,
        title: "Create pull request".into(),
        text,
        answer: Answer::YOnly,
        then: Then::Intent(Box::new(AppIntent::PullRequests(
            PullRequestsIntent::ConfirmCreate,
        ))),
        range: false,
        head: None,
    }
}

/// The head as GitHub names it: `owner:branch` from a fork.
fn head_ref(draft: &PrDraft, target: &ForgeRepo) -> String {
    if same_repo(&draft.head_repo, target) {
        draft.head.clone()
    } else {
        format!("{}:{}", draft.head_repo.owner, draft.head)
    }
}

impl AppState {
    /// The global config's GitLab hosts and browser, for forge links.
    pub fn with_forge_links(self, gitlab: Vec<ForgeHost>, browser: Option<Vec<OsString>>) -> Self {
        Self {
            forge_gitlab_hosts: gitlab,
            browser,
            ..self
        }
    }

    /// Hand the selected branch's page to the configured browser.
    pub(crate) fn open_forge_url(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        if let Some(reason) = open_url_disabled(self) {
            self.notice = Some(reason);
            return vec![];
        }
        let (Ok(url), Some(mut argv)) = (branch_page(self), self.browser.clone()) else {
            return vec![];
        };
        // Its own argument, last: never joined into another one.
        argv.push(url.clone().into());
        self.notice = Some(format!("Opening {url}").into());
        vec![Effect::OpenUrl { argv }]
    }

    /// Put the selected branch's page on the clipboard.
    pub(crate) fn copy_forge_url(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        match branch_page(self) {
            Ok(url) => self.copy_text(CopyWhat::ForgeUrl, url),
            Err(reason) => {
                self.notice = Some(reason);
                vec![]
            }
        }
    }

    /// Start a pull request for the selected branch: from the remote its
    /// upstream is on, to a remote on the same GitHub host.
    pub(crate) fn create_pr_start(&mut self) {
        if self.context != Context::Workspace {
            return;
        }
        if let Some(reason) = create_disabled(self) {
            self.notice = Some(reason);
            return;
        }
        match self.new_draft() {
            Ok(draft) => {
                let rows: Vec<String> = draft
                    .targets
                    .iter()
                    .map(|(name, repo)| {
                        let row = format!("{}  {}/{}", name.display(), repo.owner, repo.name);
                        terminal_safe(row.as_bytes())
                    })
                    .collect();
                let single = rows.len() == 1;
                self.pr_draft = Some(draft);
                if single {
                    self.choose_target(0);
                } else {
                    let list = PickList::new(PickPurpose::TargetRemote, rows, 0);
                    self.popups.push(Popup::PickList(list));
                }
            }
            Err(reason) => self.notice = Some(reason.into()),
        }
    }

    /// The draft for the selected branch, from its upstream.
    fn new_draft(&self) -> Result<PrDraft, &'static str> {
        let branch = self.branches.selected_branch().ok_or(NO_BRANCH)?;
        let upstream = branch.upstream.as_deref().ok_or(NO_UPSTREAM)?;
        let remotes = &self.branches.remotes;
        let pushed = remotes
            .branch_named(upstream)
            .ok_or("The upstream is not a listed remote branch; fetch first")?;
        let not_github = "The upstream's remote is not on a GitHub host in the global config";
        let head_remote = remotes
            .remotes()
            .find(|r| r.configured && r.name == pushed.remote)
            .ok_or(not_github)?;
        let (_, head_repo) = repo_of(head_remote, &self.forge.hosts, &[]).ok_or(not_github)?;
        let head = std::str::from_utf8(&pushed.short)
            .map_err(|_| "The upstream's name is not UTF-8, which GitHub needs")?
            .to_string();
        let targets = remotes
            .remotes()
            .filter(|r| r.configured)
            .filter_map(|r| Some((r.name.clone(), repo_of(r, &self.forge.hosts, &[])?.1)))
            .filter(|(_, repo)| repo.host.eq_ignore_ascii_case(&head_repo.host))
            // Only the repository `gh` is asked to change must be allowed;
            // the head may be in the user's own, unlisted fork.
            .filter(|(_, repo)| allowed(repo, &self.forge.hosts).is_some())
            .collect::<Vec<_>>();
        if targets.is_empty() {
            return Err(NOT_ALLOWED);
        }
        Ok(PrDraft {
            branch: branch.name.clone(),
            head_remote: head_remote.name.clone(),
            head_repo,
            head,
            targets,
            target: None,
            bases: Vec::new(),
            base: None,
            title: branch.name.display(),
        })
    }

    pub(crate) fn move_pick_cursor(&mut self, delta: isize) {
        if let Some(Popup::PickList(list)) = self.popups.last_mut() {
            list.move_cursor(delta);
        }
    }

    /// Take the picked row and go on to the next step.
    pub(crate) fn pick_select(&mut self) -> Vec<Effect> {
        let Some(Popup::PickList(list)) = self.popups.last() else {
            return vec![];
        };
        let (purpose, cursor) = (list.purpose, list.cursor);
        self.popups.pop();
        match purpose {
            PickPurpose::TargetRemote => self.choose_target(cursor),
            PickPurpose::BaseBranch => self.choose_base(cursor),
            PickPurpose::CheckDetails => return self.pick_check(cursor),
            PickPurpose::MergeMethod => self.pick_merge_method(cursor),
            PickPurpose::ReviewCommit => return self.pick_review_commit(cursor),
            PickPurpose::ReviewEvent => self.pick_review_event(cursor),
        }
        vec![]
    }

    /// Target `targets[i]`, then pick its base branch: `main`, else
    /// `master`, else the first, comes preselected.
    fn choose_target(&mut self, i: usize) {
        let Some(draft) = self.pr_draft.as_ref() else {
            return;
        };
        let Some(target) = draft.targets.get(i).cloned() else {
            return;
        };
        let same_remote = target.0 == draft.head_remote;
        let bases: Vec<String> = self
            .branches
            .remotes
            .remotes()
            .filter(|r| r.name == target.0)
            .flat_map(|r| &r.branches)
            .filter_map(|b| std::str::from_utf8(&b.short).ok())
            .filter(|name| *name != "HEAD" && !(same_remote && *name == draft.head))
            .map(String::from)
            .collect();
        if bases.is_empty() {
            self.pr_draft = None;
            self.notice = Some("No branch on that remote to merge into; fetch first".into());
            return;
        }
        let preselect = ["main", "master"]
            .iter()
            .find_map(|name| bases.iter().position(|b| b == name))
            .unwrap_or(0);
        let rows: Vec<String> = bases.iter().map(|b| terminal_safe(b.as_bytes())).collect();
        let Some(draft) = self.pr_draft.as_mut() else {
            return;
        };
        draft.target = Some(target);
        draft.bases = bases;
        if rows.len() == 1 {
            self.choose_base(0);
        } else {
            let list = PickList::new(PickPurpose::BaseBranch, rows, preselect);
            self.popups.push(Popup::PickList(list));
        }
    }

    /// Merge into `bases[i]`, then ask the title.
    fn choose_base(&mut self, i: usize) {
        let Some(draft) = self.pr_draft.as_mut() else {
            return;
        };
        let Some(base) = draft.bases.get(i).cloned() else {
            return;
        };
        draft.base = Some(base);
        let input = LineInput::with_text(&draft.title);
        self.popups.push(Popup::Prompt {
            kind: PromptKind::PullRequestTitle,
            input,
        });
    }

    /// Keep the title and ask before creating; an empty one is asked again.
    pub(crate) fn pull_request_titled(&mut self, text: &str) {
        let Some(draft) = self.pr_draft.as_mut() else {
            return;
        };
        let title = text.trim();
        if title.is_empty() {
            self.notice = Some("A pull request needs a title".into());
            self.popups.push(Popup::Prompt {
                kind: PromptKind::PullRequestTitle,
                input: LineInput::with_text(text),
            });
            return;
        }
        draft.title = title.to_string();
        let question = create_question(self);
        self.popups.push(Popup::Question(question));
    }

    /// Ask GitHub to create the pull request the question described.
    pub(crate) fn confirm_create_pull_request(&mut self) -> Vec<Effect> {
        if self
            .take_question(KeyScope::CreatePullRequest, Answer::YOnly)
            .is_none()
        {
            return vec![];
        }
        let Some(draft) = self.pr_draft.take() else {
            return vec![];
        };
        let (Some((_, target)), Some(base)) = (draft.target.clone(), draft.base.clone()) else {
            return vec![];
        };
        if self.pr_create.is_some() {
            self.notice = Some(CREATING.into());
            return vec![];
        }
        let Some(host) = matches(&target, &self.forge.hosts).cloned() else {
            self.notice = Some(NO_FORGE.into());
            return vec![];
        };
        if allowed(&target, &self.forge.hosts).is_none() {
            self.notice = Some(NOT_ALLOWED.into());
            return vec![];
        }
        if self.forge.limited.contains_key(host.as_str()) {
            self.notice = Some(RATE_LIMITED.into());
            return vec![];
        }
        let head = head_ref(&draft, &target);
        let seq = self.take_seq();
        let tag = self.user_tag(seq);
        self.notice = Some(
            format!(
                "Creating the pull request on {}/{}…",
                target.owner, target.name
            )
            .into(),
        );
        self.pr_create = Some(PrCreating {
            seq,
            branch: draft.branch,
            host: host.clone(),
            target: target.clone(),
        });
        vec![Effect::CreatePullRequest {
            tag,
            host,
            repo: target,
            head,
            base,
            title: draft.title,
        }]
    }

    /// GitHub answered the create. A new pull request on the repository
    /// the badges come from shows its badge at once, and the badges are
    /// asked for again; one on another repository is only said.
    pub(crate) fn pull_request_created(
        &mut self,
        tag: crate::effect::Tag,
        result: Result<(u64, String), CreateError>,
    ) -> Vec<Effect> {
        self.user_done(tag);
        let Some(creating) = self.pr_create.take_if(|c| c.seq == tag.seq) else {
            return vec![];
        };
        if tag.generation != self.generation {
            return vec![];
        }
        match result {
            Ok((number, _)) => {
                let target = &creating.target;
                let on_badge_repo = self
                    .forge
                    .repo
                    .as_ref()
                    .is_some_and(|r| same_repo(r, target));
                if !on_badge_repo {
                    self.notice = Some(
                        format!("Created #{number} on {}/{}", target.owner, target.name).into(),
                    );
                    return vec![];
                }
                let badge = PrBadge {
                    number,
                    state: PrState::Open,
                    ci: None,
                };
                // Loads from here on are asked after the create.
                let known_from = self.next_seq;
                self.forge.add_created(creating.branch, badge, known_from);
                self.notice = Some(format!("Created pull request #{number}").into());
                self.forge.want_refresh();
                self.load_pull_requests()
            }
            Err(CreateError::RateLimited { until }) => {
                self.forge.record_rate_limit(&creating.host, until);
                self.notice = Some("GitHub is rate limiting: no pull request was created".into());
                vec![]
            }
            Err(error) => {
                self.push_error(ErrorPopup::new(
                    "Could not create the pull request",
                    &error.message(),
                    "",
                ));
                vec![]
            }
        }
    }

    /// Closing a step of a pull request stops it. The check details,
    /// review commit and review type lists belong to review mode, not to a
    /// draft.
    pub(crate) fn pull_requests_popup_closed(&mut self, closed: &Popup) {
        let step = match closed {
            Popup::PickList(list) => !matches!(
                list.purpose,
                PickPurpose::CheckDetails | PickPurpose::ReviewCommit | PickPurpose::ReviewEvent
            ),
            Popup::Prompt { kind, .. } => *kind == PromptKind::PullRequestTitle,
            Popup::Question(q) => q.scope == KeyScope::CreatePullRequest,
            _ => false,
        };
        if step {
            self.pr_draft = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;
    use crate::effect::Tag;
    use crate::intent::AppIntent;
    use crate::layout::SidePanel;
    use crate::menu::{self, MenuId};
    use crate::model::Focus;
    use crate::test_support::{load_list, load_remotes};
    use kika_forge::{CiState, ForgeHost, ForgeStatus, ForgeView};
    use kika_git::branches::{Branch, Track};
    use kika_git::log::CommitRow;
    use kika_git::remotes::{Remote, RemoteBranch};

    const OID: &str = "0123456789abcdef0123456789abcdef01234567";

    fn host(name: &str) -> ForgeHost {
        ForgeHost::parse(name).unwrap()
    }

    fn repo(owner: &str, name: &str) -> ForgeRepo {
        ForgeRepo {
            host: "github.com".into(),
            owner: owner.into(),
            name: name.into(),
        }
    }

    fn branch(name: &str, upstream: Option<&str>) -> Branch {
        Branch {
            name: name.into(),
            head: false,
            upstream: upstream.map(String::from),
            track: Track::None,
            merged: false,
        }
    }

    fn remote(name: &str, url: &str, branches: &[&str]) -> Remote {
        Remote {
            name: name.into(),
            url: Some(url.into()),
            configured: true,
            branches: branches
                .iter()
                .map(|short| RemoteBranch {
                    remote: name.into(),
                    short: short.as_bytes().to_vec(),
                    refname: format!("refs/remotes/{name}/{short}").into_bytes(),
                })
                .collect(),
        }
    }

    fn browser() -> Vec<OsString> {
        vec!["open".into(), "-a".into(), "My Browser".into()]
    }

    /// A workspace on github.com/acme/widgets with `topic` pushed to
    /// `origin` and selected in the Branches panel, a browser set and the
    /// badges' repository known.
    fn on_github(remotes: Vec<Remote>) -> AppState {
        let mut state = AppState::new(Context::Workspace)
            .with_forge_hosts(vec![host("github.com")])
            .with_forge_links(vec![host("gitlab.example.com")], Some(browser()));
        load_list(
            &mut state.branches.list,
            Ok(vec![
                branch("topic", Some("origin/topic")),
                branch("main", Some("origin/main")),
                branch("local", None),
            ]),
        );
        load_remotes(&mut state.branches.remotes.list, Ok(remotes));
        state.forge.repo = Some(repo("acme", "widgets"));
        state.forge.cli_checked([]);
        state.focus = Focus::Branches;
        state.expanded = SidePanel::Branches;
        state.files.loaded = true;
        state
    }

    fn origin(branches: &[&str]) -> Remote {
        remote("origin", "git@github.com:acme/widgets.git", branches)
    }

    fn one_remote() -> AppState {
        on_github(vec![origin(&["main", "topic"])])
    }

    fn select(state: &mut AppState, name: &str) {
        let at = state
            .branches
            .list
            .rows
            .iter()
            .position(|b| b.name == name.into());
        state.branches.list.selected = at.unwrap();
    }

    fn badge(number: u64) -> PrBadge {
        PrBadge {
            number,
            state: PrState::Open,
            ci: Some(CiState::Success),
        }
    }

    fn reason(state: &AppState, label: &str) -> Option<String> {
        let ids = [MenuId::PullRequest, MenuId::Copy];
        let item = ids
            .iter()
            .flat_map(|id| menu::items(*id))
            .find(|item| item.label == label)
            .unwrap_or_else(|| panic!("no item {label}"));
        (item.disabled)(state).map(String::from)
    }

    fn top_pick(state: &AppState) -> &PickList {
        match state.popups.last() {
            Some(Popup::PickList(list)) => list,
            other => panic!("expected a pick list, got {other:?}"),
        }
    }

    fn top_prompt(state: &AppState) -> (PromptKind, String) {
        match state.popups.last() {
            Some(Popup::Prompt { kind, input }) => (*kind, input.text.clone()),
            other => panic!("expected a prompt, got {other:?}"),
        }
    }

    /// The one `CreatePullRequest` in `effects`: tag, repo, head, base, title.
    fn created(effects: &[Effect]) -> (Tag, ForgeRepo, String, String, String) {
        match effects {
            [
                Effect::CreatePullRequest {
                    tag,
                    host,
                    repo,
                    head,
                    base,
                    title,
                },
            ] => {
                assert_eq!(host.as_str(), "github.com");
                (
                    *tag,
                    repo.clone(),
                    head.clone(),
                    base.clone(),
                    title.clone(),
                )
            }
            other => panic!("expected one create, got {other:?}"),
        }
    }

    /// The create question on top.
    fn asked(state: &AppState) -> &ConfirmQuestion {
        match state.popups.last() {
            Some(Popup::Question(q)) if q.scope == KeyScope::CreatePullRequest => q,
            other => panic!("expected the create question, got {other:?}"),
        }
    }

    fn press(state: &mut AppState, code: crossterm::event::KeyCode) -> Vec<Effect> {
        let key = crate::keymap::KeyPress::new(code, crossterm::event::KeyModifiers::NONE);
        state.update(AppIntent::Key(key))
    }

    fn press_char(state: &mut AppState, c: char) -> Vec<Effect> {
        press(state, crossterm::event::KeyCode::Char(c))
    }

    /// Answer the title prompt on top with `text`, as typed.
    fn answer_title(state: &mut AppState, text: &str) {
        match state.popups.last_mut() {
            Some(Popup::Prompt {
                kind: PromptKind::PullRequestTitle,
                input,
            }) => *input = LineInput::with_text(text),
            other => panic!("expected the title prompt, got {other:?}"),
        }
        state.update(AppIntent::SubmitPrompt);
    }

    #[test]
    fn copy_url_gives_the_github_pull_request_page() {
        let mut state = one_remote();
        state.forge.badges.insert("topic".into(), badge(7));
        let effects = state.update(AppIntent::PullRequests(PullRequestsIntent::CopyForgeUrl));
        let url = "https://github.com/acme/widgets/pull/7";
        assert_eq!(effects, [Effect::Clipboard(url.into())]);
        assert_eq!(
            state.notice.as_deref(),
            Some(format!("Copied URL: {}", crate::clipboard::preview(url)).as_str())
        );
    }

    #[test]
    fn copy_url_on_gitlab_gives_the_merge_requests_of_the_branch() {
        let mut state = on_github(vec![remote(
            "origin",
            "https://gitlab.example.com/grp/proj.git",
            &["main"],
        )]);
        let effects = state.update(AppIntent::PullRequests(PullRequestsIntent::CopyForgeUrl));
        assert_eq!(
            effects,
            [Effect::Clipboard(
                "https://gitlab.example.com/grp/proj/-/merge_requests?scope=all&source_branch=topic"
                    .into()
            )]
        );
        // No pull request number is needed there.
        assert_eq!(reason(&state, "Copy URL"), None);
        let unsupported = kika_forge::forge::Unsupported {
            kind: kika_forge::url::ForgeKind::GitLab,
            capability: Capability::CreatePull,
        };
        assert_eq!(
            reason(&state, "Create pull request…"),
            Some(unsupported.message())
        );
    }

    /// [`on_github`], but with `gitlab.example.com` asked through `glab`
    /// and the remote on it.
    fn on_gitlab_ask() -> AppState {
        let mut state = on_github(vec![remote(
            "origin",
            "git@gitlab.example.com:acme/widgets.git",
            &["main", "topic"],
        )]);
        let lab = ForgeHost::parse_gitlab("gitlab.example.com").unwrap();
        state.forge.hosts = vec![host("github.com"), lab];
        state.forge.repo = Some(ForgeRepo {
            host: "gitlab.example.com".into(),
            ..repo("acme", "widgets")
        });
        state
    }

    #[test]
    fn on_a_gitlab_ask_host_the_page_is_the_merge_request_list_and_create_is_off() {
        let mut state = on_gitlab_ask();
        // A badge is known, yet the page stays the filtered list.
        state.forge.badges.insert("topic".into(), badge(12));
        let list = "https://gitlab.example.com/acme/widgets/-/merge_requests?scope=all&source_branch=topic";
        let effects = state.update(AppIntent::PullRequests(PullRequestsIntent::OpenForgeUrl));
        let mut argv = browser();
        argv.push(list.into());
        assert_eq!(effects, [Effect::OpenUrl { argv }]);
        let effects = state.update(AppIntent::PullRequests(PullRequestsIntent::CopyForgeUrl));
        assert_eq!(effects, [Effect::Clipboard(list.into())]);
        // Create is refused before anything else is looked at.
        assert_eq!(
            reason(&state, "Create pull request…").as_deref(),
            Some("Creating a pull request is not available on GitLab yet")
        );
        state.forge.badges.clear();
        assert_eq!(
            reason(&state, "Create pull request…").as_deref(),
            Some("Creating a pull request is not available on GitLab yet")
        );
    }

    #[test]
    fn the_copy_menu_gives_the_selected_commits_page() {
        let mut state = one_remote();
        state.commits.list.rows = vec![CommitRow {
            oid: OID.into(),
            short_oid: "0123456".into(),
            parents: vec![],
            author: "a".into(),
            time: 0,
            subject: "s".into(),
            subject_altered: false,
        }];
        // The menu's own intent, as a user picks it.
        let copy_url = menu::items(MenuId::Copy)
            .iter()
            .find(|item| item.label == "Forge URL")
            .map(|item| item.intent.clone())
            .expect("the Copy menu has a Forge URL item");
        assert_eq!(reason(&state, "Forge URL"), None);
        let effects = state.update(copy_url.clone());
        let url = format!("https://github.com/acme/widgets/commit/{OID}");
        assert_eq!(effects, [Effect::Clipboard(url)]);
        assert!(
            state
                .notice
                .as_deref()
                .is_some_and(|t| t.starts_with("Copied URL: https://")),
            "{:?}",
            state.notice
        );

        state.branches.remotes.list.rows.clear();
        assert_eq!(reason(&state, "Forge URL").as_deref(), Some(NO_FORGE));
        assert_eq!(state.update(copy_url), vec![]);
        assert_eq!(state.notice.as_deref(), Some(NO_FORGE));
    }

    #[test]
    fn open_in_browser_hands_the_url_as_the_last_argument() {
        let mut state = one_remote();
        state.forge.badges.insert("topic".into(), badge(12));
        let effects = state.update(AppIntent::PullRequests(PullRequestsIntent::OpenForgeUrl));
        let mut argv = browser();
        argv.push("https://github.com/acme/widgets/pull/12".into());
        assert_eq!(effects, [Effect::OpenUrl { argv }]);
    }

    #[test]
    fn every_item_says_why_it_is_off() {
        let mut state = one_remote();
        // No badge: open and copy have nothing to show; create may run.
        assert_eq!(reason(&state, "Open in browser").as_deref(), Some(NO_PR));
        assert_eq!(reason(&state, "Copy URL").as_deref(), Some(NO_PR));
        assert_eq!(reason(&state, "Create pull request…"), None);

        state.forge.badges.insert("topic".into(), badge(3));
        assert_eq!(reason(&state, "Open in browser"), None);
        assert_eq!(
            reason(&state, "Create pull request…").as_deref(),
            Some("The branch already has pull request #3")
        );

        state.browser = None;
        assert_eq!(
            reason(&state, "Open in browser").as_deref(),
            Some(NO_BROWSER)
        );
        assert_eq!(
            state.update(AppIntent::PullRequests(PullRequestsIntent::OpenForgeUrl)),
            vec![]
        );
        assert_eq!(state.notice.as_deref(), Some(NO_BROWSER));
        assert_eq!(reason(&state, "Copy URL"), None);

        select(&mut state, "local");
        assert_eq!(
            reason(&state, "Create pull request…").as_deref(),
            Some(NO_UPSTREAM)
        );

        select(&mut state, "main");
        state.forge.limited.insert("github.com".into(), 1);
        assert_eq!(
            reason(&state, "Create pull request…").as_deref(),
            Some(RATE_LIMITED)
        );
        state.forge.limited.clear();
        state.pr_create = Some(PrCreating {
            seq: 1,
            branch: "x".into(),
            host: host("github.com"),
            target: repo("acme", "widgets"),
        });
        assert_eq!(
            reason(&state, "Create pull request…").as_deref(),
            Some(CREATING)
        );

        // No remote on a listed host: nothing at all.
        load_remotes(
            &mut state.branches.remotes.list,
            Ok(vec![remote("origin", "https://example.org/a/b", &[])]),
        );
        for label in ["Open in browser", "Copy URL", "Create pull request…"] {
            let want = if label == "Open in browser" {
                NO_BROWSER
            } else {
                NO_FORGE
            };
            assert_eq!(reason(&state, label).as_deref(), Some(want), "{label}");
        }
        state.browser = Some(browser());
        assert_eq!(reason(&state, "Open in browser").as_deref(), Some(NO_FORGE));
    }

    #[test]
    fn one_remote_and_one_base_go_straight_to_the_title() {
        let mut state = one_remote();
        state.update(AppIntent::PullRequests(PullRequestsIntent::CreateStart));
        // `topic` itself is no base, so `main` is the only one.
        assert_eq!(
            top_prompt(&state),
            (PromptKind::PullRequestTitle, "topic".into())
        );
        assert_eq!(state.popups.len(), 1);
        state.update(AppIntent::SubmitPrompt);
        assert_eq!(state.popups, [Popup::Question(create_question(&state))]);
        assert_eq!(
            asked(&state).text,
            "Press y to create it on GitHub, n or Esc to cancel\nacme/widgets: topic → main\ntopic"
        );
    }

    #[test]
    fn submitting_the_title_keeps_the_draft_for_the_question() {
        let mut state = one_remote();
        state.update(AppIntent::PullRequests(PullRequestsIntent::CreateStart));
        assert_eq!(state.update(AppIntent::SubmitPrompt), vec![]);
        assert_eq!(asked(&state).title, "Create pull request");
        assert_eq!(
            state.pr_draft.as_ref().map(|d| d.title.as_str()),
            Some("topic")
        );
    }

    #[test]
    fn a_remote_name_with_control_characters_is_drawn_safe() {
        let mut state = on_github(vec![
            origin(&["main", "topic"]),
            remote(
                "up\x1b[31m\u{9b}stream",
                "https://github.com/acme-org/widgets",
                &["main"],
            ),
        ]);
        state.update(AppIntent::PullRequests(PullRequestsIntent::CreateStart));
        let list = top_pick(&state);
        assert_eq!(list.rows.len(), 2);
        for row in &list.rows {
            assert!(!row.chars().any(char::is_control), "{row:?}");
        }
        assert!(
            list.rows[1].ends_with("  acme-org/widgets"),
            "{:?}",
            list.rows
        );
    }

    #[test]
    fn the_question_puts_the_answer_keys_first_and_cuts_a_long_title() {
        let mut state = one_remote();
        state.update(AppIntent::PullRequests(PullRequestsIntent::CreateStart));
        answer_title(&mut state, &"t".repeat(200));
        let question = asked(&state).text.clone();
        let lines: Vec<&str> = question.lines().collect();
        assert_eq!(
            lines[0],
            "Press y to create it on GitHub, n or Esc to cancel"
        );
        assert_eq!(lines[2], format!("{}…", "t".repeat(QUESTION_TITLE)));
        // The title sent is the whole one.
        let (.., title) = created(&press_char(&mut state, 'y'));
        assert_eq!(title, "t".repeat(200));
    }

    #[test]
    fn an_empty_title_is_asked_again() {
        let mut state = one_remote();
        state.update(AppIntent::PullRequests(PullRequestsIntent::CreateStart));
        answer_title(&mut state, "   ");
        assert_eq!(
            state.notice.as_deref(),
            Some("A pull request needs a title")
        );
        assert_eq!(
            top_prompt(&state),
            (PromptKind::PullRequestTitle, "   ".into())
        );
        assert_eq!(state.popups.len(), 1);
        answer_title(&mut state, "  Add login ");
        assert_eq!(state.pr_draft.as_ref().unwrap().title, "Add login");
        assert_eq!(state.popups, [Popup::Question(create_question(&state))]);
    }

    #[test]
    fn two_github_remotes_open_the_remote_picker_then_the_base_picker_on_main() {
        let mut state = on_github(vec![
            origin(&["dev", "main", "topic"]),
            remote(
                "upstream",
                "https://github.com/acme-org/widgets",
                &["HEAD", "a-first", "main", "master", "topic"],
            ),
            // Another host is never a target.
            remote("mirror", "https://gitlab.example.com/x/widgets", &["main"]),
        ]);
        state.update(AppIntent::PullRequests(PullRequestsIntent::CreateStart));
        let list = top_pick(&state);
        assert_eq!(list.purpose, PickPurpose::TargetRemote);
        assert_eq!(
            list.rows,
            ["origin  acme/widgets", "upstream  acme-org/widgets"]
        );
        state.update(AppIntent::PullRequests(PullRequestsIntent::PickNext));
        state.update(AppIntent::PullRequests(PullRequestsIntent::PickSelect));
        let list = top_pick(&state);
        assert_eq!(list.purpose, PickPurpose::BaseBranch);
        // On another remote, its own `topic` is a base like any other.
        assert_eq!(list.rows, ["a-first", "main", "master", "topic"]);
        assert_eq!(list.rows[list.cursor], "main");
        assert_eq!(state.popups.len(), 1);
    }

    #[test]
    fn pick_move_by_moves_the_cursor_clamped_and_never_picks() {
        let mut state = on_github(vec![
            origin(&["dev", "main", "topic"]),
            remote(
                "upstream",
                "https://github.com/acme-org/widgets",
                &["main", "topic"],
            ),
        ]);
        state.update(AppIntent::PullRequests(PullRequestsIntent::CreateStart));
        let by = |delta| AppIntent::PullRequests(PullRequestsIntent::PickMoveBy(delta));
        assert_eq!(state.update(by(9)), vec![], "moving never picks");
        assert_eq!(top_pick(&state).cursor, 1, "clamped at the end");
        assert_eq!(top_pick(&state).purpose, PickPurpose::TargetRemote);
        state.update(by(-9));
        assert_eq!(top_pick(&state).cursor, 0, "clamped at the start");
        assert_eq!(state.popups.len(), 1);
    }

    #[test]
    fn enter_never_answers_the_question_and_y_creates_with_the_owner_prefix_from_a_fork() {
        use crossterm::event::KeyCode;
        let mut state = on_github(vec![
            origin(&["main", "topic"]),
            remote(
                "upstream",
                "https://github.com/acme-org/widgets",
                &["main", "dev"],
            ),
        ]);
        state.update(AppIntent::PullRequests(PullRequestsIntent::CreateStart));
        state.update(AppIntent::PullRequests(PullRequestsIntent::PickNext));
        state.update(AppIntent::PullRequests(PullRequestsIntent::PickSelect));
        state.update(AppIntent::PullRequests(PullRequestsIntent::PickSelect));
        answer_title(&mut state, "Add login");
        assert_eq!(
            state.key_focus(),
            crate::keymap::KeyScope::CreatePullRequest
        );
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        // `ConfirmYes` cannot answer it either.
        assert_eq!(state.update(AppIntent::ConfirmYes), vec![]);
        assert_eq!(state.popups, [Popup::Question(create_question(&state))]);
        assert!(
            asked(&state)
                .text
                .ends_with("\nacme-org/widgets: acme:topic → main\nAdd login")
        );

        let busy = state.busy_count();
        let (tag, target, head, base, title) = created(&press_char(&mut state, 'y'));
        assert!(tag.user);
        assert_eq!(state.busy_count(), busy + 1);
        assert_eq!(target, repo("acme-org", "widgets"));
        assert_eq!(
            (head.as_str(), base.as_str(), title.as_str()),
            ("acme:topic", "main", "Add login")
        );
        assert!(state.popups.is_empty());
        assert_eq!(state.pr_draft, None);
        assert!(state.pr_create.is_some());
    }

    #[test]
    fn y_from_the_same_repository_sends_the_bare_head_and_n_cancels() {
        let mut state = one_remote();
        state.update(AppIntent::PullRequests(PullRequestsIntent::CreateStart));
        state.update(AppIntent::SubmitPrompt);
        assert_eq!(press_char(&mut state, 'n'), vec![]);
        assert!(state.popups.is_empty());
        assert_eq!(state.pr_draft, None);

        state.update(AppIntent::PullRequests(PullRequestsIntent::CreateStart));
        state.update(AppIntent::SubmitPrompt);
        let (_, target, head, base, _) = created(&press_char(&mut state, 'y'));
        assert_eq!(target, repo("acme", "widgets"));
        assert_eq!((head.as_str(), base.as_str()), ("topic", "main"));
    }

    #[test]
    fn esc_on_a_picker_or_the_title_stops_the_draft() {
        let mut state = on_github(vec![
            origin(&["main", "topic"]),
            remote("upstream", "https://github.com/acme-org/widgets", &["main"]),
        ]);
        state.update(AppIntent::PullRequests(PullRequestsIntent::CreateStart));
        assert!(state.pr_draft.is_some());
        press(&mut state, crossterm::event::KeyCode::Esc);
        assert!(state.popups.is_empty());
        assert_eq!(state.pr_draft, None);
    }

    /// `remotes` on github.com, which `[forge.allow]` narrows to `acme`.
    fn acme_only(remotes: Vec<Remote>) -> AppState {
        let mut state = on_github(remotes);
        let rule = kika_forge::allow::AllowRule::parse("acme").unwrap();
        state.forge.hosts = vec![host("github.com").with_allow(vec![rule])];
        state
    }

    #[test]
    fn a_target_the_allow_list_leaves_out_is_refused() {
        let mut state = acme_only(vec![remote(
            "origin",
            "git@github.com:other/repo.git",
            &["main", "topic"],
        )]);
        assert_eq!(
            state.update(AppIntent::PullRequests(PullRequestsIntent::CreateStart)),
            vec![]
        );
        assert_eq!(state.notice.as_deref(), Some(NOT_ALLOWED));
        assert_eq!(state.pr_draft, None);
        assert!(state.popups.is_empty());
    }

    #[test]
    fn a_fork_into_an_allowed_target_proceeds_and_the_unlisted_fork_is_no_target() {
        let mut state = acme_only(vec![
            remote("origin", "git@github.com:me/repo.git", &["main", "topic"]),
            remote("upstream", "https://github.com/acme/repo", &["main"]),
        ]);
        state.update(AppIntent::PullRequests(PullRequestsIntent::CreateStart));
        let draft = state.pr_draft.as_ref().unwrap();
        assert_eq!(draft.targets, [("upstream".into(), repo("acme", "repo"))]);
        state.update(AppIntent::SubmitPrompt);
        let (_, target, head, base, _) = created(&press_char(&mut state, 'y'));
        assert_eq!(target, repo("acme", "repo"));
        assert_eq!((head.as_str(), base.as_str()), ("me:topic", "main"));
    }

    /// Run a create from `state` to its effect; the effect's tag.
    fn start_create(state: &mut AppState) -> Tag {
        state.update(AppIntent::PullRequests(PullRequestsIntent::CreateStart));
        state.update(AppIntent::SubmitPrompt);
        created(&press_char(state, 'y')).0
    }

    #[test]
    fn a_created_pull_request_shows_its_badge_at_once_and_refreshes() {
        let mut state = one_remote();
        let tag = start_create(&mut state);
        let effects = state.update(AppIntent::PullRequests(PullRequestsIntent::Created {
            tag,
            result: Ok((42, "https://github.com/acme/widgets/pull/42".into())),
        }));
        let want = PrBadge {
            number: 42,
            state: PrState::Open,
            ci: None,
        };
        assert_eq!(state.forge.badges.get(&"topic".into()), Some(&want));
        assert!(
            matches!(
                effects.as_slice(),
                [Effect::LoadPullRequests { refresh: true, .. }]
            ),
            "{effects:?}"
        );
        assert_eq!(state.pr_create, None);
        let rows = crate::test_support::draw_branches(&state, 80, 12);
        assert!(
            rows.iter().any(|r| r.contains("topic  #42 open")),
            "{rows:#?}"
        );

        // The refresh's cache read does not know it yet; the badge stays.
        let Some(Effect::LoadPullRequests { tag: load, .. }) = effects.first() else {
            unreachable!()
        };
        state.update(AppIntent::PullRequests(PullRequestsIntent::Cached {
            tag: *load,
            repo: repo("acme", "widgets"),
            badges: Some(Default::default()),
        }));
        assert_eq!(state.forge.badges.get(&"topic".into()), Some(&want));
        // Its answer does, and takes over.
        state.update(AppIntent::PullRequests(PullRequestsIntent::Fetched {
            tag: *load,
            result: Ok([("topic".into(), badge(42))].into()),
            limited_until: None,
        }));
        assert_eq!(state.forge.badges.get(&"topic".into()), Some(&badge(42)));
        assert!(state.forge.created.is_empty());
    }

    #[test]
    fn a_pull_request_on_another_repository_is_only_said() {
        let mut state = on_github(vec![
            origin(&["main", "topic"]),
            remote("upstream", "https://github.com/acme-org/widgets", &["main"]),
        ]);
        state.update(AppIntent::PullRequests(PullRequestsIntent::CreateStart));
        state.update(AppIntent::PullRequests(PullRequestsIntent::PickNext));
        state.update(AppIntent::PullRequests(PullRequestsIntent::PickSelect));
        state.update(AppIntent::SubmitPrompt);
        let (tag, ..) = created(&press_char(&mut state, 'y'));
        let effects = state.update(AppIntent::PullRequests(PullRequestsIntent::Created {
            tag,
            result: Ok((5, "https://github.com/acme-org/widgets/pull/5".into())),
        }));
        assert_eq!(effects, vec![]);
        assert!(state.forge.badges.is_empty());
        assert_eq!(
            state.notice.as_deref(),
            Some("Created #5 on acme-org/widgets")
        );
    }

    #[test]
    fn a_refused_create_shows_githubs_words_and_a_rate_limit_is_kept() {
        let mut state = one_remote();
        let busy = state.busy_count();
        let tag = start_create(&mut state);
        state.update(AppIntent::PullRequests(PullRequestsIntent::Created {
            tag,
            result: Err(CreateError::Refused(
                "Validation Failed: A pull request already exists for acme:topic.".into(),
            )),
        }));
        assert_eq!(state.busy_count(), busy);
        match state.popups.as_slice() {
            [Popup::Error(error)] => {
                assert_eq!(error.title, "Could not create the pull request");
                assert!(error.message.contains("already exists"), "{error:?}");
            }
            other => panic!("{other:?}"),
        }

        let mut state = one_remote();
        let tag = start_create(&mut state);
        state.update(AppIntent::PullRequests(PullRequestsIntent::Created {
            tag,
            result: Err(CreateError::RateLimited {
                until: 1_700_000_100,
            }),
        }));
        assert!(state.popups.is_empty());
        assert_eq!(state.forge.limited.get("github.com"), Some(&1_700_000_100));
        assert_eq!(state.forge.limited_until, Some(1_700_000_100));
        assert_eq!(
            state.forge.status,
            ForgeView::Problem(ForgeStatus::RateLimited)
        );
        assert_eq!(
            reason(&state, "Create pull request…").as_deref(),
            Some(RATE_LIMITED)
        );
    }

    #[test]
    fn an_old_answer_changes_nothing() {
        let mut state = one_remote();
        let tag = start_create(&mut state);
        let stale = Tag {
            seq: tag.seq + 1,
            ..tag
        };
        state.update(AppIntent::PullRequests(PullRequestsIntent::Created {
            tag: stale,
            result: Ok((1, String::new())),
        }));
        assert!(state.forge.badges.is_empty());
        assert!(state.pr_create.is_some());
    }
}
