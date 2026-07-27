//! isksh自身が出力するメッセージの多言語化を提供する。

/// 利用できるメッセージ言語を表す。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MessageLanguage {
    /// 英語を使用する。
    English,
    /// 日本語を使用する。
    Japanese,
    /// 簡体字中国語を使用する。
    Chinese,
}

/// 言語判定に利用する環境変数を優先順に保持する定数。
#[cfg(not(test))]
const LANGUAGE_ENVIRONMENTS: &[&str] = &["ISKSH_LANG", "LC_ALL", "LC_MESSAGES", "LANGUAGE", "LANG"];

/// 日本語で記述されている構文診断と英語訳を対応付ける定数。
const JAPANESE_TO_ENGLISH: &[(&str, &str)] = &[
    (
        "コマンドの区切りが必要です",
        "a command separator is required",
    ),
    ("ifの条件が必要です", "an if condition is required"),
    ("ループ条件が必要です", "a loop condition is required"),
    ("for変数名が必要です", "a for variable name is required"),
    ("無効なfor変数名です", "invalid for variable name"),
    ("caseのwordが必要です", "a case word is required"),
    ("caseパターンが必要です", "a case pattern is required"),
    (
        "caseがesacで閉じられていません",
        "case is not closed with esac",
    ),
    (
        "リダイレクト先が必要です",
        "a redirection target is required",
    ),
    ("コマンドが必要です", "a command is required"),
    (
        "末尾のバックスラッシュに続く文字がありません",
        "nothing follows the trailing backslash",
    ),
    ("二重引用符が閉じられていません", "unclosed double quote"),
    ("置換式が閉じられていません", "unclosed substitution"),
    (
        "算術展開が閉じられていません",
        "unclosed arithmetic expansion",
    ),
    ("バッククォートが閉じられていません", "unclosed backquote"),
    ("引用符が閉じられていません", "unclosed quote"),
    ("が必要です", " is required"),
];

/// 日本語で記述されている構文診断と中国語訳を対応付ける定数。
const JAPANESE_TO_CHINESE: &[(&str, &str)] = &[
    ("コマンドの区切りが必要です", "需要命令分隔符"),
    ("ifの条件が必要です", "需要 if 条件"),
    ("ループ条件が必要です", "需要循环条件"),
    ("for変数名が必要です", "需要 for 变量名"),
    ("無効なfor変数名です", "for 变量名无效"),
    ("caseのwordが必要です", "需要 case 单词"),
    ("caseパターンが必要です", "需要 case 模式"),
    ("caseがesacで閉じられていません", "case 未使用 esac 结束"),
    ("リダイレクト先が必要です", "需要重定向目标"),
    ("コマンドが必要です", "需要命令"),
    (
        "末尾のバックスラッシュに続く文字がありません",
        "末尾反斜杠后没有字符",
    ),
    ("二重引用符が閉じられていません", "双引号未闭合"),
    ("置換式が閉じられていません", "替换表达式未闭合"),
    ("算術展開が閉じられていません", "算术展开未闭合"),
    ("バッククォートが閉じられていません", "反引号未闭合"),
    ("引用符が閉じられていません", "引号未闭合"),
    ("が必要です", " 为必需项"),
];

/// 英語の診断句と日本語訳を対応付ける定数。
const ENGLISH_TO_JAPANESE: &[(&str, &str)] = &[
    (
        "arguments must be valid UTF-8",
        "引数は有効なUTF-8である必要があります",
    ),
    (
        "input must be valid UTF-8",
        "入力は有効なUTF-8である必要があります",
    ),
    ("invalid byte at offset", "無効なバイトの位置"),
    (" is an alias for ", " は次の別名です: "),
    (" is an alias", " は別名です"),
    (" is a shell function", " はシェル関数です"),
    (" is a function", " は関数です"),
    (" is a shell builtin", " はシェル組み込みコマンドです"),
    ("unknown option", "不明なオプション"),
    ("requires a command string", "コマンド文字列が必要です"),
    ("command not found", "コマンドが見つかりません"),
    ("unsupported builtin", "未対応の組み込みコマンドです"),
    ("unsupported option", "未対応のオプションです"),
    ("unsupported argument", "未対応の引数です"),
    ("no such option", "そのようなオプションはありません"),
    ("no such widget", "そのようなウィジェットはありません"),
    ("no such job", "そのようなジョブはありません"),
    ("not in a function", "関数内ではありません"),
    (
        "not a shell builtin",
        "シェル組み込みコマンドではありません",
    ),
    ("not a directory", "ディレクトリではありません"),
    ("not found", "見つかりません"),
    ("too many arguments", "引数が多すぎます"),
    (
        "invalid or readonly variable",
        "無効または読み取り専用の変数です",
    ),
    ("readonly variable", "読み取り専用変数です"),
    ("invalid variable name", "変数名が無効です"),
    ("invalid array name", "配列名が無効です"),
    ("invalid shell option name", "シェルオプション名が無効です"),
    ("invalid abbreviation", "短縮入力が無効です"),
    ("invalid rename", "名前変更が無効です"),
    ("invalid input descriptor", "入力記述子が無効です"),
    (
        "invalid file descriptor duplication",
        "ファイル記述子の複製指定が無効です",
    ),
    (
        "invalid indexed-array subscript",
        "添字配列の添字が無効です",
    ),
    ("bad input descriptor", "入力記述子が不正です"),
    ("bad file descriptor", "ファイル記述子が不正です"),
    ("invalid conditional expression", "条件式が無効です"),
    ("invalid arithmetic expression", "算術式が無効です"),
    ("invalid arithmetic base", "算術の基数が無効です"),
    ("invalid arithmetic constant", "算術定数が無効です"),
    ("invalid assignment", "代入が無効です"),
    ("invalid signal", "シグナルが無効です"),
    ("invalid mask", "マスクが無効です"),
    ("invalid option", "オプションが無効です"),
    ("incomplete format specifier", "書式指定が不完全です"),
    ("invalid name", "名前が無効です"),
    ("invalid pattern", "パターンが無効です"),
    (
        "missing here-document body",
        "ヒアドキュメント本体がありません",
    ),
    ("missing ]]", "]] がありません"),
    ("missing ]", "] がありません"),
    (
        "missing ':' in conditional expression",
        "条件式に ':' がありません",
    ),
    (
        "missing ')' in arithmetic expression",
        "算術式に ')' がありません",
    ),
    (
        "unclosed parameter expansion in here-document",
        "ヒアドキュメント内のパラメータ展開が閉じられていません",
    ),
    (
        "unclosed substitution in here-document",
        "ヒアドキュメント内の置換が閉じられていません",
    ),
    ("ambiguous redirect", "リダイレクト先が曖昧です"),
    ("division by zero", "ゼロ除算です"),
    ("integer expression expected", "整数式が必要です"),
    ("expression required", "式が必要です"),
    ("expected arithmetic operand", "算術オペランドが必要です"),
    (
        "expected zsh parameter name after $+",
        "$+ の後にzshパラメータ名が必要です",
    ),
    (
        "count exceeds positional parameters",
        "個数が位置パラメータ数を超えています",
    ),
    (
        "loop control used outside a loop",
        "ループ外でループ制御が使用されました",
    ),
    ("can only be used in a function", "関数内でのみ使用できます"),
    ("directory stack empty", "ディレクトリスタックが空です"),
    ("no other directory", "ほかのディレクトリがありません"),
    (
        "background job panicked",
        "バックグラウンドジョブが異常終了しました",
    ),
    (
        "input is not valid UTF-8",
        "入力が有効なUTF-8ではありません",
    ),
    (
        "command substitution produced non-UTF-8 output",
        "コマンド置換がUTF-8以外を出力しました",
    ),
    ("no matches found", "一致するものが見つかりません"),
    ("filename required", "ファイル名が必要です"),
    ("variable name required", "変数名が必要です"),
    ("keymap name required", "キーマップ名が必要です"),
    ("action and signal are required", "動作とシグナルが必要です"),
    ("requires a variable name", "変数名が必要です"),
    ("requires an array name", "配列名が必要です"),
    ("requires a keymap", "キーマップが必要です"),
    ("requires a widget", "ウィジェットが必要です"),
    ("requires a name", "名前が必要です"),
    ("requires a command", "コマンドが必要です"),
    ("expected EVENT FUNCTION", "EVENT FUNCTIONが必要です"),
    (
        "expected PATTERN STYLE [VALUE ...]",
        "PATTERN STYLE [VALUE ...]が必要です",
    ),
    (
        "array assignment requires a closing ')'",
        "配列代入を閉じる ')' が必要です",
    ),
    ("requires an argument", "引数が必要です"),
    ("requires a format", "書式が必要です"),
    ("unsupported hook", "未対応のフックです"),
    ("invalid job", "ジョブが無効です"),
    ("no such function", "そのような関数はありません"),
    ("OLDPWD not set", "OLDPWDが設定されていません"),
    ("no help topic", "ヘルプ項目がありません"),
    ("unknown unary operator", "不明な単項演算子"),
    ("unknown binary operator", "不明な二項演算子"),
    (
        "parameter is unset or null",
        "パラメータが未設定またはnullです",
    ),
    ("usage:", "使用方法:"),
    ("Running", "実行中"),
    ("Done", "完了"),
    (" is ", " は "),
];

/// 英語の診断句と簡体字中国語訳を対応付ける定数。
const ENGLISH_TO_CHINESE: &[(&str, &str)] = &[
    ("arguments must be valid UTF-8", "参数必须是有效的 UTF-8"),
    ("input must be valid UTF-8", "输入必须是有效的 UTF-8"),
    ("invalid byte at offset", "无效字节位置"),
    (" is an alias for ", " 是以下内容的别名: "),
    (" is an alias", " 是别名"),
    (" is a shell function", " 是 shell 函数"),
    (" is a function", " 是函数"),
    (" is a shell builtin", " 是 shell 内置命令"),
    ("unknown option", "未知选项"),
    ("requires a command string", "需要命令字符串"),
    ("command not found", "找不到命令"),
    ("unsupported builtin", "不支持的内置命令"),
    ("unsupported option", "不支持的选项"),
    ("unsupported argument", "不支持的参数"),
    ("no such option", "没有该选项"),
    ("no such widget", "没有该部件"),
    ("no such job", "没有该作业"),
    ("not in a function", "当前不在函数中"),
    ("not a shell builtin", "不是 shell 内置命令"),
    ("not a directory", "不是目录"),
    ("not found", "未找到"),
    ("too many arguments", "参数过多"),
    ("invalid or readonly variable", "变量无效或为只读"),
    ("readonly variable", "变量为只读"),
    ("invalid variable name", "变量名无效"),
    ("invalid array name", "数组名无效"),
    ("invalid shell option name", "shell 选项名无效"),
    ("invalid abbreviation", "缩写无效"),
    ("invalid rename", "重命名无效"),
    ("invalid input descriptor", "输入描述符无效"),
    (
        "invalid file descriptor duplication",
        "文件描述符复制指定无效",
    ),
    ("invalid indexed-array subscript", "索引数组下标无效"),
    ("bad input descriptor", "输入描述符错误"),
    ("bad file descriptor", "文件描述符错误"),
    ("invalid conditional expression", "条件表达式无效"),
    ("invalid arithmetic expression", "算术表达式无效"),
    ("invalid arithmetic base", "算术进制无效"),
    ("invalid arithmetic constant", "算术常量无效"),
    ("invalid assignment", "赋值无效"),
    ("invalid signal", "信号无效"),
    ("invalid mask", "掩码无效"),
    ("invalid option", "选项无效"),
    ("incomplete format specifier", "格式说明符不完整"),
    ("invalid name", "名称无效"),
    ("invalid pattern", "模式无效"),
    ("missing here-document body", "缺少 here-document 内容"),
    ("missing ]]", "缺少 ]]"),
    ("missing ]", "缺少 ]"),
    (
        "missing ':' in conditional expression",
        "条件表达式中缺少 ':'",
    ),
    (
        "missing ')' in arithmetic expression",
        "算术表达式中缺少 ')'",
    ),
    (
        "unclosed parameter expansion in here-document",
        "here-document 中的参数展开未闭合",
    ),
    (
        "unclosed substitution in here-document",
        "here-document 中的替换未闭合",
    ),
    ("ambiguous redirect", "重定向目标不明确"),
    ("division by zero", "除以零"),
    ("integer expression expected", "需要整数表达式"),
    ("expression required", "需要表达式"),
    ("expected arithmetic operand", "需要算术操作数"),
    (
        "expected zsh parameter name after $+",
        "$+ 后需要 zsh 参数名",
    ),
    (
        "count exceeds positional parameters",
        "数量超过位置参数个数",
    ),
    ("loop control used outside a loop", "在循环外使用了循环控制"),
    ("can only be used in a function", "只能在函数中使用"),
    ("directory stack empty", "目录栈为空"),
    ("no other directory", "没有其他目录"),
    ("background job panicked", "后台作业异常终止"),
    ("input is not valid UTF-8", "输入不是有效的 UTF-8"),
    (
        "command substitution produced non-UTF-8 output",
        "命令替换产生了非 UTF-8 输出",
    ),
    ("no matches found", "未找到匹配项"),
    ("filename required", "需要文件名"),
    ("variable name required", "需要变量名"),
    ("keymap name required", "需要键映射名称"),
    ("action and signal are required", "需要动作和信号"),
    ("requires a variable name", "需要变量名"),
    ("requires an array name", "需要数组名"),
    ("requires a keymap", "需要键映射"),
    ("requires a widget", "需要部件"),
    ("requires a name", "需要名称"),
    ("requires a command", "需要命令"),
    ("expected EVENT FUNCTION", "需要 EVENT FUNCTION"),
    (
        "expected PATTERN STYLE [VALUE ...]",
        "需要 PATTERN STYLE [VALUE ...]",
    ),
    (
        "array assignment requires a closing ')'",
        "数组赋值需要右括号 ')'",
    ),
    ("requires an argument", "需要参数"),
    ("requires a format", "需要格式"),
    ("unsupported hook", "不支持的钩子"),
    ("invalid job", "作业无效"),
    ("no such function", "没有该函数"),
    ("OLDPWD not set", "未设置 OLDPWD"),
    ("no help topic", "没有该帮助主题"),
    ("unknown unary operator", "未知一元运算符"),
    ("unknown binary operator", "未知二元运算符"),
    ("parameter is unset or null", "参数未设置或为空"),
    ("usage:", "用法:"),
    ("Running", "运行中"),
    ("Done", "已完成"),
    (" is ", " 是 "),
];

/// 環境変数から使用するメッセージ言語を決定する。
pub(crate) fn detected_language() -> MessageLanguage {
    #[cfg(test)]
    {
        MessageLanguage::English
    }
    #[cfg(not(test))]
    {
        LANGUAGE_ENVIRONMENTS
            .iter()
            .find_map(|name| {
                std::env::var(name)
                    .ok()
                    .filter(|value| !value.trim().is_empty())
            })
            .map_or(MessageLanguage::English, |value| language_from_tag(&value))
    }
}

/// 指定された言語タグを対応するメッセージ言語へ変換する。
fn language_from_tag(value: &str) -> MessageLanguage {
    let normalized = value.trim().to_ascii_lowercase().replace('_', "-");
    if normalized == "ja" || normalized.starts_with("ja-") || normalized == "japanese" {
        MessageLanguage::Japanese
    } else if normalized == "zh" || normalized.starts_with("zh-") || normalized == "chinese" {
        MessageLanguage::Chinese
    } else {
        MessageLanguage::English
    }
}

/// 現在の言語設定に合わせてisksh内部の診断メッセージを翻訳する。
#[doc(hidden)]
pub fn localize(message: impl AsRef<str>) -> String {
    localize_for(detected_language(), message.as_ref())
}

/// 指定された言語へisksh内部の診断メッセージを翻訳する。
fn localize_for(language: MessageLanguage, message: &str) -> String {
    match language {
        MessageLanguage::English => replace_all(message, JAPANESE_TO_ENGLISH),
        MessageLanguage::Japanese => replace_all(message, ENGLISH_TO_JAPANESE),
        MessageLanguage::Chinese => {
            let translated = replace_all(message, JAPANESE_TO_CHINESE);
            replace_all(&translated, ENGLISH_TO_CHINESE)
        }
    }
}

/// メッセージ中の既知の診断句を一覧の順番で置換する。
fn replace_all(message: &str, replacements: &[(&str, &str)]) -> String {
    replacements
        .iter()
        .fold(message.to_string(), |translated, (source, target)| {
            translated.replace(source, target)
        })
}

/// CLIヘルプを指定された言語で組み立てる。
#[doc(hidden)]
pub fn cli_help(version: &str) -> String {
    cli_help_for(detected_language(), version)
}

/// 指定された言語でCLIヘルプを組み立てる。
fn cli_help_for(language: MessageLanguage, version: &str) -> String {
    match language {
        MessageLanguage::English => format!(
            "isksh {version}\n\nUsage:\n  isksh [OPTION...] SCRIPT [ARG...]\n  isksh [OPTION...] -c COMMAND [NAME [ARG...]]\n  isksh [OPTION...] -s [ARG...]\n  isksh -i\n\nOptions:\n  -i            force interactive mode\n  -l, --login   start as a login shell\n"
        ),
        MessageLanguage::Japanese => format!(
            "isksh {version}\n\n使用方法:\n  isksh [オプション...] スクリプト [引数...]\n  isksh [オプション...] -c コマンド [名前 [引数...]]\n  isksh [オプション...] -s [引数...]\n  isksh -i\n\nオプション:\n  -i            対話モードを強制する\n  -l, --login   ログインシェルとして開始する\n"
        ),
        MessageLanguage::Chinese => format!(
            "isksh {version}\n\n用法:\n  isksh [选项...] 脚本 [参数...]\n  isksh [选项...] -c 命令 [名称 [参数...]]\n  isksh [选项...] -s [参数...]\n  isksh -i\n\n选项:\n  -i            强制交互模式\n  -l, --login   作为登录 shell 启动\n"
        ),
    }
}

/// 組み込みコマンドの説明を現在の言語で返す。
pub(crate) fn builtin_description(name: &str) -> String {
    builtin_description_for(detected_language(), name)
}

/// 指定された言語で組み込みコマンドの説明を返す。
fn builtin_description_for(language: MessageLanguage, name: &str) -> String {
    match language {
        MessageLanguage::English => format!("{name}: isksh shell builtin\n"),
        MessageLanguage::Japanese => format!("{name}: iskshのシェル組み込みコマンド\n"),
        MessageLanguage::Chinese => format!("{name}: isksh shell 内置命令\n"),
    }
}

/// `abbr`組み込みコマンドの使用方法を現在の言語で返す。
pub(crate) fn abbreviation_help() -> &'static str {
    abbreviation_help_for(detected_language())
}

/// 指定された言語で`abbr`組み込みコマンドの使用方法を返す。
fn abbreviation_help_for(language: MessageLanguage) -> &'static str {
    match language {
        MessageLanguage::English => "usage: abbr [-a|-e|-r|-q|-l|-s] [NAME [EXPANSION]]\n",
        MessageLanguage::Japanese => "使用方法: abbr [-a|-e|-r|-q|-l|-s] [名前 [展開文字列]]\n",
        MessageLanguage::Chinese => "用法: abbr [-a|-e|-r|-q|-l|-s] [名称 [展开字符串]]\n",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 言語タグの別名と地域指定を判定できることを確認する。
    #[test]
    fn recognizes_supported_language_tags() {
        assert_eq!(language_from_tag("en_US.UTF-8"), MessageLanguage::English);
        assert_eq!(language_from_tag("ja_JP.UTF-8"), MessageLanguage::Japanese);
        assert_eq!(language_from_tag("zh-CN"), MessageLanguage::Chinese);
        assert_eq!(language_from_tag("Chinese"), MessageLanguage::Chinese);
    }

    /// 三言語の固定文と可変部分を含む診断を翻訳できることを確認する。
    #[test]
    fn localizes_diagnostics_in_three_languages() {
        let message = "isksh: demo: command not found";
        assert_eq!(localize_for(MessageLanguage::English, message), message);
        assert_eq!(
            localize_for(MessageLanguage::Japanese, message),
            "isksh: demo: コマンドが見つかりません"
        );
        assert_eq!(
            localize_for(MessageLanguage::Chinese, message),
            "isksh: demo: 找不到命令"
        );
        assert_eq!(
            localize_for(
                MessageLanguage::Japanese,
                "isksh: printf: incomplete format specifier"
            ),
            "isksh: printf: 書式指定が不完全です"
        );
        assert_eq!(
            localize_for(
                MessageLanguage::Chinese,
                "isksh: printf: incomplete format specifier"
            ),
            "isksh: printf: 格式说明符不完整"
        );
        assert_eq!(
            localize_for(
                MessageLanguage::English,
                "二重引用符が閉じられていません (1:2)"
            ),
            "unclosed double quote (1:2)"
        );
    }

    /// 三言語のCLIヘルプと組み込みコマンド説明を生成できることを確認する。
    #[test]
    fn builds_help_and_descriptions_in_three_languages() {
        for (language, heading, description, abbreviation) in [
            (
                MessageLanguage::English,
                "Usage:",
                "shell builtin",
                "usage:",
            ),
            (
                MessageLanguage::Japanese,
                "使用方法:",
                "シェル組み込みコマンド",
                "使用方法:",
            ),
            (MessageLanguage::Chinese, "用法:", "shell 内置命令", "用法:"),
        ] {
            assert!(cli_help_for(language, "1.2.3").contains(heading));
            assert!(builtin_description_for(language, "printf").contains(description));
            assert!(abbreviation_help_for(language).contains(abbreviation));
        }
        assert!(cli_help("1.2.3").contains("isksh 1.2.3"));
        assert!(builtin_description("printf").contains("printf"));
        assert!(abbreviation_help().contains("abbr"));
        assert!(!localize("plain message").is_empty());
    }
}
