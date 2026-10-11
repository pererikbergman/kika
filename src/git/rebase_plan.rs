//! Interactive rebase as data: which commits to rewrite and how, checked
//! against git's own todo list before git acts on it. Pure: no I/O, so the
//! UI and the hidden sequence editor agree on one plan.

use super::commit_files::CommitId;
use super::log::CommitRow;

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
}

/// One todo command. Every word exists in git 2.15.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoAction {
    Pick,
    Squash,
    Fixup,
    Drop,
    Reword,
}

impl TodoAction {
    pub fn word(self) -> &'static str {
        match self {
            TodoAction::Pick => "pick",
            TodoAction::Squash => "squash",
            TodoAction::Fixup => "fixup",
            TodoAction::Drop => "drop",
            TodoAction::Reword => "reword",
        }
    }

    fn from_word(word: &str) -> Option<Self> {
        [
            TodoAction::Pick,
            TodoAction::Squash,
            TodoAction::Fixup,
            TodoAction::Drop,
            TodoAction::Reword,
        ]
        .into_iter()
        .find(|action| action.word() == word)
    }
}

impl From<Edit> for TodoAction {
    fn from(edit: Edit) -> Self {
        match edit {
            Edit::Squash => TodoAction::Squash,
            Edit::Fixup => TodoAction::Fixup,
            Edit::Drop => TodoAction::Drop,
            Edit::Reword => TodoAction::Reword,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub action: TodoAction,
    pub commit: CommitId,
}

/// Rebase onto `base`, replaying `steps` oldest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebasePlan {
    pub base: CommitId,
    pub steps: Vec<Step>,
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
    if target.parents.is_empty() {
        return Err("Cannot rewrite the first commit");
    }
    let last = if folds { selected + 1 } else { selected };
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
    let base = full_id(&range[last].parents[0])?;
    let steps = range
        .iter()
        .enumerate()
        .rev()
        .map(|(i, row)| {
            let action = if i == selected {
                edit.into()
            } else {
                TodoAction::Pick
            };
            full_id(&row.oid).map(|commit| Step { action, commit })
        })
        .collect::<Result<_, _>>()?;
    Ok(RebasePlan { base, steps })
}

/// A full object id: 40 hex digits for SHA-1, 64 for SHA-256.
fn full_id(text: &str) -> Result<CommitId, &'static str> {
    CommitId::parse(text)
        .filter(|_| matches!(text.len(), 40 | 64))
        .ok_or("Not a commit id")
}

/// The plan's steps as text, one `<word> <oid>` line each, for the env
/// var the sequence editor reads.
pub fn encode(steps: &[Step]) -> String {
    steps
        .iter()
        .map(|step| format!("{} {}\n", step.action.word(), step.commit.as_str()))
        .collect()
}

/// [`encode`]'s text back as steps. Anything that `encode` would not have
/// written is refused.
pub fn decode(text: &str) -> Result<Vec<Step>, String> {
    let steps = text
        .strip_suffix('\n')
        .unwrap_or(text)
        .split('\n')
        .map(|line| {
            let mut fields = line.split(' ');
            let (Some(word), Some(oid), None) = (fields.next(), fields.next(), fields.next())
            else {
                return Err(format!("Not a plan line: {line:?}"));
            };
            let action = TodoAction::from_word(word)
                .ok_or_else(|| format!("Not a plan action: {word:?}"))?;
            let commit = full_id(oid).map_err(|e| format!("{e}: {oid:?}"))?;
            Ok(Step { action, commit })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if steps.is_empty() {
        return Err("The plan is empty".into());
    }
    Ok(steps)
}

/// A `pick` line from git's todo list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoLine {
    /// Possibly abbreviated, at least 4 hex digits.
    pub oid: String,
    /// What follows the id, usually the subject, kept byte for byte.
    pub rest: Vec<u8>,
}

/// The picks in git's todo list. Comments and blank lines are dropped;
/// any command but `pick` is an error, since Kika's plan only rewrites
/// picks.
pub fn parse_todo(todo: &[u8]) -> Result<Vec<TodoLine>, String> {
    todo.split(|&b| b == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
        // The comment char is configurable and unknown here; a command
        // always starts with a letter.
        .filter(|line| line.first().is_some_and(u8::is_ascii_alphanumeric))
        .map(parse_pick)
        .collect()
}

fn parse_pick(line: &[u8]) -> Result<TodoLine, String> {
    let shown = || String::from_utf8_lossy(line).into_owned();
    let (command, after) = split_field(line);
    if command != b"pick" && command != b"p" {
        return Err(format!("Unexpected todo line: {:?}", shown()));
    }
    let (oid, rest) = split_field(after);
    if oid.len() < 4 || !oid.iter().all(u8::is_ascii_hexdigit) {
        return Err(format!("Not a commit id in todo line: {:?}", shown()));
    }
    Ok(TodoLine {
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

/// The todo list git should run: `steps` in place of `todo`, which must
/// name the same commits in the same order. Any difference means the
/// history is not what the plan was built from, so it is an error.
pub fn apply(steps: &[Step], todo: &[TodoLine]) -> Result<Vec<u8>, String> {
    if steps.len() != todo.len() {
        return Err(format!(
            "The todo list has {} commits, the plan {}",
            todo.len(),
            steps.len()
        ));
    }
    let mut out = Vec::new();
    for (step, line) in steps.iter().zip(todo) {
        let full = step.commit.as_str();
        if line.oid.len() < 4 || !full.starts_with(line.oid.as_str()) {
            return Err(format!(
                "The todo list has {} where the plan has {full}",
                line.oid
            ));
        }
        out.extend_from_slice(format!("{} {full}", step.action.word()).as_bytes());
        if !line.rest.is_empty() {
            out.push(b' ');
            out.extend_from_slice(&line.rest);
        }
        out.push(b'\n');
    }
    Ok(out)
}

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
            },
            Step {
                action: TodoAction::Pick,
                commit: id("a"),
            },
        ],
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
        }
    }

    /// `HEAD` = `a`, then `b`, `c`, `d` on a line, with `d` on `e`. `e` is
    /// not loaded.
    fn linear() -> Vec<CommitRow> {
        let ids = ['a', 'b', 'c', 'd', 'e'].map(id);
        (0..4).map(|i| row(&ids[i], &[&ids[i + 1]])).collect()
    }

    fn step(action: TodoAction, digit: char) -> Step {
        Step {
            action,
            commit: CommitId::parse(&id(digit)).unwrap(),
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
                Ok(RebasePlan {
                    base: cid('c'),
                    steps: vec![step(action, 'b'), step(TodoAction::Pick, 'a')],
                }),
                "{edit:?}"
            );
        }
        assert_eq!(
            plan(&linear(), 0, Edit::Drop),
            Ok(RebasePlan {
                base: cid('b'),
                steps: vec![step(TodoAction::Drop, 'a')],
            })
        );
    }

    #[test]
    fn squash_and_fixup_fold_the_commit_into_the_one_below() {
        for (edit, action) in [
            (Edit::Squash, TodoAction::Squash),
            (Edit::Fixup, TodoAction::Fixup),
        ] {
            assert_eq!(
                plan(&linear(), 1, edit),
                Ok(RebasePlan {
                    base: cid('d'),
                    steps: vec![
                        step(TodoAction::Pick, 'c'),
                        step(action, 'b'),
                        step(TodoAction::Pick, 'a'),
                    ],
                }),
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
        let all = [
            TodoAction::Pick,
            TodoAction::Squash,
            TodoAction::Fixup,
            TodoAction::Drop,
            TodoAction::Reword,
        ]
        .map(|action| step(action, 'a'));
        assert_eq!(decode(&encode(&all)), Ok(all.to_vec()));
    }

    #[test]
    fn decode_refuses_what_encode_would_not_write() {
        let a = id('a');
        for bad in [
            String::new(),
            "\n".into(),
            format!("exec {a}\n"),
            format!("edit {a}\n"),
            format!("Pick {a}\n"),
            format!("pick {}\n", &a[..39]),
            format!("pick {}\n", "g".repeat(40)),
            format!("pick {a} subject\n"),
            format!("pick  {a}\n"),
            format!("pick {a}\r\n"),
            format!("pick {a}\n\n"),
            "pick\n".into(),
        ] {
            assert!(decode(&bad).is_err(), "{bad:?}");
        }
    }

    fn line(oid: &str, rest: &str) -> TodoLine {
        TodoLine {
            oid: oid.into(),
            rest: rest.as_bytes().to_vec(),
        }
    }

    #[test]
    fn parse_todo_keeps_picks_and_drops_comments() {
        let todo = b"pick 1234abc first one\r\n\
                     p 5678def\n\
                     \n\
                     # Rebase 1..2 onto 1\n\
                     ; a different comment char\n\
                     pick abcd  two  spaces \xff\n";
        assert_eq!(
            parse_todo(todo),
            Ok(vec![
                line("1234abc", "first one"),
                line("5678def", ""),
                TodoLine {
                    oid: "abcd".into(),
                    rest: b" two  spaces \xff".to_vec(),
                },
            ])
        );
        assert_eq!(parse_todo(b"# only comments\n\n"), Ok(vec![]));
    }

    #[test]
    fn parse_todo_refuses_anything_but_picks() {
        for bad in [
            &b"exec make\n"[..],
            b"x make\n",
            b"label onto\n",
            b"update-ref refs/heads/x\n",
            b"fixup 1234abc s\n",
            b"squash 1234abc s\n",
            b"pick 123 short\n",
            b"pick 123g567 s\n",
            b"pick\n",
            b"merge -C 1234abc x\n",
        ] {
            assert!(parse_todo(bad).is_err(), "{}", String::from_utf8_lossy(bad));
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
    fn apply_refuses_any_mismatch() {
        let steps = plan(&linear(), 1, Edit::Drop).unwrap().steps;
        let b = &id('b')[..7];
        let a = &id('a')[..7];
        for todo in [
            vec![line(b, "")],
            vec![line(b, ""), line(a, ""), line(a, "")],
            vec![line(a, ""), line(b, "")],
            vec![line(b, ""), line("aaab", "")],
            vec![line(b, ""), line("aaa", "")],
            vec![],
        ] {
            assert!(apply(&steps, &todo).is_err(), "{todo:?}");
        }
    }
}
