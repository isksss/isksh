# POSIX.1-2024 compatibility

基準文書は[Shell Command Language, POSIX.1-2024](https://pubs.opengroup.org/onlinepubs/9799919799/utilities/V3_chap02.html)です。

| 分野 | 状態 | 備考 |
|---|---|---|
| Token recognition / quoting | Partial | UTF-8入力のみ |
| Simple commands / assignments | Partial | 基本的な検索・実行に対応 |
| Parameter expansion | Partial | 基本形式と既定値演算子に対応 |
| Command / arithmetic substitution | Partial | 基本形式に対応 |
| Field splitting / pathname expansion | Partial | UTF-8とホストファイルシステムに依存 |
| Redirection / here-document | Partial | 基本リダイレクトに対応、here-documentは段階実装 |
| Pipelines / AND-OR lists | Supported | MVP範囲 |
| Compound commands | Partial | if/for/while/until/group/subshellを優先 |
| Functions | Partial | 基本的な定義・呼び出しに対応 |
| Built-in utilities | Partial | MVP対象を実装。trap/umask/hash/execの完全なOS動作は未対応 |
| Job control | Not supported | MVP対象外 |
| Interactive shell | Partial | PS1/PS2、PROMPT_COMMAND、Bash形式の主要プロンプト展開、継続入力、状態保持、exit、EOF、外部コマンドのTTY継承に対応。履歴・補完・ジョブ制御は未対応 |
| Startup configuration | Partial | `.config/isksh/.iskrc`を読込。isksh対応範囲のbashrc形式を利用可能 |
| Bash extensions | Partial | indexed/associative arrays、`[[ ]]`、process substitution、主要bashrc built-inを実装。高度な配列属性・拡張glob等は未対応 |
| Non-UTF-8 shell data | Not supported | 明示的エラー |

WindowsではPOSIXシグナル、プロセスグループ、パス表現を完全には再現できません。`.cmd`と`.bat`は`cmd.exe`経由で実行し、PowerShellスクリプトは自動実行しません。
