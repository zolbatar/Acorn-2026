use std::{error::Error, fmt, fs, path::Path};

use crate::error::RuntimeError;

const LINE_MARKER: u8 = 0x0D;
const PROGRAM_END: u8 = 0xFF;
const LINE_REFERENCE: u8 = 0x8D;
const TOKENIZED_REM: u8 = 0xF4;
const TOKENIZED_DATA: u8 = 0xDC;
const MIN_RECORD_LENGTH: usize = 5;
const MAX_LINE_NUMBER: u16 = 0xFEFF;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TokenizedBasicLine {
    pub number: u16,
    /// Tokenized statements, excluding the record's final carriage return.
    pub bytes: Vec<u8>,
    pub line_references: Vec<u16>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TokenizedBasicProgram {
    pub lines: Vec<TokenizedBasicLine>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecodeError {
    MissingTerminator,
    InvalidLineMarker {
        offset: usize,
        found: u8,
    },
    TruncatedHeader {
        offset: usize,
    },
    TrailingBytes {
        offset: usize,
    },
    InvalidLineNumber {
        line_number: u16,
    },
    InvalidRecordLength {
        line_number: u16,
        length: u8,
    },
    TruncatedRecord {
        line_number: u16,
        expected_end: usize,
        file_length: usize,
    },
    MissingLineCarriageReturn {
        line_number: u16,
    },
    NonIncreasingLineNumber {
        previous: u16,
        current: u16,
    },
    TruncatedLineReference {
        line_number: u16,
        offset: usize,
    },
    TruncatedExtendedToken {
        line_number: u16,
        offset: usize,
    },
}

impl TokenizedBasicProgram {
    /// Decode the shared tokenized saved-program record format. Token bytes
    /// stay opaque so earlier BASIC dialects can share this file reader.
    pub fn decode(file: &[u8]) -> Result<Self, DecodeError> {
        let mut cursor = 0_usize;
        let mut lines = Vec::new();
        let mut previous_line = None;

        loop {
            if cursor >= file.len() {
                return Err(DecodeError::MissingTerminator);
            }
            if file[cursor] != LINE_MARKER {
                return Err(DecodeError::InvalidLineMarker {
                    offset: cursor,
                    found: file[cursor],
                });
            }
            if cursor + 1 >= file.len() {
                return Err(DecodeError::TruncatedHeader { offset: cursor });
            }
            if file[cursor + 1] == PROGRAM_END {
                if cursor + 2 != file.len() {
                    return Err(DecodeError::TrailingBytes { offset: cursor + 2 });
                }
                return Ok(Self { lines });
            }
            if file.len() - cursor < 4 {
                return Err(DecodeError::TruncatedHeader { offset: cursor });
            }

            let line_number = u16::from_be_bytes([file[cursor + 1], file[cursor + 2]]);
            if line_number > MAX_LINE_NUMBER {
                return Err(DecodeError::InvalidLineNumber { line_number });
            }
            if let Some(previous) = previous_line {
                if line_number <= previous {
                    return Err(DecodeError::NonIncreasingLineNumber {
                        previous,
                        current: line_number,
                    });
                }
            }

            let record_length = file[cursor + 3];
            if usize::from(record_length) < MIN_RECORD_LENGTH {
                return Err(DecodeError::InvalidRecordLength {
                    line_number,
                    length: record_length,
                });
            }
            let record_end = cursor.checked_add(usize::from(record_length)).ok_or(
                DecodeError::TruncatedRecord {
                    line_number,
                    expected_end: usize::MAX,
                    file_length: file.len(),
                },
            )?;
            if record_end > file.len() {
                return Err(DecodeError::TruncatedRecord {
                    line_number,
                    expected_end: record_end,
                    file_length: file.len(),
                });
            }
            if file[record_end - 1] != LINE_MARKER {
                return Err(DecodeError::MissingLineCarriageReturn { line_number });
            }

            let body_start = cursor + 4;
            let body_end = record_end - 1;
            let body = file[body_start..body_end].to_vec();
            let line_references = scan_line_references(&body, line_number, body_start)?;
            lines.push(TokenizedBasicLine {
                number: line_number,
                bytes: body,
                line_references,
            });
            previous_line = Some(line_number);
            cursor = record_end;
        }
    }

    pub fn load_file(path: impl AsRef<Path>) -> Result<Self, RuntimeError> {
        let bytes = fs::read(path)?;
        Self::decode(&bytes).map_err(|error| RuntimeError::Program(error.to_string()))
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn line_reference_count(&self) -> usize {
        self.lines
            .iter()
            .map(|line| line.line_references.len())
            .sum()
    }

    pub fn unresolved_line_reference_count(&self) -> usize {
        self.lines
            .iter()
            .flat_map(|line| line.line_references.iter())
            .filter(|target| self.line(**target).is_none())
            .count()
    }

    pub fn line(&self, number: u16) -> Option<&TokenizedBasicLine> {
        self.lines
            .binary_search_by_key(&number, |line| line.number)
            .ok()
            .map(|index| &self.lines[index])
    }
}

fn scan_line_references(
    bytes: &[u8],
    line_number: u16,
    file_offset: usize,
) -> Result<Vec<u16>, DecodeError> {
    if bytes.first() == Some(&b'*') {
        return Ok(Vec::new());
    }

    let mut references = Vec::new();
    let mut index = 0_usize;
    let mut quoted = false;
    let mut in_data = false;
    let mut at_statement_start = true;

    while index < bytes.len() {
        let byte = bytes[index];

        if quoted {
            if byte == b'"' {
                if bytes.get(index + 1) == Some(&b'"') {
                    index += 2;
                    continue;
                }
                quoted = false;
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            quoted = true;
            index += 1;
            continue;
        }
        if in_data {
            if byte == b':' {
                in_data = false;
                at_statement_start = true;
            }
            index += 1;
            continue;
        }
        if byte == b':' {
            at_statement_start = true;
            index += 1;
            continue;
        }
        if at_statement_start && (byte == b' ' || byte == b'\t') {
            index += 1;
            continue;
        }
        if at_statement_start && byte == b'*' {
            break;
        }
        at_statement_start = false;

        if byte == TOKENIZED_REM {
            break;
        }
        if byte == TOKENIZED_DATA {
            in_data = true;
            index += 1;
            continue;
        }
        if byte == LINE_REFERENCE {
            let Some(encoded) = bytes.get(index + 1..index + 4) else {
                return Err(DecodeError::TruncatedLineReference {
                    line_number,
                    offset: file_offset + index,
                });
            };
            references.push(decode_line_reference(encoded));
            index += 4;
            continue;
        }
        if matches!(byte, 0xC6..=0xC8) {
            if index + 1 >= bytes.len() {
                return Err(DecodeError::TruncatedExtendedToken {
                    line_number,
                    offset: file_offset + index,
                });
            }
            index += 2;
            continue;
        }

        index += 1;
    }

    Ok(references)
}

fn decode_line_reference(encoded: &[u8]) -> u16 {
    let [marker, low, high] = [encoded[0], encoded[1], encoded[2]];
    let low = low ^ ((marker.wrapping_mul(4)) & 0xC0);
    let high = high ^ marker.wrapping_mul(16);
    u16::from_be_bytes([high, low])
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingTerminator => write!(f, "missing 0D FF program terminator"),
            Self::InvalidLineMarker { offset, found } => {
                write!(
                    f,
                    "expected 0D line marker at byte {offset}, found {found:02X}"
                )
            }
            Self::TruncatedHeader { offset } => {
                write!(f, "truncated line header at byte {offset}")
            }
            Self::TrailingBytes { offset } => {
                write!(
                    f,
                    "unexpected bytes after program terminator at byte {offset}"
                )
            }
            Self::InvalidLineNumber { line_number } => {
                write!(
                    f,
                    "line number {line_number} is reserved by the saved-program format"
                )
            }
            Self::InvalidRecordLength {
                line_number,
                length,
            } => write!(f, "line {line_number} has invalid record length {length}"),
            Self::TruncatedRecord {
                line_number,
                expected_end,
                file_length,
            } => write!(
                f,
                "line {line_number} extends to byte {expected_end}, past end of file ({file_length})"
            ),
            Self::MissingLineCarriageReturn { line_number } => {
                write!(
                    f,
                    "line {line_number} is missing its final 0D carriage return"
                )
            }
            Self::NonIncreasingLineNumber { previous, current } => write!(
                f,
                "line numbers are not increasing: {current} follows {previous}"
            ),
            Self::TruncatedLineReference {
                line_number,
                offset,
            } => write!(
                f,
                "line {line_number} has an incomplete line reference at byte {offset}"
            ),
            Self::TruncatedExtendedToken {
                line_number,
                offset,
            } => write!(
                f,
                "line {line_number} has an incomplete extended token at byte {offset}"
            ),
        }
    }
}

impl Error for DecodeError {}

#[cfg(test)]
mod tests {
    use super::{DecodeError, TokenizedBasicProgram};

    #[test]
    fn decodes_clocksp5_records_and_line_references() {
        let fixture = include_bytes!("../examples/clocksp5/ClockSP5.bbc");
        let program = TokenizedBasicProgram::decode(fixture)
            .expect("ClockSP5 fixture should use valid saved-program records");

        assert_eq!(fixture[3], 16, "record length includes the 4-byte preamble");
        assert_eq!(program.line_count(), 143);
        assert_eq!(program.line_reference_count(), 37);
        assert_eq!(program.unresolved_line_reference_count(), 0);
        assert_eq!(program.line(50).expect("line 50").line_references, [60]);
        assert_eq!(program.line(160).expect("line 160").line_references, [170]);
        assert_eq!(
            program.line(192).expect("line 192").line_references,
            [55; 16]
        );
        assert_eq!(program.line(300).expect("line 300").line_references, [600]);
        assert_eq!(
            program.line(30).expect("line 30").bytes,
            [
                0xD2, b'=', 0x92, b'+', b'2', b'5', b'6', 0x80, b'-', b'2', b'5', b'6'
            ]
        );
        assert_eq!(
            program.line(52).expect("line 52").bytes,
            [0xDD, 0xF2, b'T', b':', 0xE1]
        );
    }

    #[test]
    fn ignores_line_reference_bytes_in_data_comments_and_strings() {
        let body = [
            b'"', 0x8D, 0x40, 0x40, 0x40, b'"', b':', 0xDC, b'1', b',', 0x8D, b':', 0xF4, 0x8D,
        ];
        assert!(
            super::scan_line_references(&body, 10, 4)
                .expect("opaque text must be skipped")
                .is_empty()
        );
    }

    #[test]
    fn rejects_incomplete_records_and_references() {
        assert_eq!(
            TokenizedBasicProgram::decode(&[0x0D, 0x00, 0x01, 6, 0xF4]),
            Err(DecodeError::TruncatedRecord {
                line_number: 1,
                expected_end: 6,
                file_length: 5,
            })
        );

        let mut line_with_truncated_reference = vec![0x0D, 0x00, 0x01, 7, 0xE5, 0x8D, 0x0D];
        line_with_truncated_reference.extend_from_slice(&[0x0D, 0xFF]);
        assert!(matches!(
            TokenizedBasicProgram::decode(&line_with_truncated_reference),
            Err(DecodeError::TruncatedLineReference { line_number: 1, .. })
        ));
    }
}
