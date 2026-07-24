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
}

pub fn parse(source: &str) -> Result<Script, ParseError> {
    let (source, here_documents) = extract_here_documents(source)?;
    let tokens = lex(&source).map_err(|error| ParseError {
        message: error.message,
        line: error.line,
        column: error.column,
    })?;
    let mut parser = Parser {
        tokens,
        index: 0,
        here_documents,
    };
    let script = parser.parse_script_until(&[], &[])?;
    if parser.peek().is_some() {
        return parser.error("予期しないトークンです");
    }
    Ok(script)
}

struct Parser {
    tokens: Vec<Token>,
    index: usize,
    here_documents: HashMap<String, HereDocument>,
}

impl Parser {
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
            lists.push(and_or);
            if !self.skip_separators()
                && self.peek().is_some()
                && !self.at_any_word(stop_words)
                && !self.at_any_operator(stop_operators)
            {
                return self.error("コマンドの区切りが必要です");
            }
        }
        Ok(Script { lists })
    }

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

    fn parse_pipeline(&mut self) -> Result<Pipeline, ParseError> {
        let negated = self.consume_word("!");
        let mut commands = vec![self.parse_command()?];
        while self.consume_operator(Operator::Pipe) {
            self.skip_newlines();
            commands.push(self.parse_command()?);
        }
        Ok(Pipeline { negated, commands })
    }

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

    fn parse_if(&mut self) -> Result<Command, ParseError> {
        self.expect_word("if")?;
        let condition = self.parse_script_until(&["then"], &[])?;
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

    fn parse_while(&mut self) -> Result<Command, ParseError> {
        let until = self.consume_word("until");
        if !until {
            self.expect_word("while")?;
        }
        let condition = self.parse_script_until(&["do"], &[])?;
        self.expect_word("do")?;
        let body = self.parse_script_until(&["done"], &[])?;
        self.expect_word("done")?;
        Ok(Command::While {
            condition,
            body,
            until,
        })
    }

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
                command.assignments.push((name, value));
            } else {
                command.words.push(word);
            }
        }
        if command.words.is_empty()
            && command.assignments.is_empty()
            && command.redirections.is_empty()
        {
            self.error("コマンドが必要です")
        } else {
            Ok(command)
        }
    }

    fn peek_redirection(&self) -> Option<(usize, Option<u8>, RedirectionKind)> {
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

    fn function_name(&self) -> Option<String> {
        let TokenKind::Word(word) = &self.tokens.get(self.index)?.kind else {
            return None;
        };
        let name = word.as_plain_literal()?;
        if !valid_name(name)
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

    fn skip_separators(&mut self) -> bool {
        let start = self.index;
        while self.consume_operator(Operator::Newline) || self.consume_operator(Operator::Semicolon)
        {
        }
        self.index != start
    }

    fn skip_newlines(&mut self) {
        while self.consume_operator(Operator::Newline) {}
    }

    fn take_word(&mut self) -> Option<Word> {
        let TokenKind::Word(word) = &self.peek()?.kind else {
            return None;
        };
        let word = word.clone();
        self.index += 1;
        Some(word)
    }

    fn take_plain_word(&mut self) -> Option<String> {
        let value = match &self.peek()?.kind {
            TokenKind::Word(word) => word.as_plain_literal()?.to_string(),
            _ => return None,
        };
        self.index += 1;
        Some(value)
    }

    fn at_word(&self, expected: &str) -> bool {
        matches!(&self.peek().map(|token| &token.kind), Some(TokenKind::Word(word)) if word.as_plain_literal() == Some(expected))
    }

    fn at_any_word(&self, expected: &[&str]) -> bool {
        expected.iter().any(|word| self.at_word(word))
    }

    fn consume_word(&mut self, expected: &str) -> bool {
        if self.at_word(expected) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn expect_word(&mut self, expected: &str) -> Result<(), ParseError> {
        self.skip_newlines();
        if self.consume_word(expected) {
            Ok(())
        } else {
            self.error(format!("'{expected}'が必要です"))
        }
    }

    fn at_any_operator(&self, expected: &[Operator]) -> bool {
        expected.iter().any(|operator| self.at_operator(*operator))
    }

    fn at_operator(&self, expected: Operator) -> bool {
        matches!(self.peek().map(|token| &token.kind), Some(TokenKind::Operator(operator)) if *operator == expected)
    }

    fn consume_operator(&mut self, expected: Operator) -> bool {
        if self.at_operator(expected) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn expect_operator(&mut self, expected: Operator) -> Result<(), ParseError> {
        if self.consume_operator(expected) {
            Ok(())
        } else {
            self.error(format!("演算子{expected:?}が必要です"))
        }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.index)
    }

    fn current_error(&self, message: impl Into<String>) -> ParseError {
        let (line, column) = self
            .peek()
            .map_or((1, 1), |token| (token.line, token.column));
        ParseError {
            message: message.into(),
            line,
            column,
        }
    }

    fn error<T>(&self, message: impl Into<String>) -> Result<T, ParseError> {
        Err(self.current_error(message))
    }
}

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
                });
            }
            documents.insert(marker, HereDocument { body, expand });
        }
    }
    Ok((output, documents))
}

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

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(ch) if ch.is_ascii_alphabetic() || ch == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

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
    if !valid_name(name) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pipeline_and_conditionals() {
        let script = parse("value=x; if true; then echo $value | cat; else false; fi").unwrap();
        assert_eq!(script.lists.len(), 2);
        let Command::If { branches, .. } = &script.lists[1].first.commands[0] else {
            panic!("expected if")
        };
        assert_eq!(branches[0].1.lists[0].first.commands.len(), 2);
    }

    #[test]
    fn parses_loops_and_function() {
        let script =
            parse("show() { printf '%s' \"$1\"; }; for x in a b; do show $x; done").unwrap();
        assert_eq!(script.lists.len(), 2);
    }

    #[test]
    fn rejects_unclosed_if() {
        assert!(parse("if true; then echo nope").is_err());
    }
}
