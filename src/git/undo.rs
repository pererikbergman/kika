//! The undo planner: from `HEAD`'s reflog, which action an undo or a redo
//! reverses, what git must do for it, and the words that say so before
//! anything happens. Pure, so every case is tested on a fixture.
//!
//! Kika marks its own steps in the reflog (see [`Direction::marker`]), so
//! the reflog alone says how far undo has gone: each undo marker steps
//! back over one more action, and each redo marker takes one back.

use super::branches::{Branch, BranchName};
use super::commit_files::CommitId;
use super::diff::terminal_safe;
use super::reflog::{Action, REDO_MARKER, Reflog, ReflogEntry, UNDO_MARKER};

/// Characters any reflog-derived text may take in the preview.
const ELIDE: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Undo,
    Redo,
}

impl Direction {
    /// What git writes in the reflog for this step, through
    /// `GIT_REFLOG_ACTION`: `checkout` logs it as it is, and `reset`
    /// as `<marker>: updating HEAD`.
    pub fn marker(self) -> &'static str {
        match self {
            Direction::Undo => UNDO_MARKER,
            Direction::Redo => REDO_MARKER,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Direction::Undo => "Undo",
            Direction::Redo => "Redo",
        }
    }

    fn verb(self) -> &'static str {
        match self {
            Direction::Undo => "undo",
            Direction::Redo => "redo",
        }
    }

    fn done(self) -> &'static str {
        match self {
            Direction::Undo => "undone",
            Direction::Redo => "redone",
        }
    }
}

/// What git does for a plan. Only hex ids and branch names read from
/// `for-each-ref` are ever in it: nothing from the reflog's text reaches
/// argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Move the current branch, or a detached `HEAD`, to this commit.
    MoveTo(CommitId),
    CheckoutBranch(BranchName),
    CheckoutDetached(CommitId),
}

/// A step and the lines that say what it will do, fixed together so the
/// question asked is the step run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub direction: Direction,
    pub step: Step,
    pub preview: Vec<String>,
}

/// What the planner needs from the rest of the state.
pub struct Context<'a> {
    /// The current branch's name; `None` with a detached `HEAD`.
    pub branch: Option<&'a str>,
    /// The local branches as loaded, whose names may reach argv.
    pub branches: &'a [Branch],
    /// A tracked change is staged, unstaged or conflicted. Untracked
    /// files do not count: `reset --keep` refuses to touch them.
    pub dirty: bool,
}

/// One user action, or one of Kika's markers, made of one or more
/// entries. `before` is the entry just older than the action: where
/// `HEAD` was before it.
enum Logical<'a> {
    Marker(Direction),
    Move {
        after: &'a ReflogEntry,
        before: Option<&'a ReflogEntry>,
    },
    /// A whole rebase, from the entry before its start to its finish.
    Rebase {
        after: &'a ReflogEntry,
        before: Option<&'a ReflogEntry>,
        branch: &'a [u8],
    },
    Checkout {
        after: &'a ReflogEntry,
        before: Option<&'a ReflogEntry>,
        from: &'a [u8],
        to: &'a [u8],
    },
    /// Anything Kika cannot reverse.
    Fixed(&'a ReflogEntry),
}

impl Logical<'_> {
    fn label(&self) -> String {
        match self {
            Logical::Rebase { .. } => "rebase".to_string(),
            Logical::Move { after: e, .. }
            | Logical::Checkout { after: e, .. }
            | Logical::Fixed(e) => e.label(),
            Logical::Marker(direction) => direction.marker().to_string(),
        }
    }
}

/// Plan the step that undoes or redoes the last action.
pub fn plan(direction: Direction, reflog: &Reflog, ctx: &Context) -> Result<Plan, String> {
    let actions = fold(&reflog.entries);
    let target = match direction {
        Direction::Undo => undo_target(&actions).ok_or("Nothing to undo")?,
        Direction::Redo => redo_target(&actions).ok_or("Nothing to redo")?,
    };
    build(direction, target, reflog, ctx)
}

/// Group the entries, newest first, into actions. A rebase's steps fold
/// into its finish; a move that left `HEAD` where it was, such as the
/// `reset: moving to HEAD` a stash writes, is no action at all.
fn fold(entries: &[ReflogEntry]) -> Vec<Logical<'_>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < entries.len() {
        let entry = &entries[i];
        let before = entries.get(i + 1);
        match &entry.action {
            Action::Undo => out.push(Logical::Marker(Direction::Undo)),
            Action::Redo => out.push(Logical::Marker(Direction::Redo)),
            Action::Move(_) => {
                let older = before.and_then(|b| b.oid.as_ref());
                let noop = entry.oid.is_some() && entry.oid.as_ref() == older;
                if !noop {
                    out.push(Logical::Move {
                        after: entry,
                        before,
                    });
                }
            }
            Action::Checkout { from, to } => out.push(Logical::Checkout {
                after: entry,
                before,
                from,
                to,
            }),
            Action::RebaseFinish { branch } => {
                let start = entries[i + 1..]
                    .iter()
                    .position(|e| e.action != Action::RebaseStep)
                    .map(|at| i + 1 + at)
                    .filter(|&at| entries[at].action == Action::RebaseStart);
                match start {
                    Some(start) => {
                        out.push(Logical::Rebase {
                            after: entry,
                            before: entries.get(start + 1),
                            branch,
                        });
                        i = start + 1;
                        continue;
                    }
                    None => out.push(Logical::Fixed(entry)),
                }
            }
            Action::CommitInitial | Action::RebaseStart | Action::RebaseStep | Action::Other => {
                out.push(Logical::Fixed(entry))
            }
        }
        i += 1;
    }
    out
}

/// The newest action not yet undone: each undo marker skips one more,
/// each redo marker one fewer.
fn undo_target<'a, 'b>(actions: &'b [Logical<'a>]) -> Option<&'b Logical<'a>> {
    let mut skip: i64 = 0;
    for action in actions {
        match action {
            Logical::Marker(Direction::Undo) => skip += 1,
            Logical::Marker(Direction::Redo) => skip -= 1,
            other if skip <= 0 => return Some(other),
            _ => skip -= 1,
        }
    }
    None
}

/// The action the newest undo not yet redone reversed. Any action of the
/// user's own in between ends redo, as in an editor.
fn redo_target<'a, 'b>(actions: &'b [Logical<'a>]) -> Option<&'b Logical<'a>> {
    let mut skip: i64 = 0;
    for (i, action) in actions.iter().enumerate() {
        match action {
            Logical::Marker(Direction::Redo) => skip += 1,
            Logical::Marker(Direction::Undo) if skip == 0 => {
                return undo_target(&actions[i + 1..]);
            }
            Logical::Marker(Direction::Undo) => skip -= 1,
            _ => return None,
        }
    }
    None
}

fn build(
    direction: Direction,
    target: &Logical,
    reflog: &Reflog,
    ctx: &Context,
) -> Result<Plan, String> {
    let label = elide(&target.label());
    let verb = direction.verb();
    let cannot = || format!("Cannot {verb} {label}: Kika does not know how to reverse it");
    let moved = || {
        format!(
            "HEAD has moved since {label}; nothing is {}",
            direction.done()
        )
    };
    let head = reflog.head.as_ref();
    match *target {
        Logical::Move { after, before } | Logical::Rebase { after, before, .. } => {
            let (Some(after_oid), Some(before)) = (after.oid.as_ref(), before) else {
                return Err(cannot());
            };
            let Some(before_oid) = before.oid.as_ref() else {
                return Err(cannot());
            };
            let entry = after;
            let after = (after_oid, entry.tree.as_ref());
            let before = (before_oid, before.tree.as_ref());
            let (from, to) = match direction {
                Direction::Undo => (after, before),
                Direction::Redo => (before, after),
            };
            if head != Some(from.0) {
                return Err(moved());
            }
            if let Logical::Rebase { branch, .. } = target
                && ctx.branch != Some(&String::from_utf8_lossy(branch))
            {
                return Err(moved());
            }
            if ctx.dirty {
                return Err("Commit or stash your changes first".into());
            }
            let (from_short, to_short) = (short(from.0), short(to.0));
            let branch = ctx.branch.map(|b| elide(&terminal_safe(b.as_bytes())));
            let mut preview = vec![
                format!("{}: {}", direction.title(), elide(&entry.display())),
                match &branch {
                    Some(b) => format!("Branch {b} moves from {from_short} to {to_short}."),
                    None => format!("Detached HEAD moves from {from_short} to {to_short}."),
                },
                if from.1.is_some() && from.1 == to.1 {
                    "Files stay the same (same tree).".to_string()
                } else {
                    format!("Files change to match {to_short}.")
                },
            ];
            if direction == Direction::Undo {
                let owner = branch.as_deref().unwrap_or("detached HEAD");
                preview.push(format!(
                    "Commits after {to_short} leave {owner}; redo brings them back."
                ));
            }
            Ok(Plan {
                direction,
                step: Step::MoveTo(to.0.clone()),
                preview,
            })
        }
        Logical::Checkout {
            after,
            before,
            from,
            to,
        } => {
            let (Some(after_oid), Some(before_oid)) =
                (after.oid.as_ref(), before.and_then(|b| b.oid.as_ref()))
            else {
                return Err(cannot());
            };
            let (leave, leave_oid, go) = match direction {
                Direction::Undo => (to, after_oid, from),
                Direction::Redo => (from, before_oid, to),
            };
            if head != Some(leave_oid) || !on(ctx, leave) {
                return Err(moved());
            }
            // Such a name would need `--end-of-options`, which git 2.15
            // lacks.
            if go.starts_with(b"-") {
                return Err(format!(
                    "Cannot {verb} {label}: {} is named like an option",
                    name(go)
                ));
            }
            let step = destination(ctx, go)
                .ok_or_else(|| format!("Cannot {verb} {label}: {} is not a branch", name(go)))?;
            let (x, y) = (name(leave), name(go));
            Ok(Plan {
                direction,
                step,
                preview: vec![
                    format!("{}: {}", direction.title(), elide(&after.display())),
                    format!("HEAD switches from {x} to {y}. No branch moves."),
                    format!(
                        "Files change to match {y}; uncommitted changes are carried over, \
                         and git refuses if one would be overwritten."
                    ),
                ],
            })
        }
        Logical::Fixed(_) | Logical::Marker(_) => Err(cannot()),
    }
}

/// Whether `HEAD` is on the checkout side named `name`: on that branch
/// when it is one, else detached.
fn on(ctx: &Context, name: &[u8]) -> bool {
    match loaded(ctx, name) {
        Some(branch) => ctx.branch == Some(&String::from_utf8_lossy(&branch.name.0)),
        None => ctx.branch.is_none(),
    }
}

/// Where a checkout to `name` goes: the loaded branch of that exact name,
/// else a detached `HEAD` at it when it is a full id.
fn destination(ctx: &Context, name: &[u8]) -> Option<Step> {
    if let Some(branch) = loaded(ctx, name) {
        return Some(Step::CheckoutBranch(branch.name.clone()));
    }
    full_id(name).map(Step::CheckoutDetached)
}

fn loaded<'a>(ctx: &Context<'a>, name: &[u8]) -> Option<&'a Branch> {
    ctx.branches.iter().find(|b| b.name.0 == name)
}

/// A whole SHA-1 or SHA-256 id, as git logs a detached `HEAD`.
fn full_id(name: &[u8]) -> Option<CommitId> {
    if !matches!(name.len(), 40 | 64) {
        return None;
    }
    std::str::from_utf8(name).ok().and_then(CommitId::parse)
}

/// A checkout side as the preview names it: a short id, or the name.
fn name(raw: &[u8]) -> String {
    match full_id(raw) {
        Some(id) => short(&id),
        None => elide(&terminal_safe(raw).replace('\n', " ")),
    }
}

fn short(id: &CommitId) -> String {
    id.as_str().chars().take(7).collect()
}

/// `text` cut to [`ELIDE`] characters, ending in `…` when cut.
fn elide(text: &str) -> String {
    if text.chars().count() <= ELIDE {
        return text.to_string();
    }
    let mut kept: String = text.chars().take(ELIDE - 1).collect();
    kept.push('…');
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::branches::Track;
    use crate::git::reflog::parse_records;

    const COMMIT: &[u8] = include_bytes!("fixtures/reflog/commit.bin");
    const CHECKOUT: &[u8] = include_bytes!("fixtures/reflog/checkout.bin");
    const DETACHED: &[u8] = include_bytes!("fixtures/reflog/checkout_detached.bin");
    const STASH: &[u8] = include_bytes!("fixtures/reflog/stash.bin");
    const REBASE: &[u8] = include_bytes!("fixtures/reflog/rebase.bin");
    const REBASE_2_15: &[u8] = include_bytes!("fixtures/reflog/rebase_2_15.bin");
    const REBASE_2_15_I: &[u8] = include_bytes!("fixtures/reflog/rebase_2_15_interactive.bin");
    const ABORTED: &[u8] = include_bytes!("fixtures/reflog/aborted_rebase.bin");

    fn id(c: char) -> String {
        c.to_string().repeat(40)
    }

    fn oid(c: char) -> CommitId {
        CommitId::parse(&id(c)).unwrap()
    }

    /// One raw record, as git prints it, to put before a fixture.
    fn record(oid: char, tree: char, message: &str) -> Vec<u8> {
        format!("{} {} {message}\0", id(oid), id(tree)).into_bytes()
    }

    fn reflog(records: &[&[u8]], head: char) -> Reflog {
        Reflog {
            head: CommitId::parse(&id(head)),
            entries: parse_records(&records.concat()),
        }
    }

    fn branches(names: &[&str]) -> Vec<Branch> {
        names
            .iter()
            .map(|n| Branch {
                name: (*n).into(),
                head: false,
                upstream: None,
                track: Track::None,
                merged: true,
            })
            .collect()
    }

    fn ctx<'a>(branch: Option<&'a str>, list: &'a [Branch]) -> Context<'a> {
        Context {
            branch,
            branches: list,
            dirty: false,
        }
    }

    fn undo(log: &Reflog, ctx: &Context) -> Result<Plan, String> {
        plan(Direction::Undo, log, ctx)
    }

    fn redo(log: &Reflog, ctx: &Context) -> Result<Plan, String> {
        plan(Direction::Redo, log, ctx)
    }

    fn step(result: Result<Plan, String>) -> Step {
        result.unwrap().step
    }

    #[test]
    fn undoing_a_commit_moves_the_branch_to_its_parent() {
        let main = branches(&["main"]);
        let plan = undo(&reflog(&[COMMIT], 'c'), &ctx(Some("main"), &main)).unwrap();
        assert_eq!(plan.step, Step::MoveTo(oid('b')));
        assert_eq!(plan.direction, Direction::Undo);
        assert_eq!(
            plan.preview,
            [
                "Undo: commit: second",
                "Branch main moves from ccccccc to bbbbbbb.",
                "Files change to match bbbbbbb.",
                "Commits after bbbbbbb leave main; redo brings them back.",
            ]
        );
    }

    #[test]
    fn undoing_a_checkout_goes_back_to_the_branch_it_came_from() {
        let list = branches(&["main", "side"]);
        let plan = undo(&reflog(&[CHECKOUT], 'c'), &ctx(Some("side"), &list)).unwrap();
        assert_eq!(plan.step, Step::CheckoutBranch("main".into()));
        assert_eq!(
            plan.preview,
            [
                "Undo: checkout: moving from main to side",
                "HEAD switches from side to main. No branch moves.",
                "Files change to match main; uncommitted changes are carried over, and git \
                 refuses if one would be overwritten.",
            ]
        );
    }

    #[test]
    fn undoing_a_checkout_from_a_detached_head_detaches_it_again() {
        let main = branches(&["main"]);
        let plan = undo(&reflog(&[DETACHED], 'c'), &ctx(Some("main"), &main)).unwrap();
        assert_eq!(plan.step, Step::CheckoutDetached(oid('b')));
        assert_eq!(
            plan.preview[1],
            "HEAD switches from main to bbbbbbb. No branch moves."
        );
    }

    #[test]
    fn a_checkout_is_allowed_with_local_changes_since_git_refuses_to_overwrite_them() {
        let list = branches(&["main", "side"]);
        let mut dirty = ctx(Some("side"), &list);
        dirty.dirty = true;
        assert_eq!(
            step(undo(&reflog(&[CHECKOUT], 'c'), &dirty)),
            Step::CheckoutBranch("main".into())
        );
    }

    #[test]
    fn undoing_a_rebase_of_either_git_era_moves_the_branch_to_its_old_tip() {
        let topic = branches(&["main", "topic"]);
        for fixture in [REBASE, REBASE_2_15, REBASE_2_15_I] {
            let plan = undo(&reflog(&[fixture], 'e'), &ctx(Some("topic"), &topic)).unwrap();
            assert_eq!(plan.step, Step::MoveTo(oid('f')));
            assert_eq!(
                plan.preview[1],
                "Branch topic moves from eeeeeee to fffffff."
            );
        }
    }

    #[test]
    fn a_rebase_is_undone_only_on_its_own_branch() {
        let list = branches(&["main", "topic"]);
        assert_eq!(
            undo(&reflog(&[REBASE], 'e'), &ctx(Some("main"), &list)),
            Err("HEAD has moved since rebase; nothing is undone".into())
        );
    }

    #[test]
    fn two_undos_in_a_row_reach_the_second_newest_action() {
        let main = branches(&["main"]);
        let marker = record('b', '2', "kika undo: updating HEAD");
        let plan = undo(&reflog(&[&marker, COMMIT], 'b'), &ctx(Some("main"), &main)).unwrap();
        assert_eq!(plan.step, Step::MoveTo(oid('a')));
        assert_eq!(plan.preview[0], "Undo: commit: first");
    }

    #[test]
    fn undo_then_redo_then_undo_reaches_the_same_action_again() {
        let main = branches(&["main"]);
        let ctx = ctx(Some("main"), &main);
        let undone = record('b', '2', "kika undo: updating HEAD");
        let plan = redo(&reflog(&[&undone, COMMIT], 'b'), &ctx).unwrap();
        assert_eq!(plan.step, Step::MoveTo(oid('c')));
        assert_eq!(plan.direction, Direction::Redo);
        assert_eq!(plan.preview.len(), 3, "no commits leave on a redo");

        let redone = record('c', '3', "kika redo: updating HEAD");
        let plan = undo(&reflog(&[&redone, &undone, COMMIT], 'c'), &ctx).unwrap();
        assert_eq!(plan.step, Step::MoveTo(oid('b')));
        assert_eq!(plan.preview[0], "Undo: commit: second");
    }

    #[test]
    fn redo_after_two_undos_redoes_the_older_one_first() {
        let main = branches(&["main"]);
        let ctx = ctx(Some("main"), &main);
        let first = record('b', '2', "kika undo: updating HEAD");
        let second = record('a', '1', "kika undo: updating HEAD");
        let plan = redo(&reflog(&[&second, &first, COMMIT], 'a'), &ctx).unwrap();
        assert_eq!(plan.step, Step::MoveTo(oid('b')));
        let redone = record('b', '2', "kika redo: updating HEAD");
        let plan = redo(&reflog(&[&redone, &second, &first, COMMIT], 'b'), &ctx).unwrap();
        assert_eq!(plan.step, Step::MoveTo(oid('c')));
    }

    #[test]
    fn redoing_a_checkout_goes_where_it_went() {
        let list = branches(&["main", "side"]);
        let undone = record('c', '3', "kika undo");
        let plan = redo(
            &reflog(&[&undone, CHECKOUT], 'c'),
            &ctx(Some("main"), &list),
        )
        .unwrap();
        assert_eq!(plan.step, Step::CheckoutBranch("side".into()));
    }

    #[test]
    fn a_new_action_after_an_undo_ends_redo() {
        let main = branches(&["main"]);
        let undone = record('b', '2', "kika undo: updating HEAD");
        let new = record('d', '4', "commit: another");
        assert_eq!(
            redo(
                &reflog(&[&new, &undone, COMMIT], 'd'),
                &ctx(Some("main"), &main)
            ),
            Err("Nothing to redo".into())
        );
        assert_eq!(
            redo(&reflog(&[COMMIT], 'c'), &ctx(Some("main"), &main)),
            Err("Nothing to redo".into())
        );
    }

    #[test]
    fn the_reset_a_stash_writes_is_skipped() {
        let main = branches(&["main"]);
        let plan = undo(&reflog(&[STASH], 'c'), &ctx(Some("main"), &main)).unwrap();
        assert_eq!(plan.step, Step::MoveTo(oid('b')));
        assert_eq!(plan.preview[0], "Undo: commit: second");
    }

    #[test]
    fn what_kika_cannot_reverse_is_refused_by_name() {
        let main = branches(&["main"]);
        let ctx = ctx(Some("main"), &main);
        let cannot = |label: &str| {
            Err(format!(
                "Cannot undo {label}: Kika does not know how to reverse it"
            ))
        };
        let other = record('c', '3', "branch: renamed");
        assert_eq!(
            undo(&reflog(&[&other, COMMIT], 'c'), &ctx),
            cannot("branch")
        );
        let bad = b"zzzz 3333 commit: odd\0".to_vec();
        assert_eq!(undo(&reflog(&[&bad, COMMIT], 'c'), &ctx), cannot("commit"));
        let initial = record('a', '1', "commit (initial): root");
        assert_eq!(
            undo(&reflog(&[&initial], 'a'), &ctx),
            cannot("commit (initial)")
        );
        assert_eq!(
            undo(&reflog(&[ABORTED], 'f'), &ctx),
            cannot("rebase (abort)")
        );
        assert_eq!(undo(&reflog(&[], 'a'), &ctx), Err("Nothing to undo".into()));
    }

    #[test]
    fn a_head_that_moved_since_the_action_refuses() {
        let main = branches(&["main"]);
        assert_eq!(
            undo(&reflog(&[COMMIT], 'd'), &ctx(Some("main"), &main)),
            Err("HEAD has moved since commit; nothing is undone".into())
        );
        let list = branches(&["main", "side"]);
        assert_eq!(
            undo(&reflog(&[CHECKOUT], 'c'), &ctx(Some("main"), &list)),
            Err("HEAD has moved since checkout; nothing is undone".into())
        );
    }

    #[test]
    fn local_changes_refuse_a_branch_move() {
        let main = branches(&["main"]);
        let mut dirty = ctx(Some("main"), &main);
        dirty.dirty = true;
        assert_eq!(
            undo(&reflog(&[COMMIT], 'c'), &dirty),
            Err("Commit or stash your changes first".into())
        );
    }

    #[test]
    fn a_from_name_that_is_no_loaded_branch_and_not_an_id_is_refused() {
        let side = branches(&["side"]);
        assert_eq!(
            undo(&reflog(&[CHECKOUT], 'c'), &ctx(Some("side"), &side)),
            Err("Cannot undo checkout: main is not a branch".into())
        );
        let short = record('c', '3', "checkout: moving from bbbbbbb to side");
        assert!(undo(&reflog(&[&short, COMMIT], 'c'), &ctx(Some("side"), &side)).is_err());
        let list = branches(&["-f", "side"]);
        let dash = record('c', '3', "checkout: moving from -f to side");
        assert_eq!(
            undo(&reflog(&[&dash, COMMIT], 'c'), &ctx(Some("side"), &list)),
            Err("Cannot undo checkout: -f is named like an option".into())
        );
    }

    #[test]
    fn identical_trees_say_the_files_stay_and_a_detached_head_is_named() {
        let amend = record('d', '3', "commit (amend): second, reworded");
        let plan = undo(&reflog(&[&amend, COMMIT], 'd'), &ctx(None, &[])).unwrap();
        assert_eq!(
            plan.preview,
            [
                "Undo: commit (amend): second, reworded",
                "Detached HEAD moves from ddddddd to ccccccc.",
                "Files stay the same (same tree).",
                "Commits after ccccccc leave detached HEAD; redo brings them back.",
            ]
        );
    }

    #[test]
    fn no_preview_line_carries_a_control_character_and_each_is_elided() {
        let hostile = format!(
            "checkout: moving from \x1b[31m{}\u{202E} to side",
            "x".repeat(100)
        );
        let name = format!("\x1b[31m{}\u{202E}", "x".repeat(100));
        let list = branches(&[name.as_str(), "side"]);
        let undone = record('c', '3', &hostile);
        let plan = undo(&reflog(&[&undone, COMMIT], 'c'), &ctx(Some("side"), &list)).unwrap();
        let evil = format!("commit: \x1b[2J{}\r\u{202E}", "y".repeat(100));
        let commit = record('d', '4', &evil);
        let branch = "ma\x1bin";
        let main = branches(&[branch]);
        let moved = undo(&reflog(&[&commit, COMMIT], 'd'), &ctx(Some(branch), &main)).unwrap();
        for line in plan.preview.iter().chain(&moved.preview) {
            assert!(
                !line.chars().any(|c| c.is_control() || c == '\u{202E}'),
                "{line:?}"
            );
            assert!(line.chars().count() < 200, "{line:?}");
        }
        assert!(plan.preview[0].ends_with('…'), "{:?}", plan.preview[0]);
    }
}
