use isksh::{Shell, bash_startup_file, load_startup_file, run_interactive, startup_file};
use rustyline::completion::{Completer, FilenameCompleter, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{Context, Editor, Helper};
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, BufReader, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

fn main() {
    let status = match run_cli() {
        Ok(status) => status,
        Err((status, message)) => {
            eprintln!("isksh: {message}");
            status
        }
    };
    std::process::exit(status);
}

fn run_cli() -> Result<i32, (i32, String)> {
    let arguments = std::env::args_os()
        .skip(1)
        .map(|argument| {
            argument
                .into_string()
                .map_err(|_| (2, "arguments must be valid UTF-8".to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;

    if arguments.first().map(String::as_str) == Some("--help") {
        println!(
            "isksh 0.1.0\n\nUsage:\n  isksh SCRIPT [ARG...]\n  isksh -c COMMAND [NAME [ARG...]]\n  isksh -s [ARG...]\n  isksh -i\n"
        );
        return Ok(0);
    }
    if arguments.first().map(String::as_str) == Some("--version") {
        println!("isksh {}", env!("CARGO_PKG_VERSION"));
        return Ok(0);
    }

    let force_interactive = arguments.as_slice() == ["-i"];
    if force_interactive || arguments.is_empty() && io::stdin().is_terminal() {
        let mut shell = Shell::new("isksh");
        shell.set_interactive(io::stdin().is_terminal() && io::stdout().is_terminal());
        let mut reader = BufReader::new(io::stdin());
        let mut stdout = io::stdout();
        let mut stderr = io::stderr();
        for path in startup_file().into_iter().chain(bash_startup_file()) {
            match load_startup_file(&mut shell, &path) {
                Ok(Some(result)) => {
                    stdout
                        .write_all(&result.stdout)
                        .map_err(|error| (1, error.to_string()))?;
                    stderr
                        .write_all(&result.stderr)
                        .map_err(|error| (1, error.to_string()))?;
                    if let Some(status) = shell.take_exit_status() {
                        return Ok(status);
                    }
                    break;
                }
                Ok(None) => {}
                Err(error) => {
                    writeln!(stderr, "isksh: {}: {error}", path.display())
                        .map_err(|error| (1, error.to_string()))?;
                    break;
                }
            }
        }
        if io::stdin().is_terminal() && io::stdout().is_terminal() {
            return run_line_editor(&mut shell).map_err(|error| (1, error.to_string()));
        }
        return run_interactive(&mut shell, &mut reader, &mut stdout, &mut stderr, true)
            .map_err(|error| (1, error.to_string()));
    }

    let (source, name, positional) = match arguments.first().map(String::as_str) {
        Some("-c") => {
            let source = arguments
                .get(1)
                .cloned()
                .ok_or_else(|| (2, "-c requires a command string".to_string()))?;
            let name = arguments.get(2).cloned().unwrap_or_else(|| "isksh".into());
            let positional = arguments.get(3..).unwrap_or_default().to_vec();
            (source, name, positional)
        }
        Some("-s") => (
            read_stdin_utf8()?,
            "isksh".into(),
            arguments.get(1..).unwrap_or_default().to_vec(),
        ),
        Some(path) if path.starts_with('-') => {
            return Err((2, format!("unknown option: {path}")));
        }
        Some(path) => {
            let source =
                fs::read_to_string(path).map_err(|error| (126, format!("{path}: {error}")))?;
            (
                source,
                path.to_string(),
                arguments.get(1..).unwrap_or_default().to_vec(),
            )
        }
        None => (read_stdin_utf8()?, "isksh".into(), Vec::new()),
    };

    let mut shell = Shell::new(name);
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

struct ShellHelper {
    files: FilenameCompleter,
    commands: Vec<String>,
}

impl Helper for ShellHelper {}
impl Validator for ShellHelper {}
impl Highlighter for ShellHelper {}

impl Hinter for ShellHelper {
    type Hint = String;
}

impl Completer for ShellHelper {
    type Candidate = Pair;

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

fn run_line_editor(shell: &mut Shell) -> io::Result<i32> {
    let mut editor = Editor::<ShellHelper, DefaultHistory>::new().map_err(io::Error::other)?;
    editor.set_helper(Some(ShellHelper {
        files: FilenameCompleter::new(),
        commands: command_candidates(shell),
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
        }
        match editor.readline(&prompt) {
            Ok(line) => {
                source.push_str(&line);
                source.push('\n');
                if matches!(Shell::check_input(&source), isksh::InputState::Incomplete) {
                    continue;
                }
                if !source.trim().is_empty() {
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

fn command_candidates(shell: &Shell) -> Vec<String> {
    let mut commands = BTreeSet::from(
        [
            "alias", "cd", "command", "echo", "eval", "exec", "exit", "export", "false", "jobs",
            "printf", "pwd", "read", "set", "source", "test", "trap", "true", "type", "unalias",
            "unset", "wait",
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

fn read_stdin_utf8() -> Result<String, (i32, String)> {
    let mut bytes = Vec::new();
    io::stdin()
        .read_to_end(&mut bytes)
        .map_err(|error| (1, error.to_string()))?;
    String::from_utf8(bytes).map_err(|error| {
        (
            2,
            format!(
                "input must be valid UTF-8 (invalid byte at offset {})",
                error.utf8_error().valid_up_to()
            ),
        )
    })
}
