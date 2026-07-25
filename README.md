# isksh

[![CI](https://github.com/isksss/isksh/actions/workflows/ci.yml/badge.svg)](https://github.com/isksss/isksh/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/isksh.svg)](https://crates.io/crates/isksh)

[日本語](README.ja.md)

`isksh` is a cross-platform shell written in Rust. It targets the POSIX.1-2024 Shell Command Language and supports practical Bash syntax used by common dotfiles and command-line tools.

The project is under active development and is not yet fully POSIX- or Bash-compatible. See [POSIX-COMPATIBILITY.md](POSIX-COMPATIBILITY.md) for known differences.

## Install

With Rust and Cargo:

```console
cargo install isksh --locked
```

Standalone release binaries are available from [GitHub Releases](https://github.com/isksss/isksh/releases).

With [aqua](https://aquaproj.github.io/), after the package is available in the Standard Registry:

```console
aqua g -i isksss/isksh
aqua install
```

## Usage

```console
isksh SCRIPT [ARG...]
isksh -c COMMAND [NAME [ARG...]]
isksh -s [ARG...]
isksh -i
```

Running `isksh` without arguments starts interactive mode when standard input is a terminal; otherwise it reads a script from standard input.

## Highlights

- Commands, pipelines, redirections, here-documents, functions, loops, conditionals, and background jobs
- POSIX parameter, command, arithmetic, field-splitting, and pathname expansion
- Interactive editing, history, completion, prompt expansion, and `Ctrl+R` search
- Common Bash features including arrays, `[[ ... ]]`, process substitution, aliases, and frequently used built-ins
- Interactive command abbreviations with fish-style `abbr -a NAME EXPANSION`
- Bash-style initialization for Starship, mise, zoxide, Atuin, and fzf
- Native terminal handoff for Vim, Neovim, and other full-screen applications
- UTF-8 scripts with LF or CRLF line endings

Interactive startup reads the first available file below:

1. `ISKSH_RC`
2. `$XDG_CONFIG_HOME/isksh/.iskrc`
3. `$HOME/.config/isksh/.iskrc`
4. `$HOME/.bashrc` as a compatibility fallback

## Platforms

| Platform | Architectures | Support |
|---|---|---|
| Linux | x86_64, aarch64 | Tested; fully static musl binaries |
| Windows 11 | x86_64 | Tested; static GNU CRT, Windows system DLLs only |
| macOS | x86_64, aarch64 | Cross-target compile checks only |

## Development

Development is containerized; Rust does not need to be installed on the host.

```console
docker compose build dev
docker compose run --rm dev mise run check-all
```

`check-all` runs formatting, Clippy, tests, 100% line-coverage enforcement, cross-target checks, release builds, and static dependency verification. Windows host behavior is tested with:

```powershell
.\scripts\windows-smoke.ps1
```

Pushing a `vX.Y.Z` tag matching the Cargo package version publishes the crate through crates.io Trusted Publishing, then creates the GitHub Release.

## License

Licensed under either [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
