use std::collections::BTreeMap;

use crate::{
    configure::BasicLanguageMode as LanguageMode,
    error::RuntimeError,
    graphics::GraphicsProfile,
    tokenized_basic::{TokenizedBasicLine, TokenizedBasicProgram, decode_line_reference},
};

const TOKEN_AND: u8 = 0x80;
const TOKEN_DIV: u8 = 0x81;
const TOKEN_MOD: u8 = 0x83;
const TOKEN_OR: u8 = 0x84;
const TOKEN_LINE: u8 = 0x86;
const TOKEN_SPC: u8 = 0x89;
const TOKEN_TAB: u8 = 0x8A;
const TOKEN_THEN: u8 = 0x8C;
const TOKEN_ELSE: u8 = 0x8B;
const TOKEN_STEP: u8 = 0x88;
const TOKEN_PTR: u8 = 0x8F;
const TOKEN_PAGE: u8 = 0x90;
const TOKEN_TIME: u8 = 0x91;
const TOKEN_LOMEM: u8 = 0x92;
const TOKEN_HIMEM: u8 = 0x93;
const TOKEN_ABS: u8 = 0x94;
const TOKEN_ASC: u8 = 0x97;
const TOKEN_COS: u8 = 0x9B;
const TOKEN_FN: u8 = 0xA4;
const TOKEN_INKEY: u8 = 0xA6;
const TOKEN_INSTR: u8 = 0xA7;
const TOKEN_INT: u8 = 0xA8;
const TOKEN_LEN: u8 = 0xA9;
const TOKEN_LN: u8 = 0xAA;
const TOKEN_LOG: u8 = 0xAB;
const TOKEN_NOT: u8 = 0xAC;
const TOKEN_SQR: u8 = 0xB6;
const TOKEN_TAN: u8 = 0xB7;
const TOKEN_SIN: u8 = 0xB5;
const TOKEN_RND: u8 = 0xB3;
const TOKEN_TO: u8 = 0xB8;
const TOKEN_VAL: u8 = 0xBC;
const TOKEN_CHR: u8 = 0xBD;
const TOKEN_CLS: u8 = 0xDB;
const TOKEN_CLG: u8 = 0xDA;
const TOKEN_COLOUR: u8 = 0xFB;
const TOKEN_LEFT: u8 = 0xC0;
const TOKEN_MID: u8 = 0xC1;
const TOKEN_RIGHT: u8 = 0xC2;
const TOKEN_STR: u8 = 0xC3;
const TOKEN_STRING: u8 = 0xC4;
const TOKEN_CALL: u8 = 0xD6;
const TOKEN_DRAW: u8 = 0xDF;
const TOKEN_DATA: u8 = 0xDC;
const TOKEN_DEF: u8 = 0xDD;
const TOKEN_END: u8 = 0xE0;
const TOKEN_ENDPROC: u8 = 0xE1;
const TOKEN_ENDIF: u8 = 0xCD;
const TOKEN_DIM: u8 = 0xE2;
const TOKEN_FOR: u8 = 0xE3;
const TOKEN_GOSUB: u8 = 0xE4;
const TOKEN_GOTO: u8 = 0xE5;
const TOKEN_IF: u8 = 0xE7;
const TOKEN_INPUT: u8 = 0xE8;
const TOKEN_GCOL: u8 = 0xE6;
const TOKEN_MOVE: u8 = 0xEC;
const TOKEN_MODE: u8 = 0xEB;
const TOKEN_NEXT: u8 = 0xED;
const TOKEN_PRINT: u8 = 0xF1;
const TOKEN_PROC: u8 = 0xF2;
const TOKEN_PLOT: u8 = 0xF0;
const TOKEN_READ: u8 = 0xF3;
const TOKEN_REM: u8 = 0xF4;
const TOKEN_REPEAT: u8 = 0xF5;
const TOKEN_RESTORE: u8 = 0xF7;
const TOKEN_RETURN: u8 = 0xF8;
const TOKEN_UNTIL: u8 = 0xFD;
const TOKEN_VDU: u8 = 0xEF;
const TOKEN_LET: u8 = 0xE9;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Expr {
    Number(f64),
    /// An exact integer literal in the native System Profile lexer. Classic
    /// BASIC source continues to use the historical floating representation.
    Integer(i128),
    String(Vec<u8>),
    Variable(String),
    ArrayElement(String, Box<Expr>),
    Unary(UnaryOp, Box<Expr>),
    Binary(Box<Expr>, BinaryOp, Box<Expr>),
    Builtin(u8, Vec<Expr>),
    UserFunction(String, Vec<Expr>),
    ImportedFunction {
        module: String,
        name: String,
        arguments: Vec<Expr>,
    },
    MemoryRead(MemoryWidth, Box<Expr>),
    Member(Box<Expr>, String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UnaryOp {
    Plus,
    Minus,
    Not,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BinaryOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    IntegerDivide,
    Modulo,
    Power,
    ShiftLeft,
    And,
    Or,
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MemoryWidth {
    Byte,
    Word,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum LValue {
    Variable(String),
    ArrayElement(String, Expr),
    Memory(MemoryWidth, Expr),
    MemoryByteAt(Expr, Expr),
    MemoryOffset(MemoryWidth, Expr, Expr),
    MemoryString(Expr),
    StringSlice(String, Expr, Expr),
    RecordField(String, String),
    RecordPath(Vec<String>),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PrintItem {
    Value(Expr),
    Spaces(Expr),
    Tab(Expr, Expr),
    Comma,
    Semicolon,
    NewLine,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VduFormat {
    Byte,
    Word,
    Padded,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VduArgument {
    pub value: Expr,
    pub format: VduFormat,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DimDeclaration {
    pub name: String,
    pub dimensions: Vec<Expr>,
    pub byte_block: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Statement {
    Assign(LValue, Expr),
    Input(LValue),
    Print(Vec<PrintItem>),
    ClearScreen,
    ClearGraphics,
    Colour(Vec<Expr>),
    PrintFormat(Expr),
    Mode(Expr),
    Vdu(Vec<VduArgument>),
    Line(Expr, Expr, Expr, Expr),
    Move(Expr, Expr),
    Draw(Expr, Expr),
    Plot(Expr, Expr, Expr),
    Gcol(Expr, Expr),
    If(Expr, Vec<Statement>, Vec<Statement>),
    IfBlock(Expr),
    EndIf,
    Goto(u16),
    Gosub(u16),
    Dim(Vec<DimDeclaration>),
    Read(Vec<LValue>),
    Data(Vec<Expr>),
    Restore(Option<u16>),
    For {
        variable: String,
        start: Expr,
        end: Expr,
        step: Option<Expr>,
    },
    Next(Option<String>),
    Repeat,
    Until(Expr),
    ProcedureCall(String, Vec<Expr>),
    ImportedProcedureCall {
        module: String,
        name: String,
        arguments: Vec<Expr>,
    },
    LocalReadOnly {
        name: String,
        value_type: SystemType,
        value: Expr,
    },
    DefineProcedure(String, Vec<String>),
    DefineFunction(String, Vec<String>),
    Sys {
        name: Vec<u8>,
        arguments: Vec<Option<Expr>>,
        results: Vec<String>,
    },
    PrimitiveCall {
        name: String,
        arguments: Vec<Option<Expr>>,
        results: Vec<String>,
    },
    Try,
    Catch {
        error_name: String,
        error_type: String,
    },
    EndTry,
    Throw {
        error_type: String,
        code: Expr,
        message: Expr,
    },
    FunctionReturn(Expr),
    Return,
    EndProcedure,
    End,
    Call(Expr),
    StarCommand(Vec<u8>),
    NoOp,
}

#[derive(Clone, Debug)]
pub(crate) struct LocatedStatement {
    pub line_number: u16,
    pub statement: Statement,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ParsedProgram {
    pub instructions: Vec<LocatedStatement>,
    pub line_entries: BTreeMap<u16, usize>,
    pub procedures: std::collections::HashMap<String, Definition>,
    pub functions: std::collections::HashMap<String, Definition>,
    pub typed_parameters: std::collections::HashMap<String, Vec<SystemType>>,
    pub typed_results: std::collections::HashMap<String, SystemType>,
    pub throws_types: std::collections::HashMap<String, String>,
    pub system_types: BTreeMap<String, SystemTypeDefinition>,
    pub module_state_types: BTreeMap<String, SystemType>,
    pub readonly_bindings: std::collections::BTreeSet<String>,
    pub readonly_local_bindings: std::collections::HashMap<String, BTreeMap<String, SystemType>>,
    pub options: ProgramOptions,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SystemType {
    Byte,
    UInt16,
    UInt32,
    Int32,
    UInt64,
    Int64,
    Address32,
    String,
    Record(String),
    Enum(String),
    Flags(String),
    Handle(String),
    Error(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemField {
    pub name: String,
    pub value_type: SystemType,
    pub read_only: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SystemTypeDefinition {
    Record {
        fields: Vec<SystemField>,
    },
    Enum {
        underlying: SystemType,
        members: BTreeMap<String, i64>,
    },
    Flags {
        underlying: SystemType,
        members: BTreeMap<String, i64>,
    },
    Error {
        fields: Vec<SystemField>,
    },
    Handle,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ProgramOptions {
    pub mode: LanguageMode,
    pub target: GraphicsProfile,
    pub profile: Option<String>,
    pub mode_declared: bool,
    pub target_declared: bool,
    pub profile_declared: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Definition {
    pub entry: usize,
    pub parameters: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TokenProfile {
    /// Safe common subset for legacy files with shared-boundary records.
    /// Exact BASIC ROM version is not inferred from the record layout.
    SharedBoundaryCore,
    /// Token assignments used by the project's ARM BASIC V fixtures.
    ArmBasicV,
}

pub(crate) fn parse_program(
    program: &TokenizedBasicProgram,
    profile: TokenProfile,
) -> Result<ParsedProgram, RuntimeError> {
    let mut options = ProgramOptions::default();
    let mut directive_seen = false;
    let mut executable_seen = false;
    for line in &program.lines {
        if let Some(comment) = tokenized_rem_comment(&line.bytes) {
            if is_basic64_directive(comment) {
                if executable_seen || directive_seen {
                    return Err(syntax_error(
                        line.number,
                        "BASIC64 directive must appear once before executable source",
                    ));
                }
                parse_basic64_directive(comment, line.number, &mut options)?;
                directive_seen = true;
            }
        } else if !line.bytes.is_empty() {
            executable_seen = true;
        }
    }

    let mut parsed = ParsedProgram {
        options,
        ..ParsedProgram::default()
    };
    for line in &program.lines {
        if profile == TokenProfile::SharedBoundaryCore {
            validate_shared_boundary_core(line)?;
        }
        parsed
            .line_entries
            .insert(line.number, parsed.instructions.len());
        let statements = parse_line(line, LexMode::Tokenized)?;
        for statement in statements {
            if profile == TokenProfile::SharedBoundaryCore {
                validate_shared_boundary_statement(&statement, line.number)?;
            }
            record_statement(&mut parsed, line.number, statement);
        }
    }
    Ok(parsed)
}

pub(crate) fn parse_source(source: &str) -> Result<ParsedProgram, RuntimeError> {
    let mut parsed = ParsedProgram::default();
    let mut directive_seen = false;
    let mut executable_seen = false;
    let mut next_line_number = 10_u16;

    for raw_line in source.lines() {
        let (line_number, text) = split_source_line_number(raw_line, next_line_number)?;
        next_line_number = line_number.saturating_add(10);
        let bytes = text.as_bytes();
        if let Some(comment) = source_rem_comment(text) {
            if is_basic64_directive(comment.as_bytes()) {
                if executable_seen || directive_seen {
                    return Err(syntax_error(
                        line_number,
                        "BASIC64 directive must appear once before executable source",
                    ));
                }
                parse_basic64_directive(comment.as_bytes(), line_number, &mut parsed.options)?;
                directive_seen = true;
            }
        } else if !bytes.iter().all(u8::is_ascii_whitespace) {
            executable_seen = true;
        }

        parsed
            .line_entries
            .entry(line_number)
            .or_insert(parsed.instructions.len());
        let line = TokenizedBasicLine {
            number: line_number,
            bytes: bytes.to_vec(),
            line_references: Vec::new(),
        };
        for statement in parse_line(&line, LexMode::Source)? {
            record_statement(&mut parsed, line_number, statement);
        }
    }

    Ok(parsed)
}

pub(super) fn record_statement(parsed: &mut ParsedProgram, line_number: u16, statement: Statement) {
    let index = parsed.instructions.len();
    match &statement {
        Statement::DefineProcedure(name, parameters) => {
            parsed.procedures.insert(
                name.clone(),
                Definition {
                    entry: index + 1,
                    parameters: parameters.clone(),
                },
            );
        }
        Statement::DefineFunction(name, parameters) => {
            parsed.functions.insert(
                name.clone(),
                Definition {
                    entry: index + 1,
                    parameters: parameters.clone(),
                },
            );
        }
        _ => {}
    }
    parsed.instructions.push(LocatedStatement {
        line_number,
        statement,
    });
}

pub(super) fn split_source_line_number(
    line: &str,
    fallback: u16,
) -> Result<(u16, &str), RuntimeError> {
    let leading_trimmed = line.trim_start_matches([' ', '\t']);
    let digits = leading_trimmed
        .bytes()
        .take_while(u8::is_ascii_digit)
        .count();
    if digits == 0 || leading_trimmed.as_bytes().get(digits) == Some(&b'.') {
        return Ok((fallback, leading_trimmed));
    }
    let number = leading_trimmed[..digits]
        .parse::<u16>()
        .map_err(|_| RuntimeError::Program("source line number is outside 0..65535".into()))?;
    Ok((
        number,
        leading_trimmed[digits..].trim_start_matches([' ', '\t']),
    ))
}

pub(super) fn source_rem_comment(line: &str) -> Option<&str> {
    let line = line.trim_start_matches([' ', '\t']);
    let bytes = line.as_bytes();
    if bytes.len() < 3 || !bytes[..3].eq_ignore_ascii_case(b"REM") {
        return None;
    }
    if bytes.get(3).is_some_and(|byte| !byte.is_ascii_whitespace()) {
        return None;
    }
    Some(&line[3..])
}

fn tokenized_rem_comment(line: &[u8]) -> Option<&[u8]> {
    line.first()
        .is_some_and(|byte| *byte == TOKEN_REM)
        .then_some(&line[1..])
}

fn is_basic64_directive(comment: &[u8]) -> bool {
    let comment = comment
        .iter()
        .copied()
        .skip_while(u8::is_ascii_whitespace)
        .collect::<Vec<_>>();
    comment.len() >= 8
        && comment[..8].eq_ignore_ascii_case(b"@BASIC64")
        && comment.get(8).is_none_or(u8::is_ascii_whitespace)
}

pub(super) fn parse_basic64_directive(
    comment: &[u8],
    line_number: u16,
    options: &mut ProgramOptions,
) -> Result<(), RuntimeError> {
    let comment = std::str::from_utf8(comment)
        .map_err(|_| syntax_error(line_number, "BASIC64 directive must be ASCII/UTF-8"))?;
    let mut words = comment.split_ascii_whitespace();
    let marker = words.next().unwrap_or_default();
    if !marker.eq_ignore_ascii_case("@BASIC64") {
        return Err(syntax_error(line_number, "invalid BASIC64 directive"));
    }

    let mut mode_seen = false;
    let mut target_seen = false;
    let mut profile_seen = false;
    for word in words {
        let (key, value) = word
            .split_once('=')
            .ok_or_else(|| syntax_error(line_number, "directive fields must use KEY=VALUE"))?;
        if value.is_empty() {
            return Err(syntax_error(
                line_number,
                "directive values cannot be empty",
            ));
        }
        if key.eq_ignore_ascii_case("MODE") {
            if mode_seen {
                return Err(syntax_error(line_number, "duplicate MODE field"));
            }
            mode_seen = true;
            options.mode_declared = true;
            options.mode = if value.eq_ignore_ascii_case("CLASSIC") {
                LanguageMode::Classic
            } else if value.eq_ignore_ascii_case("BASIC64") {
                LanguageMode::Basic64
            } else if value.eq_ignore_ascii_case("HYBRID") {
                LanguageMode::Hybrid
            } else {
                return Err(syntax_error(line_number, "unknown BASIC language mode"));
            };
        } else if key.eq_ignore_ascii_case("TARGET") {
            if target_seen {
                return Err(syntax_error(line_number, "duplicate TARGET field"));
            }
            target_seen = true;
            options.target_declared = true;
            options.target =
                if value.eq_ignore_ascii_case("HOSTED") || value.eq_ignore_ascii_case("RISCOS") {
                    GraphicsProfile::Hosted
                } else if value.eq_ignore_ascii_case("AGON") {
                    GraphicsProfile::Agon
                } else {
                    return Err(syntax_error(line_number, "unknown BASIC runtime target"));
                };
        } else if key.eq_ignore_ascii_case("PROFILE") {
            if profile_seen {
                return Err(syntax_error(line_number, "duplicate PROFILE field"));
            }
            profile_seen = true;
            options.profile_declared = true;
            options.profile = Some(value.to_owned());
        } else {
            return Err(syntax_error(
                line_number,
                &format!("unknown BASIC64 directive field {key}"),
            ));
        }
    }
    if profile_seen && options.mode_declared && options.mode != LanguageMode::Classic {
        return Err(syntax_error(
            line_number,
            "PROFILE is only valid with MODE=CLASSIC",
        ));
    }
    Ok(())
}

fn validate_shared_boundary_statement(
    statement: &Statement,
    line_number: u16,
) -> Result<(), RuntimeError> {
    let supported = match statement {
        Statement::End | Statement::NoOp => true,
        Statement::Print(items) => items.iter().all(|item| {
            matches!(
                item,
                PrintItem::Value(Expr::String(_)) | PrintItem::Semicolon | PrintItem::NewLine
            ) || matches!(item, PrintItem::Tab(x, y) if shared_expression_supported(x) && shared_expression_supported(y))
        }),
        Statement::Mode(mode) => shared_expression_supported(mode),
        Statement::Vdu(arguments) => arguments
            .iter()
            .all(|argument| shared_expression_supported(&argument.value)),
        Statement::Line(x1, y1, x2, y2) => [x1, y1, x2, y2]
            .into_iter()
            .all(shared_expression_supported),
        Statement::Move(x, y) | Statement::Draw(x, y) => {
            shared_expression_supported(x) && shared_expression_supported(y)
        }
        Statement::Plot(code, x, y) => {
            shared_expression_supported(code)
                && shared_expression_supported(x)
                && shared_expression_supported(y)
        }
        Statement::Gcol(action, colour) => {
            shared_expression_supported(action) && shared_expression_supported(colour)
        }
        _ => false,
    };
    if supported {
        Ok(())
    } else {
        Err(syntax_error(
            line_number,
            "only leading REM comments, literal-string PRINT and END statements, plus simple graphics statements are supported by the shared-boundary core",
        ))
    }
}

fn shared_expression_supported(expression: &Expr) -> bool {
    match expression {
        Expr::Number(_) | Expr::Variable(_) => true,
        Expr::Unary(_, operand) => shared_expression_supported(operand),
        Expr::Binary(left, _, right) => {
            shared_expression_supported(left) && shared_expression_supported(right)
        }
        _ => false,
    }
}

fn validate_shared_boundary_core(line: &TokenizedBasicLine) -> Result<(), RuntimeError> {
    if line.bytes.first() == Some(&b'*') {
        return Err(syntax_error(
            line.number,
            "MOS commands are outside the shared-boundary core subset",
        ));
    }
    if line.bytes.first() == Some(&TOKEN_REM) {
        return Ok(());
    }

    let mut quoted = false;
    let mut offset = 0;
    while let Some(&byte) = line.bytes.get(offset) {
        if byte == b'"' {
            if quoted && line.bytes.get(offset + 1) == Some(&b'"') {
                offset += 2;
                continue;
            }
            quoted = !quoted;
        } else if !quoted
            && byte >= 0x7F
            && !matches!(
                byte,
                TOKEN_PRINT
                    | TOKEN_END
                    | TOKEN_MODE
                    | TOKEN_VDU
                    | TOKEN_LINE
                    | TOKEN_MOVE
                    | TOKEN_DRAW
                    | TOKEN_PLOT
                    | TOKEN_GCOL
                    | TOKEN_TAB
            )
        {
            return Err(syntax_error(
                line.number,
                &format!("token &{byte:02X} is outside the shared-boundary core subset"),
            ));
        }
        offset += 1;
    }
    Ok(())
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum LexMode {
    Tokenized,
    Source,
    SystemSource,
}

pub(super) fn parse_line(
    line: &TokenizedBasicLine,
    mode: LexMode,
) -> Result<Vec<Statement>, RuntimeError> {
    if line.bytes.first() == Some(&b'*') {
        return Ok(vec![Statement::StarCommand(line.bytes[1..].to_vec())]);
    }

    let mut parser = Parser::new(&line.bytes, line.number, mode)?;
    if parser.peek_keyword(TOKEN_REM) {
        return Ok(vec![Statement::NoOp]);
    }
    let mut statements = Vec::new();
    while !parser.is_end() {
        if parser.consume_symbol(b':') {
            continue;
        }
        let statement = parser.parse_statement()?;
        let repeat_opens_body = matches!(statement, Statement::Repeat);
        statements.push(statement);
        if !parser.is_end() && !parser.peek_symbol(b':') && !repeat_opens_body {
            return parser.error(format!(
                "expected ':' or end of line, found {:?}",
                parser.peek()
            ));
        }
    }
    Ok(statements)
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Number(f64),
    Integer(i128),
    String(Vec<u8>),
    Identifier(String),
    Keyword(u8),
    Sys,
    Primitive,
    SystemTry,
    SystemCatch,
    SystemEndTry,
    SystemThrow,
    LineReference(u16),
    Symbol(u8),
    End,
}

struct Lexer<'a> {
    bytes: &'a [u8],
    offset: usize,
    line_number: u16,
    mode: LexMode,
    previous: Option<Token>,
}

impl<'a> Lexer<'a> {
    fn new(bytes: &'a [u8], line_number: u16, mode: LexMode) -> Self {
        Self {
            bytes,
            offset: 0,
            line_number,
            mode,
            previous: None,
        }
    }

    fn tokenize(mut self) -> Result<Vec<Token>, RuntimeError> {
        let mut tokens = Vec::new();
        loop {
            let token = self.next_token()?;
            if token == Token::End {
                tokens.push(token);
                return Ok(tokens);
            }
            self.previous = Some(token.clone());
            tokens.push(token);
        }
    }

    fn next_token(&mut self) -> Result<Token, RuntimeError> {
        while matches!(self.bytes.get(self.offset), Some(b' ' | b'\t')) {
            self.offset += 1;
        }
        let Some(byte) = self.bytes.get(self.offset).copied() else {
            return Ok(Token::End);
        };

        if byte == 0x8D {
            let encoded = self
                .bytes
                .get(self.offset + 1..self.offset + 4)
                .ok_or_else(|| syntax_error(self.line_number, "truncated line reference"))?;
            self.offset += 4;
            return Ok(Token::LineReference(decode_line_reference(encoded)));
        }
        if byte == 0xC8 && self.bytes.get(self.offset + 1) == Some(&0x99) {
            self.offset += 2;
            return Ok(Token::Sys);
        }
        if byte >= 0x7F {
            self.offset += 1;
            if byte == TOKEN_REM {
                self.offset = self.bytes.len();
            }
            return Ok(Token::Keyword(byte));
        }
        if byte == b'"' {
            return self.string_literal();
        }
        if byte == b'&'
            && self
                .bytes
                .get(self.offset + 1)
                .is_some_and(u8::is_ascii_hexdigit)
        {
            return self.hex_literal();
        }
        if byte.is_ascii_digit()
            || (byte == b'.'
                && self
                    .bytes
                    .get(self.offset + 1)
                    .is_some_and(u8::is_ascii_digit))
        {
            return self.number_literal();
        }
        if byte.is_ascii_alphabetic() {
            return self.identifier();
        }
        self.offset += 1;
        Ok(Token::Symbol(byte))
    }

    fn string_literal(&mut self) -> Result<Token, RuntimeError> {
        self.offset += 1;
        let mut value = Vec::new();
        loop {
            let Some(byte) = self.bytes.get(self.offset).copied() else {
                return Err(syntax_error(
                    self.line_number,
                    "unterminated string literal",
                ));
            };
            self.offset += 1;
            if byte == b'"' {
                if self.bytes.get(self.offset) == Some(&b'"') {
                    value.push(b'"');
                    self.offset += 1;
                    continue;
                }
                return Ok(Token::String(value));
            }
            value.push(byte);
        }
    }

    fn hex_literal(&mut self) -> Result<Token, RuntimeError> {
        self.offset += 1;
        let start = self.offset;
        while self
            .bytes
            .get(self.offset)
            .is_some_and(u8::is_ascii_hexdigit)
        {
            self.offset += 1;
        }
        let digits = std::str::from_utf8(&self.bytes[start..self.offset])
            .map_err(|_| syntax_error(self.line_number, "invalid hexadecimal literal"))?;
        let value = u32::from_str_radix(digits, 16)
            .map_err(|_| syntax_error(self.line_number, "hexadecimal literal is too large"))?;
        if self.mode == LexMode::SystemSource {
            Ok(Token::Integer(i128::from(value)))
        } else {
            Ok(Token::Number(f64::from(value)))
        }
    }

    fn number_literal(&mut self) -> Result<Token, RuntimeError> {
        let start = self.offset;
        while self.bytes.get(self.offset).is_some_and(u8::is_ascii_digit) {
            self.offset += 1;
        }
        if self.bytes.get(self.offset) == Some(&b'.') {
            self.offset += 1;
            while self.bytes.get(self.offset).is_some_and(u8::is_ascii_digit) {
                self.offset += 1;
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.offset])
            .map_err(|_| syntax_error(self.line_number, "invalid numeric literal"))?;
        if self.mode == LexMode::SystemSource && !text.contains('.') {
            let value = text
                .parse::<i128>()
                .map_err(|_| syntax_error(self.line_number, "integer literal is too large"))?;
            Ok(Token::Integer(value))
        } else {
            let value = text
                .parse::<f64>()
                .map_err(|_| syntax_error(self.line_number, "invalid numeric literal"))?;
            Ok(Token::Number(value))
        }
    }

    fn identifier(&mut self) -> Result<Token, RuntimeError> {
        let start = self.offset;
        self.offset += 1;
        while self
            .bytes
            .get(self.offset)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            self.offset += 1;
        }
        if self.bytes.get(self.offset) == Some(&b'$') {
            self.offset += 1;
        } else if self.bytes.get(self.offset) == Some(&b'%') {
            self.offset += 1;
            if self.bytes.get(self.offset) == Some(&b'%') {
                self.offset += 1;
            }
        }
        let value = std::str::from_utf8(&self.bytes[start..self.offset])
            .map_err(|_| syntax_error(self.line_number, "invalid variable name"))?
            .to_ascii_uppercase();
        if self.mode == LexMode::SystemSource && matches!(self.previous, Some(Token::Symbol(b'.')))
        {
            return Ok(Token::Identifier(value));
        }
        if self.mode == LexMode::SystemSource && value == "PRIMITIVE" {
            return Ok(Token::Primitive);
        }
        if self.mode == LexMode::SystemSource {
            match value.as_str() {
                "TRY" => return Ok(Token::SystemTry),
                "CATCH" => return Ok(Token::SystemCatch),
                "ENDTRY" => return Ok(Token::SystemEndTry),
                "THROW" => return Ok(Token::SystemThrow),
                _ => {}
            }
        }
        if matches!(self.mode, LexMode::Source | LexMode::SystemSource) {
            if value == "SYS" {
                return Ok(Token::Sys);
            }
            // BBC BASIC source routinely places keywords directly beside their
            // following argument (`FORI%=...`, `PROCs`, `REPEATUNTIL...`). The
            // tokenised form makes those boundaries explicit; recover them in
            // source mode by consuming the longest reserved word prefix.
            if let Some(keyword) = source_keyword(&value) {
                if keyword == TOKEN_REM {
                    self.offset = self.bytes.len();
                }
                return Ok(Token::Keyword(keyword));
            }
            if let Some((spelling, keyword)) =
                source_keyword_prefix(&value).filter(|(_, keyword)| {
                    source_keyword_prefix_is_valid(*keyword, self.previous.as_ref())
                })
            {
                self.offset = start + spelling.len();
                if keyword == TOKEN_REM {
                    self.offset = self.bytes.len();
                }
                return Ok(Token::Keyword(keyword));
            }
        }
        Ok(Token::Identifier(value))
    }
}

fn source_keyword(name: &str) -> Option<u8> {
    Some(match name {
        "AND" => TOKEN_AND,
        "DIV" => TOKEN_DIV,
        "MOD" => TOKEN_MOD,
        "OR" => TOKEN_OR,
        "LINE" => TOKEN_LINE,
        "SPC" => TOKEN_SPC,
        "TAB" => TOKEN_TAB,
        "THEN" => TOKEN_THEN,
        "ELSE" => TOKEN_ELSE,
        "STEP" => TOKEN_STEP,
        "PTR" => TOKEN_PTR,
        "PAGE" => TOKEN_PAGE,
        "TIME" => TOKEN_TIME,
        "LOMEM" => TOKEN_LOMEM,
        "HIMEM" => TOKEN_HIMEM,
        "ABS" => TOKEN_ABS,
        "ASC" => TOKEN_ASC,
        "COS" => TOKEN_COS,
        "FN" => TOKEN_FN,
        "INKEY" => TOKEN_INKEY,
        "INSTR" => TOKEN_INSTR,
        "INT" => TOKEN_INT,
        "LEN" => TOKEN_LEN,
        "LN" => TOKEN_LN,
        "LOG" => TOKEN_LOG,
        "NOT" => TOKEN_NOT,
        "SQR" => TOKEN_SQR,
        "TAN" => TOKEN_TAN,
        "SIN" => TOKEN_SIN,
        "RND" => TOKEN_RND,
        "TO" => TOKEN_TO,
        "VAL" => TOKEN_VAL,
        "CHR$" => TOKEN_CHR,
        "CLS" => TOKEN_CLS,
        "CLG" => TOKEN_CLG,
        "COLOUR" | "COLOR" => TOKEN_COLOUR,
        "LEFT$" => TOKEN_LEFT,
        "MID$" => TOKEN_MID,
        "RIGHT$" => TOKEN_RIGHT,
        "STR$" => TOKEN_STR,
        "STRING$" => TOKEN_STRING,
        "CALL" => TOKEN_CALL,
        "DRAW" => TOKEN_DRAW,
        "DATA" => TOKEN_DATA,
        "DEF" => TOKEN_DEF,
        "END" => TOKEN_END,
        "ENDPROC" => TOKEN_ENDPROC,
        "ENDIF" => TOKEN_ENDIF,
        "DIM" => TOKEN_DIM,
        "FOR" => TOKEN_FOR,
        "GOSUB" => TOKEN_GOSUB,
        "GOTO" => TOKEN_GOTO,
        "IF" => TOKEN_IF,
        "INPUT" => TOKEN_INPUT,
        "GCOL" => TOKEN_GCOL,
        "MOVE" => TOKEN_MOVE,
        "MODE" => TOKEN_MODE,
        "NEXT" => TOKEN_NEXT,
        "PRINT" => TOKEN_PRINT,
        "PROC" => TOKEN_PROC,
        "PLOT" => TOKEN_PLOT,
        "READ" => TOKEN_READ,
        "REM" => TOKEN_REM,
        "REPEAT" => TOKEN_REPEAT,
        "RESTORE" => TOKEN_RESTORE,
        "RETURN" => TOKEN_RETURN,
        "UNTIL" => TOKEN_UNTIL,
        "VDU" => TOKEN_VDU,
        "LET" => TOKEN_LET,
        _ => return None,
    })
}

fn source_keyword_prefix(name: &str) -> Option<(&'static str, u8)> {
    const KEYWORDS: &[&str] = &[
        "ENDPROC", "STRING$", "RESTORE", "REPEAT", "RETURN", "INSTR", "RIGHT$", "COLOUR", "ENDIF",
        "GOSUB", "INPUT", "LOMEM", "HIMEM", "CHR$", "COLOR", "LEFT$", "MID$", "PRINT", "READ",
        "UNTIL", "GOTO", "MODE", "NEXT", "PROC", "PLOT", "CALL", "DRAW", "DATA", "DEF", "DIM",
        "ELSE", "FOR", "GCOL", "IF", "LET", "LINE", "MOVE", "REM", "TAB", "THEN", "VDU", "ABS",
        "AND", "ASC", "CLG", "CLS", "COS", "DIV", "END", "FN", "INT", "INKEY", "LEN", "LN", "LOG",
        "MOD", "NOT", "OR", "PAGE", "PTR", "RND", "SIN", "SPC", "SQR", "STEP", "TAN", "TIME", "TO",
        "VAL", "STR$",
    ];
    KEYWORDS
        .iter()
        .copied()
        .filter(|keyword| name.starts_with(keyword))
        .max_by_key(|keyword| keyword.len())
        .and_then(|spelling| source_keyword(spelling).map(|token| (spelling, token)))
}

fn source_keyword_prefix_is_valid(keyword: u8, previous: Option<&Token>) -> bool {
    let follows_print_separator = keyword == TOKEN_ELSE && previous == Some(&Token::Symbol(b';'));
    if follows_print_separator {
        return true;
    }
    let statement_boundary = previous.is_none_or(|previous| {
        matches!(
            previous,
            Token::Symbol(b':')
                | Token::Keyword(TOKEN_THEN | TOKEN_ELSE | TOKEN_REPEAT | TOKEN_DEF)
        )
    });
    if is_statement_keyword(keyword) || is_binary_keyword(keyword) {
        return statement_boundary || previous.is_some_and(token_ends_expression);
    }

    let starts_definition =
        previous == Some(&Token::Keyword(TOKEN_DEF)) && matches!(keyword, TOKEN_PROC | TOKEN_FN);
    let adjacent_print_control = matches!(keyword, TOKEN_SPC | TOKEN_TAB)
        && (previous.is_some_and(token_ends_expression)
            || matches!(previous, Some(Token::Symbol(b'\''))));
    starts_definition
        || follows_print_separator
        || adjacent_print_control
        || (is_expression_keyword(keyword) && previous.is_some_and(token_expects_expression))
}

fn is_binary_keyword(keyword: u8) -> bool {
    matches!(
        keyword,
        TOKEN_AND | TOKEN_DIV | TOKEN_MOD | TOKEN_OR | TOKEN_STEP | TOKEN_TO
    )
}

fn is_statement_keyword(keyword: u8) -> bool {
    matches!(
        keyword,
        TOKEN_CALL
            | TOKEN_CLS
            | TOKEN_CLG
            | TOKEN_COLOUR
            | TOKEN_DATA
            | TOKEN_DEF
            | TOKEN_DIM
            | TOKEN_DRAW
            | TOKEN_END
            | TOKEN_ENDIF
            | TOKEN_ENDPROC
            | TOKEN_FOR
            | TOKEN_GCOL
            | TOKEN_GOSUB
            | TOKEN_GOTO
            | TOKEN_IF
            | TOKEN_INPUT
            | TOKEN_LET
            | TOKEN_LINE
            | TOKEN_MODE
            | TOKEN_MOVE
            | TOKEN_NEXT
            | TOKEN_PLOT
            | TOKEN_PRINT
            | TOKEN_PROC
            | TOKEN_READ
            | TOKEN_REM
            | TOKEN_REPEAT
            | TOKEN_RESTORE
            | TOKEN_RETURN
            | TOKEN_ELSE
            | TOKEN_THEN
            | TOKEN_UNTIL
            | TOKEN_VDU
    )
}

fn is_expression_keyword(keyword: u8) -> bool {
    matches!(
        keyword,
        TOKEN_ABS
            | TOKEN_AND
            | TOKEN_ASC
            | TOKEN_CHR
            | TOKEN_COS
            | TOKEN_DIV
            | TOKEN_FN
            | TOKEN_HIMEM
            | TOKEN_INKEY
            | TOKEN_INSTR
            | TOKEN_INT
            | TOKEN_LEN
            | TOKEN_LEFT
            | TOKEN_LN
            | TOKEN_LOMEM
            | TOKEN_LOG
            | TOKEN_MID
            | TOKEN_MOD
            | TOKEN_NOT
            | TOKEN_OR
            | TOKEN_PAGE
            | TOKEN_PTR
            | TOKEN_RIGHT
            | TOKEN_RND
            | TOKEN_SIN
            | TOKEN_SPC
            | TOKEN_SQR
            | TOKEN_STEP
            | TOKEN_STR
            | TOKEN_STRING
            | TOKEN_TAB
            | TOKEN_TAN
            | TOKEN_THEN
            | TOKEN_TIME
            | TOKEN_TO
            | TOKEN_VAL
    )
}

fn token_ends_expression(token: &Token) -> bool {
    matches!(
        token,
        Token::Number(_)
            | Token::Integer(_)
            | Token::String(_)
            | Token::Identifier(_)
            | Token::LineReference(_)
            | Token::Symbol(b')')
            | Token::Keyword(
                TOKEN_INKEY | TOKEN_LOMEM | TOKEN_HIMEM | TOKEN_PAGE | TOKEN_PTR | TOKEN_TIME
            )
    )
}

fn token_expects_expression(token: &Token) -> bool {
    matches!(
        token,
        Token::Symbol(
            b'=' | b'+' | b'-' | b'*' | b'/' | b'^' | b'<' | b'>' | b'(' | b',' | b'!' | b'?'
        ) | Token::Keyword(
            TOKEN_ABS
                | TOKEN_AND
                | TOKEN_ASC
                | TOKEN_COS
                | TOKEN_DIV
                | TOKEN_FN
                | TOKEN_IF
                | TOKEN_INSTR
                | TOKEN_INT
                | TOKEN_LEN
                | TOKEN_LEFT
                | TOKEN_LN
                | TOKEN_LOG
                | TOKEN_MID
                | TOKEN_MOD
                | TOKEN_NOT
                | TOKEN_OR
                | TOKEN_PRINT
                | TOKEN_RIGHT
                | TOKEN_SIN
                | TOKEN_SPC
                | TOKEN_SQR
                | TOKEN_STR
                | TOKEN_STRING
                | TOKEN_TAB
                | TOKEN_TAN
                | TOKEN_THEN
                | TOKEN_TO
                | TOKEN_UNTIL
        )
    )
}

struct Parser {
    tokens: Vec<Token>,
    cursor: usize,
    line_number: u16,
    source_mode: bool,
    system_source: bool,
}

impl Parser {
    fn new(bytes: &[u8], line_number: u16, mode: LexMode) -> Result<Self, RuntimeError> {
        Ok(Self {
            tokens: Lexer::new(bytes, line_number, mode).tokenize()?,
            cursor: 0,
            line_number,
            source_mode: matches!(mode, LexMode::Source | LexMode::SystemSource),
            system_source: mode == LexMode::SystemSource,
        })
    }

    fn parse_statement(&mut self) -> Result<Statement, RuntimeError> {
        let token = self.next().clone();
        match token {
            Token::Keyword(TOKEN_IF) => self.parse_if(),
            Token::Keyword(TOKEN_PRINT) => self.parse_print(),
            Token::Keyword(TOKEN_CLS) => Ok(Statement::ClearScreen),
            Token::Keyword(TOKEN_CLG) => Ok(Statement::ClearGraphics),
            Token::Keyword(TOKEN_COLOUR) => self.parse_colour(),
            Token::Keyword(TOKEN_LET) => {
                if self.system_source
                    && matches!(self.peek(), Token::Identifier(name) if name == "READONLY")
                {
                    self.next();
                    self.parse_readonly_local()
                } else {
                    self.parse_statement()
                }
            }
            Token::Keyword(TOKEN_MODE) => Ok(Statement::Mode(self.parse_expression(0)?)),
            Token::Symbol(b'@') => {
                self.expect_symbol(b'%')?;
                self.expect_symbol(b'=')?;
                Ok(Statement::PrintFormat(self.parse_expression(0)?))
            }
            Token::Keyword(TOKEN_VDU) => self.parse_vdu(),
            Token::Primitive => self.parse_primitive_call(),
            Token::SystemTry => Ok(Statement::Try),
            Token::SystemCatch => {
                let error_name = self.expect_identifier("catch error binding")?;
                let as_name = self.expect_identifier("AS")?;
                if as_name != "AS" {
                    return self.error("CATCH syntax is CATCH name AS ErrorType");
                }
                let error_type = self.expect_identifier("structured error type")?;
                Ok(Statement::Catch {
                    error_name,
                    error_type,
                })
            }
            Token::SystemEndTry => Ok(Statement::EndTry),
            Token::SystemThrow => {
                let error_type = self.expect_identifier("structured error type")?;
                self.expect_symbol(b',')?;
                let code = self.parse_expression(0)?;
                self.expect_symbol(b',')?;
                let message = self.parse_expression(0)?;
                Ok(Statement::Throw {
                    error_type,
                    code,
                    message,
                })
            }
            Token::Keyword(TOKEN_LINE) => self.parse_line_statement(),
            Token::Keyword(TOKEN_MOVE) => self.parse_move_statement(false),
            Token::Keyword(TOKEN_DRAW) => self.parse_move_statement(true),
            Token::Keyword(TOKEN_PLOT) => self.parse_plot_statement(),
            Token::Keyword(TOKEN_GCOL) => self.parse_gcol_statement(),
            Token::Keyword(TOKEN_INPUT) => Ok(Statement::Input(self.parse_lvalue()?)),
            Token::Keyword(TOKEN_GOTO) => {
                Ok(Statement::Goto(self.expect_line_reference("GOTO target")?))
            }
            Token::Keyword(TOKEN_GOSUB) => Ok(Statement::Gosub(
                self.expect_line_reference("GOSUB target")?,
            )),
            Token::Keyword(TOKEN_DIM) => self.parse_dim(),
            Token::Keyword(TOKEN_READ) => self.parse_read(),
            Token::Keyword(TOKEN_DATA) => self.parse_data(),
            Token::Keyword(TOKEN_RESTORE) => {
                let target = if matches!(self.peek(), Token::LineReference(_))
                    || (self.source_mode
                        && matches!(self.peek(), Token::Number(_) | Token::Integer(_)))
                {
                    Some(self.expect_line_reference("RESTORE target")?)
                } else {
                    None
                };
                Ok(Statement::Restore(target))
            }
            Token::Keyword(TOKEN_FOR) => self.parse_for(),
            Token::Keyword(TOKEN_NEXT) => {
                let name = if let Token::Identifier(name) = self.peek().clone() {
                    self.next();
                    Some(name)
                } else {
                    None
                };
                Ok(Statement::Next(name))
            }
            Token::Keyword(TOKEN_REPEAT) => Ok(Statement::Repeat),
            Token::Keyword(TOKEN_UNTIL) => Ok(Statement::Until(self.parse_expression(0)?)),
            Token::Keyword(TOKEN_PROC) => {
                let mut name = self.expect_routine_name("procedure name")?;
                let module = if self.system_source && self.consume_symbol(b'.') {
                    let symbol = self.expect_routine_name("imported procedure name")?;
                    let module = name;
                    name = symbol;
                    Some(module)
                } else {
                    None
                };
                let arguments = if self.consume_symbol(b'(') {
                    self.parse_call_arguments_after_open()?
                } else {
                    Vec::new()
                };
                if let Some(module) = module {
                    Ok(Statement::ImportedProcedureCall {
                        module,
                        name,
                        arguments,
                    })
                } else {
                    Ok(Statement::ProcedureCall(name, arguments))
                }
            }
            Token::Keyword(TOKEN_DEF) => self.parse_definition(),
            Token::Sys => self.parse_sys(),
            Token::Keyword(TOKEN_RETURN) => Ok(Statement::Return),
            Token::Keyword(TOKEN_ENDPROC) => Ok(Statement::EndProcedure),
            Token::Keyword(TOKEN_ENDIF) => Ok(Statement::EndIf),
            Token::Keyword(TOKEN_END) => Ok(Statement::End),
            Token::Keyword(TOKEN_CALL) => Ok(Statement::Call(self.parse_expression(0)?)),
            Token::Keyword(TOKEN_REM) => Ok(Statement::NoOp),
            Token::Keyword(token) if pseudo_variable_for_assignment(token).is_some() => {
                let name = pseudo_variable_for_assignment(token)
                    .expect("guard matches pseudo-variable assignments");
                self.expect_symbol(b'=')?;
                Ok(Statement::Assign(
                    LValue::Variable(name.into()),
                    self.parse_expression(0)?,
                ))
            }
            Token::Symbol(b'=') => Ok(Statement::FunctionReturn(self.parse_expression(0)?)),
            Token::Symbol(b'$') => {
                let address = match self.next().clone() {
                    Token::Keyword(TOKEN_MODE) => Expr::Variable("MODE".into()),
                    Token::Identifier(name) => Expr::Variable(name),
                    other => {
                        return self.error(format!("expected string address, found {other:?}"));
                    }
                };
                self.expect_symbol(b'=')?;
                Ok(Statement::Assign(
                    LValue::MemoryString(address),
                    self.parse_expression(0)?,
                ))
            }
            Token::Symbol(b'!') => {
                let address = self.parse_expression(4)?;
                self.expect_symbol(b'=')?;
                Ok(Statement::Assign(
                    LValue::Memory(MemoryWidth::Word, address),
                    self.parse_expression(0)?,
                ))
            }
            Token::Symbol(b'?') => {
                let address = self.parse_expression(4)?;
                self.expect_symbol(b'=')?;
                Ok(Statement::Assign(
                    LValue::Memory(MemoryWidth::Byte, address),
                    self.parse_expression(0)?,
                ))
            }
            Token::Identifier(name) => self.parse_assignment_or_function_call(name),
            Token::Keyword(TOKEN_MID) => self.parse_string_slice_assignment(),
            Token::LineReference(target) => Ok(Statement::Goto(target)),
            Token::End => self.error("expected a statement"),
            other => self.error(format!("unexpected token {other:?} at statement start")),
        }
    }

    fn parse_readonly_local(&mut self) -> Result<Statement, RuntimeError> {
        let name = self.expect_identifier("read-only local binding name")?;
        let as_keyword = self.expect_identifier("AS")?;
        if as_keyword != "AS" {
            return self.error("LET READONLY syntax is name AS Type = expression");
        }
        let mut type_name = self.expect_identifier("read-only local type")?;
        if type_name == "HANDLE" && self.consume_symbol(b'<') {
            let handle_type = self.expect_identifier("opaque handle type")?;
            self.expect_symbol(b'>')?;
            type_name = format!("HANDLE<{handle_type}>");
        }
        let value_type = super::system_profile::parse_system_type(&type_name)
            .map_err(|message| syntax_error(self.line_number, &message))?;
        self.expect_symbol(b'=')?;
        let value = self.parse_expression(0)?;
        Ok(Statement::LocalReadOnly {
            name,
            value_type,
            value,
        })
    }

    fn parse_if(&mut self) -> Result<Statement, RuntimeError> {
        let condition = self.parse_expression(0)?;
        self.consume_keyword(TOKEN_THEN);
        let then_body = self.parse_if_body()?;
        let else_body = if self.consume_keyword(TOKEN_ELSE) {
            self.parse_if_body()?
        } else {
            Vec::new()
        };
        if then_body.is_empty() {
            if else_body.is_empty() {
                return Ok(Statement::IfBlock(condition));
            }
            return self.error("IF has no statement to execute");
        }
        Ok(Statement::If(condition, then_body, else_body))
    }

    fn parse_if_body(&mut self) -> Result<Vec<Statement>, RuntimeError> {
        let mut body = Vec::new();
        while !self.is_end() && !self.peek_keyword(TOKEN_ELSE) {
            if self.consume_symbol(b':') {
                continue;
            }
            if self.source_mode {
                if let Token::Integer(target) = self.peek() {
                    if (0..=i128::from(u16::MAX)).contains(target) {
                        let target = *target as u16;
                        self.next();
                        body.push(Statement::Goto(target));
                        continue;
                    }
                }
                if let Token::Number(target) = self.peek() {
                    if target.is_finite()
                        && *target >= 0.0
                        && *target <= f64::from(u16::MAX)
                        && target.fract() == 0.0
                    {
                        let target = *target as u16;
                        self.next();
                        body.push(Statement::Goto(target));
                        continue;
                    }
                }
            }
            body.push(self.parse_statement()?);
            if !self.is_end() && !self.peek_symbol(b':') && !self.peek_keyword(TOKEN_ELSE) {
                return self.error("expected ':' or end of IF line");
            }
        }
        Ok(body)
    }

    fn parse_print(&mut self) -> Result<Statement, RuntimeError> {
        let mut items = Vec::new();
        while !self.is_end() && !self.peek_symbol(b':') && !self.peek_keyword(TOKEN_ELSE) {
            if self.consume_symbol(b';') {
                items.push(PrintItem::Semicolon);
            } else if self.consume_symbol(b',') {
                items.push(PrintItem::Comma);
            } else if self.consume_symbol(b'\'') {
                items.push(PrintItem::NewLine);
            } else if self.consume_keyword(TOKEN_SPC) {
                items.push(PrintItem::Spaces(self.parse_expression(0)?));
            } else if self.consume_keyword(TOKEN_TAB) {
                self.expect_symbol(b'(')?;
                let x = self.parse_expression(0)?;
                self.expect_symbol(b',')?;
                let y = self.parse_expression(0)?;
                self.expect_symbol(b')')?;
                items.push(PrintItem::Tab(x, y));
            } else {
                items.push(PrintItem::Value(self.parse_expression(0)?));
            }
        }
        Ok(Statement::Print(items))
    }

    fn parse_vdu(&mut self) -> Result<Statement, RuntimeError> {
        let mut arguments = Vec::new();
        while !self.is_end() && !self.peek_symbol(b':') && !self.peek_keyword(TOKEN_ELSE) {
            let value = self.parse_expression(0)?;
            let (format, more_arguments) = if self.consume_symbol(b';') {
                (VduFormat::Word, true)
            } else if self.consume_symbol(b'|') {
                (VduFormat::Padded, true)
            } else if self.consume_symbol(b',') {
                (VduFormat::Byte, true)
            } else {
                (VduFormat::Byte, false)
            };
            arguments.push(VduArgument { value, format });
            if !more_arguments {
                break;
            }
            self.consume_symbol(b',');
        }
        if arguments.is_empty() {
            return self.error("VDU requires at least one argument");
        }
        Ok(Statement::Vdu(arguments))
    }

    fn parse_line_statement(&mut self) -> Result<Statement, RuntimeError> {
        let x1 = self.parse_expression(0)?;
        self.expect_symbol(b',')?;
        let y1 = self.parse_expression(0)?;
        self.expect_symbol(b',')?;
        let x2 = self.parse_expression(0)?;
        self.expect_symbol(b',')?;
        let y2 = self.parse_expression(0)?;
        Ok(Statement::Line(x1, y1, x2, y2))
    }

    fn parse_move_statement(&mut self, draw: bool) -> Result<Statement, RuntimeError> {
        let x = self.parse_expression(0)?;
        self.expect_symbol(b',')?;
        let y = self.parse_expression(0)?;
        Ok(if draw {
            Statement::Draw(x, y)
        } else {
            Statement::Move(x, y)
        })
    }

    fn parse_plot_statement(&mut self) -> Result<Statement, RuntimeError> {
        let code = self.parse_expression(0)?;
        self.expect_symbol(b',')?;
        let x = self.parse_expression(0)?;
        self.expect_symbol(b',')?;
        let y = self.parse_expression(0)?;
        Ok(Statement::Plot(code, x, y))
    }

    fn parse_gcol_statement(&mut self) -> Result<Statement, RuntimeError> {
        let action = self.parse_expression(0)?;
        self.expect_symbol(b',')?;
        let colour = self.parse_expression(0)?;
        Ok(Statement::Gcol(action, colour))
    }

    fn parse_colour(&mut self) -> Result<Statement, RuntimeError> {
        let mut colours = vec![self.parse_expression(0)?];
        while self.consume_symbol(b',') {
            colours.push(self.parse_expression(0)?);
        }
        Ok(Statement::Colour(colours))
    }

    fn parse_dim(&mut self) -> Result<Statement, RuntimeError> {
        let mut declarations = Vec::new();
        loop {
            let name = self.expect_identifier("DIM variable")?;
            let (dimensions, byte_block) = if self.consume_symbol(b'(') {
                let dimensions = self.parse_expression_list_until(b')')?;
                self.expect_symbol(b')')?;
                (dimensions, false)
            } else if !self.peek_symbol(b',') && !self.is_end() {
                (vec![self.parse_expression(0)?], name.ends_with('%'))
            } else {
                (Vec::new(), false)
            };
            declarations.push(DimDeclaration {
                name,
                dimensions,
                byte_block,
            });
            if !self.consume_symbol(b',') {
                break;
            }
        }
        Ok(Statement::Dim(declarations))
    }

    fn parse_read(&mut self) -> Result<Statement, RuntimeError> {
        let mut targets = vec![self.parse_lvalue()?];
        while self.consume_symbol(b',') {
            targets.push(self.parse_lvalue()?);
        }
        Ok(Statement::Read(targets))
    }

    fn parse_data(&mut self) -> Result<Statement, RuntimeError> {
        let mut values = vec![self.parse_expression(0)?];
        while self.consume_symbol(b',') {
            values.push(self.parse_expression(0)?);
        }
        Ok(Statement::Data(values))
    }

    fn parse_for(&mut self) -> Result<Statement, RuntimeError> {
        let variable = self.expect_identifier("FOR variable")?;
        self.expect_symbol(b'=')?;
        let start = self.parse_expression(0)?;
        if !self.consume_keyword(TOKEN_TO) {
            return self.error("expected TO in FOR statement");
        }
        let end = self.parse_expression(0)?;
        let step = if self.consume_keyword(TOKEN_STEP) {
            Some(self.parse_expression(0)?)
        } else {
            None
        };
        Ok(Statement::For {
            variable,
            start,
            end,
            step,
        })
    }

    fn parse_definition(&mut self) -> Result<Statement, RuntimeError> {
        if self.consume_keyword(TOKEN_PROC) {
            let name = self.expect_routine_name("procedure name")?;
            let parameters = self.parse_parameter_list()?;
            Ok(Statement::DefineProcedure(name, parameters))
        } else if self.consume_keyword(TOKEN_FN) {
            let name = self.expect_named_routine("function name")?;
            let parameters = self.parse_parameter_list()?;
            Ok(Statement::DefineFunction(name, parameters))
        } else {
            self.error("expected PROC or FN after DEF")
        }
    }

    fn parse_parameter_list(&mut self) -> Result<Vec<String>, RuntimeError> {
        if !self.consume_symbol(b'(') {
            return Ok(Vec::new());
        }
        let mut parameters = Vec::new();
        if self.consume_symbol(b')') {
            return Ok(parameters);
        }
        loop {
            parameters.push(self.expect_identifier("parameter name")?);
            if self.consume_symbol(b')') {
                break;
            }
            self.expect_symbol(b',')?;
        }
        Ok(parameters)
    }

    fn parse_sys(&mut self) -> Result<Statement, RuntimeError> {
        let Token::String(name) = self.next().clone() else {
            return self.error("SYS requires a quoted SWI name");
        };
        let mut arguments = Vec::new();
        if self.consume_symbol(b',') {
            loop {
                if self.peek_symbol(b',') || self.peek_keyword(TOKEN_TO) || self.is_end() {
                    arguments.push(None);
                } else {
                    arguments.push(Some(self.parse_expression(0)?));
                }
                if !self.consume_symbol(b',') {
                    break;
                }
            }
        }
        let mut results = Vec::new();
        if self.consume_keyword(TOKEN_TO) {
            loop {
                results.push(self.expect_identifier("SYS result variable")?);
                if !self.consume_symbol(b',') {
                    break;
                }
            }
        }
        Ok(Statement::Sys {
            name,
            arguments,
            results,
        })
    }

    fn parse_primitive_call(&mut self) -> Result<Statement, RuntimeError> {
        let mut name = self.expect_identifier("primitive import name")?;
        while self.consume_symbol(b'.') {
            name.push('.');
            name.push_str(&self.expect_identifier("qualified primitive name")?);
        }
        let mut arguments = Vec::new();
        if self.consume_symbol(b',') {
            loop {
                if self.peek_symbol(b',') || self.peek_keyword(TOKEN_TO) || self.is_end() {
                    arguments.push(None);
                } else {
                    arguments.push(Some(self.parse_expression(0)?));
                }
                if !self.consume_symbol(b',') {
                    break;
                }
            }
        }
        let mut results = Vec::new();
        if self.consume_keyword(TOKEN_TO) {
            loop {
                results.push(self.expect_identifier("primitive result variable")?);
                if !self.consume_symbol(b',') {
                    break;
                }
            }
        }
        Ok(Statement::PrimitiveCall {
            name,
            arguments,
            results,
        })
    }

    fn parse_string_slice_assignment(&mut self) -> Result<Statement, RuntimeError> {
        let arguments = self.parse_call_arguments()?;
        if arguments.len() != 3 {
            return self.error("MID$ assignment requires three arguments");
        }
        let Expr::Variable(name) = arguments[0].clone() else {
            return self.error("MID$ assignment requires a string variable");
        };
        self.expect_symbol(b'=')?;
        Ok(Statement::Assign(
            LValue::StringSlice(name, arguments[1].clone(), arguments[2].clone()),
            self.parse_expression(0)?,
        ))
    }

    fn parse_assignment_or_function_call(
        &mut self,
        name: String,
    ) -> Result<Statement, RuntimeError> {
        if self.system_source && self.consume_symbol(b'.') {
            let mut path = vec![name.clone(), self.expect_identifier("record field")?];
            while self.consume_symbol(b'.') {
                path.push(self.expect_identifier("record field")?);
            }
            if !self.consume_symbol(b'=') {
                return self.error("expected '=' after record field");
            }
            let target = if path.len() == 2 {
                LValue::RecordField(path[0].clone(), path[1].clone())
            } else {
                LValue::RecordPath(path)
            };
            return Ok(Statement::Assign(target, self.parse_expression(0)?));
        }
        let target = if self.consume_symbol(b'(') {
            let index = self.parse_expression(0)?;
            self.expect_symbol(b')')?;
            LValue::ArrayElement(name, index)
        } else {
            LValue::Variable(name.clone())
        };

        if self.consume_symbol(b'!') {
            let offset = self.parse_expression(4)?;
            self.expect_symbol(b'=')?;
            let base = match &target {
                LValue::Variable(base) => Expr::Variable(base.clone()),
                _ => return self.error("word indirection requires a scalar base variable"),
            };
            return Ok(Statement::Assign(
                LValue::MemoryOffset(MemoryWidth::Word, base, offset),
                self.parse_expression(0)?,
            ));
        }

        if self.consume_symbol(b'?') {
            let offset = self.parse_expression(4)?;
            self.expect_symbol(b'=')?;
            let value = self.parse_expression(0)?;
            let base = match target {
                LValue::Variable(base) => Expr::Variable(base),
                _ => return self.error("byte access requires a scalar base variable"),
            };
            return Ok(Statement::Assign(LValue::MemoryByteAt(base, offset), value));
        }
        let append = self.consume_symbol(b'+');
        if append {
            self.expect_symbol(b'=')?;
        } else if !self.consume_symbol(b'=') {
            return self.error("expected '=' after variable name");
        }
        let value = self.parse_expression(0)?;
        let value = if append {
            let left = match &target {
                LValue::Variable(name) => Expr::Variable(name.clone()),
                _ => return self.error("+= is only supported for scalar variables"),
            };
            Expr::Binary(Box::new(left), BinaryOp::Add, Box::new(value))
        } else {
            value
        };
        Ok(Statement::Assign(target, value))
    }

    fn parse_lvalue(&mut self) -> Result<LValue, RuntimeError> {
        if self.consume_symbol(b'!') {
            return Ok(LValue::Memory(MemoryWidth::Word, self.parse_expression(4)?));
        }
        if self.consume_symbol(b'?') {
            return Ok(LValue::Memory(MemoryWidth::Byte, self.parse_expression(4)?));
        }
        let name = self.expect_identifier("variable name")?;
        if self.consume_symbol(b'(') {
            let index = self.parse_expression(0)?;
            self.expect_symbol(b')')?;
            Ok(LValue::ArrayElement(name, index))
        } else {
            Ok(LValue::Variable(name))
        }
    }

    fn parse_expression_list_until(&mut self, terminator: u8) -> Result<Vec<Expr>, RuntimeError> {
        if self.consume_symbol(terminator) {
            return Ok(Vec::new());
        }
        let mut values = vec![self.parse_expression(0)?];
        while self.consume_symbol(b',') {
            values.push(self.parse_expression(0)?);
        }
        if !self.peek_symbol(terminator) {
            return self.error("expected closing delimiter");
        }
        Ok(values)
    }

    fn parse_expression(&mut self, minimum_precedence: u8) -> Result<Expr, RuntimeError> {
        let mut left = self.parse_prefix()?;
        loop {
            if self.system_source && minimum_precedence <= 9 && self.consume_symbol(b'.') {
                let field = self.expect_identifier("record or enum member")?;
                left = Expr::Member(Box::new(left), field);
                continue;
            }
            let Some((operator, precedence, width)) = self.peek_binary_operator() else {
                break;
            };
            if precedence < minimum_precedence {
                break;
            }
            self.cursor += width;
            let right = self.parse_expression(precedence + 1)?;
            left = Expr::Binary(Box::new(left), operator, Box::new(right));
        }
        Ok(left)
    }

    fn parse_prefix(&mut self) -> Result<Expr, RuntimeError> {
        let token = self.next().clone();
        match token {
            Token::Number(value) => Ok(Expr::Number(value)),
            Token::Integer(value) => Ok(Expr::Integer(value)),
            Token::String(value) => Ok(Expr::String(value)),
            Token::Identifier(name) => {
                if self.consume_symbol(b'(') {
                    let index = self.parse_expression(0)?;
                    self.expect_symbol(b')')?;
                    Ok(Expr::ArrayElement(name, Box::new(index)))
                } else if self.system_source && self.consume_symbol(b'.') {
                    let field = self.expect_identifier("record or enum member")?;
                    Ok(Expr::Member(Box::new(Expr::Variable(name)), field))
                } else {
                    Ok(Expr::Variable(name))
                }
            }
            Token::Keyword(TOKEN_PTR) => Ok(Expr::Variable("PTR".into())),
            Token::Keyword(TOKEN_PAGE) => Ok(Expr::Variable("PAGE".into())),
            Token::Keyword(TOKEN_TIME) => Ok(Expr::Variable("TIME".into())),
            Token::Keyword(TOKEN_LOMEM) => Ok(Expr::Variable("LOMEM".into())),
            Token::Keyword(TOKEN_HIMEM) => Ok(Expr::Variable("HIMEM".into())),
            Token::Keyword(TOKEN_INKEY | TOKEN_RND) => {
                let Token::Keyword(token) = token else {
                    unreachable!()
                };
                let arguments = if self.consume_symbol(b'(') {
                    self.parse_call_arguments_after_open()?
                } else {
                    Vec::new()
                };
                Ok(Expr::Builtin(token, arguments))
            }
            Token::Keyword(
                TOKEN_ABS | TOKEN_COS | TOKEN_INT | TOKEN_LEN | TOKEN_LN | TOKEN_LOG | TOKEN_SQR
                | TOKEN_TAN | TOKEN_SIN | TOKEN_STR | TOKEN_ASC | TOKEN_VAL | TOKEN_CHR,
            ) => {
                let Token::Keyword(token) = token else {
                    unreachable!()
                };
                let arguments = if self.consume_symbol(b'(') {
                    self.parse_call_arguments_after_open()?
                } else {
                    vec![self.parse_expression(7)?]
                };
                Ok(Expr::Builtin(token, arguments))
            }
            Token::Keyword(TOKEN_INSTR | TOKEN_LEFT | TOKEN_MID | TOKEN_RIGHT | TOKEN_STRING) => {
                let Token::Keyword(token) = token else {
                    unreachable!()
                };
                Ok(Expr::Builtin(token, self.parse_call_arguments()?))
            }
            Token::Keyword(TOKEN_FN) => {
                let mut name = self.expect_named_routine("function name")?;
                let module = if self.system_source && self.consume_symbol(b'.') {
                    let symbol = self.expect_named_routine("imported function name")?;
                    let module = name;
                    name = symbol;
                    Some(module)
                } else {
                    None
                };
                let arguments = if self.consume_symbol(b'(') {
                    self.parse_expression_list_until(b')')?
                } else {
                    Vec::new()
                };
                if self.peek_symbol(b')') {
                    self.next();
                }
                if let Some(module) = module {
                    Ok(Expr::ImportedFunction {
                        module,
                        name,
                        arguments,
                    })
                } else {
                    Ok(Expr::UserFunction(name, arguments))
                }
            }
            Token::Keyword(TOKEN_NOT) | Token::Symbol(b'-') | Token::Symbol(b'+') => {
                let operator = match token {
                    Token::Keyword(TOKEN_NOT) => UnaryOp::Not,
                    Token::Symbol(b'-') => UnaryOp::Minus,
                    _ => UnaryOp::Plus,
                };
                Ok(Expr::Unary(operator, Box::new(self.parse_expression(7)?)))
            }
            Token::Symbol(b'!') => Ok(Expr::MemoryRead(
                MemoryWidth::Word,
                Box::new(self.parse_expression(7)?),
            )),
            Token::Symbol(b'?') => Ok(Expr::MemoryRead(
                MemoryWidth::Byte,
                Box::new(self.parse_expression(7)?),
            )),
            Token::Symbol(b'(') => {
                let expression = self.parse_expression(0)?;
                self.expect_symbol(b')')?;
                Ok(expression)
            }
            other => self.error(format!("expected expression, found {other:?}")),
        }
    }

    fn parse_call_arguments(&mut self) -> Result<Vec<Expr>, RuntimeError> {
        // In ARM BASIC source, the opening parenthesis is ordinary text. In
        // tokenized BASIC the string-function token carries that delimiter,
        // while the closing parenthesis remains in the token stream.
        if self.source_mode {
            self.expect_symbol(b'(')?;
        }
        self.parse_call_arguments_after_open()
    }

    fn parse_call_arguments_after_open(&mut self) -> Result<Vec<Expr>, RuntimeError> {
        let mut arguments = Vec::new();
        if self.consume_symbol(b')') {
            return Ok(arguments);
        }
        arguments.push(self.parse_expression(0)?);
        while self.consume_symbol(b',') {
            arguments.push(self.parse_expression(0)?);
        }
        self.expect_symbol(b')')?;
        Ok(arguments)
    }

    fn peek_binary_operator(&self) -> Option<(BinaryOp, u8, usize)> {
        match self.peek() {
            Token::Keyword(TOKEN_OR) => Some((BinaryOp::Or, 1, 1)),
            Token::Keyword(TOKEN_AND) => Some((BinaryOp::And, 2, 1)),
            Token::Symbol(b'=') => Some((BinaryOp::Equal, 3, 1)),
            Token::Symbol(b'<') if self.peek_n(1) == &Token::Symbol(b'<') => {
                Some((BinaryOp::ShiftLeft, 4, 2))
            }
            Token::Symbol(b'<') if self.peek_n(1) == &Token::Symbol(b'=') => {
                Some((BinaryOp::LessEqual, 3, 2))
            }
            Token::Symbol(b'>') if self.peek_n(1) == &Token::Symbol(b'=') => {
                Some((BinaryOp::GreaterEqual, 3, 2))
            }
            Token::Symbol(b'<') if self.peek_n(1) == &Token::Symbol(b'>') => {
                Some((BinaryOp::NotEqual, 3, 2))
            }
            Token::Symbol(b'<') => Some((BinaryOp::Less, 3, 1)),
            Token::Symbol(b'>') => Some((BinaryOp::Greater, 3, 1)),
            Token::Symbol(b'+') => Some((BinaryOp::Add, 4, 1)),
            Token::Symbol(b'-') => Some((BinaryOp::Subtract, 4, 1)),
            Token::Symbol(b'*') => Some((BinaryOp::Multiply, 5, 1)),
            Token::Symbol(b'/') => Some((BinaryOp::Divide, 5, 1)),
            Token::Keyword(TOKEN_DIV) => Some((BinaryOp::IntegerDivide, 5, 1)),
            Token::Keyword(TOKEN_MOD) => Some((BinaryOp::Modulo, 5, 1)),
            Token::Symbol(b'^') => Some((BinaryOp::Power, 6, 1)),
            _ => None,
        }
    }

    fn expect_line_reference(&mut self, description: &str) -> Result<u16, RuntimeError> {
        match self.next().clone() {
            Token::LineReference(target) => Ok(target),
            Token::Number(target)
                if self.source_mode
                    && target.is_finite()
                    && target >= 0.0
                    && target <= f64::from(u16::MAX)
                    && target.fract() == 0.0 =>
            {
                Ok(target as u16)
            }
            Token::Integer(target)
                if self.source_mode && (0..=i128::from(u16::MAX)).contains(&target) =>
            {
                Ok(target as u16)
            }
            _ => self.error(format!("expected {description}")),
        }
    }

    fn expect_identifier(&mut self, description: &str) -> Result<String, RuntimeError> {
        if let Token::Identifier(name) = self.next().clone() {
            Ok(name)
        } else {
            self.error(format!("expected {description}"))
        }
    }

    fn expect_routine_name(&mut self, description: &str) -> Result<String, RuntimeError> {
        if matches!(self.peek(), Token::Keyword(TOKEN_MODE)) {
            self.next();
            return Ok("MODE".into());
        }
        self.expect_named_routine(description)
    }

    fn expect_named_routine(&mut self, description: &str) -> Result<String, RuntimeError> {
        let leading_underscore = self.consume_symbol(b'_');
        match self.next().clone() {
            Token::Identifier(name) => Ok(if leading_underscore {
                format!("_{name}")
            } else {
                name
            }),
            other => self.error(format!("expected {description}, found {other:?}")),
        }
    }

    fn expect_symbol(&mut self, symbol: u8) -> Result<(), RuntimeError> {
        if self.consume_symbol(symbol) {
            Ok(())
        } else {
            self.error(format!("expected '{}'", symbol as char))
        }
    }

    fn consume_symbol(&mut self, symbol: u8) -> bool {
        if self.peek_symbol(symbol) {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    fn consume_keyword(&mut self, keyword: u8) -> bool {
        if self.peek_keyword(keyword) {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    fn peek_symbol(&self, symbol: u8) -> bool {
        self.peek() == &Token::Symbol(symbol)
    }

    fn peek_keyword(&self, keyword: u8) -> bool {
        self.peek() == &Token::Keyword(keyword)
    }

    fn is_end(&self) -> bool {
        self.peek() == &Token::End
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.cursor]
    }

    fn peek_n(&self, offset: usize) -> &Token {
        self.tokens
            .get(self.cursor + offset)
            .unwrap_or_else(|| self.tokens.last().expect("tokenizer always emits END"))
    }

    fn next(&mut self) -> &Token {
        let current = self.cursor;
        if !self.is_end() {
            self.cursor += 1;
        }
        &self.tokens[current]
    }

    fn error<T>(&self, message: impl AsRef<str>) -> Result<T, RuntimeError> {
        Err(syntax_error(self.line_number, message.as_ref()))
    }
}

fn pseudo_variable_for_assignment(token: u8) -> Option<&'static str> {
    match token {
        0xCF | TOKEN_PTR => Some("PTR"),
        0xD0 | TOKEN_PAGE => Some("PAGE"),
        TOKEN_TIME | 0xD1 => Some("TIME"),
        0xD2 | TOKEN_LOMEM => Some("LOMEM"),
        0xD3 | TOKEN_HIMEM => Some("HIMEM"),
        _ => None,
    }
}

fn syntax_error(line_number: u16, message: &str) -> RuntimeError {
    RuntimeError::Program(format!("line {line_number}: {message}"))
}

#[cfg(test)]
mod tests {
    use super::{Statement, TokenProfile, parse_program, parse_source};
    use crate::tokenized_basic::TokenizedBasicProgram;

    #[test]
    fn parses_dense_clocksp5_source_without_splitting_keyword_prefixed_names() {
        let clock = parse_source(include_str!("../../examples/clocksp5/ClockSP5.bas"))
            .expect("dense ClockSP5 source should parse");
        assert!(clock.procedures.contains_key("T"));
        assert!(clock.procedures.contains_key("S"));
        assert!(clock.functions.contains_key("B"));

        let tokenized =
            TokenizedBasicProgram::decode(include_bytes!("../../examples/clocksp5/ClockSP5.bbc"))
                .expect("ClockSP5 tokenized fixture should decode");
        let tokenized = parse_program(&tokenized, TokenProfile::ArmBasicV)
            .expect("ClockSP5 tokenized fixture should parse");
        assert_eq!(clock.instructions.len(), tokenized.instructions.len());
        for (source, tokenized) in clock.instructions.iter().zip(&tokenized.instructions) {
            assert_eq!(source.line_number, tokenized.line_number);
            assert_eq!(
                format!("{:?}", source.statement),
                format!("{:?}", tokenized.statement),
                "statement mismatch at BASIC line {}",
                source.line_number
            );
        }

        parse_source(include_str!("../../examples/wimp/two-windows/alpha.bas64"))
            .expect("BASIC64 identifiers such as definition% should remain intact");
    }

    #[test]
    fn desktop_and_filer_basic64_components_parse() {
        parse_source(include_str!("../../demo-volume/System/Desktop.bas64"))
            .expect("the editable desktop bootstrap should parse");
        parse_source(include_str!("../../demo-volume/System/Filer.bas64"))
            .expect("the editable Filer policy should parse");
    }

    #[test]
    fn parses_clocksp5_tokenized_program() {
        let program =
            TokenizedBasicProgram::decode(include_bytes!("../../examples/clocksp5/ClockSP5.bbc"))
                .expect("ClockSP5 fixture should decode");
        let parsed = parse_program(&program, TokenProfile::ArmBasicV)
            .expect("ClockSP5 token stream should parse");

        assert_eq!(parsed.instructions.len(), 335);
        assert!(parsed.procedures.contains_key("S"));
        assert!(parsed.procedures.contains_key("P"));
        assert!(parsed.functions.contains_key("B"));
        assert!(
            parsed
                .instructions
                .iter()
                .any(|instruction| { matches!(instruction.statement, Statement::Goto(60)) })
        );
    }

    #[test]
    fn parses_shared_boundary_print_end_core_fixture() {
        let program = TokenizedBasicProgram::decode(include_bytes!(
            "../../examples/tokenized-compat/classic-core-smoke.bbc"
        ))
        .expect("shared-boundary core fixture should decode");
        let parsed = parse_program(&program, TokenProfile::SharedBoundaryCore)
            .expect("common PRINT and END tokens should parse");

        assert_eq!(parsed.instructions.len(), 2);
        assert!(matches!(
            parsed.instructions[0].statement,
            Statement::Print(_)
        ));
        assert!(matches!(parsed.instructions[1].statement, Statement::End));
    }

    #[test]
    fn shared_boundary_core_rejects_tokens_outside_its_subset() {
        let bytes = [0x0D, 0x00, 0x0A, 0x05, 0xC7, 0x0D, 0xFF];
        let program = TokenizedBasicProgram::decode(&bytes).expect("shared record should decode");

        let error = parse_program(&program, TokenProfile::SharedBoundaryCore)
            .expect_err("unsupported classic tokens must not be parsed as ARM BASIC V");
        assert!(error.to_string().contains("token &C7"));
    }

    #[test]
    fn shared_boundary_core_rejects_non_literal_statements() {
        let bytes = [0x0D, 0x00, 0x0A, 0x07, b'A', b'=', b'1', 0x0D, 0xFF];
        let program = TokenizedBasicProgram::decode(&bytes).expect("shared record should decode");

        let error = parse_program(&program, TokenProfile::SharedBoundaryCore)
            .expect_err("non-core statements must be rejected for now");
        assert!(
            error
                .to_string()
                .contains("literal-string PRINT and END statements")
        );
    }
}
