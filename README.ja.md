# isksh

[![CI](https://github.com/isksss/isksh/actions/workflows/ci.yml/badge.svg)](https://github.com/isksss/isksh/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/isksh.svg)](https://crates.io/crates/isksh)
[![aqua](https://img.shields.io/github/v/release/isksss/isksh?label=aqua&color=2e9afe)](https://github.com/aquaproj/aqua-registry/blob/main/pkgs/isksss/isksh/registry.yaml)

[English](README.md)

`isksh`はRust製のクロスプラットフォームシェルです。POSIX.1-2024 Shell Command Languageへの準拠を目標とし、一般的なdotfilesやCLIツールで使われるBash構文にも対応します。

現在も開発中であり、POSIXやBashとの完全互換ではありません。既知の差異は[POSIX-COMPATIBILITY.md](POSIX-COMPATIBILITY.md)を参照してください。

## インストール

RustとCargoを使用する場合：

```console
cargo install isksh --locked
```

単体配布バイナリは[GitHub Releases](https://github.com/isksss/isksh/releases)から取得できます。

[aqua](https://aquaproj.github.io/) Standard Registryから、次の方法でも導入できます。

```console
aqua g -i isksss/isksh
aqua install
```

## 使用方法

```console
isksh SCRIPT [ARG...]
isksh -c COMMAND [NAME [ARG...]]
isksh -s [ARG...]
isksh -i
isksh -l
```

引数なしで標準入力が端末の場合は対話モードを開始し、それ以外では標準入力からスクリプトを読み込みます。

## 主な機能

- コマンド、パイプ、リダイレクト、here-document、関数、ループ、条件分岐、バックグラウンドジョブ
- POSIX形式のパラメータ・コマンド・算術・フィールド分割・パス名展開
- 対話編集、履歴、補完、プロンプト展開、`Ctrl+R`検索
- 配列、`[[ ... ]]`、プロセス置換、alias、主要built-inなどのBash機能
- 対話コマンドを短縮するfish形式の`abbr -a NAME EXPANSION`
- Starship、mise、zoxide、Atuin、fzfのBash形式初期化
- scalar展開、`${+name}`・`$+functions[name]`、プロンプトescape、`print`、`setopt`、`emulate`、`whence`、`precmd`・`chpwd` hookのzsh互換モード
- Vim、Neovimなどの全画面アプリへの端末引き渡し
- UTF-8スクリプトとLF・CRLF改行

起動ファイルは`$XDG_CONFIG_HOME/isksh`（未設定時は`$HOME/.config/isksh`）配下だけを使用します。

1. すべての起動で`.iskenv`
2. login shell（`-l`、`--login`、`-il`、`-li`）で`.iskprofile`
3. interactive shellで`.iskrc`

`ISKSH_MODE`の既定値は`bash`です。プロセス環境または`.iskenv`で`ISKSH_MODE=zsh`を設定すると、後続の起動ファイルでzsh互換モードが有効になります。不明な値は`bash`へフォールバックします。

zshモードでは、引用符なしのscalar parameterを既定でfield分割しません。zsh互換のfield分割が必要な場合は`setopt SH_WORD_SPLIT`を使います。option名は大文字・小文字を区別せず、underscoreを無視し、先頭の`no`による1回の反転に対応します。

## 対応環境

| 環境 | アーキテクチャ | 対応状況 |
|---|---|---|
| Linux | x86_64、aarch64 | テスト済み、musl完全静的バイナリ |
| Windows 11 | x86_64 | テスト済み、静的GNU CRT、Windows標準DLLのみ使用 |
| macOS | x86_64、aarch64 | クロスターゲットのコンパイル検査のみ |

## 開発

開発環境はコンテナ化されているため、ホストへのRust導入は不要です。

```console
docker compose build dev
docker compose run --rm dev mise run check-all
```

`check-all`はフォーマット、Clippy、テスト、行カバレッジ100%、クロスターゲット検査、リリースビルド、静的依存検証を実行します。Windows実機では次を実行します。

```powershell
.\scripts\windows-smoke.ps1
```

Cargoのバージョンと一致する`vX.Y.Z`タグをpushすると、Trusted Publishingでcrates.ioへ公開した後、GitHub Releaseを作成します。

## ライセンス

[MIT](LICENSE-MIT)または[Apache-2.0](LICENSE-APACHE)を選択できます。
