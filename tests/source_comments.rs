use std::fs;
use std::path::Path;

/// Rustソースを再帰的に列挙する。
fn rust_sources(directory: &Path, output: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_sources(&path, output);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            output.push(path);
        }
    }
}

/// 宣言行から関数名または定数名を取り出す。
fn declaration_name(line: &str) -> Option<&str> {
    let mut line = line.trim_start();
    if line.starts_with("//") || line.starts_with('#') {
        return None;
    }
    loop {
        if let Some(rest) = line.strip_prefix("pub(") {
            line = rest.split_once(')')?.1.trim_start();
        } else if let Some(rest) = line.strip_prefix("pub ") {
            line = rest.trim_start();
        } else if let Some(rest) = line.strip_prefix("async ") {
            line = rest.trim_start();
        } else if let Some(rest) = line.strip_prefix("const ") {
            if rest.starts_with("fn ") {
                line = rest;
            } else {
                return rest
                    .trim_start_matches("mut ")
                    .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
                    .next();
            }
        } else if let Some(rest) = line.strip_prefix("unsafe ") {
            line = rest.trim_start();
        } else if let Some(rest) = line.strip_prefix("extern ") {
            line = rest.split_once(' ')?.1.trim_start();
        } else {
            break;
        }
    }
    if let Some(rest) = line.strip_prefix("fn ") {
        return rest
            .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
            .next();
    }
    for keyword in ["const ", "static "] {
        if let Some(rest) = line.strip_prefix(keyword) {
            return rest
                .trim_start_matches("mut ")
                .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
                .next();
        }
    }
    None
}

/// 指定位置の宣言に日本語コメントが付いているか確認する。
fn has_japanese_comment(lines: &[&str], index: usize) -> bool {
    lines[..index]
        .iter()
        .rev()
        .take_while(|line| {
            let line = line.trim();
            line.is_empty() || line.starts_with("//") || line.starts_with("#[")
        })
        .take_while(|line| !line.trim().is_empty())
        .any(|line| {
            line.chars().any(|character| {
                ('\u{3040}'..='\u{30ff}').contains(&character)
                    || ('\u{3400}'..='\u{9fff}').contains(&character)
            })
        })
}

/// 文字列に日本語の文字が含まれるか判定する。
fn contains_japanese(text: &str) -> bool {
    text.chars().any(|character| {
        ('\u{3040}'..='\u{30ff}').contains(&character)
            || ('\u{3400}'..='\u{9fff}').contains(&character)
    })
}

/// すべての関数と定数に日本語コメントがあることを確認する。
#[test]
fn every_function_and_constant_has_a_japanese_comment() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    rust_sources(&root.join("src"), &mut sources);
    rust_sources(&root.join("tests"), &mut sources);
    let mut missing = Vec::new();
    for path in sources {
        let source = fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = source.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            if let Some(name) = declaration_name(line)
                && !has_japanese_comment(&lines, index)
            {
                missing.push(format!("{}:{}: {name}", path.display(), index + 1));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "日本語コメントがありません:\n{}",
        missing.join("\n")
    );
}

/// 英字を含むソースコードコメントが日本語で説明されていることを確認する。
#[test]
fn every_source_comment_is_written_in_japanese() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    rust_sources(&root.join("src"), &mut sources);
    rust_sources(&root.join("tests"), &mut sources);
    let mut violations = Vec::new();
    for path in sources {
        let source = fs::read_to_string(&path).unwrap();
        for (index, line) in source.lines().enumerate() {
            let line = line.trim_start();
            if line.starts_with("//")
                && line
                    .chars()
                    .any(|character| character.is_ascii_alphabetic())
                && !contains_japanese(line)
            {
                violations.push(format!("{}:{}: {line}", path.display(), index + 1));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "日本語でないコメントがあります:\n{}",
        violations.join("\n")
    );
}
