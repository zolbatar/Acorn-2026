use std::collections::BTreeMap;

use crate::{
    error::RuntimeError,
    tokenized_basic::{TokenizedBasicLine, TokenizedBasicProgram, decode_line_reference},
};

const TOKEN_AND: u8 = 0x80;
const TOKEN_DIV: u8 = 0x81;
const TOKEN_MOD: u8 = 0x83;
const TOKEN_OR: u8 = 0x84;
const TOKEN_SPC: u8 = 0x89;
const TOKEN_THEN: u8 = 0x8C;
const TOKEN_ELSE: u8 = 0x8B;
const TOKEN_STEP: u8 = 0x88;
const TOKEN_PTR: u8 = 0x8F;
const TOKEN_PAGE: u8 = 0x90;
const TOKEN_TIME: u8 = 0x91;
const TOKEN_LOMEM: u8 = 0x92;
const TOKEN_HIMEM: u8 = 0x93;
const TOKEN_ABS: u8 = 0x94;
const TOKEN_COS: u8 = 0x9B;
const TOKEN_FN: u8 = 0xA4;
const TOKEN_INKEY: u8 = 0xA6;
const TOKEN_INT: u8 = 0xA8;
const TOKEN_LEN: u8 = 0xA9;
const TOKEN_LN: u8 = 0xAA;
const TOKEN_LOG: u8 = 0xAB;
const TOKEN_NOT: u8 = 0xAC;
const TOKEN_SQR: u8 = 0xB6;
const TOKEN_TAN: u8 = 0xB7;
const TOKEN_TO: u8 = 0xB8;
const TOKEN_LEFT: u8 = 0xC0;
const TOKEN_MID: u8 = 0xC1;
const TOKEN_RIGHT: u8 = 0xC2;
const TOKEN_STR: u8 = 0xC3;
const TOKEN_STRING: u8 = 0xC4;
const TOKEN_CALL: u8 = 0xD6;
const TOKEN_DATA: u8 = 0xDC;
const TOKEN_DEF: u8 = 0xDD;
const TOKEN_END: u8 = 0xE0;
const TOKEN_ENDPROC: u8 = 0xE1;
const TOKEN_DIM: u8 = 0xE2;
const TOKEN_FOR: u8 = 0xE3;
const TOKEN_GOSUB: u8 = 0xE4;
const TOKEN_GOTO: u8 = 0xE5;
const TOKEN_IF: u8 = 0xE7;
const TOKEN_INPUT: u8 = 0xE8;
const TOKEN_NEXT: u8 = 0xED;
const TOKEN_PRINT: u8 = 0xF1;
const TOKEN_PROC: u8 = 0xF2;
const TOKEN_READ: u8 = 0xF3;
const TOKEN_REM: u8 = 0xF4;
const TOKEN_REPEAT: u8 = 0xF5;
const TOKEN_RESTORE: u8 = 0xF7;
const TOKEN_RETURN: u8 = 0xF8;
const TOKEN_UNTIL: u8 = 0xFD;

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
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PrintItem {
    Value(Expr),
    Spaces(Expr),
    Comma,
    Semicolon,
    NewLine,
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
    If(Expr, Vec<Statement>, Vec<Statement>),
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
    ProcedureCall(String),
    DefineProcedure(String),
    DefineFunction(String),
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
    pub procedures: std::collections::HashMap<String, usize>,
    pub functions: std::collections::HashMap<String, usize>,
}

pub(crate) fn parse_program(
    program: &TokenizedBasicProgram,
) -> Result<ParsedProgram, RuntimeError> {
    let mut parsed = ParsedProgram::default();
    for line in &program.lines {
        parsed
            .line_entries
            .insert(line.number, parsed.instructions.len());
        let statements = parse_line(line)?;
        for statement in statements {
            let index = parsed.instructions.len();
            match &statement {
                Statement::DefineProcedure(name) => {
                    parsed.procedures.insert(name.clone(), index + 1);
                }
                Statement::DefineFunction(name) => {
                    parsed.functions.insert(name.clone(), index + 1);
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
        if byte >= 0x7F {
            self.offset += 1;
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
            .is_some_and(u8::is_ascii_alphanumeric)
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
            Token::Keyword(TOKEN_PROC) => Ok(Statement::ProcedureCall(
                self.expect_identifier("procedure name")?,
            )),
            Token::Keyword(TOKEN_DEF) => self.parse_definition(),
            Token::Keyword(TOKEN_RETURN) => Ok(Statement::Return),
            Token::Keyword(TOKEN_ENDPROC) => Ok(Statement::EndProcedure),
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
            Token::Symbol(b'!') => {
                let address = self.parse_expression(4)?;
                self.expect_symbol(b'=')?;
                Ok(Statement::Assign(
                    LValue::Memory(MemoryWidth::Word, address),
                    self.parse_expression(0)?,
                ))
            }
            Token::Identifier(name) => self.parse_assignment_or_function_call(name),
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
            } else {
                items.push(PrintItem::Value(self.parse_expression(0)?));
            }
        }
        Ok(Statement::Print(items))
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
            Ok(Statement::DefineProcedure(
                self.expect_identifier("procedure name")?,
            ))
        } else if self.consume_keyword(TOKEN_FN) {
            Ok(Statement::DefineFunction(
                self.expect_identifier("function name")?,
            ))
        } else {
            self.error("expected PROC or FN after DEF")
        }
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
        if !self.consume_symbol(b'=') {
            return self.error("expected '=' after variable name");
        }
        Ok(Statement::Assign(target, self.parse_expression(0)?))
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
            Token::Keyword(TOKEN_INKEY) => Ok(Expr::Variable("INKEY".into())),
            Token::Keyword(
                TOKEN_ABS | TOKEN_COS | TOKEN_INT | TOKEN_LEN | TOKEN_LN | TOKEN_LOG | TOKEN_SQR
                | TOKEN_TAN | TOKEN_STR,
            ) => {
                let Token::Keyword(token) = token else {
                    unreachable!()
                };
                Ok(Expr::Builtin(token, vec![self.parse_expression(7)?]))
            }
            Token::Keyword(TOKEN_LEFT | TOKEN_MID | TOKEN_RIGHT | TOKEN_STRING) => {
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

    fn peek_binary_operator(&self) -> Option<(BinaryOp, u8, usize)> {
        match self.peek() {
            Token::Keyword(TOKEN_OR) => Some((BinaryOp::Or, 1, 1)),
            Token::Keyword(TOKEN_AND) => Some((BinaryOp::And, 2, 1)),
            Token::Symbol(b'=') => Some((BinaryOp::Equal, 3, 1)),
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
    use super::{Statement, parse_program};
    use crate::tokenized_basic::TokenizedBasicProgram;

    #[test]
    fn parses_clocksp5_tokenized_program() {
        let program =
            TokenizedBasicProgram::decode(include_bytes!("../../examples/clocksp5/ClockSP5.bbc"))
                .expect("ClockSP5 fixture should decode");
        let parsed = parse_program(&program).expect("ClockSP5 token stream should parse");

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
}
