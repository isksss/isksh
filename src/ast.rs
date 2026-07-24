#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    pub lists: Vec<AndOr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AndOr {
    pub first: Pipeline,
    pub rest: Vec<(AndOrOp, Pipeline)>,
    pub background: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AndOrOp {
    And,
    Or,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pipeline {
    pub negated: bool,
    pub commands: Vec<Command>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Simple(SimpleCommand),
    If {
        branches: Vec<(Script, Script)>,
        else_body: Option<Script>,
    },
    While {
        condition: Script,
        body: Script,
        until: bool,
    },
    For {
        name: String,
        words: Vec<Word>,
        body: Script,
    },
    Case {
        word: Word,
        arms: Vec<CaseArm>,
    },
    Group {
        body: Script,
        subshell: bool,
    },
    Function {
        name: String,
        body: Box<Command>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseArm {
    pub patterns: Vec<Word>,
    pub body: Script,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SimpleCommand {
    pub assignments: Vec<(String, Word)>,
    pub array_assignments: Vec<(String, Vec<Word>)>,
    pub words: Vec<Word>,
    pub redirections: Vec<Redirection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redirection {
    pub fd: Option<u32>,
    pub kind: RedirectionKind,
    pub target: Word,
    pub here_document: Option<HereDocument>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HereDocument {
    pub body: String,
    pub expand: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectionKind {
    Input,
    Output,
    Append,
    Clobber,
    ReadWrite,
    DuplicateInput,
    DuplicateOutput,
    HereDocument,
    HereDocumentStrip,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Word {
    pub parts: Vec<WordPart>,
}

impl Word {
    pub fn as_plain_literal(&self) -> Option<&str> {
        match self.parts.as_slice() {
            [
                WordPart::Literal {
                    value,
                    quoted: false,
                },
            ] => Some(value),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WordPart {
    Literal { value: String, quoted: bool },
    Parameter { expression: String, quoted: bool },
    CommandSubstitution { source: String, quoted: bool },
    Arithmetic { expression: String, quoted: bool },
    ProcessSubstitution { source: String, input: bool },
}
