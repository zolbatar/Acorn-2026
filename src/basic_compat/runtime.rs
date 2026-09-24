use std::{collections::HashMap, time::Instant};

use crate::{
    error::RuntimeError,
    memory::{GUEST_MEMORY_BASE, GUEST_MEMORY_SIZE, Task},
    swi::{OS_NEW_LINE, OS_READ_LINE, SwiContext, SwiDispatcher},
};

use super::parser::{
    BinaryOp, DimDeclaration, Expr, LValue, MemoryWidth, ParsedProgram, PrintItem, Statement,
    UnaryOp,
};

const INPUT_BUFFER: u32 = GUEST_MEMORY_BASE + 0x3000;
const INPUT_BUFFER_SIZE: u32 = 4096;
const FIRST_HEAP_ADDRESS: u32 = GUEST_MEMORY_BASE + 0x8000;
const MAX_EXECUTION_STEPS: usize = 1_000_000_000;
const PRINT_ZONE_WIDTH: usize = 14;

#[derive(Clone, Debug, PartialEq)]
enum Value {
    Number(f64),
    String(Vec<u8>),
}

impl Value {
    fn number(&self, line: u16) -> Result<f64, RuntimeError> {
        match self {
            Self::Number(value) => Ok(*value),
            Self::String(_) => Err(program_error(line, "expected a numeric expression")),
        }
    }

    fn string(&self, line: u16) -> Result<&[u8], RuntimeError> {
        match self {
            Self::String(value) => Ok(value),
            Self::Number(_) => Err(program_error(line, "expected a string expression")),
        }
    }
}

#[derive(Clone, Copy)]
enum ReturnKind {
    Procedure,
    Subroutine,
}

#[derive(Clone, Copy)]
struct ReturnFrame {
    kind: ReturnKind,
    address: usize,
}

struct ForFrame {
    variable: String,
    limit: f64,
    step: f64,
    body_address: usize,
    next_address: usize,
}

enum Flow {
    Next,
    Jump(usize),
    Stop,
}

pub(super) struct Interpreter {
    program: ParsedProgram,
    variables: HashMap<String, Value>,
    arrays: HashMap<String, Vec<Value>>,
    data: Vec<(u16, Expr)>,
    data_cursor: usize,
    returns: Vec<ReturnFrame>,
    for_loops: Vec<ForFrame>,
    repeat_loops: Vec<usize>,
    for_pairs: HashMap<usize, usize>,
    started: Instant,
    steps: usize,
    print_column: usize,
    next_heap_address: u32,
}

impl Interpreter {
    pub(super) fn new(program: ParsedProgram) -> Self {
        let data = program
            .instructions
            .iter()
            .filter_map(|instruction| match &instruction.statement {
                Statement::Data(values) => Some(
                    values
                        .iter()
                        .cloned()
                        .map(|value| (instruction.line_number, value)),
                ),
                _ => None,
            })
            .flatten()
            .collect();
        let for_pairs = match_for_loops(&program);

        Self {
            program,
            variables: HashMap::new(),
            arrays: HashMap::new(),
            data,
            data_cursor: 0,
            returns: Vec::new(),
            for_loops: Vec::new(),
            repeat_loops: Vec::new(),
            for_pairs,
            started: Instant::now(),
            steps: 0,
            print_column: 0,
            next_heap_address: FIRST_HEAP_ADDRESS,
        }
    }

    pub(super) fn run(
        &mut self,
        task: &mut Task,
        dispatcher: &mut SwiDispatcher,
    ) -> Result<(), RuntimeError> {
        let mut address = 0_usize;
        while address < self.program.instructions.len() {
            self.steps += 1;
            if self.steps > MAX_EXECUTION_STEPS {
                let line = self.program.instructions[address].line_number;
                return Err(program_error(line, "execution step limit reached"));
            }

            let instruction = self.program.instructions[address].clone();
            match self.execute_statement(
                &instruction.statement,
                address,
                instruction.line_number,
                task,
                dispatcher,
            )? {
                Flow::Next => address += 1,
                Flow::Jump(destination) => address = destination,
                Flow::Stop => break,
            }
        }
        Ok(())
    }

    fn execute_statement(
        &mut self,
        statement: &Statement,
        address: usize,
        line: u16,
        task: &mut Task,
        dispatcher: &mut SwiDispatcher,
    ) -> Result<Flow, RuntimeError> {
        match statement {
            Statement::Assign(target, expression) => {
                let value = self.evaluate(expression, line, task)?;
                self.assign(target, value, line, task)?;
                Ok(Flow::Next)
            }
            Statement::Input(target) => {
                let value = self.read_input(line, task, dispatcher)?;
                self.assign(target, value, line, task)?;
                Ok(Flow::Next)
            }
            Statement::Print(items) => {
                self.print(items, line, task, dispatcher)?;
                Ok(Flow::Next)
            }
            Statement::If(condition, then_body, else_body) => {
                let selected = if self.evaluate(condition, line, task)?.number(line)? != 0.0 {
                    then_body
                } else {
                    else_body
                };
                for nested in selected {
                    match self.execute_statement(nested, address, line, task, dispatcher)? {
                        Flow::Next => {}
                        flow => return Ok(flow),
                    }
                }
                Ok(Flow::Next)
            }
            Statement::Goto(target) => self.jump_to_line(*target, line),
            Statement::Gosub(target) => {
                let destination = self.line_address(*target, line)?;
                self.returns.push(ReturnFrame {
                    kind: ReturnKind::Subroutine,
                    address: address + 1,
                });
                Ok(Flow::Jump(destination))
            }
            Statement::Dim(declarations) => {
                for declaration in declarations {
                    self.dim(declaration, line, task)?;
                }
                Ok(Flow::Next)
            }
            Statement::Read(targets) => {
                for target in targets {
                    let Some((_, expression)) = self.data.get(self.data_cursor).cloned() else {
                        return Err(program_error(line, "READ passed the end of DATA"));
                    };
                    self.data_cursor += 1;
                    let value = self.evaluate(&expression, line, task)?;
                    self.assign(target, value, line, task)?;
                }
                Ok(Flow::Next)
            }
            Statement::Data(_) | Statement::NoOp | Statement::DefineProcedure(_) => Ok(Flow::Next),
            Statement::Restore(target) => {
                self.data_cursor = match target {
                    Some(line) => self.data.partition_point(|(data_line, _)| data_line < line),
                    None => 0,
                };
                Ok(Flow::Next)
            }
            Statement::For {
                variable,
                start,
                end,
                step,
            } => self.start_for(variable, start, end, step.as_ref(), address, line, task),
            Statement::Next(variable) => self.next_for(variable.as_deref(), address, line),
            Statement::Repeat => {
                self.repeat_loops.push(address + 1);
                Ok(Flow::Next)
            }
            Statement::Until(condition) => {
                let Some(repeat_address) = self.repeat_loops.last().copied() else {
                    return Err(program_error(line, "UNTIL has no matching REPEAT"));
                };
                if self.evaluate(condition, line, task)?.number(line)? != 0.0 {
                    self.repeat_loops.pop();
                    Ok(Flow::Next)
                } else {
                    Ok(Flow::Jump(repeat_address))
                }
            }
            Statement::ProcedureCall(name) => {
                let Some(destination) = self.program.procedures.get(name).copied() else {
                    return Err(program_error(
                        line,
                        format!("unknown procedure PROC {name}"),
                    ));
                };
                self.returns.push(ReturnFrame {
                    kind: ReturnKind::Procedure,
                    address: address + 1,
                });
                Ok(Flow::Jump(destination))
            }
            Statement::DefineFunction(_) => Ok(Flow::Next),
            Statement::FunctionReturn(_) => Err(program_error(
                line,
                "function return was reached outside a function call",
            )),
            Statement::Return => self.return_from(ReturnKind::Subroutine, line),
            Statement::EndProcedure => self.return_from(ReturnKind::Procedure, line),
            Statement::End => Ok(Flow::Stop),
            Statement::Call(_) => Err(program_error(
                line,
                "CALL requires an ARM compatibility service not available in the hosted profile",
            )),
            Statement::StarCommand(command) => {
                self.execute_star_command(command, line)?;
                Ok(Flow::Next)
            }
        }
    }

    fn jump_to_line(&self, target: u16, line: u16) -> Result<Flow, RuntimeError> {
        Ok(Flow::Jump(self.line_address(target, line)?))
    }

    fn line_address(&self, target: u16, line: u16) -> Result<usize, RuntimeError> {
        self.program
            .line_entries
            .get(&target)
            .copied()
            .ok_or_else(|| program_error(line, format!("line {target} does not exist")))
    }

    fn return_from(&mut self, expected: ReturnKind, line: u16) -> Result<Flow, RuntimeError> {
        let Some(frame) = self.returns.last().copied() else {
            return Err(program_error(line, "RETURN or ENDPROC has no active call"));
        };
        let matches = matches!(
            (expected, frame.kind),
            (ReturnKind::Procedure, ReturnKind::Procedure)
                | (ReturnKind::Subroutine, ReturnKind::Subroutine)
        );
        if !matches {
            return Err(program_error(line, "mismatched RETURN and ENDPROC"));
        }
        self.returns.pop();
        Ok(Flow::Jump(frame.address))
    }

    fn start_for(
        &mut self,
        variable: &str,
        start: &Expr,
        end: &Expr,
        step: Option<&Expr>,
        address: usize,
        line: u16,
        task: &mut Task,
    ) -> Result<Flow, RuntimeError> {
        let start_value = self.evaluate(start, line, task)?.number(line)?;
        let end_value = self.evaluate(end, line, task)?.number(line)?;
        let step_value = match step {
            Some(expression) => self.evaluate(expression, line, task)?.number(line)?,
            None => 1.0,
        };
        if step_value == 0.0 {
            return Err(program_error(line, "FOR STEP cannot be zero"));
        }
        let next_address = self
            .for_pairs
            .get(&address)
            .copied()
            .ok_or_else(|| program_error(line, "FOR has no matching NEXT"))?;
        self.set_variable(variable, Value::Number(start_value), line)?;

        let enters_loop = if step_value > 0.0 {
            start_value <= end_value
        } else {
            start_value >= end_value
        };
        if !enters_loop {
            return Ok(Flow::Jump(next_address + 1));
        }

        self.for_loops.push(ForFrame {
            variable: variable.to_owned(),
            limit: end_value,
            step: step_value,
            body_address: address + 1,
            next_address,
        });
        Ok(Flow::Next)
    }

    fn next_for(
        &mut self,
        requested_variable: Option<&str>,
        address: usize,
        line: u16,
    ) -> Result<Flow, RuntimeError> {
        let Some(frame) = self.for_loops.last() else {
            return Err(program_error(line, "NEXT has no active FOR"));
        };
        if frame.next_address != address {
            return Err(program_error(line, "NEXT does not match the active FOR"));
        }
        if requested_variable.is_some_and(|name| name != frame.variable) {
            return Err(program_error(line, "NEXT variable does not match FOR"));
        }

        let variable = frame.variable.clone();
        let step = frame.step;
        let limit = frame.limit;
        let body_address = frame.body_address;
        let current = self.get_variable(&variable).number(line)?;
        let next_value = current + step;
        self.set_variable(&variable, Value::Number(next_value), line)?;
        let continues = if step > 0.0 {
            next_value <= limit
        } else {
            next_value >= limit
        };
        if continues {
            Ok(Flow::Jump(body_address))
        } else {
            self.for_loops.pop();
            Ok(Flow::Next)
        }
    }

    fn dim(
        &mut self,
        declaration: &DimDeclaration,
        line: u16,
        task: &mut Task,
    ) -> Result<(), RuntimeError> {
        if declaration.dimensions.is_empty() {
            self.variables
                .insert(declaration.name.clone(), default_value(&declaration.name));
            return Ok(());
        }
        let mut length = 1_usize;
        for dimension in &declaration.dimensions {
            let upper_bound = self.evaluate(dimension, line, task)?.number(line)?;
            if upper_bound < 0.0 || upper_bound > 1_000_000.0 {
                return Err(program_error(
                    line,
                    "DIM size is outside the supported range",
                ));
            }
            let axis = upper_bound.trunc() as usize + 1;
            length = length
                .checked_mul(axis)
                .ok_or_else(|| program_error(line, "DIM array size overflowed"))?;
        }

        if declaration.byte_block {
            let byte_count =
                u32::try_from(length).map_err(|_| program_error(line, "DIM block is too large"))?;
            let block_end = self
                .next_heap_address
                .checked_add(byte_count)
                .ok_or_else(|| program_error(line, "DIM block address overflowed"))?;
            let memory_end = GUEST_MEMORY_BASE
                .checked_add(u32::try_from(GUEST_MEMORY_SIZE).unwrap_or(u32::MAX))
                .ok_or_else(|| program_error(line, "task memory size overflowed"))?;
            if block_end > memory_end {
                return Err(program_error(line, "DIM block exceeds task memory"));
            }
            task.memory
                .write_bytes(self.next_heap_address, &vec![0; length])?;
            self.set_variable(
                &declaration.name,
                Value::Number(f64::from(self.next_heap_address)),
                line,
            )?;
            self.next_heap_address = block_end;
        } else {
            self.arrays.insert(
                declaration.name.clone(),
                vec![default_value(&declaration.name); length],
            );
        }
        Ok(())
    }

    fn evaluate(
        &mut self,
        expression: &Expr,
        line: u16,
        task: &Task,
    ) -> Result<Value, RuntimeError> {
        match expression {
            Expr::Number(value) => Ok(Value::Number(*value)),
            Expr::String(value) => Ok(Value::String(value.clone())),
            Expr::Variable(name) if name == "TIME" => {
                let ticks = self.started.elapsed().as_millis() / 10;
                Ok(Value::Number(ticks as f64))
            }
            Expr::Variable(name) if name == "INKEY" => Ok(Value::Number(-256.0)),
            Expr::Variable(name) => Ok(self.get_variable(name)),
            Expr::ArrayElement(name, index) => {
                let index = self.evaluate(index, line, task)?.number(line)?;
                let index = array_index(index, line)?;
                let Some(array) = self.arrays.get(name) else {
                    return Err(program_error(
                        line,
                        format!("array {name} was not dimensioned"),
                    ));
                };
                array.get(index).cloned().ok_or_else(|| {
                    program_error(line, format!("array index {index} is out of range"))
                })
            }
            Expr::Unary(operator, operand) => {
                let operand = self.evaluate(operand, line, task)?.number(line)?;
                let value = match operator {
                    UnaryOp::Plus => operand,
                    UnaryOp::Minus => -operand,
                    UnaryOp::Not => f64::from(!(operand as i32)),
                };
                Ok(Value::Number(value))
            }
            Expr::Binary(left, operator, right) => {
                let left = self.evaluate(left, line, task)?;
                let right = self.evaluate(right, line, task)?;
                self.evaluate_binary(left, *operator, right, line)
            }
            Expr::Builtin(token, arguments) => self.evaluate_builtin(*token, arguments, line, task),
            Expr::UserFunction(name, _arguments) => {
                if !self.program.functions.contains_key(name) {
                    return Err(program_error(line, format!("unknown function FN {name}")));
                }
                Err(program_error(
                    line,
                    format!(
                        "FN {name} requires a hardware or compatibility path not active in this run"
                    ),
                ))
            }
            Expr::MemoryRead(width, address) => {
                let address = self.evaluate(address, line, task)?.number(line)?;
                self.read_memory(*width, address, line, task)
            }
        }
    }

    fn evaluate_binary(
        &self,
        left: Value,
        operator: BinaryOp,
        right: Value,
        line: u16,
    ) -> Result<Value, RuntimeError> {
        if operator == BinaryOp::Add {
            if let (Value::String(left), Value::String(right)) = (&left, &right) {
                let mut joined = left.clone();
                joined.extend_from_slice(right);
                return Ok(Value::String(joined));
            }
        }
        if matches!(
            operator,
            BinaryOp::Equal
                | BinaryOp::NotEqual
                | BinaryOp::Less
                | BinaryOp::LessEqual
                | BinaryOp::Greater
                | BinaryOp::GreaterEqual
        ) {
            let ordering = match (&left, &right) {
                (Value::String(left), Value::String(right)) => left.cmp(right),
                (Value::Number(left), Value::Number(right)) => {
                    left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal)
                }
                _ => return Err(program_error(line, "cannot compare a number with a string")),
            };
            let result = match operator {
                BinaryOp::Equal => ordering.is_eq(),
                BinaryOp::NotEqual => !ordering.is_eq(),
                BinaryOp::Less => ordering.is_lt(),
                BinaryOp::LessEqual => !ordering.is_gt(),
                BinaryOp::Greater => ordering.is_gt(),
                BinaryOp::GreaterEqual => !ordering.is_lt(),
                _ => unreachable!(),
            };
            return Ok(Value::Number(if result { 1.0 } else { 0.0 }));
        }

        let left = left.number(line)?;
        let right = right.number(line)?;
        let value = match operator {
            BinaryOp::Add => left + right,
            BinaryOp::Subtract => left - right,
            BinaryOp::Multiply => left * right,
            BinaryOp::Divide => {
                if right == 0.0 {
                    return Err(program_error(line, "division by zero"));
                }
                left / right
            }
            BinaryOp::IntegerDivide => {
                let divisor = right as i32;
                if divisor == 0 {
                    return Err(program_error(line, "integer division by zero"));
                }
                f64::from(
                    (left as i32)
                        .checked_div(divisor)
                        .ok_or_else(|| program_error(line, "integer division overflowed"))?,
                )
            }
            BinaryOp::Modulo => {
                let divisor = right as i32;
                if divisor == 0 {
                    return Err(program_error(line, "MOD by zero"));
                }
                f64::from(
                    (left as i32)
                        .checked_rem(divisor)
                        .ok_or_else(|| program_error(line, "MOD overflowed"))?,
                )
            }
            BinaryOp::Power => left.powf(right),
            BinaryOp::And => f64::from((left as i32) & (right as i32)),
            BinaryOp::Or => f64::from((left as i32) | (right as i32)),
            BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterEqual => unreachable!(),
        };
        Ok(Value::Number(value))
    }

    fn evaluate_builtin(
        &mut self,
        token: u8,
        arguments: &[Expr],
        line: u16,
        task: &Task,
    ) -> Result<Value, RuntimeError> {
        let mut values = Vec::with_capacity(arguments.len());
        for argument in arguments {
            values.push(self.evaluate(argument, line, task)?);
        }
        let one_number = || -> Result<f64, RuntimeError> {
            if values.len() != 1 {
                return Err(program_error(
                    line,
                    "built-in function expects one argument",
                ));
            }
            values[0].number(line)
        };

        match token {
            0x94 => Ok(Value::Number(one_number()?.abs())),
            0x9B => Ok(Value::Number(one_number()?.cos())),
            0xA8 => Ok(Value::Number(one_number()?.floor())),
            0xA9 => {
                if values.len() != 1 {
                    return Err(program_error(line, "LEN expects one argument"));
                }
                Ok(Value::Number(values[0].string(line)?.len() as f64))
            }
            0xAA => Ok(Value::Number(one_number()?.ln())),
            0xAB => Ok(Value::Number(one_number()?.log10())),
            0xB6 => Ok(Value::Number(one_number()?.sqrt())),
            0xB7 => Ok(Value::Number(one_number()?.tan())),
            0xC0 => {
                require_argument_count(&values, 2, line, "LEFT$")?;
                let text = values[0].string(line)?;
                let count = bounded_string_length(values[1].number(line)?, line)?;
                Ok(Value::String(text[..text.len().min(count)].to_vec()))
            }
            0xC1 => {
                if !(2..=3).contains(&values.len()) {
                    return Err(program_error(line, "MID$ expects two or three arguments"));
                }
                let text = values[0].string(line)?;
                let start = values[1].number(line)?.trunc().max(1.0) as usize - 1;
                let length = if values.len() == 3 {
                    bounded_string_length(values[2].number(line)?, line)?
                } else {
                    text.len()
                };
                let end = start.saturating_add(length).min(text.len());
                let start = start.min(text.len());
                Ok(Value::String(text[start..end].to_vec()))
            }
            0xC2 => {
                require_argument_count(&values, 2, line, "RIGHT$")?;
                let text = values[0].string(line)?;
                let count = bounded_string_length(values[1].number(line)?, line)?;
                Ok(Value::String(
                    text[text.len().saturating_sub(count)..].to_vec(),
                ))
            }
            0xC3 => Ok(Value::String(format_number(one_number()?).into_bytes())),
            0xC4 => {
                require_argument_count(&values, 2, line, "STRING$")?;
                let count = bounded_string_length(values[0].number(line)?, line)?;
                let pattern = values[1].string(line)?;
                let byte = pattern.first().copied().unwrap_or_default();
                Ok(Value::String(vec![byte; count]))
            }
            _ => Err(program_error(
                line,
                format!("unsupported built-in token &{token:02X}"),
            )),
        }
    }

    fn get_variable(&self, name: &str) -> Value {
        self.variables
            .get(name)
            .cloned()
            .unwrap_or_else(|| default_value(name))
    }

    fn set_variable(&mut self, name: &str, value: Value, line: u16) -> Result<(), RuntimeError> {
        let value = if name.ends_with('$') {
            match value {
                Value::String(_) => value,
                Value::Number(_) => {
                    return Err(program_error(
                        line,
                        format!("{name} requires a string value"),
                    ));
                }
            }
        } else {
            let number = value.number(line)?;
            Value::Number(if name.ends_with('%') {
                f64::from(number.trunc() as i32)
            } else {
                number
            })
        };
        self.variables.insert(name.to_owned(), value);
        Ok(())
    }

    fn assign(
        &mut self,
        target: &LValue,
        value: Value,
        line: u16,
        task: &mut Task,
    ) -> Result<(), RuntimeError> {
        match target {
            LValue::Variable(name) => self.set_variable(name, value, line),
            LValue::ArrayElement(name, index) => {
                let index = array_index(self.evaluate(index, line, task)?.number(line)?, line)?;
                let Some(array) = self.arrays.get_mut(name) else {
                    return Err(program_error(
                        line,
                        format!("array {name} was not dimensioned"),
                    ));
                };
                let Some(slot) = array.get_mut(index) else {
                    return Err(program_error(
                        line,
                        format!("array index {index} is out of range"),
                    ));
                };
                *slot = coerce_for_variable(name, value, line)?;
                Ok(())
            }
            LValue::Memory(width, address) => {
                let address = self.address_value(address, line, task)?;
                self.write_memory(*width, address, value, line, task)
            }
            LValue::MemoryByteAt(base, offset) => {
                let base = self.evaluate(base, line, task)?.number(line)?;
                let offset = self.evaluate(offset, line, task)?.number(line)?;
                let address = checked_guest_address(base + offset, line)?;
                self.write_memory(MemoryWidth::Byte, address, value, line, task)
            }
        }
    }

    fn address_value(
        &mut self,
        expression: &Expr,
        line: u16,
        task: &Task,
    ) -> Result<u32, RuntimeError> {
        checked_guest_address(self.evaluate(expression, line, task)?.number(line)?, line)
    }

    fn read_memory(
        &self,
        width: MemoryWidth,
        address: f64,
        line: u16,
        task: &Task,
    ) -> Result<Value, RuntimeError> {
        let address = checked_guest_address(address, line)?;
        let value =
            match width {
                MemoryWidth::Byte => u32::from(task.memory.read_byte(address)?),
                MemoryWidth::Word => {
                    let bytes =
                        [
                            task.memory.read_byte(address)?,
                            task.memory
                                .read_byte(address.checked_add(1).ok_or_else(|| {
                                    program_error(line, "memory address overflowed")
                                })?)?,
                            task.memory
                                .read_byte(address.checked_add(2).ok_or_else(|| {
                                    program_error(line, "memory address overflowed")
                                })?)?,
                            task.memory
                                .read_byte(address.checked_add(3).ok_or_else(|| {
                                    program_error(line, "memory address overflowed")
                                })?)?,
                        ];
                    u32::from_le_bytes(bytes) as i32 as u32
                }
            };
        let signed = match width {
            MemoryWidth::Byte => value as i32,
            MemoryWidth::Word => value as i32,
        };
        Ok(Value::Number(f64::from(signed)))
    }

    fn write_memory(
        &self,
        width: MemoryWidth,
        address: u32,
        value: Value,
        line: u16,
        task: &mut Task,
    ) -> Result<(), RuntimeError> {
        let number = value.number(line)? as i32;
        match width {
            MemoryWidth::Byte => task.memory.write_byte(address, number as u8)?,
            MemoryWidth::Word => task.memory.write_bytes(address, &number.to_le_bytes())?,
        }
        Ok(())
    }

    fn read_input(
        &mut self,
        line: u16,
        task: &mut Task,
        dispatcher: &mut SwiDispatcher,
    ) -> Result<Value, RuntimeError> {
        dispatcher.write_inline(task, b"? ")?;
        let mut input = SwiContext::default();
        input.registers[0] = INPUT_BUFFER;
        input.registers[1] = INPUT_BUFFER_SIZE - 1;
        input.registers[2] = u32::from(b' ');
        input.registers[3] = u32::from(u8::MAX);
        dispatcher.dispatch(OS_READ_LINE, task, &mut input)?;
        if input.carry {
            return Err(program_error(line, "input interrupted"));
        }

        let length = input.registers[1];
        if length >= INPUT_BUFFER_SIZE {
            return Err(program_error(
                line,
                "OS_ReadLine returned an invalid string length",
            ));
        }
        let terminator = INPUT_BUFFER
            .checked_add(length)
            .ok_or(crate::memory::MemoryError::AddressOverflow)?;
        task.memory.write_byte(terminator, 0)?;
        let value = task
            .memory
            .read_c_string(INPUT_BUFFER, INPUT_BUFFER_SIZE as usize)?;
        Ok(Value::String(value))
    }

    fn print(
        &mut self,
        items: &[PrintItem],
        line: u16,
        task: &mut Task,
        dispatcher: &mut SwiDispatcher,
    ) -> Result<(), RuntimeError> {
        for item in items {
            match item {
                PrintItem::Value(expression) => {
                    let value = self.evaluate(expression, line, task)?;
                    self.emit(&value_to_bytes(value), task, dispatcher)?;
                }
                PrintItem::Spaces(expression) => {
                    let count = self.evaluate(expression, line, task)?.number(line)?;
                    let count = bounded_string_length(count, line)?;
                    self.emit(&vec![b' '; count], task, dispatcher)?;
                }
                PrintItem::Comma => {
                    let remainder = self.print_column % PRINT_ZONE_WIDTH;
                    let count = PRINT_ZONE_WIDTH - remainder;
                    self.emit(&vec![b' '; count], task, dispatcher)?;
                }
                PrintItem::Semicolon => {}
                PrintItem::NewLine => self.new_line(task, dispatcher)?,
            }
        }

        let suppress_final_newline =
            matches!(items.last(), Some(PrintItem::Semicolon | PrintItem::Comma));
        if !suppress_final_newline {
            self.new_line(task, dispatcher)?;
        }
        Ok(())
    }

    fn emit(
        &mut self,
        bytes: &[u8],
        task: &mut Task,
        dispatcher: &mut SwiDispatcher,
    ) -> Result<(), RuntimeError> {
        if bytes.is_empty() {
            return Ok(());
        }
        dispatcher.write_indirect(task, bytes)?;
        for byte in bytes {
            if *byte == b'\n' || *byte == b'\r' {
                self.print_column = 0;
            } else {
                self.print_column = self.print_column.saturating_add(1);
            }
        }
        Ok(())
    }

    fn new_line(
        &mut self,
        task: &mut Task,
        dispatcher: &mut SwiDispatcher,
    ) -> Result<(), RuntimeError> {
        dispatcher.dispatch(OS_NEW_LINE, task, &mut SwiContext::default())?;
        self.print_column = 0;
        Ok(())
    }

    fn execute_star_command(&self, command: &[u8], line: u16) -> Result<(), RuntimeError> {
        let command = String::from_utf8_lossy(command);
        let normalized: String = command
            .chars()
            .filter(|character| !character.is_ascii_whitespace())
            .flat_map(char::to_uppercase)
            .collect();
        if normalized == "FX151,78,243" {
            // ClockSP5 resets machine-specific display/timing state here.
            // The hosted profile has no such hardware state to restore.
            return Ok(());
        }
        Err(program_error(
            line,
            format!("MOS command *{command} is not available in the hosted profile"),
        ))
    }
}

fn match_for_loops(program: &ParsedProgram) -> HashMap<usize, usize> {
    let mut stack = Vec::new();
    let mut matches = HashMap::new();
    for (address, instruction) in program.instructions.iter().enumerate() {
        match &instruction.statement {
            Statement::For { .. } => stack.push(address),
            Statement::Next(_) => {
                if let Some(start) = stack.pop() {
                    matches.insert(start, address);
                }
            }
            _ => {}
        }
    }
    matches
}

fn default_value(name: &str) -> Value {
    if name.ends_with('$') {
        Value::String(Vec::new())
    } else {
        Value::Number(0.0)
    }
}

fn coerce_for_variable(name: &str, value: Value, line: u16) -> Result<Value, RuntimeError> {
    if name.ends_with('$') {
        match value {
            string @ Value::String(_) => Ok(string),
            Value::Number(_) => Err(program_error(
                line,
                format!("{name} requires a string value"),
            )),
        }
    } else {
        let number = value.number(line)?;
        Ok(Value::Number(if name.ends_with('%') {
            f64::from(number.trunc() as i32)
        } else {
            number
        }))
    }
}

fn array_index(number: f64, line: u16) -> Result<usize, RuntimeError> {
    if !number.is_finite() || number < 0.0 || number > usize::MAX as f64 {
        return Err(program_error(
            line,
            "array index is outside the supported range",
        ));
    }
    Ok(number.trunc() as usize)
}

fn checked_guest_address(number: f64, line: u16) -> Result<u32, RuntimeError> {
    if !number.is_finite() || number < 0.0 || number > f64::from(u32::MAX) {
        return Err(program_error(
            line,
            "memory address is outside the logical address range",
        ));
    }
    Ok(number.trunc() as u32)
}

fn bounded_string_length(number: f64, line: u16) -> Result<usize, RuntimeError> {
    if !number.is_finite() || number < 0.0 || number > 1_000_000.0 {
        return Err(program_error(
            line,
            "string length is outside the supported range",
        ));
    }
    Ok(number.trunc() as usize)
}

fn value_to_bytes(value: Value) -> Vec<u8> {
    match value {
        Value::String(value) => value,
        Value::Number(value) => format_number(value).into_bytes(),
    }
}

fn format_number(number: f64) -> String {
    if number == 0.0 {
        return "0".into();
    }
    if number.is_finite() && number.fract() == 0.0 && number.abs() < 1.0e15 {
        return format!("{number:.0}");
    }
    number.to_string()
}

fn require_argument_count(
    values: &[Value],
    expected: usize,
    line: u16,
    function: &str,
) -> Result<(), RuntimeError> {
    if values.len() != expected {
        Err(program_error(
            line,
            format!("{function} expects {expected} arguments"),
        ))
    } else {
        Ok(())
    }
}

fn program_error(line: u16, message: impl AsRef<str>) -> RuntimeError {
    RuntimeError::Program(format!("line {line}: {}", message.as_ref()))
}
