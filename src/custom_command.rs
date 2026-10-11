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
const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "fish"];

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
/// where the script reads it as `"$1"`.
///
/// Conservative, since some options take an argument (`-o pipefail`,
/// `--rcfile f`) and `+x` is an option too: the last option before the
/// first placeholder ends the options, and the first plain element after
/// it is taken as the script. The placeholder must come after that.
fn check_shell(command: &[String], argv: &[Vec<Part>]) -> Result<(), String> {
    let program = command[0].rsplit(['/', '\\']).next().unwrap_or_default();
    let program = program.to_ascii_lowercase();
    let program = program.strip_suffix(".exe").unwrap_or(&program);
    if !SHELLS.contains(&program) {
        return Ok(());
    }
    let Some((first, part)) = argv.iter().enumerate().skip(1).find_map(|(i, parts)| {
        let value = parts.iter().find(|p| !matches!(p, Part::Text(_)));
        value.map(|part| (i, part))
    }) else {
        return Ok(());
    };
    let is_option = |element: &String| element.starts_with(['-', '+']);
    let options_end = (1..first)
        .rev()
        .find(|&i| is_option(&command[i]))
        .unwrap_or(0);
    let script = (options_end + 1..command.len())
        .find(|&i| !is_option(&command[i]))
        .unwrap_or(command.len());
    if first <= script {
        return Err(format!(
            "\"{}\" is in the shell script or before it, where it would run as code; \
             pass it after the script and read it as \"$1\"",
            placeholder(part)
        ));
    }
    Ok(())
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
            &["sh", "-{path}"],
        ] {
            let e = error(Panel::Files, command, &[]);
            assert!(
                e.starts_with(r#"command 2: "{path}" is in the shell script or before it"#)
                    && e.ends_with(rule),
                "{command:?}: {e}"
            );
        }
        assert!(parse(Panel::Files, &["sh", "-c", "x \"$1\"", "sh", "{path}"], &[]).is_ok());
        assert!(
            parse(
                Panel::Files,
                &["bash", "-c", "x {{a}} \"$1\"", "{path}"],
                &[]
            )
            .is_ok()
        );
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
