use crate::ast::*;
use crate::lexer::{Operator, Token, TokenKind, lex};
use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
#[error("{message} ({line}:{column})")]
pub struct ParseError {
    pub message: String,
    pub line: usize,
    pub column: usize,
    pub incomplete: bool,
}

/// シェルのソース文字列を構文木へ変換する。
///
/// # エラー
///
/// 字句解析または文法検証に失敗した場合は[`ParseError`]を返す。
pub fn parse(source: &str) -> Result<Script, ParseError> {
    let (source, here_documents) = extract_here_documents(source)?;
    let tokens = lex(&source).map_err(|error| ParseError {
        message: error.message,
        line: error.line,
        column: error.column,
        incomplete: error.incomplete,
    })?;
    let mut parser = Parser {
        tokens,
        index: 0,
        here_documents,
    };
    let script = parser.parse_script_until(&[], &[])?;
    debug_assert!(parser.peek().is_none());
    Ok(script)
}

struct Parser {
    tokens: Vec<Token>,
    index: usize,
    here_documents: HashMap<String, HereDocument>,
}

impl Parser {
    /// `parse_script_until`に対応する処理を行う。
    fn parse_script_until(
        &mut self,
        stop_words: &[&str],
        stop_operators: &[Operator],
    ) -> Result<Script, ParseError> {
        let mut lists = Vec::new();
        self.skip_separators();
        while self.peek().is_some()
            && !self.at_any_word(stop_words)
            && !self.at_any_operator(stop_operators)
        {
            let mut and_or = self.parse_and_or()?;
            if self.consume_operator(Operator::Background) {
                and_or.background = true;
            }
            let background = and_or.background;
            lists.push(and_or);
            if !background
                && !self.skip_separators()
                && self.peek().is_some()
                && !self.at_any_word(stop_words)
                && !self.at_any_operator(stop_operators)
            {
                return self.error("コマンドの区切りが必要です");
            }
            if background {
                self.skip_separators();
            }
        }
        Ok(Script { lists })
    }

    /// `parse_and_or`に対応する処理を行う。
    fn parse_and_or(&mut self) -> Result<AndOr, ParseError> {
        let first = self.parse_pipeline()?;
        let mut rest = Vec::new();
        loop {
            let op = if self.consume_operator(Operator::AndIf) {
                Some(AndOrOp::And)
            } else if self.consume_operator(Operator::OrIf) {
                Some(AndOrOp::Or)
            } else {
                None
            };
            let Some(op) = op else { break };
            self.skip_newlines();
            rest.push((op, self.parse_pipeline()?));
        }
        Ok(AndOr {
            first,
            rest,
            background: false,
        })
    }

    /// `parse_pipeline`に対応する処理を行う。
    fn parse_pipeline(&mut self) -> Result<Pipeline, ParseError> {
        let negated = self.consume_word("!");
        let mut commands = vec![self.parse_command()?];
        while self.consume_operator(Operator::Pipe) {
            self.skip_newlines();
            commands.push(self.parse_command()?);
        }
        Ok(Pipeline { negated, commands })
    }

    /// `parse_command`に対応する処理を行う。
    fn parse_command(&mut self) -> Result<Command, ParseError> {
        if self.at_word("if") {
            return self.parse_if();
        }
        if self.at_word("while") || self.at_word("until") {
            return self.parse_while();
        }
        if self.at_word("for") {
            return self.parse_for();
        }
        if self.at_word("case") {
            return self.parse_case();
        }
        if self.consume_operator(Operator::LeftBrace) {
            let body = self.parse_script_until(&[], &[Operator::RightBrace])?;
            self.expect_operator(Operator::RightBrace)?;
            return Ok(Command::Group {
                body,
                subshell: false,
            });
        }
        if self.consume_operator(Operator::LeftParen) {
            let body = self.parse_script_until(&[], &[Operator::RightParen])?;
            self.expect_operator(Operator::RightParen)?;
            return Ok(Command::Group {
                body,
                subshell: true,
            });
        }
        if let Some(name) = self.function_name() {
            self.index += 3;
            self.skip_newlines();
            let body = self.parse_command()?;
            return Ok(Command::Function {
                name,
                body: Box::new(body),
            });
        }
        self.parse_simple().map(Command::Simple)
    }

    /// `parse_if`に対応する処理を行う。
    fn parse_if(&mut self) -> Result<Command, ParseError> {
        self.expect_word("if")?;
        let condition = self.parse_script_until(&["then"], &[])?;
        if condition.lists.is_empty() {
            return self.error("ifの条件が必要です");
        }
        self.expect_word("then")?;
        let body = self.parse_script_until(&["elif", "else", "fi"], &[])?;
        let mut branches = vec![(condition, body)];
        while self.consume_word("elif") {
            let condition = self.parse_script_until(&["then"], &[])?;
            self.expect_word("then")?;
            let body = self.parse_script_until(&["elif", "else", "fi"], &[])?;
            branches.push((condition, body));
        }
        let else_body = if self.consume_word("else") {
            Some(self.parse_script_until(&["fi"], &[])?)
        } else {
            None
        };
        self.expect_word("fi")?;
        Ok(Command::If {
            branches,
            else_body,
        })
    }

    /// `parse_while`に対応する処理を行う。
    fn parse_while(&mut self) -> Result<Command, ParseError> {
        let until = self.consume_word("until");
        if !until {
            self.expect_word("while")?;
        }
        let condition = self.parse_script_until(&["do"], &[])?;
        if condition.lists.is_empty() {
            return self.error("ループ条件が必要です");
        }
        self.expect_word("do")?;
        let body = self.parse_script_until(&["done"], &[])?;
        self.expect_word("done")?;
        Ok(Command::While {
            condition,
            body,
            until,
        })
    }

    /// `parse_for`に対応する処理を行う。
    fn parse_for(&mut self) -> Result<Command, ParseError> {
        self.expect_word("for")?;
        let name = self
            .take_plain_word()
            .ok_or_else(|| self.current_error("for変数名が必要です"))?;
        if !valid_name(&name) {
            return self.error("無効なfor変数名です");
        }
        let mut words = Vec::new();
        if self.consume_word("in") {
            while let Some(Token {
                kind: TokenKind::Word(word),
                ..
            }) = self.peek()
            {
                if word.as_plain_literal() == Some("do") {
                    break;
                }
                words.push(word.clone());
                self.index += 1;
            }
        }
        self.skip_separators();
        self.expect_word("do")?;
        let body = self.parse_script_until(&["done"], &[])?;
        self.expect_word("done")?;
        Ok(Command::For { name, words, body })
    }

    /// `parse_case`に対応する処理を行う。
    fn parse_case(&mut self) -> Result<Command, ParseError> {
        self.expect_word("case")?;
        let word = self
            .take_word()
            .ok_or_else(|| self.current_error("caseのwordが必要です"))?;
        self.expect_word("in")?;
        self.skip_separators();
        let mut arms = Vec::new();
        while !self.at_word("esac") {
            self.consume_operator(Operator::LeftParen);
            let mut patterns = Vec::new();
            loop {
                patterns.push(
                    self.take_word()
                        .ok_or_else(|| self.current_error("caseパターンが必要です"))?,
                );
                if !self.consume_operator(Operator::Pipe) {
                    break;
                }
            }
            self.expect_operator(Operator::RightParen)?;
            let body = self.parse_script_until(&["esac"], &[Operator::CaseEnd])?;
            self.consume_operator(Operator::CaseEnd);
            self.skip_separators();
            arms.push(CaseArm { patterns, body });
            if self.peek().is_none() {
                return self.error("caseがesacで閉じられていません");
            }
        }
        self.expect_word("esac")?;
        Ok(Command::Case { word, arms })
    }

    /// `parse_simple`に対応する処理を行う。
    fn parse_simple(&mut self) -> Result<SimpleCommand, ParseError> {
        let mut command = SimpleCommand::default();
        loop {
            if let Some((consumed, fd, kind)) = self.peek_redirection() {
                self.index += consumed;
                let target = self
                    .take_word()
                    .ok_or_else(|| self.current_error("リダイレクト先が必要です"))?;
                let here_document = target
                    .as_plain_literal()
                    .and_then(|marker| self.here_documents.remove(marker));
                command.redirections.push(Redirection {
                    fd,
                    kind,
                    target,
                    here_document,
                });
                continue;
            }
            let Some(word) = self.take_word() else { break };
            if command.words.is_empty()
                && let Some((name, value)) = split_assignment(&word)
            {
                if value.parts.is_empty() && self.consume_operator(Operator::LeftParen) {
                    let mut values = Vec::new();
                    while !self.at_operator(Operator::RightParen) {
                        values.push(self.take_word().ok_or_else(|| {
                            self.current_error("array assignment requires a closing ')'")
                        })?);
                    }
                    self.expect_operator(Operator::RightParen)?;
                    command.array_assignments.push((name, values));
                } else {
                    command.assignments.push((name, value));
                }
            } else {
                command.words.push(word);
            }
        }
        if command.words.is_empty()
            && command.assignments.is_empty()
            && command.array_assignments.is_empty()
            && command.redirections.is_empty()
        {
            self.error("コマンドが必要です")
        } else {
            Ok(command)
        }
    }

    /// `peek_redirection`に対応する処理を行う。
    fn peek_redirection(&self) -> Option<(usize, Option<u32>, RedirectionKind)> {
        let (offset, fd) = match self.peek() {
            Some(Token {
                kind: TokenKind::Word(word),
                ..
            }) if word
                .as_plain_literal()
                .is_some_and(|v| v.chars().all(|ch| ch.is_ascii_digit())) =>
            {
                let value = word.as_plain_literal()?.parse().ok()?;
                (1, Some(value))
            }
            _ => (0, None),
        };
        let token = self.tokens.get(self.index + offset)?;
        let TokenKind::Operator(operator) = token.kind else {
            return None;
        };
        let kind = match operator {
            Operator::Input => RedirectionKind::Input,
            Operator::Output => RedirectionKind::Output,
            Operator::Append => RedirectionKind::Append,
            Operator::Clobber => RedirectionKind::Clobber,
            Operator::ReadWrite => RedirectionKind::ReadWrite,
            Operator::DuplicateInput => RedirectionKind::DuplicateInput,
            Operator::DuplicateOutput => RedirectionKind::DuplicateOutput,
            Operator::HereDocument => RedirectionKind::HereDocument,
            Operator::HereDocumentStrip => RedirectionKind::HereDocumentStrip,
            _ => return None,
        };
        Some((offset + 1, fd, kind))
    }

    /// `function_name`に対応する処理を行う。
    fn function_name(&self) -> Option<String> {
        let TokenKind::Word(word) = &self.tokens.get(self.index)?.kind else {
            return None;
        };
        let name = word.as_plain_literal()?;
        if !valid_function_name(name)
            || !matches!(
                self.tokens.get(self.index + 1)?.kind,
                TokenKind::Operator(Operator::LeftParen)
            )
            || !matches!(
                self.tokens.get(self.index + 2)?.kind,
                TokenKind::Operator(Operator::RightParen)
            )
        {
            return None;
        }
        Some(name.to_string())
    }

    /// `skip_separators`に対応する処理を行う。
    fn skip_separators(&mut self) -> bool {
        let start = self.index;
        while self.consume_operator(Operator::Newline) || self.consume_operator(Operator::Semicolon)
        {
        }
        self.index != start
    }

    /// `skip_newlines`に対応する処理を行う。
    fn skip_newlines(&mut self) {
        while self.consume_operator(Operator::Newline) {}
    }

    /// `take_word`に対応する処理を行う。
    fn take_word(&mut self) -> Option<Word> {
        let TokenKind::Word(word) = &self.peek()?.kind else {
            return None;
        };
        let word = word.clone();
        self.index += 1;
        Some(word)
    }

    /// `take_plain_word`に対応する処理を行う。
    fn take_plain_word(&mut self) -> Option<String> {
        let value = match &self.peek()?.kind {
            TokenKind::Word(word) => word.as_plain_literal()?.to_string(),
            _ => return None,
        };
        self.index += 1;
        Some(value)
    }

    /// `at_word`に対応する処理を行う。
    fn at_word(&self, expected: &str) -> bool {
        matches!(&self.peek().map(|token| &token.kind), Some(TokenKind::Word(word)) if word.as_plain_literal() == Some(expected))
    }

    /// `at_any_word`に対応する処理を行う。
    fn at_any_word(&self, expected: &[&str]) -> bool {
        expected.iter().any(|word| self.at_word(word))
    }

    /// `consume_word`に対応する処理を行う。
    fn consume_word(&mut self, expected: &str) -> bool {
        if self.at_word(expected) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    /// `expect_word`に対応する処理を行う。
    fn expect_word(&mut self, expected: &str) -> Result<(), ParseError> {
        self.skip_newlines();
        if self.consume_word(expected) {
            Ok(())
        } else {
            self.error(format!("'{expected}'が必要です"))
        }
    }

    /// `at_any_operator`に対応する処理を行う。
    fn at_any_operator(&self, expected: &[Operator]) -> bool {
        expected.iter().any(|operator| self.at_operator(*operator))
    }

    /// `at_operator`に対応する処理を行う。
    fn at_operator(&self, expected: Operator) -> bool {
        matches!(self.peek().map(|token| &token.kind), Some(TokenKind::Operator(operator)) if *operator == expected)
    }

    /// `consume_operator`に対応する処理を行う。
    fn consume_operator(&mut self, expected: Operator) -> bool {
        if self.at_operator(expected) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    /// `expect_operator`に対応する処理を行う。
    fn expect_operator(&mut self, expected: Operator) -> Result<(), ParseError> {
        if self.consume_operator(expected) {
            Ok(())
        } else {
            self.error(format!("演算子{expected:?}が必要です"))
        }
    }

    /// `peek`に対応する処理を行う。
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.index)
    }

    /// `current_error`に対応する処理を行う。
    fn current_error(&self, message: impl Into<String>) -> ParseError {
        let (line, column) = self
            .peek()
            .map_or((1, 1), |token| (token.line, token.column));
        ParseError {
            message: message.into(),
            line,
            column,
            incomplete: self.peek().is_none(),
        }
    }

    /// `error`に対応する処理を行う。
    fn error<T>(&self, message: impl Into<String>) -> Result<T, ParseError> {
        Err(self.current_error(message))
    }
}

/// `extract_here_documents`に対応する処理を行う。
fn extract_here_documents(
    source: &str,
) -> Result<(String, HashMap<String, HereDocument>), ParseError> {
    let lines: Vec<&str> = source.split_inclusive('\n').collect();
    let mut output = String::new();
    let mut documents = HashMap::new();
    let mut index = 0usize;
    let mut sequence = 0usize;
    while index < lines.len() {
        let (rewritten, specs) = rewrite_here_document_line(lines[index], &mut sequence);
        output.push_str(&rewritten);
        index += 1;
        for (marker, delimiter, expand, strip_tabs) in specs {
            let mut body = String::new();
            let mut found = false;
            while index < lines.len() {
                let line = lines[index];
                index += 1;
                let without_lf = line.strip_suffix('\n').unwrap_or(line);
                let without_newline = without_lf.strip_suffix('\r').unwrap_or(without_lf);
                let compared = if strip_tabs {
                    without_newline.trim_start_matches('\t')
                } else {
                    without_newline
                };
                if compared == delimiter {
                    found = true;
                    break;
                }
                if strip_tabs {
                    body.push_str(line.trim_start_matches('\t'));
                } else {
                    body.push_str(line);
                }
            }
            if !found {
                return Err(ParseError {
                    message: format!("here-documentが'{delimiter}'で閉じられていません"),
                    line: index.max(1),
                    column: 1,
                    incomplete: true,
                });
            }
            documents.insert(marker, HereDocument { body, expand });
        }
    }
    Ok((output, documents))
}

/// `rewrite_here_document_line`に対応する処理を行う。
fn rewrite_here_document_line(
    line: &str,
    sequence: &mut usize,
) -> (String, Vec<(String, String, bool, bool)>) {
    let chars: Vec<char> = line.chars().collect();
    let mut output = String::new();
    let mut specs = Vec::new();
    let mut index = 0usize;
    let mut quote = None;
    while index < chars.len() {
        let ch = chars[index];
        if let Some(active) = quote {
            output.push(ch);
            if ch == active {
                quote = None;
            }
            index += 1;
            continue;
        }
        if matches!(ch, '\'' | '"') {
            quote = Some(ch);
            output.push(ch);
            index += 1;
            continue;
        }
        if ch == '<' && chars.get(index + 1) == Some(&'<') {
            output.push_str("<<");
            index += 2;
            let strip_tabs = chars.get(index) == Some(&'-');
            if strip_tabs {
                output.push('-');
                index += 1;
            }
            while chars.get(index).is_some_and(|ch| matches!(ch, ' ' | '\t')) {
                output.push(chars[index]);
                index += 1;
            }
            let start = index;
            let mut delimiter_quote = None;
            while index < chars.len() {
                let current = chars[index];
                if let Some(active) = delimiter_quote {
                    if current == active {
                        delimiter_quote = None;
                    }
                    index += 1;
                } else if matches!(current, '\'' | '"') {
                    delimiter_quote = Some(current);
                    index += 1;
                } else if matches!(current, ' ' | '\t' | '\r' | '\n' | ';' | '&' | '|') {
                    break;
                } else {
                    index += 1;
                }
            }
            let raw: String = chars[start..index].iter().collect();
            if raw.is_empty() {
                continue;
            }
            let expand = !raw.contains(['\'', '"', '\\']);
            let delimiter = raw.replace(['\'', '"', '\\'], "");
            let marker = format!("__ISKSH_HEREDOC_{}__", *sequence);
            *sequence += 1;
            output.push_str(&marker);
            specs.push((marker, delimiter, expand, strip_tabs));
            continue;
        }
        output.push(ch);
        index += 1;
    }
    (output, specs)
}

/// `valid_name`に対応する処理を行う。
fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(ch) if ch.is_ascii_alphabetic() || ch == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// `valid_function_name`に対応する処理を行う。
fn valid_function_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(ch) if ch.is_ascii_alphabetic() || ch == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
}

/// `split_assignment`に対応する処理を行う。
fn split_assignment(word: &Word) -> Option<(String, Word)> {
    let WordPart::Literal {
        value,
        quoted: false,
    } = word.parts.first()?
    else {
        return None;
    };
    let equals = value.find('=')?;
    let name = &value[..equals];
    if !valid_name(name) && parse_array_reference(name).is_none() {
        return None;
    }
    let mut parts = word.parts.clone();
    let remainder = value[equals + 1..].to_string();
    if remainder.is_empty() {
        parts.remove(0);
    } else {
        parts[0] = WordPart::Literal {
            value: remainder,
            quoted: false,
        };
    }
    Some((name.to_string(), Word { parts }))
}

/// `parse_array_reference`に対応する処理を行う。
fn parse_array_reference(value: &str) -> Option<(&str, &str)> {
    let (name, subscript) = value.split_once('[')?;
    let subscript = subscript.strip_suffix(']')?;
    (valid_name(name) && !subscript.is_empty()).then_some((name, subscript))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// `parses_pipeline_and_conditionals`に対応する処理を行う。
    fn parses_pipeline_and_conditionals() {
        let script = parse("value=x; if true; then echo $value | cat; else false; fi").unwrap();
        assert_eq!(script.lists.len(), 2);
        assert!(matches!(
            &script.lists[1].first.commands[0],
            Command::If { .. }
        ));
    }

    #[test]
    /// `parses_loops_and_function`に対応する処理を行う。
    fn parses_loops_and_function() {
        let script =
            parse("show() { printf '%s' \"$1\"; }; for x in a b; do show $x; done").unwrap();
        assert_eq!(script.lists.len(), 2);
    }

    #[test]
    /// `rejects_unclosed_if`に対応する処理を行う。
    fn rejects_unclosed_if() {
        let error = parse("if true; then echo nope").unwrap_err();
        assert!(error.incomplete);
    }

    #[test]
    /// `parses_every_compound_command_and_redirection`に対応する処理を行う。
    fn parses_every_compound_command_and_redirection() {
        let source = concat!(
            "until false; do break; done\n",
            "case x in (x|y) echo yes;; *) echo no;; esac\n",
            "(echo sub)\n",
            "{ echo group; }\n",
            "! echo x 0<input 1>output 2>>errors 3<>rw 4<&0 5>&1 6>|clobber\n",
        );
        let script = parse(source).unwrap();
        assert_eq!(script.lists.len(), 5);
        assert!(matches!(
            &script.lists[4].first.commands[0],
            Command::Simple(simple) if simple.redirections.len() == 7
        ));
        assert!(script.lists[4].first.negated);
    }

    #[test]
    /// `parses_heredoc_variants_and_crlf`に対応する処理を行う。
    fn parses_heredoc_variants_and_crlf() {
        let script = parse("cat <<-EOF\r\n\tvalue\r\n\tEOF\r\ncat <<'Q'\n$x\nQ\n").unwrap();
        assert!(matches!(
            &script.lists[0].first.commands[0],
            Command::Simple(first)
                if first.redirections[0].here_document.as_ref().unwrap().body == "value\r\n"
        ));
        assert!(!matches!(
            &script.lists[1].first.commands[0],
            Command::Simple(simple) if simple.redirections[0].here_document.as_ref().unwrap().expand
        ));
    }

    #[test]
    /// `rejects_invalid_syntax_forms`に対応する処理を行う。
    fn rejects_invalid_syntax_forms() {
        for source in [
            ")",
            "for 1; do :; done",
            "for ; do :; done",
            "for x in a | do :; done",
            "case x in x echo;; esac",
            "echo >",
            "if; then :; fi",
            "f()\n",
        ] {
            assert!(parse(source).is_err(), "unexpectedly accepted {source:?}");
        }
        let error = parse("cat <<EOF\nmissing\n").unwrap_err();
        assert!(error.incomplete);
        assert!(parse("array=(one").unwrap_err().incomplete);
    }

    #[test]
    /// `covers_elif_empty_loop_and_parser_helpers`に対応する処理を行う。
    fn covers_elif_empty_loop_and_parser_helpers() {
        let script = parse("if false; then :; elif true; then :; else false; fi").unwrap();
        assert!(matches!(
            script.lists[0].first.commands[0],
            Command::If { .. }
        ));
        assert!(parse("while; do :; done").is_err());
        assert!(parse("case x in x) :;;").is_err());
        assert!(parse("case").is_err());
        assert!(parse("case x in )").is_err());
        assert!(parse("name( echo bad").is_err());
        assert!(parse("echo x )").is_err());
        assert!(parse("for x in a do :; done").is_ok());

        let mut sequence = 0;
        let (rewritten, specs) = rewrite_here_document_line("cat <<   EOF\n", &mut sequence);
        assert!(rewritten.contains("__ISKSH_HEREDOC_0__"));
        assert_eq!(specs.len(), 1);
        let (unchanged, specs) = rewrite_here_document_line("cat << \n", &mut sequence);
        assert_eq!(unchanged, "cat << \n");
        assert!(specs.is_empty());

        let quoted = Word {
            parts: vec![WordPart::Literal {
                value: "A=x".into(),
                quoted: true,
            }],
        };
        assert!(split_assignment(&quoted).is_none());
        let invalid = Word {
            parts: vec![WordPart::Literal {
                value: "1A=x".into(),
                quoted: false,
            }],
        };
        assert!(split_assignment(&invalid).is_none());
    }
}
