# isksh

[![CI](https://github.com/isksss/isksh/actions/workflows/ci.yml/badge.svg)](https://github.com/isksss/isksh/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/isksh.svg)](https://crates.io/crates/isksh)

[English](README.md)

`isksh`はRust製のクロスプラットフォームシェルです。POSIX.1-2024 Shell Command Languageへの準拠を目標とし、一般的なdotfilesやCLIツールで使われるBash構文にも対応します。

現在も開発中であり、POSIXやBashとの完全互換ではありません。既知の差異は[POSIX-COMPATIBILITY.md](POSIX-COMPATIBILITY.md)を参照してください。

## インストール

RustとCargoを使用する場合：

```console
cargo install isksh --locked
```

単体配布バイナリは[GitHub Releases](https://github.com/isksss/isksh/releases)から取得できます。

## 使用方法

```console
isksh SCRIPT [ARG...]
isksh -c COMMAND [NAME [ARG...]]
isksh -s [ARG...]
isksh -i
```

引数なしで標準入力が端末の場合は対話モードを開始し、それ以外では標準入力からスクリプトを読み込みます。

## 主な機能

- コマンド、パイプ、リダイレクト、here-document、関数、ループ、条件分岐、バックグラウンドジョブ
- POSIX形式のパラメータ・コマンド・算術・フィールド分割・パス名展開
- 対話編集、履歴、補完、プロンプト展開、`Ctrl+R`検索
- 配列、`[[ ... ]]`、プロセス置換、alias、主要built-inなどのBash機能
- Starship、mise、zoxide、Atuin、fzfのBash形式初期化
- Vim、Neovimなどの全画面アプリへの端末引き渡し
- UTF-8スクリプトとLF・CRLF改行

対話モードでは、次の順で最初に見つかった設定を読み込みます。

1. `ISKSH_RC`
2. `$XDG_CONFIG_HOME/isksh/.iskrc`
3. `$HOME/.config/isksh/.iskrc`
4. 互換用フォールバックの`$HOME/.bashrc`

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
