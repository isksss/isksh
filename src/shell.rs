use crate::ast::*;
use crate::i18n::{abbreviation_help, builtin_description, localize};
use crate::parser::parse;
use chrono::Local;
use glob::{MatchOptions, Pattern, glob_with};
use regex::Regex;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
#[cfg(windows)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// `PROCESS_SUBSTITUTION_ID`で使用する値を保持する定数。
static PROCESS_SUBSTITUTION_ID: AtomicU64 = AtomicU64::new(0);
/// `BACKGROUND_JOB_ID`で使用する値を保持する定数。
static BACKGROUND_JOB_ID: AtomicU32 = AtomicU32::new(1);
#[cfg(windows)]
/// `WINDOWS_CHILD_FOREGROUND`で使用する値を保持する定数。
static WINDOWS_CHILD_FOREGROUND: AtomicBool = AtomicBool::new(false);

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

struct PreparedExternal {
    name: String,
    arguments: Vec<String>,
    assignments: Vec<(String, String)>,
}

type BackgroundJobs = Arc<Mutex<BTreeMap<u32, std::thread::JoinHandle<ExecResult>>>>;

#[derive(Debug, Clone, Default)]
struct LocalScope {
    variables: HashMap<String, Option<Variable>>,
    indexed_arrays: HashMap<String, Option<BTreeMap<usize, String>>>,
    associative_arrays: HashMap<String, Option<BTreeMap<String, String>>>,
    shell_options: Option<HashSet<String>>,
}

impl ExecResult {
    /// `status`に対応する処理を行う。
    fn status(status: i32) -> Self {
        Self {
            status,
            stdout: Vec::new(),
            stderr: Vec::new(),
            flow: Flow::None,
        }
    }

    /// `error`に対応する処理を行う。
    fn error(status: i32, message: impl AsRef<str>) -> Self {
        let mut result = Self::status(status);
        let message = localize(message);
        result.stderr.extend_from_slice(message.as_bytes());
        if !message.ends_with('\n') {
            result.stderr.push(b'\n');
        }
        result
    }

    /// `append`に対応する処理を行う。
    fn append(&mut self, mut other: ExecResult) {
        self.status = other.status;
        self.stdout.append(&mut other.stdout);
        self.stderr.append(&mut other.stderr);
        if other.flow != Flow::None {
            self.flow = other.flow;
        }
    }
}

/// シェルソースを実行した結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunResult {
    /// コマンドの終了ステータス。
    pub status: i32,
    /// 標準出力へ書き込まれたバイト列。
    pub stdout: Vec<u8>,
    /// 標準エラー出力へ書き込まれたバイト列。
    pub stderr: Vec<u8>,
}

/// ソース断片を実行できる状態かどうかを表す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputState {
    /// ソースの構文が完結している。
    Complete,
    /// ソースに追加の入力が必要である。
    Incomplete,
    /// ソースが無効で、関連する診断メッセージを保持している。
    Invalid(String),
}

/// [`Shell`]が提供する互換動作を選択する。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ShellMode {
    /// デフォルトのBash指向互換動作を有効にする。
    #[default]
    Bash,
    /// zsh指向のプロンプト、組み込みコマンド、フックを有効にする。
    Zsh,
}

impl ShellMode {
    /// `ISKSH_MODE`環境変数の値からモードを解決する。
    ///
    /// 値が正確に`zsh`の場合だけ[`ShellMode::Zsh`]を選択する。
    /// 未設定または未対応の値は[`ShellMode::Bash`]へフォールバックする。
    pub fn from_environment(value: Option<&str>) -> Self {
        match value {
            Some("zsh") => Self::Zsh,
            _ => Self::Bash,
        }
    }

    /// `ISKSH_MODE`環境変数で使用する正規値を返す。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
        }
    }
}

/// 状態を保持するシェルインタプリタ。
///
/// `Shell`は[`Shell::run`]の呼び出し間で変数、関数、別名、オプション、
/// 作業ディレクトリなどの実行状態を保持する。
#[derive(Debug, Clone)]
pub struct Shell {
    variables: HashMap<String, Variable>,
    positional: Vec<String>,
    name: String,
    last_status: i32,
    functions: HashMap<String, Command>,
    aliases: HashMap<String, String>,
    global_aliases: HashMap<String, String>,
    suffix_aliases: HashMap<String, String>,
    abbreviations: HashMap<String, String>,
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
    background_jobs: BackgroundJobs,
    last_background_job: Option<u32>,
    open_descriptors: HashMap<u32, OutputSink>,
    traps: HashMap<String, String>,
    trap_depth: usize,
    prompt_number: u64,
    command_hash: HashMap<String, String>,
    creation_mask: u32,
    directory_stack: Vec<PathBuf>,
    mode: ShellMode,
    precmd_hooks: Vec<String>,
    chpwd_hooks: Vec<String>,
    preexec_hooks: Vec<String>,
    periodic_hooks: Vec<String>,
    zshaddhistory_hooks: Vec<String>,
    zshexit_hooks: Vec<String>,
    autoload_functions: HashSet<String>,
    loaded_modules: HashSet<String>,
    function_stack: Vec<String>,
    function_modes: HashMap<String, ShellMode>,
    traced_functions: HashSet<String>,
    zstyles: BTreeMap<(String, String), Vec<String>>,
    key_bindings: BTreeMap<(String, String), String>,
    zle_widgets: BTreeMap<String, String>,
    completion_definitions: BTreeMap<String, String>,
    completion_candidates: Vec<String>,
    zsh_hook_depth: usize,
}

impl Default for Shell {
    /// `default`に対応する処理を行う。
    fn default() -> Self {
        Self::new("isksh")
    }
}

impl Shell {
    /// 現在のプロセス環境と作業ディレクトリを使用してシェルを生成する。
    ///
    /// `name`はシェルの`$0`になる。初期互換モードは`ISKSH_MODE`から解決し、
    /// デフォルトではBash互換動作を使用する。
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
        let mode = ShellMode::from_environment(std::env::var("ISKSH_MODE").ok().as_deref());
        variables.insert(
            "ISKSH_MODE".into(),
            Variable {
                value: mode.as_str().into(),
                exported: true,
                readonly: false,
            },
        );
        let shell_options = if mode == ShellMode::Zsh {
            HashSet::from(["nomatch".to_string()])
        } else {
            HashSet::new()
        };
        Self {
            variables,
            positional: Vec::new(),
            name: name.into(),
            last_status: 0,
            functions: HashMap::new(),
            aliases: HashMap::new(),
            global_aliases: HashMap::new(),
            suffix_aliases: HashMap::new(),
            abbreviations: HashMap::new(),
            indexed_arrays: HashMap::new(),
            associative_arrays: HashMap::new(),
            shell_options,
            pending_process_substitutions: Vec::new(),
            local_scopes: Vec::new(),
            expanding_aliases: Vec::new(),
            cwd,
            loop_depth: 0,
            function_depth: 0,
            getopts_offset: 1,
            exit_status: None,
            terminal_io: false,
            background_jobs: Arc::new(Mutex::new(BTreeMap::new())),
            last_background_job: None,
            open_descriptors: HashMap::new(),
            traps: HashMap::new(),
            trap_depth: 0,
            prompt_number: 0,
            command_hash: HashMap::new(),
            creation_mask: 0o022,
            directory_stack: Vec::new(),
            mode,
            precmd_hooks: Vec::new(),
            chpwd_hooks: Vec::new(),
            preexec_hooks: Vec::new(),
            periodic_hooks: Vec::new(),
            zshaddhistory_hooks: Vec::new(),
            zshexit_hooks: Vec::new(),
            autoload_functions: HashSet::new(),
            loaded_modules: HashSet::new(),
            function_stack: Vec::new(),
            function_modes: HashMap::new(),
            traced_functions: HashSet::new(),
            zstyles: BTreeMap::new(),
            key_bindings: BTreeMap::new(),
            zle_widgets: BTreeMap::new(),
            completion_definitions: BTreeMap::new(),
            completion_candidates: Vec::new(),
            zsh_hook_depth: 0,
        }
    }

    /// 有効な互換モードを返す。
    pub fn mode(&self) -> ShellMode {
        self.mode
    }

    /// シェルの`ISKSH_MODE`変数から互換モードを再読み込みする。
    ///
    /// 未対応の値は`bash`へ正規化し、シェル環境へ再度exportする。
    pub fn refresh_mode(&mut self) {
        self.mode = ShellMode::from_environment(self.value_of("ISKSH_MODE").as_deref());
        if self.mode == ShellMode::Zsh {
            self.shell_options.insert("nomatch".into());
        }
        let _ = self.set_variable("ISKSH_MODE", self.mode.as_str().into(), Some(true), false);
    }

    /// シェルの位置パラメータを置き換える。
    pub fn set_positional(&mut self, values: Vec<String>) {
        self.positional = values;
    }

    /// 現在の実行ファイル検索パスを返し、`PATH`が未設定なら`None`を返す。
    pub fn command_search_path(&self) -> Option<String> {
        self.value_of("PATH")
    }

    /// 設定済みの別名、短縮入力、関数の名前を返す。
    ///
    /// 返却順は未規定で、複数の設定分類に同じ名前がある場合は重複を含む。
    pub fn configured_command_names(&self) -> Vec<String> {
        self.aliases
            .keys()
            .chain(self.abbreviations.keys())
            .chain(self.functions.keys())
            .chain(self.autoload_functions.iter())
            .cloned()
            .collect()
    }

    /// 設定済みの対話用短縮入力をすべて複製して返す。
    pub fn configured_abbreviations(&self) -> HashMap<String, String> {
        self.abbreviations.clone()
    }

    /// zsh互換の`compadd`組み込みコマンドで追加された補完候補を返す。
    pub fn configured_completion_candidates(&self) -> Vec<String> {
        self.completion_candidates.clone()
    }

    /// zshの履歴オプションとフックを適用し、行を保存するかどうかを返す。
    pub fn record_history(&mut self, source: &str) -> bool {
        if self.mode != ShellMode::Zsh {
            return true;
        }
        if self.shell_options.contains("histignorespace")
            && source.chars().next().is_some_and(char::is_whitespace)
        {
            return false;
        }
        let line = if self.shell_options.contains("histreduceblanks") {
            source.split_whitespace().collect::<Vec<_>>().join(" ")
        } else {
            source.to_string()
        };
        if self.shell_options.contains("histignoredups")
            && self.value_of("ZSH_LAST_HISTORY").as_deref() == Some(&line)
        {
            return false;
        }
        let _ = self.set_variable("ZSH_HISTORY_LINE", line.clone(), None, false);
        let result = self.run_zsh_hooks(self.zshaddhistory_hooks.clone());
        if result.status != 0 {
            return false;
        }
        let _ = self.set_variable("ZSH_LAST_HISTORY", line, None, false);
        true
    }

    /// 対話用ソース内のコマンド位置にある短縮入力を展開する。
    pub fn expand_abbreviations(&self, source: &str) -> String {
        let mut output = String::with_capacity(source.len());
        let mut rest = source;
        let mut command_position = true;
        while !rest.is_empty() {
            let whitespace = rest
                .find(|character: char| !character.is_whitespace())
                .unwrap_or(rest.len());
            output.push_str(&rest[..whitespace]);
            if rest[..whitespace].contains('\n') {
                command_position = true;
            }
            rest = &rest[whitespace..];
            if rest.is_empty() {
                break;
            }
            if rest.starts_with([';', '|', '&', '(', ')']) {
                let length = rest.chars().next().unwrap().len_utf8();
                output.push_str(&rest[..length]);
                rest = &rest[length..];
                command_position = true;
                continue;
            }
            let mut quoted = false;
            let mut quote = None;
            let mut escaped = false;
            let mut length = rest.len();
            for (index, character) in rest.char_indices() {
                if escaped {
                    escaped = false;
                } else if let Some(delimiter) = quote {
                    if character == delimiter {
                        quote = None;
                    } else if delimiter == '"' && character == '\\' {
                        escaped = true;
                    }
                } else if matches!(character, '\'' | '"') {
                    quoted = true;
                    quote = Some(character);
                } else if character == '\\' {
                    quoted = true;
                    escaped = true;
                } else if character.is_whitespace() || ";|&()".contains(character) {
                    length = index;
                    break;
                }
            }
            let word = &rest[..length];
            if command_position
                && !quoted
                && let Some(expansion) = self.abbreviations.get(word)
            {
                output.push_str(expansion);
            } else {
                output.push_str(word);
            }
            if !word
                .split_once('=')
                .is_some_and(|(name, _)| valid_variable_name(name))
            {
                command_position = false;
            }
            rest = &rest[length..];
        }
        output
    }

    /// `expand_zsh_aliases`に対応する処理を行う。
    fn expand_zsh_aliases(&self, source: &str) -> String {
        let mut output = String::with_capacity(source.len());
        let mut word = String::new();
        let mut quote = None;
        let mut escaped = false;
        let flush = |word: &mut String, output: &mut String| {
            if word.is_empty() {
                return;
            }
            if let Some(expansion) = self.global_aliases.get(word) {
                output.push_str(expansion);
            } else {
                output.push_str(word);
            }
            word.clear();
        };
        for character in source.chars() {
            if escaped {
                word.push(character);
                escaped = false;
            } else if let Some(delimiter) = quote {
                word.push(character);
                if character == delimiter {
                    quote = None;
                } else if delimiter == '"' && character == '\\' {
                    escaped = true;
                }
            } else if matches!(character, '\'' | '"') {
                word.push(character);
                quote = Some(character);
            } else if character == '\\' {
                word.push(character);
                escaped = true;
            } else if character.is_whitespace() || ";|&()<>".contains(character) {
                flush(&mut word, &mut output);
                output.push(character);
            } else {
                word.push(character);
            }
        }
        flush(&mut word, &mut output);
        output
    }

    /// フォアグラウンドの外部コマンドが端末を直接使用できるか設定する。
    pub fn set_interactive(&mut self, interactive: bool) {
        self.terminal_io = interactive;
        if interactive {
            install_console_control_handler();
        }
    }

    /// ソース文字列を完結、不完全、無効のいずれかに分類する。
    pub fn check_input(source: &str) -> InputState {
        match parse(source) {
            Ok(_) => InputState::Complete,
            Err(error) if error.incomplete => InputState::Incomplete,
            Err(error) => InputState::Invalid(localize(error.to_string())),
        }
    }

    /// 現在のシェル状態から一次または継続プロンプトを生成する。
    ///
    /// 一次プロンプトでは、プロンプトのエスケープと置換を展開する前に、
    /// 設定済みのプロンプトフックと`PROMPT_COMMAND`を実行する。
    pub fn prompt(&mut self, continuation: bool) -> String {
        let saved_status = self.last_status;
        let mut prefix = String::new();
        if !continuation && self.mode == ShellMode::Zsh {
            let result = self.run_zsh_hooks(self.periodic_hooks.clone());
            prefix.push_str(&String::from_utf8_lossy(&result.stdout));
            let result = self.run_zsh_hooks(self.precmd_hooks.clone());
            prefix.push_str(&String::from_utf8_lossy(&result.stdout));
        }
        self.last_status = saved_status;
        let names = match (self.mode, continuation) {
            (ShellMode::Zsh, false) => ["PROMPT", "PS1"],
            (ShellMode::Zsh, true) => ["PROMPT2", "PS2"],
            (ShellMode::Bash, false) => ["PS1", "PS1"],
            (ShellMode::Bash, true) => ["PS2", "PS2"],
        };
        let default = || {
            if continuation {
                "> ".to_string()
            } else {
                "$ ".to_string()
            }
        };
        if !continuation {
            self.prompt_number += 1;
        }
        if !continuation
            && let Some(command) = self.value_of("PROMPT_COMMAND")
            && !command.is_empty()
        {
            let result = self.run(&command, &[]);
            prefix.push_str(&String::from_utf8_lossy(&result.stdout));
        }
        self.last_status = saved_status;
        let value = self
            .value_of(names[0])
            .or_else(|| self.value_of(names[1]))
            .unwrap_or_else(default);
        let escaped = if self.mode == ShellMode::Zsh {
            self.expand_zsh_prompt_escapes(&value, saved_status)
        } else {
            self.expand_prompt_escapes(&value)
        };
        let expanded = if self.mode == ShellMode::Bash || self.shell_options.contains("promptsubst")
        {
            self.expand_here_document(&escaped)
                .unwrap_or_else(|_| escaped.clone())
        } else {
            escaped
        };
        prefix.push_str(&expanded);
        self.last_status = saved_status;
        prefix
    }

    /// zsh互換の右プロンプト（`RPROMPT`または`RPS1`）を展開する。
    pub fn right_prompt(&self) -> String {
        if self.mode != ShellMode::Zsh {
            return String::new();
        }
        let value = self
            .value_of("RPROMPT")
            .or_else(|| self.value_of("RPS1"))
            .unwrap_or_default();
        self.expand_zsh_prompt_escapes(&value, self.last_status)
    }

    /// 実行済みの`exit`組み込みコマンドが要求した終了ステータスを取得する。
    ///
    /// この呼び出し後に保存値を消去する。
    pub fn take_exit_status(&mut self) -> Option<i32> {
        self.exit_status.take()
    }

    /// 指定された標準入力バイト列を使ってシェルソースを解析・実行する。
    ///
    /// 実行状態は後続の呼び出しへ保持する。構文エラーと実行時エラーはRustの
    /// エラーではなく[`RunResult`]で返す。
    pub fn run(&mut self, source: &str, input: &[u8]) -> RunResult {
        if let Some(result) = self.apply_known_bash_integration(source) {
            self.last_status = result.status;
            return RunResult {
                status: result.status,
                stdout: result.stdout,
                stderr: result.stderr,
            };
        }
        let expanded_source;
        let source = if self.mode == ShellMode::Zsh {
            expanded_source = normalize_zsh_function_syntax(&self.expand_zsh_aliases(source));
            expanded_source.as_str()
        } else {
            source
        };
        let script = match parse(source) {
            Ok(script) => script,
            Err(error) => {
                return RunResult {
                    status: 2,
                    stdout: Vec::new(),
                    stderr: format!("{}\n", localize(format!("isksh: {error}"))).into_bytes(),
                };
            }
        };
        let mut result = self.execute_script(&script, input);
        let status = match result.flow {
            Flow::Exit(status) => {
                let mut trap = self.run_trap("EXIT");
                result.stdout.append(&mut trap.stdout);
                result.stderr.append(&mut trap.stderr);
                if self.mode == ShellMode::Zsh {
                    let mut hooks = self.run_zsh_hooks(self.zshexit_hooks.clone());
                    result.stdout.append(&mut hooks.stdout);
                    result.stderr.append(&mut hooks.stderr);
                }
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

    /// `execute_script`に対応する処理を行う。
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

    /// `execute_and_or`に対応する処理を行う。
    fn execute_and_or(&mut self, list: &AndOr, input: &[u8]) -> ExecResult {
        let mut result = if list.background {
            let mut child = self.clone();
            child.terminal_io = false;
            child.background_jobs = Arc::new(Mutex::new(BTreeMap::new()));
            let pipeline = list.first.clone();
            let input = input.to_vec();
            let job_id = BACKGROUND_JOB_ID.fetch_add(1, Ordering::Relaxed);
            let handle = std::thread::spawn(move || child.execute_pipeline(&pipeline, &input));
            self.background_jobs
                .lock()
                .expect("background jobs lock")
                .insert(job_id, handle);
            self.last_background_job = Some(job_id);
            ExecResult::status(0)
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

    /// `execute_pipeline`に対応する処理を行う。
    fn execute_pipeline(&mut self, pipeline: &Pipeline, input: &[u8]) -> ExecResult {
        if pipeline.commands.len() > 1
            && let Some(prepared) = self.prepare_external_pipeline(pipeline)
        {
            return match prepared {
                Ok(commands) => self.execute_external_pipeline(commands, input, pipeline.negated),
                Err(message) => ExecResult::error(1, message),
            };
        }
        let mut pipe_input = input.to_vec();
        let mut all_stderr = Vec::new();
        let mut last = ExecResult::status(0);
        let mut statuses = Vec::new();
        for (index, command) in pipeline.commands.iter().enumerate() {
            let is_last = index + 1 == pipeline.commands.len();
            let mut result = if pipeline.commands.len() == 1 {
                self.execute_command(command, &pipe_input)
            } else {
                let mut child = self.clone();
                child.terminal_io = false;
                child.execute_command(command, &pipe_input)
            };
            statuses.push(result.status);
            all_stderr.append(&mut result.stderr);
            if is_last {
                last = result;
            } else {
                pipe_input = result.stdout;
            }
        }
        last.stderr.splice(0..0, all_stderr);
        self.set_pipeline_statuses(&statuses);
        if self.shell_options.contains("pipefail")
            && let Some(status) = statuses.iter().rev().find(|status| **status != 0)
        {
            last.status = *status;
        }
        if pipeline.negated {
            last.status = i32::from(last.status == 0);
        }
        last
    }

    /// `prepare_external_pipeline`に対応する処理を行う。
    fn prepare_external_pipeline(
        &mut self,
        pipeline: &Pipeline,
    ) -> Option<Result<Vec<PreparedExternal>, String>> {
        let mut simple_commands = Vec::new();
        for command in &pipeline.commands {
            let Command::Simple(command) = command else {
                return None;
            };
            if !command.redirections.is_empty()
                || !command.array_assignments.is_empty()
                || command.words.is_empty()
            {
                return None;
            }
            simple_commands.push(command);
            let name = command.words[0].as_plain_literal()?;
            if is_builtin(name)
                || self.functions.contains_key(name)
                || self.aliases.contains_key(name)
            {
                return None;
            }
        }
        Some(
            simple_commands
                .iter()
                .map(|command| {
                    let mut assignments = Vec::new();
                    for (name, word) in &command.assignments {
                        assignments.push((name.clone(), self.expand_scalar(word)?));
                    }
                    let mut words = Vec::new();
                    for word in &command.words {
                        words.extend(self.expand_word(word)?);
                    }
                    Ok(PreparedExternal {
                        name: words.remove(0),
                        arguments: words,
                        assignments,
                    })
                })
                .collect(),
        )
    }

    /// `execute_external_pipeline`に対応する処理を行う。
    fn execute_external_pipeline(
        &mut self,
        commands: Vec<PreparedExternal>,
        input: &[u8],
        negated: bool,
    ) -> ExecResult {
        let interactive = self.terminal_io;
        let mut children: Vec<std::process::Child> = Vec::new();
        let mut previous_stdout = None;
        let mut stderr_readers = Vec::new();
        for (index, prepared) in commands.iter().enumerate() {
            let last = index + 1 == commands.len();
            let resolved_name = self.resolve_external_name(&prepared.name);
            let mut process = platform_command(&resolved_name, &prepared.arguments);
            configure_process_group(&mut process, children.first().map(std::process::Child::id));
            process.current_dir(&self.cwd).env_clear();
            for (name, variable) in &self.variables {
                if variable.exported {
                    process.env(name, &variable.value);
                }
            }
            process.envs(prepared.assignments.iter().cloned());
            if index == 0 {
                process.stdin(if interactive && input.is_empty() {
                    Stdio::inherit()
                } else {
                    Stdio::piped()
                });
            } else {
                process.stdin(Stdio::from(
                    previous_stdout
                        .take()
                        .expect("previous pipeline command has stdout"),
                ));
            }
            process.stdout(if last && interactive {
                Stdio::inherit()
            } else {
                Stdio::piped()
            });
            process.stderr(if interactive {
                Stdio::inherit()
            } else {
                Stdio::piped()
            });
            let mut child = match process.spawn() {
                Ok(child) => child,
                Err(error) => {
                    for mut child in children {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                    let status = if error.kind() == std::io::ErrorKind::NotFound {
                        127
                    } else {
                        126
                    };
                    return ExecResult::error(status, format!("isksh: {}: {error}", prepared.name));
                }
            };
            previous_stdout = child.stdout.take();
            if let Some(mut stderr) = child.stderr.take() {
                stderr_readers.push(std::thread::spawn(move || {
                    let mut output = Vec::new();
                    let _ = stderr.read_to_end(&mut output);
                    output
                }));
            }
            children.push(child);
        }
        let foreground_group = children.first().map(std::process::Child::id);
        if interactive && let Some(group) = foreground_group {
            set_foreground_process_group(group);
        }
        let stdin_writer = children
            .first_mut()
            .and_then(|child| child.stdin.take())
            .map(|mut stdin| {
                let input = input.to_vec();
                std::thread::spawn(move || stdin.write_all(&input))
            });
        let stdout_reader = previous_stdout.map(|mut stdout| {
            std::thread::spawn(move || {
                let mut output = Vec::new();
                let _ = stdout.read_to_end(&mut output);
                output
            })
        });
        let mut statuses = Vec::new();
        for mut child in children {
            statuses.push(pipeline_wait_status(child.wait()));
        }
        if interactive && foreground_group.is_some() {
            restore_shell_process_group();
        }
        if let Some(writer) = stdin_writer {
            let _ = writer.join();
        }
        let stdout = stdout_reader
            .and_then(|reader| reader.join().ok())
            .unwrap_or_default();
        let mut stderr = Vec::new();
        for reader in stderr_readers {
            if let Ok(mut output) = reader.join() {
                stderr.append(&mut output);
            }
        }
        self.set_pipeline_statuses(&statuses);
        let mut status = statuses.last().copied().unwrap_or_default();
        if self.shell_options.contains("pipefail")
            && let Some(failed) = statuses.iter().rev().find(|status| **status != 0)
        {
            status = *failed;
        }
        if negated {
            status = i32::from(status == 0);
        }
        ExecResult {
            status,
            stdout,
            stderr,
            flow: Flow::None,
        }
    }

    /// `set_pipeline_statuses`に対応する処理を行う。
    fn set_pipeline_statuses(&mut self, statuses: &[i32]) {
        self.indexed_arrays.insert(
            "PIPESTATUS".into(),
            statuses
                .iter()
                .enumerate()
                .map(|(index, status)| (index, status.to_string()))
                .collect(),
        );
    }

    /// `execute_command`に対応する処理を行う。
    fn execute_command(&mut self, command: &Command, input: &[u8]) -> ExecResult {
        match command {
            Command::Simple(command) => {
                let mut result = self.run_trap("DEBUG");
                if self.mode == ShellMode::Zsh && self.zsh_hook_depth == 0 {
                    result.append(self.run_zsh_hooks(self.preexec_hooks.clone()));
                }
                result.append(self.execute_simple(command, input));
                result
            }
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
                } else {
                    output.status = 0;
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
                self.function_modes.insert(name.clone(), self.mode);
                ExecResult::status(0)
            }
        }
    }

    /// `run_zsh_hooks`に対応する処理を行う。
    fn run_zsh_hooks(&mut self, hooks: Vec<String>) -> ExecResult {
        let mut output = ExecResult::status(0);
        self.zsh_hook_depth += 1;
        for hook in hooks {
            output.append(self.execute_eval(&[hook], &[]));
        }
        self.zsh_hook_depth -= 1;
        output
    }

    /// `execute_loop`に対応する処理を行う。
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

    /// `execute_for`に対応する処理を行う。
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

    /// `execute_case`に対応する処理を行う。
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

    /// `execute_simple`に対応する処理を行う。
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
            self.sync_zsh_tied_array(name);
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
            ) && redirection.fd.unwrap_or(0) == 0
            {
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
            } else if redirection.kind == RedirectionKind::DuplicateInput
                && redirection.fd.unwrap_or(0) == 0
            {
                let target = match self.expand_scalar(&redirection.target) {
                    Ok(target) => target,
                    Err(message) => return ExecResult::error(1, message),
                };
                if target == "-" {
                    command_input.clear();
                    continue;
                }
                let target_fd = match target.parse::<u32>() {
                    Ok(fd) => fd,
                    Err(_) => return ExecResult::error(1, "isksh: invalid input descriptor"),
                };
                match self.open_descriptors.get(&target_fd) {
                    Some(OutputSink::File(path)) => match fs::read(path) {
                        Ok(bytes) => command_input = bytes,
                        Err(error) => return ExecResult::error(1, format!("isksh: {error}")),
                    },
                    Some(OutputSink::Closed) => command_input.clear(),
                    _ => {
                        return ExecResult::error(
                            1,
                            format!("isksh: {target_fd}: bad input descriptor"),
                        );
                    }
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
        if self.mode == ShellMode::Zsh
            && assignments.is_empty()
            && let Some((_, suffix)) = name.rsplit_once('.')
            && let Some(replacement) = self.suffix_aliases.get(suffix).cloned()
        {
            let mut source = format!("{replacement} {}", shell_quote(&name));
            for argument in &words {
                source.push(' ');
                source.push_str(&shell_quote(argument));
            }
            let result = self.execute_eval(&[source], &command_input);
            return self.apply_redirections(command, &command_input, result);
        }
        if self.mode == ShellMode::Zsh
            && assignments.is_empty()
            && words.is_empty()
            && self.shell_options.contains("autocd")
            && self.resolve_path(&name).is_dir()
        {
            return self.builtin_cd(&[name]);
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
        if self.mode == ShellMode::Zsh && self.autoload_functions.contains(&name) {
            let loaded = self.load_autoload_function(&name);
            if loaded.status != 0 {
                return loaded;
            }
        }
        let mut result = if let Some(function) = self.functions.get(&name).cloned() {
            self.execute_function(&name, &function, words, &command_input)
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

    /// `apply_redirections`に対応する処理を行う。
    fn apply_redirections(
        &mut self,
        command: &SimpleCommand,
        _input: &[u8],
        mut result: ExecResult,
    ) -> ExecResult {
        let mut descriptors = self.open_descriptors.clone();
        descriptors.entry(1).or_insert(OutputSink::Stdout);
        descriptors.entry(2).or_insert(OutputSink::Stderr);
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
                    descriptors.insert(fd, OutputSink::File(path));
                }
                RedirectionKind::DuplicateOutput | RedirectionKind::DuplicateInput => {
                    let target = match self.expand_scalar(&redirection.target) {
                        Ok(target) => target,
                        Err(message) => return ExecResult::error(1, message),
                    };
                    let sink = if target == "-" {
                        OutputSink::Closed
                    } else {
                        let target_fd = match target.parse::<u32>() {
                            Ok(target_fd) => target_fd,
                            Err(_) => {
                                return ExecResult::error(
                                    1,
                                    "isksh: invalid file descriptor duplication",
                                );
                            }
                        };
                        match descriptors.get(&target_fd).cloned() {
                            Some(sink) => sink,
                            None => {
                                return ExecResult::error(
                                    1,
                                    format!("isksh: {target_fd}: bad file descriptor"),
                                );
                            }
                        }
                    };
                    descriptors.insert(fd, sink);
                }
                RedirectionKind::HereDocument | RedirectionKind::HereDocumentStrip => {}
                RedirectionKind::Input => {
                    if fd != 0 {
                        let path = match self.redirection_path(&redirection.target) {
                            Ok(path) => path,
                            Err(message) => return ExecResult::error(1, message),
                        };
                        if let Err(error) = OpenOptions::new().read(true).open(&path) {
                            return ExecResult::error(1, format!("isksh: {error}"));
                        }
                        descriptors.insert(fd, OutputSink::File(path));
                    }
                }
            }
        }
        if command.words.first().and_then(Word::as_plain_literal) == Some("exec")
            && command.words.len() == 1
        {
            self.open_descriptors = descriptors.clone();
        }
        let stdout = std::mem::take(&mut result.stdout);
        let stderr = std::mem::take(&mut result.stderr);
        if let Err(error) = write_output_sink(
            descriptors.get(&1).unwrap_or(&OutputSink::Stdout),
            &stdout,
            &mut result.stdout,
            &mut result.stderr,
        ) {
            return ExecResult::error(1, format!("isksh: {error}"));
        }
        if let Err(error) = write_output_sink(
            descriptors.get(&2).unwrap_or(&OutputSink::Stderr),
            &stderr,
            &mut result.stdout,
            &mut result.stderr,
        ) {
            return ExecResult::error(1, format!("isksh: {error}"));
        }
        result
    }

    /// `execute_function`に対応する処理を行う。
    fn execute_function(
        &mut self,
        name: &str,
        body: &Command,
        arguments: Vec<String>,
        input: &[u8],
    ) -> ExecResult {
        let old_positional = std::mem::replace(&mut self.positional, arguments);
        let old_mode = self.mode;
        if let Some(mode) = self.function_modes.get(name).copied() {
            self.mode = mode;
        }
        self.function_depth += 1;
        self.function_stack.push(name.to_string());
        let shell_options = self
            .shell_options
            .contains("localoptions")
            .then(|| self.shell_options.clone());
        self.local_scopes.push(LocalScope {
            shell_options,
            ..LocalScope::default()
        });
        let mut result = self.execute_command(body, input);
        if self.traced_functions.contains(name) {
            result.stderr.splice(0..0, format!("+{name}\n").bytes());
        }
        let scope = self.local_scopes.pop().expect("function scope exists");
        restore_map(&mut self.variables, scope.variables);
        restore_map(&mut self.indexed_arrays, scope.indexed_arrays);
        restore_map(&mut self.associative_arrays, scope.associative_arrays);
        if let Some(options) = scope.shell_options {
            self.shell_options = options;
        }
        self.function_stack.pop();
        self.function_depth -= 1;
        self.positional = old_positional;
        self.mode = old_mode;
        if let Flow::Return(status) = result.flow {
            result.status = status;
            result.flow = Flow::None;
        }
        result
    }

    /// `execute_external`に対応する処理を行う。
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
            configure_process_group(&mut process, None);
            let mut child = match process.spawn() {
                Ok(child) => child,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return ExecResult::error(127, format!("isksh: {name}: command not found"));
                }
                Err(error) => return ExecResult::error(126, format!("isksh: {name}: {error}")),
            };
            set_foreground_process_group(child.id());
            let status = child.wait();
            restore_shell_process_group();
            return finish_external_status(name, status);
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

    /// `execute_builtin`に対応する処理を行う。
    fn execute_builtin(&mut self, name: &str, args: &[String], input: &[u8]) -> ExecResult {
        match name {
            ":" | "true" => ExecResult::status(0),
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
            "printf" => self.builtin_printf(args),
            "print" if self.mode == ShellMode::Zsh => self.builtin_print(args),
            "pwd" => {
                let mut value = self.cwd.to_string_lossy().into_owned().into_bytes();
                value.push(b'\n');
                ExecResult {
                    stdout: value,
                    ..ExecResult::status(0)
                }
            }
            "cd" | "chdir" => self.builtin_cd(args),
            "pushd" => self.builtin_pushd(args),
            "popd" => self.builtin_popd(args),
            "dirs" => self.builtin_dirs(args),
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
            "setopt" if self.mode == ShellMode::Zsh => self.builtin_setopt(args, true),
            "unsetopt" if self.mode == ShellMode::Zsh => self.builtin_setopt(args, false),
            "emulate" if self.mode == ShellMode::Zsh => self.builtin_emulate(args),
            "whence" | "which" | "where" if self.mode == ShellMode::Zsh => {
                self.builtin_whence(args)
            }
            "autoload" if self.mode == ShellMode::Zsh => self.builtin_autoload(args),
            "zmodload" if self.mode == ShellMode::Zsh => self.builtin_zmodload(args),
            "functions" if self.mode == ShellMode::Zsh => self.builtin_functions(args),
            "zstyle" if self.mode == ShellMode::Zsh => self.builtin_zstyle(args),
            "bindkey" if self.mode == ShellMode::Zsh => self.builtin_bindkey(args),
            "zle" if self.mode == ShellMode::Zsh => self.builtin_zle(args),
            "vared" if self.mode == ShellMode::Zsh => self.builtin_vared(args, input),
            "compinit" if self.mode == ShellMode::Zsh => ExecResult::status(0),
            "compdef" if self.mode == ShellMode::Zsh => self.builtin_compdef(args),
            "compadd" if self.mode == ShellMode::Zsh => self.builtin_compadd(args),
            "compset" if self.mode == ShellMode::Zsh => self.builtin_compset(args),
            "integer" | "float" | "private" if self.mode == ShellMode::Zsh => {
                self.builtin_declare("typeset", args)
            }
            "add-zsh-hook" if self.mode == ShellMode::Zsh => self.builtin_add_zsh_hook(args),
            "unfunction" if self.mode == ShellMode::Zsh => self.builtin_unfunction(args),
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
            "builtin" => self.builtin_builtin(args, input),
            "help" => self.builtin_help(args),
            "let" => self.builtin_let(args),
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
            "abbr" => self.builtin_abbr(args),
            "unalias" => self.builtin_unalias(args),
            "getopts" => self.builtin_getopts(args),
            "jobs" => self.builtin_jobs(),
            "wait" => self.builtin_wait(args),
            "times" => ExecResult {
                stdout: b"0m0.000s 0m0.000s\n0m0.000s 0m0.000s\n".to_vec(),
                ..ExecResult::status(0)
            },
            "hash" => self.builtin_hash(args),
            "trap" => self.builtin_trap(args),
            "umask" => self.builtin_umask(args),
            _ => ExecResult::error(127, format!("isksh: {name}: unsupported builtin")),
        }
    }

    /// `builtin_cd`に対応する処理を行う。
    fn builtin_cd(&mut self, args: &[String]) -> ExecResult {
        if args.len() > 1 {
            return ExecResult::error(1, "isksh: cd: too many arguments");
        }
        if let Some(argument) = args.first()
            && let Some(index) = directory_stack_index(argument, self.directory_stack.len() + 1)
        {
            let paths = std::iter::once(&self.cwd)
                .chain(self.directory_stack.iter())
                .cloned()
                .collect::<Vec<_>>();
            let target = &paths[index];
            let mut result = self.change_directory(&target.to_string_lossy());
            if result.status == 0 {
                result.stdout = format!("{}\n", self.cwd.display()).into_bytes();
            }
            return result;
        }
        let print_directory = args.first().map(String::as_str) == Some("-");
        let target = if print_directory {
            let Some(previous) = self.value_of("OLDPWD") else {
                return ExecResult::error(1, "isksh: cd: OLDPWD not set");
            };
            previous
        } else {
            args.first()
                .cloned()
                .or_else(|| self.value_of("HOME"))
                .unwrap_or(".".into())
        };
        let previous = self.cwd.clone();
        let mut result = self.change_directory(&target);
        if result.status == 0
            && self.mode == ShellMode::Zsh
            && self.shell_options.contains("autopushd")
        {
            self.push_directory(previous);
        }
        if result.status == 0 && print_directory {
            result.stdout = format!("{}\n", self.cwd.display()).into_bytes();
        }
        result
    }

    /// `change_directory`に対応する処理を行う。
    fn change_directory(&mut self, target: &str) -> ExecResult {
        let path = self.resolve_path(target);
        match fs::canonicalize(path) {
            Ok(path) if path.is_dir() => {
                let previous = self.cwd.to_string_lossy().into_owned();
                self.cwd = path;
                let current = self.cwd.to_string_lossy().into_owned();
                let _ = self.set_variable("OLDPWD", previous, Some(true), false);
                let _ = self.set_variable("PWD", current, Some(true), false);
                if self.mode == ShellMode::Zsh {
                    for hook in self.chpwd_hooks.clone() {
                        let _ = self.run(&hook, &[]);
                    }
                }
                ExecResult::status(0)
            }
            Ok(_) => ExecResult::error(1, format!("isksh: cd: {target}: not a directory")),
            Err(error) => ExecResult::error(1, format!("isksh: cd: {target}: {error}")),
        }
    }

    /// `builtin_print`に対応する処理を行う。
    fn builtin_print(&self, args: &[String]) -> ExecResult {
        let mut newline = true;
        let mut raw = false;
        let mut prompt = false;
        let mut line_mode = false;
        let mut nul_mode = false;
        let mut format = None;
        let mut array = None;
        let mut columns = None;
        let mut pattern = None;
        let mut sort = 0i8;
        let mut index = 0;
        while let Some(option) = args.get(index).map(String::as_str) {
            match option {
                "--" => {
                    index += 1;
                    break;
                }
                _ if option.starts_with('-') => {
                    let mut flags = option[1..].chars().peekable();
                    while let Some(flag) = flags.next() {
                        match flag {
                            'n' => newline = false,
                            'r' | 'R' => raw = true,
                            'P' => prompt = true,
                            'l' => line_mode = true,
                            'N' => nul_mode = true,
                            'b' | 'D' | 'i' | 'p' | 's' | 'S' | 'z' => {}
                            'o' => sort = 1,
                            'O' => sort = -1,
                            'a' | 'm' | 'C' | 'c' => {
                                let attached = flags.collect::<String>();
                                let value = if attached.is_empty() {
                                    index += 1;
                                    let Some(value) = args.get(index) else {
                                        return ExecResult::error(
                                            2,
                                            format!("isksh: print: -{flag} requires an argument"),
                                        );
                                    };
                                    value.clone()
                                } else {
                                    attached
                                };
                                match flag {
                                    'a' => array = Some(value),
                                    'm' => pattern = Some(value),
                                    _ => {
                                        columns =
                                            value.parse::<usize>().ok().filter(|value| *value > 0)
                                    }
                                }
                                break;
                            }
                            'f' => {
                                let attached = flags.collect::<String>();
                                if !attached.is_empty() {
                                    format = Some(attached);
                                } else {
                                    index += 1;
                                    let Some(value) = args.get(index) else {
                                        return ExecResult::error(
                                            2,
                                            "isksh: print: -f requires a format",
                                        );
                                    };
                                    format = Some(value.clone());
                                }
                                break;
                            }
                            _ => {
                                return ExecResult::error(
                                    2,
                                    format!("isksh: print: unsupported option: {option}"),
                                );
                            }
                        }
                    }
                }
                _ => break,
            }
            index += 1;
        }
        if let Some(format) = format {
            let mut values = vec![format];
            values.extend_from_slice(&args[index..]);
            return builtin_printf(&values);
        }
        let mut values = if let Some(name) = array {
            self.array_values(&name)
        } else {
            args[index..].to_vec()
        };
        if let Some(pattern) = pattern {
            let Ok(pattern) = Pattern::new(&pattern) else {
                return ExecResult::error(1, "isksh: print: invalid pattern");
            };
            values.retain(|value| pattern.matches(value));
        }
        if sort != 0 {
            values.sort();
            if sort < 0 {
                values.reverse();
            }
        }
        let separator = if nul_mode {
            "\0"
        } else if line_mode {
            "\n"
        } else {
            " "
        };
        let value = if let Some(columns) = columns {
            values
                .chunks(columns)
                .map(|row| row.join(" "))
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            values.join(separator)
        };
        let value = if prompt {
            self.expand_zsh_prompt_escapes(&value, self.last_status)
        } else if raw {
            value
        } else {
            decode_echo_escapes(&value)
        };
        let mut stdout = value.into_bytes();
        if nul_mode {
            stdout.push(0);
        } else if newline {
            stdout.push(b'\n');
        }
        ExecResult {
            stdout,
            ..ExecResult::status(0)
        }
    }

    /// `builtin_setopt`に対応する処理を行う。
    fn builtin_setopt(&mut self, args: &[String], enabled: bool) -> ExecResult {
        if args.is_empty() {
            let mut options: Vec<_> = self.shell_options.iter().cloned().collect();
            options.sort();
            return ExecResult {
                stdout: options.join("\n").into_bytes(),
                ..ExecResult::status(0)
            };
        }
        for option in args {
            let raw = option.to_ascii_lowercase().replace('_', "");
            let (normalized, is_inverted) = normalize_zsh_option(option);
            if raw.starts_with("nono") && !matches!(normalized.as_str(), "nomatch" | "notify") {
                let command = if enabled { "setopt" } else { "unsetopt" };
                return ExecResult::error(1, format!("isksh: {command}: no such option: {option}"));
            }
            if enabled != is_inverted {
                self.shell_options.insert(normalized);
            } else {
                self.shell_options.remove(&normalized);
            }
        }
        ExecResult::status(0)
    }

    /// `builtin_emulate`に対応する処理を行う。
    fn builtin_emulate(&mut self, args: &[String]) -> ExecResult {
        let mut shell = None;
        let mut command = None;
        let mut local = false;
        let mut reset = false;
        let mut index = 0;
        while let Some(argument) = args.get(index) {
            match argument.as_str() {
                "-L" => local = true,
                "-R" => reset = true,
                "-LR" | "-RL" => {
                    local = true;
                    reset = true;
                }
                "-c" => {
                    index += 1;
                    let Some(value) = args.get(index) else {
                        return ExecResult::error(2, "isksh: emulate: -c requires a command");
                    };
                    command = Some(value.clone());
                }
                value if !value.starts_with('-') && shell.is_none() => shell = Some(value),
                value => {
                    return ExecResult::error(
                        2,
                        format!("isksh: emulate: unsupported argument: {value}"),
                    );
                }
            }
            index += 1;
        }
        let Some(shell) = shell else {
            return ExecResult {
                stdout: format!("{}\n", self.mode.as_str()).into_bytes(),
                ..ExecResult::status(0)
            };
        };
        let saved_mode = self.mode;
        let saved_options = self.shell_options.clone();
        self.mode = match shell {
            "sh" | "ksh" | "csh" => ShellMode::Bash,
            _ => ShellMode::Zsh,
        };
        if reset {
            self.shell_options.clear();
            if self.mode == ShellMode::Zsh {
                self.shell_options.insert("nomatch".into());
            }
        }
        let _ = self.set_variable("ISKSH_MODE", self.mode.as_str().into(), Some(true), false);
        let had_command = command.is_some();
        let result = command.map_or_else(
            || ExecResult::status(0),
            |source| {
                let result = self.run(&source, &[]);
                ExecResult {
                    status: result.status,
                    stdout: result.stdout,
                    stderr: result.stderr,
                    flow: Flow::None,
                }
            },
        );
        if local || had_command {
            self.mode = saved_mode;
            self.shell_options = saved_options;
            let _ = self.set_variable("ISKSH_MODE", self.mode.as_str().into(), Some(true), false);
        }
        result
    }

    /// `builtin_whence`に対応する処理を行う。
    fn builtin_whence(&self, args: &[String]) -> ExecResult {
        let mut verbose = false;
        let mut word = false;
        let mut all = false;
        let mut external_only = false;
        let mut function_only = false;
        let mut pattern_mode = false;
        let mut path_only = false;
        let mut index = 0;
        while let Some(argument) = args.get(index) {
            match argument.as_str() {
                "--" => {
                    index += 1;
                    break;
                }
                value if value.starts_with('-') => {
                    for flag in value[1..].chars() {
                        match flag {
                            'v' => verbose = true,
                            'w' => word = true,
                            'a' => all = true,
                            'p' | 'x' => external_only = true,
                            'f' => function_only = true,
                            'm' => pattern_mode = true,
                            'c' | 's' | 'S' => path_only = true,
                            _ => {
                                return ExecResult::error(
                                    2,
                                    format!("isksh: whence: unsupported option: -{flag}"),
                                );
                            }
                        }
                    }
                }
                _ => break,
            }
            index += 1;
        }
        let mut status = 0;
        let mut output = String::new();
        let requested: Vec<String> = if pattern_mode {
            let mut names = self.configured_command_names();
            names.extend(BUILTIN_NAMES.iter().map(|name| (*name).to_string()));
            names.sort();
            names.dedup();
            let patterns: Vec<_> = args[index..]
                .iter()
                .filter_map(|value| Pattern::new(value).ok())
                .collect();
            names.retain(|name| patterns.iter().any(|pattern| pattern.matches(name)));
            names
        } else {
            args[index..].to_vec()
        };
        for name in &requested {
            let external = self.resolve_command_file(name);
            let (kind, detail) = if let Some(alias) = self.aliases.get(name) {
                if external_only || function_only {
                    ("none", format!("{name} not found"))
                } else {
                    ("alias", format!("{name} is an alias for {alias}"))
                }
            } else if self.functions.contains_key(name) {
                if external_only {
                    ("none", format!("{name} not found"))
                } else {
                    ("function", format!("{name} is a shell function"))
                }
            } else if is_builtin(name) {
                if external_only || function_only {
                    ("none", format!("{name} not found"))
                } else {
                    ("builtin", format!("{name} is a shell builtin"))
                }
            } else {
                if external.is_file() && !function_only {
                    ("command", external.to_string_lossy().into_owned())
                } else {
                    ("none", format!("{name} not found"))
                }
            };
            if kind == "none" {
                status = 1;
            }
            if word {
                output.push_str(&format!("{name}: {kind}\n"));
            } else if path_only && kind == "command" {
                output.push_str(&format!("{}\n", external.display()));
            } else if verbose || kind != "none" {
                output.push_str(&localize(&detail));
                output.push('\n');
            }
            if all && kind != "command" && external.is_file() {
                output.push_str(&format!("{}\n", external.display()));
            }
        }
        ExecResult {
            status,
            stdout: output.into_bytes(),
            stderr: Vec::new(),
            flow: Flow::None,
        }
    }

    /// `builtin_add_zsh_hook`に対応する処理を行う。
    fn builtin_add_zsh_hook(&mut self, args: &[String]) -> ExecResult {
        let (remove, operands) = if args.first().map(String::as_str) == Some("-d") {
            (true, &args[1..])
        } else {
            (false, args)
        };
        let [event, function] = operands else {
            return ExecResult::error(2, "isksh: add-zsh-hook: expected EVENT FUNCTION");
        };
        let hooks = match event.as_str() {
            "precmd" => &mut self.precmd_hooks,
            "chpwd" => &mut self.chpwd_hooks,
            "preexec" => &mut self.preexec_hooks,
            "periodic" => &mut self.periodic_hooks,
            "zshaddhistory" => &mut self.zshaddhistory_hooks,
            "zshexit" => &mut self.zshexit_hooks,
            _ => {
                return ExecResult::error(
                    2,
                    format!("isksh: add-zsh-hook: unsupported hook: {event}"),
                );
            }
        };
        if remove {
            hooks.retain(|hook| hook != function);
        } else if !hooks.contains(function) {
            hooks.push(function.clone());
        }
        ExecResult::status(0)
    }

    /// `builtin_unfunction`に対応する処理を行う。
    fn builtin_unfunction(&mut self, args: &[String]) -> ExecResult {
        let mut status = 0;
        for name in args {
            if self.functions.remove(name).is_none() {
                status = 1;
            }
        }
        ExecResult::status(status)
    }

    /// `builtin_autoload`に対応する処理を行う。
    fn builtin_autoload(&mut self, args: &[String]) -> ExecResult {
        let mut load_now = false;
        let mut names = Vec::new();
        for argument in args {
            if argument == "+X" {
                load_now = true;
            } else if argument.starts_with(['-', '+']) {
                continue;
            } else {
                names.push(argument.clone());
            }
        }
        if names.is_empty() {
            let mut names = self.autoload_functions.iter().cloned().collect::<Vec<_>>();
            names.sort();
            return ExecResult {
                stdout: names.join("\n").into_bytes(),
                ..ExecResult::status(0)
            };
        }
        for name in names {
            self.autoload_functions.insert(name.clone());
            if load_now {
                let result = self.load_autoload_function(&name);
                if result.status != 0 {
                    return result;
                }
            }
        }
        ExecResult::status(0)
    }

    /// `load_autoload_function`に対応する処理を行う。
    fn load_autoload_function(&mut self, name: &str) -> ExecResult {
        let search = self
            .indexed_arrays
            .get("fpath")
            .map(|values| values.values().cloned().collect::<Vec<_>>())
            .unwrap_or_else(|| {
                self.value_of("FPATH")
                    .unwrap_or_default()
                    .split(if cfg!(windows) { ';' } else { ':' })
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
                    .collect()
            });
        let path = search
            .into_iter()
            .map(|directory| self.resolve_path(&directory).join(name))
            .find(|path| path.is_file());
        let Some(path) = path else {
            return ExecResult::error(1, format!("isksh: {name}: autoload function not found"));
        };
        let source = match fs::read_to_string(path) {
            Ok(source) => source,
            Err(error) => return ExecResult::error(1, format!("isksh: {name}: {error}")),
        };
        let result = self.execute_eval(&[format!("{name}() {{\n{source}\n}}")], &[]);
        if result.status == 0 {
            self.autoload_functions.remove(name);
        }
        result
    }

    /// `builtin_zmodload`に対応する処理を行う。
    fn builtin_zmodload(&mut self, args: &[String]) -> ExecResult {
        if args.is_empty() || args.iter().any(|arg| arg == "-L") {
            let mut modules = self.loaded_modules.iter().cloned().collect::<Vec<_>>();
            modules.sort();
            let stdout = modules
                .into_iter()
                .map(|module| format!("zmodload {module}\n"))
                .collect::<String>()
                .into_bytes();
            return ExecResult {
                stdout,
                ..ExecResult::status(0)
            };
        }
        let unload = args.first().map(String::as_str) == Some("-u");
        for module in args.iter().skip(usize::from(unload)) {
            if module.starts_with('-') {
                continue;
            }
            if unload {
                self.loaded_modules.remove(module);
            } else {
                self.loaded_modules.insert(module.clone());
            }
        }
        ExecResult::status(0)
    }

    /// `builtin_functions`に対応する処理を行う。
    fn builtin_functions(&mut self, args: &[String]) -> ExecResult {
        let trace = args
            .iter()
            .any(|argument| argument.starts_with('-') && argument[1..].contains('t'));
        let untrace = args
            .iter()
            .any(|argument| argument.starts_with('+') && argument[1..].contains('t'));
        let mut names = args
            .iter()
            .filter(|argument| !argument.starts_with(['-', '+']))
            .cloned()
            .collect::<Vec<_>>();
        if names.is_empty() {
            names.extend(self.functions.keys().cloned());
            names.extend(self.autoload_functions.iter().cloned());
            names.sort();
        }
        if trace || untrace {
            for name in &names {
                if untrace {
                    self.traced_functions.remove(name);
                } else {
                    self.traced_functions.insert(name.clone());
                }
            }
        }
        let mut status = 0;
        let mut output = String::new();
        for name in names {
            if self.functions.contains_key(&name) {
                output.push_str(&format!("{name} () {{ ... }}\n"));
            } else if self.autoload_functions.contains(&name) {
                output.push_str(&format!("{name} () {{ # undefined }}\n"));
            } else {
                status = 1;
            }
        }
        ExecResult {
            status,
            stdout: output.into_bytes(),
            stderr: Vec::new(),
            flow: Flow::None,
        }
    }

    /// `builtin_zstyle`に対応する処理を行う。
    fn builtin_zstyle(&mut self, args: &[String]) -> ExecResult {
        if args.first().map(String::as_str) == Some("-d") {
            if let (Some(pattern), Some(style)) = (args.get(1), args.get(2)) {
                self.zstyles.remove(&(pattern.clone(), style.clone()));
            }
            return ExecResult::status(0);
        }
        if let Some(query) = args.first().map(String::as_str)
            && matches!(query, "-s" | "-a" | "-t")
        {
            let (Some(pattern), Some(style)) = (args.get(1), args.get(2)) else {
                return ExecResult::status(1);
            };
            let values = self.zstyles.get(&(pattern.clone(), style.clone()));
            if query == "-t" {
                return ExecResult::status(i32::from(values.is_none()));
            }
            let Some(name) = args.get(3) else {
                return ExecResult::status(1);
            };
            let Some(values) = values.cloned() else {
                return ExecResult::status(1);
            };
            if query == "-a" {
                self.indexed_arrays.insert(
                    name.clone(),
                    values.into_iter().enumerate().collect::<BTreeMap<_, _>>(),
                );
            } else {
                let _ = self.set_variable(name, values.join(" "), None, false);
            }
            return ExecResult::status(0);
        }
        let (Some(pattern), Some(style)) = (args.first(), args.get(1)) else {
            return ExecResult::error(2, "isksh: zstyle: expected PATTERN STYLE [VALUE ...]");
        };
        self.zstyles.insert(
            (pattern.clone(), style.clone()),
            args.get(2..).unwrap_or_default().to_vec(),
        );
        ExecResult::status(0)
    }

    /// `builtin_bindkey`に対応する処理を行う。
    fn builtin_bindkey(&mut self, args: &[String]) -> ExecResult {
        if args.is_empty() || args.first().map(String::as_str) == Some("-L") {
            let stdout = self
                .key_bindings
                .iter()
                .map(|((keymap, key), widget)| format!("bindkey -M {keymap} {key} {widget}\n"))
                .collect::<String>()
                .into_bytes();
            return ExecResult {
                stdout,
                ..ExecResult::status(0)
            };
        }
        if matches!(args.first().map(String::as_str), Some("-e" | "-v")) {
            return ExecResult::status(0);
        }
        if matches!(args.first().map(String::as_str), Some("-A" | "-N")) {
            let Some(target) = args.get(1) else {
                return ExecResult::error(2, "isksh: bindkey: keymap name required");
            };
            let source = args.get(2).cloned().unwrap_or_else(|| "main".into());
            let copied: Vec<_> = self
                .key_bindings
                .iter()
                .filter(|((keymap, _), _)| keymap == &source)
                .map(|((_, key), widget)| (key.clone(), widget.clone()))
                .collect();
            for (key, widget) in copied {
                self.key_bindings.insert((target.clone(), key), widget);
            }
            return ExecResult::status(0);
        }
        if args.first().map(String::as_str) == Some("-D") {
            for keymap in &args[1..] {
                self.key_bindings
                    .retain(|(binding, _), _| binding != keymap);
            }
            return ExecResult::status(0);
        }
        let (keymap, operands) = match args {
            [flag] if flag == "-M" => {
                return ExecResult::error(2, "isksh: bindkey: -M requires a keymap");
            }
            [flag, keymap, operands @ ..] if flag == "-M" => (keymap.clone(), operands),
            _ => ("main".to_string(), args),
        };
        let Some((key, values)) = operands.split_first() else {
            return ExecResult::status(0);
        };
        if values.is_empty() {
            return match self.key_bindings.get(&(keymap, key.clone())) {
                None => ExecResult::status(1),
                Some(widget) => ExecResult {
                    stdout: format!("{widget}\n").into_bytes(),
                    ..ExecResult::status(0)
                },
            };
        }
        self.key_bindings
            .insert((keymap, key.clone()), values[0].clone());
        ExecResult::status(0)
    }

    /// `builtin_zle`に対応する処理を行う。
    fn builtin_zle(&mut self, args: &[String]) -> ExecResult {
        match args.first().map(String::as_str) {
            Some("-N") => {
                let Some(widget) = args.get(1) else {
                    return ExecResult::error(2, "isksh: zle: -N requires a widget");
                };
                let function = args.get(2).cloned().unwrap_or_else(|| widget.clone());
                self.zle_widgets.insert(widget.clone(), function);
                ExecResult::status(0)
            }
            Some("-D") => {
                for widget in &args[1..] {
                    self.zle_widgets.remove(widget);
                }
                ExecResult::status(0)
            }
            Some("-l") => ExecResult {
                stdout: self
                    .zle_widgets
                    .keys()
                    .map(|widget| format!("{widget}\n"))
                    .collect::<String>()
                    .into_bytes(),
                ..ExecResult::status(0)
            },
            Some("-R") | Some("-M") => ExecResult::status(0),
            Some(widget) => {
                let Some(function) = self.zle_widgets.get(widget).cloned() else {
                    return ExecResult::error(1, format!("isksh: zle: no such widget: {widget}"));
                };
                let Some(body) = self.functions.get(&function).cloned() else {
                    return ExecResult::error(
                        1,
                        format!("isksh: zle: no such function: {function}"),
                    );
                };
                self.execute_function(&function, &body, args[1..].to_vec(), &[])
            }
            None => ExecResult::status(0),
        }
    }

    /// `builtin_vared`に対応する処理を行う。
    fn builtin_vared(&mut self, args: &[String], input: &[u8]) -> ExecResult {
        let Some(name) = args.iter().find(|argument| !argument.starts_with('-')) else {
            return ExecResult::error(2, "isksh: vared: variable name required");
        };
        let value = String::from_utf8_lossy(input)
            .lines()
            .next()
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| self.value_of(name).unwrap_or_default());
        match self.set_variable(name, value, None, false) {
            Ok(()) => ExecResult::status(0),
            Err(message) => ExecResult::error(1, message),
        }
    }

    /// `builtin_compdef`に対応する処理を行う。
    fn builtin_compdef(&mut self, args: &[String]) -> ExecResult {
        let Some((function, commands)) = args.split_first() else {
            return ExecResult::status(1);
        };
        for command in commands {
            self.completion_definitions
                .insert(command.clone(), function.clone());
        }
        ExecResult::status(0)
    }

    /// `builtin_compadd`に対応する処理を行う。
    fn builtin_compadd(&mut self, args: &[String]) -> ExecResult {
        let mut options = true;
        let mut skip = false;
        for argument in args {
            if skip {
                skip = false;
                continue;
            }
            if argument == "--" {
                options = false;
                continue;
            }
            if options && matches!(argument.as_str(), "-M" | "-J" | "-V" | "-X" | "-P" | "-S") {
                skip = true;
                continue;
            }
            if options && argument.starts_with('-') {
                continue;
            }
            if !self.completion_candidates.contains(argument) {
                self.completion_candidates.push(argument.clone());
            }
        }
        ExecResult::status(0)
    }

    /// `builtin_compset`に対応する処理を行う。
    fn builtin_compset(&mut self, args: &[String]) -> ExecResult {
        let Some(option) = args.first().map(String::as_str) else {
            return ExecResult::status(1);
        };
        let amount = args
            .get(1)
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        let buffer = self.value_of("BUFFER").unwrap_or_default();
        let characters: Vec<_> = buffer.chars().collect();
        let split = amount.min(characters.len());
        match option {
            "-p" => {
                let _ =
                    self.set_variable("PREFIX", characters[..split].iter().collect(), None, false);
            }
            "-s" => {
                let _ = self.set_variable(
                    "SUFFIX",
                    characters[characters.len().saturating_sub(split)..]
                        .iter()
                        .collect(),
                    None,
                    false,
                );
            }
            _ => return ExecResult::error(2, "isksh: compset: unsupported option"),
        }
        ExecResult::status(0)
    }

    /// `builtin_pushd`に対応する処理を行う。
    fn builtin_pushd(&mut self, args: &[String]) -> ExecResult {
        if args.len() > 1 {
            return ExecResult::error(1, "isksh: pushd: too many arguments");
        }
        if let Some(target) = args.first() {
            if let Some(index) = directory_stack_index(target, self.directory_stack.len() + 1) {
                if index == 0 {
                    return ExecResult {
                        stdout: self.directory_listing(false, false).into_bytes(),
                        ..ExecResult::status(0)
                    };
                }
                let target = self.directory_stack.remove(index - 1);
                let current = self.cwd.clone();
                let mut result = self.change_directory(&target.to_string_lossy());
                if result.status == 0 {
                    self.push_directory(current);
                    result.stdout = self.directory_listing(false, false).into_bytes();
                }
                return result;
            }
            let previous = self.cwd.clone();
            let mut result = self.change_directory(target);
            if result.status == 0 {
                self.push_directory(previous);
                result.stdout = self.directory_listing(false, false).into_bytes();
            }
            return result;
        }
        let Some(previous) = self.directory_stack.first().cloned() else {
            return ExecResult::error(1, "isksh: pushd: no other directory");
        };
        let current = self.cwd.clone();
        let mut result = self.change_directory(&previous.to_string_lossy());
        if result.status == 0 {
            self.directory_stack[0] = current;
            result.stdout = self.directory_listing(false, false).into_bytes();
        }
        result
    }

    /// `push_directory`に対応する処理を行う。
    fn push_directory(&mut self, path: PathBuf) {
        if self.mode == ShellMode::Zsh && self.shell_options.contains("pushdignoredups") {
            self.directory_stack.retain(|existing| existing != &path);
        }
        self.directory_stack.insert(0, path);
    }

    /// `builtin_popd`に対応する処理を行う。
    fn builtin_popd(&mut self, args: &[String]) -> ExecResult {
        if args.len() > 1 {
            return ExecResult::error(1, "isksh: popd: unsupported argument");
        }
        if let Some(argument) = args.first()
            && let Some(index) = directory_stack_index(argument, self.directory_stack.len() + 1)
        {
            if index == 0 {
                let Some(target) = self.directory_stack.first().cloned() else {
                    return ExecResult::error(1, "isksh: popd: directory stack empty");
                };
                let mut result = self.change_directory(&target.to_string_lossy());
                if result.status == 0 {
                    self.directory_stack.remove(0);
                    result.stdout = self.directory_listing(false, false).into_bytes();
                }
                return result;
            }
            self.directory_stack.remove(index - 1);
            return ExecResult {
                stdout: self.directory_listing(false, false).into_bytes(),
                ..ExecResult::status(0)
            };
        }
        if !args.is_empty() {
            return ExecResult::error(1, "isksh: popd: unsupported argument");
        }
        let Some(target) = self.directory_stack.first().cloned() else {
            return ExecResult::error(1, "isksh: popd: directory stack empty");
        };
        let mut result = self.change_directory(&target.to_string_lossy());
        if result.status == 0 {
            self.directory_stack.remove(0);
            result.stdout = self.directory_listing(false, false).into_bytes();
        }
        result
    }

    /// `builtin_dirs`に対応する処理を行う。
    fn builtin_dirs(&mut self, args: &[String]) -> ExecResult {
        let mut per_line = false;
        let mut indexed = false;
        let mut clear = false;
        let mut selected = None;
        for argument in args {
            match argument.as_str() {
                "-c" => {
                    self.directory_stack.clear();
                    clear = true;
                }
                "-p" => per_line = true,
                "-v" => {
                    per_line = true;
                    indexed = true;
                }
                value if directory_stack_index(value, self.directory_stack.len() + 1).is_some() => {
                    selected = directory_stack_index(value, self.directory_stack.len() + 1);
                }
                _ => return ExecResult::error(1, "isksh: dirs: unsupported argument"),
            }
        }
        ExecResult {
            stdout: if clear {
                Vec::new()
            } else if let Some(index) = selected {
                let paths = std::iter::once(&self.cwd)
                    .chain(self.directory_stack.iter())
                    .collect::<Vec<_>>();
                paths.get(index).map_or_else(Vec::new, |path| {
                    format!("{}\n", path.display()).into_bytes()
                })
            } else {
                self.directory_listing(per_line, indexed).into_bytes()
            },
            ..ExecResult::status(0)
        }
    }

    /// `directory_listing`に対応する処理を行う。
    fn directory_listing(&self, per_line: bool, indexed: bool) -> String {
        let paths = std::iter::once(&self.cwd)
            .chain(self.directory_stack.iter())
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        if per_line {
            paths
                .iter()
                .enumerate()
                .map(|(index, path)| {
                    if indexed {
                        format!("{index}  {path}\n")
                    } else {
                        format!("{path}\n")
                    }
                })
                .collect()
        } else {
            format!("{}\n", paths.join(" "))
        }
    }

    /// `builtin_trap`に対応する処理を行う。
    fn builtin_trap(&mut self, args: &[String]) -> ExecResult {
        if args.is_empty() || args.first().map(String::as_str) == Some("-p") {
            let requested = if args.first().map(String::as_str) == Some("-p") {
                &args[1..]
            } else {
                &[]
            };
            let mut traps = self.traps.iter().collect::<Vec<_>>();
            traps.sort_by_key(|(signal, _)| *signal);
            let output = traps
                .into_iter()
                .filter(|(signal, _)| requested.is_empty() || requested.contains(signal))
                .map(|(signal, action)| format!("trap -- {} {signal}\n", shell_quote(action)))
                .collect::<String>();
            return ExecResult {
                stdout: output.into_bytes(),
                ..ExecResult::status(0)
            };
        }
        if args.len() < 2 {
            return ExecResult::error(2, "isksh: trap: action and signal are required");
        }
        let action = &args[0];
        for signal in &args[1..] {
            let signal = normalize_signal(signal);
            if !matches!(signal.as_str(), "EXIT" | "INT" | "TERM" | "DEBUG") {
                return ExecResult::error(2, format!("isksh: trap: {signal}: invalid signal"));
            }
            if action == "-" {
                self.traps.remove(&signal);
            } else {
                self.traps.insert(signal, action.clone());
            }
        }
        ExecResult::status(0)
    }

    /// `run_trap`に対応する処理を行う。
    fn run_trap(&mut self, signal: &str) -> ExecResult {
        if self.trap_depth != 0 {
            return ExecResult::status(0);
        }
        self.trap_depth += 1;
        let result = if let Some(action) = self.traps.get(signal).cloned() {
            self.execute_eval(&[action], &[])
        } else {
            let function_name = format!("TRAP{}", signal.trim_start_matches("SIG"));
            let Some(function) = self.functions.get(&function_name).cloned() else {
                self.trap_depth -= 1;
                return ExecResult::status(0);
            };
            self.execute_function(&function_name, &function, Vec::new(), &[])
        };
        self.trap_depth -= 1;
        result
    }

    /// `builtin_jobs`に対応する処理を行う。
    fn builtin_jobs(&self) -> ExecResult {
        let jobs = self.background_jobs.lock().expect("background jobs lock");
        let mut output = String::new();
        for (id, handle) in jobs.iter() {
            let state = if handle.is_finished() {
                "Done"
            } else {
                "Running"
            };
            output.push_str(&format!("[{id}] {}\n", localize(state)));
        }
        ExecResult {
            stdout: output.into_bytes(),
            ..ExecResult::status(0)
        }
    }

    /// `builtin_hash`に対応する処理を行う。
    fn builtin_hash(&mut self, args: &[String]) -> ExecResult {
        if args == ["-r"] {
            self.command_hash.clear();
            return ExecResult::status(0);
        }
        if args.is_empty() {
            let mut entries = self.command_hash.iter().collect::<Vec<_>>();
            entries.sort_by_key(|(name, _)| *name);
            let output = entries
                .into_iter()
                .map(|(name, path)| format!("{name}={path}\n"))
                .collect::<String>();
            return ExecResult {
                stdout: output.into_bytes(),
                ..ExecResult::status(0)
            };
        }
        for name in args {
            let path = self.resolve_command_file(name);
            if !path.is_file() {
                return ExecResult::error(1, format!("isksh: hash: {name}: not found"));
            }
            self.command_hash
                .insert(name.clone(), path.to_string_lossy().into_owned());
        }
        ExecResult::status(0)
    }

    /// `builtin_umask`に対応する処理を行う。
    fn builtin_umask(&mut self, args: &[String]) -> ExecResult {
        if args.is_empty() || args == ["-S"] {
            let output = if args == ["-S"] {
                symbolic_umask(self.creation_mask)
            } else {
                format!("{:04o}\n", self.creation_mask)
            };
            return ExecResult {
                stdout: output.into_bytes(),
                ..ExecResult::status(0)
            };
        }
        if args.len() != 1 {
            return ExecResult::error(2, "isksh: umask: too many arguments");
        }
        let digits = args[0].trim_start_matches('0');
        let digits = if digits.is_empty() { "0" } else { digits };
        let mask = match u32::from_str_radix(digits, 8) {
            Ok(mask) if mask <= 0o777 => mask,
            _ => return ExecResult::error(2, format!("isksh: umask: {}: invalid mask", args[0])),
        };
        self.creation_mask = mask;
        set_process_umask(mask);
        ExecResult::status(0)
    }

    /// `builtin_wait`に対応する処理を行う。
    fn builtin_wait(&mut self, args: &[String]) -> ExecResult {
        let ids = if args.is_empty() {
            self.background_jobs
                .lock()
                .expect("background jobs lock")
                .keys()
                .copied()
                .collect::<Vec<_>>()
        } else {
            let mut ids = Vec::new();
            for argument in args {
                match argument.trim_start_matches('%').parse::<u32>() {
                    Ok(id) => ids.push(id),
                    Err(_) => {
                        return ExecResult::error(
                            127,
                            format!("isksh: wait: {argument}: invalid job"),
                        );
                    }
                }
            }
            ids
        };
        let mut combined = ExecResult::status(0);
        for id in ids {
            let handle = self
                .background_jobs
                .lock()
                .expect("background jobs lock")
                .remove(&id);
            let Some(handle) = handle else {
                return ExecResult::error(127, format!("isksh: wait: {id}: no such job"));
            };
            match handle.join() {
                Ok(result) => combined.append(result),
                Err(_) => {
                    combined.append(ExecResult::error(1, "isksh: wait: background job panicked"))
                }
            }
        }
        combined
    }

    /// `builtin_declare`に対応する処理を行う。
    fn builtin_declare(&mut self, command: &str, args: &[String]) -> ExecResult {
        if command == "local" && self.function_depth == 0 {
            return ExecResult::error(1, "isksh: local: can only be used in a function");
        }
        let mut indexed = false;
        let mut associative = false;
        let mut print = false;
        let mut global = false;
        let mut exported = false;
        let mut readonly = false;
        let mut integer = false;
        let mut lowercase = false;
        let mut uppercase = false;
        let mut width = None;
        let mut index = 0;
        while let Some(option) = args.get(index).filter(|value| value.starts_with('-')) {
            for flag in option[1..].chars() {
                match flag {
                    'a' => indexed = true,
                    'A' => associative = true,
                    'p' => print = true,
                    'g' => global = true,
                    'x' => exported = true,
                    'r' => readonly = true,
                    'i' | 'F' | 'E' => integer = true,
                    'l' => lowercase = true,
                    'u' => uppercase = true,
                    'L' | 'R' | 'Z' => {
                        width = option[2..].parse::<usize>().ok();
                        break;
                    }
                    't' | 'T' | 'U' | 'h' | 'H' => {}
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
            } else if let Some(value) = value {
                let mut value = value.to_string();
                if integer {
                    value = match self.evaluate_arithmetic(&value) {
                        Ok(value) => value.to_string(),
                        Err(message) => return ExecResult::error(1, message),
                    };
                }
                if lowercase {
                    value.make_ascii_lowercase();
                }
                if uppercase {
                    value.make_ascii_uppercase();
                }
                if let Some(width) = width {
                    value = format!("{value:>width$}");
                }
                if let Err(message) = self.set_variable(name, value, Some(exported), false) {
                    return ExecResult::error(1, message);
                }
                if readonly && let Some(variable) = self.variables.get_mut(name) {
                    variable.readonly = true;
                }
            } else if exported || readonly {
                let value = self.value_of(name).unwrap_or_default();
                if let Err(message) = self.set_variable(name, value, Some(exported), false) {
                    return ExecResult::error(1, message);
                }
                if readonly && let Some(variable) = self.variables.get_mut(name) {
                    variable.readonly = true;
                }
            }
        }
        ExecResult {
            stdout: output.into_bytes(),
            ..ExecResult::status(0)
        }
    }

    /// `builtin_shopt`に対応する処理を行う。
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
        /// `OPTIONS`で使用する値を保持する定数。
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

    /// `builtin_type`に対応する処理を行う。
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
            if terse {
                output.push_str(kind);
            } else {
                output.push_str(&localize(&detail));
            }
            output.push('\n');
        }
        ExecResult {
            stdout: output.into_bytes(),
            ..ExecResult::status(0)
        }
    }

    /// `builtin_mapfile`に対応する処理を行う。
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

    /// `builtin_double_bracket`に対応する処理を行う。
    fn builtin_double_bracket(&mut self, args: &[String]) -> ExecResult {
        if args.last().map(String::as_str) != Some("]]") {
            return ExecResult::error(2, "isksh: [[: missing ]]");
        }
        match evaluate_conditional(&args[..args.len() - 1], self) {
            Ok(value) => ExecResult::status(i32::from(!value)),
            Err(message) => ExecResult::error(2, format!("isksh: [[: {message}")),
        }
    }

    /// `builtin_command`に対応する処理を行う。
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

    /// `builtin_export`に対応する処理を行う。
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

    /// `builtin_unset`に対応する処理を行う。
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

    /// `builtin_set`に対応する処理を行う。
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
        } else if matches!(args, [flag, option] if matches!(flag.as_str(), "-o" | "+o")) {
            if args[1] != "pipefail" {
                return ExecResult::error(2, format!("isksh: set: {}: invalid option", args[1]));
            }
            if args[0] == "-o" {
                self.shell_options.insert("pipefail".into());
            } else {
                self.shell_options.remove("pipefail");
            }
            ExecResult::status(0)
        } else if matches!(args, [flag] if matches!(flag.as_str(), "-o" | "+o")) {
            let enabled = self.shell_options.contains("pipefail");
            let text = if args[0] == "-o" {
                format!("pipefail\t{}\n", if enabled { "on" } else { "off" })
            } else if enabled {
                "set -o pipefail\n".into()
            } else {
                "set +o pipefail\n".into()
            };
            ExecResult {
                stdout: text.into_bytes(),
                ..ExecResult::status(0)
            }
        } else {
            ExecResult::error(2, "isksh: set: unsupported option")
        }
    }

    /// `builtin_shift`に対応する処理を行う。
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

    /// `loop_flow`に対応する処理を行う。
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

    /// `execute_eval`に対応する処理を行う。
    fn execute_eval(&mut self, args: &[String], input: &[u8]) -> ExecResult {
        let source = args.join(" ");
        if let Some(result) = self.apply_known_bash_integration(&source) {
            return result;
        }
        match parse(&source) {
            Ok(script) => self.execute_script(&script, input),
            Err(error) => ExecResult::error(2, format!("isksh: {error}")),
        }
    }

    /// `apply_known_bash_integration`に対応する処理を行う。
    fn apply_known_bash_integration(&mut self, source: &str) -> Option<ExecResult> {
        if source.contains("starship_precmd()") && source.contains("STARSHIP_SHELL=\"bash\"") {
            let _ = self.set_variable("PS1", "$(starship prompt --status=$?)".into(), None, false);
            let _ = self.set_variable(
                "PS2",
                "$(starship prompt --continuation)".into(),
                None,
                false,
            );
            let _ = self.set_variable("STARSHIP_SHELL", "bash".into(), Some(true), false);
            return Some(ExecResult::status(0));
        }
        if source.contains("_mise_hook_prompt_command") && source.contains("__MISE_EXE=") {
            let executable = source.lines().find_map(|line| {
                line.trim()
                    .strip_prefix("export __MISE_EXE=")
                    .map(|value| value.trim_matches(['\'', '"']).to_string())
            });
            if let Some(executable) = executable {
                let _ = self.set_variable("__MISE_EXE", executable, Some(true), false);
            }
            let _ = self.set_variable("MISE_SHELL", "bash".into(), Some(true), false);
            self.append_prompt_command("eval \"$(mise hook-env -s bash)\"");
            return Some(ExecResult::status(0));
        }
        if source.contains("function __zoxide_hook()") && source.contains("__zoxide_z()") {
            let _ = self.set_variable("_ZO_DOCTOR", "0".into(), Some(true), false);
            self.append_prompt_command("zoxide add -- \"$PWD\" >/dev/null 2>&1");
            let functions = concat!(
                "z() { __zoxide_result=$(zoxide query -- \"$@\") || return; ",
                "cd \"$__zoxide_result\"; }\n",
                "zi() { __zoxide_result=$(zoxide query -i -- \"$@\") || return; ",
                "cd \"$__zoxide_result\"; }\n",
            );
            let script = parse(functions).expect("built-in zoxide adapter must parse");
            let result = self.execute_script(&script, &[]);
            return Some(result);
        }
        if source.contains("__atuin_bind_ctrl_r=true") && source.contains("__atuin_initialized") {
            let _ = self.set_variable("__atuin_initialized", "true".into(), None, false);
            let _ = self.set_variable("ATUIN_SHELL", "bash".into(), Some(true), false);
            if source.contains("export ATUIN_TMUX_POPUP=false") {
                let _ = self.set_variable("ATUIN_TMUX_POPUP", "false".into(), Some(true), false);
            }
            return Some(ExecResult::status(0));
        }
        if source.contains("### key-bindings.bash ###")
            || source.contains("### completion.bash ###")
        {
            let _ = self.set_variable("ISKSH_FZF_INTEGRATION", "1".into(), Some(true), false);
            return Some(ExecResult::status(0));
        }
        None
    }

    /// `append_prompt_command`に対応する処理を行う。
    fn append_prompt_command(&mut self, command: &str) {
        let current = self.value_of("PROMPT_COMMAND").unwrap_or_default();
        if !current.split(';').any(|part| part.trim() == command) {
            let value = if current.is_empty() {
                command.to_string()
            } else {
                format!("{current}; {command}")
            };
            let _ = self.set_variable("PROMPT_COMMAND", value, None, false);
        }
    }

    /// `builtin_dot`に対応する処理を行う。
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

    /// `builtin_printf`に対応する処理を行う。
    fn builtin_printf(&mut self, args: &[String]) -> ExecResult {
        if args.first().map(String::as_str) != Some("-v") {
            return builtin_printf(args);
        }
        let Some(name) = args.get(1) else {
            return ExecResult::error(2, "isksh: printf: -v requires a variable name");
        };
        if !valid_assignment_name(name) {
            return ExecResult::error(2, format!("isksh: printf: {name}: invalid variable name"));
        }
        let result = builtin_printf(&args[2..]);
        let value = String::from_utf8(result.stdout).expect("builtin printf always emits UTF-8");
        match self.set_assignment(name, value, None) {
            Ok(()) => ExecResult::status(result.status),
            Err(message) => ExecResult::error(1, message),
        }
    }

    /// `builtin_read`に対応する処理を行う。
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
        let mut raw = false;
        let mut array = None;
        let mut index = 0;
        while let Some(option) = args.get(index).map(String::as_str) {
            match option {
                "-r" => raw = true,
                "-a" => {
                    index += 1;
                    let Some(name) = args.get(index) else {
                        return ExecResult::error(2, "isksh: read: -a requires an array name");
                    };
                    if !valid_variable_name(name) {
                        return ExecResult::error(2, "isksh: read: invalid array name");
                    }
                    array = Some(name.clone());
                }
                "--" => {
                    index += 1;
                    break;
                }
                value if value.starts_with('-') => {
                    return ExecResult::error(
                        2,
                        format!("isksh: read: {value}: unsupported option"),
                    );
                }
                _ => break,
            }
            index += 1;
        }
        let line = input.lines().next().unwrap_or_default();
        let line = if raw {
            line.to_string()
        } else {
            let mut value = String::new();
            let mut escaped = false;
            for character in line.chars() {
                if escaped {
                    value.push(character);
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else {
                    value.push(character);
                }
            }
            value
        };
        let ifs = self.value_of("IFS").unwrap_or_else(|| " \t\n".into());
        let fields = split_fields(&line, &ifs);
        if let Some(name) = array {
            self.associative_arrays.remove(&name);
            self.indexed_arrays
                .insert(name, fields.into_iter().enumerate().collect());
            return ExecResult::status(i32::from(input.is_empty()));
        }
        let names = if index == args.len() {
            vec!["REPLY".to_string()]
        } else {
            args[index..].to_vec()
        };
        for (index, name) in names.iter().enumerate() {
            let value = if index + 1 == names.len() {
                fields.get(index..).unwrap_or_default().join(" ")
            } else {
                fields.get(index).cloned().unwrap_or_default()
            };
            if let Err(message) = self.set_variable(name, value, None, false) {
                return ExecResult::error(1, message);
            }
        }
        ExecResult::status(i32::from(input.is_empty()))
    }

    /// `builtin_builtin`に対応する処理を行う。
    fn builtin_builtin(&mut self, args: &[String], input: &[u8]) -> ExecResult {
        let Some(name) = args.first() else {
            return ExecResult::status(0);
        };
        if !is_builtin(name) {
            return ExecResult::error(1, format!("isksh: builtin: {name}: not a shell builtin"));
        }
        self.execute_builtin(name, &args[1..], input)
    }

    /// `builtin_help`に対応する処理を行う。
    fn builtin_help(&self, args: &[String]) -> ExecResult {
        if args.is_empty() {
            return ExecResult {
                stdout: format!("{}\n", BUILTIN_NAMES.join(" ")).into_bytes(),
                ..ExecResult::status(0)
            };
        }
        let mut output = String::new();
        for name in args {
            if !is_builtin(name) {
                return ExecResult::error(1, format!("isksh: help: {name}: no help topic"));
            }
            output.push_str(&builtin_description(name));
        }
        ExecResult {
            stdout: output.into_bytes(),
            ..ExecResult::status(0)
        }
    }

    /// `builtin_let`に対応する処理を行う。
    fn builtin_let(&mut self, args: &[String]) -> ExecResult {
        if args.is_empty() {
            return ExecResult::status(1);
        }
        let mut value = 0;
        for expression in args {
            value = match self.evaluate_let_expression(expression) {
                Ok(value) => value,
                Err(message) => return ExecResult::error(1, format!("isksh: let: {message}")),
            };
        }
        ExecResult::status(i32::from(value == 0))
    }

    /// `evaluate_let_expression`に対応する処理を行う。
    fn evaluate_let_expression(&mut self, expression: &str) -> Result<i64, String> {
        let expression = expression.trim();
        for (prefix, delta) in [("++", 1i64), ("--", -1)] {
            if let Some(name) = expression.strip_prefix(prefix) {
                return self.update_arithmetic_variable(name, delta, true);
            }
        }
        for (suffix, delta) in [("++", 1i64), ("--", -1)] {
            if let Some(name) = expression.strip_suffix(suffix) {
                return self.update_arithmetic_variable(name, delta, false);
            }
        }
        for operator in ["+=", "-=", "*=", "/=", "%=", "="] {
            if let Some((name, right)) = expression.split_once(operator) {
                if !valid_variable_name(name.trim()) {
                    return Err("invalid assignment".into());
                }
                let right = self.evaluate_arithmetic(right)?;
                let current = self
                    .value_of(name.trim())
                    .and_then(|value| value.parse::<i64>().ok())
                    .unwrap_or(0);
                let value = match operator {
                    "+=" => current.wrapping_add(right),
                    "-=" => current.wrapping_sub(right),
                    "*=" => current.wrapping_mul(right),
                    "/=" if right != 0 => current / right,
                    "%=" if right != 0 => current % right,
                    "/=" | "%=" => return Err("division by zero".into()),
                    _ => right,
                };
                self.set_variable(name.trim(), value.to_string(), None, false)?;
                return Ok(value);
            }
        }
        self.evaluate_arithmetic(expression)
    }

    /// `update_arithmetic_variable`に対応する処理を行う。
    fn update_arithmetic_variable(
        &mut self,
        name: &str,
        delta: i64,
        prefix: bool,
    ) -> Result<i64, String> {
        if !valid_variable_name(name) {
            return Err("invalid variable name".into());
        }
        let previous = self
            .value_of(name)
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let value = previous.wrapping_add(delta);
        self.set_variable(name, value.to_string(), None, false)?;
        Ok(if prefix { value } else { previous })
    }

    /// `builtin_getopts`に対応する処理を行う。
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

    /// `builtin_alias`に対応する処理を行う。
    fn builtin_alias(&mut self, args: &[String]) -> ExecResult {
        let mut global = false;
        let mut suffix = false;
        let mut list_form = false;
        let mut index = 0;
        while let Some(argument) = args.get(index) {
            match argument.as_str() {
                "-g" => global = true,
                "-s" => suffix = true,
                "-r" => {}
                "-L" => list_form = true,
                "--" | "-" => {
                    index += 1;
                    break;
                }
                value if value.starts_with('-') => {
                    return ExecResult::error(
                        2,
                        format!("isksh: alias: unsupported option: {value}"),
                    );
                }
                _ => break,
            }
            index += 1;
        }
        let aliases = if global {
            &mut self.global_aliases
        } else if suffix {
            &mut self.suffix_aliases
        } else {
            &mut self.aliases
        };
        if args[index..].is_empty() {
            let mut aliases: Vec<_> = aliases.iter().collect();
            aliases.sort_by_key(|(name, _)| *name);
            let stdout = aliases
                .into_iter()
                .map(|(name, value)| {
                    let prefix = if global {
                        "alias -g"
                    } else if suffix {
                        "alias -s"
                    } else {
                        "alias"
                    };
                    if list_form {
                        format!("{prefix} {name}='{}'\n", value.replace('\'', "'\\''"))
                    } else {
                        format!("{name}='{}'\n", value.replace('\'', "'\\''"))
                    }
                })
                .collect::<String>()
                .into_bytes();
            return ExecResult {
                stdout,
                ..ExecResult::status(0)
            };
        }
        for argument in &args[index..] {
            if let Some((name, value)) = argument.split_once('=') {
                aliases.insert(name.to_string(), value.to_string());
            } else if !aliases.contains_key(argument) {
                return ExecResult::error(1, format!("isksh: alias: {argument}: not found"));
            }
        }
        ExecResult::status(0)
    }

    /// `builtin_unalias`に対応する処理を行う。
    fn builtin_unalias(&mut self, args: &[String]) -> ExecResult {
        if args.first().map(String::as_str) == Some("-a") {
            self.aliases.clear();
            self.global_aliases.clear();
            self.suffix_aliases.clear();
            return ExecResult::status(0);
        }
        for name in args.iter().filter(|name| !name.starts_with('-')) {
            self.aliases.remove(name);
            self.global_aliases.remove(name);
            self.suffix_aliases.remove(name);
        }
        ExecResult::status(0)
    }

    /// `builtin_abbr`に対応する処理を行う。
    fn builtin_abbr(&mut self, args: &[String]) -> ExecResult {
        let mut operation = None;
        let mut operands = Vec::new();
        let mut options = true;
        for argument in args {
            match argument.as_str() {
                "-a" | "--add" if options => operation = Some("add"),
                "-e" | "--erase" if options => operation = Some("erase"),
                "-q" | "--query" if options => operation = Some("query"),
                "-l" | "--list" if options => operation = Some("list"),
                "-s" | "--show" if options => operation = Some("show"),
                "-r" | "--rename" if options => operation = Some("rename"),
                "-g" | "--global" | "-U" | "--universal" if options => {}
                "-h" | "--help" if options => operation = Some("help"),
                "--" if options => options = false,
                value if options && value.starts_with('-') => {
                    return ExecResult::error(2, format!("isksh: abbr: unknown option: {value}"));
                }
                value => operands.push(value),
            }
        }
        let operation = operation.unwrap_or(if operands.len() >= 2 { "add" } else { "show" });
        match operation {
            "add" => {
                let Some((name, expansion)) = operands.split_first() else {
                    return ExecResult::error(2, "isksh: abbr: --add requires a name");
                };
                if expansion.is_empty()
                    || name.is_empty()
                    || name.chars().any(|character| character.is_whitespace())
                {
                    return ExecResult::error(2, "isksh: abbr: invalid abbreviation");
                }
                self.abbreviations
                    .insert((*name).to_string(), expansion.join(" "));
                ExecResult::status(0)
            }
            "erase" => {
                for name in operands {
                    self.abbreviations.remove(name);
                }
                ExecResult::status(0)
            }
            "query" => ExecResult::status(i32::from(
                operands.is_empty()
                    || !operands
                        .iter()
                        .any(|name| self.abbreviations.contains_key(*name)),
            )),
            "list" => {
                let mut names = self.abbreviations.keys().cloned().collect::<Vec<_>>();
                names.sort();
                ExecResult {
                    stdout: names
                        .into_iter()
                        .map(|name| format!("{name}\n"))
                        .collect::<String>()
                        .into_bytes(),
                    ..ExecResult::status(0)
                }
            }
            "rename" => {
                if operands.len() != 2 || !self.abbreviations.contains_key(operands[0]) {
                    return ExecResult::error(2, "isksh: abbr: invalid rename");
                }
                let expansion = self.abbreviations.remove(operands[0]).unwrap();
                self.abbreviations
                    .insert(operands[1].to_string(), expansion);
                ExecResult::status(0)
            }
            "help" => ExecResult {
                stdout: abbreviation_help().as_bytes().to_vec(),
                ..ExecResult::status(0)
            },
            _ => {
                let mut abbreviations = self.abbreviations.iter().collect::<Vec<_>>();
                abbreviations.sort_by_key(|(name, _)| *name);
                let stdout = abbreviations
                    .into_iter()
                    .filter(|(name, _)| operands.is_empty() || operands.contains(&name.as_str()))
                    .map(|(name, value)| {
                        format!("abbr -a {name} '{}'\n", value.replace('\'', "'\\''"))
                    })
                    .collect::<String>()
                    .into_bytes();
                ExecResult {
                    stdout,
                    ..ExecResult::status(0)
                }
            }
        }
    }

    /// `expand_word`に対応する処理を行う。
    fn expand_word(&mut self, word: &Word) -> Result<Vec<String>, String> {
        self.expand_word_context(word, true, true)
    }

    /// `expand_word_context`に対応する処理を行う。
    fn expand_word_context(
        &mut self,
        word: &Word,
        allow_split: bool,
        allow_glob: bool,
    ) -> Result<Vec<String>, String> {
        if let [
            WordPart::Parameter {
                expression,
                quoted: true,
            },
        ] = word.parts.as_slice()
            && expression == "@"
        {
            return Ok(self.positional.clone());
        }
        if let [
            WordPart::Parameter {
                expression,
                quoted: true,
            },
        ] = word.parts.as_slice()
        {
            if let Some(reference) = expression.strip_prefix('!')
                && let Some((name, subscript)) = parse_array_reference(reference)
                && subscript == "@"
            {
                return Ok(self.array_keys(name));
            }
            if let Some((name, subscript)) = parse_array_reference(expression)
                && subscript == "@"
            {
                return Ok(self.array_values(name));
            }
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
                        // コマンドが失敗した場合も、コマンド置換の標準出力は維持する。
                    }
                    let output = match String::from_utf8(result.stdout) {
                        Ok(output) => output,
                        Err(_) => {
                            return Err("command substitution produced non-UTF-8 output".into());
                        }
                    };
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
        let fields = if allow_split
            && split
            && (self.mode == ShellMode::Bash || self.shell_options.contains("shwordsplit"))
        {
            let ifs = self.value_of("IFS").unwrap_or_else(|| " \t\n".into());
            split_fields(&value, &ifs)
        } else {
            vec![value]
        };
        if !allow_glob || !globbable {
            return Ok(fields);
        }
        let mut expanded = Vec::new();
        for field in fields {
            if matches!(field.as_str(), "[" | "[[") {
                expanded.push(field);
                continue;
            }
            let extended =
                self.mode == ShellMode::Zsh && self.shell_options.contains("extendedglob");
            let (include, exclude) = if extended {
                if let Some(pattern) = field.strip_prefix('^') {
                    ("*".to_string(), Some(pattern.to_string()))
                } else if let Some((include, exclude)) = field.split_once('~') {
                    (include.to_string(), Some(exclude.to_string()))
                } else {
                    (field.clone(), None)
                }
            } else {
                (field.clone(), None)
            };
            if !include.contains(['*', '?', '[']) {
                expanded.push(field);
                continue;
            }
            let absolute_pattern = self.resolve_path(&include).to_string_lossy().into_owned();
            let exclude = exclude
                .as_deref()
                .map(Pattern::new)
                .transpose()
                .map_err(|error| error.to_string())?;
            let options = MatchOptions {
                case_sensitive: !cfg!(windows),
                require_literal_separator: true,
                require_literal_leading_dot: !self.shell_options.contains("dotglob")
                    && !self.shell_options.contains("globdots"),
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
                .filter(|path| {
                    !exclude
                        .as_ref()
                        .is_some_and(|pattern| pattern.matches(path))
                })
                .collect::<Vec<_>>();
            matches.sort();
            if matches.is_empty() {
                if self.mode == ShellMode::Zsh && self.shell_options.contains("nomatch") {
                    return Err(format!("no matches found: {field}"));
                }
                if !self.shell_options.contains("nullglob") {
                    expanded.push(field);
                }
            } else {
                expanded.extend(matches);
            }
        }
        Ok(expanded)
    }

    /// `expand_scalar`に対応する処理を行う。
    fn expand_scalar(&mut self, word: &Word) -> Result<String, String> {
        let fields = self.expand_word_context(word, false, false)?;
        Ok(fields.join(" "))
    }

    /// `expand_parameter`に対応する処理を行う。
    fn expand_parameter(&mut self, expression: &str) -> Result<String, String> {
        if self.mode == ShellMode::Zsh
            && let Some(name) = expression.strip_prefix('+')
            && valid_assignment_name(name)
        {
            return Ok(usize::from(self.zsh_parameter_is_set(name)).to_string());
        }
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
        if let Some(values) = self.indexed_arrays.get(expression) {
            return Ok(values.get(&0).cloned().unwrap_or_default());
        }
        for operator in ["%%", "##", "%", "#"] {
            if let Some((name, pattern)) = expression.split_once(operator)
                && valid_variable_name(name)
            {
                let value = self.value_of(name).unwrap_or_default();
                let pattern = self.expand_here_document(pattern)?;
                return Ok(remove_parameter_pattern(&value, &pattern, operator));
            }
        }
        for operator in [":-", ":=", ":+", ":?", "-", "=", "+", "?"] {
            if let Some((name, word)) = expression.split_once(operator) {
                if !valid_assignment_name(name)
                    && !(name.len() == 1 && name.as_bytes()[0].is_ascii_digit())
                {
                    break;
                }
                let current = self.parameter_value(name);
                let colon = operator.starts_with(':');
                let missing = current.is_none() || colon && current.as_deref() == Some("");
                let operation = operator.trim_start_matches(':');
                let expanded_word = self.expand_here_document(word)?;
                return if operation == "-" {
                    Ok(if missing {
                        expanded_word
                    } else {
                        current.unwrap_or_default()
                    })
                } else if operation == "+" {
                    Ok(if missing {
                        String::new()
                    } else {
                        expanded_word
                    })
                } else if operation == "=" {
                    if missing {
                        if parse_array_reference(name).is_some() {
                            self.set_assignment(name, expanded_word.clone(), None)?;
                        } else {
                            self.set_variable(name, expanded_word.clone(), None, false)?;
                        }
                        Ok(expanded_word)
                    } else {
                        Ok(current.unwrap_or_default())
                    }
                } else if missing {
                    Err(if expanded_word.is_empty() {
                        format!("{name}: parameter is unset or null")
                    } else {
                        expanded_word
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
            "!" => self
                .last_background_job
                .map(|id| id.to_string())
                .unwrap_or_default(),
            "-" => {
                let mut options = String::new();
                if self.shell_options.contains("pipefail") {
                    options.push('o');
                }
                options
            }
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

    /// `expand_prompt_escapes`に対応する処理を行う。
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
                '!' | '#' => output.push_str(&self.prompt_number.to_string()),
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

    /// `expand_zsh_prompt_escapes`に対応する処理を行う。
    fn expand_zsh_prompt_escapes(&self, value: &str, status: i32) -> String {
        let username = self
            .value_of("USER")
            .or_else(|| self.value_of("USERNAME"))
            .unwrap_or_default();
        let hostname = self
            .value_of("HOSTNAME")
            .or_else(|| self.value_of("COMPUTERNAME"))
            .unwrap_or_default();
        let cwd = self.cwd.to_string_lossy().into_owned();
        let display_cwd = self
            .value_of("HOME")
            .or_else(|| self.value_of("USERPROFILE"))
            .filter(|home| {
                cwd == *home || cwd.starts_with(&format!("{home}{}", std::path::MAIN_SEPARATOR))
            })
            .map_or_else(|| cwd.clone(), |home| format!("~{}", &cwd[home.len()..]));
        let chars: Vec<_> = value.chars().collect();
        let mut output = String::new();
        let mut index = 0;
        while index < chars.len() {
            if chars[index] != '%' {
                output.push(chars[index]);
                index += 1;
                continue;
            }
            index += 1;
            let Some(code) = chars.get(index).copied() else {
                output.push('%');
                break;
            };
            index += 1;
            if code == '(' {
                let start = index;
                let mut depth = 1usize;
                while index < chars.len() && depth > 0 {
                    depth += usize::from(chars[index] == '(');
                    depth = depth.saturating_sub(usize::from(chars[index] == ')'));
                    index += 1;
                }
                if depth != 0 {
                    continue;
                }
                let expression: String = chars[start..index - 1].iter().collect();
                let Some((test, alternatives)) = expression.split_once('.') else {
                    continue;
                };
                let mut parts = alternatives.splitn(2, '.');
                let yes = parts.next().unwrap_or_default();
                let no = parts.next().unwrap_or_default();
                let passes = match test.chars().last().unwrap_or('?') {
                    '?' => status == 0,
                    'j' => match self.background_jobs.lock() {
                        Ok(jobs) => !jobs.is_empty(),
                        Err(_) => false,
                    },
                    '#' => username.eq_ignore_ascii_case("root"),
                    'L' => match self.value_of("SHLVL") {
                        Some(value) => value != "0",
                        None => false,
                    },
                    _ => false,
                };
                output.push_str(
                    &self.expand_zsh_prompt_escapes(if passes { yes } else { no }, status),
                );
                continue;
            }
            if code.is_ascii_digit() {
                let mut digits = code.to_string();
                while chars.get(index).is_some_and(char::is_ascii_digit) {
                    digits.push(chars[index]);
                    index += 1;
                }
                if chars
                    .get(index)
                    .is_some_and(|value| matches!(value, '~' | 'd'))
                {
                    index += 1;
                    let count = digits.parse::<usize>().unwrap_or(0);
                    let components: Vec<_> = display_cwd.split(['/', '\\']).collect();
                    let start = components.len().saturating_sub(count);
                    output.push_str(&components[start..].join(std::path::MAIN_SEPARATOR_STR));
                    continue;
                }
                output.push('%');
                output.push_str(&digits);
                continue;
            }
            match code {
                '%' => output.push('%'),
                'n' => output.push_str(&username),
                'm' => output.push_str(hostname.split('.').next().unwrap_or_default()),
                'M' => output.push_str(&hostname),
                '~' | 'd' => output.push_str(&display_cwd),
                '#' => output.push(if username.eq_ignore_ascii_case("root") {
                    '#'
                } else {
                    '%'
                }),
                '?' => output.push_str(&status.to_string()),
                'j' => {
                    let count = match self.background_jobs.lock() {
                        Ok(jobs) => jobs.len(),
                        Err(_) => 0,
                    };
                    output.push_str(&count.to_string());
                }
                'L' => output.push_str(&self.value_of("SHLVL").unwrap_or_else(|| "1".into())),
                'N' => output.push_str(&self.name),
                'i' => output.push_str(&self.prompt_number.to_string()),
                '_' => output.push_str(self.function_stack.last().map_or("", String::as_str)),
                'D' => {
                    let format = if chars.get(index) == Some(&'{') {
                        index += 1;
                        let start = index;
                        while index < chars.len() && chars[index] != '}' {
                            index += 1;
                        }
                        let format: String = chars[start..index].iter().collect();
                        index += usize::from(index < chars.len());
                        format
                    } else {
                        "%y-%m-%d".into()
                    };
                    output.push_str(&Local::now().format(&format).to_string());
                }
                'T' => output.push_str(&Local::now().format("%H:%M").to_string()),
                't' | '@' => output.push_str(&Local::now().format("%l:%M%p").to_string()),
                '*' => output.push_str(&Local::now().format("%H:%M:%S").to_string()),
                'E' => output.push_str("\x1b[K"),
                'f' => output.push_str("\x1b[39m"),
                'k' => output.push_str("\x1b[49m"),
                'B' => output.push_str("\x1b[1m"),
                'b' => output.push_str("\x1b[22m"),
                'S' => output.push_str("\x1b[7m"),
                's' => output.push_str("\x1b[27m"),
                'U' => output.push_str("\x1b[4m"),
                'u' => output.push_str("\x1b[24m"),
                'G' => {}
                'F' | 'K' if chars.get(index) == Some(&'{') => {
                    index += 1;
                    let start = index;
                    while index < chars.len() && chars[index] != '}' {
                        index += 1;
                    }
                    let color: String = chars[start..index].iter().collect();
                    if index < chars.len() {
                        index += 1;
                    }
                    output.push_str(&zsh_color_escape(code == 'K', &color));
                }
                '{' | '}' => {}
                other => {
                    output.push('%');
                    output.push(other);
                }
            }
        }
        output
    }

    /// `expand_here_document`に対応する処理を行う。
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

    /// `evaluate_arithmetic`に対応する処理を行う。
    fn evaluate_arithmetic(&self, expression: &str) -> Result<i64, String> {
        ArithmeticParser::new(expression, self).parse()
    }

    /// `redirection_path`に対応する処理を行う。
    fn redirection_path(&mut self, word: &Word) -> Result<PathBuf, String> {
        let fields = self.expand_word(word)?;
        if fields.len() != 1 {
            return Err("ambiguous redirect".into());
        }
        Ok(self.resolve_path(&fields[0]))
    }

    /// `resolve_path`に対応する処理を行う。
    fn resolve_path(&self, value: &str) -> PathBuf {
        let path = Path::new(value);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cwd.join(path)
        }
    }

    /// `resolve_command_file`に対応する処理を行う。
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
    /// `resolve_external_name`に対応する処理を行う。
    fn resolve_external_name(&self, name: &str) -> String {
        if let Some(path) = self.command_hash.get(name) {
            return path.clone();
        }
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
    /// `resolve_external_name`に対応する処理を行う。
    fn resolve_external_name(&self, name: &str) -> String {
        self.command_hash
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_string())
    }

    /// `value_of`に対応する処理を行う。
    fn value_of(&self, name: &str) -> Option<String> {
        self.variables
            .get(name)
            .map(|variable| variable.value.clone())
    }

    /// `parameter_value`に対応する処理を行う。
    fn parameter_value(&self, name: &str) -> Option<String> {
        if let Some((array, subscript)) = parse_array_reference(name) {
            return self.array_value(array, subscript);
        }
        if name.len() == 1 && name.as_bytes()[0].is_ascii_digit() {
            let index = name.parse::<usize>().ok()?;
            return if index == 0 {
                Some(self.name.clone())
            } else {
                self.positional.get(index - 1).cloned()
            };
        }
        if self.mode == ShellMode::Zsh {
            match name {
                "path" => return Some(self.zsh_path_values("PATH").join(" ")),
                "fpath" => return Some(self.zsh_path_values("FPATH").join(" ")),
                "funcstack" => return Some(self.function_stack.join(" ")),
                "pipestatus" => return Some(self.array_values("PIPESTATUS").join(" ")),
                "signals" => return Some(ZSH_SIGNALS.join(" ")),
                _ => {}
            }
        }
        self.value_of(name)
    }

    /// `zsh_parameter_is_set`に対応する処理を行う。
    fn zsh_parameter_is_set(&self, name: &str) -> bool {
        if let Some((parameter, subscript)) = parse_array_reference(name) {
            return match parameter {
                "functions" => self.functions.contains_key(subscript),
                "builtins" => is_builtin(subscript),
                "commands" => self.resolve_command_file(subscript).is_file(),
                _ => self.parameter_value(name).is_some(),
            };
        }
        self.parameter_value(name).is_some()
    }

    /// `set_variable`に対応する処理を行う。
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

    /// `set_assignment`に対応する処理を行う。
    fn set_assignment(
        &mut self,
        target: &str,
        value: String,
        exported: Option<bool>,
    ) -> Result<(), String> {
        if let Some((name, subscript)) = parse_array_reference(target) {
            if let Some(values) = self.associative_arrays.get_mut(name) {
                values.insert(subscript.to_string(), value);
                self.sync_zsh_tied_array(name);
                return Ok(());
            }
            let index = self
                .array_index(subscript)
                .ok_or_else(|| format!("isksh: {target}: invalid indexed-array subscript"))?;
            self.indexed_arrays
                .entry(name.to_string())
                .or_default()
                .insert(index, value);
            self.sync_zsh_tied_array(name);
            Ok(())
        } else {
            self.set_variable(target, value, exported, false)
        }
    }

    /// `array_value`に対応する処理を行う。
    fn array_value(&self, name: &str, subscript: &str) -> Option<String> {
        if self.mode == ShellMode::Zsh {
            match name {
                "functions" => {
                    return self
                        .functions
                        .contains_key(subscript)
                        .then(|| format!("{subscript} () {{ ... }}"));
                }
                "builtins" => return is_builtin(subscript).then(|| "defined".to_string()),
                "commands" => {
                    let path = self.resolve_command_file(subscript);
                    return path.is_file().then(|| path.to_string_lossy().into_owned());
                }
                "options" => {
                    let option = normalize_zsh_option(subscript).0;
                    return Some(if self.shell_options.contains(&option) {
                        "on".into()
                    } else {
                        "off".into()
                    });
                }
                "jobstates" => {
                    let id = subscript.trim_start_matches('%').parse::<u32>().ok()?;
                    return self
                        .background_jobs
                        .lock()
                        .ok()?
                        .contains_key(&id)
                        .then(|| "running".into());
                }
                "path" => return self.zsh_array_element(self.zsh_path_values("PATH"), subscript),
                "fpath" => {
                    return self.zsh_array_element(self.zsh_path_values("FPATH"), subscript);
                }
                "funcstack" => {
                    return self.zsh_array_element(self.function_stack.clone(), subscript);
                }
                "pipestatus" => return self.array_value("PIPESTATUS", subscript),
                "signals" => {
                    return self.zsh_array_element(
                        ZSH_SIGNALS
                            .iter()
                            .map(|value| (*value).to_string())
                            .collect(),
                        subscript,
                    );
                }
                _ => {}
            }
        }
        if let Some(values) = self.associative_arrays.get(name) {
            let key = subscript
                .strip_prefix('$')
                .and_then(|name| self.parameter_value(name))
                .unwrap_or_else(|| subscript.to_string());
            values.get(&key).cloned()
        } else {
            let index = self.array_index(subscript)?;
            self.indexed_arrays
                .get(name)
                .and_then(|values| values.get(&index))
                .cloned()
        }
    }

    /// `array_index`に対応する処理を行う。
    fn array_index(&self, subscript: &str) -> Option<usize> {
        let value = self.evaluate_arithmetic(subscript).ok()?;
        if self.mode == ShellMode::Zsh {
            usize::try_from(if value == 0 { 0 } else { value.checked_sub(1)? }).ok()
        } else {
            usize::try_from(value).ok()
        }
    }

    /// `array_values`に対応する処理を行う。
    fn array_values(&self, name: &str) -> Vec<String> {
        if self.mode == ShellMode::Zsh {
            match name {
                "path" => return self.zsh_path_values("PATH"),
                "fpath" => return self.zsh_path_values("FPATH"),
                "funcstack" => return self.function_stack.clone(),
                "pipestatus" => return self.array_values("PIPESTATUS"),
                "signals" => {
                    return ZSH_SIGNALS
                        .iter()
                        .map(|value| (*value).to_string())
                        .collect();
                }
                "functions" => return self.functions.keys().cloned().collect(),
                "builtins" => {
                    return BUILTIN_NAMES
                        .iter()
                        .map(|value| (*value).to_string())
                        .collect();
                }
                "options" => return self.shell_options.iter().cloned().collect(),
                _ => {}
            }
        }
        if let Some(values) = self.indexed_arrays.get(name) {
            return values.values().cloned().collect();
        }
        if let Some(values) = self.associative_arrays.get(name) {
            return values.values().cloned().collect();
        }
        Vec::new()
    }

    /// `array_keys`に対応する処理を行う。
    fn array_keys(&self, name: &str) -> Vec<String> {
        if self.mode == ShellMode::Zsh {
            match name {
                "functions" => return self.functions.keys().cloned().collect(),
                "builtins" => {
                    return BUILTIN_NAMES
                        .iter()
                        .map(|value| (*value).to_string())
                        .collect();
                }
                "options" => return self.shell_options.iter().cloned().collect(),
                "commands" => return self.command_names_from_path(),
                "jobstates" => {
                    return self
                        .background_jobs
                        .lock()
                        .map(|jobs| jobs.keys().map(|id| id.to_string()).collect())
                        .unwrap_or_default();
                }
                _ => {}
            }
        }
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

    /// `zsh_path_values`に対応する処理を行う。
    fn zsh_path_values(&self, variable: &str) -> Vec<String> {
        let array = variable.to_ascii_lowercase();
        self.indexed_arrays
            .get(&array)
            .map(|values| values.values().cloned().collect())
            .unwrap_or_else(|| {
                self.value_of(variable)
                    .unwrap_or_default()
                    .split(if cfg!(windows) { ';' } else { ':' })
                    .map(ToOwned::to_owned)
                    .collect()
            })
    }

    /// `zsh_array_element`に対応する処理を行う。
    fn zsh_array_element(&self, values: Vec<String>, subscript: &str) -> Option<String> {
        let index = self.evaluate_arithmetic(subscript).ok()?;
        let index = if index < 0 {
            i64::try_from(values.len()).ok()?.checked_add(index)?
        } else {
            index.checked_sub(1)?
        };
        values.get(usize::try_from(index).ok()?).cloned()
    }

    /// `sync_zsh_tied_array`に対応する処理を行う。
    fn sync_zsh_tied_array(&mut self, name: &str) {
        if self.mode != ShellMode::Zsh || !matches!(name, "path" | "fpath") {
            return;
        }
        let variable = name.to_ascii_uppercase();
        let separator = if cfg!(windows) { ";" } else { ":" };
        let value = self
            .indexed_arrays
            .get(name)
            .map(|values| values.values().cloned().collect::<Vec<_>>().join(separator))
            .unwrap_or_default();
        let _ = self.set_variable(&variable, value, Some(true), false);
    }

    /// `command_names_from_path`に対応する処理を行う。
    fn command_names_from_path(&self) -> Vec<String> {
        let mut names = HashSet::new();
        for directory in self.zsh_path_values("PATH") {
            if let Ok(entries) = fs::read_dir(self.resolve_path(&directory)) {
                for entry in entries.flatten() {
                    if !entry.path().is_file() {
                        continue;
                    }
                    let file_name = entry.file_name();
                    let Some(name) = file_name.to_str() else {
                        continue;
                    };
                    names.insert(name.to_string());
                }
            }
        }
        let mut names = names.into_iter().collect::<Vec<_>>();
        names.sort();
        names
    }

    /// `finish_process_substitutions`に対応する処理を行う。
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

/// `parse_array_reference`に対応する処理を行う。
fn parse_array_reference(value: &str) -> Option<(&str, &str)> {
    let (name, subscript) = value.split_once('[')?;
    let subscript = subscript.strip_suffix(']')?;
    (valid_variable_name(name) && !subscript.is_empty()).then_some((name, subscript))
}

/// `io_error_string`に対応する処理を行う。
fn io_error_string(error: std::io::Error) -> String {
    error.to_string()
}

/// `restore_map`に対応する処理を行う。
fn restore_map<T>(target: &mut HashMap<String, T>, saved: HashMap<String, Option<T>>) {
    for (name, value) in saved {
        if let Some(value) = value {
            target.insert(name, value);
        } else {
            target.remove(&name);
        }
    }
}

/// `valid_assignment_name`に対応する処理を行う。
fn valid_assignment_name(value: &str) -> bool {
    valid_variable_name(value) || parse_array_reference(value).is_some()
}

/// `format_array_declaration`に対応する処理を行う。
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

/// `evaluate_conditional`に対応する処理を行う。
fn evaluate_conditional(tokens: &[String], shell: &mut Shell) -> Result<bool, String> {
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
            "-r" => fs::File::open(shell.resolve_path(value)).is_ok(),
            "-w" => fs::OpenOptions::new()
                .write(true)
                .open(shell.resolve_path(value))
                .is_ok(),
            "-x" => is_executable_file(&shell.resolve_path(value)),
            "-s" => match fs::metadata(shell.resolve_path(value)) {
                Ok(meta) => meta.len() > 0,
                Err(_) => false,
            },
            "-L" | "-h" => match fs::symlink_metadata(shell.resolve_path(value)) {
                Ok(meta) => meta.file_type().is_symlink(),
                Err(_) => false,
            },
            "-p" => path_has_unix_type(&shell.resolve_path(value), true),
            "-S" => path_has_unix_type(&shell.resolve_path(value), false),
            "-o" => {
                let (name, inverted) = normalize_zsh_option(value);
                shell.shell_options.contains(&name) != inverted
            }
            "-v" => {
                if shell.value_of(value).is_some() {
                    true
                } else if let Some((name, key)) = parse_array_reference(value) {
                    shell.array_value(name, key).is_some()
                } else {
                    false
                }
            }
            _ => return Err(format!("unknown unary operator: {operator}")),
        }),
        [left, operator, right] => match operator.as_str() {
            "=" | "==" => match Pattern::new(right) {
                Ok(pattern) => Ok(pattern.matches(left)),
                Err(error) => Err(error.to_string()),
            },
            "!=" => match Pattern::new(right) {
                Ok(pattern) => Ok(!pattern.matches(left)),
                Err(error) => Err(error.to_string()),
            },
            "=~" => {
                let regex = match Regex::new(right) {
                    Ok(regex) => regex,
                    Err(error) => return Err(error.to_string()),
                };
                let Some(captures) = regex.captures(left) else {
                    return Ok(false);
                };
                let mut values = BTreeMap::new();
                for (index, value) in captures.iter().skip(1).enumerate() {
                    let value = match value {
                        Some(value) => value.as_str(),
                        None => "",
                    };
                    values.insert(index, value.to_string());
                }
                shell.indexed_arrays.insert("match".into(), values);
                let matched = captures.get(0).expect("regex capture zero").as_str();
                let _ = shell.set_variable("MATCH", matched.into(), None, false);
                Ok(true)
            }
            "<" => Ok(left < right),
            ">" => Ok(left > right),
            "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge" => {
                let left = conditional_integer(left)?;
                let right = conditional_integer(right)?;
                Ok(match operator.as_str() {
                    "-eq" => left == right,
                    "-ne" => left != right,
                    "-lt" => left < right,
                    "-le" => left <= right,
                    "-gt" => left > right,
                    _ => left >= right,
                })
            }
            "-nt" | "-ot" => {
                let left = path_modified_time(&shell.resolve_path(left))?;
                let right = path_modified_time(&shell.resolve_path(right))?;
                Ok(if operator == "-nt" {
                    left > right
                } else {
                    left < right
                })
            }
            "-ef" => {
                let left = canonical_conditional_path(&shell.resolve_path(left))?;
                let right = canonical_conditional_path(&shell.resolve_path(right))?;
                Ok(left == right)
            }
            _ => Err(format!("unknown binary operator: {operator}")),
        },
        _ => Err("invalid conditional expression".into()),
    }
}

/// `conditional_integer`に対応する処理を行う。
fn conditional_integer(value: &str) -> Result<i64, String> {
    match value.parse() {
        Ok(value) => Ok(value),
        Err(_) => Err("integer expression expected".into()),
    }
}

/// `path_modified_time`に対応する処理を行う。
fn path_modified_time(path: &Path) -> Result<std::time::SystemTime, String> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => return Err(error.to_string()),
    };
    Ok(metadata.modified().unwrap_or(std::time::UNIX_EPOCH))
}

/// `canonical_conditional_path`に対応する処理を行う。
fn canonical_conditional_path(path: &Path) -> Result<PathBuf, String> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(error) => Err(error.to_string()),
    }
}

/// `conditional_operator`に対応する処理を行う。
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

#[cfg(unix)]
/// `is_executable_file`に対応する処理を行う。
fn is_executable_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(windows)]
/// `is_executable_file`に対応する処理を行う。
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| {
                matches!(
                    value.to_ascii_lowercase().as_str(),
                    "exe" | "com" | "bat" | "cmd"
                )
            })
}

#[cfg(unix)]
/// `path_has_unix_type`に対応する処理を行う。
fn path_has_unix_type(path: &Path, fifo: bool) -> bool {
    fs::metadata(path).is_ok_and(|meta| {
        if fifo {
            meta.file_type().is_fifo()
        } else {
            meta.file_type().is_socket()
        }
    })
}

#[cfg(windows)]
/// `path_has_unix_type`に対応する処理を行う。
fn path_has_unix_type(_: &Path, _: bool) -> bool {
    false
}

/// `finish_external`に対応する処理を行う。
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

/// `finish_external_status`に対応する処理を行う。
fn finish_external_status(
    name: &str,
    status: std::io::Result<std::process::ExitStatus>,
) -> ExecResult {
    match status {
        Ok(status) => ExecResult::status(exit_status(&status)),
        Err(error) => ExecResult::error(126, format!("isksh: {name}: {error}")),
    }
}

/// `pipeline_wait_status`に対応する処理を行う。
fn pipeline_wait_status(status: std::io::Result<std::process::ExitStatus>) -> i32 {
    status.map_or(126, |status| exit_status(&status))
}

/// `is_special_builtin`に対応する処理を行う。
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

/// `BUILTIN_NAMES`で使用する値を保持する定数。
const BUILTIN_NAMES: &[&str] = &[
    ".",
    ":",
    "[",
    "[[",
    "abbr",
    "alias",
    "add-zsh-hook",
    "autoload",
    "bindkey",
    "break",
    "builtin",
    "cd",
    "chdir",
    "command",
    "continue",
    "compadd",
    "compdef",
    "compinit",
    "compset",
    "declare",
    "dirs",
    "echo",
    "emulate",
    "eval",
    "exec",
    "exit",
    "export",
    "false",
    "float",
    "functions",
    "getopts",
    "hash",
    "help",
    "jobs",
    "let",
    "integer",
    "local",
    "mapfile",
    "popd",
    "print",
    "private",
    "printf",
    "pushd",
    "pwd",
    "read",
    "readarray",
    "readonly",
    "return",
    "set",
    "setopt",
    "shift",
    "shopt",
    "source",
    "test",
    "times",
    "trap",
    "true",
    "type",
    "typeset",
    "umask",
    "unalias",
    "unfunction",
    "unset",
    "unsetopt",
    "wait",
    "whence",
    "where",
    "which",
    "vared",
    "zle",
    "zmodload",
    "zstyle",
];

/// `ZSH_SIGNALS`で使用する値を保持する定数。
const ZSH_SIGNALS: &[&str] = &[
    "EXIT", "HUP", "INT", "QUIT", "ILL", "TRAP", "ABRT", "BUS", "FPE", "KILL", "USR1", "SEGV",
    "USR2", "PIPE", "ALRM", "TERM", "CHLD", "CONT", "STOP", "TSTP", "TTIN", "TTOU",
];

/// `normalize_zsh_function_syntax`に対応する処理を行う。
fn normalize_zsh_function_syntax(source: &str) -> String {
    let named = Regex::new(
        r"(?s)\bfunction\s+([A-Za-z_][A-Za-z0-9_]*(?:\s+[A-Za-z_][A-Za-z0-9_]*)*)\s*(?:\(\s*\))?\s*\{([^{}]*)\}",
    )
    .expect("valid zsh function regex");
    let expanded = named.replace_all(source, |captures: &regex::Captures<'_>| {
        captures[1]
            .split_whitespace()
            .map(|name| format!("{name}() {{{}}}", &captures[2]))
            .collect::<Vec<_>>()
            .join("; ")
    });
    let anonymous = Regex::new(r"(?s)(^|[;\n])\s*\(\s*\)\s*\{([^{}]*)\}\s*([^;\n]*)")
        .expect("valid anonymous function regex");
    anonymous
        .replace_all(&expanded, |captures: &regex::Captures<'_>| {
            format!(
                "{} __isksh_anonymous() {{{}}}; __isksh_anonymous {}; unfunction __isksh_anonymous",
                &captures[1], &captures[2], &captures[3]
            )
        })
        .into_owned()
}

/// `directory_stack_index`に対応する処理を行う。
fn directory_stack_index(value: &str, length: usize) -> Option<usize> {
    let (direction, digits) = value.split_at_checked(1)?;
    let index = digits.parse::<usize>().ok()?;
    match direction {
        "+" if index < length => Some(index),
        "-" if index < length => Some(length - index - 1),
        _ => None,
    }
}

/// `normalize_zsh_option`に対応する処理を行う。
fn normalize_zsh_option(option: &str) -> (String, bool) {
    let normalized = option.to_ascii_lowercase().replace('_', "");
    if matches!(normalized.as_str(), "nomatch" | "notify") {
        return (normalized, false);
    }
    normalized
        .strip_prefix("no")
        .map_or((normalized.clone(), false), |name| (name.to_string(), true))
}

/// `decode_echo_escapes`に対応する処理を行う。
fn decode_echo_escapes(value: &str) -> String {
    value
        .replace("\\n", "\n")
        .replace("\\r", "\r")
        .replace("\\t", "\t")
        .replace("\\\\", "\\")
}

/// `zsh_color_escape`に対応する処理を行う。
fn zsh_color_escape(background: bool, color: &str) -> String {
    let layer = if background { 48 } else { 38 };
    if let Some(hex) = color.strip_prefix('#')
        && hex.len() == 6
        && let (Ok(red), Ok(green), Ok(blue)) = (
            u8::from_str_radix(&hex[0..2], 16),
            u8::from_str_radix(&hex[2..4], 16),
            u8::from_str_radix(&hex[4..6], 16),
        )
    {
        return format!("\x1b[{layer};2;{red};{green};{blue}m");
    }
    let code = match color {
        "black" => 0,
        "red" => 1,
        "green" => 2,
        "yellow" => 3,
        "blue" => 4,
        "magenta" => 5,
        "cyan" => 6,
        "white" => 7,
        _ => return String::new(),
    };
    format!("\x1b[{}m", if background { 40 + code } else { 30 + code })
}

/// `is_builtin`に対応する処理を行う。
fn is_builtin(name: &str) -> bool {
    BUILTIN_NAMES.contains(&name)
}

/// `valid_variable_name`に対応する処理を行う。
fn valid_variable_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(ch) if ch.is_ascii_alphabetic() || ch == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// `remove_parameter_pattern`に対応する処理を行う。
fn remove_parameter_pattern(value: &str, pattern: &str, operator: &str) -> String {
    let Ok(pattern) = Pattern::new(pattern) else {
        return value.to_string();
    };
    let mut boundaries = value
        .char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(value.len()))
        .collect::<Vec<_>>();
    match operator {
        "#" => boundaries
            .into_iter()
            .find(|index| pattern.matches(&value[..*index]))
            .map_or_else(|| value.to_string(), |index| value[index..].to_string()),
        "##" => {
            boundaries.reverse();
            boundaries
                .into_iter()
                .find(|index| pattern.matches(&value[..*index]))
                .map_or_else(|| value.to_string(), |index| value[index..].to_string())
        }
        "%" => {
            boundaries.reverse();
            boundaries
                .into_iter()
                .find(|index| pattern.matches(&value[*index..]))
                .map_or_else(|| value.to_string(), |index| value[..index].to_string())
        }
        "%%" => boundaries
            .into_iter()
            .find(|index| pattern.matches(&value[*index..]))
            .map_or_else(|| value.to_string(), |index| value[..index].to_string()),
        _ => value.to_string(),
    }
}

/// `split_fields`に対応する処理を行う。
fn split_fields(value: &str, ifs: &str) -> Vec<String> {
    if ifs.is_empty() {
        return vec![value.to_string()];
    }
    let is_ifs_whitespace = |ch: char| ifs.contains(ch) && matches!(ch, ' ' | '\t' | '\n');
    let is_ifs_other = |ch: char| ifs.contains(ch) && !matches!(ch, ' ' | '\t' | '\n');
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if is_ifs_other(ch) {
            fields.push(std::mem::take(&mut field));
            while chars.peek().is_some_and(|next| is_ifs_whitespace(*next)) {
                chars.next();
            }
        } else if is_ifs_whitespace(ch) {
            while chars.peek().is_some_and(|next| is_ifs_whitespace(*next)) {
                chars.next();
            }
            if !field.is_empty() {
                fields.push(std::mem::take(&mut field));
            }
        } else {
            field.push(ch);
        }
    }
    if !field.is_empty() {
        fields.push(field);
    }
    fields
}

/// `normalize_signal`に対応する処理を行う。
fn normalize_signal(signal: &str) -> String {
    match signal.trim_start_matches("SIG") {
        "0" => "EXIT".into(),
        "2" => "INT".into(),
        "15" => "TERM".into(),
        name => name.to_ascii_uppercase(),
    }
}

/// `symbolic_umask`に対応する処理を行う。
fn symbolic_umask(mask: u32) -> String {
    let permissions = 0o777 & !mask;
    let render = |read, write, execute| {
        format!(
            "{}{}{}",
            if permissions & read != 0 { 'r' } else { '-' },
            if permissions & write != 0 { 'w' } else { '-' },
            if permissions & execute != 0 { 'x' } else { '-' }
        )
    };
    format!(
        "u={},g={},o={}\n",
        render(0o400, 0o200, 0o100),
        render(0o040, 0o020, 0o010),
        render(0o004, 0o002, 0o001)
    )
}

#[cfg(all(unix, not(target_os = "macos")))]
/// `set_process_umask`に対応する処理を行う。
fn set_process_umask(mask: u32) {
    use nix::sys::stat::{Mode, umask};
    umask(Mode::from_bits_truncate(mask));
}

#[cfg(target_os = "macos")]
/// `set_process_umask`に対応する処理を行う。
fn set_process_umask(mask: u32) {
    use nix::sys::stat::{Mode, umask};
    let mask = u16::try_from(mask).expect("validated permission masks fit macOS mode_t");
    umask(Mode::from_bits_truncate(mask));
}

#[cfg(not(unix))]
/// `set_process_umask`に対応する処理を行う。
fn set_process_umask(_mask: u32) {}

/// `shell_quote`に対応する処理を行う。
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// `write_output_sink`に対応する処理を行う。
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

/// `flow_status`に対応する処理を行う。
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

/// `builtin_printf`に対応する処理を行う。
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

/// `builtin_test`に対応する処理を行う。
fn builtin_test(args: &[String]) -> ExecResult {
    let success = match args {
        [] => false,
        [value] => !value.is_empty(),
        [operator, value] if operator == "-n" => !value.is_empty(),
        [operator, value] if operator == "-z" => value.is_empty(),
        [operator, value] if operator == "-e" => Path::new(value).exists(),
        [operator, value] if operator == "-f" => Path::new(value).is_file(),
        [operator, value] if operator == "-d" => Path::new(value).is_dir(),
        [operator, value] if operator == "-r" => fs::File::open(value).is_ok(),
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
/// `platform_command`に対応する処理を行う。
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
/// `platform_command`に対応する処理を行う。
fn platform_command(name: &str, arguments: &[String]) -> ProcessCommand {
    let mut command = ProcessCommand::new(name);
    command.args(arguments);
    command
}

#[cfg(unix)]
/// `configure_process_group`に対応する処理を行う。
fn configure_process_group(command: &mut ProcessCommand, group: Option<u32>) {
    use std::os::unix::process::CommandExt;
    command.process_group(group.unwrap_or(0) as i32);
}

#[cfg(not(unix))]
/// `configure_process_group`に対応する処理を行う。
fn configure_process_group(_command: &mut ProcessCommand, _group: Option<u32>) {}

#[cfg(unix)]
/// `set_foreground_process_group`に対応する処理を行う。
fn set_foreground_process_group(group: u32) {
    use nix::sys::signal::{SigHandler, Signal, signal};
    use nix::unistd::{Pid, tcsetpgrp};
    // SAFETY: シェルが端末の所有権を移す間だけSIGTTOUを無視する。
    let _ = unsafe { signal(Signal::SIGTTOU, SigHandler::SigIgn) };
    let _ = tcsetpgrp(std::io::stdin(), Pid::from_raw(group as i32));
}

#[cfg(windows)]
/// `set_foreground_process_group`に対応する処理を行う。
fn set_foreground_process_group(_group: u32) {
    WINDOWS_CHILD_FOREGROUND.store(true, Ordering::SeqCst);
}

#[cfg(not(any(unix, windows)))]
/// `set_foreground_process_group`に対応する処理を行う。
fn set_foreground_process_group(_group: u32) {}

#[cfg(unix)]
/// `restore_shell_process_group`に対応する処理を行う。
fn restore_shell_process_group() {
    use nix::sys::signal::{SigHandler, Signal, signal};
    use nix::unistd::{getpgrp, tcsetpgrp};
    let _ = tcsetpgrp(std::io::stdin(), getpgrp());
    // SAFETY: 端末の所有権を回収した後に標準の既定動作へ戻す。
    let _ = unsafe { signal(Signal::SIGTTOU, SigHandler::SigDfl) };
}

#[cfg(windows)]
/// `restore_shell_process_group`に対応する処理を行う。
fn restore_shell_process_group() {
    WINDOWS_CHILD_FOREGROUND.store(false, Ordering::SeqCst);
}

#[cfg(not(any(unix, windows)))]
/// `restore_shell_process_group`に対応する処理を行う。
fn restore_shell_process_group() {}

#[cfg(windows)]
/// `install_console_control_handler`に対応する処理を行う。
fn install_console_control_handler() {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    /// `handler`に対応する処理を行う。
    unsafe extern "system" fn handler(control: u32) -> i32 {
        /// `CTRL_C_EVENT`で使用する値を保持する定数。
        const CTRL_C_EVENT: u32 = 0;
        i32::from(control == CTRL_C_EVENT && WINDOWS_CHILD_FOREGROUND.load(Ordering::SeqCst))
    }
    // SAFETY: コールバックはstatic lifetimeを持ち、atomic操作だけを行う。
    let _ = unsafe { SetConsoleCtrlHandler(Some(handler), 1) };
}

#[cfg(not(windows))]
/// `install_console_control_handler`に対応する処理を行う。
fn install_console_control_handler() {}

/// `exit_status`に対応する処理を行う。
fn exit_status(status: &std::process::ExitStatus) -> i32 {
    status.code().unwrap_or_else(|| signal_exit_status(status))
}

#[cfg(unix)]
/// `signal_exit_status`に対応する処理を行う。
fn signal_exit_status(status: &std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status.signal().map_or(128, |signal| 128 + signal)
}

#[cfg(not(unix))]
/// `signal_exit_status`に対応する処理を行う。
fn signal_exit_status(_status: &std::process::ExitStatus) -> i32 {
    128
}

struct ArithmeticParser<'a> {
    chars: Vec<char>,
    index: usize,
    shell: &'a Shell,
}

impl<'a> ArithmeticParser<'a> {
    /// `new`に対応する処理を行う。
    fn new(source: &str, shell: &'a Shell) -> Self {
        Self {
            chars: source.chars().collect(),
            index: 0,
            shell,
        }
    }

    /// `parse`に対応する処理を行う。
    fn parse(mut self) -> Result<i64, String> {
        let value = self.conditional()?;
        self.whitespace();
        if self.index == self.chars.len() {
            Ok(value)
        } else {
            Err("invalid arithmetic expression".into())
        }
    }

    /// `conditional`に対応する処理を行う。
    fn conditional(&mut self) -> Result<i64, String> {
        let condition = self.logical_or()?;
        self.whitespace();
        if !self.consume('?') {
            return Ok(condition);
        }
        let yes = self.conditional()?;
        self.whitespace();
        if !self.consume(':') {
            return Err("missing ':' in conditional expression".into());
        }
        let no = self.conditional()?;
        Ok(if condition != 0 { yes } else { no })
    }

    /// `logical_or`に対応する処理を行う。
    fn logical_or(&mut self) -> Result<i64, String> {
        let mut value = self.logical_and()?;
        while self.consume_str("||") {
            let right = self.logical_and()?;
            value = i64::from(value != 0 || right != 0);
        }
        Ok(value)
    }

    /// `logical_and`に対応する処理を行う。
    fn logical_and(&mut self) -> Result<i64, String> {
        let mut value = self.bit_or()?;
        while self.consume_str("&&") {
            let right = self.bit_or()?;
            value = i64::from(value != 0 && right != 0);
        }
        Ok(value)
    }

    /// `bit_or`に対応する処理を行う。
    fn bit_or(&mut self) -> Result<i64, String> {
        let mut value = self.bit_xor()?;
        loop {
            self.whitespace();
            if self.starts_with("||") || !self.consume('|') {
                return Ok(value);
            }
            value |= self.bit_xor()?;
        }
    }

    /// `bit_xor`に対応する処理を行う。
    fn bit_xor(&mut self) -> Result<i64, String> {
        let mut value = self.bit_and()?;
        while self.consume_str("^") {
            value ^= self.bit_and()?;
        }
        Ok(value)
    }

    /// `bit_and`に対応する処理を行う。
    fn bit_and(&mut self) -> Result<i64, String> {
        let mut value = self.equality()?;
        loop {
            self.whitespace();
            if self.starts_with("&&") || !self.consume('&') {
                return Ok(value);
            }
            value &= self.equality()?;
        }
    }

    /// `equality`に対応する処理を行う。
    fn equality(&mut self) -> Result<i64, String> {
        let mut value = self.comparison()?;
        loop {
            if self.consume_str("==") {
                value = i64::from(value == self.comparison()?);
            } else if self.consume_str("!=") {
                value = i64::from(value != self.comparison()?);
            } else {
                return Ok(value);
            }
        }
    }

    /// `comparison`に対応する処理を行う。
    fn comparison(&mut self) -> Result<i64, String> {
        let mut value = self.shift()?;
        loop {
            if self.consume_str("<=") {
                value = i64::from(value <= self.shift()?);
            } else if self.consume_str(">=") {
                value = i64::from(value >= self.shift()?);
            } else if self.consume_str("<") {
                value = i64::from(value < self.shift()?);
            } else if self.consume_str(">") {
                value = i64::from(value > self.shift()?);
            } else {
                return Ok(value);
            }
        }
    }

    /// `shift`に対応する処理を行う。
    fn shift(&mut self) -> Result<i64, String> {
        let mut value = self.expression()?;
        loop {
            if self.consume_str("<<") {
                value = value.wrapping_shl(self.expression()? as u32);
            } else if self.consume_str(">>") {
                value = value.wrapping_shr(self.expression()? as u32);
            } else {
                return Ok(value);
            }
        }
    }

    /// `expression`に対応する処理を行う。
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

    /// `term`に対応する処理を行う。
    fn term(&mut self) -> Result<i64, String> {
        let mut value = self.power()?;
        loop {
            self.whitespace();
            if self.consume('*') {
                value = value.wrapping_mul(self.power()?);
            } else if self.consume('/') {
                let right = self.power()?;
                if right == 0 {
                    return Err("division by zero".into());
                }
                value /= right;
            } else if self.consume('%') {
                let right = self.power()?;
                if right == 0 {
                    return Err("division by zero".into());
                }
                value %= right;
            } else {
                return Ok(value);
            }
        }
    }

    /// `power`に対応する処理を行う。
    fn power(&mut self) -> Result<i64, String> {
        let value = self.factor()?;
        if self.consume_str("**") {
            let exponent = u32::try_from(self.power()?).map_err(|_| "negative exponent")?;
            Ok(value.wrapping_pow(exponent))
        } else {
            Ok(value)
        }
    }

    /// `factor`に対応する処理を行う。
    fn factor(&mut self) -> Result<i64, String> {
        self.whitespace();
        if self.consume('$') && self.consume('+') {
            let start = self.index;
            while self
                .peek()
                .is_some_and(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '[' | ']'))
            {
                self.index += 1;
            }
            let name: String = self.chars[start..self.index].iter().collect();
            if !valid_assignment_name(&name) {
                return Err("expected zsh parameter name after $+".into());
            }
            return Ok(i64::from(self.shell.zsh_parameter_is_set(&name)));
        }
        if self.consume('-') {
            return Ok(-self.factor()?);
        }
        if self.consume('+') {
            return self.factor();
        }
        if self.consume('!') {
            return Ok(i64::from(self.factor()? == 0));
        }
        if self.consume('~') {
            return Ok(!self.factor()?);
        }
        if self.consume('(') {
            let value = self.conditional()?;
            self.whitespace();
            if !self.consume(')') {
                return Err("missing ')' in arithmetic expression".into());
            }
            return Ok(value);
        }
        let start = self.index;
        while self
            .peek()
            .is_some_and(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '#'))
        {
            self.index += 1;
        }
        if start == self.index {
            return Err("expected arithmetic operand".into());
        }
        let token: String = self.chars[start..self.index].iter().collect();
        if let Some((base, digits)) = token.split_once('#') {
            let base = match base.parse::<u32>() {
                Ok(base) => base,
                Err(_) => return Err("invalid arithmetic base".into()),
            };
            match i64::from_str_radix(digits, base) {
                Ok(value) => Ok(value),
                Err(_) => Err("invalid arithmetic constant".into()),
            }
        } else if let Some(hex) = token
            .strip_prefix("0x")
            .or_else(|| token.strip_prefix("0X"))
        {
            i64::from_str_radix(hex, 16).map_err(|_| "invalid arithmetic constant".into())
        } else if let Ok(value) = token.parse() {
            Ok(value)
        } else {
            Ok(self
                .shell
                .value_of(&token)
                .and_then(|value| value.parse().ok())
                .unwrap_or(0))
        }
    }

    /// `whitespace`に対応する処理を行う。
    fn whitespace(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.index += 1;
        }
    }
    /// `peek`に対応する処理を行う。
    fn peek(&self) -> Option<char> {
        self.chars.get(self.index).copied()
    }
    /// `consume`に対応する処理を行う。
    fn consume(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    /// `starts_with`に対応する処理を行う。
    fn starts_with(&mut self, expected: &str) -> bool {
        self.whitespace();
        let expected = expected.chars().collect::<Vec<_>>();
        self.chars[self.index..].starts_with(&expected)
    }

    /// `consume_str`に対応する処理を行う。
    fn consume_str(&mut self, expected: &str) -> bool {
        if self.starts_with(expected) {
            self.index += expected.chars().count();
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `run`に対応する処理を行う。
    fn run(source: &str) -> RunResult {
        Shell::default().run(source, &[])
    }

    #[test]
    /// `executes_assignments_expansions_and_printf`に対応する処理を行う。
    fn executes_assignments_expansions_and_printf() {
        let result = run("name=world; printf 'hello %s\\n' \"$name\"");
        assert_eq!(result.status, 0);
        assert_eq!(result.stdout, b"hello world\n");
    }

    #[test]
    /// `executes_conditionals_and_loops`に対応する処理を行う。
    fn executes_conditionals_and_loops() {
        let result = run(
            "for value in a b c; do if test \"$value\" != b; then printf '%s' \"$value\"; fi; done",
        );
        assert_eq!(result.stdout, b"ac");
    }

    #[test]
    /// `executes_function_with_positional_parameters`に対応する処理を行う。
    fn executes_function_with_positional_parameters() {
        let result = run("show() { printf '<%s>' \"$1\"; }; show ok");
        assert_eq!(result.stdout, b"<ok>");
        let result = run("hyphen-name() { printf compatible; }; hyphen-name");
        assert_eq!(result.stdout, b"compatible");
    }

    #[test]
    /// `arithmetic_and_command_substitution_work`に対応する処理を行う。
    fn arithmetic_and_command_substitution_work() {
        let result = run("value=$((2 + 3 * 4)); printf '%s:%s' \"$value\" \"$(printf done)\"");
        assert_eq!(result.stdout, b"14:done");
    }

    #[test]
    /// `case_while_break_and_group_work`に対応する処理を行う。
    fn case_while_break_and_group_work() {
        let result = run(
            "i=0; while test $i -ne 4; do i=$((i + 1)); case $i in 2) continue;; 4) break;; *) printf '%s' $i;; esac; done; { printf done; }",
        );
        assert_eq!(result.stdout, b"13done");
    }

    #[test]
    /// `getopts_reads_grouped_options_and_arguments`に対応する処理を行う。
    fn getopts_reads_grouped_options_and_arguments() {
        let result = run(
            "set -- -ab value; while getopts 'ab:' option; do printf '%s:%s;' \"$option\" \"${OPTARG:-}\"; done",
        );
        assert_eq!(result.stdout, b"a:;b:value;");
    }

    #[test]
    /// `exercises_control_flow_and_shell_state`に対応する処理を行う。
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
    /// `exercises_parameter_tilde_glob_and_arithmetic_errors`に対応する処理を行う。
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
    /// `preserves_invalid_glob_literals_for_bracket_builtin`に対応する処理を行う。
    fn preserves_invalid_glob_literals_for_bracket_builtin() {
        let mut shell = Shell::default();
        assert_eq!(shell.run("[ -n value ]", &[]).status, 0);
        assert_eq!(shell.run("printf '%s' '['", &[]).stdout, b"[");
    }

    #[test]
    /// `exercises_redirection_order_and_errors`に対応する処理を行う。
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
        let descriptor = directory.path().join("descriptor");
        assert_eq!(
            shell
                .run(
                    &format!("exec 9>'{}'; printf descriptor >&9", descriptor.display()),
                    &[],
                )
                .status,
            0
        );
        assert_eq!(fs::read(descriptor).unwrap(), b"descriptor");
        assert_eq!(shell.run("printf x >&9", &[]).status, 0);
        assert_eq!(
            fs::read(directory.path().join("descriptor")).unwrap(),
            b"descriptorx"
        );
        assert_eq!(shell.run("printf x 1>&-", &[]).stdout, b"");
        assert_ne!(shell.run("cat < /missing/isksh-file", &[]).status, 0);
    }

    #[test]
    /// `exercises_builtins`に対応する処理を行う。
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
        assert_eq!(shell.execute_builtin("trap", &[], &[]).status, 0);
        assert_ne!(shell.execute_builtin("unsupported", &[], &[]).status, 0);
    }

    #[test]
    /// `exercises_printf_test_getopts_and_external_commands`に対応する処理を行う。
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
            (vec!["-r", "Cargo.toml"], 0),
            (vec!["-r", "missing-isksh-file"], 1),
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
    /// `exercises_input_classification_background_pipeline_and_nested_flow`に対応する処理を行う。
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
        assert!(background.stdout.is_empty());
        assert!(background.stderr.is_empty());
        assert!(!shell.run("jobs", &[]).stdout.is_empty());
        assert_eq!(shell.run("wait", &[]).stdout, b"bg");
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
    /// `exercises_assignment_alias_and_expansion_edge_cases`に対応する処理を行う。
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
    /// `exercises_heredoc_and_redirection_internal_errors`に対応する処理を行う。
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
        assert_eq!(shell.run("printf x 3<&-", &[]).status, 0);
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
    /// `exercises_remaining_builtin_and_arithmetic_paths`に対応する処理を行う。
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
        assert_eq!(shell.execute_builtin("umask", &[], &[]).stdout, b"0022\n");
        assert!(
            String::from_utf8(shell.execute_builtin("umask", &["-S".into()], &[]).stdout)
                .unwrap()
                .contains("u=rwx")
        );
        assert_eq!(
            shell.execute_builtin("umask", &["077".into()], &[]).status,
            0
        );
        assert_ne!(
            shell.execute_builtin("umask", &["999".into()], &[]).status,
            0
        );
        assert_ne!(
            shell
                .execute_builtin("umask", &["022".into(), "extra".into()], &[])
                .status,
            0
        );
        assert_eq!(
            shell.execute_builtin("umask", &["022".into()], &[]).status,
            0
        );
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
    /// `exercises_getopts_operand_variants`に対応する処理を行う。
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
    /// `supports_bash_arrays_and_conditionals`に対応する処理を行う。
    fn supports_bash_arrays_and_conditionals() {
        let mut shell = Shell::default();
        let result = shell.run(
            "a=(zero 'one value' two); a[4]=four; printf '%s|%s|%s|%s\\n' \"${a[1]}\" \"${#a[@]}\" \"${!a[*]}\" \"${a[*]}\"; [[ foobar == foo* && 4 -gt 2 ]]; echo $?",
            &[],
        );
        assert_eq!(result.status, 0);
        assert_eq!(
            result.stdout,
            b"one value|4|0 1 2 4|zero one value two four\n0\n"
        );

        shell.set_positional(vec!["argument".into()]);
        assert_eq!(
            shell
                .run(
                    "idx=3; a[idx]=three; declare -A labels; labels[three]=odd; key=three; printf '%s|%s|%s|%s|%s|' \"${0:-missing}\" \"${1:-missing}\" \"${2:-missing}\" \"${labels[$key]:-unknown}\" \"${a[6]:=six}\"; for value in \"${a[@]}\"; do printf '<%s>' \"$value\"; done; printf '|'; for key in \"${!a[@]}\"; do printf '<%s>' \"$key\"; done",
                    &[],
                )
                .stdout,
            b"isksh|argument|missing|odd|six|<zero><one value><two><three><four><six>|<0><1><2><3><4><6>"
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
        assert_eq!(
            shell
                .run("[[ -v a[4] && -d . && -r Cargo.toml && ! -f . ]]", &[])
                .status,
            0
        );
    }

    #[test]
    /// `supports_process_substitution_and_bashrc_builtins`に対応する処理を行う。
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
    /// `supports_common_bash_compatibility_builtins`に対応する処理を行う。
    fn supports_common_bash_compatibility_builtins() {
        let mut shell = Shell::default();

        assert_eq!(
            shell
                .run(
                    "printf -v answer '%s:%d' ok 7; printf '%s' \"$answer\"",
                    &[]
                )
                .stdout,
            b"ok:7"
        );
        assert_ne!(shell.run("printf -v", &[]).status, 0);
        assert_ne!(shell.run("printf -v 1bad value", &[]).status, 0);
        shell.run("readonly locked=value", &[]);
        assert_ne!(shell.run("printf -v locked changed", &[]).status, 0);
        assert_eq!(
            shell
                .run(
                    "a=(zero); printf -v 'a[1]' one; printf '%s' \"${a[1]}\"",
                    &[]
                )
                .stdout,
            b"one"
        );

        assert_eq!(shell.run("builtin", &[]).status, 0);
        assert_eq!(shell.run("builtin printf '%s' works", &[]).stdout, b"works");
        assert_ne!(shell.run("builtin missing", &[]).status, 0);
        assert!(
            String::from_utf8(shell.run("help", &[]).stdout)
                .unwrap()
                .contains("pushd")
        );
        assert_eq!(
            shell.run("help printf", &[]).stdout,
            b"printf: isksh shell builtin\n"
        );
        assert_ne!(shell.run("help missing", &[]).status, 0);

        assert_eq!(shell.run("let", &[]).status, 1);
        for expression in [
            "x=2", "x+=3", "x-=1", "x*=3", "x/=4", "x%=2", "x++", "++x", "x--", "--x",
        ] {
            assert_eq!(
                shell.run(&format!("let '{expression}'"), &[]).status,
                0,
                "{expression}"
            );
        }
        assert_eq!(shell.value_of("x").as_deref(), Some("1"));
        assert_eq!(shell.run("let 0", &[]).status, 1);
        assert_eq!(shell.run("let 1", &[]).status, 0);
        assert_ne!(shell.run("let '1bad=2'", &[]).status, 0);
        assert_ne!(shell.run("let '++1bad'", &[]).status, 0);
        assert_ne!(shell.run("let 'x/=0'", &[]).status, 0);
        assert_ne!(shell.run("let '?'", &[]).status, 0);

        assert_eq!(shell.run("read -r RAW", b"a\\b\n").status, 0);
        assert_eq!(shell.value_of("RAW").as_deref(), Some("a\\b"));
        assert_eq!(shell.run("read COOKED", b"a\\b\n").status, 0);
        assert_eq!(shell.value_of("COOKED").as_deref(), Some("ab"));
        assert_eq!(shell.run("read TRAILING", b"a\\").status, 0);
        assert_eq!(shell.value_of("TRAILING").as_deref(), Some("a"));
        assert_eq!(shell.run("read -a words", b"one two\n").status, 0);
        assert_eq!(shell.array_value("words", "1").as_deref(), Some("two"));
        assert_eq!(shell.run("read -- VALUE", b"value\n").status, 0);
        assert_ne!(shell.run("read -a", b"").status, 0);
        assert_ne!(shell.run("read -a 1bad", b"").status, 0);
        assert_ne!(shell.run("read -z value", b"").status, 0);
    }

    #[test]
    /// `manages_the_bash_directory_stack`に対応する処理を行う。
    fn manages_the_bash_directory_stack() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first");
        let second = root.path().join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        let mut shell = Shell {
            cwd: root.path().to_path_buf(),
            ..Shell::default()
        };
        shell.variables.remove("OLDPWD");

        assert_ne!(shell.builtin_cd(&["-".into()]).status, 0);
        assert_ne!(shell.builtin_cd(&["a".into(), "b".into()]).status, 0);
        assert_ne!(shell.builtin_pushd(&["missing".into()]).status, 0);
        assert_ne!(shell.builtin_pushd(&["a".into(), "b".into()]).status, 0);
        assert_eq!(
            shell
                .builtin_pushd(&[first.to_string_lossy().into_owned()])
                .status,
            0
        );
        assert_eq!(shell.cwd, first);
        assert!(
            String::from_utf8(shell.builtin_dirs(&[]).stdout)
                .unwrap()
                .contains(root.path().to_str().unwrap())
        );
        assert_eq!(
            String::from_utf8(shell.builtin_dirs(&["-p".into()]).stdout)
                .unwrap()
                .lines()
                .count(),
            2
        );
        assert!(
            String::from_utf8(shell.builtin_dirs(&["-v".into()]).stdout)
                .unwrap()
                .starts_with("0  ")
        );
        assert_ne!(shell.builtin_dirs(&["-x".into()]).status, 0);

        assert_eq!(shell.builtin_pushd(&[]).status, 0);
        assert_eq!(shell.cwd, root.path());
        assert_ne!(shell.builtin_popd(&["-n".into()]).status, 0);
        assert_eq!(shell.builtin_popd(&[]).status, 0);
        assert_eq!(shell.cwd, first);
        assert_eq!(shell.builtin_cd(&["-".into()]).status, 0);
        assert_eq!(shell.cwd, root.path());
        let cleared = shell.builtin_dirs(&["-c".into()]);
        assert_eq!(cleared.status, 0);
        assert!(cleared.stdout.is_empty());
        assert_ne!(shell.builtin_popd(&[]).status, 0);
        assert_ne!(shell.builtin_pushd(&[]).status, 0);
        assert_eq!(
            shell
                .builtin_pushd(&[second.to_string_lossy().into_owned()])
                .status,
            0
        );
    }

    #[test]
    /// `covers_bash_compatibility_errors_and_variants`に対応する処理を行う。
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
        assert_ne!(shell.run("indexed[1/0]=x", &[]).status, 0);
    }

    #[test]
    /// `expands_bash_style_prompts_and_runs_prompt_command`に対応する処理を行う。
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
    /// `supports_zsh_mode_prompts_builtins_and_hooks`に対応する処理を行う。
    fn supports_zsh_mode_prompts_builtins_and_hooks() {
        let mut shell = Shell::new("isksh");
        shell.run("export ISKSH_MODE=zsh", &[]);
        shell.refresh_mode();
        assert_eq!(shell.mode(), ShellMode::Zsh);
        shell
            .set_variable("USER", "tester".into(), Some(true), false)
            .unwrap();
        shell
            .set_variable("HOSTNAME", "host.example".into(), Some(true), false)
            .unwrap();
        shell.run(
            "count=0; before_prompt() { count=$((count + 1)); }; add-zsh-hook precmd before_prompt; PROMPT='%F{green}%n:%~:%?:%#%f '",
            &[],
        );
        let prompt = shell.prompt(false);
        assert!(prompt.starts_with("\u{1b}[32m"));
        assert!(prompt.ends_with("\u{1b}[39m "));
        assert_eq!(shell.value_of("count").as_deref(), Some("1"));
        shell.run("PROMPT2='%K{blue}>%k '", &[]);
        assert_eq!(shell.prompt(true), "\u{1b}[44m>\u{1b}[49m ");

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().to_string_lossy().replace('\\', "/");
        shell.run(
            "changed=0; directory_changed() { changed=1; }; add-zsh-hook chpwd directory_changed",
            &[],
        );
        assert_eq!(shell.run(&format!("cd '{}'", target), &[]).status, 0);
        assert_eq!(shell.value_of("changed").as_deref(), Some("1"));
        assert_eq!(shell.run("print -rn -- value", &[]).stdout, b"value");
        assert_eq!(
            shell.run("print 'a\\nb\\tc\\r\\\\'", &[]).stdout,
            b"a\nb\tc\r\\\n"
        );
        assert!(shell.run("print -x value", &[]).status != 0);
        assert_eq!(
            shell.run("print -P -- '%F{#010203}x%f'", &[]).stdout,
            b"\x1b[38;2;1;2;3mx\x1b[39m\n"
        );
        assert_eq!(shell.run("setopt prompt_subst", &[]).status, 0);
        assert_eq!(shell.run("unsetopt prompt_subst", &[]).status, 0);
        assert_eq!(shell.run("add-zsh-hook", &[]).status, 2);
        assert_eq!(shell.run("add-zsh-hook unsupported hook", &[]).status, 2);
        assert_eq!(
            shell.run("add-zsh-hook precmd before_prompt", &[]).status,
            0
        );
        assert_eq!(
            shell
                .run("add-zsh-hook -d precmd before_prompt", &[])
                .status,
            0
        );
        assert_eq!(shell.run("unfunction before_prompt", &[]).status, 0);
        assert_eq!(shell.run("unfunction missing", &[]).status, 1);
        assert_eq!(
            shell
                .run("autoload -Uz add-zsh-hook; zmodload zsh/datetime", &[])
                .status,
            0
        );

        shell
            .set_variable("USER", "root".into(), Some(true), false)
            .unwrap();
        let escapes = shell.expand_zsh_prompt_escapes(
            "plain%%:%n:%m:%M:%~:%d:%#:%?:%Bbold%b:%{hidden%}:%q:%F{red",
            7,
        );
        assert!(escapes.contains("plain%:root:host:host.example:"));
        assert!(escapes.contains(":#:7:\u{1b}[1mbold\u{1b}[22m:hidden:%q:"));
        assert_eq!(shell.expand_zsh_prompt_escapes("trailing%", 0), "trailing%");

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
                "/not-the-current-directory".into(),
                None,
                false,
            )
            .unwrap();
        let fallback = shell.expand_zsh_prompt_escapes("%n:%M:%~", 0);
        assert_eq!(
            fallback,
            format!("fallback-user:fallback-host:{}", shell.cwd.display())
        );
        shell
            .set_variable(
                "USERPROFILE",
                shell.cwd.to_string_lossy().into_owned(),
                None,
                false,
            )
            .unwrap();
        assert_eq!(shell.expand_zsh_prompt_escapes("%~", 0), "~");
    }

    #[test]
    /// `supports_zsh_parameter_option_print_and_command_semantics`に対応する処理を行う。
    fn supports_zsh_parameter_option_print_and_command_semantics() {
        let mut shell = Shell::new("isksh");
        shell.run("export ISKSH_MODE=zsh", &[]);
        shell.refresh_mode();

        assert_eq!(
            shell.run("value='a b'; printf '<%s>' $value", &[]).stdout,
            b"<a b>"
        );
        assert_eq!(
            shell
                .run(
                    "typeset -a zitems; zitems[0]=x; printf '%s:%s:%s' ${+value} ${+missing} ${+zitems[0]}",
                    &[],
                )
                .stdout,
            b"1:0:1"
        );
        assert_eq!(shell.run("setopt SH_WORD_SPLIT", &[]).status, 0);
        assert_eq!(shell.run("printf '<%s>' $value", &[]).stdout, b"<a><b>");
        assert_eq!(shell.run("setopt No_SH_WORD_SPLIT", &[]).status, 0);
        assert_eq!(shell.run("printf '<%s>' $value", &[]).stdout, b"<a b>");
        assert_eq!(shell.run("unsetopt NO_SH_WORD_SPLIT", &[]).status, 0);
        assert_eq!(shell.run("printf '<%s>' $value", &[]).stdout, b"<a><b>");
        assert_eq!(shell.run("setopt notify nomatch", &[]).status, 0);
        assert!(shell.shell_options.contains("notify"));
        assert!(shell.shell_options.contains("nomatch"));
        assert_eq!(shell.run("setopt nonomatch", &[]).status, 0);
        assert!(!shell.shell_options.contains("nomatch"));
        assert_eq!(shell.run("setopt nonobeep", &[]).status, 1);

        assert_eq!(shell.run("print -ln -- a b", &[]).stdout, b"a\nb");
        assert_eq!(shell.run("print -N -- a b", &[]).stdout, b"a\0b\0");
        assert_eq!(shell.run("print -R -- 'a\\nb'", &[]).stdout, b"a\\nb\n");
        assert_eq!(shell.run("print -f '%s:%s' a b", &[]).stdout, b"a:b");
        assert_eq!(shell.run("print -f%s value", &[]).stdout, b"value");
        assert_eq!(shell.run("print -f", &[]).status, 2);

        shell.run("alias short='print ok'; named() { :; }", &[]);
        let commands = tempfile::tempdir().unwrap();
        fs::write(commands.path().join("zsh-tool"), "").unwrap();
        shell
            .set_variable(
                "PATH",
                commands.path().to_string_lossy().into_owned(),
                Some(true),
                false,
            )
            .unwrap();
        assert_eq!(
            shell
                .run(
                    "printf '%s:%s:%s:%s' ${+functions[named]} ${+builtins[print]} ${+commands[zsh-tool]} ${+functions[missing]}",
                    &[],
                )
                .stdout,
            b"1:1:1:0"
        );
        assert_eq!(
            shell
                .run(
                    "printf '%s:%s' $(( $+functions[named] )) $(( $+functions[missing] ))",
                    &[]
                )
                .stdout,
            b"1:0"
        );
        assert!(shell.evaluate_arithmetic("$+").is_err());
        let kinds = shell.run("whence -w short named print missing", &[]).stdout;
        assert_eq!(
            kinds,
            b"short: alias\nnamed: function\nprint: builtin\nmissing: none\n"
        );
        assert_eq!(shell.run("whence missing", &[]).status, 1);
        assert_eq!(
            shell.run("whence -- print", &[]).stdout,
            b"print is a shell builtin\n"
        );
        assert!(
            String::from_utf8(
                shell
                    .run("whence -v short named print zsh-tool missing", &[])
                    .stdout
            )
            .unwrap()
            .contains("shell builtin")
        );
        assert_eq!(shell.run("whence -x print", &[]).status, 1);

        assert_eq!(shell.run("emulate", &[]).stdout, b"zsh\n");
        assert_eq!(shell.run("emulate -LR zsh", &[]).status, 0);
        assert_eq!(shell.builtin_emulate(&["sh".into()]).status, 0);
        assert_eq!(shell.mode, ShellMode::Bash);
        assert_eq!(shell.value_of("ISKSH_MODE").as_deref(), Some("bash"));
        assert_eq!(shell.builtin_emulate(&["invalid".into()]).status, 0);
        assert_eq!(shell.mode, ShellMode::Zsh);
        shell.mode = ShellMode::Zsh;
        assert_eq!(shell.builtin_emulate(&["-x".into()]).status, 2);
    }

    #[test]
    /// `expands_every_supported_zsh_prompt_color`に対応する処理を行う。
    fn expands_every_supported_zsh_prompt_color() {
        assert_eq!(decode_echo_escapes("a\\nb\\rc\\t\\\\"), "a\nb\rc\t\\");
        assert_eq!(
            zsh_color_escape(false, "#abcdef"),
            "\u{1b}[38;2;171;205;239m"
        );
        assert_eq!(zsh_color_escape(true, "#010203"), "\u{1b}[48;2;1;2;3m");
        assert_eq!(zsh_color_escape(false, "#invalid"), "");
        assert_eq!(zsh_color_escape(false, "unknown"), "");
        for (name, code) in [
            ("black", 30),
            ("red", 31),
            ("green", 32),
            ("yellow", 33),
            ("blue", 34),
            ("magenta", 35),
            ("cyan", 36),
            ("white", 37),
        ] {
            assert_eq!(zsh_color_escape(false, name), format!("\u{1b}[{code}m"));
            assert_eq!(
                zsh_color_escape(true, name),
                format!("\u{1b}[{}m", code + 10)
            );
        }
    }

    #[test]
    /// `unknown_mode_refreshes_to_bash`に対応する処理を行う。
    fn unknown_mode_refreshes_to_bash() {
        let mut shell = Shell::new("isksh");
        shell.run("export ISKSH_MODE=unknown", &[]);
        shell.refresh_mode();
        assert_eq!(shell.mode(), ShellMode::Bash);
        assert_eq!(shell.value_of("ISKSH_MODE").as_deref(), Some("bash"));
    }

    #[test]
    /// `interactive_external_commands_inherit_the_terminal`に対応する処理を行う。
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

    #[test]
    /// `runs_external_pipelines_concurrently_and_tracks_statuses`に対応する処理を行う。
    fn runs_external_pipelines_concurrently_and_tracks_statuses() {
        let mut shell = Shell::default();
        let result = shell.run("yes | head -n 1", &[]);
        assert_eq!(result.status, 0);
        assert_eq!(result.stdout, b"y\n");
        assert_eq!(
            shell.run("printf '%s' \"${PIPESTATUS[@]}\"", &[]).stdout,
            b"1410"
        );
        assert_eq!(
            shell
                .run("set -o pipefail; sh -c 'exit 7' | sh -c 'exit 0'", &[])
                .status,
            7
        );
        assert!(
            String::from_utf8(shell.run("set -o", &[]).stdout)
                .unwrap()
                .contains("pipefail\ton")
        );
        assert!(
            String::from_utf8(shell.run("set +o", &[]).stdout)
                .unwrap()
                .contains("set -o pipefail")
        );
        assert_eq!(shell.run("set +o pipefail", &[]).status, 0);
        assert!(
            String::from_utf8(shell.run("set +o", &[]).stdout)
                .unwrap()
                .contains("set +o pipefail")
        );
        assert_eq!(shell.expand_parameter("-").unwrap(), "");
        assert_ne!(shell.run("set -o missing", &[]).status, 0);
        assert_ne!(shell.run("set -x", &[]).status, 0);

        let failed = shell.run("sh -c 'sleep 1' | missing-isksh-command", &[]);
        assert_eq!(failed.status, 127);
        assert_eq!(shell.run("/ | sh -c 'cat'", &[]).status, 126);
        assert_eq!(
            shell
                .run("BAD=$((1/0)) sh -c 'cat' | sh -c 'cat'", &[])
                .status,
            1
        );
        assert_eq!(shell.run("(printf group) | cat", &[]).stdout, b"group");
        assert_eq!(shell.run("sh </dev/null | cat", &[]).status, 0);
        shell.run("set -o pipefail", &[]);
        assert_eq!(shell.run("false | true", &[]).status, 1);
        assert_eq!(shell.expand_parameter("-").unwrap(), "o");
        assert_eq!(shell.expand_parameter("PIPESTATUS").unwrap(), "1");
        assert_eq!(
            shell.run("! sh -c 'exit 0' | sh -c 'exit 0'", &[]).status,
            1
        );
        shell.set_interactive(true);
        assert_eq!(shell.run("sh -c 'exit 0' | sh -c 'exit 0'", &[]).status, 0);
    }

    #[test]
    /// `manages_background_jobs_wait_and_special_parameter`に対応する処理を行う。
    fn manages_background_jobs_wait_and_special_parameter() {
        let mut shell = Shell::default();
        let (release, blocked) = std::sync::mpsc::channel();
        let running_id = BACKGROUND_JOB_ID.fetch_add(1, Ordering::Relaxed);
        shell.background_jobs.lock().unwrap().insert(
            running_id,
            std::thread::spawn(move || {
                blocked.recv().unwrap();
                ExecResult::status(0)
            }),
        );
        assert_eq!(
            shell.builtin_jobs().stdout,
            format!("[{running_id}] Running\n").into_bytes()
        );
        release.send(()).unwrap();
        assert_eq!(shell.builtin_wait(&[running_id.to_string()]).status, 0);

        assert_eq!(shell.run("printf async &", &[]).status, 0);
        let id = shell.expand_parameter("!").unwrap().parse::<u32>().unwrap();
        while !shell
            .background_jobs
            .lock()
            .unwrap()
            .get(&id)
            .is_some_and(std::thread::JoinHandle::is_finished)
        {
            std::thread::yield_now();
        }
        assert!(
            String::from_utf8(shell.builtin_jobs().stdout)
                .unwrap()
                .contains(&format!("[{id}]"))
        );
        assert_eq!(shell.builtin_wait(&[format!("%{id}")]).stdout, b"async");
        assert_ne!(shell.builtin_wait(&["bad".into()]).status, 0);
        assert_ne!(shell.builtin_wait(&[id.to_string()]).status, 0);
        assert!(shell.builtin_wait(&[]).stdout.is_empty());
        let panic_id = BACKGROUND_JOB_ID.fetch_add(1, Ordering::Relaxed);
        shell.background_jobs.lock().unwrap().insert(
            panic_id,
            std::thread::spawn(|| panic!("expected test panic")),
        );
        assert_eq!(shell.builtin_wait(&[panic_id.to_string()]).status, 1);
    }

    #[test]
    /// `expands_posix_patterns_nested_defaults_and_ifs_fields`に対応する処理を行う。
    fn expands_posix_patterns_nested_defaults_and_ifs_fields() {
        let mut shell = Shell::default();
        let result = shell.run(
            "base=abcabc; fallback=ok; printf '%s|' \"${base%c*}\" \"${base%%c*}\" \"${base#a*}\" \"${base##a*}\" \"${missing:-$fallback}\"; IFS=:; fields='a::b:'; printf '<%s>' $fields",
            &[],
        );
        assert_eq!(result.stdout, b"abcab|ab|bcabc||ok|<a><><b>");
        assert_eq!(split_fields(" a  b ", " \t\n"), vec!["a", "b"]);
        assert_eq!(split_fields("a:  b", ": "), vec!["a", "b"]);
        assert_eq!(split_fields("unchanged", ""), vec!["unchanged"]);
        assert_eq!(remove_parameter_pattern("value", "[", "%"), "value");
        assert_eq!(remove_parameter_pattern("value", "*", "invalid"), "value");
        for operator in ["#", "##", "%", "%%"] {
            assert_eq!(remove_parameter_pattern("value", "z*", operator), "value");
        }
    }

    #[test]
    /// `supports_traps_and_starship_bash_initialization_fallback`に対応する処理を行う。
    fn supports_traps_and_starship_bash_initialization_fallback() {
        let mut shell = Shell::default();
        let result = shell.run("trap 'printf debug' DEBUG; printf body", &[]);
        assert_eq!(result.stdout, b"debugbody");
        assert!(
            String::from_utf8(shell.run("trap -p DEBUG", &[]).stdout)
                .unwrap()
                .contains("DEBUG")
        );
        assert_eq!(shell.run("trap - DEBUG", &[]).status, 0);
        assert_ne!(shell.run("trap action UNKNOWN", &[]).status, 0);
        assert_ne!(shell.run("trap action", &[]).status, 0);
        let exit = shell.run("trap 'printf exit-hook' EXIT; exit 4", &[]);
        assert_eq!(exit.status, 4);
        assert_eq!(exit.stdout, b"exit-hook");

        assert_eq!(
            shell
                .execute_eval(
                    &["starship_precmd() { :; }; STARSHIP_SHELL=\"bash\"".into()],
                    &[],
                )
                .status,
            0
        );
        assert_eq!(
            shell.value_of("PS1").as_deref(),
            Some("$(starship prompt --status=$?)")
        );
        assert_eq!(shell.value_of("STARSHIP_SHELL").as_deref(), Some("bash"));
    }

    #[test]
    /// `translates_dotfiles_bash_tool_integrations`に対応する処理を行う。
    fn translates_dotfiles_bash_tool_integrations() {
        let mut shell = Shell::default();
        assert!(shell.command_search_path().is_some());
        shell.builtin_unset(&["PATH".into()]);
        assert_eq!(shell.command_search_path(), None);
        shell
            .set_variable("PATH", "/tools".into(), Some(true), false)
            .unwrap();
        shell
            .set_variable("PROMPT_COMMAND", "printf pre".into(), None, false)
            .unwrap();
        let mise = concat!(
            "export __MISE_EXE='/tools/mise'\n",
            "_mise_hook_prompt_command() { :; }\n",
        );
        assert_eq!(shell.run(mise, &[]).status, 0);
        assert_eq!(shell.value_of("__MISE_EXE").as_deref(), Some("/tools/mise"));
        assert_eq!(shell.value_of("MISE_SHELL").as_deref(), Some("bash"));
        assert_eq!(shell.run(mise, &[]).status, 0);
        let prompt_command = shell.value_of("PROMPT_COMMAND").unwrap();
        assert!(prompt_command.starts_with("printf pre; "));
        assert_eq!(prompt_command.matches("mise hook-env").count(), 1);

        let zoxide = "function __zoxide_hook() { :; }; __zoxide_z() { :; }";
        assert_eq!(shell.execute_eval(&[zoxide.into()], &[]).status, 0);
        assert!(shell.functions.contains_key("z"));
        assert!(shell.functions.contains_key("zi"));
        assert!(shell.configured_command_names().contains(&"z".to_string()));
        assert_eq!(shell.value_of("_ZO_DOCTOR").as_deref(), Some("0"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let directory = tempfile::tempdir().unwrap();
            let destination = directory.path().join("destination");
            fs::create_dir(&destination).unwrap();
            let executable = directory.path().join("zoxide");
            fs::write(
                &executable,
                format!(
                    "#!/bin/sh\ncase $1 in query) printf '%s' '{}' ;; add) exit 0 ;; esac\n",
                    destination.display()
                ),
            )
            .unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
            shell
                .set_variable(
                    "PATH",
                    directory.path().to_string_lossy().into_owned(),
                    Some(true),
                    false,
                )
                .unwrap();
            let expected = format!("{}\n", destination.display()).into_bytes();
            assert_eq!(shell.run("z target; pwd", &[]).stdout, expected);
            shell.cwd = std::env::current_dir().unwrap();
            let expected = format!("{}\n", destination.display()).into_bytes();
            assert_eq!(shell.run("zi target; pwd", &[]).stdout, expected);
        }

        let atuin = concat!(
            "export ATUIN_TMUX_POPUP=false\n",
            "__atuin_bind_ctrl_r=true; __atuin_initialized=true",
        );
        assert_eq!(shell.run(atuin, &[]).status, 0);
        assert_eq!(shell.value_of("ATUIN_SHELL").as_deref(), Some("bash"));
        assert_eq!(
            shell.value_of("__atuin_initialized").as_deref(),
            Some("true")
        );
        assert_eq!(shell.value_of("ATUIN_TMUX_POPUP").as_deref(), Some("false"));

        assert_eq!(
            shell
                .run("### key-bindings.bash ###\nprintf ignored", &[])
                .status,
            0
        );
        assert_eq!(
            shell.value_of("ISKSH_FZF_INTEGRATION").as_deref(),
            Some("1")
        );

        let mut mise_without_export = Shell::default();
        assert_eq!(
            mise_without_export
                .run("__MISE_EXE=x; _mise_hook_prompt_command() { :; }", &[])
                .status,
            0
        );
        assert_eq!(mise_without_export.value_of("__MISE_EXE"), None);
    }

    #[test]
    /// `handles_persistent_descriptors_hash_and_wait_errors`に対応する処理を行う。
    fn handles_persistent_descriptors_hash_and_wait_errors() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("input");
        fs::write(&input, b"descriptor-input").unwrap();
        let mut shell = Shell::default();
        assert_eq!(
            shell
                .run(&format!("exec 4<'{}'; cat <&4", input.display()), &[])
                .stdout,
            b"descriptor-input"
        );
        assert_eq!(shell.run("exec 5<&-; cat <&5", &[]).status, 0);
        assert_ne!(shell.run("cat <&bad", &[]).status, 0);
        assert_ne!(shell.run("cat <&99", &[]).status, 0);
        assert_ne!(shell.run("cat <&$((1/0))", &[]).status, 0);
        fs::remove_file(&input).unwrap();
        assert_ne!(shell.run("cat <&4", &[]).status, 0);

        let mut fresh = Shell::default();
        assert_ne!(fresh.run("printf x >&bad", &[]).status, 0);
        assert_ne!(fresh.run("printf x >&8", &[]).status, 0);
        assert_ne!(
            fresh
                .run(
                    &format!("exec 6<'{}'", directory.path().join("missing").display()),
                    &[],
                )
                .status,
            0
        );
        assert_ne!(fresh.run("exec 6<$((1/0))", &[]).status, 0);

        assert_eq!(fresh.builtin_hash(&["sh".into()]).status, 0);
        assert!(
            String::from_utf8(fresh.builtin_hash(&[]).stdout)
                .unwrap()
                .contains("sh=")
        );
        assert_ne!(fresh.builtin_hash(&["missing-isksh".into()]).status, 0);
        assert_eq!(fresh.builtin_hash(&["-r".into()]).status, 0);
        assert!(fresh.command_hash.is_empty());

        let error = std::io::Error::other("wait failed");
        assert_eq!(pipeline_wait_status(Err(error)), 126);
        let error = std::io::Error::other("wait failed");
        assert_eq!(finish_external_status("command", Err(error)).status, 126);
    }

    #[test]
    /// `manages_and_expands_interactive_abbreviations`に対応する処理を行う。
    fn manages_and_expands_interactive_abbreviations() {
        let mut shell = Shell::default();
        assert_ne!(shell.run("abbr -a", &[]).status, 0);
        assert_ne!(shell.run("abbr -a bad", &[]).status, 0);
        assert_ne!(shell.run("abbr --unknown", &[]).status, 0);
        assert_eq!(shell.run("abbr -a g printf git", &[]).status, 0);
        assert_eq!(shell.run("abbr --add z 'printf zed'", &[]).status, 0);
        assert_eq!(shell.run("abbr -g c printf core", &[]).status, 0);
        assert_eq!(shell.run("abbr -- --dash printf dash", &[]).status, 0);
        assert!(shell.configured_command_names().contains(&"g".to_string()));
        assert!(!shell.configured_abbreviations().is_empty());
        assert_eq!(shell.run("abbr -q g missing", &[]).status, 0);
        assert_ne!(shell.run("abbr --query missing", &[]).status, 0);
        assert_ne!(shell.run("abbr -q", &[]).status, 0);
        assert_eq!(shell.run("abbr -r z zz", &[]).status, 0);
        assert_ne!(shell.run("abbr --rename missing nope", &[]).status, 0);
        assert_eq!(shell.run("abbr -l", &[]).stdout, b"--dash\nc\ng\nzz\n");
        assert_eq!(
            shell.run("abbr -s g", &[]).stdout,
            b"abbr -a g 'printf git'\n"
        );
        assert_eq!(
            shell.expand_abbreviations(
                "g ok; zz\nX=1 g assigned\nprintf g\n'g'\necho \" ; g \"\necho \"escaped \\\" g\"\n\\g\n",
            ),
            "printf git ok; printf zed\nX=1 printf git assigned\nprintf g\n'g'\necho \" ; g \"\necho \"escaped \\\" g\"\n\\g\n"
        );
        assert_eq!(shell.run("abbr -e g", &[]).status, 0);
        assert_eq!(shell.expand_abbreviations("g"), "g");
        assert_eq!(shell.run("abbr --erase z", &[]).status, 0);
        assert!(!shell.run("abbr --show", &[]).stdout.is_empty());
        assert!(!shell.run("abbr --help", &[]).stdout.is_empty());
        assert_eq!(shell.run("abbr -e -- --dash c zz", &[]).status, 0);
        assert!(shell.run("abbr --list", &[]).stdout.is_empty());
    }

    #[test]
    /// `supports_extended_zsh_runtime_compatibility`に対応する処理を行う。
    fn supports_extended_zsh_runtime_compatibility() {
        let mut shell = Shell::default();
        shell.run("ISKSH_MODE=zsh", &[]);
        shell.refresh_mode();

        assert_eq!(
            shell
                .run(
                    "setopt AUTO_CD AUTO_PUSHD EXTENDED_GLOB PROMPT_SUBST LOCAL_OPTIONS",
                    &[]
                )
                .status,
            0
        );
        assert!(
            String::from_utf8(shell.run("setopt", &[]).stdout)
                .unwrap()
                .contains("autocd")
        );
        assert_eq!(shell.run("unsetopt NO_AUTO_CD", &[]).status, 0);
        assert_ne!(shell.run("setopt NONO_BAD", &[]).status, 0);

        shell.run("PATH=/one:/two; FPATH=/functions:/more", &[]);
        assert_eq!(
            shell.run("print -r -- ${path[1]}:${fpath[2]}", &[]).stdout,
            b"/one:/more\n"
        );
        shell.run("path=(/bin /usr/bin); fpath=(/tmp /opt)", &[]);
        assert!(shell.value_of("PATH").unwrap().contains("/bin"));
        assert!(shell.array_keys("builtins").contains(&"print".to_string()));
        assert!(!shell.array_keys("commands").is_empty());
        assert_eq!(
            shell.array_value("options", "autopushd").as_deref(),
            Some("on")
        );
        assert!(shell.array_values("signals").contains(&"INT".to_string()));
        assert!(shell.array_values("functions").is_empty());

        shell.run("alias -g G='world'; alias -s txt='print suffix'", &[]);
        assert_eq!(shell.run("print hello G", &[]).stdout, b"hello world\n");
        assert_eq!(shell.run("sample.txt", &[]).stdout, b"suffix sample.txt\n");
        assert!(
            String::from_utf8(shell.run("alias -g -L", &[]).stdout)
                .unwrap()
                .contains("alias -g")
        );
        assert_eq!(shell.run("unalias G txt", &[]).status, 0);

        shell.run(
            "function first second { print function; }; functions -t first; first",
            &[],
        );
        assert!(shell.functions.contains_key("second"));
        assert!(shell.traced_functions.contains("first"));
        assert_eq!(shell.run("functions +t first", &[]).status, 0);
        assert!(!shell.traced_functions.contains("first"));
        assert_eq!(
            shell.run("() { print anonymous; }", &[]).stdout,
            b"anonymous\n"
        );

        shell.run("events=''; pre() { events=${events}p; }; hist() { events=${events}h; }; bye() { events=${events}x; }; add-zsh-hook preexec pre; add-zsh-hook zshaddhistory hist; add-zsh-hook zshexit bye", &[]);
        assert!(shell.record_history("print history"));
        assert!(shell.value_of("events").unwrap().contains('h'));
        shell.run("exit 0", &[]);
        assert!(shell.value_of("events").unwrap().contains('x'));

        assert_eq!(
            shell
                .evaluate_arithmetic("2 ** 3 + (8 >> 1) + (1 ? 2 : 9) | 1")
                .unwrap(),
            15
        );
        assert_eq!(shell.evaluate_arithmetic("16#ff").unwrap(), 255);
        assert_eq!(shell.evaluate_arithmetic("!0 && ~0 != 0 || 0").unwrap(), 1);
        assert_eq!(
            shell.run("[[ abc123 =~ '([a-z]+)([0-9]+)' ]]", &[]).status,
            0
        );
        assert_eq!(shell.value_of("MATCH").as_deref(), Some("abc123"));
        assert_eq!(shell.array_value("match", "1").as_deref(), Some("abc"));
        assert_eq!(shell.run("[[ -o AUTO_PUSHD ]]", &[]).status, 0);

        shell
            .set_variable("SHLVL", "2".into(), None, false)
            .unwrap();
        let prompt = shell
            .expand_zsh_prompt_escapes("%(?.ok.bad):%j:%L:%N:%i:%D{%Y}:%T:%*:%Sx%s:%Uy%u:%E", 0);
        assert!(prompt.starts_with("ok:0:2:isksh:"));
        assert!(prompt.contains("\x1b[7mx\x1b[27m"));
        shell
            .set_variable("RPROMPT", "right:%?".into(), None, false)
            .unwrap();
        assert_eq!(shell.right_prompt(), "right:0");

        assert_eq!(shell.run("zstyle ':completion:*' matcher-list 'm:{a-z}={A-Z}'; zstyle -s ':completion:*' matcher-list MATCHER", &[]).status, 0);
        assert!(shell.value_of("MATCHER").is_some());
        assert_eq!(
            shell
                .run(
                    "compinit; compdef _files edit; compadd -J files -- alpha beta; compset -p 2",
                    &[]
                )
                .status,
            0
        );
        assert_eq!(shell.configured_completion_candidates(), ["alpha", "beta"]);
        shell.run("widget() { BUFFER=changed; region_highlight=(one two); }; zle -N custom widget; bindkey -M main '^X' custom", &[]);
        assert_eq!(shell.run("zle custom", &[]).status, 0);
        assert_eq!(shell.value_of("BUFFER").as_deref(), Some("changed"));
        assert_eq!(shell.run("bindkey -M main '^X'", &[]).stdout, b"custom\n");
        assert_eq!(shell.run("vared VALUE", b"edited\n").status, 0);
        assert_eq!(shell.value_of("VALUE").as_deref(), Some("edited"));

        assert_eq!(
            shell.run("items=(c a b); print -o -a items", &[]).stdout,
            b"a b c\n"
        );
        assert_eq!(shell.run("print -m 'a*' aa bb", &[]).stdout, b"aa\n");
        assert_eq!(
            shell
                .run(
                    "typeset -iu NUMBER=2+3; typeset -l LOWER=ABC; typeset -u UPPER=abc",
                    &[]
                )
                .status,
            0
        );
        assert_eq!(shell.value_of("NUMBER").as_deref(), Some("5"));
        assert_eq!(shell.value_of("LOWER").as_deref(), Some("abc"));
        assert_eq!(shell.value_of("UPPER").as_deref(), Some("ABC"));
        assert_eq!(
            shell.run("whence -w print", &[]).stdout,
            b"print: builtin\n"
        );
        assert_eq!(shell.run("whence -m 'pri*'", &[]).status, 0);
        shell.run("TRAPINT() { TRAPPED=yes; }", &[]);
        assert_eq!(shell.run_trap("INT").status, 0);
        assert_eq!(shell.value_of("TRAPPED").as_deref(), Some("yes"));
    }

    #[test]
    /// `covers_extended_zsh_compatibility_errors_and_variants`に対応する処理を行う。
    fn covers_extended_zsh_compatibility_errors_and_variants() {
        let mut shell = Shell::default();
        shell.run("ISKSH_MODE=zsh", &[]);
        shell.refresh_mode();
        assert!(shell.record_history(" hidden"));
        shell.run(
            "setopt HIST_IGNORE_SPACE HIST_IGNORE_DUPS HIST_REDUCE_BLANKS",
            &[],
        );
        assert!(!shell.record_history(" hidden"));
        assert!(shell.record_history("print   one"));
        assert!(!shell.record_history("print   one"));

        assert_ne!(shell.run("autoload +X missing", &[]).status, 0);
        assert_eq!(shell.run("autoload -Uz name; autoload", &[]).status, 0);
        assert_eq!(
            shell
                .run(
                    "zmodload zsh/complist; zmodload -L; zmodload -u zsh/complist",
                    &[]
                )
                .status,
            0
        );
        assert_ne!(shell.builtin_functions(&["absent".into()]).status, 0);
        assert_eq!(shell.run("zstyle -t ':x' missing", &[]).status, 1);
        assert_eq!(shell.run("zstyle -d ':x' style", &[]).status, 0);
        assert_ne!(shell.run("bindkey -M", &[]).status, 0);
        assert_eq!(
            shell
                .run("bindkey -e; bindkey -N copy main; bindkey -D copy", &[])
                .status,
            0
        );
        assert_ne!(shell.run("zle missing", &[]).status, 0);
        assert_eq!(shell.run("zle -l; zle -D missing; zle -R", &[]).status, 0);
        assert_ne!(shell.run("compset -x 1", &[]).status, 0);
        assert_eq!(shell.run("BUFFER=abcd; compset -s 2", &[]).status, 0);
        assert_eq!(shell.value_of("SUFFIX").as_deref(), Some("cd"));
        assert_ne!(shell.run("print -m '[' a", &[]).status, 0);
        assert_ne!(shell.run("print -a", &[]).status, 0);
        assert_ne!(shell.run("whence -q print", &[]).status, 0);
        assert_ne!(shell.run("emulate -c", &[]).status, 0);
        assert_eq!(
            shell.run("emulate -LR zsh -c 'print local'", &[]).stdout,
            b"local\n"
        );
        assert_ne!(shell.run("[[ -o MISSING ]]", &[]).status, 0);
        assert_ne!(shell.run("[[ file -nt missing ]]", &[]).status, 0);
        assert_eq!(directory_stack_index("+0", 1), Some(0));
        assert_eq!(directory_stack_index("-0", 1), Some(0));
        assert_eq!(directory_stack_index("+2", 1), None);
    }

    #[test]
    /// `covers_remaining_zsh_compatibility_paths`に対応する処理を行う。
    fn covers_remaining_zsh_compatibility_paths() {
        // SAFETY: カバレッジタスクはこのモジュールを単一テストスレッドで実行する。
        unsafe { std::env::set_var("ISKSH_MODE", "zsh") };
        let mut shell = Shell::default();
        unsafe { std::env::remove_var("ISKSH_MODE") };
        assert_eq!(shell.mode(), ShellMode::Zsh);
        let mut bash = shell.clone();
        bash.mode = ShellMode::Bash;
        assert!(bash.record_history("plain"));
        assert!(bash.right_prompt().is_empty());
        shell.variables.remove("RPROMPT");
        shell
            .set_variable("RPS1", "alternate".into(), None, false)
            .unwrap();
        assert_eq!(shell.right_prompt(), "alternate");
        shell.variables.remove("RPS1");
        assert!(shell.right_prompt().is_empty());
        shell.global_aliases.insert("Q".into(), "quoted".into());
        assert!(
            shell
                .expand_zsh_aliases("print \"a\\\"Q\" \\Q")
                .contains("Q")
        );
        shell.zshaddhistory_hooks.push("false".into());
        assert!(!shell.record_history("rejected"));
        shell.zshaddhistory_hooks.clear();

        let root = tempfile::tempdir().unwrap();
        let functions = root.path().join("functions");
        fs::create_dir(&functions).unwrap();
        fs::write(functions.join("loaded"), "print autoloaded $1").unwrap();
        shell
            .set_variable(
                "FPATH",
                functions.to_string_lossy().into_owned(),
                None,
                false,
            )
            .unwrap();
        assert_eq!(
            shell.run("autoload -Uz loaded; loaded value", &[]).stdout,
            b"autoloaded value\n"
        );
        assert!(!shell.autoload_functions.contains("loaded"));
        assert_eq!(shell.run("functions", &[]).status, 0);
        shell.autoload_functions.insert("placeholder".into());
        assert!(
            String::from_utf8(shell.run("functions placeholder", &[]).stdout)
                .unwrap()
                .contains("undefined")
        );
        assert_eq!(shell.run("autoload +X loaded", &[]).status, 0);
        shell.indexed_arrays.insert(
            "fpath".into(),
            [(0, functions.to_string_lossy().into_owned())]
                .into_iter()
                .collect(),
        );
        fs::write(functions.join("indexed_load"), "print indexed").unwrap();
        assert_eq!(
            shell.run("autoload indexed_load; indexed_load", &[]).stdout,
            b"indexed\n"
        );
        assert_ne!(
            shell
                .run("autoload absent_autoload; absent_autoload", &[])
                .status,
            0
        );
        fs::write(functions.join("unreadable"), [0xff]).unwrap();
        let _ = shell.run("autoload +X unreadable", &[]);
        assert_eq!(shell.run("zmodload -f ignored zsh/example", &[]).status, 0);

        let folder = root.path().join("folder");
        let other = root.path().join("other");
        fs::create_dir(&folder).unwrap();
        fs::create_dir(&other).unwrap();
        shell.cwd = root.path().to_path_buf();
        shell.shell_options.insert("autocd".into());
        shell.shell_options.insert("autopushd".into());
        assert_eq!(shell.run("folder", &[]).status, 0);
        assert_eq!(shell.cwd, folder.canonicalize().unwrap());
        assert_eq!(shell.builtin_cd(&["+1".into()]).status, 0);
        shell.cwd = root.path().to_path_buf();
        shell.directory_stack = vec![folder.clone(), other.clone()];
        assert_eq!(shell.builtin_pushd(&["+0".into()]).status, 0);
        assert_eq!(shell.builtin_pushd(&["+2".into()]).status, 0);
        assert_eq!(shell.builtin_dirs(&["-1".into()]).status, 0);
        assert_eq!(shell.builtin_popd(&["+1".into()]).status, 0);
        shell.directory_stack = vec![folder.clone()];
        assert_eq!(shell.builtin_popd(&["+0".into()]).status, 0);
        assert_ne!(shell.builtin_popd(&["+1".into(), "+2".into()]).status, 0);
        shell.directory_stack.clear();
        assert_ne!(shell.builtin_popd(&["+0".into()]).status, 0);
        shell.shell_options.insert("pushdignoredups".into());
        shell.directory_stack = vec![folder.clone()];
        shell.push_directory(folder.clone());
        assert_eq!(shell.directory_stack.len(), 1);

        assert_eq!(shell.run("print -O c a b", &[]).stdout, b"c b a\n");
        assert_eq!(shell.run("print -l a b", &[]).stdout, b"a\nb\n");
        assert_eq!(shell.run("print -N a b", &[]).stdout, b"a\0b\0");
        assert_eq!(shell.run("print -C2 a b c", &[]).stdout, b"a b\nc\n");
        assert_eq!(shell.run("print -c 2 a b c", &[]).stdout, b"a b\nc\n");
        assert_eq!(shell.run("print -bDispSz value", &[]).status, 0);
        assert_ne!(shell.run("print -C", &[]).status, 0);

        shell.run("alias normal='print normal'; named() { :; }", &[]);
        let external_normal = root.path().join("normal");
        fs::write(&external_normal, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            let mut permissions = fs::metadata(&external_normal).unwrap().permissions();
            permissions.set_mode(0o700);
            fs::set_permissions(&external_normal, permissions).unwrap();
        }
        let old_path = shell.value_of("PATH").unwrap_or_default();
        shell
            .set_variable(
                "PATH",
                format!(
                    "{}{}{}",
                    root.path().display(),
                    if cfg!(windows) { ";" } else { ":" },
                    old_path
                ),
                None,
                false,
            )
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let invalid = root.path().join(std::ffi::OsString::from_vec(vec![0xff]));
            fs::write(&invalid, "x").unwrap();
            let _ = shell.command_names_from_path();
            fs::remove_file(invalid).unwrap();
        }
        let configured_path = shell.value_of("PATH").unwrap();
        shell
            .set_variable(
                "PATH",
                root.path()
                    .join("absent-path")
                    .to_string_lossy()
                    .into_owned(),
                None,
                false,
            )
            .unwrap();
        assert!(shell.command_names_from_path().is_empty());
        shell
            .set_variable("PATH", configured_path, None, false)
            .unwrap();
        shell
            .suffix_aliases
            .insert("suffix".into(), "print suffix".into());
        assert!(
            String::from_utf8(shell.run("alias -s -L", &[]).stdout)
                .unwrap()
                .contains("alias -s")
        );
        assert_eq!(
            shell.run("thing.suffix extra argument", &[]).stdout,
            b"suffix thing.suffix extra argument\n"
        );
        assert_eq!(shell.run("whence -pf normal", &[]).status, 1);
        assert_eq!(shell.run("whence -p named", &[]).status, 1);
        assert_eq!(shell.run("whence -f named", &[]).status, 0);
        assert_eq!(shell.run("whence -c sh", &[]).status, 0);
        assert_eq!(shell.run("whence -a print", &[]).status, 0);
        assert_eq!(shell.run("whence -a normal", &[]).status, 0);
        assert_eq!(shell.run("whence -S sh", &[]).status, 0);

        assert_eq!(
            shell
                .run(
                    "zstyle ':x' list one two; zstyle -a ':x' list STYLE_ARRAY",
                    &[]
                )
                .status,
            0
        );
        assert_eq!(shell.array_values("STYLE_ARRAY"), ["one", "two"]);
        assert_eq!(shell.run("zstyle -s ':none' value TARGET", &[]).status, 1);
        assert_eq!(shell.run("zstyle -s ':x' list", &[]).status, 1);
        assert_eq!(shell.run("zstyle -s", &[]).status, 1);
        assert_ne!(shell.run("zstyle", &[]).status, 0);
        shell.run("bindkey -M main key widget", &[]);
        assert!(
            String::from_utf8(shell.run("bindkey -L", &[]).stdout)
                .unwrap()
                .contains("widget")
        );
        assert_eq!(shell.run("bindkey -A copy", &[]).status, 0);
        assert_eq!(shell.run("bindkey -D copy", &[]).status, 0);
        assert_ne!(shell.run("bindkey -A", &[]).status, 0);
        assert_eq!(shell.run("bindkey -M empty", &[]).status, 0);
        assert_eq!(
            shell.builtin_bindkey(&["-M".into(), "main".into()]).status,
            0
        );
        assert_eq!(
            shell
                .builtin_bindkey(&["-M".into(), "main".into(), "direct".into(), "widget".into()])
                .status,
            0
        );
        assert_eq!(
            shell
                .builtin_bindkey(&["direct2".into(), "widget".into()])
                .status,
            0
        );
        assert_eq!(shell.run("bindkey -M main absent", &[]).status, 1);
        assert_ne!(shell.run("zle -N", &[]).status, 0);
        shell.run("default_widget() { :; }; zle -N default_widget", &[]);
        assert!(
            String::from_utf8(shell.run("zle -l", &[]).stdout)
                .unwrap()
                .contains("default_widget")
        );
        shell.zle_widgets.insert("broken".into(), "absent".into());
        assert_ne!(shell.run("zle broken", &[]).status, 0);
        assert_eq!(shell.run("zle", &[]).status, 0);
        assert_ne!(shell.run("vared", &[]).status, 0);
        shell
            .set_variable("CURRENT", "kept".into(), None, false)
            .unwrap();
        assert_eq!(shell.builtin_vared(&["CURRENT".into()], b"").status, 0);
        shell.run("typeset -r LOCKED=value", &[]);
        assert_ne!(shell.builtin_vared(&["LOCKED".into()], b"new\n").status, 0);
        assert_eq!(shell.run("compdef", &[]).status, 1);
        assert_eq!(
            shell.run("compadd -M matcher -P pre -- gamma", &[]).status,
            0
        );
        assert_eq!(shell.run("compadd -M", &[]).status, 0);
        assert_eq!(shell.run("compadd gamma", &[]).status, 0);
        assert_eq!(shell.run("compadd -q", &[]).status, 0);
        assert_eq!(shell.run("compset", &[]).status, 1);

        assert_eq!(
            shell
                .run("integer INT=3+4; float FLOAT=5; private PRIVATE=x", &[])
                .status,
            0
        );
        assert_eq!(shell.run("typeset -x EXPORTED=value; typeset -r READONLY; typeset -L5 PAD=x; typeset -th META=y", &[]).status, 0);
        assert_eq!(shell.value_of("PAD").as_deref(), Some("    x"));
        assert_ne!(shell.run("typeset -i BAD=invalid+", &[]).status, 0);
        assert_ne!(shell.run("typeset -r LOCKED=again", &[]).status, 0);
        assert_ne!(shell.run("typeset -x LOCKED", &[]).status, 0);
        assert_eq!(shell.run("typeset -r FRESH", &[]).status, 0);
        assert_eq!(shell.run("typeset -x NEW_EXPORT", &[]).status, 0);
        assert_eq!(shell.run("typeset PLAIN", &[]).status, 0);
        assert_eq!(shell.run("alias -- normal", &[]).status, 0);
        assert_ne!(shell.run("alias -x", &[]).status, 0);
        assert_eq!(shell.run("unalias -a", &[]).status, 0);

        fs::write(root.path().join("keep.txt"), "x").unwrap();
        fs::write(root.path().join("skip.log"), "x").unwrap();
        shell.cwd = root.path().to_path_buf();
        shell.shell_options.insert("extendedglob".into());
        assert_eq!(shell.run("print ^*.log", &[]).status, 0);
        assert_eq!(shell.run("print *~*.log", &[]).status, 0);
        assert_ne!(shell.run("print *.missing", &[]).status, 0);
        assert_ne!(shell.run("print *~[", &[]).status, 0);

        shell
            .set_variable("USER", "root".into(), None, false)
            .unwrap();
        shell
            .set_variable("SHLVL", "0".into(), None, false)
            .unwrap();
        shell.function_stack.push("inside".into());
        let prompt = shell.expand_zsh_prompt_escapes(
            "%(#.root.user)%(L.high.low)%(x.y.n)%(broken):%12~:%12x:%_:%D:%t:%@:%G",
            1,
        );
        assert!(prompt.contains("rootlow"));
        assert!(prompt.contains("inside"));
        assert_eq!(shell.expand_zsh_prompt_escapes("%(", 0), "");
        shell.variables.remove("SHLVL");
        assert!(shell.expand_zsh_prompt_escapes("%(L.y.n)", 0).contains('n'));
        assert_eq!(shell.expand_zsh_prompt_escapes("%L", 0), "1");

        shell.run("shown() { :; }", &[]);
        assert!(shell.array_value("functions", "shown").is_some());
        assert_eq!(
            shell.array_value("builtins", "print").as_deref(),
            Some("defined")
        );
        assert!(
            shell
                .array_values("functions")
                .contains(&"shown".to_string())
        );
        assert!(
            shell
                .array_values("builtins")
                .contains(&"print".to_string())
        );
        assert!(shell.array_value("commands", "sh").is_some());
        assert_eq!(
            shell.array_value("options", "missing").as_deref(),
            Some("off")
        );
        assert_eq!(
            shell
                .zsh_array_element(vec!["a".into(), "b".into()], "-1")
                .as_deref(),
            Some("b")
        );
        assert_eq!(
            shell.array_value("funcstack", "1").as_deref(),
            Some("inside")
        );
        assert_eq!(shell.array_value("signals", "1").as_deref(), Some("EXIT"));
        assert!(shell.array_keys("unknown").is_empty());
        assert!(shell.array_values("unknown").is_empty());
        shell.indexed_arrays.insert(
            "ordinary".into(),
            [(0, "value".into())].into_iter().collect(),
        );
        assert_eq!(shell.array_values("ordinary"), ["value"]);
        shell.associative_arrays.insert(
            "mapping".into(),
            [("key".into(), "mapped".into())].into_iter().collect(),
        );
        assert_eq!(shell.array_values("mapping"), ["mapped"]);
        let job = std::thread::spawn(|| ExecResult::status(0));
        shell.background_jobs.lock().unwrap().insert(77, job);
        assert!(shell.expand_zsh_prompt_escapes("%(j.y.n)", 0).contains('y'));
        assert_eq!(
            shell.array_value("jobstates", "%77").as_deref(),
            Some("running")
        );
        assert!(shell.array_keys("jobstates").contains(&"77".to_string()));
        let _ = shell.builtin_wait(&["77".into()]);

        let poisoned = shell.background_jobs.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoned.lock().unwrap();
            panic!("poison jobs lock for prompt fallback");
        })
        .join();
        assert!(shell.expand_zsh_prompt_escapes("%(j.y.n)", 0).contains('n'));
        assert_eq!(shell.expand_zsh_prompt_escapes("%j", 0), "0");

        let file = root.path().join("file");
        fs::write(&file, "data").unwrap();
        #[cfg(unix)]
        let link = {
            let link = root.path().join("link");
            std::os::unix::fs::symlink(&file, &link).unwrap();
            assert_eq!(
                shell
                    .builtin_double_bracket(&[
                        "-L".into(),
                        link.to_string_lossy().into_owned(),
                        "]]".into()
                    ])
                    .status,
                0
            );
            link
        };
        assert_eq!(
            shell
                .builtin_double_bracket(&[
                    "-w".into(),
                    file.to_string_lossy().into_owned(),
                    "]]".into()
                ])
                .status,
            0
        );
        assert_eq!(
            shell
                .builtin_double_bracket(&[
                    "-s".into(),
                    file.to_string_lossy().into_owned(),
                    "]]".into()
                ])
                .status,
            0
        );
        assert_eq!(
            shell
                .builtin_double_bracket(&[
                    "-x".into(),
                    external_normal.to_string_lossy().into_owned(),
                    "]]".into()
                ])
                .status,
            0
        );
        assert_eq!(shell.run("[[ -s absent-file ]]", &[]).status, 1);
        assert_eq!(shell.run("[[ -L absent-file ]]", &[]).status, 1);
        shell
            .set_variable("VISIBLE", "yes".into(), None, false)
            .unwrap();
        assert_eq!(shell.run("[[ -v VISIBLE ]]", &[]).status, 0);
        assert_eq!(shell.run("[[ -v ABSENT ]]", &[]).status, 1);
        assert_eq!(shell.run("[[ nope =~ '[0-9]+' ]]", &[]).status, 1);
        assert_eq!(shell.run("[[ y =~ '(x)?y' ]]", &[]).status, 0);
        assert_eq!(shell.run("[[ file -ef file ]]", &[]).status, 0);
        assert_ne!(shell.run("[[ absent-file -ef file ]]", &[]).status, 0);
        assert_ne!(shell.run("[[ bad -eq 1 ]]", &[]).status, 0);
        assert_ne!(shell.run("[[ 1 -eq bad ]]", &[]).status, 0);
        assert_eq!(shell.run("[[ file -nt file ]]", &[]).status, 1);
        assert_eq!(shell.run("[[ file -ot file ]]", &[]).status, 1);
        assert_ne!(shell.run("[[ -q value ]]", &[]).status, 0);
        assert_ne!(shell.run("[[ a -unknown b ]]", &[]).status, 0);

        #[cfg(unix)]
        {
            use std::os::unix::net::UnixListener;
            let fifo = root.path().join("fifo");
            nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRUSR).unwrap();
            assert!(path_has_unix_type(&fifo, true));
            let socket = root.path().join("socket");
            let _listener = UnixListener::bind(&socket).unwrap();
            assert!(path_has_unix_type(&socket, false));
            let _ = link;
        }

        assert_eq!(shell.evaluate_arithmetic("5 ^ 3 & 7").unwrap(), 6);
        assert_eq!(
            shell
                .evaluate_arithmetic("2 <= 2 && 3 >= 2 && 1 < 2 && 2 > 1")
                .unwrap(),
            1
        );
        assert_eq!(
            shell
                .evaluate_arithmetic("1 == 1 && 1 != 2 && (8 << 1) == 16")
                .unwrap(),
            1
        );
        assert!(shell.evaluate_arithmetic("1 ? 2").is_err());
        assert!(shell.evaluate_arithmetic("0xGG").is_err());
        assert!(shell.evaluate_arithmetic("bad#1").is_err());
        assert!(shell.evaluate_arithmetic("16#zz").is_err());
    }
}
