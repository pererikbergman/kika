//! Interactive rebase as data: which commits to rewrite and how, checked
//! against git's own todo list before git acts on it. Pure: no I/O, so the
//! UI and the hidden sequence editor agree on one plan.

use super::commit_files::CommitId;
use super::log::CommitRow;
use super::safe_text::terminal_safe;

/// What the user asked to do with the selected commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    /// Fold it into the commit below, combining both messages.
    Squash,
    /// Fold it into the commit below, keeping the older message.
    Fixup,
    /// Remove it and its change.
    Drop,
    /// Keep its change, write a new message.
    Reword,
    /// Stop the rebase once it is applied, so it can be amended.
    Stop,
    /// Apply it as it is. Only a todo row can become one: a log row
    /// already is.
    Pick,
}

/// One todo command. Every word exists in git 2.15.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoAction {
    Pick,
    Squash,
    Fixup,
    Drop,
    Reword,
    Edit,
}

impl TodoAction {
    const ALL: [TodoAction; 6] = [
        TodoAction::Pick,
        TodoAction::Squash,
        TodoAction::Fixup,
        TodoAction::Drop,
        TodoAction::Reword,
        TodoAction::Edit,
    ];

    pub fn word(self) -> &'static str {
        match self {
            TodoAction::Pick => "pick",
            TodoAction::Squash => "squash",
            TodoAction::Fixup => "fixup",
            TodoAction::Drop => "drop",
            TodoAction::Reword => "reword",
            TodoAction::Edit => "edit",
        }
    }

    /// The one-letter form git also accepts in a todo list.
    fn short(self) -> &'static str {
        &self.word()[..1]
    }

    /// The long word only, as [`encode`] writes it.
    fn from_word(word: &[u8]) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|action| action.word().as_bytes() == word)
    }

    /// The long or one-letter word, as a todo list git or the user wrote
    /// may hold.
    fn from_todo_word(word: &[u8]) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|action| action.word().as_bytes() == word || action.short().as_bytes() == word)
    }
}

impl From<Edit> for TodoAction {
    fn from(edit: Edit) -> Self {
        match edit {
            Edit::Squash => TodoAction::Squash,
            Edit::Fixup => TodoAction::Fixup,
            Edit::Drop => TodoAction::Drop,
            Edit::Reword => TodoAction::Reword,
            Edit::Stop => TodoAction::Edit,
            Edit::Pick => TodoAction::Pick,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub action: TodoAction,
    pub commit: CommitId,
    /// Stop the rebase right after this step.
    pub break_after: bool,
}

/// Rebase onto `base`, replaying `steps` oldest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebasePlan {
    pub base: CommitId,
    pub steps: Vec<Step>,
    /// Move branches that point into the rewritten range along with their
    /// commits (`--update-refs`, git 2.38). The planner leaves it off; the
    /// model turns it on when the installed git has it.
    pub update_refs: bool,
}

/// Which way a commit moves in the log: up is newer, down is older.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
}

/// The most steps a plan may have. It travels in one env var, which this
/// keeps under the OS limit for a single string: at most 72 bytes a step,
/// against 128 KiB on Linux and 32,767 characters for one Windows env
/// var.
pub const MAX_STEPS: usize = if cfg!(windows) { 400 } else { 1000 };

/// Why a fold of the last loaded commit cannot be planned: moving the
/// cursor down to the end of the log loads the next page.
pub const LOAD_MORE: &str = "Move down to load the commit below first";

/// The plan for `edit` on `rows[selected]`, where `rows` is the log from
/// `HEAD`, newest first. Squash and fixup fold the commit into its parent,
/// so the parent is rewritten too. Refuses anything but a linear
/// first-parent chain from `HEAD` with a parent below it, since the todo
/// for anything else would hold more than picks.
pub fn plan(rows: &[CommitRow], selected: usize, edit: Edit) -> Result<RebasePlan, &'static str> {
    let folds = matches!(edit, Edit::Squash | Edit::Fixup);
    let target = rows.get(selected).ok_or("No commits")?;
    if edit == Edit::Pick {
        return Err("Already a pick");
    }
    if target.parents.is_empty() {
        return Err(FIRST_COMMIT);
    }
    let last = if folds { selected + 1 } else { selected };
    let mut plan = picks(linear_range(rows, last)?)?;
    step_of(&mut plan, selected).action = edit.into();
    Ok(plan)
}

/// Why a range cannot be planned: it is not one unbroken run of loaded
/// rows, which only happens when the filter hides commits inside it.
pub const RANGE_GAP: &str = "The filter hides commits inside the selection; clear it first";

/// Why an edit other than squash, fixup or drop cannot act on a range.
pub const RANGE_EDIT: &str = "Only squash, fixup and drop act on a selection";

/// Why the root commit cannot be rewritten: there is no parent to rebase
/// onto.
pub const FIRST_COMMIT: &str = "Cannot rewrite the first commit";

/// The plan for `edit` on every row of `range`, log row indexes newest
/// first, in one rebase. Squash and fixup fold the whole range into the
/// commit below it; drop removes every commit in it. Refuses a range with a
/// gap, since rewriting commits the user cannot see would widen it
/// silently, and any edit but those three.
pub fn plan_range(
    rows: &[CommitRow],
    range: &[usize],
    edit: Edit,
) -> Result<RebasePlan, &'static str> {
    if !matches!(edit, Edit::Squash | Edit::Fixup | Edit::Drop) {
        return Err(RANGE_EDIT);
    }
    let (Some(&lo), Some(&hi)) = (range.first(), range.last()) else {
        return Err(RANGE_GAP);
    };
    if range.windows(2).any(|pair| pair[1] != pair[0] + 1) {
        return Err(RANGE_GAP);
    }
    let targets = rows.get(lo..=hi).ok_or("No commits")?;
    if targets.iter().any(|row| row.parents.is_empty()) {
        return Err(FIRST_COMMIT);
    }
    let last = if edit == Edit::Drop { hi } else { hi + 1 };
    let mut plan = picks(linear_range(rows, last)?)?;
    for &index in range {
        step_of(&mut plan, index).action = edit.into();
    }
    Ok(plan)
}

/// The plan that swaps `rows[selected]` with its neighbour in `direction`,
/// every other commit picked as it is. Down needs the commit below loaded.
pub fn plan_move(
    rows: &[CommitRow],
    selected: usize,
    direction: Direction,
) -> Result<RebasePlan, &'static str> {
    let target = rows.get(selected).ok_or("No commits")?;
    let (newer, older) = match direction {
        Direction::Up if selected == 0 => return Err("Already the newest commit"),
        Direction::Up => (selected - 1, selected),
        Direction::Down => (selected, selected + 1),
    };
    if target.parents.is_empty() {
        return Err(FIRST_COMMIT);
    }
    let mut plan = picks(linear_range(rows, older)?)?;
    let n = plan.steps.len();
    plan.steps.swap(n - 1 - newer, n - 1 - older);
    Ok(plan)
}

/// The plan that stops the rebase right after `rows[selected]`. Every
/// step is a pick, so git fast-forwards over them and nothing below the
/// stop is rewritten.
pub fn plan_break(rows: &[CommitRow], selected: usize) -> Result<RebasePlan, &'static str> {
    let target = rows.get(selected).ok_or("No commits")?;
    if target.parents.is_empty() {
        return Err(FIRST_COMMIT);
    }
    let mut plan = picks(linear_range(rows, selected)?)?;
    step_of(&mut plan, selected).break_after = true;
    Ok(plan)
}

/// The plan that stops the rebase at each of `stops`, log row indexes in
/// any order, so the commit there can be amended; every other commit is
/// picked. The base is the parent of the oldest stop, so git
/// fast-forwards up to it and nothing below it is rewritten.
pub fn plan_stops(rows: &[CommitRow], stops: &[usize]) -> Result<RebasePlan, &'static str> {
    let oldest = *stops.iter().max().ok_or("No commits")?;
    if rows.get(oldest).is_some_and(|row| row.parents.is_empty()) {
        return Err(FIRST_COMMIT);
    }
    let mut plan = picks(linear_range(rows, oldest)?)?;
    for &stop in stops {
        step_of(&mut plan, stop).action = TodoAction::Edit;
    }
    Ok(plan)
}

/// Why there is nothing to autosquash.
pub const NO_FIXUPS: &str = "No fixup! commits to fold";

/// Why a `fixup!` commit cannot be folded: no commit below it in the
/// loaded log has the subject it names.
pub const NO_TARGET: &str = "No target loaded for a fixup!; move down to load more";

/// Why a `fixup!` commit cannot be folded: its target is below a merge.
pub const ACROSS_MERGE: &str = "Cannot rewrite across a merge";

/// Why a `fixup!` commit cannot be folded: its subject, or its target's,
/// was changed to be drawn safely, so matching it could pick the wrong
/// commit.
pub const INEXACT_SUBJECT: &str =
    "A fixup! commit or its target has a subject with control characters; fold it by hand";

/// What git writes in front of the subject of a fixup commit.
const FIXUP_PREFIX: &str = "fixup! ";

/// Each `fixup!` commit on the first-parent chain from `HEAD`, with the
/// commit it fixes, as log row indexes `(fixup, target)`, newest fixup
/// first. The chain ends at the first merge or the last loaded row. A
/// fixup's target is the nearest older commit in the chain whose subject
/// is the fixup's, every leading `fixup! ` stripped, and that is not a
/// fixup itself. Subjects are compared as drawn, so any fixup or matched
/// target whose subject drawing changed is refused rather than guessed.
pub fn fixup_targets(rows: &[CommitRow]) -> Result<Vec<(usize, usize)>, &'static str> {
    let merge = rows.iter().position(|row| row.parents.len() > 1);
    let chain = &rows[..merge.unwrap_or(rows.len())];
    let mut pairs = Vec::new();
    for (i, row) in chain.iter().enumerate() {
        let Some(mut subject) = row.subject.strip_prefix(FIXUP_PREFIX) else {
            continue;
        };
        if row.subject_altered {
            return Err(INEXACT_SUBJECT);
        }
        while let Some(rest) = subject.strip_prefix(FIXUP_PREFIX) {
            subject = rest;
        }
        let target = chain[i + 1..]
            .iter()
            .position(|older| older.subject == subject && !older.subject.starts_with(FIXUP_PREFIX))
            .map(|at| i + 1 + at);
        match target {
            Some(target) if chain[target].subject_altered => return Err(INEXACT_SUBJECT),
            Some(target) => pairs.push((i, target)),
            None if merge.is_some() => return Err(ACROSS_MERGE),
            None => return Err(NO_TARGET),
        }
    }
    if pairs.is_empty() {
        return Err(NO_FIXUPS);
    }
    Ok(pairs)
}

/// The plan that folds every `fixup!` commit into the commit it fixes
/// (see [`fixup_targets`]). Each fixup moves to right after its target,
/// after any older fixup of the same target, and becomes a `fixup`; every
/// other commit is picked in its order. Kika orders the todo itself, so
/// git's own `--autosquash` and `rebase.autoSquash` play no part.
pub fn plan_autosquash(rows: &[CommitRow]) -> Result<RebasePlan, &'static str> {
    let pairs = fixup_targets(rows)?;
    let deepest = pairs.iter().map(|&(_, target)| target).max().unwrap_or(0);
    if rows[deepest].parents.is_empty() {
        return Err(FIRST_COMMIT);
    }
    let picked = picks(linear_range(rows, deepest)?)?;
    let n = picked.steps.len();
    // `picked.steps[n - 1 - i]` is row `i`.
    let mut steps = Vec::with_capacity(n);
    for row in (0..=deepest).rev() {
        if pairs.iter().any(|&(fixup, _)| fixup == row) {
            continue;
        }
        steps.push(picked.steps[n - 1 - row].clone());
        // Older fixups first: the higher row index is the older commit.
        for &(fixup, _) in pairs.iter().rev().filter(|&&(_, target)| target == row) {
            let mut step = picked.steps[n - 1 - fixup].clone();
            step.action = TodoAction::Fixup;
            steps.push(step);
        }
    }
    Ok(RebasePlan { steps, ..picked })
}

/// `rows[..=last]`, once they are one first-parent chain with a loaded
/// parent below each and a parent below the last, at most [`MAX_STEPS`].
fn linear_range(rows: &[CommitRow], last: usize) -> Result<&[CommitRow], &'static str> {
    if last >= MAX_STEPS {
        return Err("Too many commits above it");
    }
    let Some(range) = rows.get(..=last) else {
        return Err(LOAD_MORE);
    };
    for (i, row) in range.iter().enumerate() {
        match row.parents.as_slice() {
            [] if i == last => return Err("The commit below is the first commit"),
            [parent] if range.get(i + 1).is_none_or(|next| next.oid == *parent) => {}
            _ => return Err("Cannot rewrite across a merge"),
        }
    }
    Ok(range)
}

/// Pick every commit of `range` onto the parent of its last.
fn picks(range: &[CommitRow]) -> Result<RebasePlan, &'static str> {
    let base = full_id(&range[range.len() - 1].parents[0])?;
    let steps = range
        .iter()
        .rev()
        .map(|row| {
            full_id(&row.oid).map(|commit| Step {
                action: TodoAction::Pick,
                commit,
                break_after: false,
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(RebasePlan {
        base,
        steps,
        update_refs: false,
    })
}

/// The step for log row `index`; the steps run oldest first.
fn step_of(plan: &mut RebasePlan, index: usize) -> &mut Step {
    let n = plan.steps.len();
    &mut plan.steps[n - 1 - index]
}

/// A full object id: 40 lowercase hex digits for SHA-1, 64 for SHA-256.
/// Git reads a full-length id as an object, never as a ref name, and
/// writes it lowercase, as the todo's abbreviations it is matched against.
fn full_id(text: &str) -> Result<CommitId, &'static str> {
    let lower_hex = text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    CommitId::parse(text)
        .filter(|_| lower_hex && matches!(text.len(), 40 | 64))
        .ok_or("Not a commit id")
}

/// The plan's steps as text, one `<word> <oid>` line each and a bare
/// `break` after a step that stops, for the env var the sequence editor
/// reads.
pub fn encode(steps: &[Step]) -> String {
    let mut text = String::new();
    for step in steps {
        text.push_str(&format!(
            "{} {}\n",
            step.action.word(),
            step.commit.as_str()
        ));
        if step.break_after {
            text.push_str("break\n");
        }
    }
    text
}

/// [`encode`]'s text back as steps. Anything that `encode` would not have
/// written is refused, a `break` that follows no step or another `break`
/// included.
pub fn decode(text: &str) -> Result<Vec<Step>, String> {
    let mut steps: Vec<Step> = Vec::new();
    for line in text.strip_suffix('\n').unwrap_or(text).split('\n') {
        if line == "break" {
            match steps.last_mut() {
                None => return Err("A break must follow a commit".into()),
                Some(step) if step.break_after => return Err("Two breaks in a row".into()),
                Some(step) => step.break_after = true,
            }
            continue;
        }
        let mut fields = line.split(' ');
        let (Some(word), Some(oid), None) = (fields.next(), fields.next(), fields.next()) else {
            return Err(format!("Not a plan line: {line:?}"));
        };
        let action = TodoAction::from_word(word.as_bytes())
            .ok_or_else(|| format!("Not a plan action: {word:?}"))?;
        let commit = full_id(oid).map_err(|e| format!("{e}: {oid:?}"))?;
        steps.push(Step {
            action,
            commit,
            break_after: false,
        });
    }
    if steps.is_empty() {
        return Err("The plan is empty".into());
    }
    Ok(steps)
}

/// One command of git's todo list that Kika understands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TodoEntry {
    Commit {
        action: TodoAction,
        /// Possibly abbreviated, at least 4 hex digits.
        oid: String,
        /// What follows the id, usually the subject, kept byte for byte.
        rest: Vec<u8>,
    },
    /// Stop here.
    Break,
    /// Point this ref (`refs/heads/<name>`) at the commit made just before.
    UpdateRef(Vec<u8>),
}

/// The commands in git's todo list. Comments led by `comment`, the
/// prefix git uses (see `comment_prefix`), and blank lines are dropped;
/// `exec`, `label`, `reset`, `merge` and any unknown word are errors,
/// since Kika neither writes nor edits them.
pub fn parse_todo(todo: &[u8], comment: &[u8]) -> Result<Vec<TodoEntry>, String> {
    // Git reads no further than a NUL, so a line past one would be kept
    // here and dropped by git. Such a list is not Kika's to edit.
    if todo.contains(&0) {
        return Err("The todo list has a NUL byte".into());
    }
    command_lines(todo, comment).map(parse_line).collect()
}

/// The lines of a todo list that git does not skip, with the spaces and
/// tabs git skips before a command taken off. Git skips only blank lines
/// and comments: after that trim, a line that is empty, starts with `\r`
/// or starts with `comment`. Every other line is a command to git, or a
/// line it rejects, so it is kept here for [`parse_todo`] to accept or
/// refuse. An empty `comment` marks no line, so a prefix that could not be
/// read hides nothing. Like git, it reads no further than the first NUL.
pub fn command_lines<'a>(todo: &'a [u8], comment: &'a [u8]) -> impl Iterator<Item = &'a [u8]> {
    numbered_lines(todo, comment).map(|(_, line)| line)
}

/// [`command_lines`] with each line's 1-based number in the file.
fn numbered_lines<'a>(
    todo: &'a [u8],
    comment: &'a [u8],
) -> impl Iterator<Item = (usize, &'a [u8])> {
    let end = todo.iter().position(|&b| b == 0).unwrap_or(todo.len());
    todo[..end]
        .split(|&b| b == b'\n')
        .enumerate()
        .map(|(i, line)| {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let indent = line
                .iter()
                .take_while(|&&b| b == b' ' || b == b'\t')
                .count();
            (i + 1, &line[indent..])
        })
        .filter(move |(_, line)| {
            let skipped = matches!(line.first(), None | Some(b'\r'))
                || (!comment.is_empty() && line.starts_with(comment));
            !skipped
        })
}

/// What in a todo list keeps Kika from reading it, as [`parse_todo`]
/// refuses it, for saying why it is not edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unreadable<'a> {
    /// A NUL byte, after which git reads nothing.
    Nul,
    /// The first line that is not a command Kika reads, by its 1-based
    /// number in the file, without its indent.
    Line { number: usize, text: &'a [u8] },
}

/// Why [`parse_todo`] refuses `todo`, or `None` when it reads it.
pub fn unreadable<'a>(todo: &'a [u8], comment: &'a [u8]) -> Option<Unreadable<'a>> {
    if todo.contains(&0) {
        return Some(Unreadable::Nul);
    }
    numbered_lines(todo, comment)
        .find(|(_, line)| parse_line(line).is_err())
        .map(|(number, text)| Unreadable::Line { number, text })
}

fn parse_line(line: &[u8]) -> Result<TodoEntry, String> {
    let shown = || String::from_utf8_lossy(line).into_owned();
    let (command, after) = split_field(line);
    if command == b"break" || command == b"b" {
        return if after.iter().all(u8::is_ascii_whitespace) {
            Ok(TodoEntry::Break)
        } else {
            Err(format!("Unexpected todo line: {:?}", shown()))
        };
    }
    if command == b"update-ref" {
        let (name, extra) = split_field(after);
        let plain = name.len() > b"refs/heads/".len()
            && name.starts_with(b"refs/heads/")
            && !name
                .iter()
                .any(|b| b.is_ascii_whitespace() || b.is_ascii_control());
        return if plain && extra.is_empty() {
            Ok(TodoEntry::UpdateRef(name.to_vec()))
        } else {
            Err(format!("Unexpected todo line: {:?}", shown()))
        };
    }
    let action = TodoAction::from_todo_word(command)
        .ok_or_else(|| format!("Unexpected todo line: {:?}", shown()))?;
    let (oid, rest) = split_field(after);
    if oid.len() < 4 || !oid.iter().all(u8::is_ascii_hexdigit) {
        return Err(format!("Not a commit id in todo line: {:?}", shown()));
    }
    Ok(TodoEntry::Commit {
        action,
        oid: String::from_utf8_lossy(oid).into_owned(),
        rest: rest.to_vec(),
    })
}

/// The bytes up to the first space, and those after it.
fn split_field(line: &[u8]) -> (&[u8], &[u8]) {
    match line.iter().position(|&b| b == b' ') {
        Some(at) => (&line[..at], &line[at + 1..]),
        None => (line, &[]),
    }
}

/// A ref as the user knows it, `refs/heads/` dropped, safe to print.
pub fn branch_name(full: &[u8]) -> String {
    terminal_safe(full.strip_prefix(b"refs/heads/").unwrap_or(full))
}

/// Why moving commit `oid` would change what `branch` holds.
pub fn crossing(oid: &str, branch: &[u8]) -> String {
    let short: String = oid.chars().take(7).collect();
    format!(
        "Moving {short} past branch {} would change what it holds",
        branch_name(branch)
    )
}

/// The todo list git should run: `steps` in place of `todo`'s picks,
/// which must name the same commits. Each step is matched to exactly one
/// todo commit by its id; any other difference means the history is not
/// what the plan was built from, so it is an error.
///
/// The `update-ref` lines split the todo into segments, one per stacked
/// branch. A step must stay in its commit's segment and the segments keep
/// their order, so no branch ends up holding other commits. Each segment
/// is written as its steps in plan order, then its `update-ref` lines.
pub fn apply(steps: &[Step], todo: &[TodoEntry]) -> Result<Vec<u8>, String> {
    full_ids(steps)?;
    let segments = collect_todo(todo)?;
    let matched = match_steps(steps, &segments)?;
    Ok(emit(steps, &matched, &segments))
}

/// Refuse the whole plan unless every step names its commit by a
/// [`full_id`] of one length: a short id could match the wrong todo
/// commit, and one repo has one hash.
fn full_ids(steps: &[Step]) -> Result<(), String> {
    let mut len = None;
    for step in steps {
        let oid = step.commit.as_str();
        full_id(oid).map_err(|e| format!("{e}: {oid:?}"))?;
        if *len.get_or_insert(oid.len()) != oid.len() {
            return Err(format!("The plan mixes id lengths: {oid:?}"));
        }
    }
    Ok(())
}

/// A todo commit as [`apply`] keeps it: its id as git wrote it, what
/// follows the id, and the segment it sits in.
#[derive(Debug, PartialEq, Eq)]
struct TodoCommit<'a> {
    oid: &'a str,
    rest: &'a [u8],
    segment: usize,
}

/// A fresh todo list split at its `update-ref` lines: every commit with
/// its segment, and the refs that close each segment. Git writes only
/// picks there, so anything else is an error.
#[derive(Debug, PartialEq, Eq)]
struct Segments<'a> {
    commits: Vec<TodoCommit<'a>>,
    refs: Vec<Vec<&'a [u8]>>,
}

fn collect_todo(todo: &[TodoEntry]) -> Result<Segments<'_>, String> {
    let mut commits = Vec::new();
    let mut refs: Vec<Vec<&[u8]>> = vec![Vec::new()];
    for entry in todo {
        match entry {
            TodoEntry::Commit {
                action: TodoAction::Pick,
                oid,
                rest,
            } => {
                if !refs[refs.len() - 1].is_empty() {
                    refs.push(Vec::new());
                }
                commits.push(TodoCommit {
                    oid,
                    rest,
                    segment: refs.len() - 1,
                });
            }
            TodoEntry::UpdateRef(name) => {
                let last = refs.len() - 1;
                refs[last].push(name);
            }
            TodoEntry::Commit { action, oid, .. } => {
                return Err(format!("Unexpected todo line: {} {oid}", action.word()));
            }
            TodoEntry::Break => return Err("Unexpected todo line: break".into()),
        }
    }
    Ok(Segments { commits, refs })
}

/// For each step, the index of the one todo commit its id names: a
/// bijection, with every step kept in its commit's segment and the
/// segments in todo order.
fn match_steps(steps: &[Step], segments: &Segments) -> Result<Vec<usize>, String> {
    let commits = &segments.commits;
    if steps.len() != commits.len() {
        return Err(format!(
            "The todo list has {} commits, the plan {}",
            commits.len(),
            steps.len()
        ));
    }
    let mut used = vec![false; commits.len()];
    let mut matched = Vec::with_capacity(steps.len());
    let mut segment = 0;
    for step in steps {
        let full = step.commit.as_str();
        let mut found = commits
            .iter()
            .enumerate()
            .filter(|(_, commit)| commit.oid.len() >= 4 && full.starts_with(commit.oid));
        let (Some((at, commit)), None) = (found.next(), found.next()) else {
            return Err(format!("The todo list does not name {full} exactly once"));
        };
        if std::mem::replace(&mut used[at], true) {
            return Err(format!("The plan names {full} twice"));
        }
        if commit.segment < segment {
            return Err(crossing(full, segments.refs[commit.segment][0]));
        }
        segment = commit.segment;
        matched.push(at);
    }
    Ok(matched)
}

/// The todo list to write: each segment's steps in plan order, a `break`
/// after each that stops, then that segment's `update-ref` lines.
/// Commits are named by the step's full id, never git's abbreviation,
/// since a short id can resolve to a branch or tag of the same name.
fn emit(steps: &[Step], matched: &[usize], segments: &Segments) -> Vec<u8> {
    let mut out = Vec::new();
    for (segment, names) in segments.refs.iter().enumerate() {
        for (step, &at) in steps.iter().zip(matched) {
            let commit = &segments.commits[at];
            if commit.segment != segment {
                continue;
            }
            out.extend_from_slice(step.action.word().as_bytes());
            out.push(b' ');
            out.extend_from_slice(step.commit.as_str().as_bytes());
            if !commit.rest.is_empty() {
                out.push(b' ');
                out.extend_from_slice(commit.rest);
            }
            out.push(b'\n');
            if step.break_after {
                out.extend_from_slice(b"break\n");
            }
        }
        for name in names {
            out.extend_from_slice(b"update-ref ");
            out.extend_from_slice(name);
            out.push(b'\n');
        }
    }
    out
}

/// Whether `after` is a safe edit of the todo list `before` of a rebase
/// that has stopped: the same commits with the same subjects, the same
/// `update-ref` lines in the same order, each commit in its segment, and
/// only actions, order and `break`s changed. With nothing done yet
/// (`done_empty`), the first commit cannot be a squash or fixup, as there
/// is nothing before it to fold into.
pub fn validate_edit(
    before: &[TodoEntry],
    after: &[TodoEntry],
    done_empty: bool,
) -> Result<(), String> {
    let refs = |entries: &[TodoEntry]| -> Vec<Vec<u8>> {
        entries
            .iter()
            .filter_map(|entry| match entry {
                TodoEntry::UpdateRef(name) => Some(name.clone()),
                _ => None,
            })
            .collect()
    };
    let before_refs = refs(before);
    if before_refs != refs(after) {
        return Err("The branch updates in the todo list would change".into());
    }
    let segments = |entries: &[TodoEntry]| -> Vec<Vec<(String, Vec<u8>)>> {
        let mut out = vec![Vec::new()];
        for entry in entries {
            match entry {
                TodoEntry::Commit { oid, rest, .. } => {
                    let last = out.len() - 1;
                    out[last].push((oid.clone(), rest.clone()));
                }
                TodoEntry::UpdateRef(_) => out.push(Vec::new()),
                TodoEntry::Break => {}
            }
        }
        for segment in &mut out {
            segment.sort();
        }
        out
    };
    let (old, new) = (segments(before), segments(after));
    let all = |segments: &[Vec<(String, Vec<u8>)>]| {
        let mut all: Vec<(String, Vec<u8>)> = segments.concat();
        all.sort();
        all
    };
    if all(&old) != all(&new) {
        return Err("The commits in the todo list would change".into());
    }
    if let Some(seg) = (0..old.len()).find(|&i| old[i] != new[i]) {
        let moved = old[seg]
            .iter()
            .find(|commit| !new[seg].contains(commit))
            .or_else(|| new[seg].iter().find(|commit| !old[seg].contains(commit)))
            .map_or("", |(oid, _)| oid.as_str());
        let branch = &before_refs[seg.min(before_refs.len().saturating_sub(1))];
        return Err(crossing(moved, branch));
    }
    let first = after.iter().find_map(|entry| match entry {
        TodoEntry::Commit { action, .. } => Some(*action),
        _ => None,
    });
    if done_empty && matches!(first, Some(TodoAction::Squash | TodoAction::Fixup)) {
        return Err(NOTHING_TO_FOLD.into());
    }
    Ok(())
}

/// Why the first commit of a rebase that has done nothing yet cannot be
/// folded.
pub const NOTHING_TO_FOLD: &str = "Nothing before it to fold into";

/// A small plan for tests elsewhere: drop `b…` from `c…`, `b…`, `a…`.
#[cfg(test)]
pub fn sample() -> RebasePlan {
    let id = |digit: &str| CommitId::parse(&digit.repeat(40)).unwrap();
    RebasePlan {
        base: id("c"),
        steps: vec![
            Step {
                action: TodoAction::Drop,
                commit: id("b"),
                break_after: false,
            },
            Step {
                action: TodoAction::Pick,
                commit: id("a"),
                break_after: false,
            },
        ],
        update_refs: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A full id made of `digit`.
    fn id(digit: char) -> String {
        digit.to_string().repeat(40)
    }

    fn row(oid: &str, parents: &[&str]) -> CommitRow {
        CommitRow {
            oid: oid.into(),
            short_oid: oid.chars().take(7).collect(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
            author: "Ann".into(),
            time: 0,
            subject: "s".into(),
            subject_altered: false,
        }
    }

    /// `HEAD` = `a`, then `b`, `c`, `d` on a line, with `d` on `e`. `e` is
    /// not loaded.
    fn linear() -> Vec<CommitRow> {
        let ids = ['a', 'b', 'c', 'd', 'e'].map(id);
        (0..4).map(|i| row(&ids[i], &[&ids[i + 1]])).collect()
    }

    fn step(action: TodoAction, digit: char) -> Step {
        step_for(action, &id(digit))
    }

    /// A step for `oid`, taken as it is.
    fn step_for(action: TodoAction, oid: &str) -> Step {
        Step {
            action,
            commit: CommitId::parse(oid).unwrap(),
            break_after: false,
        }
    }

    fn pick(digit: char) -> Step {
        step(TodoAction::Pick, digit)
    }

    fn plan_of(base: char, steps: Vec<Step>) -> RebasePlan {
        RebasePlan {
            base: cid(base),
            steps,
            update_refs: false,
        }
    }

    fn cid(digit: char) -> CommitId {
        CommitId::parse(&id(digit)).unwrap()
    }

    #[test]
    fn drop_and_reword_rewrite_the_commit_and_those_above_it() {
        for (edit, action) in [
            (Edit::Drop, TodoAction::Drop),
            (Edit::Reword, TodoAction::Reword),
        ] {
            assert_eq!(
                plan(&linear(), 1, edit),
                Ok(plan_of(
                    'c',
                    vec![step(action, 'b'), step(TodoAction::Pick, 'a')]
                )),
                "{edit:?}"
            );
        }
        assert_eq!(
            plan(&linear(), 0, Edit::Drop),
            Ok(plan_of('b', vec![step(TodoAction::Drop, 'a')]))
        );
    }

    #[test]
    fn plan_stops_edits_each_stop_from_the_parent_of_the_oldest() {
        let edit = |digit| step(TodoAction::Edit, digit);
        assert_eq!(
            plan_stops(&linear(), &[1]),
            Ok(plan_of('c', vec![edit('b'), pick('a')]))
        );
        // A newer target: the source, then the target.
        assert_eq!(
            plan_stops(&linear(), &[2, 0]),
            Ok(plan_of('d', vec![edit('c'), pick('b'), edit('a')]))
        );
        assert_eq!(
            plan_stops(&linear(), &[0, 2]),
            plan_stops(&linear(), &[2, 0])
        );
        assert_eq!(plan_stops(&linear(), &[]), Err("No commits"));
    }

    #[test]
    fn plan_stops_refuses_root_and_merge_commits() {
        let ids = ['a', 'b', 'c', 'd'].map(id);
        let root = vec![row(&ids[0], &[&ids[1]]), row(&ids[1], &[])];
        assert_eq!(
            plan_stops(&root, &[1]),
            Err("Cannot rewrite the first commit")
        );
        let merge = vec![
            row(&ids[0], &[&ids[1]]),
            row(&ids[1], &[&ids[2], &ids[3]]),
            row(&ids[2], &[&ids[3]]),
        ];
        assert_eq!(
            plan_stops(&merge, &[1]),
            Err("Cannot rewrite across a merge")
        );
        assert_eq!(
            plan_stops(&merge, &[2]),
            Err("Cannot rewrite across a merge")
        );
        assert_eq!(plan_stops(&linear(), &[4]), Err(LOAD_MORE));
    }

    #[test]
    fn squash_and_fixup_fold_the_commit_into_the_one_below() {
        for (edit, action) in [
            (Edit::Squash, TodoAction::Squash),
            (Edit::Fixup, TodoAction::Fixup),
        ] {
            assert_eq!(
                plan(&linear(), 1, edit),
                Ok(plan_of(
                    'd',
                    vec![
                        step(TodoAction::Pick, 'c'),
                        step(action, 'b'),
                        step(TodoAction::Pick, 'a'),
                    ]
                )),
                "{edit:?}"
            );
        }
    }

    #[test]
    fn the_last_loaded_commit_can_be_dropped_but_not_folded() {
        let rows = linear();
        assert_eq!(plan(&rows, 3, Edit::Drop).unwrap().base, cid('e'));
        assert_eq!(plan(&rows, 3, Edit::Squash), Err(LOAD_MORE));
        assert_eq!(plan(&rows, 3, Edit::Fixup), Err(LOAD_MORE));
    }

    #[test]
    fn a_merge_in_the_range_is_refused() {
        let mut rows = linear();
        rows[0].parents.push(id('f'));
        for edit in [Edit::Squash, Edit::Fixup, Edit::Drop, Edit::Reword] {
            assert_eq!(
                plan(&rows, 1, edit),
                Err("Cannot rewrite across a merge"),
                "{edit:?}"
            );
        }
        // The commit below a fold is in the range too.
        let mut rows = linear();
        rows[2].parents.push(id('f'));
        assert_eq!(plan(&rows, 1, Edit::Drop).map(|p| p.base), Ok(cid('c')));
        assert_eq!(
            plan(&rows, 1, Edit::Squash),
            Err("Cannot rewrite across a merge")
        );
        // Rows that are not one first-parent chain, as a graph shows a
        // merged branch, are refused the same way.
        let mut rows = linear();
        rows[0].parents = vec![id('f')];
        assert_eq!(
            plan(&rows, 1, Edit::Drop),
            Err("Cannot rewrite across a merge")
        );
    }

    #[test]
    fn the_root_commit_cannot_be_rewritten_or_folded_into() {
        let mut rows = linear();
        rows[3].parents.clear();
        for edit in [Edit::Squash, Edit::Fixup, Edit::Drop, Edit::Reword] {
            assert_eq!(
                plan(&rows, 3, edit),
                Err("Cannot rewrite the first commit"),
                "{edit:?}"
            );
        }
        assert_eq!(
            plan(&rows, 2, Edit::Squash),
            Err("The commit below is the first commit")
        );
        assert_eq!(
            plan(&rows, 2, Edit::Fixup),
            Err("The commit below is the first commit")
        );
        assert!(plan(&rows, 2, Edit::Drop).is_ok());
    }

    #[test]
    fn no_selection_is_refused() {
        assert_eq!(plan(&[], 0, Edit::Drop), Err("No commits"));
    }

    #[test]
    fn more_than_max_steps_are_refused() {
        let n = MAX_STEPS;
        let ids: Vec<String> = (0..=n + 2).map(|i| format!("{i:040x}")).collect();
        let rows: Vec<CommitRow> = (0..n + 2).map(|i| row(&ids[i], &[&ids[i + 1]])).collect();
        assert_eq!(plan(&rows, n - 1, Edit::Drop).unwrap().steps.len(), n);
        assert_eq!(plan(&rows, n, Edit::Drop), Err("Too many commits above it"));
        assert_eq!(plan(&rows, n - 2, Edit::Squash).unwrap().steps.len(), n);
        assert_eq!(
            plan(&rows, n - 1, Edit::Squash),
            Err("Too many commits above it")
        );
    }

    #[test]
    fn a_range_folds_every_selected_commit_into_the_one_below_it() {
        for (edit, action) in [
            (Edit::Squash, TodoAction::Squash),
            (Edit::Fixup, TodoAction::Fixup),
        ] {
            assert_eq!(
                plan_range(&linear(), &[1, 2], edit),
                Ok(plan_of(
                    'e',
                    vec![pick('d'), step(action, 'c'), step(action, 'b'), pick('a')]
                )),
                "{edit:?}"
            );
        }
    }

    #[test]
    fn a_range_drop_drops_every_selected_commit_from_the_parent_of_the_oldest() {
        assert_eq!(
            plan_range(&linear(), &[0, 1], Edit::Drop),
            Ok(plan_of(
                'c',
                vec![step(TodoAction::Drop, 'b'), step(TodoAction::Drop, 'a')]
            ))
        );
    }

    #[test]
    fn a_one_row_range_plans_as_the_single_row_action() {
        for edit in [Edit::Squash, Edit::Fixup, Edit::Drop] {
            assert_eq!(
                plan_range(&linear(), &[1], edit),
                plan(&linear(), 1, edit),
                "{edit:?}"
            );
        }
    }

    #[test]
    fn a_range_with_a_gap_or_none_is_refused() {
        for edit in [Edit::Squash, Edit::Fixup, Edit::Drop] {
            assert_eq!(plan_range(&linear(), &[0, 2], edit), Err(RANGE_GAP));
            assert_eq!(plan_range(&linear(), &[], edit), Err(RANGE_GAP));
            assert_eq!(plan_range(&linear(), &[1, 0], edit), Err(RANGE_GAP));
        }
    }

    #[test]
    fn a_range_refuses_every_other_edit() {
        for edit in [Edit::Reword, Edit::Stop, Edit::Pick] {
            assert_eq!(
                plan_range(&linear(), &[0, 1], edit),
                Err(RANGE_EDIT),
                "{edit:?}"
            );
        }
    }

    #[test]
    fn a_range_holding_the_root_commit_is_refused() {
        let mut rows = linear();
        rows[3].parents.clear();
        for edit in [Edit::Squash, Edit::Fixup, Edit::Drop] {
            assert_eq!(
                plan_range(&rows, &[2, 3], edit),
                Err("Cannot rewrite the first commit"),
                "{edit:?}"
            );
        }
    }

    #[test]
    fn a_range_fold_needs_the_commit_below_loaded() {
        assert_eq!(plan_range(&linear(), &[2, 3], Edit::Squash), Err(LOAD_MORE));
        assert_eq!(plan_range(&linear(), &[2, 3], Edit::Fixup), Err(LOAD_MORE));
        assert_eq!(
            plan_range(&linear(), &[2, 3], Edit::Drop).map(|p| p.base),
            Ok(cid('e'))
        );
        assert_eq!(
            plan_range(&linear(), &[3, 4], Edit::Drop),
            Err("No commits")
        );
    }

    #[test]
    fn a_range_across_a_merge_is_refused() {
        let mut rows = linear();
        rows[1].parents.push(id('f'));
        for edit in [Edit::Squash, Edit::Fixup, Edit::Drop] {
            assert_eq!(
                plan_range(&rows, &[0, 1], edit),
                Err("Cannot rewrite across a merge"),
                "{edit:?}"
            );
        }
    }

    #[test]
    fn a_range_past_max_steps_is_refused() {
        let n = MAX_STEPS;
        let ids: Vec<String> = (0..=n + 2).map(|i| format!("{i:040x}")).collect();
        let rows: Vec<CommitRow> = (0..n + 2).map(|i| row(&ids[i], &[&ids[i + 1]])).collect();
        let all: Vec<usize> = (0..n).collect();
        assert_eq!(plan_range(&rows, &all, Edit::Drop).unwrap().steps.len(), n);
        let too_many: Vec<usize> = (0..=n).collect();
        assert_eq!(
            plan_range(&rows, &too_many, Edit::Drop),
            Err("Too many commits above it")
        );
        assert_eq!(
            plan_range(&rows, &all, Edit::Squash),
            Err("Too many commits above it")
        );
    }

    #[test]
    fn the_largest_plan_fits_in_one_env_var() {
        let ids: Vec<String> = (0..=MAX_STEPS).map(|i| format!("{i:064x}")).collect();
        let rows: Vec<CommitRow> = (0..MAX_STEPS)
            .map(|i| row(&ids[i], &[&ids[i + 1]]))
            .collect();
        let mut steps = plan(&rows, MAX_STEPS - 1, Edit::Reword).unwrap().steps;
        // The longest word on every line.
        for step in &mut steps {
            step.action = TodoAction::Reword;
        }
        let len = "KIKA_REBASE_PLAN=".len() + encode(&steps).len();
        let limit = if cfg!(windows) { 32_767 } else { 128 * 1024 };
        assert!(len < limit, "{len} >= {limit}");
    }

    #[test]
    fn ids_that_are_not_full_hex_are_refused() {
        let cases: [fn(&mut Vec<CommitRow>); 4] = [
            |rows| rows[0].oid = "abc1234".into(),
            |rows| rows[0].oid = "--upload-pack=x".into(),
            |rows| rows[1].parents = vec!["c".repeat(39)],
            |rows| rows[0].oid = "g".repeat(40),
        ];
        for (n, break_it) in cases.into_iter().enumerate() {
            let mut rows = linear();
            break_it(&mut rows);
            // Keep the chain intact where only an id was broken.
            if n != 2 {
                rows.truncate(2);
            }
            let result = plan(&rows, if n == 2 { 1 } else { 0 }, Edit::Drop);
            assert_eq!(result, Err("Not a commit id"), "case {n}");
        }
        let mut sha256 = linear();
        sha256[0].oid = "a".repeat(64);
        sha256[0].parents = vec!["b".repeat(64)];
        assert_eq!(
            plan(&sha256[..1], 0, Edit::Reword).map(|p| p.base),
            Ok(CommitId::parse(&"b".repeat(64)).unwrap())
        );
    }

    #[test]
    fn encode_and_decode_round_trip() {
        let steps = plan(&linear(), 1, Edit::Squash).unwrap().steps;
        let text = encode(&steps);
        assert_eq!(
            text,
            format!("pick {}\nsquash {}\npick {}\n", id('c'), id('b'), id('a'))
        );
        assert_eq!(decode(&text), Ok(steps));
        let all = TodoAction::ALL.map(|action| step(action, 'a'));
        assert_eq!(decode(&encode(&all)), Ok(all.to_vec()));
    }

    #[test]
    fn edit_and_break_encode_as_their_own_lines_and_round_trip() {
        let stop = plan(&linear(), 1, Edit::Stop).unwrap();
        assert_eq!(
            encode(&stop.steps),
            format!("edit {}\npick {}\n", id('b'), id('a'))
        );
        assert_eq!(decode(&encode(&stop.steps)), Ok(stop.steps));
        let stops = plan_break(&linear(), 1).unwrap();
        let text = encode(&stops.steps);
        assert_eq!(text, format!("pick {}\nbreak\npick {}\n", id('b'), id('a')));
        assert_eq!(decode(&text), Ok(stops.steps));
    }

    #[test]
    fn decode_refuses_what_encode_would_not_write() {
        let a = id('a');
        for bad in [
            String::new(),
            "\n".into(),
            format!("exec {a}\n"),
            "exec make\n".into(),
            "label onto\n".into(),
            "update-ref refs/heads/x\n".into(),
            format!("e {a}\n"),
            format!("p {a}\n"),
            format!("Pick {a}\n"),
            format!("pick {}\n", &a[..39]),
            format!("pick {}\n", "g".repeat(40)),
            format!("pick {}\n", "A".repeat(40)),
            format!("pick {}\n", &a[..7]),
            format!("pick {a} subject\n"),
            format!("pick  {a}\n"),
            format!("pick {a}\r\n"),
            format!("pick {a}\n\n"),
            "pick\n".into(),
            "break\n".into(),
            format!("break\npick {a}\n"),
            format!("pick {a}\nbreak\nbreak\n"),
            format!("pick {a}\nbreak \n"),
            format!("pick {a}\nb\n"),
        ] {
            assert!(decode(&bad).is_err(), "{bad:?}");
        }
    }

    fn commit(action: TodoAction, oid: &str, rest: &str) -> TodoEntry {
        TodoEntry::Commit {
            action,
            oid: oid.into(),
            rest: rest.as_bytes().to_vec(),
        }
    }

    fn line(oid: &str, rest: &str) -> TodoEntry {
        commit(TodoAction::Pick, oid, rest)
    }

    fn update_ref(name: &str) -> TodoEntry {
        TodoEntry::UpdateRef(format!("refs/heads/{name}").into_bytes())
    }

    #[test]
    fn parse_todo_keeps_commands_and_drops_comments() {
        let todo = b"pick 1234abc first one\r\n\
                     p 5678def\n\
                     \n\
                     # Rebase 1..2 onto 1\n\
                     pick abcd  two  spaces \xff\n";
        assert_eq!(
            parse_todo(todo, b"#"),
            Ok(vec![
                line("1234abc", "first one"),
                line("5678def", ""),
                TodoEntry::Commit {
                    action: TodoAction::Pick,
                    oid: "abcd".into(),
                    rest: b" two  spaces \xff".to_vec(),
                },
            ])
        );
        assert_eq!(parse_todo(b"# only comments\n\n", b"#"), Ok(vec![]));
    }

    #[test]
    fn a_nul_byte_ends_the_lines_git_reads_and_makes_the_list_unparsed() {
        let todo = b"pick 1111 one\npick 2222 two\0x\npick 3333 three\n";
        assert!(parse_todo(todo, b"#").is_err());
        assert!(parse_todo(b"\0", b"#").is_err());
        let lines: Vec<&[u8]> = command_lines(todo, b"#").collect();
        assert_eq!(lines, [&b"pick 1111 one"[..], b"pick 2222 two"]);
    }

    #[test]
    fn only_blanks_and_hash_comments_are_skipped_everything_else_must_parse() {
        assert_eq!(
            parse_todo(b"# c\n  # c\n\n \t\n \rexec x\r\n\r\npick 1234 a\n", b"#"),
            Ok(vec![line("1234", "a")])
        );
        for bad in [
            &b"\xef\xbb\xbfpick 1234 a\n"[..],
            b"-pick 1234 a\n",
            b"\x0bpick 1234 a\n",
            b"; other comment char\n",
            b"1234 a\n",
        ] {
            assert!(
                parse_todo(bad, b"#").is_err(),
                "{}",
                String::from_utf8_lossy(bad)
            );
            assert_eq!(command_lines(bad, b"#").count(), 1);
        }
    }

    #[test]
    fn only_lines_led_by_the_comment_prefix_are_comments() {
        let todo = b"// Rebase 1..2 onto 1\n  // note\n\tpick 1234 a\n//\n";
        assert_eq!(parse_todo(todo, b"//"), Ok(vec![line("1234", "a")]));
        assert_eq!(
            parse_todo(b"; c\n  ;c\npick 1234 a\n", b";"),
            Ok(vec![line("1234", "a")])
        );
        // `#` is not special once the prefix is something else, and a
        // prefix is matched whole.
        for (bad, comment) in [
            (&b"# help\npick 1234 a\n"[..], &b";"[..]),
            (b"/ half\npick 1234 a\n", b"//"),
            (b"exec make\n", b";"),
        ] {
            assert!(parse_todo(bad, comment).is_err(), "{bad:?}");
            assert!(unreadable(bad, comment).is_some(), "{bad:?}");
        }
    }

    #[test]
    fn an_empty_comment_prefix_hides_only_blank_and_carriage_return_lines() {
        let todo = b"\n \t\n\rx\npick 1234 a\n";
        assert_eq!(parse_todo(todo, b""), Ok(vec![line("1234", "a")]));
        assert_eq!(command_lines(b"# c\n; c\n", b"").count(), 2);
        assert!(parse_todo(b"# c\npick 1234 a\n", b"").is_err());
        assert_eq!(
            unreadable(b"pick 1234 a\n# c\n", b""),
            Some(Unreadable::Line {
                number: 2,
                text: b"# c"
            })
        );
    }

    #[test]
    fn unreadable_names_the_first_line_kika_cannot_read_by_its_number() {
        assert_eq!(unreadable(b"# c\npick 1234 a\n", b"#"), None);
        assert_eq!(
            unreadable(b"# c\n\npick 1234 a\n  exec make\nlabel x\n", b"#"),
            Some(Unreadable::Line {
                number: 4,
                text: b"exec make"
            })
        );
        assert_eq!(unreadable(b"pick 1234 a\0\n", b"#"), Some(Unreadable::Nul));
    }

    #[test]
    fn indented_lines_are_commands_as_git_reads_them() {
        assert_eq!(
            parse_todo(
                b"  pick 1234 one\n\t pick 5678 two\n  # comment\n\t\n",
                b"#"
            ),
            Ok(vec![line("1234", "one"), line("5678", "two")])
        );
        for hidden in [
            &b"pick 1234 a\n\texec touch x\n"[..],
            b"   x make\n",
            b"\tlabel l\n",
        ] {
            assert!(
                parse_todo(hidden, b"#").is_err(),
                "{}",
                String::from_utf8_lossy(hidden)
            );
        }
    }

    #[test]
    fn parse_todo_accepts_long_and_short_words_breaks_and_update_refs() {
        let todo = b"r 1111 a\nreword 1112 b\ne 2222\nedit 2223\ns 3333\nsquash 3334\n\
                     f 4444\nfixup 4445\nd 5555\ndrop 5556\nb\nbreak\n\
                     update-ref refs/heads/x\n";
        use TodoAction::*;
        assert_eq!(
            parse_todo(todo, b"#"),
            Ok(vec![
                commit(Reword, "1111", "a"),
                commit(Reword, "1112", "b"),
                commit(Edit, "2222", ""),
                commit(Edit, "2223", ""),
                commit(Squash, "3333", ""),
                commit(Squash, "3334", ""),
                commit(Fixup, "4444", ""),
                commit(Fixup, "4445", ""),
                commit(Drop, "5555", ""),
                commit(Drop, "5556", ""),
                TodoEntry::Break,
                TodoEntry::Break,
                update_ref("x"),
            ])
        );
    }

    #[test]
    fn parse_todo_refuses_what_kika_does_not_edit() {
        for bad in [
            &b"exec make\n"[..],
            b"x make\n",
            b"label onto\n",
            b"l onto\n",
            b"reset onto\n",
            b"t onto\n",
            b"merge -C 1234abc x\n",
            b"m -C 1234abc x\n",
            b"noop\n",
            b"fixup -C 1234abc s\n",
            b"update-ref refs/tags/v1\n",
            b"update-ref refs/heads/\n",
            b"update-ref refs/heads/a b\n",
            b"update-ref\n",
            b"break now\n",
            b"pick 123 short\n",
            b"pick 123g567 s\n",
            b"pick\n",
            b"Pick 1234abc s\n",
        ] {
            assert!(
                parse_todo(bad, b"#").is_err(),
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn apply_writes_the_plan_with_git_s_subjects() {
        let steps = plan(&linear(), 1, Edit::Fixup).unwrap().steps;
        let todo = [
            line(&id('c')[..7], "third"),
            line(&id('b')[..4], ""),
            line(&id('a'), "first \u{e9}"),
        ];
        assert_eq!(
            String::from_utf8(apply(&steps, &todo).unwrap()).unwrap(),
            format!(
                "pick {} third\nfixup {}\npick {} first \u{e9}\n",
                id('c'),
                id('b'),
                id('a')
            )
        );
    }

    #[test]
    fn apply_writes_moves_by_id_and_a_break_after_its_step() {
        let moved = plan_move(&linear(), 1, Direction::Down).unwrap().steps;
        let todo = [
            line(&id('c')[..7], "c"),
            line(&id('b')[..7], "b"),
            line(&id('a')[..7], "a"),
        ];
        assert_eq!(
            String::from_utf8(apply(&moved, &todo).unwrap()).unwrap(),
            format!(
                "pick {} b\npick {} c\npick {} a\n",
                id('b'),
                id('c'),
                id('a')
            )
        );
        let stops = plan_break(&linear(), 1).unwrap().steps;
        assert_eq!(
            String::from_utf8(apply(&stops, &todo[1..]).unwrap()).unwrap(),
            format!("pick {} b\nbreak\npick {} a\n", id('b'), id('a'))
        );
    }

    #[test]
    fn apply_keeps_update_ref_lines_after_their_segment() {
        // `x` sits on `c`; `b` and `a` are above it.
        let todo = [
            line(&id('d')[..7], "d"),
            line(&id('c')[..7], "c"),
            update_ref("x"),
            line(&id('b')[..7], "b"),
            line(&id('a')[..7], "a"),
        ];
        let mut steps = vec![pick('d'), pick('c'), pick('a'), pick('b')];
        steps[0].action = TodoAction::Reword;
        steps[1].break_after = true;
        assert_eq!(
            String::from_utf8(apply(&steps, &todo).unwrap()).unwrap(),
            format!(
                "reword {} d\npick {} c\nbreak\nupdate-ref refs/heads/x\npick {} a\npick {} b\n",
                id('d'),
                id('c'),
                id('a'),
                id('b')
            )
        );
    }

    #[test]
    fn collect_todo_splits_commits_into_segments_at_update_refs() {
        let todo = [
            line("cccc", "c"),
            update_ref("x"),
            update_ref("y"),
            line("bbbb", "b"),
        ];
        let segments = collect_todo(&todo).unwrap();
        let at: Vec<(&str, usize)> = segments
            .commits
            .iter()
            .map(|c| (c.oid, c.segment))
            .collect();
        assert_eq!(at, [("cccc", 0), ("bbbb", 1)]);
        assert_eq!(
            segments.refs,
            [vec![&b"refs/heads/x"[..], b"refs/heads/y"], vec![]]
        );
        assert!(collect_todo(&[TodoEntry::Break]).is_err());
        assert!(collect_todo(&[commit(TodoAction::Drop, "cccc", "")]).is_err());
    }

    #[test]
    fn match_steps_pairs_each_step_with_its_todo_commit() {
        let todo = [line(&id('c')[..5], ""), line(&id('a')[..7], "")];
        let segments = collect_todo(&todo).unwrap();
        assert_eq!(
            match_steps(&[pick('a'), pick('c')], &segments),
            Ok(vec![1, 0])
        );
        assert!(match_steps(&[pick('a')], &segments).is_err());
        assert!(match_steps(&[pick('a'), pick('b')], &segments).is_err());
    }

    #[test]
    fn emit_writes_full_ids_subjects_breaks_and_refs() {
        let todo = [line("cccc", "c"), update_ref("x")];
        let segments = collect_todo(&todo).unwrap();
        let mut stop = pick('c');
        stop.break_after = true;
        assert_eq!(
            String::from_utf8(emit(&[stop], &[0], &segments)).unwrap(),
            format!("pick {} c\nbreak\nupdate-ref refs/heads/x\n", id('c'))
        );
    }

    #[test]
    fn apply_refuses_a_move_across_an_update_ref() {
        let todo = [
            line(&id('c')[..7], "c"),
            update_ref("x"),
            line(&id('b')[..7], "b"),
            line(&id('a')[..7], "a"),
        ];
        let steps = [pick('b'), pick('c'), pick('a')];
        assert_eq!(
            apply(&steps, &todo),
            Err("Moving ccccccc past branch x would change what it holds".into())
        );
    }

    #[test]
    fn apply_refuses_any_mismatch() {
        let steps = plan(&linear(), 1, Edit::Drop).unwrap().steps;
        let b = &id('b')[..7];
        let a = &id('a')[..7];
        for todo in [
            vec![line(b, "")],
            vec![line(b, ""), line(a, ""), line(a, "")],
            vec![line(b, ""), line("aaab", "")],
            vec![line(b, ""), line("aaa", "")],
            // Ambiguous: two todo commits match `a…`.
            vec![line(b, ""), line(a, ""), line(&a[..5], "")],
            vec![line(b, ""), commit(TodoAction::Edit, a, "")],
            vec![line(b, ""), TodoEntry::Break, line(a, "")],
            vec![],
        ] {
            assert!(apply(&steps, &todo).is_err(), "{todo:?}");
        }
        // The same commit named twice by the plan.
        let twice = [pick('a'), pick('a')];
        assert!(apply(&twice, &[line(a, ""), line(b, "")]).is_err());
        // The order of the todo does not matter, only which commits.
        assert!(apply(&steps, &[line(a, ""), line(b, "")]).is_ok());
    }

    #[test]
    fn apply_names_every_action_s_commit_by_its_full_id() {
        let digits = ['a', 'b', 'c', 'd', 'e', 'f'];
        for len in [40, 64] {
            let full = digits.map(|d| d.to_string().repeat(len));
            let steps: Vec<Step> = TodoAction::ALL
                .into_iter()
                .zip(&full)
                .map(|(action, oid)| step_for(action, oid))
                .collect();
            // Git abbreviates to as few as 4 digits.
            let todo: Vec<TodoEntry> = full
                .iter()
                .zip([4, 5, 6, 7, 4, 7])
                .map(|(oid, short)| line(&oid[..short], "subject"))
                .collect();
            let out = String::from_utf8(apply(&steps, &todo).unwrap()).unwrap();
            let lines: Vec<&str> = out.lines().collect();
            assert_eq!(lines.len(), steps.len(), "{out}");
            for (text, step) in lines.iter().zip(&steps) {
                let mut fields = text.split(' ');
                assert_eq!(fields.next(), Some(step.action.word()), "{text}");
                let oid = fields.next().unwrap();
                assert_eq!(oid.len(), len, "{text}");
                assert!(
                    oid.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
                    "{text}"
                );
                assert_eq!(oid, step.commit.as_str(), "{text}");
            }
        }
    }

    #[test]
    fn apply_refuses_a_plan_with_any_id_that_is_not_full() {
        let a = id('a');
        let b = id('b');
        let todo = [line(&a[..7], "a"), line(&b[..7], "b")];
        assert!(apply(&[step_for(TodoAction::Pick, &a), pick('b')], &todo).is_ok());
        let upper_todo = [line("AAAAAAA", "a"), line(&b[..7], "b")];
        for (steps, todo) in [
            // A short id, though it matches the todo's abbreviation.
            (vec![step_for(TodoAction::Pick, &a[..7]), pick('b')], &todo),
            (vec![pick('a'), step_for(TodoAction::Drop, &b[..39])], &todo),
            // Uppercase hex, even against a todo spelled the same way.
            (
                vec![step_for(TodoAction::Pick, &"A".repeat(40)), pick('b')],
                &upper_todo,
            ),
            // SHA-1 and SHA-256 ids in one plan.
            (
                vec![pick('a'), step_for(TodoAction::Pick, &"b".repeat(64))],
                &todo,
            ),
        ] {
            assert!(apply(&steps, todo).is_err(), "{steps:?}");
        }
    }

    #[test]
    fn move_up_and_down_swap_two_commits_and_pick_the_rest() {
        assert_eq!(
            plan_move(&linear(), 1, Direction::Up),
            Ok(plan_of('c', vec![pick('a'), pick('b')]))
        );
        assert_eq!(
            plan_move(&linear(), 1, Direction::Down),
            Ok(plan_of('d', vec![pick('b'), pick('c'), pick('a')]))
        );
        assert_eq!(
            plan_move(&linear(), 0, Direction::Down),
            Ok(plan_of('c', vec![pick('a'), pick('b')]))
        );
    }

    #[test]
    fn moves_are_refused_at_the_ends_across_merges_and_at_the_root() {
        let rows = linear();
        assert_eq!(
            plan_move(&rows, 0, Direction::Up),
            Err("Already the newest commit")
        );
        assert_eq!(plan_move(&rows, 3, Direction::Down), Err(LOAD_MORE));
        assert_eq!(plan_move(&[], 0, Direction::Down), Err("No commits"));
        let mut merge = linear();
        merge[1].parents.push(id('f'));
        assert_eq!(
            plan_move(&merge, 2, Direction::Up),
            Err("Cannot rewrite across a merge")
        );
        let mut root = linear();
        root[3].parents.clear();
        assert_eq!(
            plan_move(&root, 3, Direction::Up),
            Err("Cannot rewrite the first commit")
        );
        assert_eq!(
            plan_move(&root, 2, Direction::Down),
            Err("The commit below is the first commit")
        );
        assert!(plan_move(&root, 2, Direction::Up).is_ok());
    }

    #[test]
    fn a_break_picks_up_to_the_commit_and_stops_after_it() {
        let mut stop = pick('b');
        stop.break_after = true;
        assert_eq!(
            plan_break(&linear(), 1),
            Ok(plan_of('c', vec![stop, pick('a')]))
        );
        let mut root = linear();
        root[3].parents.clear();
        assert_eq!(plan_break(&root, 3), Err("Cannot rewrite the first commit"));
        assert_eq!(plan_break(&[], 0), Err("No commits"));
    }

    #[test]
    fn edit_stops_at_the_commit_and_pick_is_already_one() {
        assert_eq!(
            plan(&linear(), 1, Edit::Stop),
            Ok(plan_of('c', vec![step(TodoAction::Edit, 'b'), pick('a')]))
        );
        assert_eq!(plan(&linear(), 1, Edit::Pick), Err("Already a pick"));
    }

    fn todo_with_branch() -> Vec<TodoEntry> {
        vec![
            line("cccc", "c"),
            update_ref("x"),
            line("bbbb", "b"),
            TodoEntry::Break,
            line("aaaa", "a"),
        ]
    }

    #[test]
    fn validate_edit_allows_actions_order_and_breaks() {
        let before = todo_with_branch();
        let after = vec![
            commit(TodoAction::Reword, "cccc", "c"),
            TodoEntry::Break,
            update_ref("x"),
            line("aaaa", "a"),
            commit(TodoAction::Squash, "bbbb", "b"),
        ];
        assert_eq!(validate_edit(&before, &after, true), Ok(()));
    }

    #[test]
    fn validate_edit_refuses_changed_commits_and_branch_updates() {
        let before = todo_with_branch();
        let mut added = before.clone();
        added.push(line("dddd", "d"));
        let mut removed = before.clone();
        removed.remove(0);
        let mut duplicated = before.clone();
        duplicated.push(line("aaaa", "a"));
        let mut retitled = before.clone();
        retitled[0] = line("cccc", "other");
        let mut renamed = before.clone();
        renamed[1] = update_ref("y");
        let mut dropped_ref = before.clone();
        dropped_ref.remove(1);
        for after in [added, removed, duplicated, retitled, renamed, dropped_ref] {
            assert!(validate_edit(&before, &after, false).is_err(), "{after:?}");
        }
    }

    #[test]
    fn validate_edit_refuses_a_commit_moved_past_a_branch() {
        let before = todo_with_branch();
        let after = vec![
            line("bbbb", "b"),
            line("cccc", "c"),
            update_ref("x"),
            line("aaaa", "a"),
        ];
        assert_eq!(
            validate_edit(&before, &after, false),
            Err("Moving bbbb past branch x would change what it holds".into())
        );
    }

    #[test]
    fn validate_edit_refuses_a_fold_first_only_when_nothing_is_done() {
        let before = vec![line("aaaa", "a"), line("bbbb", "b")];
        for fold in [TodoAction::Squash, TodoAction::Fixup] {
            let after = vec![commit(fold, "aaaa", "a"), line("bbbb", "b")];
            assert_eq!(
                validate_edit(&before, &after, true),
                Err(NOTHING_TO_FOLD.into())
            );
            assert_eq!(validate_edit(&before, &after, false), Ok(()));
        }
    }

    /// A first-parent chain from `HEAD`, newest first: each `(digit,
    /// subject)` is a commit on the next, the last on `9…`, not loaded.
    fn titled(commits: &[(char, &str)]) -> Vec<CommitRow> {
        commits
            .iter()
            .enumerate()
            .map(|(i, &(digit, subject))| {
                let parent = commits.get(i + 1).map_or('9', |&(d, _)| d);
                CommitRow {
                    subject: subject.into(),
                    ..row(&id(digit), &[&id(parent)])
                }
            })
            .collect()
    }

    fn fixup(digit: char) -> Step {
        step(TodoAction::Fixup, digit)
    }

    #[test]
    fn autosquash_moves_a_fixup_after_its_target_and_picks_the_rest() {
        let rows = titled(&[
            ('f', "fixup! A"),
            ('c', "C"),
            ('b', "B"),
            ('a', "A"),
            ('0', "base"),
        ]);
        assert_eq!(
            plan_autosquash(&rows),
            Ok(plan_of(
                '0',
                vec![pick('a'), fixup('f'), pick('b'), pick('c')]
            ))
        );
    }

    #[test]
    fn autosquash_keeps_two_fixups_of_one_commit_in_their_order() {
        let rows = titled(&[('e', "fixup! A"), ('b', "B"), ('d', "fixup! A"), ('a', "A")]);
        assert_eq!(
            plan_autosquash(&rows),
            Ok(plan_of(
                '9',
                vec![pick('a'), fixup('d'), fixup('e'), pick('b')]
            ))
        );
    }

    #[test]
    fn autosquash_strips_every_fixup_prefix_to_find_the_target() {
        let rows = titled(&[('d', "fixup! fixup! A"), ('b', "B"), ('a', "A")]);
        assert_eq!(
            plan_autosquash(&rows),
            Ok(plan_of('9', vec![pick('a'), fixup('d'), pick('b')]))
        );
    }

    #[test]
    fn autosquash_folds_fixups_of_several_commits_from_the_deepest_target() {
        let rows = titled(&[
            ('e', "fixup! B"),
            ('d', "fixup! A"),
            ('b', "B"),
            ('a', "A"),
            ('0', "base"),
        ]);
        assert_eq!(
            plan_autosquash(&rows),
            Ok(plan_of(
                '0',
                vec![pick('a'), fixup('d'), pick('b'), fixup('e')]
            ))
        );
        assert_eq!(fixup_targets(&rows), Ok(vec![(0, 2), (1, 3)]));
    }

    #[test]
    fn autosquash_picks_the_nearest_older_commit_with_the_subject() {
        let rows = titled(&[('d', "fixup! A"), ('b', "A"), ('a', "A")]);
        assert_eq!(
            plan_autosquash(&rows),
            Ok(plan_of('a', vec![pick('b'), fixup('d')]))
        );
    }

    #[test]
    fn autosquash_needs_a_fixup_and_its_target_loaded() {
        let none = titled(&[('b', "B"), ('a', "A")]);
        assert_eq!(plan_autosquash(&none), Err(NO_FIXUPS));
        let lost = titled(&[('d', "fixup! Z"), ('b', "B"), ('a', "A")]);
        assert_eq!(plan_autosquash(&lost), Err(NO_TARGET));
        // A fixup is never another fixup's target.
        let only_fixups = titled(&[('d', "fixup! A"), ('b', "A x"), ('a', "fixup! A")]);
        assert_eq!(plan_autosquash(&only_fixups), Err(NO_TARGET));
        assert_eq!(plan_autosquash(&[]), Err(NO_FIXUPS));
    }

    #[test]
    fn autosquash_refuses_a_merge_before_the_target_and_a_root_target() {
        let mut merged = titled(&[('d', "fixup! A"), ('b', "B"), ('a', "A")]);
        merged[1].parents.push(id('7'));
        assert_eq!(plan_autosquash(&merged), Err(ACROSS_MERGE));
        let mut root = titled(&[('d', "fixup! A"), ('b', "B"), ('a', "A")]);
        root[2].parents.clear();
        assert_eq!(
            plan_autosquash(&root),
            Err("Cannot rewrite the first commit")
        );
    }

    #[test]
    fn autosquash_ignores_fixups_below_a_merge() {
        // The chain from HEAD ends at the merge; what is below is not
        // this branch's own line.
        let mut rows = titled(&[('d', "fixup! B"), ('b', "B"), ('c', "M"), ('e', "fixup! A")]);
        rows[2].parents.push(id('7'));
        assert_eq!(
            plan_autosquash(&rows),
            Ok(plan_of('c', vec![pick('b'), fixup('d')]))
        );
    }

    #[test]
    fn autosquash_refuses_a_fixup_or_target_whose_subject_was_made_safe() {
        // ` fixup! A` reads as `fixup! A` once trimmed, though git would
        // not take it for a fixup.
        let mut padded = titled(&[('d', "fixup! A"), ('b', "B"), ('a', "A")]);
        padded[0].subject_altered = true;
        assert_eq!(plan_autosquash(&padded), Err(INEXACT_SUBJECT));
        // `A\x01` and `A\x02` both read as `A\u{FFFD}`.
        let mut target = titled(&[('d', "fixup! A\u{FFFD}"), ('b', "B"), ('a', "A\u{FFFD}")]);
        target[2].subject_altered = true;
        assert_eq!(plan_autosquash(&target), Err(INEXACT_SUBJECT));
        // A commit in between with an altered subject plays no part.
        let mut between = titled(&[('d', "fixup! A"), ('b', "B"), ('a', "A")]);
        between[1].subject_altered = true;
        assert_eq!(
            plan_autosquash(&between),
            Ok(plan_of('9', vec![pick('a'), fixup('d'), pick('b')]))
        );
    }

    #[test]
    fn autosquash_refuses_more_than_max_steps() {
        let mut rows: Vec<CommitRow> = (0..=MAX_STEPS)
            .map(|i| {
                let oid = format!("{:040x}", i + 1);
                let parent = format!("{:040x}", i + 2);
                CommitRow {
                    subject: format!("s{i}"),
                    ..row(&oid, &[&parent])
                }
            })
            .collect();
        rows[0].subject = format!("fixup! s{MAX_STEPS}");
        assert_eq!(plan_autosquash(&rows), Err("Too many commits above it"));
    }

    #[test]
    fn branch_names_drop_refs_heads_and_control_characters() {
        assert_eq!(branch_name(b"refs/heads/feature/x"), "feature/x");
        assert!(!branch_name(b"refs/heads/a\x1b[31m").contains('\x1b'));
    }
}
