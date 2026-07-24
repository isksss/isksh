# isksh

`isksh`はRustで実装する、POSIX.1-2024 Shell Command Language互換を目標としたクロスプラットフォームシェルです。

現在はMVP開発段階であり、POSIX完全準拠ではありません。対応状況は[POSIX-COMPATIBILITY.md](POSIX-COMPATIBILITY.md)を参照してください。

## 開発

ホストにはDockerだけが必要です。Rustと関連ツールはmiseによってコンテナ内へ導入されます。

```console
docker compose run --rm dev mise run build
docker compose run --rm dev mise run test
docker compose run --rm dev mise run coverage
docker compose run --rm dev mise run check-all
```

Dev Containerから同じ環境を利用することもできます。

`mise run build-release`は`dist/`へLinux x64/arm64とWindows x64の単体バイナリおよびSHA-256チェックサムを生成します。Windows実行確認はホスト側で次を実行します。

```powershell
.\scripts\windows-smoke.ps1
```

## 使用方法

```console
isksh script.sh arg1 arg2
isksh -c 'name=world; printf "hello %s\n" "$name"'
printf 'echo hello\n' | isksh -s
isksh -i
```

引数なしで標準入力が端末の場合、または`-i`を指定した場合は対話モードになります。`PS1`/`PS2`、複数行の構文入力、シェル状態の保持、`exit`、EOFに対応しています。履歴、補完、ジョブ制御は未対応です。

## 起動設定

対話モードでは次の順番で起動設定ファイルを探索し、最初のパスを読み込みます。

1. `ISKSH_RC`で指定したファイル
2. `$XDG_CONFIG_HOME/isksh/.iskrc`
3. `$HOME/.config/isksh/.iskrc`（Windowsでは`USERPROFILE`も使用）

`ISKSH_RC`を空文字列にすると読込を無効化できます。ファイルはUTF-8で記述します。bashrcで一般的な変数代入、`export`、`alias`、関数、`if`、`case`、ループ、`PS1`、`PS2`に加え、配列、`[[ ... ]]`、プロセス置換、`source`、`declare`、`local`、`shopt`、`type`、`mapfile`/`readarray`を利用できます。

```sh
export EDITOR=vim
alias ll='ls -la'
PS1='isksh> '

greet() {
    printf 'hello %s\n' "$USER"
}
```

Bash拡張は実用的な部分互換です。添字・連想配列の基本代入と参照、`[[ ... ]]`の文字列・数値・ファイル・glob・正規表現・論理演算、`<(...)`と`>(...)`を実装しています。配列スライス、多次元配列、`declare`の高度な属性、拡張globの構文などは未対応です。プロセス置換はWindowsを含む全対象OSで同じ動作にするため一時ファイルへ直列化しており、BashのFIFO/`/dev/fd`による非同期実行とはタイミングが異なります。

スクリプトと変数はUTF-8です。Windowsではパス区切りに`/`を推奨します。外部ユーティリティは同梱せず、実行環境の`PATH`から探索します。

## ライセンス

MITまたはApache License 2.0のいずれかを選択できます。
