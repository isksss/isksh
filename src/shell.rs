use crate::ast::*;
use crate::parser::parse;
use glob::{MatchOptions, Pattern, glob_with};
use regex::Regex;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static PROCESS_SUBSTITUTION_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
struct Variable {
    value: String,
    exported: bool,
    readonly: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Flow {
    None,
    Break(usize),
    Continue(usize),
    Return(i32),
    Exit(i32),
}

#[derive(Debug, Clone)]
struct ExecResult {
    status: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    flow: Flow,
}

#[derive(Debug, Clone)]
enum OutputSink {
    Stdout,
    Stderr,
    File(PathBuf),
    Closed,
}

#[derive(Debug, Clone)]
struct PendingProcessSubstitution {
    path: PathBuf,
    source: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct LocalScope {
    variables: HashMap<String, Option<Variable>>,
    indexed_arrays: HashMap<String, Option<BTreeMap<usize, String>>>,
    associative_arrays: HashMap<String, Option<BTreeMap<String, String>>>,
}

impl ExecResult {
    fn status(status: i32) -> Self {
        Self {
            status,
            stdout: Vec::new(),
            stderr: Vec::new(),
            flow: Flow::None,
        }
    }

    fn error(status: i32, message: impl AsRef<str>) -> Self {
        let mut result = Self::status(status);
        result.stderr.extend_from_slice(message.as_ref().as_bytes());
        if !message.as_ref().ends_with('\n') {
            result.stderr.push(b'\n');
        }
        result
    }

    fn append(&mut self, mut other: ExecResult) {
        self.status = other.status;
        self.stdout.append(&mut other.stdout);
        self.stderr.append(&mut other.stderr);
        if other.flow != Flow::None {
            self.flow = other.flow;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunResult {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputState {
    Complete,
    Incomplete,
    Invalid(String),
}

#[derive(Debug, Clone)]
pub struct Shell {
    variables: HashMap<String, Variable>,
    positional: Vec<String>,
    name: String,
    last_status: i32,
    functions: HashMap<String, Command>,
    aliases: HashMap<String, String>,
    indexed_arrays: HashMap<String, BTreeMap<usize, String>>,
    associative_arrays: HashMap<String, BTreeMap<String, String>>,
    shell_options: HashSet<String>,
    pending_process_substitutions: Vec<PendingProcessSubstitution>,
    local_scopes: Vec<LocalScope>,
    expanding_aliases: Vec<String>,
    cwd: PathBuf,
    loop_depth: usize,
    function_depth: usize,
    getopts_offset: usize,
    exit_status: Option<i32>,
    terminal_io: bool,
}

impl Default for Shell {
    fn default() -> Self {
        Self::new("isksh")
    }
}

impl Shell {
    pub fn new(name: impl Into<String>) -> Self {
        let mut variables: HashMap<_, _> = std::env::vars()
            .map(|(name, value)| {
                (
                    name,
                    Variable {
                        value,
                        exported: true,
                        readonly: false,
                    },
                )
            })
            .collect();
        let cwd = std::env::current_dir().unwrap_or(PathBuf::from("."));
        variables.insert(
            "PWD".into(),
            Variable {
                value: cwd.to_string_lossy().into_owned(),
                exported: true,
                readonly: false,
            },
        );
        Self {
            variables,
            positional: Vec::new(),
            name: name.into(),
            last_status: 0,
            functions: HashMap::new(),
            aliases: HashMap::new(),
            indexed_arrays: HashMap::new(),
            associative_arrays: HashMap::new(),
            shell_options: HashSet::new(),
            pending_process_substitutions: Vec::new(),
            local_scopes: Vec::new(),
            expanding_aliases: Vec::new(),
            cwd,
            loop_depth: 0,
            function_depth: 0,
            getopts_offset: 1,
            exit_status: None,
            terminal_io: false,
        }
    }

    pub fn set_positional(&mut self, values: Vec<String>) {
        self.positional = values;
    }

    /// Controls whether foreground external commands may use the shell's terminal directly.
    pub fn set_interactive(&mut self, interactive: bool) {
        self.terminal_io = interactive;
    }

    pub fn check_input(source: &str) -> InputState {
        match parse(source) {
            Ok(_) => InputState::Complete,
            Err(error) if error.incomplete => InputState::Incomplete,
            Err(error) => InputState::Invalid(error.to_string()),
        }
    }

    pub fn prompt(&mut self, continuation: bool) -> String {
        let name = if continuation { "PS2" } else { "PS1" };
        let default = || {
            if continuation {
                "> ".to_string()
            } else {
                "$ ".to_string()
            }
        };
        let saved_status = self.last_status;
        let mut prefix = String::new();
        if !continuation
            && let Some(command) = self.value_of("PROMPT_COMMAND")
            && !command.is_empty()
        {
            let result = self.run(&command, &[]);
            prefix.push_str(&String::from_utf8_lossy(&result.stdout));
        }
        self.last_status = saved_status;
        let value = self.value_of(name).unwrap_or_else(default);
        let escaped = self.expand_prompt_escapes(&value);
        prefix.push_str(
            &self
                .expand_here_document(&escaped)
                .unwrap_or_else(|_| escaped.clone()),
        );
        self.last_status = saved_status;
        prefix
    }

    pub fn take_exit_status(&mut self) -> Option<i32> {
        self.exit_status.take()
    }

    pub fn run(&mut self, source: &str, input: &[u8]) -> RunResult {
        let script = match parse(source) {
            Ok(script) => script,
            Err(error) => {
                return RunResult {
                    status: 2,
                    stdout: Vec::new(),
                    stderr: format!("isksh: {error}\n").into_bytes(),
                };
            }
        };
        let result = self.execute_script(&script, input);
        let status = match result.flow {
            Flow::Exit(status) => {
                self.exit_status = Some(status);
                status
            }
            _ => result.status,
        };
        self.last_status = status;
        RunResult {
            status,
            stdout: result.stdout,
            stderr: result.stderr,
        }
    }

    fn execute_script(&mut self, script: &Script, input: &[u8]) -> ExecResult {
        let mut combined = ExecResult::status(0);
        for list in &script.lists {
            let result = self.execute_and_or(list, input);
            combined.append(result);
            self.last_status = combined.status;
            if combined.flow != Flow::None {
                break;
            }
        }
        combined
    }

    fn execute_and_or(&mut self, list: &AndOr, input: &[u8]) -> ExecResult {
        let mut result = if list.background {
            let mut child = self.clone();
            child.terminal_io = false;
            let mut result = child.execute_pipeline(&list.first, input);
            result
                .stderr
                .extend_from_slice(b"isksh: background execution is synchronous in this release\n");
            result
        } else {
            self.execute_pipeline(&list.first, input)
        };
        for (operator, pipeline) in &list.rest {
            if result.flow != Flow::None {
                break;
            }
            let should_run = match operator {
                AndOrOp::And => result.status == 0,
                AndOrOp::Or => result.status != 0,
            };
            if should_run {
                let next = self.execute_pipeline(pipeline, input);
                result.append(next);
            }
        }
        result
    }

    fn execute_pipeline(&mut self, pipeline: &Pipeline, input: &[u8]) -> ExecResult {
        let mut pipe_input = input.to_vec();
        let mut all_stderr = Vec::new();
        let mut last = ExecResult::status(0);
        for (index, command) in pipeline.commands.iter().enumerate() {
            let is_last = index + 1 == pipeline.commands.len();
            let mut result = if pipeline.commands.len() == 1 {
                self.execute_command(command, &pipe_input)
            } else {
                let mut child = self.clone();
                child.terminal_io = false;
                child.execute_command(command, &pipe_input)
            };
            all_stderr.append(&mut result.stderr);
            if is_last {
                last = result;
            } else {
                pipe_input = result.stdout;
            }
        }
        last.stderr.splice(0..0, all_stderr);
        if pipeline.negated {
            last.status = i32::from(last.status == 0);
        }
        last
    }

    fn execute_command(&mut self, command: &Command, input: &[u8]) -> ExecResult {
        match command {
            Command::Simple(command) => self.execute_simple(command, input),
            Command::If {
                branches,
                else_body,
            } => {
                let mut output = ExecResult::status(0);
                for (condition, body) in branches {
                    let condition_result = self.execute_script(condition, input);
                    let success = condition_result.status == 0;
                    output.append(condition_result);
                    if success {
                        output.append(self.execute_script(body, input));
                        return output;
                    }
                }
                if let Some(body) = else_body {
                    output.append(self.execute_script(body, input));
                }
                output
            }
            Command::While {
                condition,
                body,
                until,
            } => self.execute_loop(condition, body, *until, input),
            Command::For { name, words, body } => self.execute_for(name, words, body, input),
            Command::Case { word, arms } => self.execute_case(word, arms, input),
            Command::Group { body, subshell } => {
                if *subshell {
                    self.clone().execute_script(body, input)
                } else {
                    self.execute_script(body, input)
                }
            }
            Command::Function { name, body } => {
                self.functions.insert(name.clone(), (**body).clone());
                ExecResult::status(0)
            }
        }
    }

    fn execute_loop(
        &mut self,
        condition: &Script,
        body: &Script,
        until: bool,
        input: &[u8],
    ) -> ExecResult {
        self.loop_depth += 1;
        let mut output = ExecResult::status(0);
        loop {
            let condition_result = self.execute_script(condition, input);
            let run = (condition_result.status == 0) != until;
            output.append(condition_result);
            if !run || output.flow != Flow::None {
                break;
            }
            let mut iteration = self.execute_script(body, input);
            output.stdout.append(&mut iteration.stdout);
            output.stderr.append(&mut iteration.stderr);
            output.status = iteration.status;
            match iteration.flow {
                Flow::Break(level) if level <= 1 => break,
                Flow::Break(level) => {
                    output.flow = Flow::Break(level - 1);
                    break;
                }
                Flow::Continue(level) if level <= 1 => continue,
                Flow::Continue(level) => {
                    output.flow = Flow::Continue(level - 1);
                    break;
                }
                Flow::None => {}
                flow => {
                    output.flow = flow;
                    break;
                }
            }
        }
        self.loop_depth -= 1;
        output
    }

    fn execute_for(
        &mut self,
        name: &str,
        words: &[Word],
        body: &Script,
        input: &[u8],
    ) -> ExecResult {
        let values = if words.is_empty() {
            self.positional.clone()
        } else {
            let mut values = Vec::new();
            for word in words {
                match self.expand_word(word) {
                    Ok(mut expanded) => values.append(&mut expanded),
                    Err(message) => return ExecResult::error(1, format!("isksh: {message}")),
                }
            }
            values
        };
        self.loop_depth += 1;
        let mut output = ExecResult::status(0);
        for value in values {
            if let Err(message) = self.set_variable(name, value, None, false) {
                self.loop_depth -= 1;
                return ExecResult::error(1, message);
            }
            let mut iteration = self.execute_script(body, input);
            output.stdout.append(&mut iteration.stdout);
            output.stderr.append(&mut iteration.stderr);
            output.status = iteration.status;
            match iteration.flow {
                Flow::Break(level) if level <= 1 => break,
                Flow::Break(level) => {
                    output.flow = Flow::Break(level - 1);
                    break;
                }
                Flow::Continue(level) if level <= 1 => continue,
                Flow::Continue(level) => {
                    output.flow = Flow::Continue(level - 1);
                    break;
                }
                Flow::None => {}
                flow => {
                    output.flow = flow;
                    break;
                }
            }
        }
        self.loop_depth -= 1;
        output
    }

    fn execute_case(&mut self, word: &Word, arms: &[CaseArm], input: &[u8]) -> ExecResult {
        let value = match self.expand_scalar(word) {
            Ok(value) => value,
            Err(message) => return ExecResult::error(1, message),
        };
        for arm in arms {
            for pattern in &arm.patterns {
                let pattern = match self.expand_scalar(pattern) {
                    Ok(value) => value,
                    Err(message) => return ExecResult::error(1, message),
                };
                if Pattern::new(&pattern).is_ok_and(|pattern| pattern.matches(&value)) {
                    return self.execute_script(&arm.body, input);
                }
            }
        }
        ExecResult::status(0)
    }

    fn execute_simple(&mut self, command: &SimpleCommand, input: &[u8]) -> ExecResult {
        for (name, words) in &command.array_assignments {
            let mut values = BTreeMap::new();
            for (index, word) in words.iter().enumerate() {
                match self.expand_scalar(word) {
                    Ok(value) => {
                        values.insert(index, value);
                    }
                    Err(message) => return ExecResult::error(1, format!("isksh: {message}")),
                }
            }
            self.associative_arrays.remove(name);
            self.indexed_arrays.insert(name.clone(), values);
        }
        let mut assignments = Vec::new();
        for (name, word) in &command.assignments {
            match self.expand_scalar(word) {
                Ok(value) => assignments.push((name.clone(), value)),
                Err(message) => return ExecResult::error(1, format!("isksh: {message}")),
            }
        }

        if command.words.is_empty() {
            for (name, value) in assignments {
                if let Err(message) = self.set_assignment(&name, value, None) {
                    return ExecResult::error(1, message);
                }
            }
            return self.apply_redirections(command, input, ExecResult::status(0));
        }

        let conditional = command.words.first().and_then(Word::as_plain_literal) == Some("[[");
        let mut words = Vec::new();
        for word in &command.words {
            let expanded = if conditional {
                self.expand_scalar(word).map(|value| vec![value])
            } else {
                self.expand_word(word)
            };
            match expanded {
                Ok(mut fields) => words.append(&mut fields),
                Err(message) => return ExecResult::error(1, format!("isksh: {message}")),
            }
        }
        if words.is_empty() {
            return ExecResult::status(0);
        }

        let mut command_input = input.to_vec();
        for redirection in &command.redirections {
            if matches!(
                redirection.kind,
                RedirectionKind::HereDocument | RedirectionKind::HereDocumentStrip
            ) {
                let Some(document) = &redirection.here_document else {
                    return ExecResult::error(2, "isksh: missing here-document body");
                };
                if document.expand {
                    match self.expand_here_document(&document.body) {
                        Ok(body) => command_input = body.into_bytes(),
                        Err(message) => return ExecResult::error(1, format!("isksh: {message}")),
                    }
                } else {
                    command_input = document.body.as_bytes().to_vec();
                }
            } else if matches!(
                redirection.kind,
                RedirectionKind::Input | RedirectionKind::ReadWrite
            ) {
                let path = match self.redirection_path(&redirection.target) {
                    Ok(path) => path,
                    Err(message) => return ExecResult::error(1, message),
                };
                if redirection.kind == RedirectionKind::ReadWrite
                    && let Err(error) = OpenOptions::new()
                        .create(true)
                        .read(true)
                        .write(true)
                        .truncate(false)
                        .open(&path)
                {
                    return ExecResult::error(1, format!("isksh: {error}"));
                }
                match fs::read(path) {
                    Ok(bytes) => command_input = bytes,
                    Err(error) => return ExecResult::error(1, format!("isksh: {error}")),
                }
            }
        }

        let name = words.remove(0);
        if assignments.is_empty()
            && !self.expanding_aliases.contains(&name)
            && let Some(replacement) = self.aliases.get(&name).cloned()
        {
            let mut source = replacement;
            for argument in &words {
                source.push(' ');
                source.push_str(&shell_quote(argument));
            }
            self.expanding_aliases.push(name.clone());
            let result = self.execute_eval(&[source], &command_input);
            self.expanding_aliases.pop();
            return self.apply_redirections(command, &command_input, result);
        }
        let is_special = is_special_builtin(&name);
        let has_temporary_assignments = !assignments.is_empty();
        let saved_variables = if is_special || !has_temporary_assignments {
            Vec::new()
        } else {
            assignments
                .iter()
                .map(|(key, _)| (key.clone(), self.variables.get(key).cloned()))
                .collect()
        };
        if let Some((key, _)) = assignments.iter().find(|(key, _)| {
            !valid_assignment_name(key)
                || self
                    .variables
                    .get(key)
                    .is_some_and(|variable| variable.readonly)
        }) {
            return ExecResult::error(1, format!("isksh: {key}: invalid or readonly variable"));
        }
        for (key, value) in assignments {
            let inserted = self.set_assignment(&key, value, Some(true));
            debug_assert!(inserted.is_ok());
        }
        let previous_terminal_io = self.terminal_io;
        self.terminal_io &= command.redirections.is_empty() && command_input.is_empty();
        let mut result = if let Some(function) = self.functions.get(&name).cloned() {
            self.execute_function(&function, words, &command_input)
        } else if is_builtin(&name) {
            self.execute_builtin(&name, &words, &command_input)
        } else {
            self.execute_external(&name, &words, &command_input, self.terminal_io)
        };
        self.terminal_io = previous_terminal_io;
        for (name, previous) in saved_variables {
            if let Some(previous) = previous {
                self.variables.insert(name, previous);
            } else {
                self.variables.remove(&name);
            }
        }
        result = self.apply_redirections(command, &command_input, result);
        let mut substitutions = self.finish_process_substitutions();
        result.stdout.append(&mut substitutions.stdout);
        result.stderr.append(&mut substitutions.stderr);
        if substitutions.status != 0 && result.status == 0 {
            result.status = substitutions.status;
        }
        result
    }

    fn apply_redirections(
        &mut self,
        command: &SimpleCommand,
        _input: &[u8],
        mut result: ExecResult,
    ) -> ExecResult {
        let mut stdout_sink = OutputSink::Stdout;
        let mut stderr_sink = OutputSink::Stderr;
        for redirection in &command.redirections {
            let fd = redirection.fd.unwrap_or(match redirection.kind {
                RedirectionKind::Input
                | RedirectionKind::DuplicateInput
                | RedirectionKind::ReadWrite => 0,
                _ => 1,
            });
            match redirection.kind {
                RedirectionKind::Output
                | RedirectionKind::Clobber
                | RedirectionKind::Append
                | RedirectionKind::ReadWrite => {
                    if redirection.kind == RedirectionKind::ReadWrite && fd == 0 {
                        continue;
                    }
                    if !matches!(fd, 1 | 2) {
                        return ExecResult::error(1, "isksh: unsupported output file descriptor");
                    }
                    let path = match self.redirection_path(&redirection.target) {
                        Ok(path) => path,
                        Err(message) => return ExecResult::error(1, message),
                    };
                    let mut options = OpenOptions::new();
                    options.create(true).write(true).append(true);
                    if redirection.kind != RedirectionKind::Append
                        && redirection.kind != RedirectionKind::ReadWrite
                        && let Err(error) = fs::write(&path, [])
                    {
                        return ExecResult::error(1, format!("isksh: {error}"));
                    }
                    if redirection.kind == RedirectionKind::ReadWrite {
                        options.read(true);
                    }
                    if let Err(error) = options.open(&path) {
                        return ExecResult::error(1, format!("isksh: {error}"));
                    }
                    let sink = OutputSink::File(path);
                    if fd == 2 {
                        stderr_sink = sink;
                    } else {
                        stdout_sink = sink;
                    }
                }
                RedirectionKind::DuplicateOutput | RedirectionKind::DuplicateInput => {
                    let target = match self.expand_scalar(&redirection.target) {
                        Ok(target) => target,
                        Err(message) => return ExecResult::error(1, message),
                    };
                    if matches!(fd, 1 | 2) {
                        let sink = match target.as_str() {
                            "1" => stdout_sink.clone(),
                            "2" => stderr_sink.clone(),
                            "-" => OutputSink::Closed,
                            _ => {
                                return ExecResult::error(
                                    1,
                                    "isksh: unsupported file descriptor duplication",
                                );
                            }
                        };
                        if fd == 2 {
                            stderr_sink = sink;
                        } else {
                            stdout_sink = sink;
                        }
                    } else if fd != 0 || target != "-" {
                        return ExecResult::error(
                            1,
                            "isksh: unsupported file descriptor duplication",
                        );
                    }
                }
                RedirectionKind::HereDocument | RedirectionKind::HereDocumentStrip => {}
                RedirectionKind::Input => {}
            }
        }
        let stdout = std::mem::take(&mut result.stdout);
        let stderr = std::mem::take(&mut result.stderr);
        if let Err(error) = write_output_sink(
            &stdout_sink,
            &stdout,
            &mut result.stdout,
            &mut result.stderr,
        ) {
            return ExecResult::error(1, format!("isksh: {error}"));
        }
        if let Err(error) = write_output_sink(
            &stderr_sink,
            &stderr,
            &mut result.stdout,
            &mut result.stderr,
        ) {
            return ExecResult::error(1, format!("isksh: {error}"));
        }
        result
    }

    fn execute_function(
        &mut self,
        body: &Command,
        arguments: Vec<String>,
        input: &[u8],
    ) -> ExecResult {
        let old_positional = std::mem::replace(&mut self.positional, arguments);
        self.function_depth += 1;
        self.local_scopes.push(LocalScope::default());
        let mut result = self.execute_command(body, input);
        let scope = self.local_scopes.pop().expect("function scope exists");
        restore_map(&mut self.variables, scope.variables);
        restore_map(&mut self.indexed_arrays, scope.indexed_arrays);
        restore_map(&mut self.associative_arrays, scope.associative_arrays);
        self.function_depth -= 1;
        self.positional = old_positional;
        if let Flow::Return(status) = result.flow {
            result.status = status;
            result.flow = Flow::None;
        }
        result
    }

    fn execute_external(
        &self,
        name: &str,
        arguments: &[String],
        input: &[u8],
        terminal_io: bool,
    ) -> ExecResult {
        let resolved_name = self.resolve_external_name(name);
        let mut process = platform_command(&resolved_name, arguments);
        process.current_dir(&self.cwd).env_clear();
        for (name, variable) in &self.variables {
            if variable.exported {
                process.env(name, &variable.value);
            }
        }
        if terminal_io {
            process
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
            return match process.status() {
                Ok(status) => ExecResult::status(exit_status(&status)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    ExecResult::error(127, format!("isksh: {name}: command not found"))
                }
                Err(error) => ExecResult::error(126, format!("isksh: {name}: {error}")),
            };
        }
        process
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = match process.spawn() {
            Ok(child) => child,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return ExecResult::error(127, format!("isksh: {name}: command not found"));
            }
            Err(error) => return ExecResult::error(126, format!("isksh: {name}: {error}")),
        };
        let stdin_writer = child.stdin.take().map(|mut stdin| {
            let input = input.to_vec();
            std::thread::spawn(move || stdin.write_all(&input))
        });
        let output = child.wait_with_output();
        if let Some(writer) = stdin_writer {
            let _ = writer.join();
        }
        finish_external(name, output)
    }

    fn execute_builtin(&mut self, name: &str, args: &[String], input: &[u8]) -> ExecResult {
        match name {
            ":" | "true" | "wait" => ExecResult::status(0),
            "false" => ExecResult::status(1),
            "echo" => {
                let newline = args.first().map(String::as_str) != Some("-n");
                let start = usize::from(!newline);
                let mut value = args[start..].join(" ").into_bytes();
                if newline {
                    value.push(b'\n');
                }
                ExecResult {
                    stdout: value,
                    ..ExecResult::status(0)
                }
            }
            "printf" => builtin_printf(args),
            "pwd" => {
                let mut value = self.cwd.to_string_lossy().into_owned().into_bytes();
                value.push(b'\n');
                ExecResult {
                    stdout: value,
                    ..ExecResult::status(0)
                }
            }
            "cd" => self.builtin_cd(args),
            "export" => self.builtin_export(args, false),
            "readonly" => self.builtin_export(args, true),
            "unset" => self.builtin_unset(args),
            "set" => self.builtin_set(args),
            "shift" => self.builtin_shift(args),
            "exit" => flow_status(args, Flow::Exit, self.last_status),
            "return" => {
                if self.function_depth == 0 {
                    ExecResult::error(1, "isksh: return: not in a function")
                } else {
                    flow_status(args, Flow::Return, self.last_status)
                }
            }
            "break" => self.loop_flow(args, true),
            "continue" => self.loop_flow(args, false),
            "eval" => self.execute_eval(args, input),
            "." | "source" => self.builtin_dot(args, input),
            "declare" | "typeset" | "local" => self.builtin_declare(name, args),
            "shopt" => self.builtin_shopt(args),
            "type" => self.builtin_type(args),
            "mapfile" | "readarray" => self.builtin_mapfile(args, input),
            "[[" => self.builtin_double_bracket(args),
            "exec" => {
                if args.is_empty() {
                    ExecResult::status(0)
                } else {
                    let mut result = if is_builtin(&args[0]) {
                        self.execute_builtin(&args[0], &args[1..], input)
                    } else {
                        self.execute_external(
                            &args[0],
                            &args[1..],
                            input,
                            self.terminal_io && input.is_empty(),
                        )
                    };
                    if result.flow == Flow::None {
                        result.flow = Flow::Exit(result.status);
                    }
                    result
                }
            }
            "command" => self.builtin_command(args, input),
            "read" => self.builtin_read(args, input),
            "test" => builtin_test(args),
            "[" => {
                if args.last().map(String::as_str) != Some("]") {
                    ExecResult::error(2, "isksh: [: missing ]")
                } else {
                    builtin_test(&args[..args.len() - 1])
                }
            }
            "alias" => self.builtin_alias(args),
            "unalias" => self.builtin_unalias(args),
            "getopts" => self.builtin_getopts(args),
            "times" => ExecResult {
                stdout: b"0m0.000s 0m0.000s\n0m0.000s 0m0.000s\n".to_vec(),
                ..ExecResult::status(0)
            },
            "hash" => ExecResult::status(0),
            "trap" | "umask" => {
                ExecResult::error(2, format!("isksh: {name}: not supported on this release"))
            }
            _ => ExecResult::error(127, format!("isksh: {name}: unsupported builtin")),
        }
    }

    fn builtin_cd(&mut self, args: &[String]) -> ExecResult {
        let target = args
            .first()
            .cloned()
            .or_else(|| self.value_of("HOME"))
            .unwrap_or(".".into());
        let path = self.resolve_path(&target);
        match fs::canonicalize(path) {
            Ok(path) if path.is_dir() => {
                let previous = self.cwd.to_string_lossy().into_owned();
                self.cwd = path;
                let current = self.cwd.to_string_lossy().into_owned();
                let _ = self.set_variable("OLDPWD", previous, Some(true), false);
                let _ = self.set_variable("PWD", current, Some(true), false);
                ExecResult::status(0)
            }
            Ok(_) => ExecResult::error(1, format!("isksh: cd: {target}: not a directory")),
            Err(error) => ExecResult::error(1, format!("isksh: cd: {target}: {error}")),
        }
    }

    fn builtin_declare(&mut self, command: &str, args: &[String]) -> ExecResult {
        if command == "local" && self.function_depth == 0 {
            return ExecResult::error(1, "isksh: local: can only be used in a function");
        }
        let mut indexed = false;
        let mut associative = false;
        let mut print = false;
        let mut global = false;
        let mut index = 0;
        while let Some(option) = args.get(index).filter(|value| value.starts_with('-')) {
            for flag in option[1..].chars() {
                match flag {
                    'a' => indexed = true,
                    'A' => associative = true,
                    'p' => print = true,
                    'g' => global = true,
                    _ => {
                        return ExecResult::error(
                            2,
                            format!("isksh: {command}: -{flag}: unsupported option"),
                        );
                    }
                }
            }
            index += 1;
        }
        let mut output = String::new();
        for argument in &args[index..] {
            let (name, value) = argument
                .split_once('=')
                .map_or((argument.as_str(), None), |(name, value)| {
                    (name, Some(value))
                });
            if !valid_variable_name(name) {
                return ExecResult::error(1, format!("isksh: {command}: {name}: invalid name"));
            }
            if (command == "local" || command != "local" && self.function_depth > 0 && !global)
                && let Some(scope) = self.local_scopes.last_mut()
            {
                scope
                    .variables
                    .entry(name.to_string())
                    .or_insert_with(|| self.variables.get(name).cloned());
                scope
                    .indexed_arrays
                    .entry(name.to_string())
                    .or_insert_with(|| self.indexed_arrays.get(name).cloned());
                scope
                    .associative_arrays
                    .entry(name.to_string())
                    .or_insert_with(|| self.associative_arrays.get(name).cloned());
            }
            if print {
                if let Some(values) = self.indexed_arrays.get(name) {
                    output.push_str(&format_array_declaration(
                        "declare -a",
                        name,
                        values.iter().map(|(k, v)| (k.to_string(), v.as_str())),
                    ));
                } else if let Some(values) = self.associative_arrays.get(name) {
                    output.push_str(&format_array_declaration(
                        "declare -A",
                        name,
                        values.iter().map(|(k, v)| (k.clone(), v.as_str())),
                    ));
                } else if let Some(value) = self.value_of(name) {
                    output.push_str(&format!("declare -- {name}={}\n", shell_quote(&value)));
                } else {
                    return ExecResult::status(1);
                }
            } else if associative {
                self.indexed_arrays.remove(name);
                self.associative_arrays.entry(name.to_string()).or_default();
            } else if indexed {
                self.associative_arrays.remove(name);
                self.indexed_arrays.entry(name.to_string()).or_default();
            } else if let Some(value) = value
                && let Err(message) = self.set_variable(name, value.to_string(), None, false)
            {
                return ExecResult::error(1, message);
            }
        }
        ExecResult {
            stdout: output.into_bytes(),
            ..ExecResult::status(0)
        }
    }

    fn builtin_shopt(&mut self, args: &[String]) -> ExecResult {
        let mut mode = None;
        let mut quiet = false;
        let mut names = Vec::new();
        for argument in args {
            match argument.as_str() {
                "-s" => mode = Some(true),
                "-u" => mode = Some(false),
                "-q" => quiet = true,
                "-p" => {}
                value if value.starts_with('-') => {
                    return ExecResult::error(2, "isksh: shopt: unsupported option");
                }
                _ => names.push(argument.as_str()),
            }
        }
        const OPTIONS: &[&str] = &["dotglob", "extglob", "globstar", "nocasematch", "nullglob"];
        if names.iter().any(|name| !OPTIONS.contains(name)) {
            return ExecResult::error(1, "isksh: shopt: invalid shell option name");
        }
        if let Some(enabled) = mode {
            for name in &names {
                if enabled {
                    self.shell_options.insert((*name).to_string());
                } else {
                    self.shell_options.remove(*name);
                }
            }
        }
        let selected: Vec<_> = if names.is_empty() {
            OPTIONS.to_vec()
        } else {
            names
        };
        let all_enabled = selected
            .iter()
            .all(|name| self.shell_options.contains(*name));
        let stdout = if quiet {
            Vec::new()
        } else {
            selected
                .into_iter()
                .map(|name| {
                    format!(
                        "shopt -{} {name}\n",
                        if self.shell_options.contains(name) {
                            's'
                        } else {
                            'u'
                        }
                    )
                })
                .collect::<String>()
                .into_bytes()
        };
        ExecResult {
            status: i32::from(!all_enabled),
            stdout,
            ..ExecResult::status(0)
        }
    }

    fn builtin_type(&self, args: &[String]) -> ExecResult {
        let terse = args.first().map(String::as_str) == Some("-t");
        let names = if terse { &args[1..] } else { args };
        let mut output = String::new();
        for name in names {
            let (kind, detail) = if self.aliases.contains_key(name) {
                ("alias", format!("{name} is an alias"))
            } else if self.functions.contains_key(name) {
                ("function", format!("{name} is a function"))
            } else if is_builtin(name) {
                ("builtin", format!("{name} is a shell builtin"))
            } else {
                let path = self.resolve_command_file(name);
                if !path.is_file() {
                    return ExecResult::status(1);
                }
                ("file", format!("{name} is {}", path.display()))
            };
            output.push_str(if terse { kind } else { &detail });
            output.push('\n');
        }
        ExecResult {
            stdout: output.into_bytes(),
            ..ExecResult::status(0)
        }
    }

    fn builtin_mapfile(&mut self, args: &[String], input: &[u8]) -> ExecResult {
        let mut trim = false;
        let mut index = 0;
        while args.get(index).is_some_and(|arg| arg.starts_with('-')) {
            match args[index].as_str() {
                "-t" => trim = true,
                "--" => {
                    index += 1;
                    break;
                }
                _ => return ExecResult::error(2, "isksh: mapfile: unsupported option"),
            }
            index += 1;
        }
        let name = args.get(index).map(String::as_str).unwrap_or("MAPFILE");
        if !valid_variable_name(name) {
            return ExecResult::error(1, "isksh: mapfile: invalid array name");
        }
        let text = match std::str::from_utf8(input) {
            Ok(value) => value,
            Err(_) => return ExecResult::error(1, "isksh: mapfile: input is not valid UTF-8"),
        };
        let values = text
            .split_inclusive('\n')
            .enumerate()
            .map(|(index, line)| {
                let value = if trim {
                    line.trim_end_matches('\n').trim_end_matches('\r')
                } else {
                    line
                };
                (index, value.to_string())
            })
            .collect();
        self.associative_arrays.remove(name);
        self.indexed_arrays.insert(name.to_string(), values);
        ExecResult::status(0)
    }

    fn builtin_double_bracket(&self, args: &[String]) -> ExecResult {
        if args.last().map(String::as_str) != Some("]]") {
            return ExecResult::error(2, "isksh: [[: missing ]]");
        }
        match evaluate_conditional(&args[..args.len() - 1], self) {
            Ok(value) => ExecResult::status(i32::from(!value)),
            Err(message) => ExecResult::error(2, format!("isksh: [[: {message}")),
        }
    }

    fn builtin_command(&mut self, args: &[String], input: &[u8]) -> ExecResult {
        let mut index = 0;
        let mut describe = false;
        while let Some(option) = args.get(index).map(String::as_str) {
            match option {
                "-v" | "-V" => {
                    describe = true;
                    index += 1;
                }
                "--" => {
                    index += 1;
                    break;
                }
                _ => break,
            }
        }
        let Some(name) = args.get(index) else {
            return ExecResult::status(0);
        };
        if describe {
            let description = if let Some(alias) = self.aliases.get(name) {
                Some(format!("alias {name}='{alias}'"))
            } else if is_builtin(name) || self.functions.contains_key(name) {
                Some(name.clone())
            } else {
                let path = self.resolve_command_file(name);
                path.is_file().then(|| path.to_string_lossy().into_owned())
            };
            return match description {
                Some(mut value) => {
                    value.push('\n');
                    ExecResult {
                        stdout: value.into_bytes(),
                        ..ExecResult::status(0)
                    }
                }
                None => ExecResult::status(1),
            };
        }
        if is_builtin(name) {
            self.execute_builtin(name, &args[index + 1..], input)
        } else {
            self.execute_external(
                name,
                &args[index + 1..],
                input,
                self.terminal_io && input.is_empty(),
            )
        }
    }

    fn builtin_export(&mut self, args: &[String], readonly: bool) -> ExecResult {
        if args.is_empty() {
            let mut names: Vec<_> = self
                .variables
                .iter()
                .filter(|(_, value)| {
                    if readonly {
                        value.readonly
                    } else {
                        value.exported
                    }
                })
                .collect();
            names.sort_by_key(|(name, _)| *name);
            let declaration = if readonly { "readonly" } else { "export" };
            let stdout = names
                .into_iter()
                .map(|(name, value)| {
                    format!(
                        "{declaration} {name}='{}'\n",
                        value.value.replace('\'', "'\\''")
                    )
                })
                .collect::<String>()
                .into_bytes();
            return ExecResult {
                stdout,
                ..ExecResult::status(0)
            };
        }
        for argument in args {
            let (name, value) = argument
                .split_once('=')
                .map_or((argument.as_str(), None), |(name, value)| {
                    (name, Some(value.to_string()))
                });
            if !valid_variable_name(name) {
                return ExecResult::error(1, format!("isksh: {name}: invalid variable name"));
            }
            if let Err(message) = self.set_variable(
                name,
                value.unwrap_or_else(|| self.value_of(name).unwrap_or_default()),
                (!readonly).then_some(true),
                readonly,
            ) {
                return ExecResult::error(1, message);
            }
            let variable = self.variables.get_mut(name).expect("variable was inserted");
            if readonly {
                variable.readonly = true;
            } else {
                variable.exported = true;
            }
        }
        ExecResult::status(0)
    }

    fn builtin_unset(&mut self, args: &[String]) -> ExecResult {
        for name in args {
            if self.variables.get(name).is_some_and(|value| value.readonly) {
                return ExecResult::error(1, format!("isksh: unset: {name}: readonly variable"));
            }
            self.variables.remove(name);
            self.functions.remove(name);
        }
        ExecResult::status(0)
    }

    fn builtin_set(&mut self, args: &[String]) -> ExecResult {
        if args.is_empty() {
            let mut variables: Vec<_> = self.variables.iter().collect();
            variables.sort_by_key(|(name, _)| *name);
            let stdout = variables
                .into_iter()
                .map(|(name, value)| format!("{name}='{}'\n", value.value.replace('\'', "'\\''")))
                .collect::<String>()
                .into_bytes();
            return ExecResult {
                stdout,
                ..ExecResult::status(0)
            };
        }
        if args.first().map(String::as_str) == Some("--") {
            self.positional = args[1..].to_vec();
            self.getopts_offset = 1;
            let _ = self.set_variable("OPTIND", "1".into(), None, false);
            ExecResult::status(0)
        } else {
            ExecResult::error(2, "isksh: set: shell options are not implemented")
        }
    }

    fn builtin_shift(&mut self, args: &[String]) -> ExecResult {
        let count = args
            .first()
            .map_or(Ok(1usize), |value| value.parse::<usize>())
            .unwrap_or(usize::MAX);
        if count > self.positional.len() {
            ExecResult::error(1, "isksh: shift: count exceeds positional parameters")
        } else {
            self.positional.drain(..count);
            ExecResult::status(0)
        }
    }

    fn loop_flow(&self, args: &[String], is_break: bool) -> ExecResult {
        if self.loop_depth == 0 {
            return ExecResult::error(1, "isksh: loop control used outside a loop");
        }
        let level = args
            .first()
            .and_then(|value| value.parse().ok())
            .unwrap_or(1);
        let mut result = ExecResult::status(0);
        result.flow = if is_break {
            Flow::Break(level)
        } else {
            Flow::Continue(level)
        };
        result
    }

    fn execute_eval(&mut self, args: &[String], input: &[u8]) -> ExecResult {
        match parse(&args.join(" ")) {
            Ok(script) => self.execute_script(&script, input),
            Err(error) => ExecResult::error(2, format!("isksh: {error}")),
        }
    }

    fn builtin_dot(&mut self, args: &[String], input: &[u8]) -> ExecResult {
        let Some(name) = args.first() else {
            return ExecResult::error(2, "isksh: .: filename required");
        };
        let path = self.resolve_command_file(name);
        match fs::read_to_string(path) {
            Ok(source) => self.execute_eval(&[source], input),
            Err(error) => ExecResult::error(1, format!("isksh: .: {name}: {error}")),
        }
    }

    fn builtin_read(&mut self, args: &[String], input: &[u8]) -> ExecResult {
        let input = match std::str::from_utf8(input) {
            Ok(input) => input,
            Err(error) => {
                return ExecResult::error(
                    1,
                    format!(
                        "isksh: read: input is not valid UTF-8 at byte {}",
                        error.valid_up_to()
                    ),
                );
            }
        };
        let line = input.lines().next().unwrap_or_default().to_string();
        let names = if args.is_empty() {
            vec!["REPLY".to_string()]
        } else {
            args.to_vec()
        };
        let fields: Vec<_> = line.split_whitespace().collect();
        for (index, name) in names.iter().enumerate() {
            let value = if index + 1 == names.len() {
                fields.get(index..).unwrap_or_default().join(" ")
            } else {
                fields.get(index).copied().unwrap_or_default().to_string()
            };
            if let Err(message) = self.set_variable(name, value, None, false) {
                return ExecResult::error(1, message);
            }
        }
        ExecResult::status(i32::from(input.is_empty()))
    }

    fn builtin_getopts(&mut self, args: &[String]) -> ExecResult {
        if args.len() < 2 || !valid_variable_name(&args[1]) {
            return ExecResult::error(2, "isksh: getopts: usage: getopts optstring name [arg ...]");
        }
        let option_spec = &args[0];
        let silent = option_spec.starts_with(':');
        let option_spec = option_spec.strip_prefix(':').unwrap_or(option_spec);
        let operands = if args.len() > 2 {
            args[2..].to_vec()
        } else {
            self.positional.clone()
        };
        let mut operand_index = self
            .value_of("OPTIND")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1)
            .max(1);
        if operand_index > operands.len() {
            return ExecResult::status(1);
        }
        let operand = &operands[operand_index - 1];
        if operand == "--" {
            operand_index += 1;
            let _ = self.set_variable("OPTIND", operand_index.to_string(), None, false);
            self.getopts_offset = 1;
            return ExecResult::status(1);
        }
        if !operand.starts_with('-') || operand == "-" {
            return ExecResult::status(1);
        }
        let option_chars: Vec<char> = operand.chars().collect();
        if self.getopts_offset >= option_chars.len() {
            self.getopts_offset = 1;
            operand_index += 1;
            let _ = self.set_variable("OPTIND", operand_index.to_string(), None, false);
            return self.builtin_getopts(args);
        }
        let option = option_chars[self.getopts_offset];
        self.getopts_offset += 1;
        let spec_chars: Vec<char> = option_spec.chars().collect();
        let spec_index = spec_chars.iter().position(|candidate| *candidate == option);
        let requires_argument =
            spec_index.is_some_and(|index| spec_chars.get(index + 1) == Some(&':'));
        let mut optarg = None;
        let result_option;
        if spec_index.is_none() {
            result_option = '?';
            if silent {
                optarg = Some(option.to_string());
            }
        } else if requires_argument {
            if self.getopts_offset < option_chars.len() {
                optarg = Some(option_chars[self.getopts_offset..].iter().collect());
                self.getopts_offset = 1;
                operand_index += 1;
            } else if operand_index < operands.len() {
                optarg = Some(operands[operand_index].clone());
                self.getopts_offset = 1;
                operand_index += 2;
            } else {
                result_option = if silent { ':' } else { '?' };
                if silent {
                    optarg = Some(option.to_string());
                }
                self.getopts_offset = 1;
                operand_index += 1;
                let _ = self.set_variable("OPTIND", operand_index.to_string(), None, false);
                let _ = self.set_variable(&args[1], result_option.to_string(), None, false);
                if let Some(value) = optarg {
                    let _ = self.set_variable("OPTARG", value, None, false);
                }
                return ExecResult::status(0);
            }
            result_option = option;
        } else {
            result_option = option;
            if self.getopts_offset >= option_chars.len() {
                self.getopts_offset = 1;
                operand_index += 1;
            }
        }
        let _ = self.set_variable("OPTIND", operand_index.to_string(), None, false);
        let _ = self.set_variable(&args[1], result_option.to_string(), None, false);
        if let Some(value) = optarg {
            let _ = self.set_variable("OPTARG", value, None, false);
        } else {
            self.variables.remove("OPTARG");
        }
        ExecResult::status(0)
    }

    fn builtin_alias(&mut self, args: &[String]) -> ExecResult {
        if args.is_empty() {
            let mut aliases: Vec<_> = self.aliases.iter().collect();
            aliases.sort_by_key(|(name, _)| *name);
            let stdout = aliases
                .into_iter()
                .map(|(name, value)| format!("alias {name}='{}'\n", value.replace('\'', "'\\''")))
                .collect::<String>()
                .into_bytes();
            return ExecResult {
                stdout,
                ..ExecResult::status(0)
            };
        }
        for argument in args {
            if let Some((name, value)) = argument.split_once('=') {
                self.aliases.insert(name.to_string(), value.to_string());
            } else if !self.aliases.contains_key(argument) {
                return ExecResult::error(1, format!("isksh: alias: {argument}: not found"));
            }
        }
        ExecResult::status(0)
    }

    fn builtin_unalias(&mut self, args: &[String]) -> ExecResult {
        for name in args {
            self.aliases.remove(name);
        }
        ExecResult::status(0)
    }

    fn expand_word(&mut self, word: &Word) -> Result<Vec<String>, String> {
        self.expand_word_context(word, true, true)
    }

    fn expand_word_context(
        &mut self,
        word: &Word,
        allow_split: bool,
        allow_glob: bool,
    ) -> Result<Vec<String>, String> {
        if matches!(word.parts.as_slice(), [WordPart::Parameter { expression, quoted: true }] if expression == "@")
        {
            return Ok(self.positional.clone());
        }
        let mut value = String::new();
        let mut split = false;
        let mut globbable = false;
        for (index, part) in word.parts.iter().enumerate() {
            match part {
                WordPart::Literal {
                    value: part,
                    quoted,
                } => {
                    if index == 0
                        && !quoted
                        && part.starts_with('~')
                        && (part.len() == 1 || part.as_bytes().get(1) == Some(&b'/'))
                    {
                        value.push_str(&self.value_of("HOME").unwrap_or("~".into()));
                        value.push_str(&part[1..]);
                    } else {
                        value.push_str(part);
                    }
                    globbable |= !quoted && part.contains(['*', '?', '[']);
                }
                WordPart::Parameter { expression, quoted } => {
                    value.push_str(&self.expand_parameter(expression)?);
                    split |= !quoted;
                    globbable |= !quoted;
                }
                WordPart::CommandSubstitution { source, quoted } => {
                    let mut child = self.clone();
                    child.terminal_io = false;
                    let result = child.run(source, &[]);
                    if result.status != 0 && !result.stderr.is_empty() {
                        // Command substitution preserves stdout even when the command fails.
                    }
                    let output = String::from_utf8(result.stdout).map_err(|_| {
                        "command substitution produced non-UTF-8 output".to_string()
                    })?;
                    value.push_str(output.trim_end_matches('\n'));
                    split |= !quoted;
                    globbable |= !quoted;
                }
                WordPart::Arithmetic { expression, quoted } => {
                    value.push_str(&self.evaluate_arithmetic(expression)?.to_string());
                    split |= !quoted;
                }
                WordPart::ProcessSubstitution { source, input } => {
                    let id = PROCESS_SUBSTITUTION_ID.fetch_add(1, Ordering::Relaxed);
                    let path =
                        std::env::temp_dir().join(format!("isksh-{}-{id}.tmp", std::process::id()));
                    if *input {
                        let mut child = self.clone();
                        child.terminal_io = false;
                        let result = child.run(source, &[]);
                        fs::write(&path, result.stdout).map_err(io_error_string)?;
                        self.pending_process_substitutions
                            .push(PendingProcessSubstitution {
                                path: path.clone(),
                                source: None,
                            });
                    } else {
                        fs::write(&path, []).map_err(io_error_string)?;
                        self.pending_process_substitutions
                            .push(PendingProcessSubstitution {
                                path: path.clone(),
                                source: Some(source.clone()),
                            });
                    }
                    value.push_str(&path.to_string_lossy());
                }
            }
        }
        let fields = if allow_split && split {
            let ifs = self.value_of("IFS").unwrap_or_else(|| " \t\n".into());
            value
                .split(|ch| ifs.contains(ch))
                .filter(|field| !field.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        } else {
            vec![value]
        };
        if !allow_glob || !globbable {
            return Ok(fields);
        }
        let mut expanded = Vec::new();
        for field in fields {
            if !field.contains(['*', '?', '[']) {
                expanded.push(field);
                continue;
            }
            let absolute_pattern = self.resolve_path(&field).to_string_lossy().into_owned();
            let options = MatchOptions {
                case_sensitive: !cfg!(windows),
                require_literal_separator: true,
                require_literal_leading_dot: true,
            };
            let mut matches = glob_with(&absolute_pattern, options)
                .map_err(|error| error.to_string())?
                .filter_map(Result::ok)
                .map(|path| {
                    path.strip_prefix(&self.cwd)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .replace('\\', "/")
                })
                .collect::<Vec<_>>();
            matches.sort();
            if matches.is_empty() {
                expanded.push(field);
            } else {
                expanded.extend(matches);
            }
        }
        Ok(expanded)
    }

    fn expand_scalar(&mut self, word: &Word) -> Result<String, String> {
        let fields = self.expand_word_context(word, false, false)?;
        Ok(fields.join(" "))
    }

    fn expand_parameter(&mut self, expression: &str) -> Result<String, String> {
        if let Some(reference) = expression.strip_prefix('!')
            && let Some((name, subscript)) = parse_array_reference(reference)
            && matches!(subscript, "@" | "*")
        {
            return Ok(self.array_keys(name).join(" "));
        }
        if let Some(reference) = expression.strip_prefix('#')
            && let Some((name, subscript)) = parse_array_reference(reference)
        {
            return Ok(if matches!(subscript, "@" | "*") {
                self.array_values(name).len().to_string()
            } else {
                self.array_value(name, subscript)
                    .unwrap_or_default()
                    .chars()
                    .count()
                    .to_string()
            });
        }
        if let Some((name, subscript)) = parse_array_reference(expression) {
            return Ok(if matches!(subscript, "@" | "*") {
                self.array_values(name).join(" ")
            } else {
                self.array_value(name, subscript).unwrap_or_default()
            });
        }
        if let Some(name) = expression.strip_prefix('#')
            && valid_variable_name(name)
        {
            return Ok(self
                .value_of(name)
                .unwrap_or_default()
                .chars()
                .count()
                .to_string());
        }
        for operator in [":-", ":=", ":+", ":?", "-", "=", "+", "?"] {
            if let Some((name, word)) = expression.split_once(operator) {
                if !valid_variable_name(name) {
                    break;
                }
                let current = self.value_of(name);
                let colon = operator.starts_with(':');
                let missing = current.is_none() || colon && current.as_deref() == Some("");
                let operation = operator.trim_start_matches(':');
                return if operation == "-" {
                    Ok(if missing {
                        word.to_string()
                    } else {
                        current.unwrap_or_default()
                    })
                } else if operation == "+" {
                    Ok(if missing {
                        String::new()
                    } else {
                        word.to_string()
                    })
                } else if operation == "=" {
                    if missing {
                        self.set_variable(name, word.to_string(), None, false)?;
                        Ok(word.to_string())
                    } else {
                        Ok(current.unwrap_or_default())
                    }
                } else if missing {
                    Err(if word.is_empty() {
                        format!("{name}: parameter is unset or null")
                    } else {
                        word.to_string()
                    })
                } else {
                    Ok(current.unwrap_or_default())
                };
            }
        }
        Ok(match expression {
            "?" => self.last_status.to_string(),
            "#" => self.positional.len().to_string(),
            "$" => std::process::id().to_string(),
            "@" => self.positional.join(" "),
            "*" => self.positional.join(
                &self
                    .value_of("IFS")
                    .unwrap_or_else(|| " ".into())
                    .chars()
                    .next()
                    .unwrap_or(' ')
                    .to_string(),
            ),
            "0" => self.name.clone(),
            value if value.len() == 1 && value.as_bytes()[0].is_ascii_digit() => value
                .parse::<usize>()
                .ok()
                .and_then(|index| self.positional.get(index.saturating_sub(1)))
                .cloned()
                .unwrap_or_default(),
            name => self.value_of(name).unwrap_or_default(),
        })
    }

    fn expand_prompt_escapes(&self, value: &str) -> String {
        let username = self
            .value_of("USER")
            .or_else(|| self.value_of("USERNAME"))
            .unwrap_or_default();
        let hostname = self
            .value_of("HOSTNAME")
            .or_else(|| self.value_of("COMPUTERNAME"))
            .unwrap_or_default();
        let cwd = self.cwd.to_string_lossy().into_owned();
        let home = self
            .value_of("HOME")
            .or_else(|| self.value_of("USERPROFILE"));
        let display_cwd = home
            .filter(|home| {
                cwd == *home || cwd.starts_with(&format!("{home}{}", std::path::MAIN_SEPARATOR))
            })
            .map_or_else(|| cwd.clone(), |home| format!("~{}", &cwd[home.len()..]));
        let directory = if display_cwd == "~" {
            "~".to_string()
        } else {
            Path::new(&display_cwd)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or(display_cwd.clone())
        };
        let shell_name = Path::new(&self.name)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.name.clone());
        let mut chars = value.chars().peekable();
        let mut output = String::new();
        while let Some(ch) = chars.next() {
            if ch != '\\' {
                output.push(ch);
                continue;
            }
            let Some(escape) = chars.next() else {
                output.push('\\');
                break;
            };
            match escape {
                'a' => output.push('\x07'),
                'e' => output.push('\x1b'),
                'h' => output.push_str(hostname.split('.').next().unwrap_or_default()),
                'H' => output.push_str(&hostname),
                'j' => output.push('0'),
                'n' => output.push('\n'),
                'r' => output.push('\r'),
                's' => output.push_str(&shell_name),
                'u' => output.push_str(&username),
                'v' | 'V' => output.push_str(env!("CARGO_PKG_VERSION")),
                'w' => output.push_str(&display_cwd),
                'W' => output.push_str(&directory),
                '!' | '#' => output.push('1'),
                '$' => output.push(if username.eq_ignore_ascii_case("root") {
                    '#'
                } else {
                    '$'
                }),
                '\\' => output.push('\\'),
                '[' | ']' => {}
                first if first.is_ascii_digit() && first < '8' => {
                    let mut octal = first.to_string();
                    while octal.len() < 3
                        && chars
                            .peek()
                            .is_some_and(|next| next.is_ascii_digit() && *next < '8')
                    {
                        octal.push(chars.next().expect("peeked octal digit"));
                    }
                    if let Ok(byte) = u8::from_str_radix(&octal, 8) {
                        output.push(char::from(byte));
                    }
                }
                other => {
                    output.push('\\');
                    output.push(other);
                }
            }
        }
        output
    }

    fn expand_here_document(&mut self, body: &str) -> Result<String, String> {
        let chars: Vec<char> = body.chars().collect();
        let mut output = String::new();
        let mut index = 0usize;
        while index < chars.len() {
            if chars[index] == '\\' {
                if let Some(next) = chars.get(index + 1).copied() {
                    if matches!(next, '$' | '`' | '\\') {
                        output.push(next);
                        index += 2;
                        continue;
                    }
                    if next == '\n' {
                        index += 2;
                        continue;
                    }
                }
                output.push('\\');
                index += 1;
                continue;
            }
            if chars[index] != '$' {
                output.push(chars[index]);
                index += 1;
                continue;
            }
            index += 1;
            if chars.get(index) == Some(&'{') {
                index += 1;
                let start = index;
                while chars.get(index) != Some(&'}') && index < chars.len() {
                    index += 1;
                }
                if index >= chars.len() {
                    return Err("unclosed parameter expansion in here-document".into());
                }
                let expression: String = chars[start..index].iter().collect();
                output.push_str(&self.expand_parameter(&expression)?);
                index += 1;
                continue;
            }
            if chars.get(index) == Some(&'(') {
                let arithmetic = chars.get(index + 1) == Some(&'(');
                index += if arithmetic { 2 } else { 1 };
                let start = index;
                let mut depth = 1usize;
                while index < chars.len() {
                    if chars[index] == '(' {
                        depth += 1;
                    } else if chars[index] == ')' {
                        if arithmetic {
                            if depth == 1 && chars.get(index + 1) == Some(&')') {
                                break;
                            }
                            if depth > 1 {
                                depth -= 1;
                            }
                        } else {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                    }
                    index += 1;
                }
                if index >= chars.len() {
                    return Err("unclosed substitution in here-document".into());
                }
                let expression: String = chars[start..index].iter().collect();
                if arithmetic {
                    output.push_str(&self.evaluate_arithmetic(&expression)?.to_string());
                    index += 2;
                } else {
                    let mut child = self.clone();
                    child.terminal_io = false;
                    let result = child.run(&expression, &[]);
                    let text = String::from_utf8(result.stdout).map_err(|_| {
                        "command substitution produced non-UTF-8 output".to_string()
                    })?;
                    output.push_str(text.trim_end_matches('\n'));
                    index += 1;
                }
                continue;
            }
            let start = index;
            if chars
                .get(index)
                .is_some_and(|ch| ch.is_ascii_alphabetic() || *ch == '_')
            {
                index += 1;
                while chars
                    .get(index)
                    .is_some_and(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
                {
                    index += 1;
                }
            } else if chars
                .get(index)
                .is_some_and(|ch| matches!(ch, '?' | '#' | '$' | '@' | '*' | '0'..='9'))
            {
                index += 1;
            } else {
                output.push('$');
                continue;
            }
            let expression: String = chars[start..index].iter().collect();
            output.push_str(&self.expand_parameter(&expression)?);
        }
        Ok(output)
    }

    fn evaluate_arithmetic(&self, expression: &str) -> Result<i64, String> {
        ArithmeticParser::new(expression, self).parse()
    }

    fn redirection_path(&mut self, word: &Word) -> Result<PathBuf, String> {
        let fields = self.expand_word(word)?;
        if fields.len() != 1 {
            return Err("ambiguous redirect".into());
        }
        Ok(self.resolve_path(&fields[0]))
    }

    fn resolve_path(&self, value: &str) -> PathBuf {
        let path = Path::new(value);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cwd.join(path)
        }
    }

    fn resolve_command_file(&self, name: &str) -> PathBuf {
        if name.contains(['/', '\\']) {
            return self.resolve_path(name);
        }
        self.value_of("PATH")
            .unwrap_or_default()
            .split(if cfg!(windows) { ';' } else { ':' })
            .map(|directory| self.resolve_path(directory).join(name))
            .find(|path| path.is_file())
            .unwrap_or_else(|| self.resolve_path(name))
    }

    #[cfg(windows)]
    fn resolve_external_name(&self, name: &str) -> String {
        let path = Path::new(name);
        let has_separator = name.contains(['/', '\\']);
        let mut bases = Vec::new();
        if has_separator || path.is_absolute() {
            bases.push(self.resolve_path(name));
        } else {
            bases.push(self.cwd.join(name));
            if let Some(search_path) = self.value_of("PATH") {
                bases.extend(
                    search_path
                        .split(';')
                        .filter(|directory| !directory.is_empty())
                        .map(|directory| self.resolve_path(directory).join(name)),
                );
            }
        }
        let extensions = self
            .value_of("PATHEXT")
            .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into())
            .split(';')
            .filter(|extension| !extension.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        for base in bases {
            if base.is_file() {
                return base.to_string_lossy().into_owned();
            }
            if base.extension().is_none() {
                for extension in &extensions {
                    let candidate =
                        PathBuf::from(format!("{}{}", base.to_string_lossy(), extension));
                    if candidate.is_file() {
                        return candidate.to_string_lossy().into_owned();
                    }
                }
            }
        }
        name.to_string()
    }

    #[cfg(not(windows))]
    fn resolve_external_name(&self, name: &str) -> String {
        name.to_string()
    }

    fn value_of(&self, name: &str) -> Option<String> {
        self.variables
            .get(name)
            .map(|variable| variable.value.clone())
    }

    fn set_variable(
        &mut self,
        name: &str,
        value: String,
        exported: Option<bool>,
        readonly: bool,
    ) -> Result<(), String> {
        if !valid_variable_name(name) {
            return Err(format!("isksh: {name}: invalid variable name"));
        }
        if self
            .variables
            .get(name)
            .is_some_and(|variable| variable.readonly)
        {
            return Err(format!("isksh: {name}: readonly variable"));
        }
        let previous_export = self
            .variables
            .get(name)
            .is_some_and(|variable| variable.exported);
        self.variables.insert(
            name.to_string(),
            Variable {
                value,
                exported: exported.unwrap_or(previous_export),
                readonly,
            },
        );
        Ok(())
    }

    fn set_assignment(
        &mut self,
        target: &str,
        value: String,
        exported: Option<bool>,
    ) -> Result<(), String> {
        if let Some((name, subscript)) = parse_array_reference(target) {
            if let Some(values) = self.associative_arrays.get_mut(name) {
                values.insert(subscript.to_string(), value);
                return Ok(());
            }
            let index = subscript
                .parse::<usize>()
                .map_err(|_| format!("isksh: {target}: invalid indexed-array subscript"))?;
            self.indexed_arrays
                .entry(name.to_string())
                .or_default()
                .insert(index, value);
            Ok(())
        } else {
            self.set_variable(target, value, exported, false)
        }
    }

    fn array_value(&self, name: &str, subscript: &str) -> Option<String> {
        if let Some(values) = self.associative_arrays.get(name) {
            values.get(subscript).cloned()
        } else {
            let index = subscript.parse::<usize>().ok()?;
            self.indexed_arrays
                .get(name)
                .and_then(|values| values.get(&index))
                .cloned()
        }
    }

    fn array_values(&self, name: &str) -> Vec<String> {
        self.indexed_arrays
            .get(name)
            .map(|values| values.values().cloned().collect())
            .or_else(|| {
                self.associative_arrays
                    .get(name)
                    .map(|values| values.values().cloned().collect())
            })
            .unwrap_or_default()
    }

    fn array_keys(&self, name: &str) -> Vec<String> {
        self.indexed_arrays
            .get(name)
            .map(|values| values.keys().map(ToString::to_string).collect())
            .or_else(|| {
                self.associative_arrays
                    .get(name)
                    .map(|values| values.keys().cloned().collect())
            })
            .unwrap_or_default()
    }

    fn finish_process_substitutions(&mut self) -> ExecResult {
        let mut result = ExecResult::status(0);
        for pending in std::mem::take(&mut self.pending_process_substitutions) {
            if let Some(source) = pending.source {
                match fs::read(&pending.path) {
                    Ok(input) => {
                        let child = self.clone().run(&source, &input);
                        result.stdout.extend(child.stdout);
                        result.stderr.extend(child.stderr);
                        if child.status != 0 {
                            result.status = child.status;
                        }
                    }
                    Err(error) => result
                        .stderr
                        .extend_from_slice(format!("isksh: {error}\n").as_bytes()),
                }
            }
            let _ = fs::remove_file(pending.path);
        }
        result
    }
}

fn parse_array_reference(value: &str) -> Option<(&str, &str)> {
    let (name, subscript) = value.split_once('[')?;
    let subscript = subscript.strip_suffix(']')?;
    (valid_variable_name(name) && !subscript.is_empty()).then_some((name, subscript))
}

fn io_error_string(error: std::io::Error) -> String {
    error.to_string()
}

fn restore_map<T>(target: &mut HashMap<String, T>, saved: HashMap<String, Option<T>>) {
    for (name, value) in saved {
        if let Some(value) = value {
            target.insert(name, value);
        } else {
            target.remove(&name);
        }
    }
}

fn valid_assignment_name(value: &str) -> bool {
    valid_variable_name(value) || parse_array_reference(value).is_some()
}

fn format_array_declaration<'a>(
    prefix: &str,
    name: &str,
    values: impl Iterator<Item = (String, &'a str)>,
) -> String {
    let entries = values
        .map(|(key, value)| format!("[{key}]={}", shell_quote(value)))
        .collect::<Vec<_>>()
        .join(" ");
    format!("{prefix} {name}=({entries})\n")
}

fn evaluate_conditional(tokens: &[String], shell: &Shell) -> Result<bool, String> {
    if tokens.is_empty() {
        return Err("expression required".into());
    }
    if let Some(index) = conditional_operator(tokens, "||") {
        return Ok(evaluate_conditional(&tokens[..index], shell)?
            || evaluate_conditional(&tokens[index + 1..], shell)?);
    }
    if let Some(index) = conditional_operator(tokens, "&&") {
        return Ok(evaluate_conditional(&tokens[..index], shell)?
            && evaluate_conditional(&tokens[index + 1..], shell)?);
    }
    if tokens.first().map(String::as_str) == Some("!") {
        return Ok(!evaluate_conditional(&tokens[1..], shell)?);
    }
    if tokens.first().map(String::as_str) == Some("(")
        && tokens.last().map(String::as_str) == Some(")")
    {
        return evaluate_conditional(&tokens[1..tokens.len() - 1], shell);
    }
    match tokens {
        [value] => Ok(!value.is_empty()),
        [operator, value] => Ok(match operator.as_str() {
            "-n" => !value.is_empty(),
            "-z" => value.is_empty(),
            "-e" => shell.resolve_path(value).exists(),
            "-f" => shell.resolve_path(value).is_file(),
            "-d" => shell.resolve_path(value).is_dir(),
            "-v" => {
                shell.value_of(value).is_some()
                    || parse_array_reference(value)
                        .is_some_and(|(name, key)| shell.array_value(name, key).is_some())
            }
            _ => return Err(format!("unknown unary operator: {operator}")),
        }),
        [left, operator, right] => match operator.as_str() {
            "=" | "==" => Pattern::new(right)
                .map(|pattern| pattern.matches(left))
                .map_err(|error| error.to_string()),
            "!=" => Pattern::new(right)
                .map(|pattern| !pattern.matches(left))
                .map_err(|error| error.to_string()),
            "=~" => Regex::new(right)
                .map(|regex| regex.is_match(left))
                .map_err(|error| error.to_string()),
            "<" => Ok(left < right),
            ">" => Ok(left > right),
            "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge" => {
                let left = left
                    .parse::<i64>()
                    .map_err(|_| "integer expression expected".to_string())?;
                let right = right
                    .parse::<i64>()
                    .map_err(|_| "integer expression expected".to_string())?;
                Ok(match operator.as_str() {
                    "-eq" => left == right,
                    "-ne" => left != right,
                    "-lt" => left < right,
                    "-le" => left <= right,
                    "-gt" => left > right,
                    _ => left >= right,
                })
            }
            _ => Err(format!("unknown binary operator: {operator}")),
        },
        _ => Err("invalid conditional expression".into()),
    }
}

fn conditional_operator(tokens: &[String], expected: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate() {
        match token.as_str() {
            "(" => depth += 1,
            ")" => depth = depth.saturating_sub(1),
            value if value == expected && depth == 0 => return Some(index),
            _ => {}
        }
    }
    None
}

fn finish_external(name: &str, output: std::io::Result<std::process::Output>) -> ExecResult {
    match output {
        Ok(output) => ExecResult {
            status: exit_status(&output.status),
            stdout: output.stdout,
            stderr: output.stderr,
            flow: Flow::None,
        },
        Err(error) => ExecResult::error(126, format!("isksh: {name}: {error}")),
    }
}

fn is_special_builtin(name: &str) -> bool {
    matches!(
        name,
        ":" | "."
            | "break"
            | "continue"
            | "eval"
            | "exec"
            | "exit"
            | "export"
            | "readonly"
            | "return"
            | "set"
            | "shift"
            | "times"
            | "trap"
            | "unset"
    )
}

fn is_builtin(name: &str) -> bool {
    is_special_builtin(name)
        || matches!(
            name,
            "alias"
                | "unalias"
                | "cd"
                | "command"
                | "echo"
                | "false"
                | "getopts"
                | "hash"
                | "printf"
                | "pwd"
                | "read"
                | "test"
                | "["
                | "true"
                | "umask"
                | "wait"
                | "source"
                | "declare"
                | "typeset"
                | "local"
                | "shopt"
                | "type"
                | "mapfile"
                | "readarray"
                | "[["
        )
}

fn valid_variable_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(ch) if ch.is_ascii_alphabetic() || ch == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn write_output_sink(
    sink: &OutputSink,
    data: &[u8],
    stdout: &mut Vec<u8>,
    stderr: &mut Vec<u8>,
) -> std::io::Result<()> {
    match sink {
        OutputSink::Stdout => stdout.extend_from_slice(data),
        OutputSink::Stderr => stderr.extend_from_slice(data),
        OutputSink::File(path) => OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?
            .write_all(data)?,
        OutputSink::Closed => {}
    }
    Ok(())
}

fn flow_status(args: &[String], constructor: fn(i32) -> Flow, default: i32) -> ExecResult {
    let status = args
        .first()
        .map_or(Ok(default), |value| value.parse::<i32>())
        .unwrap_or(2)
        & 0xff;
    let mut result = ExecResult::status(status);
    result.flow = constructor(status);
    result
}

fn builtin_printf(args: &[String]) -> ExecResult {
    let Some(format) = args.first() else {
        return ExecResult::status(0);
    };
    let mut output = String::new();
    let mut arguments = args[1..].iter().cycle();
    let rounds = if args.len() <= 1 {
        1
    } else {
        (args.len() - 1).max(1)
    };
    let mut consumed = 0usize;
    let chars: Vec<_> = format.chars().collect();
    let mut index = 0;
    while index < chars.len() || consumed < rounds {
        if index >= chars.len() {
            index = 0;
            if !format.contains('%') {
                break;
            }
        }
        let ch = chars[index];
        index += 1;
        if ch == '\\' && index < chars.len() {
            let escaped = chars[index];
            index += 1;
            output.push(match escaped {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                '\\' => '\\',
                other => other,
            });
        } else if ch == '%' && index < chars.len() {
            let specifier = chars[index];
            index += 1;
            if specifier == '%' {
                output.push('%');
                continue;
            }
            let value = arguments.next().map(String::as_str).unwrap_or("");
            consumed += 1;
            match specifier {
                's' => output.push_str(value),
                'd' | 'i' => output.push_str(&value.parse::<i64>().unwrap_or(0).to_string()),
                'b' => output.push_str(
                    &value
                        .replace("\\n", "\n")
                        .replace("\\t", "\t")
                        .replace("\\r", "\r"),
                ),
                other => {
                    output.push('%');
                    output.push(other);
                }
            }
        } else {
            output.push(ch);
        }
        if index >= chars.len() && consumed >= rounds {
            break;
        }
    }
    ExecResult {
        stdout: output.into_bytes(),
        ..ExecResult::status(0)
    }
}

fn builtin_test(args: &[String]) -> ExecResult {
    let success = match args {
        [] => false,
        [value] => !value.is_empty(),
        [operator, value] if operator == "-n" => !value.is_empty(),
        [operator, value] if operator == "-z" => value.is_empty(),
        [operator, value] if operator == "-e" => Path::new(value).exists(),
        [operator, value] if operator == "-f" => Path::new(value).is_file(),
        [operator, value] if operator == "-d" => Path::new(value).is_dir(),
        [left, operator, right] if operator == "=" => left == right,
        [left, operator, right] if operator == "!=" => left != right,
        [left, operator, right] if operator == "-eq" => {
            left.parse::<i64>().ok() == right.parse::<i64>().ok()
        }
        [left, operator, right] if operator == "-ne" => {
            left.parse::<i64>().ok() != right.parse::<i64>().ok()
        }
        _ => false,
    };
    ExecResult::status(i32::from(!success))
}

#[cfg(windows)]
fn platform_command(name: &str, arguments: &[String]) -> ProcessCommand {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".cmd") || lower.ends_with(".bat") {
        let mut command = ProcessCommand::new("cmd.exe");
        command
            .arg("/d")
            .arg("/s")
            .arg("/c")
            .arg(name)
            .args(arguments);
        command
    } else {
        let mut command = ProcessCommand::new(name);
        command.args(arguments);
        command
    }
}

#[cfg(not(windows))]
fn platform_command(name: &str, arguments: &[String]) -> ProcessCommand {
    let mut command = ProcessCommand::new(name);
    command.args(arguments);
    command
}

fn exit_status(status: &std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(128)
}

struct ArithmeticParser<'a> {
    chars: Vec<char>,
    index: usize,
    shell: &'a Shell,
}

impl<'a> ArithmeticParser<'a> {
    fn new(source: &str, shell: &'a Shell) -> Self {
        Self {
            chars: source.chars().collect(),
            index: 0,
            shell,
        }
    }

    fn parse(mut self) -> Result<i64, String> {
        let value = self.expression()?;
        self.whitespace();
        if self.index == self.chars.len() {
            Ok(value)
        } else {
            Err("invalid arithmetic expression".into())
        }
    }

    fn expression(&mut self) -> Result<i64, String> {
        let mut value = self.term()?;
        loop {
            self.whitespace();
            if self.consume('+') {
                value = value.wrapping_add(self.term()?);
            } else if self.consume('-') {
                value = value.wrapping_sub(self.term()?);
            } else {
                return Ok(value);
            }
        }
    }

    fn term(&mut self) -> Result<i64, String> {
        let mut value = self.factor()?;
        loop {
            self.whitespace();
            if self.consume('*') {
                value = value.wrapping_mul(self.factor()?);
            } else if self.consume('/') {
                let right = self.factor()?;
                if right == 0 {
                    return Err("division by zero".into());
                }
                value /= right;
            } else if self.consume('%') {
                let right = self.factor()?;
                if right == 0 {
                    return Err("division by zero".into());
                }
                value %= right;
            } else {
                return Ok(value);
            }
        }
    }

    fn factor(&mut self) -> Result<i64, String> {
        self.whitespace();
        if self.consume('-') {
            return Ok(-self.factor()?);
        }
        if self.consume('+') {
            return self.factor();
        }
        if self.consume('(') {
            let value = self.expression()?;
            self.whitespace();
            if !self.consume(')') {
                return Err("missing ')' in arithmetic expression".into());
            }
            return Ok(value);
        }
        let start = self.index;
        while self
            .peek()
            .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        {
            self.index += 1;
        }
        if start == self.index {
            return Err("expected arithmetic operand".into());
        }
        let token: String = self.chars[start..self.index].iter().collect();
        if let Ok(value) = token.parse() {
            Ok(value)
        } else {
            Ok(self
                .shell
                .value_of(&token)
                .and_then(|value| value.parse().ok())
                .unwrap_or(0))
        }
    }

    fn whitespace(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.index += 1;
        }
    }
    fn peek(&self) -> Option<char> {
        self.chars.get(self.index).copied()
    }
    fn consume(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.index += 1;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(source: &str) -> RunResult {
        Shell::default().run(source, &[])
    }

    #[test]
    fn executes_assignments_expansions_and_printf() {
        let result = run("name=world; printf 'hello %s\\n' \"$name\"");
        assert_eq!(result.status, 0);
        assert_eq!(result.stdout, b"hello world\n");
    }

    #[test]
    fn executes_conditionals_and_loops() {
        let result = run(
            "for value in a b c; do if test \"$value\" != b; then printf '%s' \"$value\"; fi; done",
        );
        assert_eq!(result.stdout, b"ac");
    }

    #[test]
    fn executes_function_with_positional_parameters() {
        let result = run("show() { printf '<%s>' \"$1\"; }; show ok");
        assert_eq!(result.stdout, b"<ok>");
    }

    #[test]
    fn arithmetic_and_command_substitution_work() {
        let result = run("value=$((2 + 3 * 4)); printf '%s:%s' \"$value\" \"$(printf done)\"");
        assert_eq!(result.stdout, b"14:done");
    }

    #[test]
    fn case_while_break_and_group_work() {
        let result = run(
            "i=0; while test $i -ne 4; do i=$((i + 1)); case $i in 2) continue;; 4) break;; *) printf '%s' $i;; esac; done; { printf done; }",
        );
        assert_eq!(result.stdout, b"13done");
    }

    #[test]
    fn getopts_reads_grouped_options_and_arguments() {
        let result = run(
            "set -- -ab value; while getopts 'ab:' option; do printf '%s:%s;' \"$option\" \"${OPTARG:-}\"; done",
        );
        assert_eq!(result.stdout, b"a:;b:value;");
    }

    #[test]
    fn exercises_control_flow_and_shell_state() {
        let mut shell = Shell::default();
        let result = shell.run(
            "x=outer; (x=inner); { x=group; }; until true; do false; done; false || true; ! false; printf '%s' \"$x\"",
            &[],
        );
        assert_eq!(result.stdout, b"group");
        assert_eq!(result.status, 0);

        assert_eq!(shell.run("return", &[]).status, 1);
        assert_eq!(shell.run("break", &[]).status, 1);
        assert_eq!(shell.run("continue", &[]).status, 1);
        assert_eq!(shell.run("exit 258", &[]).status, 2);
        assert_eq!(shell.take_exit_status(), Some(2));
        assert_eq!(shell.take_exit_status(), None);
    }

    #[test]
    fn exercises_parameter_tilde_glob_and_arithmetic_errors() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("a.txt"), b"").unwrap();
        fs::write(directory.path().join("b.txt"), b"").unwrap();
        let mut shell = Shell {
            cwd: directory.path().to_path_buf(),
            ..Shell::default()
        };
        shell
            .set_variable(
                "HOME",
                directory.path().to_string_lossy().into_owned(),
                None,
                false,
            )
            .unwrap();
        shell.set_positional(vec!["one".into(), "two".into()]);
        let result = shell.run(
            "unset x; printf '%s|' \"${x-word}\" \"${x+no}\" \"${x=assigned}\" \"${x+yes}\" \"${#x}\" \"$#\" \"$0\" \"$1\" \"$9\" \"$@\" \"$*\" ~/*.txt",
            &[],
        );
        let output = String::from_utf8(result.stdout).unwrap();
        assert!(output.contains("word||assigned|yes|8|2|"));
        assert!(output.contains("a.txt"));
        assert!(output.contains("b.txt"));
        assert_ne!(shell.run("printf '%s' $((1 / 0))", &[]).status, 0);
        assert_ne!(shell.run("printf '%s' $((1 +))", &[]).status, 0);
        assert_ne!(shell.run("printf '%s' $((1 2))", &[]).status, 0);
        assert_ne!(shell.run("printf '%s' $((1 + (2))", &[]).status, 0);
        assert_ne!(shell.run("printf '%s' ${missing:?required}", &[]).status, 0);
    }

    #[test]
    fn exercises_redirection_order_and_errors() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("combined");
        let escaped = path.to_string_lossy().replace('\\', "/");
        let mut shell = Shell::default();
        let result = shell.run(
            &format!("sh -c 'printf out; printf err >&2' >'{escaped}' 2>&1"),
            &[],
        );
        assert_eq!(result.status, 0);
        assert_eq!(fs::read(path).unwrap(), b"outerr");
        let result = shell.run("printf err 1>&2", &[]);
        assert_eq!(result.stderr, b"err");
        assert_ne!(shell.run("printf x 9>file", &[]).status, 0);
        assert_ne!(shell.run("printf x 2>&9", &[]).status, 0);
        assert_eq!(shell.run("printf x 1>&-", &[]).stdout, b"");
        assert_ne!(shell.run("cat < /missing/isksh-file", &[]).status, 0);
    }

    #[test]
    fn exercises_builtins() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("file");
        fs::write(&file, "value=dot\n").unwrap();
        let mut shell = Shell::default();
        let _named = Shell::new(String::from("named"));

        assert_eq!(shell.execute_builtin("false", &[], &[]).status, 1);
        assert_eq!(
            shell
                .execute_builtin("echo", &["-n".into(), "x".into()], &[])
                .stdout,
            b"x"
        );
        assert!(
            shell
                .execute_builtin("pwd", &[], &[])
                .stdout
                .ends_with(b"\n")
        );
        assert_eq!(
            shell
                .builtin_cd(&[directory.path().to_string_lossy().into_owned()])
                .status,
            0
        );
        shell
            .set_variable(
                "HOME",
                directory.path().to_string_lossy().into_owned(),
                None,
                false,
            )
            .unwrap();
        assert_eq!(shell.builtin_cd(&[]).status, 0);
        assert_ne!(
            shell
                .builtin_cd(&[file.to_string_lossy().into_owned()])
                .status,
            0
        );
        assert_ne!(shell.builtin_cd(&["missing".into()]).status, 0);

        assert_eq!(
            shell
                .builtin_export(&["EXPORTED=value".into()], false)
                .status,
            0
        );
        assert_eq!(shell.builtin_export(&["NAME_ONLY".into()], false).status, 0);
        assert!(
            String::from_utf8(shell.builtin_export(&[], false).stdout)
                .unwrap()
                .contains("EXPORTED")
        );
        assert_eq!(
            shell.builtin_export(&["LOCKED=value".into()], true).status,
            0
        );
        assert!(
            String::from_utf8(shell.builtin_export(&[], true).stdout)
                .unwrap()
                .contains("readonly LOCKED")
        );
        assert_ne!(shell.builtin_export(&["1BAD=x".into()], false).status, 0);
        assert_ne!(shell.builtin_unset(&["LOCKED".into()]).status, 0);
        assert_eq!(shell.builtin_unset(&["EXPORTED".into()]).status, 0);

        assert!(!shell.builtin_set(&[]).stdout.is_empty());
        assert_eq!(
            shell
                .builtin_set(&["--".into(), "a".into(), "b".into()])
                .status,
            0
        );
        assert_eq!(shell.builtin_shift(&[]).status, 0);
        assert_ne!(shell.builtin_shift(&["9".into()]).status, 0);
        assert_ne!(shell.builtin_set(&["-e".into()]).status, 0);

        assert_eq!(
            shell.execute_eval(&["printf eval".into()], &[]).stdout,
            b"eval"
        );
        assert_ne!(shell.execute_eval(&["if".into()], &[]).status, 0);
        assert_eq!(
            shell
                .builtin_dot(&[file.to_string_lossy().into_owned()], &[])
                .status,
            0
        );
        assert_ne!(shell.builtin_dot(&[], &[]).status, 0);
        assert_ne!(shell.builtin_dot(&["missing".into()], &[]).status, 0);

        assert_eq!(shell.builtin_read(&[], b"answer\n").status, 0);
        assert_eq!(shell.value_of("REPLY").as_deref(), Some("answer"));
        assert_eq!(
            shell
                .builtin_read(&["A".into(), "B".into()], b"a b c\n")
                .status,
            0
        );
        assert_eq!(shell.value_of("B").as_deref(), Some("b c"));
        assert_ne!(shell.builtin_read(&[], &[0xff]).status, 0);
        assert_ne!(shell.builtin_read(&["1BAD".into()], b"x").status, 0);

        assert_eq!(shell.builtin_alias(&["ll=printf alias".into()]).status, 0);
        assert_eq!(shell.run("ll", &[]).stdout, b"alias");
        assert!(!shell.builtin_alias(&[]).stdout.is_empty());
        assert_ne!(shell.builtin_alias(&["missing".into()]).status, 0);
        assert_eq!(shell.builtin_unalias(&["ll".into()]).status, 0);

        assert_eq!(
            shell
                .builtin_command(&["-v".into(), "printf".into()], &[])
                .status,
            0
        );
        assert_eq!(
            shell
                .builtin_command(&["-v".into(), "missing-isksh".into()], &[])
                .status,
            1
        );
        assert_eq!(shell.builtin_command(&[], &[]).status, 0);
        assert_eq!(
            shell
                .builtin_command(&["--".into(), "true".into()], &[])
                .status,
            0
        );
        assert_ne!(shell.execute_builtin("trap", &[], &[]).status, 0);
        assert_ne!(shell.execute_builtin("unsupported", &[], &[]).status, 0);
    }

    #[test]
    fn exercises_printf_test_getopts_and_external_commands() {
        assert_eq!(builtin_printf(&[]).status, 0);
        assert_eq!(
            builtin_printf(&[
                "%%:%d:%i:%b:%q\\n".into(),
                "2".into(),
                "bad".into(),
                "a\\tb".into(),
                "x".into()
            ])
            .stdout,
            b"%:2:0:a\tb:%q\n"
        );
        for (args, expected) in [
            (vec![], 1),
            (vec!["x"], 0),
            (vec!["-n", "x"], 0),
            (vec!["-z", ""], 0),
            (vec!["a", "=", "a"], 0),
            (vec!["a", "!=", "b"], 0),
            (vec!["1", "-eq", "1"], 0),
            (vec!["1", "-ne", "2"], 0),
            (vec!["bad", "-eq", "bad"], 0),
            (vec!["too", "many", "values", "here"], 1),
        ] {
            let args = args.into_iter().map(str::to_string).collect::<Vec<_>>();
            assert_eq!(builtin_test(&args).status, expected);
        }

        let mut shell = Shell::default();
        assert_ne!(shell.builtin_getopts(&[]).status, 0);
        shell.set_positional(vec!["-x".into()]);
        assert_eq!(
            shell.builtin_getopts(&[":a".into(), "OPT".into()]).status,
            0
        );
        assert_eq!(shell.value_of("OPT").as_deref(), Some("?"));
        shell.builtin_set(&["--".into(), "-a".into()]);
        assert_eq!(
            shell.builtin_getopts(&["a:".into(), "OPT".into()]).status,
            0
        );
        assert_eq!(shell.value_of("OPT").as_deref(), Some("?"));
        shell.builtin_set(&["--".into(), "--".into()]);
        assert_eq!(shell.builtin_getopts(&["a".into(), "OPT".into()]).status, 1);
        assert_eq!(
            shell
                .execute_external("missing-isksh-command", &[], &[], false)
                .status,
            127
        );
        assert_eq!(
            shell
                .execute_external("sh", &["-c".into(), "exit 3".into()], &[], false)
                .status,
            3
        );
    }

    #[test]
    fn exercises_input_classification_background_pipeline_and_nested_flow() {
        assert!(matches!(
            Shell::check_input("echo ok"),
            InputState::Complete
        ));
        assert!(matches!(
            Shell::check_input("if true"),
            InputState::Incomplete
        ));
        assert!(matches!(Shell::check_input(")"), InputState::Invalid(_)));

        let mut shell = Shell::default();
        let parse_error = shell.run(")", &[]);
        assert_eq!(parse_error.status, 2);
        assert!(!parse_error.stderr.is_empty());
        let background = shell.run("printf bg &", &[]);
        assert_eq!(background.stdout, b"bg");
        assert!(
            String::from_utf8(background.stderr)
                .unwrap()
                .contains("synchronous")
        );
        assert_eq!(shell.run("printf pipe | cat", &[]).stdout, b"pipe");
        assert_eq!(
            shell
                .run(
                    "for a in 1; do for b in 1; do break 2; done; printf no; done; printf yes",
                    &[],
                )
                .stdout,
            b"yes"
        );
        assert_eq!(
            shell
                .run(
                    "for a in 1 2; do for b in 1; do continue 2; done; printf no; done; printf yes",
                    &[],
                )
                .stdout,
            b"yes"
        );
        shell.set_positional(vec!["a".into(), "b".into()]);
        assert_eq!(shell.run("for x; do printf %s $x; done", &[]).stdout, b"ab");
        assert_eq!(shell.run("case no in yes) false;; esac", &[]).status, 0);
        assert_eq!(shell.run("f() { return 5; }; f", &[]).status, 5);
        assert_eq!(
            shell
                .run("if false; then printf no; else printf else; fi", &[])
                .stdout,
            b"else"
        );
        assert_eq!(shell.run("exit 3 && printf no", &[]).status, 3);
        assert_eq!(shell.run("true && printf and", &[]).stdout, b"and");
        assert_eq!(
            shell
                .run("f() { while true; do return 4; done; }; f", &[])
                .status,
            4
        );
        assert_eq!(
            shell
                .run(
                    "while true; do while true; do break 2; done; done; printf done",
                    &[],
                )
                .stdout,
            b"done"
        );
        assert_eq!(
            shell
                .run(
                    "i=0; while test $i -ne 2; do i=$((i+1)); while true; do continue 2; done; done; printf done",
                    &[],
                )
                .stdout,
            b"done"
        );
        assert_eq!(shell.run("for x in a; do exit 6; done", &[]).status, 6);
    }

    #[test]
    fn exercises_assignment_alias_and_expansion_edge_cases() {
        let mut shell = Shell::default();
        shell.run("KEEP=old; readonly LOCK=old", &[]);
        assert_ne!(shell.run("KEEP=temp LOCK=new true", &[]).status, 0);
        assert_eq!(shell.value_of("KEEP").as_deref(), Some("old"));
        assert_eq!(
            shell
                .run("TEMP=value true; printf %s \"${TEMP-no}\"", &[])
                .stdout,
            b"no"
        );
        assert_eq!(shell.run("PREFIX=x read RESULT", b"persist\n").status, 0);
        assert_eq!(shell.value_of("RESULT").as_deref(), Some("persist"));
        assert_eq!(shell.run("printf '<%s>' $UNSET", &[]).stdout, b"<>");

        shell.run("alias say='printf \"<%s>\"'", &[]);
        assert_eq!(shell.run("say \"a'b\"", &[]).stdout, b"<a'b>");
        shell.run("alias self=self", &[]);
        assert_eq!(shell.run("self", &[]).status, 127);
        assert!(
            shell
                .builtin_command(&["-v".into(), "say".into()], &[])
                .stdout
                .starts_with(b"alias")
        );

        shell.set_positional(vec!["a".into(), "b".into()]);
        assert_ne!(shell.run("printf x >\"$@\"", &[]).status, 0);
        assert_eq!(
            shell.run("printf %s 'no-match-*.isksh'", &[]).stdout,
            b"no-match-*.isksh"
        );
        assert_ne!(shell.run("printf %s [bad", &[]).status, 0);
        assert_eq!(shell.expand_parameter("bad name:-x").unwrap(), "");
        assert_eq!(
            shell.expand_parameter("EMPTY?").unwrap_err(),
            "EMPTY: parameter is unset or null"
        );
        shell
            .set_variable("PRESENT", "yes".into(), None, false)
            .unwrap();
        assert_eq!(shell.expand_parameter("PRESENT=other").unwrap(), "yes");
        assert_eq!(shell.expand_parameter("PRESENT?bad").unwrap(), "yes");
        assert_ne!(shell.run("readonly ONLY=old; ONLY=new", &[]).status, 0);
        assert_eq!(shell.run("$UNSET", &[]).status, 0);
        assert_eq!(
            shell.run("printf %s no-match-*.isksh", &[]).stdout,
            b"no-match-*.isksh"
        );

        let non_utf8 = Word {
            parts: vec![WordPart::CommandSubstitution {
                source: "sh -c 'printf \\\\377'".into(),
                quoted: false,
            }],
        };
        assert!(shell.expand_word(&non_utf8).is_err());
        let failed = Word {
            parts: vec![WordPart::CommandSubstitution {
                source: "sh -c 'printf err >&2; exit 1'".into(),
                quoted: false,
            }],
        };
        assert!(shell.expand_word(&failed).unwrap().is_empty());
        let bad = Word {
            parts: vec![WordPart::Arithmetic {
                expression: "1/0".into(),
                quoted: false,
            }],
        };
        let empty = Script { lists: Vec::new() };
        assert_ne!(
            shell
                .execute_for("x", std::slice::from_ref(&bad), &empty, &[])
                .status,
            0
        );
        assert_ne!(
            shell
                .execute_for(
                    "1BAD",
                    &[Word {
                        parts: vec![WordPart::Literal {
                            value: "x".into(),
                            quoted: false,
                        }],
                    }],
                    &empty,
                    &[],
                )
                .status,
            0
        );
        assert_ne!(shell.execute_case(&bad, &[], &[]).status, 0);
        let literal = Word {
            parts: vec![WordPart::Literal {
                value: "x".into(),
                quoted: false,
            }],
        };
        assert_ne!(
            shell
                .execute_case(
                    &literal,
                    &[CaseArm {
                        patterns: vec![bad],
                        body: empty,
                    }],
                    &[],
                )
                .status,
            0
        );
    }

    #[test]
    fn exercises_heredoc_and_redirection_internal_errors() {
        let mut shell = Shell::default();
        assert_eq!(
            shell
                .expand_here_document("\\$x|\\`|\\\\|a\\\nb|$?|$x|$((2*(3)))|$(printf sub)|$!")
                .unwrap(),
            "$x|`|\\|ab|0||6|sub|$!"
        );
        assert!(shell.expand_here_document("${x").is_err());
        assert!(shell.expand_here_document("$(printf x").is_err());
        assert!(shell.expand_here_document("$((1 + 2)").is_err());
        assert_eq!(shell.expand_here_document("tail\\").unwrap(), "tail\\");
        assert_eq!(shell.expand_here_document("\\q").unwrap(), "\\q");
        shell.set_variable("HD", "ok".into(), None, false).unwrap();
        assert_eq!(shell.expand_here_document("${HD}").unwrap(), "ok");
        shell
            .set_variable("LONG_NAME", "long".into(), None, false)
            .unwrap();
        assert_eq!(shell.expand_here_document("$LONG_NAME").unwrap(), "long");
        assert_eq!(
            shell.expand_here_document("$(printf (nested))").unwrap(),
            ""
        );
        assert!(
            shell
                .expand_here_document("$(sh -c 'printf \\\\377')")
                .is_err()
        );

        let missing_document = SimpleCommand {
            words: vec![Word {
                parts: vec![WordPart::Literal {
                    value: "cat".into(),
                    quoted: false,
                }],
            }],
            redirections: vec![Redirection {
                fd: None,
                kind: RedirectionKind::HereDocument,
                target: Word::default(),
                here_document: None,
            }],
            ..SimpleCommand::default()
        };
        assert_eq!(shell.execute_simple(&missing_document, &[]).status, 2);

        let invalid_document = SimpleCommand {
            words: vec![Word {
                parts: vec![WordPart::Literal {
                    value: "cat".into(),
                    quoted: false,
                }],
            }],
            redirections: vec![Redirection {
                fd: None,
                kind: RedirectionKind::HereDocument,
                target: Word::default(),
                here_document: Some(HereDocument {
                    body: "${x".into(),
                    expand: true,
                }),
            }],
            ..SimpleCommand::default()
        };
        assert_ne!(shell.execute_simple(&invalid_document, &[]).status, 0);
        assert_eq!(
            shell.run("V=expanded\ncat <<EOF\n$V\nEOF\n", &[]).stdout,
            b"expanded\n"
        );
        assert_eq!(shell.run("cat <<'EOF'\n$V\nEOF\n", &[]).stdout, b"$V\n");

        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("rw")
            .to_string_lossy()
            .replace('\\', "/");
        fs::write(&path, b"input").unwrap();
        assert_eq!(shell.run(&format!("cat <'{path}'"), &[]).stdout, b"input");
        assert_eq!(shell.run(&format!("cat 0<>'{path}'"), &[]).stdout, b"input");
        assert_eq!(
            shell
                .run(&format!("printf changed 1<>'{path}'"), &[])
                .status,
            0
        );
        assert_ne!(shell.run("cat <>/missing/isksh/dir/file", &[]).status, 0);
        shell.set_positional(vec!["one".into(), "two".into()]);
        assert_ne!(shell.run("cat <\"$@\"", &[]).status, 0);
        assert_eq!(shell.run("printf x 2>&-", &[]).stderr, b"");
        assert_eq!(shell.run("printf x 0<&-", &[]).status, 0);
        assert_ne!(shell.run("printf x 3<&-", &[]).status, 0);
        assert_ne!(
            shell
                .run(&format!("printf x >'{}'", directory.path().display()), &[])
                .status,
            0
        );
        assert_ne!(shell.run("printf x >>/dev/full", &[]).status, 0);
        assert_ne!(
            shell.run("sh -c 'printf x >&2' 2>>/dev/full", &[]).status,
            0
        );
        assert_ne!(shell.run("printf x >>/", &[]).status, 0);

        let bad_word = Word {
            parts: vec![WordPart::Arithmetic {
                expression: "1 / 0".into(),
                quoted: false,
            }],
        };
        let bad_assignment = SimpleCommand {
            assignments: vec![("X".into(), bad_word.clone())],
            ..SimpleCommand::default()
        };
        assert_ne!(shell.execute_simple(&bad_assignment, &[]).status, 0);
        let invalid_name_assignment = SimpleCommand {
            assignments: vec![("1BAD".into(), Word::default())],
            ..SimpleCommand::default()
        };
        assert_ne!(
            shell.execute_simple(&invalid_name_assignment, &[]).status,
            0
        );
        let bad_command = SimpleCommand {
            words: vec![bad_word.clone()],
            ..SimpleCommand::default()
        };
        assert_ne!(shell.execute_simple(&bad_command, &[]).status, 0);
        let bad_redirect = SimpleCommand {
            words: vec![Word {
                parts: vec![WordPart::Literal {
                    value: "true".into(),
                    quoted: false,
                }],
            }],
            redirections: vec![Redirection {
                fd: Some(2),
                kind: RedirectionKind::DuplicateOutput,
                target: bad_word,
                here_document: None,
            }],
            ..SimpleCommand::default()
        };
        assert_ne!(shell.execute_simple(&bad_redirect, &[]).status, 0);
    }

    #[test]
    fn exercises_remaining_builtin_and_arithmetic_paths() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("file");
        fs::write(&file, b"").unwrap();
        let mut shell = Shell::default();
        assert_eq!(shell.execute_builtin(":", &[], &[]).status, 0);
        assert_eq!(
            shell.execute_builtin("echo", &["x".into()], &[]).stdout,
            b"x\n"
        );
        assert_eq!(shell.execute_builtin("exec", &[], &[]).status, 0);
        assert_eq!(shell.run("exec true; printf no", &[]).stdout, b"");
        assert_eq!(shell.run("exec sh -c 'exit 9'", &[]).status, 9);
        assert_eq!(shell.execute_builtin("[", &["x".into()], &[]).status, 2);
        assert_eq!(
            shell
                .execute_builtin("[", &["x".into(), "]".into()], &[])
                .status,
            0
        );
        assert!(!shell.execute_builtin("times", &[], &[]).stdout.is_empty());
        assert_eq!(shell.execute_builtin("hash", &[], &[]).status, 0);
        assert_ne!(shell.execute_builtin("umask", &[], &[]).status, 0);
        shell.run("readonly LOCKED_EXPORT=x", &[]);
        assert_ne!(
            shell
                .builtin_export(&["LOCKED_EXPORT=y".into()], false)
                .status,
            0
        );

        let command_path = std::env::var("PATH")
            .unwrap()
            .split(':')
            .map(PathBuf::from)
            .find(|path| path.join("sh").is_file())
            .unwrap()
            .join("sh");
        assert_eq!(
            shell
                .builtin_command(&["-V".into(), "sh".into()], &[])
                .status,
            0
        );
        assert_eq!(
            shell
                .builtin_command(
                    &[
                        command_path.to_string_lossy().into_owned(),
                        "-c".into(),
                        "exit 6".into()
                    ],
                    &[]
                )
                .status,
            6
        );

        for args in [
            vec!["-e", file.to_str().unwrap()],
            vec!["-f", file.to_str().unwrap()],
            vec!["-d", directory.path().to_str().unwrap()],
        ] {
            assert_eq!(
                builtin_test(&args.into_iter().map(str::to_string).collect::<Vec<_>>()).status,
                0
            );
        }
        assert_eq!(shell.evaluate_arithmetic("-5 + +2 - 1").unwrap(), -4);
        assert_eq!(shell.evaluate_arithmetic("7 % 4").unwrap(), 3);
        assert_eq!(shell.evaluate_arithmetic("8 / 2").unwrap(), 4);
        assert!(shell.evaluate_arithmetic("7 % 0").is_err());
        assert!(shell.evaluate_arithmetic("(1 + 2").is_err());
        assert!(shell.set_variable("1BAD", "x".into(), None, false).is_err());
        shell.set_variable("RO", "x".into(), None, true).unwrap();
        assert!(shell.set_variable("RO", "y".into(), None, false).is_err());
        assert_eq!(shell.execute_external("/", &[], &[], false).status, 126);
        assert_eq!(
            finish_external(
                "broken",
                Err(std::io::Error::other("simulated wait failure"))
            )
            .status,
            126
        );
        assert_eq!(builtin_printf(&["\\r\\t\\\\\\x".into()]).stdout, b"\r\t\\x");
        shell.builtin_alias(&["known=value".into()]);
        assert_eq!(shell.builtin_alias(&["known".into()]).status, 0);
        shell.run("RESTORE=old", &[]);
        assert_eq!(shell.run("RESTORE=temp true", &[]).status, 0);
        assert_eq!(shell.value_of("RESTORE").as_deref(), Some("old"));
    }

    #[test]
    fn exercises_getopts_operand_variants() {
        let mut shell = Shell::default();
        assert_eq!(
            shell
                .builtin_getopts(&["a:".into(), "OPT".into(), "-avalue".into()])
                .status,
            0
        );
        assert_eq!(shell.value_of("OPTARG").as_deref(), Some("value"));
        shell
            .set_variable("OPTIND", "1".into(), None, false)
            .unwrap();
        shell.getopts_offset = 1;
        assert_eq!(
            shell
                .builtin_getopts(&["a:".into(), "OPT".into(), "-a".into(), "value".into(),])
                .status,
            0
        );
        shell
            .set_variable("OPTIND", "1".into(), None, false)
            .unwrap();
        shell.getopts_offset = 1;
        assert_eq!(
            shell
                .builtin_getopts(&[":a:".into(), "OPT".into(), "-a".into()])
                .status,
            0
        );
        assert_eq!(shell.value_of("OPT").as_deref(), Some(":"));
        shell
            .set_variable("OPTIND", "1".into(), None, false)
            .unwrap();
        assert_eq!(
            shell
                .builtin_getopts(&["a".into(), "OPT".into(), "plain".into()])
                .status,
            1
        );
        shell
            .set_variable("OPTIND", "1".into(), None, false)
            .unwrap();
        shell.getopts_offset = 2;
        assert_eq!(
            shell
                .builtin_getopts(&["a".into(), "OPT".into(), "-a".into()])
                .status,
            1
        );
        shell
            .set_variable("OPTIND", "1".into(), None, false)
            .unwrap();
        shell.getopts_offset = 1;
        assert_eq!(
            shell
                .builtin_getopts(&["a".into(), "OPT".into(), "-a".into()])
                .status,
            0
        );
    }

    #[test]
    fn supports_bash_arrays_and_conditionals() {
        let mut shell = Shell::default();
        let result = shell.run(
            "a=(zero 'one value' two); a[4]=four; printf '%s|%s|%s|%s\\n' \"${a[1]}\" \"${#a[@]}\" \"${!a[@]}\" \"${a[@]}\"; [[ foobar == foo* && 4 -gt 2 ]]; echo $?",
            &[],
        );
        assert_eq!(result.status, 0);
        assert_eq!(
            result.stdout,
            b"one value|4|0 1 2 4|zero one value two four\n0\n"
        );

        assert_eq!(
            shell
                .run(
                    "declare -A map; map[key]=value; [[ ${map[key]} =~ ^val && ! -z ${map[key]} ]]",
                    &[]
                )
                .status,
            0
        );
        assert_eq!(
            shell
                .run("[[ 2 -ge 3 || ( x != y && -n yes ) ]]", &[])
                .status,
            0
        );
        assert_eq!(shell.run("[[ -v a[4] && -d . && ! -f . ]]", &[]).status, 0);
    }

    #[test]
    fn supports_process_substitution_and_bashrc_builtins() {
        let mut shell = Shell::default();
        let result = shell.run("cat <(printf input); printf output > >(cat)", &[]);
        assert_eq!(result.status, 0);
        assert_eq!(result.stdout, b"inputoutput");

        assert_eq!(
            shell
                .run("shopt -s nullglob; shopt -q nullglob", &[])
                .status,
            0
        );
        assert!(!shell.run("shopt -u nullglob; shopt", &[]).stdout.is_empty());
        assert_eq!(
            shell
                .run("mapfile -t lines; printf '%s' \"${lines[1]}\"", b"a\nb\n")
                .stdout,
            b"b"
        );
        assert_eq!(shell.run("declare -p lines", &[]).status, 0);
        assert_eq!(shell.run("type -t printf", &[]).stdout, b"builtin\n");
        assert_eq!(shell.run("local value=x", &[]).status, 1);
        assert_eq!(shell.run("value=outer; f() { local value=x; printf '%s' \"$value\"; }; f; printf '%s' \"$value\"", &[]).stdout, b"xouter");
        assert_eq!(shell.run("g() { local created=yes; declare -a local_array; local_array[0]=x; }; g; printf '%s' \"$created${local_array[0]}\"", &[]).stdout, b"");
    }

    #[test]
    fn covers_bash_compatibility_errors_and_variants() {
        let mut shell = Shell::default();
        assert_eq!(
            io_error_string(std::io::Error::other("expected")),
            "expected"
        );
        let bad_word = Word {
            parts: vec![WordPart::Arithmetic {
                expression: "1/0".into(),
                quoted: false,
            }],
        };
        let command = SimpleCommand {
            array_assignments: vec![("bad".into(), vec![bad_word])],
            ..SimpleCommand::default()
        };
        assert_ne!(shell.execute_simple(&command, &[]).status, 0);

        assert_eq!(shell.run("printf x > >(false)", &[]).status, 1);
        let missing = std::env::temp_dir().join("isksh-deliberately-missing-process-substitution");
        shell
            .pending_process_substitutions
            .push(PendingProcessSubstitution {
                path: missing,
                source: Some(":".into()),
            });
        assert!(!shell.finish_process_substitutions().stderr.is_empty());

        shell.run(
            "scalar=value; indexed=(abc); declare -A assoc; assoc[key]=xyz",
            &[],
        );
        assert_eq!(
            shell
                .run("printf '%s|%s' \"${assoc[@]}\" \"${!assoc[@]}\"", &[])
                .stdout,
            b"xyz|key"
        );
        for source in [
            "declare -p scalar",
            "declare -p indexed",
            "declare -p assoc",
            "declare -a new_indexed",
            "declare -g plain=x",
        ] {
            assert_eq!(shell.run(source, &[]).status, 0, "{source}");
        }
        for source in [
            "declare -p missing",
            "declare -z x",
            "declare 1bad=x",
            "shopt invalid",
            "shopt -x",
        ] {
            assert_ne!(shell.run(source, &[]).status, 0, "{source}");
        }
        shell.run("readonly locked=x", &[]);
        assert_ne!(shell.run("declare locked=y", &[]).status, 0);

        shell.run("alias named='true'; fun() { :; }", &[]);
        assert!(
            String::from_utf8(shell.run("type named fun printf sh", &[]).stdout)
                .unwrap()
                .contains("alias")
        );
        assert_eq!(shell.run("type definitely_missing_command", &[]).status, 1);

        assert_eq!(shell.run("mapfile -- rows", b"a\n").status, 0);
        assert_ne!(shell.run("mapfile -x", b"").status, 0);
        assert_ne!(shell.run("mapfile 1bad", b"").status, 0);
        assert_ne!(shell.run("mapfile rows", &[0xff]).status, 0);

        assert_eq!(
            shell.run("printf '%s' \"${#indexed[0]}\"", &[]).stdout,
            b"3"
        );
        for source in [
            "[[ ]]",
            "[[",
            "[[ -x value ]]",
            "[[ a nonsense b ]]",
            "[[ 1 -eq nope ]]",
            "[[ nope -eq 1 ]]",
            "[[ x == [ ]]",
            "[[ x != [ ]]",
            "[[ x =~ ( ]]",
            "[[ a b c d ]]",
        ] {
            assert_ne!(shell.run(source, &[]).status, 0, "{source}");
        }
        assert_eq!(shell.run("[[ value ]]", &[]).status, 0);
        assert_ne!(shell.run("indexed[bad]=x", &[]).status, 0);
    }

    #[test]
    fn expands_bash_style_prompts_and_runs_prompt_command() {
        let directory = tempfile::tempdir().unwrap();
        let child = directory.path().join("project");
        fs::create_dir(&child).unwrap();
        let mut shell = Shell::new("path/to/isksh");
        shell.cwd = child;
        shell
            .set_variable(
                "HOME",
                directory.path().to_string_lossy().into_owned(),
                None,
                false,
            )
            .unwrap();
        shell
            .set_variable("USER", "tester".into(), None, false)
            .unwrap();
        shell
            .set_variable("HOSTNAME", "host.example".into(), None, false)
            .unwrap();
        shell
            .set_variable("PROMPT_COMMAND", "printf pre".into(), None, false)
            .unwrap();
        shell.last_status = 7;
        shell
            .set_variable(
                "PS1",
                "\\u@\\h/\\H:\\w:\\W:\\s:\\v:\\V:\\j:\\!:\\#:\\$:\\[\\e\\]\\101:\\q:\\\\:$(printf dyn):$? ".into(),
                None,
                false,
            )
            .unwrap();
        let prompt = shell.prompt(false);
        assert!(prompt.starts_with("pretester@host/host.example:~/project:project:isksh:"));
        assert!(prompt.contains(":0:1:1:$:\u{1b}A:\\q:\\:dyn:7 "));
        assert_eq!(shell.last_status, 7);

        shell
            .set_variable("USER", "root".into(), None, false)
            .unwrap();
        shell
            .set_variable("PS1", "\\$".into(), None, false)
            .unwrap();
        assert_eq!(shell.prompt(false), "pre#");
        shell.set_variable("PS1", "$(".into(), None, false).unwrap();
        assert_eq!(shell.prompt(false), "pre$(");
        shell.cwd = directory.path().to_path_buf();
        shell
            .set_variable("PS1", "\\W\\a\\n\\r\\".into(), None, false)
            .unwrap();
        assert_eq!(shell.prompt(false), "pre~\u{7}\n\r\\");
        shell.variables.remove("USER");
        shell.variables.remove("HOSTNAME");
        shell.variables.remove("HOME");
        shell
            .set_variable("USERNAME", "fallback-user".into(), None, false)
            .unwrap();
        shell
            .set_variable("COMPUTERNAME", "fallback-host".into(), None, false)
            .unwrap();
        shell
            .set_variable(
                "USERPROFILE",
                directory.path().to_string_lossy().into_owned(),
                None,
                false,
            )
            .unwrap();
        shell.name = "/".into();
        shell
            .set_variable("PS1", "\\u@\\H:\\s".into(), None, false)
            .unwrap();
        assert_eq!(shell.prompt(false), "prefallback-user@fallback-host:/");
        shell
            .set_variable("PS2", "next> ".into(), None, false)
            .unwrap();
        assert_eq!(shell.prompt(true), "next> ");
    }

    #[test]
    fn interactive_external_commands_inherit_the_terminal() {
        let mut shell = Shell::default();
        shell.set_interactive(true);
        assert_eq!(
            shell
                .execute_external("sh", &["-c".into(), "exit 7".into()], &[], true)
                .status,
            7
        );
        assert_eq!(
            shell
                .execute_external("missing-isksh-command", &[], &[], true)
                .status,
            127
        );
        assert_eq!(shell.execute_external("/", &[], &[], true).status, 126);
        shell.set_interactive(false);
    }
}
