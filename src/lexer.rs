use crate::ast::{Word, WordPart};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Newline,
    Semicolon,
    Background,
    AndIf,
    OrIf,
    Pipe,
    LeftParen,
    RightParen,
    LeftBrace,
    RightBrace,
    Input,
    Output,
    Append,
    HereDocument,
    HereDocumentStrip,
    DuplicateInput,
    DuplicateOutput,
    ReadWrite,
    Clobber,
    CaseEnd,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    Word(Word),
    Operator(Operator),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("{message} ({line}:{column})")]
pub struct LexError {
    pub message: String,
    pub line: usize,
    pub column: usize,
}

pub fn lex(source: &str) -> Result<Vec<Token>, LexError> {
    Lexer::new(source).lex_all()
}

struct Lexer<'a> {
    chars: Vec<char>,
    index: usize,
    line: usize,
    column: usize,
    _source: &'a str,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            chars: source.chars().collect(),
            index: 0,
            line: 1,
            column: 1,
            _source: source,
        }
    }

    fn lex_all(mut self) -> Result<Vec<Token>, LexError> {
        let mut tokens = Vec::new();
        while let Some(ch) = self.peek() {
            if matches!(ch, ' ' | '\t' | '\r') {
                self.bump();
                continue;
            }
            if ch == '#' {
                while !matches!(self.peek(), None | Some('\n')) {
                    self.bump();
                }
                continue;
            }
            let line = self.line;
            let column = self.column;
            if let Some(op) = self.operator() {
                tokens.push(Token {
                    kind: TokenKind::Operator(op),
                    line,
                    column,
                });
            } else {
                let word = self.word()?;
                tokens.push(Token {
                    kind: TokenKind::Word(word),
                    line,
                    column,
                });
            }
        }
        Ok(tokens)
    }

    fn operator(&mut self) -> Option<Operator> {
        let pairs = [
            ("<<-", Operator::HereDocumentStrip),
            ("&&", Operator::AndIf),
            ("||", Operator::OrIf),
            (">>", Operator::Append),
            ("<<", Operator::HereDocument),
            ("<&", Operator::DuplicateInput),
            (">&", Operator::DuplicateOutput),
            ("<>", Operator::ReadWrite),
            (">|", Operator::Clobber),
            (";;", Operator::CaseEnd),
        ];
        for (text, op) in pairs {
            if self.starts_with(text) {
                for _ in text.chars() {
                    self.bump();
                }
                return Some(op);
            }
        }
        let op = match self.peek()? {
            '\n' => Operator::Newline,
            ';' => Operator::Semicolon,
            '&' => Operator::Background,
            '|' => Operator::Pipe,
            '(' => Operator::LeftParen,
            ')' => Operator::RightParen,
            '{' => Operator::LeftBrace,
            '}' => Operator::RightBrace,
            '<' => Operator::Input,
            '>' => Operator::Output,
            _ => return None,
        };
        self.bump();
        Some(op)
    }

    fn word(&mut self) -> Result<Word, LexError> {
        let mut parts = Vec::new();
        let mut literal = String::new();
        while let Some(ch) = self.peek() {
            if matches!(
                ch,
                ' ' | '\t' | '\r' | '\n' | ';' | '&' | '|' | '(' | ')' | '{' | '}' | '<' | '>'
            ) {
                break;
            }
            match ch {
                '\\' => {
                    self.bump();
                    if self.peek() == Some('\n') {
                        self.bump();
                    } else if let Some(escaped) = self.bump() {
                        literal.push(escaped);
                    } else {
                        return self.error("末尾のバックスラッシュに文字がありません");
                    }
                }
                '\'' => {
                    self.flush_literal(&mut parts, &mut literal, false);
                    self.bump();
                    let value = self.collect_until('\'')?;
                    parts.push(WordPart::Literal {
                        value,
                        quoted: true,
                    });
                }
                '"' => {
                    self.flush_literal(&mut parts, &mut literal, false);
                    self.bump();
                    self.double_quoted(&mut parts)?;
                }
                '$' => {
                    self.flush_literal(&mut parts, &mut literal, false);
                    self.dollar(&mut parts, false)?;
                }
                '`' => {
                    self.flush_literal(&mut parts, &mut literal, false);
                    parts.push(WordPart::CommandSubstitution {
                        source: self.backticks()?,
                        quoted: false,
                    });
                }
                _ => {
                    literal.push(ch);
                    self.bump();
                }
            }
        }
        self.flush_literal(&mut parts, &mut literal, false);
        if parts.is_empty() {
            self.error("空のwordです")
        } else {
            Ok(Word { parts })
        }
    }

    fn double_quoted(&mut self, parts: &mut Vec<WordPart>) -> Result<(), LexError> {
        let mut literal = String::new();
        loop {
            match self.peek() {
                None => return self.error("二重引用符が閉じられていません"),
                Some('"') => {
                    self.bump();
                    self.flush_literal(parts, &mut literal, true);
                    if parts.is_empty() {
                        parts.push(WordPart::Literal {
                            value: String::new(),
                            quoted: true,
                        });
                    }
                    return Ok(());
                }
                Some('\\') => {
                    self.bump();
                    match self.peek() {
                        Some('\n') => {
                            self.bump();
                        }
                        Some(next @ ('$' | '`' | '"' | '\\')) => {
                            literal.push(next);
                            self.bump();
                        }
                        Some(_) => literal.push('\\'),
                        None => return self.error("二重引用符が閉じられていません"),
                    }
                }
                Some('$') => {
                    self.flush_literal(parts, &mut literal, true);
                    self.dollar(parts, true)?;
                }
                Some('`') => {
                    self.flush_literal(parts, &mut literal, true);
                    parts.push(WordPart::CommandSubstitution {
                        source: self.backticks()?,
                        quoted: true,
                    });
                }
                Some(ch) => {
                    literal.push(ch);
                    self.bump();
                }
            }
        }
    }

    fn dollar(&mut self, parts: &mut Vec<WordPart>, quoted: bool) -> Result<(), LexError> {
        self.bump();
        if self.starts_with("((") {
            self.bump();
            self.bump();
            let expression = self.collect_balanced_arithmetic()?;
            parts.push(WordPart::Arithmetic { expression, quoted });
            return Ok(());
        }
        if self.peek() == Some('(') {
            self.bump();
            let source = self.collect_balanced('(', ')')?;
            parts.push(WordPart::CommandSubstitution { source, quoted });
            return Ok(());
        }
        if self.peek() == Some('{') {
            self.bump();
            let expression = self.collect_balanced('{', '}')?;
            parts.push(WordPart::Parameter { expression, quoted });
            return Ok(());
        }
        let mut expression = String::new();
        match self.peek() {
            Some(ch) if ch.is_ascii_alphabetic() || ch == '_' => {
                while let Some(ch) = self.peek() {
                    if ch.is_ascii_alphanumeric() || ch == '_' {
                        expression.push(ch);
                        self.bump();
                    } else {
                        break;
                    }
                }
            }
            Some(ch @ ('?' | '#' | '$' | '!' | '-' | '@' | '*' | '0'..='9')) => {
                expression.push(ch);
                self.bump();
            }
            _ => {
                parts.push(WordPart::Literal {
                    value: "$".into(),
                    quoted,
                });
                return Ok(());
            }
        }
        parts.push(WordPart::Parameter { expression, quoted });
        Ok(())
    }

    fn collect_balanced(&mut self, open: char, close: char) -> Result<String, LexError> {
        let mut value = String::new();
        let mut depth = 1usize;
        let mut quote = None;
        while let Some(ch) = self.bump() {
            if let Some(active) = quote {
                if ch == active {
                    quote = None;
                }
                value.push(ch);
                continue;
            }
            if matches!(ch, '\'' | '"') {
                quote = Some(ch);
                value.push(ch);
            } else if ch == open {
                depth += 1;
                value.push(ch);
            } else if ch == close {
                depth -= 1;
                if depth == 0 {
                    return Ok(value);
                }
                value.push(ch);
            } else {
                value.push(ch);
            }
        }
        self.error("置換式が閉じられていません")
    }

    fn collect_balanced_arithmetic(&mut self) -> Result<String, LexError> {
        let mut value = String::new();
        let mut depth = 1usize;
        while let Some(ch) = self.bump() {
            if ch == '(' {
                depth += 1;
                value.push(ch);
            } else if ch == ')' {
                if self.peek() == Some(')') && depth == 1 {
                    self.bump();
                    return Ok(value);
                }
                depth = depth.saturating_sub(1);
                value.push(ch);
            } else {
                value.push(ch);
            }
        }
        self.error("算術展開が閉じられていません")
    }

    fn backticks(&mut self) -> Result<String, LexError> {
        self.bump();
        let mut value = String::new();
        while let Some(ch) = self.bump() {
            if ch == '`' {
                return Ok(value);
            }
            if ch == '\\' {
                if let Some(next) = self.bump() {
                    value.push(next);
                }
            } else {
                value.push(ch);
            }
        }
        self.error("バッククォートが閉じられていません")
    }

    fn collect_until(&mut self, end: char) -> Result<String, LexError> {
        let mut value = String::new();
        while let Some(ch) = self.bump() {
            if ch == end {
                return Ok(value);
            }
            value.push(ch);
        }
        self.error("引用符が閉じられていません")
    }

    fn flush_literal(&self, parts: &mut Vec<WordPart>, value: &mut String, quoted: bool) {
        if !value.is_empty() {
            parts.push(WordPart::Literal {
                value: std::mem::take(value),
                quoted,
            });
        }
    }

    fn starts_with(&self, text: &str) -> bool {
        self.chars[self.index..]
            .iter()
            .copied()
            .zip(text.chars())
            .all(|(left, right)| left == right)
            && self.chars.len().saturating_sub(self.index) >= text.chars().count()
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.index).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.index += 1;
        if ch == '\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        Some(ch)
    }

    fn error<T>(&self, message: impl Into<String>) -> Result<T, LexError> {
        Err(LexError {
            message: message.into(),
            line: self.line,
            column: self.column,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_quote_context_and_expansions() {
        let tokens = lex("echo 'a b' \"$name\" ${value:-x} $(printf y)").unwrap();
        assert_eq!(tokens.len(), 5);
        let TokenKind::Word(word) = &tokens[2].kind else {
            panic!("expected word")
        };
        assert!(matches!(
            word.parts.as_slice(),
            [WordPart::Parameter { quoted: true, .. }]
        ));
    }

    #[test]
    fn reports_unclosed_quote_with_position() {
        let error = lex("echo 'oops").unwrap_err();
        assert_eq!(error.line, 1);
    }
}
