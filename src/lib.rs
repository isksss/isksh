//! A portable, POSIX-oriented shell with optional Bash and zsh compatibility behavior.
//!
//! The crate exposes [`Shell`] for executing source text, startup-file helpers, and
//! [`run_interactive`] for embedding the interactive read-evaluate-print loop.

#![warn(missing_docs)]

mod ast;
mod interactive;
mod lexer;
mod parser;
mod shell;
mod startup;

pub use interactive::run_interactive;
pub use shell::{InputState, RunResult, Shell, ShellMode};
pub use startup::{StartupFiles, load_startup_file, startup_files};
