//! Experimental backend-neutral lowering for a small integer BASIC subset.
//!
//! This interface is intentionally narrow. It accepts ARM BASIC V tokenized
//! programs containing integer scalar assignments, addition, nested
//! `REPEAT`/`UNTIL` loops using `>`, one final integer-variable `PRINT`, and
//! `END`.
//! Unsupported statements and expressions fail explicitly so a backend never
//! silently compiles only part of a program.

use std::collections::HashMap;

use crate::{
    error::RuntimeError,
    tokenized_basic::{TokenizedBasicProgram, TokenizedBasicRecordLayout},
};

use super::parser::{
    self, BinaryOp, Expr, LValue, ParsedProgram, PrintItem, Statement, TokenProfile,
};

#[derive(Clone, Debug, PartialEq)]
pub struct IntegerProgram {
    /// BASIC variable names in the same order as local indices in this IR.
    pub locals: Vec<String>,
    pub statements: Vec<IntegerStatement>,
    /// Local referenced by the program's final integer `PRINT` statement.
    pub result_local: u32,
    pub result_line: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub enum IntegerStatement {
    Assign {
        line: u16,
        local: u32,
        value: IntegerExpression,
    },
    RepeatUntil {
        line: u16,
        condition_line: u16,
        body: Vec<IntegerStatement>,
        condition: IntegerCondition,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum IntegerExpression {
    Constant(i32),
    Local(u32),
    Add(Box<IntegerExpression>, Box<IntegerExpression>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum IntegerCondition {
    GreaterThan(IntegerExpression, IntegerExpression),
}

/// Parse an ARM BASIC V tokenized program and lower the supported integer
/// subset into a backend-neutral representation suitable for Cranelift or
/// another code generator.
pub fn lower_integer_program(
    program: &TokenizedBasicProgram,
) -> Result<IntegerProgram, RuntimeError> {
    if matches!(
        program.record_layout,
        Some(TokenizedBasicRecordLayout::SharedBoundaryCarriageReturn)
    ) {
        return Err(unsupported(0, "expected an ARM BASIC V token stream"));
    }

    let parsed = parser::parse_program(program, TokenProfile::ArmBasicV)?;
    Lowerer::new().lower(&parsed)
}

struct Lowerer {
    locals: Vec<String>,
    local_indices: HashMap<String, u32>,
    result: Option<(u16, u32)>,
    saw_end: bool,
}

impl Lowerer {
    fn new() -> Self {
        Self {
            locals: Vec::new(),
            local_indices: HashMap::new(),
            result: None,
            saw_end: false,
        }
    }

    fn lower(mut self, parsed: &ParsedProgram) -> Result<IntegerProgram, RuntimeError> {
        let mut cursor = 0;
        let statements = self.lower_block(&parsed.instructions, &mut cursor, false)?;
        if !self.saw_end {
            return Err(unsupported(0, "program must end with END"));
        }
        if parsed.instructions[cursor..]
            .iter()
            .any(|instruction| !matches!(instruction.statement, Statement::NoOp))
        {
            return Err(unsupported(0, "executable statements after END are unsupported"));
        }
        let (result_line, result_local) = self
            .result
            .ok_or_else(|| unsupported(0, "program must print one integer result"))?;

        Ok(IntegerProgram {
            locals: self.locals,
            statements,
            result_local,
            result_line,
        })
    }

    fn lower_block(
        &mut self,
        instructions: &[parser::LocatedStatement],
        cursor: &mut usize,
        nested: bool,
    ) -> Result<Vec<IntegerStatement>, RuntimeError> {
        let mut statements = Vec::new();

        while *cursor < instructions.len() {
            let instruction = &instructions[*cursor];
            let line = instruction.line_number;
            let statement = instruction.statement.clone();

            match statement {
                Statement::NoOp => *cursor += 1,
                Statement::Assign(target, value) => {
                    if self.result.is_some() {
                        return Err(unsupported(line, "statements after PRINT are unsupported"));
                    }
                    let LValue::Variable(name) = target else {
                        return Err(unsupported(
                            line,
                            "only scalar integer assignments are supported",
                        ));
                    };
                    let local = self.local(&name, line)?;
                    let value = self.expression(&value, line)?;
                    statements.push(IntegerStatement::Assign { line, local, value });
                    *cursor += 1;
                }
                Statement::Repeat => {
                    if self.result.is_some() {
                        return Err(unsupported(line, "statements after PRINT are unsupported"));
                    }
                    *cursor += 1;
                    let body = self.lower_block(instructions, cursor, true)?;
                    let until = instructions
                        .get(*cursor)
                        .ok_or_else(|| unsupported(line, "REPEAT loop has no matching UNTIL"))?;
                    let Statement::Until(expression) = &until.statement else {
                        return Err(unsupported(line, "REPEAT loop has no matching UNTIL"));
                    };
                    let condition = self.condition(expression, until.line_number)?;
                    statements.push(IntegerStatement::RepeatUntil {
                        line,
                        condition_line: until.line_number,
                        body,
                        condition,
                    });
                    *cursor += 1;
                }
                Statement::Until(_) => {
                    if !nested {
                        return Err(unsupported(line, "UNTIL without a matching REPEAT"));
                    }
                    // The caller lowers this terminator as the loop condition.
                    return Ok(statements);
                }
                Statement::Print(items) => {
                    if nested || self.result.is_some() {
                        return Err(unsupported(line, "only one final PRINT is supported"));
                    }
                    let [PrintItem::Value(Expr::Variable(name))] = items.as_slice() else {
                        return Err(unsupported(
                            line,
                            "PRINT must contain exactly one integer variable",
                        ));
                    };
                    let result_local = self.local(name, line)?;
                    self.result = Some((line, result_local));
                    *cursor += 1;
                }
                Statement::End => {
                    if nested {
                        return Err(unsupported(line, "END inside a REPEAT loop is unsupported"));
                    }
                    if self.result.is_none() {
                        return Err(unsupported(line, "END must follow the result PRINT"));
                    }
                    self.saw_end = true;
                    *cursor += 1;
                    return Ok(statements);
                }
                _ => {
                    return Err(unsupported(
                        line,
                        "statement is outside the integer compiler subset",
                    ));
                }
            }
        }

        if nested {
            return Err(unsupported(0, "REPEAT loop has no matching UNTIL"));
        }
        Ok(statements)
    }

    fn local(&mut self, name: &str, line: u16) -> Result<u32, RuntimeError> {
        if !name.ends_with('%') {
            return Err(unsupported(line, "only % integer variables are supported"));
        }
        let name = name.to_ascii_uppercase();
        if let Some(index) = self.local_indices.get(&name) {
            return Ok(*index);
        }
        let index = u32::try_from(self.locals.len())
            .map_err(|_| unsupported(line, "too many local variables"))?;
        self.locals.push(name.clone());
        self.local_indices.insert(name, index);
        Ok(index)
    }

    fn expression(
        &mut self,
        expression: &Expr,
        line: u16,
    ) -> Result<IntegerExpression, RuntimeError> {
        match expression {
            Expr::Number(number)
                if number.is_finite()
                    && number.fract() == 0.0
                    && *number >= f64::from(i32::MIN)
                    && *number <= f64::from(i32::MAX) =>
            {
                Ok(IntegerExpression::Constant(*number as i32))
            }
            Expr::Variable(name) => Ok(IntegerExpression::Local(self.local(name, line)?)),
            Expr::Binary(left, BinaryOp::Add, right) => Ok(IntegerExpression::Add(
                Box::new(self.expression(left, line)?),
                Box::new(self.expression(right, line)?),
            )),
            Expr::Number(_) => Err(unsupported(
                line,
                "integer literals must be integral and fit signed 32-bit",
            )),
            _ => Err(unsupported(
                line,
                "expression is outside the integer compiler subset",
            )),
        }
    }

    fn condition(
        &mut self,
        expression: &Expr,
        line: u16,
    ) -> Result<IntegerCondition, RuntimeError> {
        let Expr::Binary(left, BinaryOp::Greater, right) = expression else {
            return Err(unsupported(line, "only > loop conditions are supported"));
        };
        Ok(IntegerCondition::GreaterThan(
            self.expression(left, line)?,
            self.expression(right, line)?,
        ))
    }
}

fn unsupported(line: u16, message: &str) -> RuntimeError {
    RuntimeError::Program(if line == 0 {
        format!("BASIC compiler subset: {message}")
    } else {
        format!("BASIC compiler subset at line {line}: {message}")
    })
}
