# isksh

[![CI](https://github.com/isksss/isksh/actions/workflows/ci.yml/badge.svg)](https://github.com/isksss/isksh/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/isksh.svg)](https://crates.io/crates/isksh)
[![aqua](https://img.shields.io/github/v/release/isksss/isksh?label=aqua&color=2e9afe)](https://github.com/aquaproj/aqua-registry/blob/main/pkgs/isksss/isksh/registry.yaml)

[日本語](README.ja.md) | [简体中文](README.zh-CN.md)

`isksh` is a cross-platform shell written in Rust. It targets the POSIX.1-2024 Shell Command Language and supports practical Bash syntax used by common dotfiles and command-line tools.

The project is under active development and is not yet fully POSIX- or Bash-compatible. See [POSIX-COMPATIBILITY.md](POSIX-COMPATIBILITY.md) for known differences.

## Install

With Rust and Cargo:

```console
cargo install isksh --locked
```

Standalone release binaries are available from [GitHub Releases](https://github.com/isksss/isksh/releases).

With [aqua](https://aquaproj.github.io/) from the Standard Registry:

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
isksh -l
```

Running `isksh` without arguments starts interactive mode when standard input is a terminal; otherwise it reads a script from standard input.

## Highlights

- Commands, pipelines, redirections, here-documents, functions, loops, conditionals, and background jobs
- POSIX parameter, command, arithmetic, field-splitting, and pathname expansion
- Interactive editing, history, completion, prompt expansion, and `Ctrl+R` search
- Common Bash features including arrays, `[[ ... ]]`, process substitution, aliases, and frequently used built-ins
- Interactive command abbreviations with fish-style `abbr -a NAME EXPANSION`
- Bash-style initialization for Starship, mise, zoxide, Atuin, and fzf
- Optional zsh compatibility mode with tied/special parameters, functional options, autoloaded and sticky functions, aliases, hooks, completion/ZLE state, extended prompts, arithmetic and conditional expressions, and zsh-oriented built-ins
- Native terminal handoff for Vim, Neovim, and other full-screen applications
- UTF-8 scripts with LF or CRLF line endings

Startup files live only under `$XDG_CONFIG_HOME/isksh` (or `$HOME/.config/isksh`):

1. `.iskenv` for every shell
2. `.iskprofile` for login shells (`-l`, `--login`, `-il`, or `-li`)
3. `.iskrc` for interactive shells

`ISKSH_MODE` defaults to `bash`. Set `ISKSH_MODE=zsh` in the process environment or in `.iskenv` to enable zsh compatibility for the later startup files. Unknown values fall back to `bash`.

isksh's own help, informational messages, and diagnostics support English, Japanese, and Simplified Chinese. Set `ISKSH_LANG=en`, `ISKSH_LANG=ja`, or `ISKSH_LANG=zh`. When it is unset, `LC_ALL`, `LC_MESSAGES`, `LANGUAGE`, and `LANG` are checked in that order; unsupported or missing values fall back to English. Output produced by external commands is never translated.

In zsh mode, unquoted scalar parameters remain a single field by default. Use `setopt SH_WORD_SPLIT` when zsh-compatible field splitting is required. Option names are case-insensitive, ignore underscores, and support a single `no` prefix inversion.

The compatibility layer includes practical subsets of `autoload`, `functions`, `zstyle`, `compinit`, `compdef`, `compadd`, `compset`, `bindkey`, `zle`, and `vared`. Interactive completion combines command/file candidates with values registered by `compadd`; it does not reproduce every zsh module or terminal-editing behavior.

## Platforms

| Platform | Architectures | Support |
|---|---|---|
| Linux | x86_64, aarch64 | Tested; fully static musl binaries |
| Windows 11 | x86_64 | Tested; static GNU CRT, Windows system DLLs only |
| macOS | x86_64, aarch64 | Tested on native runners; release binaries and aqua installation |

## Development

Development is containerized; Rust does not need to be installed on the host.

See [CONTRIBUTING.md](CONTRIBUTING.md) for the development workflow and branch, commit, pull request, and release conventions.

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
