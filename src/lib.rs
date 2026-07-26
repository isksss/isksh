//! Bash・zsh互換動作を選択できる、移植性を重視したPOSIX指向のシェル。
//!
//! ソースを実行する[`Shell`]、起動ファイル用の補助関数、対話的な読み取り・評価・表示
//! ループを組み込む[`run_interactive`]を公開する。

#![warn(missing_docs)]

mod ast;
mod i18n;
mod interactive;
mod lexer;
mod parser;
mod shell;
mod startup;

#[doc(hidden)]
pub use i18n::{cli_help, localize};
pub use interactive::run_interactive;
pub use shell::{InputState, RunResult, Shell, ShellMode};
pub use startup::{StartupFiles, load_startup_file, startup_files};
