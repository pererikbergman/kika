//! Popups that sit on one stack over the workspace: menus, confirmations,
//! one-line prompts and errors. Pure types; the model owns the stack and
//! each popup hands its result back as an intent.

use std::borrow::Cow;
use std::fmt;

use crossterm::event::{KeyCode, KeyModifiers};

use crate::confirm_question::{Answer, ConfirmQuestion, Then};
use crate::features::branches::BranchesIntent;
use crate::features::branches::remotes::RemotesIntent;
use crate::features::branches::tags::TagsIntent;
use crate::features::command_log::CommandLogView;
use crate::features::commits::CommitsIntent;
use crate::features::commits::custom_patch::CustomPatchIntent;
use crate::features::custom_commands::CustomCommandsIntent;
use crate::features::files::FilesIntent;
use crate::features::pull_requests::PullRequestsIntent;
use crate::features::repos::RecentList;
use crate::features::stash::StashIntent;
use crate::features::submodules::SubmoduleList;
use crate::features::worktrees::WorktreeList;
use crate::features::worktrees::WorktreesIntent;
use crate::intent::AppIntent;
use crate::keymap::{KeyPress, KeyScope};
use crate::menu::MenuId;
use crate::op::GitOp;
use crate::pick_list::PickList;
use kika_forge::forge::{Words, merge_label};
use kika_forge::merge::MergeMethod;
use kika_forge::{ForgeHost, ForgeRepo};
use kika_git::askpass::Secret;
use kika_git::commit_files::CommitId;
use kika_git::safe_text::terminal_safe;

/// One entry on the popup stack. The top one takes the keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Popup {
    Menu {
        id: MenuId,
        cursor: usize,
    },
    Prompt {
        kind: PromptKind,
        input: LineInput,
    },
    Error(ErrorPopup),
    /// A question that only its own yes keys answer, such as a discard, a
    /// delete or a hard reset.
    Question(ConfirmQuestion),
    Submodules(SubmoduleList),
    Worktrees(WorktreeList),
    Recent(RecentList),
    /// One row to pick, such as a pull request's target remote.
    PickList(PickList),
    /// Git or ssh asks something for a running fetch, pull or push.
    Credential(CredentialPrompt),
    /// The git commands Kika ran; the entries live in `AppState`.
    CommandLog(CommandLogView),
}

/// A question from git or ssh, and what the user has typed so far. The
/// text leaves only in the answer; it is never kept anywhere else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialPrompt {
    /// Git's or ssh's words, terminal-safe.
    pub prompt: String,
    /// When the typed text is drawn as it is rather than as dots.
    pub echo: Echo,
    /// Ssh's yes/no kind, answered with y or n instead of text.
    pub confirm: bool,
    pub input: SecretInput,
}

/// How a credential prompt draws what is typed. On older ssh a server can
/// end its own prompt with ssh's host-key sentence, so the words of a
/// prompt cannot prove it is the real host-key question; there the typed
/// text itself decides, and only a plausible answer is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Echo {
    /// Always dots: passwords, passphrases and any prompt a server chose.
    Masked,
    /// Always as typed: git's username question.
    Clear,
    /// As typed only while the text could be a host-key answer: a start of
    /// `yes` or `no`, or an ssh SHA256 fingerprint. Anything else is dots.
    HostKeyAnswer,
}

impl CredentialPrompt {
    /// Only git's username question is always shown as typed, and ssh's
    /// new-host-key question shows a plausible answer; both are matched on
    /// their exact words (the start of git's, the whole closing sentence of
    /// ssh's), so a prompt a server chose cannot ask for a secret with the
    /// text in sight. Ssh puts `(user@host) ` before a server's own prompts,
    /// so one starting with `(` stays masked however it ends. A bare
    /// `Username:` is not git's question and stays masked on purpose.
    pub fn new(prompt: String, confirm: bool) -> Self {
        let echo = if prompt.starts_with("Username for '") {
            Echo::Clear
        } else if is_host_key_question(&prompt) {
            Echo::HostKeyAnswer
        } else {
            Echo::Masked
        };
        Self {
            prompt,
            echo,
            confirm,
            input: SecretInput::default(),
        }
    }

    /// Whether the text typed so far is drawn as it is. Decided per frame,
    /// so a host-key answer turns to dots as soon as it stops being one.
    pub fn shows_typed_text(&self) -> bool {
        match self.echo {
            Echo::Masked => false,
            Echo::Clear => true,
            Echo::HostKeyAnswer => is_host_key_answer(&self.input.line().text),
        }
    }
}

/// A start of an answer to ssh's host-key question: exactly a prefix of
/// `yes` or `no`, or of an ssh SHA256 fingerprint (`SHA256:` and at most 43
/// base64 chars, one `=` allowed after them). Case and whitespace are not
/// forgiven, as showing less is the safe side.
fn is_host_key_answer(text: &str) -> bool {
    const TAG: &str = "SHA256:";
    const HASH_CHARS: usize = 43;
    if "yes".starts_with(text) || "no".starts_with(text) || TAG.starts_with(text) {
        return true;
    }
    let Some(hash) = text.strip_prefix(TAG) else {
        return false;
    };
    let hash = hash.strip_suffix('=').unwrap_or(hash);
    (1..=HASH_CHARS).contains(&hash.len())
        && hash
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
}

/// Ssh's new-host-key question: it ends with ssh's whole closing sentence,
/// after which nothing more can be asked, and is not a server prompt.
fn is_host_key_question(prompt: &str) -> bool {
    const ENDINGS: [&str; 2] = [
        "want to continue connecting (yes/no/[fingerprint])?",
        // Older ssh asks without the fingerprint choice.
        "want to continue connecting (yes/no)?",
    ];
    let prompt = prompt.trim_end();
    !prompt.starts_with('(') && ENDINGS.iter().any(|ending| prompt.ends_with(ending))
}

/// A [`LineInput`] that may hold a password, so its `Debug` hides the text.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SecretInput(LineInput);

impl SecretInput {
    pub fn edit(&mut self, key: KeyPress) {
        self.0.edit(key);
    }

    /// The typed text and cursor, only for drawing it.
    pub fn line(&self) -> &LineInput {
        &self.0
    }

    /// The typed text, as the answer to send.
    pub fn secret(&self) -> Secret {
        Secret::new(self.0.text.clone())
    }
}

impl fmt::Debug for SecretInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretInput(..)")
    }
}

/// What a discard throws away, so the question can say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscardWhat {
    File,
    Untracked,
    Hunk,
    Lines,
    /// A range of files: `restore` lose their unstaged changes and
    /// `delete` untracked ones are deleted.
    Range {
        restore: usize,
        delete: usize,
    },
}

impl DiscardWhat {
    pub fn title(self) -> Cow<'static, str> {
        match self {
            DiscardWhat::File => "Discard file".into(),
            DiscardWhat::Untracked => "Delete file".into(),
            DiscardWhat::Hunk => "Discard hunk".into(),
            DiscardWhat::Lines => "Discard lines".into(),
            DiscardWhat::Range { restore, delete } => {
                format!("Discard {}", files(restore + delete)).into()
            }
        }
    }

    pub fn question(self) -> Cow<'static, str> {
        match self {
            DiscardWhat::File => "Discard all unstaged changes to this file? (y/n)".into(),
            DiscardWhat::Untracked => "Delete this untracked file from disk? (y/n)".into(),
            DiscardWhat::Hunk => "Discard this hunk from the working tree? (y/n)".into(),
            DiscardWhat::Lines => "Discard these lines from the working tree? (y/n)".into(),
            DiscardWhat::Range { restore, delete: 0 } => {
                format!("Discard unstaged changes to {}? (y/n)", files(restore)).into()
            }
            DiscardWhat::Range { restore: 0, delete } => {
                format!("Delete {} from disk? (y/n)", untracked(delete)).into()
            }
            DiscardWhat::Range { restore, delete } => format!(
                "Discard unstaged changes to {} and delete {}? (y/n)",
                files(restore),
                untracked(delete)
            )
            .into(),
        }
    }

    /// The question, then the file's display path (already terminal-safe)
    /// on its own line; a range has no one path, so its counts are the
    /// question. `op` is fixed now, so a refresh before the answer cannot
    /// change what goes, and only `y` answers.
    pub fn into_question(self, path: String, op: GitOp) -> ConfirmQuestion {
        let text = if path.is_empty() {
            self.question().into_owned()
        } else {
            format!("{}\n{path}", self.question())
        };
        ConfirmQuestion {
            scope: KeyScope::DiscardConfirm,
            title: self.title(),
            text,
            answer: Answer::YOnly,
            then: Then::Op(op),
            range: matches!(self, DiscardWhat::Range { .. }),
            head: None,
        }
    }
}

/// `1 file` or `n files`.
fn files(n: usize) -> String {
    if n == 1 {
        "1 file".to_string()
    } else {
        format!("{n} files")
    }
}

/// `1 untracked file` or `n untracked files`.
fn untracked(n: usize) -> String {
    format!("{n} untracked {}", if n == 1 { "file" } else { "files" })
}

/// "Leave these commits behind?" Asked before a detached `HEAD` moves off
/// commits that no branch, tag or remote-tracking branch reaches. It
/// carries the op planned when it was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaveCommitsConfirm {
    pub op: GitOp,
    /// The detached `HEAD` the commits were found from, so `y` runs
    /// nothing once `HEAD` has moved.
    pub head: CommitId,
    /// `head`'s short id, as the log shows it: the question names it so
    /// the user can branch there before leaving.
    pub head_short_oid: String,
    /// The target as the user sees it: a commit's short id, as the log
    /// shows it, or the branch `HEAD` lands on.
    pub target: String,
    /// The short ids of the commits left behind, newest first.
    pub left: Vec<String>,
}

impl LeaveCommitsConfirm {
    /// How many of the left-behind ids the question names.
    const LISTED: usize = 5;

    pub fn title(&self) -> &'static str {
        "Leave commits behind"
    }

    pub fn question(&self) -> String {
        use crate::op::ResetMode;
        let count = self.left.len();
        let (noun, are) = if count == 1 {
            ("commit", "is")
        } else {
            ("commits", "are")
        };
        let mut ids = self.left[..count.min(Self::LISTED)].join(", ");
        if count > Self::LISTED {
            ids.push_str(&format!(" and {} more", count - Self::LISTED));
        }
        let head = &self.head_short_oid;
        // Review mode has no Commits panel: `n` leaves the user in review,
        // so the hint says how to get back to where `N` works.
        let keep = match &self.op {
            GitOp::CheckoutPrBranch { .. } => format!(
                "To keep them, press n, then esc until you are back in the workspace, \
                 then N in Commits to create a branch at {head}"
            ),
            _ => format!("To keep them, press n, then N to create a branch at {head}"),
        };
        let target = &self.target;
        match &self.op {
            // The warning comes first, so a small terminal that clips the
            // end still shows it.
            GitOp::Reset {
                mode: ResetMode::Hard,
                ..
            } => format!(
                "Reset hard to {target}: every uncommitted change, staged and unstaged, \
                 is thrown away, and {count} {noun} that no branch or tag reaches {are} left \
                 behind ({ids}). Afterwards only the reflog has them. {keep} (y/n)"
            ),
            op => {
                let action = match op {
                    GitOp::Reset { .. } => "Reset to",
                    _ => "Check out",
                };
                format!(
                    "{action} {target} and leave {count} {noun} that no branch or tag \
                     reaches behind ({ids})? Afterwards only the reflog has them. {keep} (y/n)"
                )
            }
        }
    }

    /// The question `y` alone answers. It runs nothing once `HEAD` has
    /// moved off `head`.
    pub fn into_question(self) -> ConfirmQuestion {
        ConfirmQuestion {
            scope: KeyScope::LeaveCommits,
            title: self.title().into(),
            text: self.question(),
            answer: Answer::YOnly,
            range: false,
            head: Some(self.head),
            then: Then::Op(self.op),
        }
    }
}

/// "Merge this pull request?" Every value is taken when it is asked, so
/// a refresh before the answer cannot change what is merged; the head
/// commit guards the merge on the forge's side too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergePrConfirm {
    pub number: u64,
    /// The title, base and head as parsed, already terminal-safe.
    pub title: String,
    pub base: String,
    pub head: String,
    pub method: MergeMethod,
    pub head_oid: CommitId,
    pub repo: ForgeRepo,
    pub host: ForgeHost,
    /// A check that is not required has not passed.
    pub unstable: bool,
    /// How the forge names the request, its number and itself.
    pub words: &'static Words,
}

/// The most of a title the merge question shows, in characters.
const MERGE_TITLE: usize = 72;

impl MergePrConfirm {
    pub fn title(&self) -> String {
        format!("Merge {}", self.words.request)
    }

    /// What to press first, so a short screen cuts the details rather
    /// than the answer; then what is merged into what, and how.
    pub fn question(&self) -> String {
        let title = terminal_safe(self.title.as_bytes());
        let title = match title.char_indices().nth(MERGE_TITLE) {
            Some((cut, _)) => format!("{}…", &title[..cut]),
            None => title,
        };
        let Words { name, sign, .. } = self.words;
        let mut text = format!(
            "Press y to merge it on {name}, n or Esc to cancel\n\
             Merge {sign}{} {title} into {} by {}?",
            self.number,
            terminal_safe(self.base.as_bytes()),
            merge_label(Some(self.host.kind()), self.method),
        );
        if self.unstable {
            text.push_str("\nSome checks have not passed");
        }
        text
    }

    /// The question `y` alone answers, since it reaches the forge.
    pub fn into_question(self) -> ConfirmQuestion {
        ConfirmQuestion {
            scope: KeyScope::MergePullRequest,
            title: self.title().into(),
            text: self.question(),
            answer: Answer::YOnly,
            then: Then::MergePr(Box::new(self)),
            range: false,
            head: None,
        }
    }
}

/// What a range delete acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeKind {
    Branches,
    Tags,
    Remotes,
    Stashes,
}

/// "Delete these?" over a range. It carries the op chosen when it was
/// asked, so a reload before the answer cannot change the targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteRangeConfirm {
    pub kind: RangeKind,
    /// The display names of what goes, already terminal-safe.
    pub names: Vec<String>,
    /// Why some rows of the range stay, and how many each.
    pub skipped: Vec<(String, usize)>,
    pub op: GitOp,
}

impl DeleteRangeConfirm {
    /// How many names the question lists before `and N more`.
    const LISTED: usize = 5;

    pub fn title(&self) -> &'static str {
        match self.kind {
            RangeKind::Branches => "Delete branches",
            RangeKind::Tags => "Delete tags",
            RangeKind::Remotes => "Remove remotes",
            RangeKind::Stashes => "Drop stashes",
        }
    }

    pub fn question(&self) -> String {
        let count = self.names.len();
        let one = count == 1;
        let (verb, noun) = match (self.kind, one) {
            (RangeKind::Branches, true) => ("Delete", "branch"),
            (RangeKind::Branches, false) => ("Delete", "branches"),
            (RangeKind::Tags, true) => ("Delete", "tag"),
            (RangeKind::Tags, false) => ("Delete", "tags"),
            (RangeKind::Remotes, true) => ("Remove", "remote"),
            (RangeKind::Remotes, false) => ("Remove", "remotes"),
            (RangeKind::Stashes, true) => ("Drop", "stash"),
            (RangeKind::Stashes, false) => ("Drop", "stashes"),
        };
        let mut names = self.names[..count.min(Self::LISTED)].join(", ");
        if count > Self::LISTED {
            names.push_str(&format!(" and {} more", count - Self::LISTED));
        }
        let note = match (self.kind, one) {
            (RangeKind::Branches, _) => String::new(),
            (RangeKind::Tags, true) => " It is not deleted on any remote.".into(),
            (RangeKind::Tags, false) => " They are not deleted on any remote.".into(),
            (RangeKind::Remotes, true) => " Its remote-tracking branches are deleted, and \
                branches tracking it lose their upstream."
                .into(),
            (RangeKind::Remotes, false) => " Their remote-tracking branches are deleted, and \
                branches tracking them lose their upstream."
                .into(),
            (RangeKind::Stashes, true) => " It cannot be recovered from the stash list.".into(),
            (RangeKind::Stashes, false) => " They cannot be recovered from the stash list.".into(),
        };
        let kept: Vec<String> = self
            .skipped
            .iter()
            .filter(|(_, n)| *n > 0)
            .map(|(why, n)| format!("{n} {why}"))
            .collect();
        let kept = match kept.is_empty() {
            true => String::new(),
            false => format!(" Kept: {}.", kept.join(", ")),
        };
        format!("{verb} {count} {noun} ({names})?{note}{kept} (y/n)")
    }

    /// The question `y` alone answers; a yes forgets the range.
    pub fn into_question(self) -> ConfirmQuestion {
        ConfirmQuestion {
            scope: KeyScope::DeleteRange,
            title: self.title().into(),
            text: self.question(),
            answer: Answer::YOnly,
            range: true,
            head: None,
            then: Then::Op(self.op),
        }
    }
}

/// A one-line text question. The typed text comes back as an intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    GoToFile,
    /// One of a custom command's prompts. Its title comes from the
    /// command, so [`AppState::prompt_title`](crate::model::AppState::prompt_title)
    /// gives it; this one is only a fallback.
    Command,
    /// The name of a branch to make at the selected one.
    NewBranch,
    /// The name of a branch to make at the detached `HEAD`.
    NewBranchAtHead,
    /// A new name for the selected branch.
    RenameBranch,
    /// The message of a stash being made; empty for git's own.
    StashMessage,
    /// A new message for the selected stash.
    RenameStash,
    /// The name of a local branch to track the selected remote branch.
    TrackBranch,
    /// A new remote's name.
    RemoteName,
    /// A new remote's URL, asked after its name.
    RemoteUrl,
    /// A new name for the selected remote.
    RenameRemote,
    /// The remote branch the selected local branch should track.
    SetUpstream,
    /// Where to add a worktree for the branch `a` was pressed on.
    WorktreePath,
    /// A new tag's name.
    TagName,
    /// An annotated tag's message, asked after its name.
    TagMessage,
    /// The pattern to add to `.gitignore`, filled with the selected file's.
    IgnorePattern,
    /// A new pull request's title, filled with the branch name.
    PullRequestTitle,
    /// The message of the commit the custom patch is split off into.
    PatchCommitMessage,
}

impl PromptKind {
    pub fn title(self) -> &'static str {
        match self {
            PromptKind::GoToFile => "Go to file",
            PromptKind::Command => "Command",
            PromptKind::NewBranch | PromptKind::NewBranchAtHead => "New branch",
            PromptKind::RenameBranch => "Rename branch",
            PromptKind::StashMessage => "Stash message (optional)",
            PromptKind::RenameStash => "Rename stash",
            PromptKind::TrackBranch => "Check out as",
            PromptKind::RemoteName => "New remote name",
            PromptKind::RemoteUrl => "New remote URL",
            PromptKind::RenameRemote => "Rename remote",
            PromptKind::SetUpstream => "Set upstream",
            PromptKind::WorktreePath => "New worktree",
            PromptKind::TagName => "New tag",
            PromptKind::TagMessage => "Tag message",
            PromptKind::IgnorePattern => "Add to .gitignore",
            PromptKind::PullRequestTitle => "Pull request title",
            PromptKind::PatchCommitMessage => "New commit message",
        }
    }

    pub fn submit(self, text: String) -> AppIntent {
        match self {
            PromptKind::GoToFile => AppIntent::Files(FilesIntent::GoToFile(text)),
            PromptKind::Command => {
                AppIntent::CustomCommands(CustomCommandsIntent::PromptAnswered(text))
            }
            PromptKind::NewBranch => AppIntent::Branches(BranchesIntent::CreateNamed(text)),
            PromptKind::NewBranchAtHead => {
                AppIntent::Branches(BranchesIntent::CreateAtHeadNamed(text))
            }
            PromptKind::RenameBranch => AppIntent::Branches(BranchesIntent::RenameTo(text)),
            PromptKind::StashMessage => AppIntent::Stash(StashIntent::MessageAnswered(text)),
            PromptKind::RenameStash => AppIntent::Stash(StashIntent::RenameTo(text)),
            PromptKind::TrackBranch => {
                AppIntent::Branches(BranchesIntent::Remotes(RemotesIntent::TrackNamed(text)))
            }
            PromptKind::RemoteName => {
                AppIntent::Branches(BranchesIntent::Remotes(RemotesIntent::AddNamed(text)))
            }
            PromptKind::RemoteUrl => {
                AppIntent::Branches(BranchesIntent::Remotes(RemotesIntent::AddUrl(text)))
            }
            PromptKind::RenameRemote => {
                AppIntent::Branches(BranchesIntent::Remotes(RemotesIntent::RenameTo(text)))
            }
            PromptKind::SetUpstream => AppIntent::Branches(BranchesIntent::SetUpstreamTo(text)),
            PromptKind::WorktreePath => AppIntent::Worktrees(WorktreesIntent::AddAt(text)),
            PromptKind::TagName => {
                AppIntent::Branches(BranchesIntent::Tags(TagsIntent::Named(text)))
            }
            PromptKind::TagMessage => {
                AppIntent::Branches(BranchesIntent::Tags(TagsIntent::Messaged(text)))
            }
            PromptKind::IgnorePattern => AppIntent::Files(FilesIntent::IgnorePatternAnswered(text)),
            PromptKind::PullRequestTitle => {
                AppIntent::PullRequests(PullRequestsIntent::Titled(text))
            }
            PromptKind::PatchCommitMessage => AppIntent::Commits(CommitsIntent::CustomPatch(
                CustomPatchIntent::CommitMessage(text),
            )),
        }
    }
}

/// One line of text being typed. `col` counts chars, not bytes, and always
/// points inside the text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineInput {
    pub text: String,
    pub col: usize,
}

impl LineInput {
    /// `text` already typed, with the cursor after it.
    pub fn with_text(text: &str) -> Self {
        Self {
            text: text.to_string(),
            col: char_len(text),
        }
    }

    /// Apply one key. Keys that mean nothing on one line are ignored.
    pub fn edit(&mut self, key: KeyPress) {
        // As in a shell: ctrl-u clears up to the cursor, so a prefilled
        // line goes in one key.
        if key.code == KeyCode::Char('u') && key.modifiers == KeyModifiers::CONTROL {
            self.text.drain(..byte_at(&self.text, self.col));
            self.col = 0;
            return;
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return;
        }
        match key.code {
            KeyCode::Char(c) if !c.is_control() => {
                self.text.insert(byte_at(&self.text, self.col), c);
                self.col += 1;
            }
            KeyCode::Backspace if self.col > 0 => {
                self.col -= 1;
                self.text.remove(byte_at(&self.text, self.col));
            }
            KeyCode::Delete if self.col < char_len(&self.text) => {
                self.text.remove(byte_at(&self.text, self.col));
            }
            KeyCode::Left => self.col = self.col.saturating_sub(1),
            KeyCode::Right => self.col = (self.col + 1).min(char_len(&self.text)),
            KeyCode::Home => self.col = 0,
            KeyCode::End => self.col = char_len(&self.text),
            _ => {}
        }
    }
}

pub(crate) fn char_len(text: &str) -> usize {
    text.chars().count()
}

/// The byte offset of char `col` in `text`, or its end.
pub(crate) fn byte_at(text: &str, col: usize) -> usize {
    text.char_indices().nth(col).map_or(text.len(), |(i, _)| i)
}

/// Something failed: what, git's own words, and what to do now. A
/// command's captured output is shown the same way, marked as no failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorPopup {
    pub title: String,
    pub message: String,
    /// Usually fixed text; owned when it names a key from the user's keymap.
    pub next: Cow<'static, str>,
    /// Only a failure gets the error colour; a command's own output reuses
    /// this popup and must not look like one.
    pub failed: bool,
}

impl ErrorPopup {
    /// The message is made safe to draw here, whatever its source, since it
    /// can quote repo-controlled paths.
    pub fn new(
        title: impl Into<String>,
        message: &str,
        next: impl Into<Cow<'static, str>>,
    ) -> Self {
        Self {
            title: title.into(),
            message: terminal_safe(message.as_bytes()),
            next: next.into(),
            failed: true,
        }
    }

    /// What a command that succeeded printed: no next step, plain border.
    pub fn output(title: impl Into<String>, message: &str) -> Self {
        Self {
            failed: false,
            ..Self::new(title, message, "")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::files::FilesIntent;
    use crate::features::operation::OperationIntent;
    use crate::features::sync::SyncIntent;

    fn press(input: &mut LineInput, code: KeyCode) {
        input.edit(KeyPress::new(code, KeyModifiers::NONE));
    }

    fn typed(text: &str) -> LineInput {
        let mut input = LineInput::default();
        for c in text.chars() {
            press(&mut input, KeyCode::Char(c));
        }
        input
    }

    #[test]
    fn the_abort_question_names_the_operation_and_yes_aborts_that_one() {
        use crate::features::operation::abort_question;
        use kika_git::repo_state::Operation;
        let yes = |op| {
            Then::Intent(Box::new(AppIntent::Operation(
                OperationIntent::ConfirmAbort(op),
            )))
        };
        let ask = abort_question(Operation::CherryPick).unwrap();
        assert_eq!(ask.title, "Abort cherry-pick");
        assert_eq!(
            ask.text,
            "Abort the cherry-pick and return to where it started? \
             Resolved conflicts are lost. (y/n)"
        );
        assert_eq!(ask.then, yes(Operation::CherryPick));
        assert_eq!(
            (ask.scope, ask.answer),
            (KeyScope::AbortConfirm, Answer::YOnly)
        );
        assert_eq!(
            abort_question(Operation::Merge).unwrap().title,
            "Abort merge"
        );
        assert_eq!(
            abort_question(Operation::Rebase).unwrap().title,
            "Abort rebase"
        );
        let revert = abort_question(Operation::Revert).unwrap();
        assert_eq!(revert.title, "Abort revert");
        assert!(revert.text.starts_with("Abort the revert "));
        assert_eq!(revert.then, yes(Operation::Revert));
        assert_eq!(abort_question(Operation::Bisect), None);
    }

    #[test]
    fn the_drop_stash_question_names_the_entry_and_says_it_is_gone_for_good() {
        use crate::features::stash::StashRef;
        use kika_git::commit_files::CommitId;
        let ask = crate::features::stash::drop_stash_question(
            "stash@{2}: On main: wip",
            GitOp::StashDrop(StashRef {
                index: 2,
                oid: CommitId::parse("abc").unwrap(),
            }),
        );
        assert_eq!(ask.title, "Drop stash");
        assert_eq!(
            ask.text,
            "Drop stash@{2}: On main: wip? It cannot be recovered from the stash list. (y/n)"
        );
    }

    #[test]
    fn the_stash_prompts_answer_with_their_text() {
        assert_eq!(PromptKind::StashMessage.title(), "Stash message (optional)");
        assert_eq!(
            PromptKind::StashMessage.submit("wip".into()),
            AppIntent::Stash(StashIntent::MessageAnswered("wip".into()))
        );
        assert_eq!(PromptKind::RenameStash.title(), "Rename stash");
        assert_eq!(
            PromptKind::RenameStash.submit("new".into()),
            AppIntent::Stash(StashIntent::RenameTo("new".into()))
        );
    }

    #[test]
    fn the_detached_checkout_question_warns_and_yes_checks_out() {
        use kika_git::commit_files::CommitId;
        let id = CommitId::parse("abc1234").unwrap();
        let ask = crate::features::commits::checkout_question("abc1234".into(), id.clone(), None);
        assert_eq!(ask.title, "Detached checkout");
        assert!(ask.text.contains("detached HEAD"));
        assert_eq!(ask.answer, Answer::YOrEnter);
        assert_eq!(ask.then, Then::Op(GitOp::CheckoutCommit(id)));
    }

    #[test]
    fn the_hard_reset_question_names_the_target_and_what_is_lost() {
        use crate::op::ResetMode;
        use kika_git::commit_files::CommitId;
        let ask = crate::features::commits::reset_hard_question(
            "abc1234".into(),
            GitOp::Reset {
                to: CommitId::parse("abc1234").unwrap(),
                mode: ResetMode::Hard,
            },
            None,
        );
        assert_eq!(ask.title, "Reset hard");
        assert_eq!(
            ask.text,
            "Reset to abc1234 and throw away every uncommitted change, staged and \
             unstaged? Commits after it leave the branch. (y/n)"
        );
    }

    fn leave(op: GitOp, count: usize) -> ConfirmQuestion {
        LeaveCommitsConfirm {
            op,
            head: kika_git::commit_files::CommitId::parse("fedcba9").unwrap(),
            head_short_oid: "fedcba9".into(),
            target: "abc1234".into(),
            left: (0..count).map(|i| format!("{i}{i}{i}")).collect(),
        }
        .into_question()
    }

    fn reset(mode: crate::op::ResetMode) -> GitOp {
        GitOp::Reset {
            to: kika_git::commit_files::CommitId::parse("abc1234").unwrap(),
            mode,
        }
    }

    #[test]
    fn the_leave_commits_question_names_the_action_count_and_ids() {
        use kika_git::commit_files::CommitId;
        let checkout = GitOp::CheckoutCommit(CommitId::parse("abc1234").unwrap());
        let ask = leave(checkout.clone(), 3);
        assert_eq!(ask.title, "Leave commits behind");
        assert_eq!(
            ask.text,
            "Check out abc1234 and leave 3 commits that no branch or tag reaches behind \
             (000, 111, 222)? Afterwards only the reflog has them. To keep them, press n, \
             then N to create a branch at fedcba9 (y/n)"
        );
        assert!(
            leave(checkout, 1)
                .text
                .contains("leave 1 commit that no branch")
        );
    }

    #[test]
    fn the_branch_checkouts_name_the_branch_head_lands_on() {
        let branch_ops = [
            GitOp::Checkout("main".into()),
            GitOp::CreateBranch {
                name: "main".into(),
                from: "topic".into(),
            },
            GitOp::CheckoutTracking {
                name: "main".into(),
                from: crate::test_support::three_remotes()[1].branches[0].clone(),
            },
        ];
        for op in branch_ops {
            let ask = LeaveCommitsConfirm {
                op: op.clone(),
                head: kika_git::commit_files::CommitId::parse("fedcba9").unwrap(),
                head_short_oid: "fedcba9".into(),
                target: "main".into(),
                left: vec!["000".into()],
            }
            .into_question();
            assert_eq!(ask.title, "Leave commits behind", "{op:?}");
            assert_eq!(
                ask.text,
                "Check out main and leave 1 commit that no branch or tag reaches behind \
                 (000)? Afterwards only the reflog has them. To keep them, press n, \
                 then N to create a branch at fedcba9 (y/n)",
                "{op:?}"
            );
            assert_eq!(ask.answer, Answer::YOnly, "{op:?}");
        }
    }

    #[test]
    fn the_review_mode_question_says_how_to_keep_them_from_review() {
        use kika_git::commit_files::CommitId;
        // The op review mode's `b` plans for request 7.
        let op = GitOp::CheckoutPrBranch {
            number: 7,
            branch: "pr/7".into(),
            head: CommitId::parse("abc1234").unwrap(),
            old: None,
            current: false,
            from: Some(CommitId::parse("fedcba9").unwrap()),
        };
        let ask = LeaveCommitsConfirm {
            op,
            head: CommitId::parse("fedcba9").unwrap(),
            head_short_oid: "fedcba9".into(),
            target: "pr/7".into(),
            left: vec!["000".into(), "111".into()],
        }
        .into_question();
        assert_eq!(
            ask.text,
            "Check out pr/7 and leave 2 commits that no branch or tag reaches behind \
             (000, 111)? Afterwards only the reflog has them. To keep them, press n, \
             then esc until you are back in the workspace, then N in Commits to create \
             a branch at fedcba9 (y/n)"
        );
        assert!(!ask.text.contains("press n, then N"), "{}", ask.text);
        assert_eq!(ask.answer, Answer::YOnly);
    }

    #[test]
    fn the_leave_commits_question_lists_five_then_how_many_more() {
        use crate::op::ResetMode;
        let q = leave(reset(ResetMode::Soft), 7).text;
        assert!(q.starts_with("Reset to abc1234 and leave 7 commits"), "{q}");
        assert!(q.contains("(000, 111, 222, 333, 444 and 2 more)"), "{q}");
        assert!(!q.contains("555"), "{q}");
        assert!(!q.contains("uncommitted"), "{q}");
    }

    #[test]
    fn the_hard_reset_version_warns_about_the_changes_first() {
        use crate::op::ResetMode;
        let q = leave(reset(ResetMode::Hard), 2).text;
        assert!(
            q.starts_with(
                "Reset hard to abc1234: every uncommitted change, staged and unstaged, \
                 is thrown away, and 2 commits"
            ),
            "{q}"
        );
        let warning = q.find("thrown away").unwrap();
        assert!(warning < q.find("000").unwrap(), "{q}");
        assert!(q.ends_with("(y/n)"), "{q}");
    }

    #[test]
    fn every_leave_commits_question_says_how_to_keep_them() {
        use crate::op::ResetMode;
        use kika_git::commit_files::CommitId;
        let checkout = GitOp::CheckoutCommit(CommitId::parse("abc1234").unwrap());
        for op in [
            checkout,
            reset(ResetMode::Soft),
            reset(ResetMode::Mixed),
            reset(ResetMode::Hard),
        ] {
            let q = leave(op, 2).text;
            assert!(
                q.ends_with("To keep them, press n, then N to create a branch at fedcba9 (y/n)"),
                "{q}"
            );
            assert!(!q.contains("git branch"), "{q}");
        }
        let q = leave(reset(ResetMode::Hard), 1).text;
        assert!(
            q.contains("1 commit that no branch or tag reaches is left"),
            "{q}"
        );
    }

    #[test]
    fn the_travel_question_is_the_preview_then_y_or_n() {
        use crate::features::commits::reflog::{Direction, Expected, Step};
        use kika_git::commit_files::CommitId;
        let ask = crate::features::commits::reflog::travel_question(
            "Undo",
            &["Undo: commit: x".into(), "Branch main moves.".into()],
            GitOp::Travel {
                direction: Direction::Undo,
                step: Step::MoveTo(CommitId::parse("abc").unwrap()),
                expect: Expected {
                    head_ref: None,
                    oid: CommitId::parse("def").unwrap(),
                },
            },
        );
        assert_eq!(ask.text, "Undo: commit: x\nBranch main moves.\n(y/n)");
    }

    #[test]
    fn typing_inserts_at_the_cursor_by_char() {
        let mut input = typed("héllo 日本");
        assert_eq!(input.col, 8);
        press(&mut input, KeyCode::Home);
        press(&mut input, KeyCode::Right);
        press(&mut input, KeyCode::Char('x'));
        assert_eq!(input.text, "hxéllo 日本");
        assert_eq!(input.col, 2);
    }

    #[test]
    fn backspace_and_delete_remove_whole_chars_and_stop_at_the_ends() {
        let mut input = typed("a日b");
        press(&mut input, KeyCode::Left);
        press(&mut input, KeyCode::Backspace);
        assert_eq!((input.text.as_str(), input.col), ("ab", 1));
        press(&mut input, KeyCode::Delete);
        assert_eq!((input.text.as_str(), input.col), ("a", 1));
        press(&mut input, KeyCode::Delete);
        assert_eq!(input.text, "a");
        press(&mut input, KeyCode::Home);
        press(&mut input, KeyCode::Backspace);
        assert_eq!((input.text.as_str(), input.col), ("a", 0));
    }

    #[test]
    fn arrows_home_and_end_stay_inside_the_text() {
        let mut input = typed("abc");
        press(&mut input, KeyCode::Right);
        assert_eq!(input.col, 3);
        press(&mut input, KeyCode::Home);
        press(&mut input, KeyCode::Left);
        assert_eq!(input.col, 0);
        press(&mut input, KeyCode::End);
        assert_eq!(input.col, 3);
    }

    #[test]
    fn control_alt_chords_and_control_chars_are_not_typed() {
        let mut input = LineInput::default();
        input.edit(KeyPress::new(KeyCode::Char('A'), KeyModifiers::SHIFT));
        input.edit(KeyPress::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        input.edit(KeyPress::new(KeyCode::Char('x'), KeyModifiers::ALT));
        press(&mut input, KeyCode::Char('\x1b'));
        press(&mut input, KeyCode::Char('\u{9b}'));
        press(&mut input, KeyCode::Enter);
        press(&mut input, KeyCode::Tab);
        assert_eq!(input.text, "A");
        assert_eq!(input.col, 1);
    }

    #[test]
    fn ctrl_u_clears_up_to_the_cursor_and_keeps_the_rest() {
        let mut input = LineInput::with_text("héllo world");
        for _ in 0..5 {
            press(&mut input, KeyCode::Left);
        }
        input.edit(KeyPress::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!((input.text.as_str(), input.col), ("world", 0));
        // Ctrl-alt-u or ctrl-shift-U is not ctrl-u.
        let mut other = LineInput::with_text("ab");
        other.edit(KeyPress::new(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ));
        assert_eq!(other.text, "ab");
    }

    #[test]
    fn prefilled_text_puts_the_cursor_after_it() {
        let input = LineInput::with_text("fé");
        assert_eq!((input.text.as_str(), input.col), ("fé", 2));
    }

    #[test]
    fn a_merged_branch_is_asked_plainly_and_an_unmerged_one_with_a_warning() {
        let ask = |force| {
            crate::features::branches::delete_branch_question(
                "topic".into(),
                GitOp::DeleteBranch {
                    name: "topic".into(),
                    force,
                },
            )
        };
        assert_eq!(ask(false).answer, Answer::YOnly);
        assert_eq!(ask(false).text, "Delete branch topic? (y/n)");
        assert_eq!(ask(true).answer, Answer::Force);
        assert_eq!(
            ask(true).text,
            "topic is not fully merged; its commits may be lost.\n\
             Press D to force delete, n or Esc to cancel"
        );
    }

    #[test]
    fn branch_prompts_answer_with_their_intents() {
        assert_eq!(
            PromptKind::NewBranch.submit("n".into()),
            AppIntent::Branches(BranchesIntent::CreateNamed("n".into()))
        );
        assert_eq!(
            PromptKind::RenameBranch.submit("r".into()),
            AppIntent::Branches(BranchesIntent::RenameTo("r".into()))
        );
        assert_eq!(
            PromptKind::NewBranchAtHead.submit("h".into()),
            AppIntent::Branches(BranchesIntent::CreateAtHeadNamed("h".into()))
        );
        assert_eq!(PromptKind::NewBranchAtHead.title(), "New branch");
    }

    #[test]
    fn remote_and_upstream_prompts_answer_with_their_intents() {
        let t = || "t".to_string();
        assert_eq!(
            PromptKind::TrackBranch.submit(t()),
            AppIntent::Branches(BranchesIntent::Remotes(RemotesIntent::TrackNamed(t())))
        );
        assert_eq!(
            PromptKind::RemoteName.submit(t()),
            AppIntent::Branches(BranchesIntent::Remotes(RemotesIntent::AddNamed(t())))
        );
        assert_eq!(
            PromptKind::RemoteUrl.submit(t()),
            AppIntent::Branches(BranchesIntent::Remotes(RemotesIntent::AddUrl(t())))
        );
        assert_eq!(
            PromptKind::RenameRemote.submit(t()),
            AppIntent::Branches(BranchesIntent::Remotes(RemotesIntent::RenameTo(t())))
        );
        assert_eq!(
            PromptKind::SetUpstream.submit(t()),
            AppIntent::Branches(BranchesIntent::SetUpstreamTo(t()))
        );
    }

    #[test]
    fn tag_prompts_answer_with_their_intents_and_delete_asks_plainly() {
        let t = || "t".to_string();
        assert_eq!(
            PromptKind::TagName.submit(t()),
            AppIntent::Branches(BranchesIntent::Tags(TagsIntent::Named(t())))
        );
        assert_eq!(
            PromptKind::TagMessage.submit(t()),
            AppIntent::Branches(BranchesIntent::Tags(TagsIntent::Messaged(t())))
        );
        let ask = crate::features::branches::tags::delete_tag_question(
            "v1".into(),
            GitOp::DeleteTag {
                name: "v1".into(),
                oid: "1".repeat(40),
            },
        );
        assert_eq!(
            ask.text,
            "Delete tag v1? It is not deleted on any remote. (y/n)"
        );
        assert_eq!(ask.title, "Delete tag");
    }

    fn range(kind: RangeKind, names: &[&str], skipped: &[(&str, usize)]) -> ConfirmQuestion {
        DeleteRangeConfirm {
            kind,
            names: names.iter().map(|n| n.to_string()).collect(),
            skipped: skipped.iter().map(|(w, n)| (w.to_string(), *n)).collect(),
            op: GitOp::DeleteBranches(vec![]),
        }
        .into_question()
    }

    #[test]
    fn a_range_question_names_the_count_the_names_and_what_is_kept() {
        let ask = range(
            RangeKind::Branches,
            &["a", "b", "c"],
            &[("not fully merged", 1), ("current", 1)],
        );
        assert_eq!(
            ask.text,
            "Delete 3 branches (a, b, c)? Kept: 1 not fully merged, 1 current. (y/n)"
        );
        assert_eq!(ask.title, "Delete branches");
        let ask = range(RangeKind::Branches, &["a"], &[("current", 0)]);
        assert_eq!(ask.text, "Delete 1 branch (a)? (y/n)");
        assert_eq!(
            range(RangeKind::Tags, &["v1", "v2"], &[]).text,
            "Delete 2 tags (v1, v2)? They are not deleted on any remote. (y/n)"
        );
        assert_eq!(
            range(RangeKind::Tags, &["v1"], &[]).text,
            "Delete 1 tag (v1)? It is not deleted on any remote. (y/n)"
        );
        assert_eq!(
            range(RangeKind::Remotes, &["a", "b"], &[]).text,
            "Remove 2 remotes (a, b)? Their remote-tracking branches are deleted, \
             and branches tracking them lose their upstream. (y/n)"
        );
        assert_eq!(
            range(RangeKind::Remotes, &["a"], &[("not configured", 2)]).text,
            "Remove 1 remote (a)? Its remote-tracking branches are deleted, \
             and branches tracking it lose their upstream. Kept: 2 not configured. (y/n)"
        );
    }

    #[test]
    fn a_stash_range_question_names_the_count_and_each_place() {
        let ask = range(
            RangeKind::Stashes,
            &["stash@{1}", "stash@{2}", "stash@{3}"],
            &[],
        );
        assert_eq!(ask.title, "Drop stashes");
        assert_eq!(
            ask.text,
            "Drop 3 stashes (stash@{1}, stash@{2}, stash@{3})? \
             They cannot be recovered from the stash list. (y/n)"
        );
        assert_eq!(
            range(RangeKind::Stashes, &["stash@{0}"], &[]).text,
            "Drop 1 stash (stash@{0})? It cannot be recovered from the stash list. (y/n)"
        );
    }

    #[test]
    fn a_long_range_lists_five_names_and_counts_the_rest() {
        let names = ["a", "b", "c", "d", "e", "f", "g"];
        let question = range(RangeKind::Tags, &names, &[]).text;
        assert!(
            question.starts_with("Delete 7 tags (a, b, c, d, e and 2 more)?"),
            "{question}"
        );
    }

    #[test]
    fn the_remove_remote_question_names_it_and_its_branches() {
        let ask = crate::features::branches::remotes::remove_remote_question(
            "fork".into(),
            GitOp::RemoveRemote("fork".into()),
        );
        let question = ask.text;
        assert!(question.starts_with("Remove remote fork?"), "{question}");
        assert!(
            question.contains("remote-tracking branches are deleted"),
            "{question}"
        );
    }

    #[test]
    fn an_error_message_holds_no_control_chars() {
        let error = ErrorPopup::new("Could not stage", "a\x1b[31mb\u{9b}c", "Try again.");
        assert!(!error.message.chars().any(char::is_control), "{error:?}");
        assert!(error.message.starts_with('a') && error.message.ends_with('c'));
        // Making it safe twice changes nothing, so any path may do it.
        let again = ErrorPopup::new("Could not stage", &error.message, "Try again.");
        assert_eq!(again, error);
    }

    #[test]
    fn an_error_keeps_the_lines_of_git_message() {
        let error = ErrorPopup::new("t", "fatal: one\nhint: two\n", "n");
        assert_eq!(error.message, "fatal: one\nhint: two");
    }

    #[test]
    fn a_credential_prompt_masks_all_but_names_and_yes_no_questions() {
        let masked = |prompt: &str| !CredentialPrompt::new(prompt.into(), false).shows_typed_text();
        assert!(masked("Password for 'https://u@h':"));
        assert!(masked(
            "Enter passphrase for key '/home/u/.ssh/id_ed25519':"
        ));
        assert!(!masked("Username for 'https://h':"));
        assert!(!masked(
            "The authenticity of host 'h' can't be established.\n\
             Are you sure you want to continue connecting (yes/no/[fingerprint])?"
        ));
        // Older ssh asks without the fingerprint choice.
        assert!(!masked(
            "Are you sure you want to continue connecting (yes/no)?"
        ));
        // A server's own prompt that only looks like one stays masked.
        assert!(masked("Username and token:"));
        assert!(masked("Token (yes/no is fine):"));
    }

    #[test]
    fn only_a_prompt_ending_in_the_host_key_choice_is_shown_in_clear() {
        let masked = |prompt: &str| !CredentialPrompt::new(prompt.into(), false).shows_typed_text();
        // Ssh's own question, with the trailing space it sends.
        assert!(!masked(
            "The authenticity of host 'h' can't be established.\n\
             ED25519 key fingerprint is SHA256:abc.\n\
             Are you sure you want to continue connecting (yes/no/[fingerprint])? "
        ));
        assert!(!masked(
            "Are you sure you want to continue connecting (yes/no)?"
        ));
        assert!(!masked("Username for 'https://h':"));
        // A server's prompt, which ssh opens with `(user@host) `.
        assert!(masked(
            "(git@host) Are you sure you want to continue connecting (yes/no)?"
        ));
        // The phrase with a secret asked for after it.
        assert!(masked(
            "Are you sure you want to continue connecting (yes/no)? Password:"
        ));
        assert!(masked(
            "want to continue connecting (yes/no/[fingerprint])? token:"
        ));
        // Only the choice, without ssh's sentence before it.
        assert!(masked("Token (yes/no)?"));
        assert!(masked("Password:\n(yes/no)?"));
        assert!(masked("Token (yes/no/[fingerprint])?"));
        // Not git's username question, so masked on purpose.
        assert!(masked("Username:"));
    }

    const HOST_KEY_QUESTION: &str = "The authenticity of host 'h' can't be established.\n\
         Are you sure you want to continue connecting (yes/no/[fingerprint])? ";

    /// Whether `typed`, typed into `prompt`, is drawn in clear.
    fn shows(prompt: &str, typed: &str) -> bool {
        let mut prompt = CredentialPrompt::new(prompt.into(), false);
        for c in typed.chars() {
            prompt
                .input
                .edit(KeyPress::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        prompt.shows_typed_text()
    }

    #[test]
    fn a_host_key_question_picks_the_answer_echo() {
        let echo = |prompt: &str| CredentialPrompt::new(prompt.into(), false).echo;
        assert_eq!(echo(HOST_KEY_QUESTION), Echo::HostKeyAnswer);
        assert_eq!(echo("Username for 'https://h':"), Echo::Clear);
        assert_eq!(echo("Password for 'https://u@h':"), Echo::Masked);
    }

    #[test]
    fn a_host_key_question_shows_a_yes_or_no_being_typed() {
        for typed in ["", "y", "ye", "yes", "n", "no"] {
            assert!(shows(HOST_KEY_QUESTION, typed), "{typed:?} masked");
        }
        assert!(shows(
            "Are you sure you want to continue connecting (yes/no)?",
            "yes"
        ));
    }

    #[test]
    fn a_host_key_question_masks_anything_but_an_answer() {
        for typed in ["yess", "yes ", "YES", "Y", "hunter2", "nope", "x", " "] {
            assert!(!shows(HOST_KEY_QUESTION, typed), "{typed:?} shown");
        }
    }

    #[test]
    fn a_host_key_question_shows_a_sha256_fingerprint() {
        let fingerprint = "SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2s";
        for typed in ["S", "SHA2", "SHA256:", "SHA256:a+/9", fingerprint] {
            assert!(shows(HOST_KEY_QUESTION, typed), "{typed:?} masked");
        }
        assert!(shows(HOST_KEY_QUESTION, &format!("{fingerprint}=")));
        let too_long = format!("SHA256:{}", "a".repeat(44));
        for typed in [
            "SHA256:abc def",
            "SHA256:abc!",
            "SHA256:=",
            "SHA256:abc==",
            "SHA256:abc=d",
            "sha256:abc",
            too_long.as_str(),
            "MD5:aa:bb",
        ] {
            assert!(!shows(HOST_KEY_QUESTION, typed), "{typed:?} shown");
        }
    }

    #[test]
    fn other_prompts_keep_their_echo_whatever_is_typed() {
        for typed in ["", "ann", "hunter2 with spaces"] {
            assert!(shows("Username for 'https://h':", typed), "{typed:?}");
        }
        for typed in ["x", "yes"] {
            assert!(!shows("Password for 'https://u@h':", typed), "{typed:?}");
            assert!(!shows("Token (yes/no)?", typed), "{typed:?}");
        }
    }

    #[test]
    fn typed_credential_text_never_prints() {
        let mut prompt = CredentialPrompt::new("Password:".into(), false);
        for c in "hunter2".chars() {
            prompt
                .input
                .edit(KeyPress::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(prompt.input.line().text, "hunter2");
        assert_eq!(prompt.input.secret().expose(), "hunter2");
        let printed = format!("{:?}", Popup::Credential(prompt));
        assert!(!printed.contains("hunter2"), "{printed}");
    }

    #[test]
    fn a_rejected_push_asks_before_forcing_with_a_lease() {
        let ask = crate::features::sync::force_push_question();
        assert_eq!(
            ask.then,
            Then::Intent(Box::new(AppIntent::Sync(SyncIntent::ForcePush)))
        );
        assert_eq!(
            (ask.scope, ask.answer),
            (KeyScope::ForcePushConfirm, Answer::YOnly)
        );
        assert_eq!(ask.title, "Push rejected");
        assert!(ask.text.starts_with(
            "The remote has commits this branch does not. Push with --force-with-lease"
        ));
    }

    #[test]
    fn confirm_and_prompt_answer_with_intents() {
        let quit = crate::confirm_question::quit_question();
        assert_eq!(quit.then, Then::Intent(Box::new(AppIntent::Quit)));
        assert_eq!(
            (quit.scope, quit.answer),
            (KeyScope::Confirm, Answer::YOrEnter)
        );
        assert_eq!(quit.text, "Quit Kika? (y/n)");
        assert_eq!(
            PromptKind::GoToFile.submit("b".into()),
            AppIntent::Files(FilesIntent::GoToFile("b".into()))
        );
        assert_eq!(
            PromptKind::Command.submit("c".into()),
            AppIntent::CustomCommands(CustomCommandsIntent::PromptAnswered("c".into()))
        );
    }
}
