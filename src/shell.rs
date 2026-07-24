use crate::ast::*;
use crate::parser::parse;
use glob::{MatchOptions, Pattern, glob_with};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};

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

#[derive(Debug, Clone)]
pub struct Shell {
    variables: HashMap<String, Variable>,
    positional: Vec<String>,
    name: String,
    last_status: i32,
    functions: HashMap<String, Command>,
    aliases: HashMap<String, String>,
    cwd: PathBuf,
    loop_depth: usize,
    function_depth: usize,
    getopts_offset: usize,
}

impl Default for Shell {
    fn default() -> Self {
        Self::new("isksh")
    }
}

impl Shell {
    pub fn new(name: impl Into<String>) -> Self {
        let variables = std::env::vars()
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
        Self {
            variables,
            positional: Vec::new(),
            name: name.into(),
            last_status: 0,
            functions: HashMap::new(),
            aliases: HashMap::new(),
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            loop_depth: 0,
            function_depth: 0,
            getopts_offset: 1,
        }
    }

    pub fn set_positional(&mut self, values: Vec<String>) {
        self.positional = values;
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
            Flow::Exit(status) | Flow::Return(status) => status,
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
        let mut assignments = Vec::new();
        for (name, word) in &command.assignments {
            match self.expand_scalar(word) {
                Ok(value) => assignments.push((name.clone(), value)),
                Err(message) => return ExecResult::error(1, format!("isksh: {message}")),
            }
        }

        if command.words.is_empty() {
            for (name, value) in assignments {
                if let Err(message) = self.set_variable(&name, value, None, false) {
                    return ExecResult::error(1, message);
                }
            }
            return self.apply_redirections(command, input, ExecResult::status(0));
        }

        let mut words = Vec::new();
        for word in &command.words {
            match self.expand_word(word) {
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
                match fs::read(path) {
                    Ok(bytes) => command_input = bytes,
                    Err(error) => return ExecResult::error(1, format!("isksh: {error}")),
                }
            }
        }

        let name = words.remove(0);
        let is_special = is_special_builtin(&name);
        let has_temporary_assignments = !assignments.is_empty();
        let saved_variables = if is_special || !has_temporary_assignments {
            None
        } else {
            Some(self.variables.clone())
        };
        for (key, value) in assignments {
            if let Err(message) = self.set_variable(&key, value, Some(true), false) {
                return ExecResult::error(1, message);
            }
        }
        let mut result = if let Some(function) = self.functions.get(&name).cloned() {
            self.execute_function(&function, words, &command_input)
        } else if is_builtin(&name) {
            self.execute_builtin(&name, &words, &command_input)
        } else {
            self.execute_external(&name, &words, &command_input)
        };
        if let Some(previous) = saved_variables {
            self.variables = previous;
        }
        result = self.apply_redirections(command, &command_input, result);
        result
    }

    fn apply_redirections(
        &mut self,
        command: &SimpleCommand,
        _input: &[u8],
        mut result: ExecResult,
    ) -> ExecResult {
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
                    let path = match self.redirection_path(&redirection.target) {
                        Ok(path) => path,
                        Err(message) => return ExecResult::error(1, message),
                    };
                    let mut options = OpenOptions::new();
                    options.create(true).write(true);
                    if redirection.kind == RedirectionKind::Append {
                        options.append(true);
                    } else if redirection.kind != RedirectionKind::ReadWrite {
                        options.truncate(true);
                    } else {
                        options.read(true);
                    }
                    let data = if fd == 2 {
                        &result.stderr
                    } else {
                        &result.stdout
                    };
                    match options.open(path).and_then(|mut file| file.write_all(data)) {
                        Ok(()) => {
                            if fd == 2 {
                                result.stderr.clear();
                            } else {
                                result.stdout.clear();
                            }
                        }
                        Err(error) => return ExecResult::error(1, format!("isksh: {error}")),
                    }
                }
                RedirectionKind::DuplicateOutput | RedirectionKind::DuplicateInput => {
                    let target = match self.expand_scalar(&redirection.target) {
                        Ok(target) => target,
                        Err(message) => return ExecResult::error(1, message),
                    };
                    if fd == 2 && target == "1" {
                        result.stdout.extend_from_slice(&result.stderr);
                        result.stderr.clear();
                    } else if target != "-" {
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
        let mut result = self.execute_command(body, input);
        self.function_depth -= 1;
        self.positional = old_positional;
        if let Flow::Return(status) = result.flow {
            result.status = status;
            result.flow = Flow::None;
        }
        result
    }

    fn execute_external(&self, name: &str, arguments: &[String], input: &[u8]) -> ExecResult {
        let resolved_name = self.resolve_external_name(name);
        let mut process = platform_command(&resolved_name, arguments);
        process
            .current_dir(&self.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_clear();
        for (name, variable) in &self.variables {
            if variable.exported {
                process.env(name, &variable.value);
            }
        }
        let mut child = match process.spawn() {
            Ok(child) => child,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return ExecResult::error(127, format!("isksh: {name}: command not found"));
            }
            Err(error) => return ExecResult::error(126, format!("isksh: {name}: {error}")),
        };
        if let Some(mut stdin) = child.stdin.take()
            && let Err(error) = stdin.write_all(input)
        {
            return ExecResult::error(1, format!("isksh: {error}"));
        }
        match child.wait_with_output() {
            Ok(output) => ExecResult {
                status: exit_status(&output.status),
                stdout: output.stdout,
                stderr: output.stderr,
                flow: Flow::None,
            },
            Err(error) => ExecResult::error(126, format!("isksh: {name}: {error}")),
        }
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
            "." => self.builtin_dot(args, input),
            "exec" | "command" => {
                if args.is_empty() {
                    ExecResult::status(0)
                } else if is_builtin(&args[0]) {
                    self.execute_builtin(&args[0], &args[1..], input)
                } else {
                    self.execute_external(&args[0], &args[1..], input)
                }
            }
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
            "trap" | "hash" | "umask" => ExecResult::status(0),
            _ => ExecResult::error(127, format!("isksh: {name}: unsupported builtin")),
        }
    }

    fn builtin_cd(&mut self, args: &[String]) -> ExecResult {
        let target = args
            .first()
            .cloned()
            .or_else(|| self.value_of("HOME"))
            .unwrap_or_else(|| ".".into());
        let path = self.resolve_path(&target);
        match fs::canonicalize(path) {
            Ok(path) if path.is_dir() => {
                self.cwd = path;
                ExecResult::status(0)
            }
            Ok(_) => ExecResult::error(1, format!("isksh: cd: {target}: not a directory")),
            Err(error) => ExecResult::error(1, format!("isksh: cd: {target}: {error}")),
        }
    }

    fn builtin_export(&mut self, args: &[String], readonly: bool) -> ExecResult {
        if args.is_empty() {
            let mut names: Vec<_> = self
                .variables
                .iter()
                .filter(|(_, value)| value.exported || readonly && value.readonly)
                .collect();
            names.sort_by_key(|(name, _)| *name);
            let stdout = names
                .into_iter()
                .map(|(name, value)| {
                    format!("export {name}='{}'\n", value.value.replace('\'', "'\\''"))
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
                Some(!readonly),
                readonly,
            ) {
                return ExecResult::error(1, message);
            }
            if let Some(variable) = self.variables.get_mut(name) {
                if readonly {
                    variable.readonly = true;
                } else {
                    variable.exported = true;
                }
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
        let line = String::from_utf8_lossy(input)
            .lines()
            .next()
            .unwrap_or_default()
            .to_string();
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
                        value.push_str(&self.value_of("HOME").unwrap_or_else(|| "~".into()));
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
                return match operator.trim_start_matches(':') {
                    "-" => Ok(if missing {
                        word.to_string()
                    } else {
                        current.unwrap_or_default()
                    }),
                    "+" => Ok(if missing {
                        String::new()
                    } else {
                        word.to_string()
                    }),
                    "=" => {
                        if missing {
                            self.set_variable(name, word.to_string(), None, false)?;
                            Ok(word.to_string())
                        } else {
                            Ok(current.unwrap_or_default())
                        }
                    }
                    "?" => {
                        if missing {
                            Err(if word.is_empty() {
                                format!("{name}: parameter is unset or null")
                            } else {
                                word.to_string()
                            })
                        } else {
                            Ok(current.unwrap_or_default())
                        }
                    }
                    _ => unreachable!(),
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
                        if arithmetic && depth == 1 && chars.get(index + 1) == Some(&')') {
                            break;
                        }
                        depth -= 1;
                        if depth == 0 {
                            break;
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
                    let result = self.clone().run(&expression, &[]);
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
        )
}

fn valid_variable_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(ch) if ch.is_ascii_alphabetic() || ch == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
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
}
