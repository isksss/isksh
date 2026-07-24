# isksh

`isksh`はRust製のクロスプラットフォームシェルです。Windows、macOS、Linuxで動作する単体バイナリを提供し、POSIX.1-2024 Shell Command Languageとの互換性を目標にしています。

現在はMVP開発段階です。POSIXおよびBashとの完全互換を保証するものではありません。詳細は[POSIX対応表](POSIX-COMPATIBILITY.md)を参照してください。

## 対象プラットフォーム

| OS | アーキテクチャ | 検証範囲 | 配布形式 |
|---|---|---|---|
| Linux | x86_64 / aarch64 | ビルド・実行・テスト | musl完全静的ELF |
| Windows 11 | x86_64 | ビルド・実機実行 | 静的GNU CRTを使用するEXE |
| macOS | x86_64 / aarch64 | `cargo check` | 将来のmacOS CIでリンク確認予定 |

WindowsバイナリはWindows標準システムDLLを利用します。macOSではOS標準ライブラリまで含めた完全静的リンクは行いません。

## 主な機能

- 単純コマンド、変数代入、終了ステータス、位置・特殊パラメータ
- 単一引用、二重引用、エスケープ
- パラメータ展開、コマンド置換、算術展開、フィールド分割、パス名展開
- リダイレクト、here-document、パイプライン、リスト、`&&`、`||`
- `if`、`case`、`for`、`while`、`until`、グループ、サブシェル、関数
- POSIX系built-inと、対話モードに必要なシェル状態管理
- `PS1`、`PS2`、複数行入力、`exit`、EOFを扱う対話シェル
- `.iskrc`による起動設定
- 添字配列・連想配列、`[[ ... ]]`、プロセス置換などの主要Bash拡張
- `source`、`declare`、`typeset`、`local`、`shopt`、`type`、`mapfile`、`readarray`

履歴、補完、ジョブ制御、完全なUnixシグナル処理は未対応です。`grep`、`sed`、`awk`などの外部ユーティリティは内包せず、実行環境の`PATH`から呼び出します。

## 使用方法

```console
isksh SCRIPT [ARG...]
isksh -c COMMAND [NAME [ARG...]]
isksh -s [ARG...]
isksh -i
```

例：

```sh
isksh -c 'name=world; printf "hello %s\n" "$name"'
isksh script.sh first second
printf 'echo hello\n' | isksh -s
isksh -i
```

引数なしで標準入力が端末の場合、または`-i`を指定した場合は対話モードになります。スクリプトと変数はUTF-8として扱い、非UTF-8入力はエラーになります。LFとCRLFの両方を受理します。

## 起動設定

対話モードでは次の優先順で設定ファイルを選びます。

1. `ISKSH_RC`で指定したパス
2. `$XDG_CONFIG_HOME/isksh/.iskrc`
3. `$HOME/.config/isksh/.iskrc`
4. Windowsでは`$USERPROFILE/.config/isksh/.iskrc`

`ISKSH_RC`を空文字列にすると読み込みを無効化できます。

```sh
export EDITOR=vim
PS1='isksh> '
alias ll='ls -la'

paths=(src tests scripts)

greet() {
    local name=${1:-world}
    if [[ $name != '' ]]; then
        printf 'hello %s\n' "$name"
    fi
}
```

`.bashrc`で利用される一般的な構文を可能な範囲で受理しますが、Bash固有機能は部分互換です。配列スライス、多次元配列、`declare`の高度な属性、拡張globなどは未対応です。

プロセス置換`<(...)`と`>(...)`は全対象OSで動かすため一時ファイルを使用して直列実行します。BashのFIFOまたは`/dev/fd`を使う非同期実行とはタイミングが異なります。

## Windows固有の動作

- `PATH`と`PATHEXT`を使用してコマンドを探索します。
- `.exe`と`.com`は直接起動します。
- `.cmd`と`.bat`は`cmd.exe`経由で起動します。
- PowerShellスクリプトは自動実行しません。
- パス区切りには`/`を推奨します。`\`を含むパスは引用してください。

## 開発環境

ホストに必要なのはDockerです。Rust、mise、Clippy、rustfmt、クロスコンパイル用ツールはDebian 13ベースの開発コンテナへ導入されます。

```console
docker compose build dev
docker compose run --rm dev mise run build
docker compose run --rm dev mise run test
docker compose run --rm dev mise run coverage
docker compose run --rm dev mise run check-all
```

Dev Containerからも同じイメージを利用できます。

利用可能なmiseタスク：

| タスク | 内容 |
|---|---|
| `build` | 開発ビルド |
| `build-release` | Linux・Windows向けリリース成果物の生成 |
| `test` | Rustテストとdash差分テスト |
| `coverage` | 本体ライブラリの行カバレッジ100%を検証 |
| `lint` | Clippyを警告ゼロで実行 |
| `fmt-check` | rustfmt差分を検査 |
| `check-targets` | 全対象ターゲットをコンパイル検査 |
| `verify-static` | Linux・Windows成果物の動的依存を検査 |
| `check-all` | 上記の品質検査を一括実行 |

## リリース成果物

```console
docker compose run --rm dev mise run build-release
```

`dist/`へ次のファイルを生成します。

- `isksh-linux-x86_64`
- `isksh-linux-aarch64`
- `isksh-windows-x86_64.exe`
- 各バイナリの`.sha256`チェックサム

Windowsホストでの実行確認：

```powershell
.\scripts\windows-smoke.ps1
```

## コントリビューション

[CONTRIBUTING.md](CONTRIBUTING.md)を確認し、変更を提出する前に次を実行してください。

```console
docker compose run --rm dev mise run check-all
```

## ライセンス

MIT LicenseまたはApache License 2.0のいずれかを選択できます。
