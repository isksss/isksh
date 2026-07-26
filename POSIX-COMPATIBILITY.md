# POSIX.1-2024 compatibility

基準文書は[Shell Command Language, POSIX.1-2024](https://pubs.opengroup.org/onlinepubs/9799919799/utilities/V3_chap02.html)です。

| 分野 | 状態 | 備考 |
|---|---|---|
| Token recognition / quoting | Partial | UTF-8入力のみ |
| Simple commands / assignments | Partial | 基本的な検索・実行に対応 |
| Parameter expansion | Partial | 既定値演算子、入れ子展開、`#`・`##`・`%`・`%%`パターン削除に対応 |
| Command / arithmetic substitution | Partial | 基本形式に対応 |
| Field splitting / pathname expansion | Partial | IFS空白・非空白区切り、nullglob、dotglobに対応。UTF-8とホストファイルシステムに依存 |
| Redirection / here-document | Partial | 任意番号の永続ディスクリプタと基本リダイレクトに対応。OSハンドルの完全継承は段階実装 |
| Pipelines / AND-OR lists | Partial | 外部コマンド間は並列OSパイプ。built-in・関数を含む場合は内部バッファを使用 |
| Compound commands | Partial | if/for/while/until/group/subshellを優先 |
| Functions | Partial | 基本的な定義・呼び出しに対応 |
| Built-in utilities | Partial | trap、umask、hash、exec永続リダイレクト、read、printfを含むMVP対象を実装 |
| Job control | Partial | 非同期`&`、ジョブID、jobs、waitに対応。停止・再開・fg・bgは未対応 |
| Interactive shell | Partial | PS1/PS2、PROMPT_COMMAND、履歴、補完、Ctrl+R、継続入力、外部コマンドのTTY・基本シグナル制御に対応 |
| Startup configuration | Partial | `.config/isksh`配下の`.iskenv`、`.iskprofile`、`.iskrc`を起動種別に応じて読込 |
| Bash extensions | Partial | 配列、`[[ ]]`、process substitution、PIPESTATUS、pipefail、ディレクトリスタック、`let`、`printf -v`、主要ツールのBash初期化変換を実装。高度な配列属性・拡張glob等は未対応 |
| zsh compatibility mode | Partial | `ISKSH_MODE=zsh`でtied/special parameter、主要option、autoload/sticky function、global/suffix alias、主要hook、completion/ZLE状態、拡張PROMPT、算術・`[[ ]]`、zsh系builtinに対応。zshの全構文、全option、全module、terminal widgetの完全再現は未対応 |
| Non-UTF-8 shell data | Not supported | 明示的エラー |

WindowsではPOSIXシグナル、プロセスグループ、パス表現を完全には再現できません。`.cmd`と`.bat`は`cmd.exe`経由で実行し、PowerShellスクリプトは自動実行しません。
