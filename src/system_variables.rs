//! Bounded, runtime-session system variables.
//!
//! This is a hosted STRING-only subset of the RISC OS system-variable heap.
//! It deliberately does not inherit the host process environment, persist
//! values or evaluate expressions. Type-0 writes accept a deliberately small,
//! bounded subset of GSTrans string expansion; macro variables are absent.

use std::collections::BTreeMap;

use crate::error::RuntimeError;

pub(crate) const MAX_NAME_BYTES: usize = 32;
pub(crate) const MAX_VALUE_BYTES: usize = 256;
pub(crate) const MAX_VARIABLE_COUNT: usize = 128;
pub(crate) const MAX_AGGREGATE_BYTES: usize = 32 * 1024;
const MAX_EXPANSION_WORK: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SystemVariableType {
    String,
    LiteralString,
}

impl SystemVariableType {
    pub(crate) fn from_register(value: u32) -> Result<Self, RuntimeError> {
        match value {
            0 => Ok(Self::String),
            4 => Ok(Self::LiteralString),
            _ => Err(RuntimeError::Structured {
                type_name: "SystemVariableTypeError".into(),
                code: 5,
                message: format!(
                    "hosted system variables support only string types 0 and 4, not {value}"
                ),
            }),
        }
    }

    pub(crate) const fn register_value(self) -> u32 {
        match self {
            Self::String => 0,
            Self::LiteralString => 4,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SystemVariable {
    pub(crate) name: String,
    pub(crate) value: String,
    pub(crate) variable_type: SystemVariableType,
}

#[derive(Debug, Default)]
pub(crate) struct SystemVariableStore {
    // Keys are ASCII uppercase for case-insensitive lookup and stable ordering.
    variables: BTreeMap<String, SystemVariable>,
    aggregate_bytes: usize,
}

impl SystemVariableStore {
    pub(crate) fn read(
        &self,
        selector: &str,
        after_key: Option<&str>,
    ) -> Result<SystemVariable, RuntimeError> {
        validate_selector(selector)?;
        let selector = selector.to_ascii_uppercase();
        let candidate = self
            .variables
            .iter()
            .filter(|(key, _)| after_key.is_none_or(|after| key.as_str() > after))
            .find(|(key, _)| pattern_matches(selector.as_bytes(), key.as_bytes()))
            .map(|(_, value)| value.clone());
        candidate.ok_or_else(|| not_found(&selector))
    }

    pub(crate) fn read_with_obey_directory(
        &self,
        selector: &str,
        after_key: Option<&str>,
        obey_directory: Option<&str>,
    ) -> Result<SystemVariable, RuntimeError> {
        let Some(directory) = obey_directory else {
            return self.read(selector, after_key);
        };
        validate_selector(selector)?;
        let pattern = selector.to_ascii_uppercase();
        let virtual_key = "OBEY$DIR";
        let mut candidate = self
            .variables
            .iter()
            .filter(|(key, _)| after_key.is_none_or(|after| key.as_str() > after))
            .find(|(key, _)| pattern_matches(pattern.as_bytes(), key.as_bytes()))
            .map(|(key, value)| {
                if key == virtual_key {
                    SystemVariable {
                        name: "Obey$Dir".into(),
                        value: directory.to_owned(),
                        variable_type: SystemVariableType::String,
                    }
                } else {
                    value.clone()
                }
            });
        if after_key.is_none_or(|after| virtual_key > after)
            && pattern_matches(pattern.as_bytes(), virtual_key.as_bytes())
            && candidate.as_ref().is_none_or(|item| {
                item.name.to_ascii_uppercase().as_bytes() > virtual_key.as_bytes()
            })
        {
            candidate = Some(SystemVariable {
                name: "Obey$Dir".into(),
                value: directory.to_owned(),
                variable_type: SystemVariableType::String,
            });
        }
        candidate.ok_or_else(|| not_found(selector))
    }

    #[cfg(test)]
    pub(crate) fn set(
        &mut self,
        name: &str,
        value: String,
        variable_type: SystemVariableType,
    ) -> Result<SystemVariable, RuntimeError> {
        self.set_with_obey_directory(name, value, variable_type, None)
    }

    pub(crate) fn set_with_obey_directory(
        &mut self,
        name: &str,
        value: String,
        variable_type: SystemVariableType,
        obey_directory: Option<&str>,
    ) -> Result<SystemVariable, RuntimeError> {
        validate_selector(name)?;
        let value = match variable_type {
            SystemVariableType::String => self.expand_string(&value, obey_directory)?,
            SystemVariableType::LiteralString => value,
        };
        validate_value(&value, variable_type)?;
        let key = name.to_ascii_uppercase();
        let wildcard = has_wildcards(name);

        let (key, display_name) = if wildcard {
            let matches = self
                .variables
                .keys()
                .filter(|candidate| pattern_matches(key.as_bytes(), candidate.as_bytes()))
                .cloned()
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [] => return Err(not_found(name)),
                [matched] => {
                    let existing = self.variables.get(matched).expect("matched key exists");
                    (matched.clone(), existing.name.clone())
                }
                _ => {
                    return Err(RuntimeError::Structured {
                        type_name: "SystemVariablePatternAmbiguous".into(),
                        code: 6,
                        message: "wildcard update must select exactly one existing variable".into(),
                    });
                }
            }
        } else if let Some(existing) = self.variables.get(&key) {
            (key, existing.name.clone())
        } else {
            (key, name.to_string())
        };

        let previous_size = self
            .variables
            .get(&key)
            .map(variable_storage_size)
            .unwrap_or(0);
        let replacement_size = display_name.len() + value.len();
        let next_aggregate = self
            .aggregate_bytes
            .checked_sub(previous_size)
            .and_then(|size| size.checked_add(replacement_size))
            .ok_or_else(|| limit_error("system-variable storage size overflowed"))?;
        let next_count = self.variables.len() + usize::from(!self.variables.contains_key(&key));
        if next_count > MAX_VARIABLE_COUNT || next_aggregate > MAX_AGGREGATE_BYTES {
            return Err(limit_error("system-variable store capacity exceeded"));
        }

        let variable = SystemVariable {
            name: display_name,
            value,
            variable_type,
        };
        self.variables.insert(key, variable.clone());
        self.aggregate_bytes = next_aggregate;
        Ok(variable)
    }

    fn expand_string(
        &self,
        source: &str,
        obey_directory: Option<&str>,
    ) -> Result<String, RuntimeError> {
        if source.len() > MAX_VALUE_BYTES {
            return Err(limit_error(format!(
                "system-variable input is limited to {MAX_VALUE_BYTES} UTF-8 bytes"
            )));
        }
        let bytes = source.as_bytes();
        let quoted = bytes.first() == Some(&b'"');
        let mut index = usize::from(quoted);
        let mut closed = !quoted;

        let mut result = String::with_capacity(source.len().min(MAX_VALUE_BYTES));
        let mut work = 0;
        while index < bytes.len() {
            work += 1;
            if work > MAX_EXPANSION_WORK {
                return Err(limit_error("system-variable expansion work limit exceeded"));
            }
            match bytes[index] {
                b'<' => {
                    let name_start = index + 1;
                    let close = bytes[name_start..]
                        .iter()
                        .position(|byte| *byte == b'>')
                        .map(|offset| name_start + offset)
                        .ok_or_else(|| expansion_error("variable reference is missing '>'"))?;
                    let name = std::str::from_utf8(&bytes[name_start..close])
                        .map_err(|_| expansion_error("variable name is not ASCII"))?;
                    validate_selector(name).map_err(|_| {
                        expansion_error("variable reference must name one visible ASCII variable")
                    })?;
                    if has_wildcards(name) || name.bytes().all(|byte| byte.is_ascii_digit()) {
                        return Err(expansion_error(
                            "wildcard and numeric angle-bracket operands are unsupported",
                        ));
                    }
                    if name.eq_ignore_ascii_case("Obey$Dir") {
                        if let Some(directory) = obey_directory {
                            append_expansion(&mut result, directory.as_bytes())?;
                        } else {
                            let variable = self
                                .variables
                                .get(&name.to_ascii_uppercase())
                                .ok_or_else(|| not_found(name))?;
                            append_expansion(&mut result, variable.value.as_bytes())?;
                        }
                    } else {
                        let variable = self
                            .variables
                            .get(&name.to_ascii_uppercase())
                            .ok_or_else(|| not_found(name))?;
                        append_expansion(&mut result, variable.value.as_bytes())?;
                    }
                    index = close + 1;
                }
                b'|' => {
                    let escaped = *bytes
                        .get(index + 1)
                        .ok_or_else(|| expansion_error("escape marker needs a following byte"))?;
                    let literal = match escaped {
                        b'<' | b'>' | b'|' | b'"' if index + 2 <= bytes.len() => escaped,
                        _ => return Err(expansion_error("unsupported GSTrans escape")),
                    };
                    append_expansion(&mut result, &[literal])?;
                    index += 2;
                }
                b'"' if quoted && index + 1 == bytes.len() => {
                    closed = true;
                    index += 1;
                    if index != bytes.len() {
                        return Err(expansion_error("characters follow the closing quote"));
                    }
                }
                b'"' if quoted && bytes.get(index + 1) == Some(&b'"') => {
                    append_expansion(&mut result, b"\"")?;
                    index += 2;
                }
                b'"' => return Err(expansion_error("embedded quote must be doubled")),
                _ => {
                    let tail = std::str::from_utf8(&bytes[index..])
                        .map_err(|_| expansion_error("input string is not UTF-8"))?;
                    let ch = tail
                        .chars()
                        .next()
                        .ok_or_else(|| expansion_error("invalid UTF-8 boundary"))?;
                    append_expansion(&mut result, ch.to_string().as_bytes())?;
                    index += ch.len_utf8();
                }
            }
        }
        if quoted && !closed {
            return Err(expansion_error(
                "quoted string is missing its closing quote",
            ));
        }
        Ok(result)
    }

    pub(crate) fn delete(&mut self, selector: &str) -> Result<usize, RuntimeError> {
        validate_selector(selector)?;
        let selector = selector.to_ascii_uppercase();
        let keys = self
            .variables
            .keys()
            .filter(|key| pattern_matches(selector.as_bytes(), key.as_bytes()))
            .cloned()
            .collect::<Vec<_>>();
        if keys.is_empty() {
            return Err(not_found(&selector));
        }
        let removed_bytes = keys.iter().fold(0_usize, |size, key| {
            size.saturating_add(
                self.variables
                    .get(key)
                    .map(variable_storage_size)
                    .unwrap_or(0),
            )
        });
        for key in &keys {
            self.variables.remove(key);
        }
        self.aggregate_bytes = self.aggregate_bytes.saturating_sub(removed_bytes);
        Ok(keys.len())
    }

    #[cfg(test)]
    pub(crate) fn find_exact(&self, name: &str) -> Result<SystemVariable, RuntimeError> {
        validate_selector(name)?;
        if has_wildcards(name) {
            return Err(name_error(
                "exact variable lookup does not accept wildcards",
            ));
        }
        self.variables
            .get(&name.to_ascii_uppercase())
            .cloned()
            .ok_or_else(|| not_found(name))
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.variables.len()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.variables.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn aggregate_bytes(&self) -> usize {
        self.aggregate_bytes
    }
}

pub(crate) fn validate_selector(selector: &str) -> Result<(), RuntimeError> {
    if selector.is_empty() || selector.len() > MAX_NAME_BYTES {
        return Err(name_error(format!(
            "variable name/pattern must contain 1..={MAX_NAME_BYTES} bytes"
        )));
    }
    if !selector
        .bytes()
        .all(|byte| byte.is_ascii_graphic() && byte.is_ascii())
    {
        return Err(name_error(
            "variable names use visible non-space ASCII characters only",
        ));
    }
    Ok(())
}

fn validate_value(value: &str, variable_type: SystemVariableType) -> Result<(), RuntimeError> {
    if value.len() > MAX_VALUE_BYTES {
        return Err(limit_error(format!(
            "system-variable values are limited to {MAX_VALUE_BYTES} UTF-8 bytes"
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(RuntimeError::Structured {
            type_name: "SystemVariableTypeError".into(),
            code: 5,
            message: "control characters are unsupported in hosted string variables".into(),
        });
    }
    let _ = variable_type;
    Ok(())
}

fn append_expansion(output: &mut String, bytes: &[u8]) -> Result<(), RuntimeError> {
    let next = output
        .len()
        .checked_add(bytes.len())
        .ok_or_else(|| limit_error("expanded system-variable value size overflowed"))?;
    if next > MAX_VALUE_BYTES {
        return Err(limit_error(format!(
            "expanded system-variable values are limited to {MAX_VALUE_BYTES} UTF-8 bytes"
        )));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| expansion_error("substituted value is not valid UTF-8"))?;
    output.push_str(text);
    Ok(())
}

fn expansion_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::Structured {
        type_name: "SystemVariableExpansionError".into(),
        code: 7,
        message: message.into(),
    }
}

fn variable_storage_size(variable: &SystemVariable) -> usize {
    variable.name.len() + variable.value.len()
}

fn has_wildcards(selector: &str) -> bool {
    selector.bytes().any(|byte| matches!(byte, b'*' | b'#'))
}

/// RISC OS system-variable wildcard subset: `*` is zero-or-more bytes and
/// `#` is exactly one byte. Names are ASCII, so byte-oriented matching is
/// unambiguous after case-folding.
fn pattern_matches(pattern: &[u8], value: &[u8]) -> bool {
    let mut row = vec![false; value.len() + 1];
    row[0] = true;
    for byte in pattern {
        let mut next = vec![false; value.len() + 1];
        match byte {
            b'*' => {
                next[0] = row[0];
                for index in 1..=value.len() {
                    next[index] = row[index] || next[index - 1];
                }
            }
            b'#' => {
                for index in 1..=value.len() {
                    next[index] = row[index - 1];
                }
            }
            literal => {
                for index in 1..=value.len() {
                    next[index] = row[index - 1] && *literal == value[index - 1];
                }
            }
        }
        row = next;
    }
    row[value.len()]
}

pub(crate) fn selector_matches_name(selector: &str, name: &str) -> bool {
    pattern_matches(
        selector.to_ascii_uppercase().as_bytes(),
        name.to_ascii_uppercase().as_bytes(),
    )
}

pub(crate) fn name_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::Structured {
        type_name: "SystemVariableNameError".into(),
        code: 1,
        message: message.into(),
    }
}

pub(crate) fn not_found(name: &str) -> RuntimeError {
    RuntimeError::Structured {
        type_name: "SystemVariableNotFound".into(),
        code: 2,
        message: format!("system variable {name:?} was not found"),
    }
}

pub(crate) fn limit_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::Structured {
        type_name: "SystemVariableLimitError".into(),
        code: 4,
        message: message.into(),
    }
}

pub(crate) fn buffer_error(required: usize, capacity: usize) -> RuntimeError {
    RuntimeError::Structured {
        type_name: "SystemVariableBufferError".into(),
        code: 3,
        message: format!(
            "system variable needs {required} value bytes, caller supplied {capacity}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn string_type() -> SystemVariableType {
        SystemVariableType::String
    }

    fn literal_type() -> SystemVariableType {
        SystemVariableType::LiteralString
    }

    #[test]
    fn names_fold_for_lookup_but_keep_creation_spelling_and_sorted_order() {
        let mut store = SystemVariableStore::default();
        store.set("zeta$", "last".into(), string_type()).unwrap();
        store.set("Alpha$", "first".into(), literal_type()).unwrap();
        store
            .set("ALPHA$", "updated".into(), string_type())
            .unwrap();
        assert_eq!(store.find_exact("alpha$").unwrap().name, "Alpha$");
        assert_eq!(store.read("*", None).unwrap().name, "Alpha$");
        assert_eq!(store.read("*", Some("ALPHA$")).unwrap().name, "zeta$");
    }

    #[test]
    fn wildcard_hash_matches_exactly_one_byte_and_delete_is_atomic() {
        let mut store = SystemVariableStore::default();
        store.set("A1", "one".into(), string_type()).unwrap();
        store.set("A2", "two".into(), string_type()).unwrap();
        assert_eq!(store.read("A#", None).unwrap().name, "A1");
        assert!(matches!(
            store.set("A#", "ambiguous".into(), string_type()),
            Err(RuntimeError::Structured { type_name, code: 6, .. })
                if type_name == "SystemVariablePatternAmbiguous"
        ));
        assert_eq!(store.find_exact("A1").unwrap().value, "one");
        assert_eq!(store.delete("A#").unwrap(), 2);
        assert!(store.is_empty());
    }

    #[test]
    fn failed_types_and_limits_leave_the_store_unchanged() {
        let mut store = SystemVariableStore::default();
        store.set("OK", "safe".into(), string_type()).unwrap();
        let before = store.aggregate_bytes();
        assert!(matches!(
            store.set("NEW", "<Other$Value>".into(), string_type()),
            Err(RuntimeError::Structured { type_name, code: 2, .. })
                if type_name == "SystemVariableNotFound"
        ));
        assert!(matches!(
            SystemVariableType::from_register(2),
            Err(RuntimeError::Structured { type_name, code: 5, .. })
                if type_name == "SystemVariableTypeError"
        ));
        assert_eq!(store.len(), 1);
        assert_eq!(store.aggregate_bytes(), before);
    }

    #[test]
    fn empty_and_literal_values_round_trip_distinctly() {
        let mut store = SystemVariableStore::default();
        store.set("Empty", String::new(), string_type()).unwrap();
        store
            .set("Literal", "<quoted>|text".into(), literal_type())
            .unwrap();
        assert_eq!(store.find_exact("EMPTY").unwrap().value, "");
        let literal = store.find_exact("literal").unwrap();
        assert_eq!(literal.value, "<quoted>|text");
        assert_eq!(literal.variable_type, literal_type());
    }

    #[test]
    fn string_expansion_is_bounded_one_pass_and_literal_strings_stay_raw() {
        let mut store = SystemVariableStore::default();
        store
            .set("INNER", "<UNRESOLVED>".into(), literal_type())
            .unwrap();
        store
            .set(
                "COMPOSED",
                "\"  <INNER> |<x|> || |\"  \"".into(),
                string_type(),
            )
            .unwrap();
        assert_eq!(
            store.find_exact("COMPOSED").unwrap().value,
            "  <UNRESOLVED> <x> | \"  "
        );
        store
            .set("RAW", "<MISSING>|Z\"".into(), literal_type())
            .unwrap();
        assert_eq!(store.find_exact("RAW").unwrap().value, "<MISSING>|Z\"");
    }

    #[test]
    fn unsupported_or_oversized_expansion_is_atomic() {
        let mut store = SystemVariableStore::default();
        store.set("KEEP", "before".into(), literal_type()).unwrap();
        for invalid in [
            "bad<OPEN",
            "bad|Q",
            "bad<NO-SUCH>",
            "<1>",
            "<KE*P>",
            "\"open",
        ] {
            assert!(
                store.set("KEEP", invalid.into(), string_type()).is_err(),
                "{invalid:?}"
            );
            assert_eq!(store.find_exact("KEEP").unwrap().value, "before");
        }
        store
            .set("BIG", "x".repeat(MAX_VALUE_BYTES), literal_type())
            .unwrap();
        assert!(
            store
                .set("KEEP", "<BIG><BIG>".into(), string_type())
                .is_err()
        );
        assert_eq!(store.find_exact("KEEP").unwrap().value, "before");
    }
}
