//! The todo list of a rebase that has stopped, shown above the log in the
//! Commits panel and edited in place. Pure: each edit returns a new list or
//! why it cannot be made, and the model writes it back through an effect.

use std::path::PathBuf;

use crate::git::diff::terminal_safe;
use crate::git::rebase_plan::{
    Direction, Edit, NOTHING_TO_FOLD, TodoAction, TodoEntry, Unreadable, command_lines, crossing,
    parse_todo, unreadable,
};

/// Why a todo list is shown but not edited, when no single line says it.
pub const NOT_EDITABLE: &str = "The todo list has commands Kika does not edit";

/// How every reason a list is not edited starts: what it is and how to
/// fix it first, so a narrow bar still shows them.
const READ_ONLY: &str = "Read-only, fix with git rebase --edit-todo:";

/// How a reason starts when the git config, not the list, keeps it from
/// being edited.
const READ_ONLY_CONFIG: &str = "Read-only, fix core.commentChar:";

/// The most characters of a todo line a reason quotes.
const QUOTED: usize = 20;

/// The most characters of a config error a reason shows, so the whole
/// reason fits in 80 columns.
const CONFIG_WHY: usize = 40;

/// Why `todo` is shown but not edited, after how to fix it: the comment
/// prefix git would use could not be read or could hide a command, or the
/// line that stops it, by number, as the user can find it in the file.
/// `comment` is the prefix read through `git config`.
pub fn not_editable(todo: &[u8], comment: &Result<Vec<u8>, String>) -> String {
    let prefix = match comment {
        Ok(prefix) if hides_commands(prefix) => {
            return format!(
                "{READ_ONLY_CONFIG} prefix {} hides commands",
                quoted(prefix)
            );
        }
        Ok(prefix) => prefix.as_slice(),
        Err(why) => {
            let why = cut(why.replace('\n', " "), CONFIG_WHY);
            return format!("{READ_ONLY_CONFIG} {why}");
        }
    };
    match unreadable(todo, prefix) {
        Some(Unreadable::Nul) => format!("{READ_ONLY} a NUL byte ends what git reads"),
        Some(Unreadable::Line { number, text }) => {
            // Every command starts with a letter; a line led by another
            // mark is most likely a comment written with another prefix
            // than the one git will use, which git would refuse.
            let cause = if text.first().is_some_and(u8::is_ascii_punctuation) {
                format!("comment not using {}", quoted(prefix))
            } else {
                "unknown command".into()
            };
            format!("{READ_ONLY} line {number}, {cause} \"{}\"", quoted(text))
        }
        None => format!("{READ_ONLY} {NOT_EDITABLE}"),
    }
}

/// Whether lines git reads as commands could start with `prefix`: every
/// todo command starts with a letter, so a letter-led prefix (`b` hides
/// `break`, `p` hides `pick`) could turn a line Kika writes into a comment.
/// Such a list is never edited.
fn hides_commands(prefix: &[u8]) -> bool {
    prefix.first().is_some_and(u8::is_ascii_alphabetic)
}

/// `text`, safe to print and cut to [`QUOTED`] characters.
fn quoted(text: &[u8]) -> String {
    cut(terminal_safe(text), QUOTED)
}

/// `safe` cut to `max` characters, the last one an ellipsis when cut.
fn cut(safe: String, max: usize) -> String {
    if safe.chars().count() <= max {
        return safe;
    }
    let mut cut: String = safe.chars().take(max - 1).collect();
    cut.push('…');
    cut
}

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
    /// The prefix git skips lines by, or why it could not be read.
    pub comment: Result<Vec<u8>, String>,
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
    /// `Err` when the list holds a line Kika does not edit, saying which
    /// and what to do instead; its lines are then shown as they are.
    pub editable: Result<(), String>,
    /// Git has done no step yet, so there is nothing to fold into.
    pub done_empty: bool,
    /// The comment prefix the list was read with, which a write checks
    /// it with again. Empty when it could not be read: no line is then a
    /// comment, and the list is not edited.
    pub comment: Vec<u8>,
}

impl RebaseTodo {
    pub fn load(file: TodoFile) -> Self {
        let comment = file.comment.clone().unwrap_or_default();
        let parsed = match &file.comment {
            Ok(prefix) if !hides_commands(prefix) => parse_todo(&file.todo, &comment).ok(),
            _ => None,
        };
        let (entries, editable) = match parsed {
            Some(entries) => (entries, Ok(())),
            None => (Vec::new(), Err(not_editable(&file.todo, &file.comment))),
        };
        let done_empty = file
            .done
            .as_deref()
            .is_some_and(|done| nothing_done(done, &comment));
        Self {
            path: file.path,
            original: file.todo,
            entries,
            editable,
            done_empty,
            comment,
        }
    }

    /// The command lines as git wrote them, for a list that is not edited.
    pub fn raw_lines(&self) -> Vec<&[u8]> {
        command_lines(&self.original, &self.comment).collect()
    }

    /// How many rows the panel shows for it.
    pub fn len(&self) -> usize {
        if self.editable.is_ok() {
            self.entries.len()
        } else {
            self.raw_lines().len()
        }
    }

    /// The list after `rewrite` on row `i`, and the row the cursor moves
    /// to, or why it cannot be done.
    pub fn edit(&self, i: usize, rewrite: Rewrite) -> Result<(Vec<TodoEntry>, usize), String> {
        self.editable.clone()?;
        match rewrite {
            Rewrite::Move(direction) => move_entry(&self.entries, i, direction),
            Rewrite::Edit(edit) => {
                set_action(&self.entries, i, edit, self.done_empty).map(|entries| (entries, i))
            }
            Rewrite::Break => toggle_break(&self.entries, i),
        }
    }
}

/// Whether git's `done` list holds no step yet, lines led by `comment`
/// skipped.
pub fn nothing_done(done: &[u8], comment: &[u8]) -> bool {
    command_lines(done, comment).next().is_none()
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
            comment: Ok(b"#".to_vec()),
        }
    }

    #[test]
    fn a_todo_with_commands_kika_does_not_edit_is_shown_read_only() {
        let todo = RebaseTodo::load(file(b"pick 1234 a\nexec make\n# help\n", Some(b"")));
        let why =
            "Read-only, fix with git rebase --edit-todo: line 2, unknown command \"exec make\"";
        assert_eq!(todo.editable, Err(why.to_string()));
        assert_eq!(todo.raw_lines(), [&b"pick 1234 a"[..], b"exec make"]);
        assert_eq!(todo.len(), 2);
        assert_eq!(todo.edit(0, Rewrite::Edit(Edit::Drop)), Err(why.into()));
    }

    #[test]
    fn a_comment_with_another_comment_char_is_named_as_such() {
        let todo = RebaseTodo::load(file(b"; Rebase 1..2 onto 1\npick 1234 a\n", None));
        assert_eq!(
            todo.editable,
            Err(
                "Read-only, fix with git rebase --edit-todo: line 1, comment not using # \
                 \"; Rebase 1..2 onto 1\""
                    .to_string()
            )
        );
    }

    fn file_with(todo: &[u8], comment: Result<&[u8], &str>) -> TodoFile {
        TodoFile {
            comment: comment.map(<[u8]>::to_vec).map_err(str::to_string),
            ..file(todo, Some(b""))
        }
    }

    #[test]
    fn comments_led_by_the_configured_prefix_keep_the_list_editable() {
        let text = b"; Rebase 1..2 onto 1\npick 1234 a\n";
        let todo = RebaseTodo::load(file_with(text, Ok(b";")));
        assert_eq!(todo.editable, Ok(()));
        assert_eq!(todo.entries.len(), 1);
        assert_eq!(todo.comment, b";");
        assert_eq!(todo.raw_lines(), [&b"pick 1234 a"[..]]);
        let hashed = RebaseTodo::load(file_with(text, Ok(b"#")));
        assert_eq!(
            hashed.editable,
            Err(
                "Read-only, fix with git rebase --edit-todo: line 1, comment not using # \
                 \"; Rebase 1..2 onto 1\""
                    .to_string()
            )
        );
        let todo = RebaseTodo::load(TodoFile {
            done: Some(b"; nothing yet\n".to_vec()),
            ..file_with(b"pick 1234 a\n", Ok(b";"))
        });
        assert!(todo.done_empty);
    }

    #[test]
    fn a_line_not_led_by_the_prefix_still_makes_the_list_read_only() {
        let todo = RebaseTodo::load(file_with(b"pick 1234 a\n# help\n", Ok(b";")));
        assert_eq!(
            todo.editable,
            Err(
                "Read-only, fix with git rebase --edit-todo: line 2, comment not using ; \
                 \"# help\""
                    .to_string()
            )
        );
        let todo = RebaseTodo::load(file_with(b"pick 1234 a\nexec make\n", Ok(b";")));
        assert!(
            todo.editable
                .unwrap_err()
                .ends_with("line 2, unknown command \"exec make\"")
        );
        let todo = RebaseTodo::load(file_with(b"# x\n", Ok(b"\x1b[2J")));
        let why = todo.editable.unwrap_err();
        assert!(why.contains("comment not using \u{FFFD}[2J"), "{why}");
    }

    #[test]
    fn a_prefix_that_could_not_be_read_hides_nothing_and_names_the_error() {
        let why = "fatal: bad config line 1\nin .git/config";
        for text in [
            &b"# Rebase 1..2 onto 1\npick 1234 a\n"[..],
            b"pick 1234 a\n",
        ] {
            let todo = RebaseTodo::load(file_with(text, Err(why)));
            assert_eq!(
                todo.editable,
                Err("Read-only, fix core.commentChar: \
                     fatal: bad config line 1 in .git/config"
                    .to_string())
            );
            assert!(todo.entries.is_empty());
            assert!(todo.comment.is_empty());
            assert_eq!(
                todo.edit(0, Rewrite::Edit(Edit::Drop)),
                Err(todo.editable.clone().unwrap_err())
            );
        }
        let todo = RebaseTodo::load(file_with(b"# c\npick 1234 a\n", Err(why)));
        assert_eq!(todo.raw_lines(), [&b"# c"[..], b"pick 1234 a"]);
        let long = "e".repeat(200);
        let todo = RebaseTodo::load(file_with(b"", Err(&long)));
        let shown = todo.editable.unwrap_err();
        let cause = shown.split("core.commentChar: ").nth(1).unwrap();
        assert_eq!(cause.chars().count(), CONFIG_WHY, "{shown}");
        assert!(cause.ends_with('…'), "{shown}");
    }

    #[test]
    fn a_prefix_that_could_hide_a_command_keeps_the_list_read_only() {
        for (prefix, text) in [
            (&b"b"[..], &b"b Rebase 1..2 onto 1\npick 1111 one\n"[..]),
            (b"p", b"pick 1111 one\nbreak\n"),
            (b"break", b"pick 1111 one\n"),
            (b"X", b"pick 1111 one\n"),
        ] {
            let todo = RebaseTodo::load(file_with(text, Ok(prefix)));
            let why = format!(
                "Read-only, fix core.commentChar: prefix {} hides commands",
                String::from_utf8_lossy(prefix)
            );
            assert_eq!(todo.editable, Err(why.clone()), "{prefix:?}");
            assert!(todo.entries.is_empty());
            assert_eq!(todo.edit(0, Rewrite::Break), Err(why));
        }
    }

    #[test]
    fn a_quoted_line_is_made_safe_and_cut_short() {
        let mut line = b"exec \x1b[31m".to_vec();
        line.extend_from_slice("é".repeat(60).as_bytes());
        let why = not_editable(&line, &Ok(b"#".to_vec()));
        assert!(!why.contains('\x1b'), "{why}");
        let quote = why.split('"').nth(1).unwrap();
        assert_eq!(quote.chars().count(), QUOTED);
        assert!(quote.ends_with('…'), "{quote}");
    }

    #[test]
    fn an_indented_pick_is_kept_and_an_indented_exec_makes_the_list_read_only() {
        let todo = RebaseTodo::load(file(b"pick 1111 one\n  pick 2222 two\n", Some(b"")));
        assert_eq!(todo.editable, Ok(()));
        assert_eq!(todo.len(), 2);
        assert_eq!(serialize(&todo.entries), b"pick 1111 one\npick 2222 two\n");

        let todo = RebaseTodo::load(file(b"pick 1111 one\n\texec touch x\n", Some(b"")));
        assert!(todo.editable.as_ref().unwrap_err().contains(": line 2, "));
        assert_eq!(todo.raw_lines(), [&b"pick 1111 one"[..], b"exec touch x"]);
        assert!(!nothing_done(b"  pick 1111 one\n", b"#"));
    }

    #[test]
    fn a_todo_with_a_nul_is_read_only_and_shows_only_what_git_reads() {
        let todo = RebaseTodo::load(file(
            b"pick 1111 one\npick 2222 two\0x\npick 3333 c\n",
            Some(b""),
        ));
        assert!(
            todo.editable
                .unwrap_err()
                .ends_with("a NUL byte ends what git reads")
        );
        let todo = RebaseTodo::load(file(b"pick 1111 one\0\n", None));
        assert_eq!(todo.raw_lines(), [&b"pick 1111 one"[..]]);
        assert!(nothing_done(b"\0pick 1111 one\n", b"#"));
    }

    #[test]
    fn a_line_git_rejects_makes_the_list_read_only_and_stays_in_view() {
        for odd in [
            &b"\xef\xbb\xbfpick 2222 two"[..],
            b"-pick 2222 two",
            b"\x0bpick 2222 two",
        ] {
            let mut bytes = b"pick 1111 one\n".to_vec();
            bytes.extend_from_slice(odd);
            bytes.push(b'\n');
            let todo = RebaseTodo::load(file(&bytes, Some(b"")));
            let why = todo.editable.clone().unwrap_err();
            assert!(why.contains(": line 2, "), "{why}");
            assert_eq!(todo.raw_lines(), [&b"pick 1111 one"[..], odd], "{odd:?}");
        }
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
        assert_eq!(parse_todo(&text, b"#"), Ok(entries));
    }
}
