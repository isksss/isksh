# isksh

[![CI](https://github.com/isksss/isksh/actions/workflows/ci.yml/badge.svg)](https://github.com/isksss/isksh/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/isksh.svg)](https://crates.io/crates/isksh)
[![aqua](https://img.shields.io/github/v/release/isksss/isksh?label=aqua&color=2e9afe)](https://github.com/aquaproj/aqua-registry/blob/main/pkgs/isksss/isksh/registry.yaml)

[English](README.md) | [日本語](README.ja.md)

`isksh` 是一个使用 Rust 编写的跨平台 shell。它以兼容 POSIX.1-2024 Shell Command Language 为目标，并支持常见 dotfiles 和命令行工具使用的实用 Bash 语法。

本项目仍在积极开发中，尚未完全兼容 POSIX 或 Bash。已知差异请参阅 [POSIX-COMPATIBILITY.md](POSIX-COMPATIBILITY.md)。

## 安装

使用 Rust 和 Cargo：

```console
cargo install isksh --locked
```

也可以从 [GitHub Releases](https://github.com/isksss/isksh/releases) 获取独立发布二进制文件。

通过 Standard Registry 使用 [aqua](https://aquaproj.github.io/)：

```console
aqua g -i isksss/isksh
aqua install
```

## 用法

```console
isksh SCRIPT [ARG...]
isksh -c COMMAND [NAME [ARG...]]
isksh -s [ARG...]
isksh -i
isksh -l
```

不带参数运行 `isksh` 时，如果标准输入是终端，则启动交互模式；否则从标准输入读取脚本。

## 主要功能

- 命令、管道、重定向、here-document、函数、循环、条件和后台作业
- POSIX 参数展开、命令替换、算术展开、字段拆分和路径名展开
- 交互编辑、历史记录、补全、提示符展开和 `Ctrl+R` 搜索
- 数组、`[[ ... ]]`、进程替换、别名和常用内置命令等 Bash 功能
- 使用 fish 风格的 `abbr -a NAME EXPANSION` 缩写交互命令
- Starship、mise、zoxide、Atuin 和 fzf 的 Bash 风格初始化
- 可选的 zsh 兼容模式，支持联动参数和特殊参数、有效选项、自动加载函数和 sticky 函数、别名、钩子、补全和 ZLE 状态、扩展提示符、算术表达式、条件表达式及 zsh 风格内置命令
- 将终端控制权交给 Vim、Neovim 等全屏应用程序
- 支持 LF 或 CRLF 换行的 UTF-8 脚本

启动文件仅位于 `$XDG_CONFIG_HOME/isksh`（未设置时为 `$HOME/.config/isksh`）下：

1. 每次启动读取 `.iskenv`
2. 登录 shell（`-l`、`--login`、`-il` 或 `-li`）读取 `.iskprofile`
3. 交互 shell 读取 `.iskrc`

`ISKSH_MODE` 默认为 `bash`。在进程环境或 `.iskenv` 中设置 `ISKSH_MODE=zsh`，可为后续启动文件启用 zsh 兼容模式。未知值会回退到 `bash`。

isksh 自身的帮助、普通消息和诊断支持英语、日语和简体中文。请设置 `ISKSH_LANG=en`、`ISKSH_LANG=ja` 或 `ISKSH_LANG=zh`。未设置时依次检查 `LC_ALL`、`LC_MESSAGES`、`LANGUAGE` 和 `LANG`；不支持或缺失的值会回退到英语。外部命令产生的输出不会被翻译。

在 zsh 模式下，未加引号的标量参数默认保持为单个字段。需要 zsh 兼容字段拆分时，请使用 `setopt SH_WORD_SPLIT`。选项名不区分大小写、忽略下划线，并支持一次前缀 `no` 反转。

兼容层实现了 `autoload`、`functions`、`zstyle`、`compinit`、`compdef`、`compadd`、`compset`、`bindkey`、`zle` 和 `vared` 的实用子集。交互补全会合并命令和文件候选项以及通过 `compadd` 注册的值，但不会完整重现所有 zsh 模块或终端编辑行为。

## 支持平台

| 平台 | 架构 | 支持状态 |
|---|---|---|
| Linux | x86_64、aarch64 | 已测试；完全静态的 musl 二进制文件 |
| Windows 11 | x86_64 | 已测试；静态 GNU CRT，仅依赖 Windows 系统 DLL |
| macOS | aarch64 | 仅支持Apple Silicon；Intel Mac的最后支持版本为v0.5.0 |

## 开发

开发环境已容器化，无需在主机上安装 Rust。

开发流程以及分支、提交、Pull Request 和发布规范请参阅[CONTRIBUTING.md](CONTRIBUTING.md)。

```console
docker compose build dev
docker compose run --rm dev mise run check-all
```

`check-all`会执行Rust和Markdown格式检查、Clippy、Markdown lint、测试、100%行覆盖率检查、交叉目标检查、发布构建和静态依赖验证。使用`docker compose run --rm dev mise run fmt-markdown`格式化所有Markdown文件。Windows主机行为使用以下命令测试：

```powershell
.\scripts\windows-smoke.ps1
```

推送与 Cargo 包版本一致的 `vX.Y.Z` 标签后，会先通过 Trusted Publishing 发布到 crates.io，然后创建 GitHub Release。

## 许可证

可选择 [MIT](LICENSE-MIT) 或 [Apache-2.0](LICENSE-APACHE) 许可证。
