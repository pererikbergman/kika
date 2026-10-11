//! The todo list of a rebase that has stopped, shown above the log in the
//! Commits panel and edited in place. Pure: each edit returns a new list or
//! why it cannot be made, and the model writes it back through an effect.

use std::path::PathBuf;

use crate::git::rebase_plan::{
    Direction, Edit, NOTHING_TO_FOLD, TodoAction, TodoEntry, crossing, parse_todo,
};

/// Why a todo list is shown but not edited.
pub const NOT_EDITABLE: &str = "The todo list has commands Kika does not edit";

/// Why an edit was not saved: git or the user changed the list since it
/// was read. The panel reloads it.
pub const TODO_CHANGED: &str = "The todo list changed on disk; reloaded it, try again";

/// A todo list as read from git's dir, with the list of steps done so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoFile {
    /// `<git dir>/rebase-merge/git-rebase-todo`.
    pub path: PathBuf,
    pub todo: Vec<u8>,
    /// `rebase-merge/done`; `None` when it could not be read.
    pub done: Option<Vec<u8>>,
}

/// What a key asks of a commit, on a log row or a todo row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rewrite {
    Edit(Edit),
    Move(Direction),
    Break,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebaseTodo {
    pub path: PathBuf,
    /// The bytes read, which a write must still find on disk.
    pub original: Vec<u8>,
    pub entries: Vec<TodoEntry>,
    /// `Err` when the list holds commands Kika does not edit; its lines
    /// are then shown as they are.
    pub editable: Result<(), &'static str>,
    /// Git has done no step yet, so there is nothing to fold into.
    pub done_empty: bool,
}

impl RebaseTodo {
    pub fn load(file: TodoFile) -> Self {
        let (entries, editable) = match parse_todo(&file.todo) {
            Ok(entries) => (entries, Ok(())),
            Err(_) => (Vec::new(), Err(NOT_EDITABLE)),
        };
        let done_empty = file.done.as_deref().is_some_and(nothing_done);
        Self {
            path: file.path,
            original: file.todo,
            entries,
            editable,
            done_empty,
        }
    }

    /// The command lines as git wrote them, for a list that is not edited.
    pub fn raw_lines(&self) -> Vec<&[u8]> {
        command_lines(&self.original).collect()
    }

    /// How many rows the panel shows for it.
    pub fn len(&self) -> usize {
        match self.editable {
            Ok(()) => self.entries.len(),
            Err(_) => self.raw_lines().len(),
        }
    }

    /// The list after `rewrite` on row `i`, and the row the cursor moves
    /// to, or why it cannot be done.
    pub fn edit(&self, i: usize, rewrite: Rewrite) -> Result<(Vec<TodoEntry>, usize), String> {
        self.editable?;
        match rewrite {
            Rewrite::Move(direction) => move_entry(&self.entries, i, direction),
            Rewrite::Edit(edit) => {
                set_action(&self.entries, i, edit, self.done_empty).map(|entries| (entries, i))
            }
            Rewrite::Break => toggle_break(&self.entries, i),
        }
    }
}

/// Whether git's `done` list holds no step yet.
pub fn nothing_done(done: &[u8]) -> bool {
    command_lines(done).next().is_none()
}

/// Lines that are commands: not blank, not a comment.
fn command_lines(text: &[u8]) -> impl Iterator<Item = &[u8]> {
    text.split(|&b| b == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
        .filter(|line| line.first().is_some_and(u8::is_ascii_alphanumeric))
}

/// Swap row `i` with the next to run (`Up`, as the list reads like future
/// history) or the one before it (`Down`). A `break` moves freely; nothing
/// crosses an `update-ref`, which would change what its branch holds.
pub fn move_entry(
    entries: &[TodoEntry],
    i: usize,
    direction: Direction,
) -> Result<(Vec<TodoEntry>, usize), String> {
    let entry = entries.get(i).ok_or("No todo row")?;
    let j = match direction {
        Direction::Up if i + 1 >= entries.len() => return Err("Already the last to run".into()),
        Direction::Up => i + 1,
        Direction::Down if i == 0 => return Err("Already the next to run".into()),
        Direction::Down => i - 1,
    };
    match (entry, &entries[j]) {
        (TodoEntry::Commit { oid, .. }, TodoEntry::UpdateRef(branch)) => {
            return Err(crossing(oid, branch));
        }
        (TodoEntry::UpdateRef(_), _) | (_, TodoEntry::UpdateRef(_)) => {
            return Err(UPDATES_STAY.into());
        }
        _ => {}
    }
    let mut out = entries.to_vec();
    out.swap(i, j);
    Ok((out, j))
}

/// Why a branch update or a stop does not move past one.
const UPDATES_STAY: &str = "Branch updates stay where git put them";

/// Give commit `i` the action for `edit`. The first commit cannot fold
/// while git has done nothing yet.
pub fn set_action(
    entries: &[TodoEntry],
    i: usize,
    edit: Edit,
    done_empty: bool,
) -> Result<Vec<TodoEntry>, String> {
    let Some(TodoEntry::Commit { action, .. }) = entries.get(i) else {
        return Err("Only a commit has an action".into());
    };
    let new = TodoAction::from(edit);
    if *action == new {
        return Err(already(new).into());
    }
    let first = entries
        .iter()
        .position(|entry| matches!(entry, TodoEntry::Commit { .. }));
    if done_empty && first == Some(i) && matches!(new, TodoAction::Squash | TodoAction::Fixup) {
        return Err(NOTHING_TO_FOLD.into());
    }
    let mut out = entries.to_vec();
    if let TodoEntry::Commit { action, .. } = &mut out[i] {
        *action = new;
    }
    Ok(out)
}

fn already(action: TodoAction) -> &'static str {
    match action {
        TodoAction::Pick => "Already a pick",
        TodoAction::Squash => "Already a squash",
        TodoAction::Fixup => "Already a fixup",
        TodoAction::Drop => "Already dropped",
        TodoAction::Reword => "Already a reword",
        TodoAction::Edit => "Already stops there",
    }
}

/// Add a `break` right after commit `i`, or take away the one there. On a
/// `break` row, take that one away. The cursor stays on the commit.
pub fn toggle_break(entries: &[TodoEntry], i: usize) -> Result<(Vec<TodoEntry>, usize), String> {
    let mut out = entries.to_vec();
    match entries.get(i) {
        Some(TodoEntry::Break) => {
            out.remove(i);
            Ok((out, i.saturating_sub(1)))
        }
        Some(TodoEntry::Commit { .. }) => {
            if entries.get(i + 1) == Some(&TodoEntry::Break) {
                out.remove(i + 1);
            } else {
                out.insert(i + 1, TodoEntry::Break);
            }
            Ok((out, i))
        }
        _ => Err("Only a commit can have a break after it".into()),
    }
}

/// The list as git reads it: one LF line per entry. Git's help comments
/// are not kept; it does not need them once the rebase has started.
pub fn serialize(entries: &[TodoEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    for entry in entries {
        match entry {
            TodoEntry::Commit { action, oid, rest } => {
                out.extend_from_slice(action.word().as_bytes());
                out.push(b' ');
                out.extend_from_slice(oid.as_bytes());
                if !rest.is_empty() {
                    out.push(b' ');
                    out.extend_from_slice(rest);
                }
            }
            TodoEntry::Break => out.extend_from_slice(b"break"),
            TodoEntry::UpdateRef(name) => {
                out.extend_from_slice(b"update-ref ");
                out.extend_from_slice(name);
            }
        }
        out.push(b'\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pick(oid: &str) -> TodoEntry {
        TodoEntry::Commit {
            action: TodoAction::Pick,
            oid: oid.into(),
            rest: format!("subject {oid}").into_bytes(),
        }
    }

    fn with(action: TodoAction, oid: &str) -> TodoEntry {
        match pick(oid) {
            TodoEntry::Commit { oid, rest, .. } => TodoEntry::Commit { action, oid, rest },
            other => other,
        }
    }

    fn branch(name: &str) -> TodoEntry {
        TodoEntry::UpdateRef(format!("refs/heads/{name}").into_bytes())
    }

    fn file(todo: &[u8], done: Option<&[u8]>) -> TodoFile {
        TodoFile {
            path: PathBuf::from("/r/.git/rebase-merge/git-rebase-todo"),
            todo: todo.to_vec(),
            done: done.map(<[u8]>::to_vec),
        }
    }

    #[test]
    fn a_todo_with_commands_kika_does_not_edit_is_shown_read_only() {
        let todo = RebaseTodo::load(file(b"pick 1234 a\nexec make\n# help\n", Some(b"")));
        assert_eq!(todo.editable, Err(NOT_EDITABLE));
        assert_eq!(todo.raw_lines(), [&b"pick 1234 a"[..], b"exec make"]);
        assert_eq!(todo.len(), 2);
        assert_eq!(
            todo.edit(0, Rewrite::Edit(Edit::Drop)),
            Err(NOT_EDITABLE.into())
        );
    }

    #[test]
    fn done_is_empty_only_when_it_was_read_and_has_no_commands() {
        let load = |done| RebaseTodo::load(file(b"pick 1234 a\n", done)).done_empty;
        assert!(load(Some(&b""[..])));
        assert!(load(Some(&b"# nothing\n"[..])));
        assert!(!load(Some(&b"pick 9999 x\n"[..])));
        assert!(!load(None));
    }

    #[test]
    fn moves_swap_neighbours_and_breaks_but_not_branch_updates() {
        let entries = [
            pick("aaaa"),
            TodoEntry::Break,
            pick("bbbb"),
            branch("x"),
            pick("cccc"),
        ];
        assert_eq!(
            move_entry(&entries, 0, Direction::Up),
            Ok((
                vec![
                    TodoEntry::Break,
                    pick("aaaa"),
                    pick("bbbb"),
                    branch("x"),
                    pick("cccc")
                ],
                1
            ))
        );
        assert_eq!(
            move_entry(&entries, 2, Direction::Down).map(|(_, at)| at),
            Ok(1)
        );
        assert_eq!(
            move_entry(&entries, 2, Direction::Up),
            Err("Moving bbbb past branch x would change what it holds".into())
        );
        assert_eq!(
            move_entry(&entries, 4, Direction::Down),
            Err("Moving cccc past branch x would change what it holds".into())
        );
        assert!(move_entry(&entries, 3, Direction::Up).is_err());
        assert!(move_entry(&entries, 0, Direction::Down).is_err());
        assert!(move_entry(&entries, 4, Direction::Up).is_err());
        assert!(move_entry(&entries, 9, Direction::Up).is_err());
    }

    #[test]
    fn set_action_changes_only_a_commit_s_word() {
        let entries = [pick("aaaa"), TodoEntry::Break, pick("bbbb")];
        assert_eq!(
            set_action(&entries, 2, Edit::Squash, true),
            Ok(vec![
                pick("aaaa"),
                TodoEntry::Break,
                with(TodoAction::Squash, "bbbb")
            ])
        );
        assert_eq!(
            set_action(&entries, 0, Edit::Pick, false),
            Err("Already a pick".into())
        );
        assert!(set_action(&entries, 1, Edit::Drop, false).is_err());
        assert_eq!(
            set_action(&entries, 0, Edit::Fixup, true),
            Err(NOTHING_TO_FOLD.into())
        );
        assert!(set_action(&entries, 0, Edit::Fixup, false).is_ok());
    }

    #[test]
    fn toggle_break_adds_and_removes_the_break_after_a_commit() {
        let entries = [pick("aaaa"), pick("bbbb")];
        let (added, at) = toggle_break(&entries, 0).unwrap();
        assert_eq!(added, [pick("aaaa"), TodoEntry::Break, pick("bbbb")]);
        assert_eq!(at, 0);
        assert_eq!(toggle_break(&added, 0), Ok((entries.to_vec(), 0)));
        assert_eq!(toggle_break(&added, 1), Ok((entries.to_vec(), 0)));
        assert!(toggle_break(&[branch("x")], 0).is_err());
    }

    #[test]
    fn serialize_writes_one_line_per_entry_and_round_trips() {
        let entries = vec![
            with(TodoAction::Edit, "aaaa"),
            TodoEntry::Break,
            branch("x"),
            TodoEntry::Commit {
                action: TodoAction::Drop,
                oid: "bbbb".into(),
                rest: Vec::new(),
            },
        ];
        let text = serialize(&entries);
        assert_eq!(
            text,
            b"edit aaaa subject aaaa\nbreak\nupdate-ref refs/heads/x\ndrop bbbb\n"
        );
        assert_eq!(parse_todo(&text), Ok(entries));
    }
}
