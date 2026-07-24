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
- 外部コマンド間の並列OSパイプ、`PIPESTATUS`、`set -o pipefail`
- 非同期バックグラウンドジョブ、`$!`、`jobs`、`wait`
- `if`、`case`、`for`、`while`、`until`、グループ、サブシェル、関数
- POSIX系built-inと、対話モードに必要なシェル状態管理
- `PS1`、`PS2`、複数行入力、履歴、補完、履歴検索、`exit`、EOFを扱う対話シェル
- 対話中の外部コマンドへ端末を直接引き渡すTTY実行
- Bash形式のプロンプトエスケープ、プロンプト内の変数・コマンド・算術展開、`PROMPT_COMMAND`
- `.iskrc`による起動設定
- 添字配列・連想配列、`[[ ... ]]`、プロセス置換などの主要Bash拡張
- `source`、`declare`、`typeset`、`local`、`shopt`、`type`、`mapfile`、`readarray`

完全なジョブ制御とUnixシグナル処理は未対応です。`grep`、`sed`、`awk`などの外部ユーティリティは内包せず、実行環境の`PATH`から呼び出します。

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

## Vim・Neovim・Starship

対話モードから起動した単独の外部コマンドは、標準入力・標準出力・標準エラーと端末機能を直接引き継ぎます。そのため、`vim`と`nvim`はWindows/Linuxのどちらでも通常の全画面UIで利用できます。

```sh
vim README.md
nvim README.md
```

パイプライン、リダイレクト、コマンド置換内では、シェルが入出力を接続または捕捉します。

Starshipは`.iskrc`へ次を追加します。この方式はStarshipのプロンプトコマンドを直接使用するため、Windows/Linuxで同じ設定を利用できます。

```sh
PS1='$(starship prompt --status=$?)'
PS2='$(starship prompt --continuation)'
```

`PS1`と`PS2`では、Bash形式の`\u`、`\h`、`\H`、`\w`、`\W`、`\s`、`\v`、`\V`、`\j`、`\n`、`\r`、`\e`、`\$`、`\nnn`、`\[`、`\]`を解釈した後、変数・コマンド・算術展開を行います。`PROMPT_COMMAND`もプライマリプロンプトの直前に実行します。

Starship公式のBash初期化形式も利用できます。

```sh
eval "$(starship init bash)"
```

生成されたBashスクリプトを検出した場合、isksh向けの`PS1`・`PS2`設定へ変換します。コマンド実行時間、右プロンプト、DEBUG trapを利用するpreexec処理など、Bash固有フックに依存する一部機能は対象外です。

## 対話履歴と補完

対話端末ではカーソル移動、編集、Tabによるコマンド・パス補完、上下キーによる履歴、`Ctrl+R`による逆方向履歴検索を利用できます。履歴は次の優先順で保存します。

1. `ISKSH_HISTORY`
2. `$XDG_STATE_HOME/isksh/history`
3. `$HOME/.local/state/isksh/history`
4. Windowsでは`$LOCALAPPDATA/isksh/history`

Unixではフォアグラウンドの外部コマンド・パイプラインへ端末のプロセスグループを移し、`Ctrl+C`後もシェルを継続します。Windowsではコンソール制御ハンドラーにより、フォアグラウンド子プロセス実行中の`Ctrl+C`でシェル自身が終了しないようにします。

## ジョブ、パイプ、ディスクリプタ

- 外部コマンドだけで構成されたパイプラインはOSパイプで並列実行します。
- built-inや関数を含むパイプラインは、現時点では内部バッファを経由します。
- `&`は非同期実行し、`$!`にはiskshのジョブIDを設定します。これはOSのPIDとは限りません。
- `jobs`で状態を表示し、`wait`または`wait %ID`で完了を待機できます。
- `exec 3>file`、`exec 4<file`、`>&3`、`<&4`などの永続ディスクリプタを利用できます。
- `trap`は`EXIT`、`INT`、`TERM`、`DEBUG`の登録・表示・解除に対応します。OSシグナルから任意のtrapを非同期実行する機能は段階実装です。

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
