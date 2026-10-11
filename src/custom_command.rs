//! The user's own programs, bound to keys in the global config file. Pure:
//! the templates are checked once when the file loads, and expanded into an
//! argv when a key is pressed. Each template element is exactly one
//! argument, so a file name or a typed answer can never become two, an
//! option, a glob or shell code.

use std::ffi::OsString;

use serde::Deserialize;

use crate::git::path::RepoPath;
use crate::keymap::{KeyPattern, KeyScope, parse_key};

/// One `[[commands]]` entry as written in the global file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandTable {
    pub context: Panel,
    pub keys: Vec<String>,
    pub description: String,
    pub command: Vec<String>,
    #[serde(default)]
    pub prompts: Vec<String>,
    #[serde(default)]
    pub output: OutputMode,
}

/// The panel a command's keys work in, which decides what it may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Panel {
    Files,
    Diff,
    Blame,
}

impl Panel {
    fn scope(self) -> KeyScope {
        match self {
            Panel::Files => KeyScope::Files,
            Panel::Diff => KeyScope::Diff,
            Panel::Blame => KeyScope::Blame,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Panel::Files => "files",
            Panel::Diff => "diff",
            Panel::Blame => "blame",
        }
    }

    /// Only what the panel can say about its selection.
    fn allows(self, part: &Part) -> bool {
        match part {
            Part::Text(_) | Part::Prompt(_) | Part::Path => true,
            Part::Line => matches!(self, Panel::Diff | Panel::Blame),
            Part::Commit => self == Panel::Blame,
        }
    }
}

/// Where the program's output goes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputMode {
    /// The program gets the terminal, for interactive tools.
    #[default]
    Terminal,
    /// Output is captured and shown in a popup.
    Popup,
    /// Output is captured and dropped; only a failure is shown.
    None,
}

/// A piece of one template element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    Text(String),
    Path,
    Line,
    Commit,
    /// The answer to prompt `n`, counted from 1.
    Prompt(usize),
}

/// A checked command, ready to expand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomCommand {
    pub scope: KeyScope,
    pub keys: Vec<KeyPattern>,
    pub description: String,
    /// One entry per argument; `argv[0]` is plain text.
    pub argv: Vec<Vec<Part>>,
    pub prompts: Vec<String>,
    pub output: OutputMode,
}

/// The selection a command runs on, captured when its key is pressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub path: RepoPath,
    pub line: Option<u32>,
    /// The full oid.
    pub commit: Option<String>,
}

/// Programs that read their first operand as shell code.
const SHELLS: &[&str] = &[
    "sh", "bash", "rbash", "zsh", "dash", "ash", "ksh", "ksh93", "mksh", "lksh", "oksh", "pdksh",
    "yash", "posh", "fish", "csh", "tcsh",
];

impl CustomCommand {
    /// Check `table`, the `n`th entry counted from 1. Every error starts
    /// with `command <n>`, so the user can find it.
    pub fn parse(table: &CommandTable, n: usize) -> Result<Self, String> {
        let fail = |message: String| format!("command {n}: {message}");
        let keys = table
            .keys
            .iter()
            .map(|text| parse_key(text).map_err(&fail))
            .collect::<Result<Vec<_>, _>>()?;
        if keys.is_empty() {
            return Err(fail("needs at least one key".into()));
        }
        if table.description.trim().is_empty() {
            return Err(fail("needs a description".into()));
        }
        let argv = table
            .command
            .iter()
            .map(|element| parse_element(element).map_err(&fail))
            .collect::<Result<Vec<_>, _>>()?;
        let program = match argv.first() {
            None => return Err(fail("`command` is empty".into())),
            Some(parts) => parts,
        };
        if program.iter().any(|p| !matches!(p, Part::Text(_))) {
            return Err(fail(
                "command[0], the program, must be plain text, not a placeholder".into(),
            ));
        }
        if table.command[0].is_empty() {
            return Err(fail("command[0], the program, is empty".into()));
        }
        for (element, parts) in table.command.iter().zip(&argv) {
            for part in parts {
                if let Part::Prompt(k) = part
                    && *k > table.prompts.len()
                {
                    return Err(fail(format!(
                        "\"{{prompt{k}}}\" in \"{element}\" but there are only {} prompts",
                        table.prompts.len()
                    )));
                }
                if !table.context.allows(part) {
                    return Err(fail(format!(
                        "\"{}\" in \"{element}\" is not available in {} commands",
                        placeholder(part),
                        table.context.name()
                    )));
                }
            }
        }
        check_shell(&table.command, &argv).map_err(fail)?;
        Ok(Self {
            scope: table.context.scope(),
            keys,
            description: table.description.clone(),
            argv,
            prompts: table.prompts.clone(),
            output: table.output,
        })
    }

    /// The argv to run on `target` with the prompts' `answers`. A value is
    /// never split, globbed or given to a shell: each template element is
    /// one argument. Fails when the selection lacks a value the template uses.
    pub fn expand(
        &self,
        target: &Target,
        answers: &[String],
    ) -> Result<Vec<OsString>, &'static str> {
        self.argv
            .iter()
            .map(|parts| {
                let mut arg = OsString::new();
                for part in parts {
                    match part {
                        Part::Text(text) => arg.push(text),
                        Part::Path => {
                            let raw = target.path.as_os_str();
                            // Only at the start of an argument can it read as an option,
                            // which is also where an empty answer before it leaves it.
                            if arg.is_empty() && raw.as_encoded_bytes().first() == Some(&b'-') {
                                arg.push("./");
                            }
                            arg.push(raw);
                        }
                        Part::Line => arg.push(target.line.ok_or("No line here")?.to_string()),
                        Part::Commit => {
                            arg.push(target.commit.as_deref().ok_or("Not committed yet")?)
                        }
                        Part::Prompt(k) => {
                            arg.push(answers.get(k - 1).ok_or("A prompt is not answered")?)
                        }
                    }
                }
                Ok(arg)
            })
            .collect()
    }
}

/// `{path}` and the like as the user wrote it, for messages.
fn placeholder(part: &Part) -> String {
    match part {
        Part::Text(text) => text.clone(),
        Part::Path => "{path}".into(),
        Part::Line => "{line}".into(),
        Part::Commit => "{commit}".into(),
        Part::Prompt(k) => format!("{{prompt{k}}}"),
    }
}

/// Split one template element into text and placeholders. `{{` and `}}`
/// are literal braces; any other brace must belong to a known placeholder.
fn parse_element(element: &str) -> Result<Vec<Part>, String> {
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut chars = element.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                text.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                text.push('}');
            }
            '}' => {
                return Err(format!(
                    "a lone \"}}\" in \"{element}\"; write }}}} for a literal brace"
                ));
            }
            '{' => {
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => name.push(c),
                        None => {
                            return Err(format!(
                                "unclosed \"{{\" in \"{element}\"; write {{{{ for a literal brace"
                            ));
                        }
                    }
                }
                let part = placeholder_named(&name).ok_or_else(|| {
                    format!(
                        "unknown placeholder \"{{{name}}}\" in \"{element}\"; use {{path}}, \
                         {{line}}, {{commit}} or {{promptN}}"
                    )
                })?;
                if !text.is_empty() {
                    parts.push(Part::Text(std::mem::take(&mut text)));
                }
                parts.push(part);
            }
            c => text.push(c),
        }
    }
    if !text.is_empty() || parts.is_empty() {
        parts.push(Part::Text(text));
    }
    Ok(parts)
}

fn placeholder_named(name: &str) -> Option<Part> {
    match name {
        "path" => Some(Part::Path),
        "line" => Some(Part::Line),
        "commit" => Some(Part::Commit),
        _ => {
            let digits = name.strip_prefix("prompt")?;
            let k: usize = digits.parse().ok()?;
            // Written the one way, so `{prompt01}` or `{prompt+1}` is a typo.
            (k >= 1 && k.to_string() == digits).then_some(Part::Prompt(k))
        }
    }
}

/// A shell reads its script, and the options before it, as code. A value
/// placed there would run as code too, so it must come after the script,
/// where the script reads it as `"$1"`. A shell started through one of
/// `LAUNCHERS` is checked the same way.
fn check_shell(command: &[String], argv: &[Vec<Part>]) -> Result<(), String> {
    if is_shell(&command[0]) {
        return check_shell_at(command, argv);
    }
    if !is_launcher(&command[0]) {
        return Ok(());
    }
    if is_env(&command[0]) && env_splits_a_shell(command) {
        return match first_value(&argv[1..]) {
            Some(part) => Err(format!(
                "\"{}\" is in a command where env -S starts a shell; Kika cannot tell \
                 where the script ends, so pass no placeholders",
                placeholder(part)
            )),
            None => Ok(()),
        };
    }
    // Options are not parsed: the first shell name among the arguments is
    // taken as the shell. This can refuse a safe command, never accept one
    // the shell would run a value of as code.
    let shell = (1..command.len())
        .find(|&k| argv[k].iter().all(|p| matches!(p, Part::Text(_))) && is_shell(&command[k]));
    let Some(k) = shell else {
        return Ok(());
    };
    if let Some(part) = first_value(&argv[1..k]) {
        return Err(format!(
            "\"{}\" is before the shell that {} starts, where it could become the program \
             or its options; pass it after the script and read it as \"$1\"",
            placeholder(part),
            command[0]
        ));
    }
    check_shell_at(&command[k..], &argv[k..])
}

/// The first placeholder in `argv`, in order.
fn first_value(argv: &[Vec<Part>]) -> Option<&Part> {
    argv.iter().flatten().find(|p| !matches!(p, Part::Text(_)))
}

/// The check for `command[0]`, a shell.
///
/// Conservative, since options may come in any order around `-c`, some
/// take a value (`-o pipefail`, `-co pipefail`, `+O extglob`, `--rcfile f`)
/// and none takes more than one: the last option before the first
/// placeholder is assumed to take a value, and the element after that is
/// taken as the script. With no option, the first operand is the script.
/// The placeholder must come after it. This can refuse a safe command,
/// never accept one that runs a value as code.
/// The slot right after the script stays closed for csh/tcsh too, though
/// their values start at `$1`: they read options there (`-c`, `-n`).
fn check_shell_at(command: &[String], argv: &[Vec<Part>]) -> Result<(), String> {
    let Some((first, part)) = argv.iter().enumerate().skip(1).find_map(|(i, parts)| {
        let value = parts.iter().find(|p| !matches!(p, Part::Text(_)));
        value.map(|part| (i, part))
    }) else {
        return Ok(());
    };
    let is_option = |element: &String| element.starts_with(['-', '+']);
    let options_end = (1..first).rev().find(|&i| is_option(&command[i]));
    let script = options_end.map_or(1, |i| i + 2);
    if first <= script {
        return Err(format!(
            "\"{}\" is in the shell script or before it, where it would run as code; \
             pass it after the script and read it as \"$1\"",
            placeholder(part)
        ));
    }
    Ok(())
}

/// Whether env's `-S`/`--split-string` value names a shell. The value is
/// split into words, so the script's position cannot be known. env also
/// unquotes the words, reads `\_` as a space and expands `${VAR}`, so
/// quotes and backslashes are dropped before the lookup, and a variable
/// counts as a shell, since it could hold one.
fn env_splits_a_shell(command: &[String]) -> bool {
    let names_a_shell = |value: &str| {
        let words = value.replace("\\_", " ").replace(['\'', '"', '\\'], "");
        words
            .split_ascii_whitespace()
            .any(|word| word.contains('$') || is_shell(word))
    };
    let next = |i: usize| command.get(i + 1).map(String::as_str).unwrap_or_default();
    command.iter().enumerate().skip(1).any(|(i, element)| {
        if let Some(long) = element.strip_prefix("--") {
            // Long options may be abbreviated, down to `--s`.
            let (name, value) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (long, None),
            };
            !name.is_empty()
                && "split-string".starts_with(name)
                && names_a_shell(value.unwrap_or_else(|| next(i)))
        } else if let Some(group) = element.strip_prefix('-') {
            match group.split_once('S') {
                Some((_, "")) => names_a_shell(next(i)),
                Some((_, attached)) => names_a_shell(attached),
                None => false,
            }
        } else {
            false
        }
    })
}

/// Programs that start the program named among their arguments, which may
/// be a shell.
const LAUNCHERS: &[&str] = &[
    "env", "busybox", "toybox", "nice", "nohup", "timeout", "xargs", "exec", "command", "stdbuf",
    "setsid", "ionice", "chrt", "taskset", "time", "sudo", "doas",
];

/// The last component of `program`, lowercased, without the trailing dots
/// and spaces that Windows drops when it starts a program.
fn program_name(program: &str) -> String {
    last_component(program)
        .trim_end_matches(['.', ' '])
        .to_string()
}

/// The last component of `program`, lowercased, split on `/` and `\\`.
fn last_component(program: &str) -> String {
    let name = program.rsplit(['/', '\\']).next().unwrap_or_default();
    name.to_ascii_lowercase()
}

/// `program_name` without `.exe`. Launchers have no versioned names.
fn launcher_name(program: &str) -> String {
    let name = program_name(program);
    name.strip_suffix(".exe").unwrap_or(&name).to_string()
}

fn is_launcher(program: &str) -> bool {
    LAUNCHERS.contains(&launcher_name(program).as_str())
}

fn is_env(program: &str) -> bool {
    launcher_name(program) == "env"
}

/// Whether `program`, a path or a bare name, is one of `SHELLS`. A trailing
/// version (`bash5`, `bash-5.2`) counts, because distributions install
/// shells under such names. So does a trailing dot or space (`bash.`),
/// which Windows drops. The untrimmed name is checked too, so trimming
/// never turns a shell into a non-shell.
fn is_shell(program: &str) -> bool {
    is_shell_name(&last_component(program)) || is_shell_name(&program_name(program))
}

/// `is_shell` for a lowercased last component.
fn is_shell_name(name: &str) -> bool {
    let name = name.strip_suffix(".exe").unwrap_or(name);
    if SHELLS.contains(&name) {
        return true;
    }
    let base = name.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    if !name[base.len()..].contains(|c: char| c.is_ascii_digit()) {
        return false;
    }
    let base = base.strip_suffix('-').unwrap_or(base);
    SHELLS.contains(&base)
}

/// A `[[commands]]` entry, for tests elsewhere.
#[cfg(test)]
pub(crate) fn test_table(
    context: Panel,
    keys: &[&str],
    command: &[&str],
    prompts: &[&str],
    output: OutputMode,
) -> CommandTable {
    let strings = |items: &[&str]| items.iter().map(|s| s.to_string()).collect();
    CommandTable {
        context,
        keys: strings(keys),
        description: format!("Run {}", command.first().unwrap_or(&"")),
        command: strings(command),
        prompts: strings(prompts),
        output,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(context: Panel, command: &[&str], prompts: &[&str]) -> CommandTable {
        CommandTable {
            context,
            keys: vec!["ctrl-t".into()],
            description: "Run it".into(),
            command: command.iter().map(|s| s.to_string()).collect(),
            prompts: prompts.iter().map(|s| s.to_string()).collect(),
            output: OutputMode::None,
        }
    }

    fn parse(context: Panel, command: &[&str], prompts: &[&str]) -> Result<CustomCommand, String> {
        CustomCommand::parse(&table(context, command, prompts), 2)
    }

    fn error(context: Panel, command: &[&str], prompts: &[&str]) -> String {
        parse(context, command, prompts).unwrap_err()
    }

    fn target(path: RepoPath) -> Target {
        Target {
            path,
            line: None,
            commit: None,
        }
    }

    fn expand(
        command: &[&str],
        target: &Target,
        answers: &[&str],
    ) -> Result<Vec<OsString>, &'static str> {
        let prompts: Vec<&str> = answers.iter().map(|_| "Q").collect();
        let answers: Vec<String> = answers.iter().map(|s| s.to_string()).collect();
        parse(Panel::Blame, command, &prompts)
            .unwrap()
            .expand(target, &answers)
    }

    #[test]
    fn a_table_parses_into_parts_keys_and_scope() {
        let command = parse(
            Panel::Files,
            &["cp", "--", "{path}", "--to={prompt1}", "{{x}}"],
            &["Copy to"],
        )
        .unwrap();
        assert_eq!(command.scope, KeyScope::Files);
        assert_eq!(command.keys, [parse_key("ctrl-t").unwrap()]);
        assert_eq!(
            command.argv,
            [
                vec![Part::Text("cp".into())],
                vec![Part::Text("--".into())],
                vec![Part::Path],
                vec![Part::Text("--to=".into()), Part::Prompt(1)],
                vec![Part::Text("{x}".into())],
            ]
        );
        assert_eq!(command.prompts, ["Copy to"]);
        assert_eq!(command.output, OutputMode::None);
    }

    #[test]
    fn the_program_must_be_plain_non_empty_text() {
        assert_eq!(
            error(Panel::Files, &["{path}"], &[]),
            "command 2: command[0], the program, must be plain text, not a placeholder"
        );
        assert_eq!(
            error(Panel::Files, &["./{path}", "x"], &[]),
            "command 2: command[0], the program, must be plain text, not a placeholder"
        );
        assert_eq!(
            error(Panel::Files, &[], &[]),
            "command 2: `command` is empty"
        );
        assert_eq!(
            error(Panel::Files, &["", "x"], &[]),
            "command 2: command[0], the program, is empty"
        );
    }

    #[test]
    fn unknown_or_unclosed_placeholders_are_errors() {
        let unknown = error(Panel::Files, &["echo", "{file}"], &[]);
        assert!(
            unknown.starts_with(r#"command 2: unknown placeholder "{file}" in "{file}""#),
            "{unknown}"
        );
        for typo in ["{prompt0}", "{prompt01}", "{prompt}", "{Path}", "{ path}"] {
            let e = error(Panel::Files, &["echo", typo], &["Q"]);
            assert!(e.contains("unknown placeholder"), "{typo}: {e}");
        }
        let unclosed = error(Panel::Files, &["echo", "a{path"], &[]);
        assert!(
            unclosed.starts_with(r#"command 2: unclosed "{" in "a{path""#),
            "{unclosed}"
        );
        let lone = error(Panel::Files, &["echo", "a}b"], &[]);
        assert!(lone.starts_with(r#"command 2: a lone "}""#), "{lone}");
    }

    #[test]
    fn a_prompt_must_exist() {
        assert_eq!(
            error(Panel::Files, &["echo", "{prompt1}", "{prompt2}"], &["One"]),
            r#"command 2: "{prompt2}" in "{prompt2}" but there are only 1 prompts"#
        );
        assert!(
            parse(
                Panel::Files,
                &["echo", "{prompt1}", "{prompt2}"],
                &["a", "b"]
            )
            .is_ok()
        );
    }

    #[test]
    fn each_panel_allows_only_its_own_placeholders() {
        assert_eq!(
            error(Panel::Files, &["echo", "{line}"], &[]),
            r#"command 2: "{line}" in "{line}" is not available in files commands"#
        );
        assert_eq!(
            error(Panel::Files, &["echo", "{commit}"], &[]),
            r#"command 2: "{commit}" in "{commit}" is not available in files commands"#
        );
        assert_eq!(
            error(Panel::Diff, &["echo", "x{commit}"], &[]),
            r#"command 2: "{commit}" in "x{commit}" is not available in diff commands"#
        );
        assert!(parse(Panel::Diff, &["echo", "{path}:{line}"], &[]).is_ok());
        assert!(parse(Panel::Blame, &["echo", "{path}", "{line}", "{commit}"], &[]).is_ok());
    }

    #[test]
    fn a_placeholder_in_a_shell_script_or_before_it_is_an_error() {
        let rule = "pass it after the script and read it as \"$1\"";
        for command in [
            &["sh", "-c", "x {path}"][..],
            &["bash", "-lc", "x {path}"],
            &["/bin/zsh", "-c", "x {path}"],
            &["dash", "{path}"],
            &["ksh", "-c{path}", "x"],
            &["fish", "-c", "--", "{path}"],
            &["SH.exe", "-c", "x {path}"],
            // Options that take an argument, or start with `+`.
            &["bash", "-o", "pipefail", "-c", "grep x {path}"],
            &["bash", "-O", "extglob", "-c", "x {path}"],
            &["sh", "+x", "-c", "x {path}"],
            &["zsh", "-o", "errexit", "-c", "x {path}"],
            &["bash", "--rcfile", "f", "-c", "x {path}"],
            &["bash", "--rcfile", "{path}", "-c", "x"],
            &["sh", "-c", "x", "sh", "-e", "{path}"],
            // An option with a value after `-c`, which is only a flag.
            &["bash", "-c", "-o", "pipefail", "x {path}"],
            &["bash", "-co", "pipefail", "x {path}"],
            &["bash", "-c", "-O", "extglob", "x {path}"],
            &["bash", "-c", "+o", "posix", "x {path}"],
            &["sh", "-c", "-o", "errexit", "x {path}"],
            &["zsh", "-c", "-o", "errexit", "x {path}"],
            // The file itself as the script.
            &["bash", "-o", "pipefail", "{path}"],
            // A value in $0 counts as the script's place too.
            &["bash", "-c", "x {{a}} \"$1\"", "{path}"],
            &["mksh", "-c", "x {path}"],
            &["tcsh", "-c", "x {path}"],
            // tcsh reads options after the script too, until the first word
            // that is not one, so the slot after the script stays closed.
            &["tcsh", "-c", "x $1:q", "{path}"],
            &["csh", "-c", "x $1:q", "{path}"],
            &["tcsh", "-c", "x $2:q", "-x", "{path}"],
            &["tcsh", "-f", "{path}"],
            &["tcsh6", "-c", "x {path}"],
            &["sh", "-{path}"],
            // Versioned names, as distributions install them.
            &["bash5", "-c", "x {path}"],
            &["zsh5", "-c", "x {path}"],
            &["bash-5.2", "-c", "x {path}"],
            &["bash5.2", "-c", "x {path}"],
            &["/usr/local/bin/bash5", "-c", "x {path}"],
            &["BASH-5.2.exe", "-c", "x {path}"],
        ] {
            let e = error(Panel::Files, command, &[]);
            assert!(
                e.starts_with(r#"command 2: "{path}" is in the shell script or before it"#)
                    && e.ends_with(rule),
                "{command:?}: {e}"
            );
        }
        assert!(parse(Panel::Files, &["sh", "-c", "x \"$1\"", "sh", "{path}"], &[]).is_ok());
        let braces = ["bash", "-c", "x {{a}} \"$1\"", "bash", "{path}"];
        assert!(parse(Panel::Files, &braces, &[]).is_ok());
        let late_option = ["bash", "-c", "-o", "pipefail", "x \"$1\"", "bash", "{path}"];
        assert!(parse(Panel::Files, &late_option, &[]).is_ok());
        let pipefail = [
            "bash",
            "-o",
            "pipefail",
            "-c",
            "x \"$1\" | y",
            "bash",
            "{path}",
        ];
        assert!(parse(Panel::Files, &pipefail, &[]).is_ok());
        // Plain text after the script, then a value: still an argument.
        assert!(parse(Panel::Files, &["sh", "-c", "x", "sh", "--to={path}"], &[]).is_ok());
        // Not a shell: the value is an argument like any other.
        assert!(parse(Panel::Files, &["shellcheck", "{path}"], &[]).is_ok());
        let versioned = ["bash5", "-c", "x \"$1\"", "bash5", "{path}"];
        assert!(parse(Panel::Files, &versioned, &[]).is_ok());
        // csh/tcsh: a name that is not an option, then the values from `$2`.
        let filler = ["tcsh", "-c", "x $2:q", "tcsh", "{path}"];
        assert!(parse(Panel::Files, &filler, &[]).is_ok());
        let with_prompt = [
            "csh",
            "-f",
            "-c",
            "x $2:q $3:q",
            "csh",
            "{path}",
            "{prompt1}",
        ];
        assert!(parse(Panel::Files, &with_prompt, &["Q"]).is_ok());
    }

    #[test]
    fn versioned_names_of_other_programs_are_not_shells() {
        for program in [
            "python3",
            "gcc-12",
            "lua5.4",
            "x264",
            "md5",
            "shellcheck",
            "bash-",
            "bash_5",
            "5",
        ] {
            let command = [program, "-c", "x {path}"];
            assert!(parse(Panel::Files, &command, &[]).is_ok(), "{program}");
        }
    }

    #[test]
    fn a_shell_name_with_a_version_suffix_is_a_shell() {
        for program in [
            "sh",
            "ksh93",
            "bash5",
            "bash-5.2",
            "bash-5.2.1",
            "zsh5.9",
            r"C:\x\Bash5.EXE",
            // Windows drops a trailing dot or space from a program name.
            "bash.",
            "bash.exe.",
            "bash ",
            "BASH.EXE. ",
            r"C:\x\bash.",
            "bash5.",
        ] {
            assert!(is_shell(program), "{program}");
        }
        for program in [
            "bash-",
            "bash_5",
            "5",
            "-5",
            "bash--5",
            "python3",
            "sh2x",
            "bash .exe",
            "python3.",
            ".",
        ] {
            assert!(!is_shell(program), "{program}");
        }
    }

    #[test]
    fn a_shell_name_with_a_trailing_dot_is_checked_as_a_shell() {
        for command in [
            &["bash.", "-c", "x {path}"][..],
            &["bash.exe.", "-c", "x {path}"],
        ] {
            let e = error(Panel::Files, command, &[]);
            assert!(
                e.starts_with(r#"command 2: "{path}" is in the shell script or before it"#),
                "{command:?}: {e}"
            );
        }
    }

    #[test]
    fn a_shell_started_through_a_launcher_is_checked() {
        for command in [
            &["env", "bash", "-c", "x {path}"][..],
            &["/usr/bin/env", "-i", "FOO=1", "sh", "-c", "x {path}"],
            &["busybox", "sh", "-c", "x {path}"],
            &["nice", "-n", "5", "bash", "-c", "x {path}"],
            &["timeout", "5", "sh", "{path}"],
            &["xargs", "-0", "sh", "-c", "x {path}"],
            &["env", "nice", "bash", "-c", "x {path}"],
            &["ENV.EXE.", "sh", "-c", "x {path}"],
            // Refused although safe: the first shell name is taken as the shell.
            &["timeout", "5", "grep", "sh", "{path}"],
        ] {
            let e = error(Panel::Files, command, &[]);
            assert!(
                e.starts_with(r#"command 2: "{path}" is in the shell script or before it"#),
                "{command:?}: {e}"
            );
        }
    }

    #[test]
    fn a_placeholder_before_a_launched_shell_is_an_error() {
        let e = error(Panel::Files, &["env", "X={path}", "sh", "-c", "x"], &[]);
        assert!(
            e.starts_with(r#"command 2: "{path}" is before the shell that env starts"#),
            "{e}"
        );
        let e = error(
            Panel::Files,
            &["nice", "{prompt1}", "bash", "-c", "x"],
            &["Q"],
        );
        assert!(
            e.starts_with(r#"command 2: "{prompt1}" is before the shell that nice starts"#),
            "{e}"
        );
        for command in [
            &["env", "-S", "sh -c x", "{path}"][..],
            &["env", "-iS", "bash -c x", "{path}"],
            &["env", "-Ssh -c x", "{path}"],
            &["env", "--split-string=sh -c x", "{path}"],
            &["env", "--split-string", "sh -c x", "{path}"],
            &["env", "-S", "nice  bash\t-c x", "a", "{path}"],
            // env -S unquotes, unescapes and expands the words itself.
            &["env", "-S", "'sh' -c x", "{path}"],
            &["env", "-S", "s\\h -c x", "{path}"],
            &["env", "-S", "x\\_sh -c x", "{path}"],
            &["env", "-S", "${{SHELL}} -c x", "{path}"],
        ] {
            let e = error(Panel::Files, command, &[]);
            assert!(
                e.starts_with(r#"command 2: "{path}" is in a command where env -S starts a shell"#),
                "{command:?}: {e}"
            );
        }
    }

    #[test]
    fn a_launcher_without_a_shell_or_with_a_safe_shell_form_parses() {
        for command in [
            &["env", "sh", "-c", "x \"$1\"", "sh", "{path}"][..],
            &[
                "nice", "-n", "5", "bash", "-o", "pipefail", "-c", "x \"$1\"", "bash", "{path}",
            ],
            &["env", "FOO=1", "grep", "x", "{path}"],
            &["env", "-S", "python3 -u", "s.py", "{path}"],
            &["env", "-S", "sh -c x"],
            &["shellcheck", "{path}"],
            &["grep", "bash", "{path}"],
        ] {
            assert!(parse(Panel::Files, command, &[]).is_ok(), "{command:?}");
        }
    }

    /// The forms the guard accepts, run by the real shells on a hostile
    /// name: the name must arrive as data, never run.
    #[cfg(unix)]
    #[test]
    fn accepted_shell_forms_never_run_the_value() {
        let tmp = crate::git::fake::TempDir::new("shell-guard");
        let hostile = target(RepoPath::from(
            "a;touch pwned1;$(touch pwned2)`touch pwned3`",
        ));
        let print = "printf '%s' \"$1\" > out";
        let forms: [&[&str]; 5] = [
            &["sh", "-c", print, "sh", "{path}"],
            &["env", "sh", "-c", print, "sh", "{path}"],
            &["bash", "-o", "pipefail", "-c", print, "bash", "{path}"],
            &["bash", "-c", "-o", "pipefail", print, "bash", "{path}"],
            &["sh", "-c", print, "sh", "--to={path}"],
        ];
        for form in forms {
            let argv = expand(form, &hostile, &[]).unwrap();
            let status = std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .current_dir(tmp.path())
                .status()
                .unwrap();
            assert!(status.success(), "{form:?}");
            let out = std::fs::read_to_string(tmp.path().join("out")).unwrap();
            assert!(
                out.ends_with("a;touch pwned1;$(touch pwned2)`touch pwned3`"),
                "{out}"
            );
            for n in 1..=3 {
                assert!(!tmp.path().join(format!("pwned{n}")).exists(), "{form:?}");
            }
        }
    }

    #[test]
    fn keys_and_description_are_required() {
        let mut no_keys = table(Panel::Files, &["true"], &[]);
        no_keys.keys.clear();
        assert_eq!(
            CustomCommand::parse(&no_keys, 1).unwrap_err(),
            "command 1: needs at least one key"
        );
        let mut bad_key = table(Panel::Files, &["true"], &[]);
        bad_key.keys = vec!["hyper-x".into()];
        let e = CustomCommand::parse(&bad_key, 1).unwrap_err();
        assert!(e.starts_with(r#"command 1: bad key "hyper-x""#), "{e}");
        let mut no_text = table(Panel::Files, &["true"], &[]);
        no_text.description = " ".into();
        assert_eq!(
            CustomCommand::parse(&no_text, 1).unwrap_err(),
            "command 1: needs a description"
        );
    }

    #[test]
    fn a_substituted_value_is_always_one_whole_argument() {
        let hostile = target(RepoPath::from("a b;$(rm -rf x)"));
        let argv = expand(&["echo", "{path}", "--file={path}"], &hostile, &[]).unwrap();
        assert_eq!(
            argv,
            [
                OsString::from("echo"),
                OsString::from("a b;$(rm -rf x)"),
                OsString::from("--file=a b;$(rm -rf x)"),
            ]
        );
        let answers = expand(
            &["echo", "{prompt1}", "{prompt2}"],
            &hostile,
            &["x *", "-y"],
        )
        .unwrap();
        assert_eq!(answers[1..], [OsString::from("x *"), OsString::from("-y")]);
    }

    #[test]
    fn a_path_that_looks_like_an_option_is_made_relative() {
        let dash = target(RepoPath::from("-rf"));
        let argv = expand(&["rm", "{path}", "--file={path}", "{path}.bak"], &dash, &[]).unwrap();
        assert_eq!(argv[1], OsString::from("./-rf"));
        assert_eq!(argv[2], OsString::from("--file=-rf"));
        assert_eq!(argv[3], OsString::from("./-rf.bak"));
        // An empty answer before it leaves the path at the start.
        let argv = expand(&["rm", "{prompt1}{path}"], &dash, &[""]).unwrap();
        assert_eq!(argv[1], OsString::from("./-rf"));
        let argv = expand(&["rm", "{prompt1}{path}"], &dash, &["x"]).unwrap();
        assert_eq!(argv[1], OsString::from("x-rf"));
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_path_keeps_its_raw_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let raw = target(RepoPath::from_bytes(b"x\xff\x1b.txt"));
        let argv = expand(&["cat", "--", "{path}", "a/{path}"], &raw, &[]).unwrap();
        assert_eq!(argv[2].as_bytes(), b"x\xff\x1b.txt");
        assert_eq!(argv[3].as_bytes(), b"a/x\xff\x1b.txt");
    }

    #[test]
    fn line_and_commit_come_from_the_target_or_say_what_is_missing() {
        let full = Target {
            path: "a.rs".into(),
            line: Some(12),
            commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
        };
        let argv = expand(&["git", "show", "{commit}", "+{line}"], &full, &[]).unwrap();
        assert_eq!(
            argv[2..],
            [
                OsString::from("0123456789abcdef0123456789abcdef01234567"),
                OsString::from("+12"),
            ]
        );
        let bare = target("a.rs".into());
        assert_eq!(expand(&["echo", "{line}"], &bare, &[]), Err("No line here"));
        assert_eq!(
            expand(&["echo", "{commit}"], &bare, &[]),
            Err("Not committed yet")
        );
        assert_eq!(expand(&["echo", "{path}"], &bare, &[]).unwrap().len(), 2);
    }

    #[test]
    fn a_table_reads_from_toml_with_defaults() {
        let table: CommandTable = toml::from_str(
            "context = \"diff\"\nkeys = [\"X\"]\ndescription = \"d\"\ncommand = [\"true\"]\n",
        )
        .unwrap();
        assert_eq!(table.context, Panel::Diff);
        assert_eq!(table.output, OutputMode::Terminal);
        assert!(table.prompts.is_empty());
        let typo = toml::from_str::<CommandTable>(
            "context = \"files\"\nkeys = []\ndescription = \"d\"\ncommand = []\nshell = true\n",
        )
        .unwrap_err();
        assert!(typo.message().contains("unknown field `shell`"), "{typo}");
    }
}
