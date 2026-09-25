use std::collections::BTreeMap;

use crate::{
    error::RuntimeError,
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
const TOKEN_TO: u8 = 0xB8;
const TOKEN_VAL: u8 = 0xBC;
const TOKEN_CHR: u8 = 0xBD;
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

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Expr {
    Number(f64),
    String(Vec<u8>),
    Variable(String),
    ArrayElement(String, Box<Expr>),
    Unary(UnaryOp, Box<Expr>),
    Binary(Box<Expr>, BinaryOp, Box<Expr>),
    Builtin(u8, Vec<Expr>),
    UserFunction(String, Vec<Expr>),
    MemoryRead(MemoryWidth, Box<Expr>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum UnaryOp {
    Plus,
    Minus,
    Not,
}

#[derive(Clone, Copy, Debug, PartialEq)]
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

#[derive(Clone, Copy, Debug, PartialEq)]
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
    DefineProcedure(String, Vec<String>),
    DefineFunction(String, Vec<String>),
    Sys {
        name: Vec<u8>,
        arguments: Vec<Option<Expr>>,
        results: Vec<String>,
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
    let mut parsed = ParsedProgram::default();
    for line in &program.lines {
        if profile == TokenProfile::SharedBoundaryCore {
            validate_shared_boundary_core(line)?;
        }
        parsed
            .line_entries
            .insert(line.number, parsed.instructions.len());
        let statements = parse_line(line)?;
        for statement in statements {
            if profile == TokenProfile::SharedBoundaryCore {
                validate_shared_boundary_statement(&statement, line.number)?;
            }
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
                line_number: line.number,
                statement,
            });
        }
    }
    Ok(parsed)
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

fn parse_line(line: &TokenizedBasicLine) -> Result<Vec<Statement>, RuntimeError> {
    if line.bytes.first() == Some(&b'*') {
        return Ok(vec![Statement::StarCommand(line.bytes[1..].to_vec())]);
    }

    let mut parser = Parser::new(&line.bytes, line.number)?;
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
            return parser.error("expected ':' or end of line");
        }
    }
    Ok(statements)
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Number(f64),
    String(Vec<u8>),
    Identifier(String),
    Keyword(u8),
    Sys,
    LineReference(u16),
    Symbol(u8),
    End,
}

struct Lexer<'a> {
    bytes: &'a [u8],
    offset: usize,
    line_number: u16,
}

impl<'a> Lexer<'a> {
    fn new(bytes: &'a [u8], line_number: u16) -> Self {
        Self {
            bytes,
            offset: 0,
            line_number,
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
        if byte.is_ascii_digit() {
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
        Ok(Token::Number(f64::from(value)))
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
        let value = text
            .parse::<f64>()
            .map_err(|_| syntax_error(self.line_number, "invalid numeric literal"))?;
        Ok(Token::Number(value))
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
        if matches!(self.bytes.get(self.offset), Some(b'$' | b'%')) {
            self.offset += 1;
        }
        let value = std::str::from_utf8(&self.bytes[start..self.offset])
            .map_err(|_| syntax_error(self.line_number, "invalid variable name"))?
            .to_ascii_uppercase();
        Ok(Token::Identifier(value))
    }
}

struct Parser {
    tokens: Vec<Token>,
    cursor: usize,
    line_number: u16,
}

impl Parser {
    fn new(bytes: &[u8], line_number: u16) -> Result<Self, RuntimeError> {
        Ok(Self {
            tokens: Lexer::new(bytes, line_number).tokenize()?,
            cursor: 0,
            line_number,
        })
    }

    fn parse_statement(&mut self) -> Result<Statement, RuntimeError> {
        let token = self.next().clone();
        match token {
            Token::Keyword(TOKEN_IF) => self.parse_if(),
            Token::Keyword(TOKEN_PRINT) => self.parse_print(),
            Token::Keyword(TOKEN_MODE) => Ok(Statement::Mode(self.parse_expression(0)?)),
            Token::Keyword(TOKEN_VDU) => self.parse_vdu(),
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
                let target = if matches!(self.peek(), Token::LineReference(_)) {
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
                let name = self.expect_routine_name("procedure name")?;
                let arguments = if self.consume_symbol(b'(') {
                    self.parse_call_arguments_after_open()?
                } else {
                    Vec::new()
                };
                Ok(Statement::ProcedureCall(name, arguments))
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
            Token::Identifier(name) => self.parse_assignment_or_function_call(name),
            Token::Keyword(TOKEN_MID) => self.parse_string_slice_assignment(),
            Token::LineReference(target) => Ok(Statement::Goto(target)),
            Token::End => self.error("expected a statement"),
            other => self.error(format!("unexpected token {other:?} at statement start")),
        }
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
            let name = self.expect_identifier("function name")?;
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
            Token::String(value) => Ok(Expr::String(value)),
            Token::Identifier(name) => {
                if self.consume_symbol(b'(') {
                    let index = self.parse_expression(0)?;
                    self.expect_symbol(b')')?;
                    Ok(Expr::ArrayElement(name, Box::new(index)))
                } else {
                    Ok(Expr::Variable(name))
                }
            }
            Token::Keyword(TOKEN_PTR) => Ok(Expr::Variable("PTR".into())),
            Token::Keyword(TOKEN_PAGE) => Ok(Expr::Variable("PAGE".into())),
            Token::Keyword(TOKEN_TIME) => Ok(Expr::Variable("TIME".into())),
            Token::Keyword(TOKEN_LOMEM) => Ok(Expr::Variable("LOMEM".into())),
            Token::Keyword(TOKEN_HIMEM) => Ok(Expr::Variable("HIMEM".into())),
            Token::Keyword(TOKEN_INKEY) => {
                let arguments = if self.consume_symbol(b'(') {
                    self.parse_call_arguments_after_open()?
                } else {
                    Vec::new()
                };
                Ok(Expr::Builtin(TOKEN_INKEY, arguments))
            }
            Token::Keyword(
                TOKEN_ABS | TOKEN_COS | TOKEN_INT | TOKEN_LEN | TOKEN_LN | TOKEN_LOG | TOKEN_SQR
                | TOKEN_TAN | TOKEN_STR | TOKEN_ASC | TOKEN_VAL | TOKEN_CHR,
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
                let name = self.expect_identifier("function name")?;
                let arguments = if self.consume_symbol(b'(') {
                    self.parse_expression_list_until(b')')?
                } else {
                    Vec::new()
                };
                if self.peek_symbol(b')') {
                    self.next();
                }
                Ok(Expr::UserFunction(name, arguments))
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
        if let Token::LineReference(target) = self.next().clone() {
            Ok(target)
        } else {
            self.error(format!("expected encoded {description}"))
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
        match self.next().clone() {
            Token::Identifier(name) => Ok(name),
            Token::Keyword(TOKEN_MODE) => Ok("MODE".into()),
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
        0xCF => Some("PTR"),
        0xD0 => Some("PAGE"),
        0xD1 => Some("TIME"),
        0xD2 => Some("LOMEM"),
        0xD3 => Some("HIMEM"),
        _ => None,
    }
}

fn syntax_error(line_number: u16, message: &str) -> RuntimeError {
    RuntimeError::Program(format!("line {line_number}: {message}"))
}

#[cfg(test)]
mod tests {
    use super::{Statement, TokenProfile, parse_program};
    use crate::tokenized_basic::TokenizedBasicProgram;

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
