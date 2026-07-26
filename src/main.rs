use isksh::{Shell, cli_help, load_startup_file, localize, run_interactive, startup_files};
use rustyline::Movement;
use rustyline::Word;
use rustyline::completion::{Completer, FilenameCompleter, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{
    Cmd, ConditionalEventHandler, Event, EventContext, EventHandler, KeyEvent, RepeatCount,
};
use rustyline::{Context, Editor, Helper};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::{self, BufReader, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// `main`に対応する処理を行う。
fn main() {
    let status = match run_cli() {
        Ok(status) => status,
        Err((status, message)) => {
            eprintln!("isksh: {}", localize(message));
            status
        }
    };
    std::process::exit(status);
}

/// `run_cli`に対応する処理を行う。
fn run_cli() -> Result<i32, (i32, String)> {
    let mut raw_arguments = std::env::args_os();
    let executable = raw_arguments.next().unwrap_or_default();
    let arguments = raw_arguments
        .map(|argument| {
            argument
                .into_string()
                .map_err(|_| (2, localize("arguments must be valid UTF-8")))
        })
        .collect::<Result<Vec<_>, _>>()?;

    if arguments.first().map(String::as_str) == Some("--help") {
        print!("{}", cli_help(env!("CARGO_PKG_VERSION")));
        return Ok(0);
    }
    if arguments.first().map(String::as_str) == Some("--version") {
        println!("isksh {}", env!("CARGO_PKG_VERSION"));
        return Ok(0);
    }

    let (force_interactive, login, arguments) = parse_shell_options(
        arguments,
        Path::new(&executable)
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with('-')),
    )?;
    let interactive = force_interactive || arguments.is_empty() && io::stdin().is_terminal();
    let name = if arguments.first().map(String::as_str) == Some("-c") {
        arguments.get(2).cloned().unwrap_or_else(|| "isksh".into())
    } else if arguments.first().is_some_and(|path| !path.starts_with('-')) {
        arguments[0].clone()
    } else {
        "isksh".into()
    };
    let mut shell = Shell::new(name);
    shell.set_interactive(interactive && io::stdin().is_terminal() && io::stdout().is_terminal());
    let mut startup_stdout = io::stdout();
    let mut startup_stderr = io::stderr();
    if let Some(files) = startup_files() {
        load_and_report_startup(
            &mut shell,
            &files.env,
            &mut startup_stdout,
            &mut startup_stderr,
        )?;
        shell.refresh_mode();
        if login {
            load_and_report_startup(
                &mut shell,
                &files.profile,
                &mut startup_stdout,
                &mut startup_stderr,
            )?;
        }
        if interactive {
            load_and_report_startup(
                &mut shell,
                &files.rc,
                &mut startup_stdout,
                &mut startup_stderr,
            )?;
        }
    }
    if let Some(status) = shell.take_exit_status() {
        return Ok(status);
    }

    if interactive && arguments.is_empty() {
        let mut reader = BufReader::new(io::stdin());
        let mut stdout = io::stdout();
        let mut stderr = io::stderr();
        if io::stdin().is_terminal() && io::stdout().is_terminal() {
            return run_line_editor(&mut shell).map_err(|error| (1, error.to_string()));
        }
        return run_interactive(&mut shell, &mut reader, &mut stdout, &mut stderr, true)
            .map_err(|error| (1, error.to_string()));
    }

    let (source, positional) = match arguments.first().map(String::as_str) {
        Some("-c") => {
            let source = arguments
                .get(1)
                .cloned()
                .ok_or_else(|| (2, localize("-c requires a command string")))?;
            let positional = arguments.get(3..).unwrap_or_default().to_vec();
            (source, positional)
        }
        Some("-s") => (
            read_stdin_utf8()?,
            arguments.get(1..).unwrap_or_default().to_vec(),
        ),
        Some(path) if path.starts_with('-') => {
            return Err((2, localize(format!("unknown option: {path}"))));
        }
        Some(path) => {
            let source =
                fs::read_to_string(path).map_err(|error| (126, format!("{path}: {error}")))?;
            (source, arguments.get(1..).unwrap_or_default().to_vec())
        }
        None => (read_stdin_utf8()?, Vec::new()),
    };

    shell.set_positional(positional);
    let result = shell.run(&source, &[]);
    io::stdout()
        .write_all(&result.stdout)
        .map_err(|error| (1, error.to_string()))?;
    io::stderr()
        .write_all(&result.stderr)
        .map_err(|error| (1, error.to_string()))?;
    Ok(result.status)
}

/// `parse_shell_options`に対応する処理を行う。
fn parse_shell_options(
    arguments: Vec<String>,
    login_from_argv0: bool,
) -> Result<(bool, bool, Vec<String>), (i32, String)> {
    let mut interactive = false;
    let mut login = login_from_argv0;
    let mut index = 0;
    while let Some(argument) = arguments.get(index).map(String::as_str) {
        match argument {
            "--" => {
                index += 1;
                break;
            }
            "--login" => login = true,
            "-c" | "-s" => break,
            option if option.starts_with('-') && option.len() > 1 => {
                for flag in option[1..].chars() {
                    match flag {
                        'i' => interactive = true,
                        'l' => login = true,
                        _ => return Err((2, localize(format!("unknown option: {option}")))),
                    }
                }
            }
            _ => break,
        }
        index += 1;
    }
    Ok((interactive, login, arguments[index..].to_vec()))
}

/// `load_and_report_startup`に対応する処理を行う。
fn load_and_report_startup(
    shell: &mut Shell,
    path: &Path,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> Result<(), (i32, String)> {
    match load_startup_file(shell, path) {
        Ok(Some(result)) => {
            stdout
                .write_all(&result.stdout)
                .map_err(|error| (1, error.to_string()))?;
            stderr
                .write_all(&result.stderr)
                .map_err(|error| (1, error.to_string()))?;
        }
        Ok(None) => {}
        Err(error) => {
            writeln!(
                stderr,
                "{}",
                localize(format!("isksh: {}: {error}", path.display()))
            )
            .map_err(|error| (1, error.to_string()))?;
        }
    }
    Ok(())
}

struct ShellHelper {
    files: FilenameCompleter,
    commands: Vec<String>,
    custom: Vec<String>,
}

struct AbbreviationHandler(Arc<RwLock<HashMap<String, String>>>);

impl ConditionalEventHandler for AbbreviationHandler {
    /// `handle`に対応する処理を行う。
    fn handle(&self, _: &Event, _: RepeatCount, _: bool, context: &EventContext) -> Option<Cmd> {
        let before_cursor = &context.line()[..context.pos()];
        if in_shell_quote(before_cursor) {
            return None;
        }
        let start = before_cursor
            .char_indices()
            .rev()
            .find(|(_, character)| character.is_whitespace() || ";|&()".contains(*character))
            .map_or(0, |(index, character)| index + character.len_utf8());
        let word = &before_cursor[start..];
        let prefix = before_cursor[..start].trim_end();
        if !prefix.is_empty() && !prefix.ends_with([';', '|', '&', '(', ')']) {
            return None;
        }
        let expansion = self.0.read().ok()?.get(word)?.clone();
        Some(Cmd::Replace(
            Movement::BackwardWord(1, Word::Big),
            Some(format!("{expansion} ")),
        ))
    }
}

/// `in_shell_quote`に対応する処理を行う。
fn in_shell_quote(source: &str) -> bool {
    let mut quote = None;
    let mut escaped = false;
    for character in source.chars() {
        if escaped {
            escaped = false;
        } else if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            } else if delimiter == '"' && character == '\\' {
                escaped = true;
            }
        } else if matches!(character, '\'' | '"') {
            quote = Some(character);
        } else if character == '\\' {
            escaped = true;
        }
    }
    quote.is_some()
}

impl Helper for ShellHelper {}
impl Validator for ShellHelper {}
impl Highlighter for ShellHelper {}

impl Hinter for ShellHelper {
    type Hint = String;
}

impl Completer for ShellHelper {
    type Candidate = Pair;

    /// `complete`に対応する処理を行う。
    fn complete(
        &self,
        line: &str,
        position: usize,
        context: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        let start = line[..position]
            .rfind(char::is_whitespace)
            .map_or(0, |index| index + 1);
        let prefix = &line[start..position];
        if line[..start].trim().is_empty() && !prefix.contains(['/', '\\']) {
            let candidates = self
                .commands
                .iter()
                .chain(self.custom.iter())
                .filter(|command| command.starts_with(prefix))
                .map(|command| Pair {
                    display: command.clone(),
                    replacement: command.clone(),
                })
                .collect();
            Ok((start, candidates))
        } else {
            self.files.complete(line, position, context)
        }
    }
}

/// `run_line_editor`に対応する処理を行う。
fn run_line_editor(shell: &mut Shell) -> io::Result<i32> {
    let mut editor = Editor::<ShellHelper, DefaultHistory>::new().map_err(io::Error::other)?;
    let abbreviations = Arc::new(RwLock::new(shell.configured_abbreviations()));
    editor.bind_sequence(
        KeyEvent::from(' '),
        EventHandler::Conditional(Box::new(AbbreviationHandler(abbreviations.clone()))),
    );
    editor.set_helper(Some(ShellHelper {
        files: FilenameCompleter::new(),
        commands: command_candidates(shell),
        custom: shell.configured_completion_candidates(),
    }));
    let history = history_file();
    if let Some(parent) = history.parent() {
        fs::create_dir_all(parent)?;
    }
    let _ = editor.load_history(&history);
    let mut source = String::new();
    let mut status = 0;
    loop {
        let prompt = shell.prompt(!source.is_empty());
        if let Some(helper) = editor.helper_mut() {
            helper.commands = command_candidates(shell);
            helper.custom = shell.configured_completion_candidates();
        }
        if let Ok(mut configured) = abbreviations.write() {
            *configured = shell.configured_abbreviations();
        }
        match editor.readline(&prompt) {
            Ok(line) => {
                source.push_str(&line);
                source.push('\n');
                if matches!(Shell::check_input(&source), isksh::InputState::Incomplete) {
                    continue;
                }
                source = shell.expand_abbreviations(&source);
                if !source.trim().is_empty() && shell.record_history(source.trim_end()) {
                    let _ = editor.add_history_entry(source.trim_end());
                }
                let result = shell.run(&source, &[]);
                io::stdout().write_all(&result.stdout)?;
                io::stderr().write_all(&result.stderr)?;
                status = result.status;
                source.clear();
                if let Some(exit_status) = shell.take_exit_status() {
                    status = exit_status;
                    break;
                }
            }
            Err(ReadlineError::Interrupted) => {
                source.clear();
                status = 130;
            }
            Err(ReadlineError::Eof) => break,
            Err(error) => return Err(io::Error::other(error)),
        }
    }
    editor.save_history(&history).map_err(io::Error::other)?;
    Ok(status)
}

/// `command_candidates`に対応する処理を行う。
fn command_candidates(shell: &Shell) -> Vec<String> {
    let mut commands = BTreeSet::from(
        [
            "abbr",
            "add-zsh-hook",
            "alias",
            "autoload",
            "builtin",
            "cd",
            "command",
            "dirs",
            "echo",
            "eval",
            "exec",
            "exit",
            "export",
            "false",
            "help",
            "jobs",
            "let",
            "popd",
            "print",
            "printf",
            "pushd",
            "pwd",
            "read",
            "set",
            "setopt",
            "source",
            "test",
            "trap",
            "true",
            "type",
            "unalias",
            "unfunction",
            "unset",
            "unsetopt",
            "wait",
        ]
        .map(str::to_string),
    );
    commands.extend(shell.configured_command_names());
    if let Some(path) = shell.command_search_path() {
        for directory in std::env::split_paths(&path) {
            let Ok(entries) = fs::read_dir(directory) else {
                continue;
            };
            for entry in entries.flatten() {
                if entry.path().is_file()
                    && let Some(name) = entry.path().file_name().and_then(|name| name.to_str())
                {
                    let name = if cfg!(windows) {
                        Path::new(name)
                            .file_stem()
                            .and_then(|name| name.to_str())
                            .unwrap_or(name)
                    } else {
                        name
                    };
                    commands.insert(name.to_string());
                }
            }
        }
    }
    commands.into_iter().collect()
}

/// `history_file`に対応する処理を行う。
fn history_file() -> PathBuf {
    std::env::var_os("ISKSH_HISTORY")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_STATE_HOME")
                .map(PathBuf::from)
                .map(|path| path.join("isksh/history"))
        })
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|path| path.join(".local/state/isksh/history"))
        })
        .or_else(|| {
            std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .map(|path| path.join("isksh/history"))
        })
        .unwrap_or_else(|| Path::new(".isksh_history").to_path_buf())
}

/// `read_stdin_utf8`に対応する処理を行う。
fn read_stdin_utf8() -> Result<String, (i32, String)> {
    let mut bytes = Vec::new();
    io::stdin()
        .read_to_end(&mut bytes)
        .map_err(|error| (1, error.to_string()))?;
    String::from_utf8(bytes).map_err(|error| {
        (
            2,
            localize(format!(
                "input must be valid UTF-8 (invalid byte at offset {})",
                error.utf8_error().valid_up_to()
            )),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::{in_shell_quote, parse_shell_options};

    #[test]
    /// `identifies_open_shell_quotes`に対応する処理を行う。
    fn identifies_open_shell_quotes() {
        assert!(in_shell_quote("echo 'open"));
        assert!(in_shell_quote("echo \"open\\\" still open"));
        assert!(!in_shell_quote("echo 'closed'"));
        assert!(!in_shell_quote("echo escaped\\ space"));
    }

    #[test]
    /// `parses_login_and_interactive_options`に対応する処理を行う。
    fn parses_login_and_interactive_options() {
        assert_eq!(
            parse_shell_options(vec!["-il".into()], false).unwrap(),
            (true, true, Vec::new())
        );
        assert_eq!(
            parse_shell_options(vec!["--login".into(), "-c".into(), "true".into()], false).unwrap(),
            (false, true, vec!["-c".into(), "true".into()])
        );
        assert!(parse_shell_options(vec!["-x".into()], false).is_err());
        assert_eq!(
            parse_shell_options(Vec::new(), true).unwrap(),
            (false, true, Vec::new())
        );
    }
}
