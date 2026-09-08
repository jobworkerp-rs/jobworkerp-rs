//! Parsing of the optional structured form stored in `worker.description`.
//!
//! This deliberately lives in mcp-server: worker storage continues to treat a
//! description as an opaque string for every non-MCP consumer.

use anyhow::{Result, anyhow};
use jsonschema::Validator;
use serde_json::{Map, Number, Value};
use std::collections::HashSet;
use std::sync::LazyLock;
use yaml_rust2::parser::{Event, EventReceiver, Parser};
use yaml_rust2::scanner::TScalarStyle;
use yaml_rust2::{Yaml, YamlLoader};

const MAX_BYTES: usize = 1024 * 1024;
const MAX_DEPTH: usize = 64;
const MAX_NODES: usize = 10_000;
const MAX_SCALAR_BYTES: usize = 64 * 1024;
const MAX_NUMBER_DIGITS: usize = 10_000;
const MAX_NUMBER_EXPONENT: i32 = 10_000;

static VALIDATOR: LazyLock<Validator> = LazyLock::new(|| {
    let schema: Value = serde_json::from_str(include_str!("worker_description_v1.schema.json"))
        .expect("embedded worker description schema must be valid JSON");
    jsonschema::draft202012::new(&schema).expect("embedded worker description schema must compile")
});

#[derive(Debug, Clone)]
pub enum ParsedDescription {
    Plain,
    Structured(StructuredDescription),
    InvalidDeclared(String),
}

#[derive(Debug, Clone)]
pub struct StructuredDescription {
    pub description: String,
    pub rpc: Option<String>,
    pub parameters: Map<String, Value>,
}

pub fn parse(value: &str) -> ParsedDescription {
    let declared_prefix = has_declared_prefix(value);
    match parse_declared_yaml(value) {
        Ok((false, _)) if !declared_prefix => ParsedDescription::Plain,
        Ok((false, _)) => ParsedDescription::InvalidDeclared(
            "invalid structured-description declaration".to_string(),
        ),
        Ok((true, json)) => match VALIDATOR.validate(&json) {
            Ok(()) => {
                let object = json.as_object().expect("schema requires object");
                ParsedDescription::Structured(StructuredDescription {
                    description: object["description"]
                        .as_str()
                        .expect("schema requires string")
                        .to_string(),
                    rpc: object
                        .get("rpc")
                        .and_then(Value::as_str)
                        .map(ToString::to_string),
                    parameters: object
                        .get("parameters")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default(),
                })
            }
            Err(error) => ParsedDescription::InvalidDeclared(error.to_string()),
        },
        Err((declared, error)) if declared || declared_prefix => {
            ParsedDescription::InvalidDeclared(error.to_string())
        }
        Err(_) => ParsedDescription::Plain,
    }
}

fn parse_declared_yaml(input: &str) -> std::result::Result<(bool, Value), (bool, anyhow::Error)> {
    if input.len() > MAX_BYTES {
        return Err((false, anyhow!("description YAML exceeds {MAX_BYTES} bytes")));
    }
    let mut audit = Audit::default();
    let mut parser = Parser::new_from_str(input);
    if let Err(error) = parser.load(&mut audit, true) {
        return Err((audit.declared, anyhow!(error.to_string())));
    }
    let declared = audit.declared;
    if let Err(error) = audit.finish() {
        return Err((declared, error));
    }
    let docs =
        YamlLoader::load_from_str(input).map_err(|error| (declared, anyhow!(error.to_string())))?;
    if docs.len() != 1 {
        return Err((
            declared,
            anyhow!("structured description must contain exactly one YAML document"),
        ));
    }
    let json = yaml_to_json(&docs[0]).map_err(|error| (declared, error))?;
    let declared = declared
        || json
            .as_object()
            .and_then(|object| object.get("format"))
            .and_then(Value::as_str)
            == Some("jobworkerp-description");
    Ok((declared, json))
}

fn yaml_to_json(value: &Yaml) -> Result<Value> {
    match value {
        Yaml::Null => Ok(Value::Null),
        Yaml::Boolean(value) => Ok(Value::Bool(*value)),
        Yaml::Integer(value) => Ok(Value::Number(Number::from(*value))),
        Yaml::Real(value) => {
            if value.eq_ignore_ascii_case(".nan") || value.to_ascii_lowercase().contains(".inf") {
                return Err(anyhow!("non-finite YAML number is not supported"));
            }
            let number = value.parse::<Number>().map_err(|_| {
                anyhow!("YAML number cannot be represented exactly as JSON; quote it as a string")
            })?;
            if canonical_decimal(value)? != canonical_decimal(&number.to_string())? {
                return Err(anyhow!(
                    "YAML number cannot be represented exactly as JSON; quote it as a string"
                ));
            }
            Ok(Value::Number(number))
        }
        Yaml::String(value) => Ok(Value::String(value.clone())),
        Yaml::Array(values) => values
            .iter()
            .map(yaml_to_json)
            .collect::<Result<Vec<_>>>()
            .map(Value::Array),
        Yaml::Hash(values) => {
            let mut object = Map::new();
            for (key, value) in values {
                let Yaml::String(key) = key else {
                    return Err(anyhow!("YAML mapping keys must be strings"));
                };
                if object.insert(key.clone(), yaml_to_json(value)?).is_some() {
                    return Err(anyhow!("duplicate YAML key: {key}"));
                }
            }
            Ok(Value::Object(object))
        }
        Yaml::Alias(_) | Yaml::BadValue => {
            Err(anyhow!("YAML aliases and invalid values are not supported"))
        }
    }
}

/// Normalize a finite JSON number into coefficient and base-10 exponent form.
/// This lets the parser detect lossy conversion to serde_json's finite number
/// representation while accepting equivalent spellings such as `1e3` and
/// `1000.0`.
fn canonical_decimal(value: &str) -> Result<(bool, String, i64)> {
    let value = value.trim();
    let (negative, value) = match value.strip_prefix('-') {
        Some(value) => (true, value),
        None => (false, value.strip_prefix('+').unwrap_or(value)),
    };
    let (coefficient, exponent) = match value.split_once(['e', 'E']) {
        Some((coefficient, exponent)) => (
            coefficient,
            exponent
                .parse::<i64>()
                .map_err(|_| anyhow!("YAML numeric exponent is invalid"))?,
        ),
        None => (value, 0),
    };
    let (integer, fraction) = coefficient.split_once('.').unwrap_or((coefficient, ""));
    if integer.is_empty() && fraction.is_empty()
        || !integer
            .bytes()
            .chain(fraction.bytes())
            .all(|byte| byte.is_ascii_digit())
    {
        return Err(anyhow!("YAML number is invalid"));
    }
    let fractional_digits = i64::try_from(fraction.len())
        .map_err(|_| anyhow!("YAML number exceeds precision limits; quote it as a string"))?;
    let mut digits = format!("{integer}{fraction}");
    let first_non_zero = digits.find(|character: char| character != '0');
    let Some(first_non_zero) = first_non_zero else {
        return Ok((false, "0".to_string(), 0));
    };
    digits.drain(..first_non_zero);
    let mut exponent = exponent
        .checked_sub(fractional_digits)
        .ok_or_else(|| anyhow!("YAML number exponent exceeds the supported range"))?;
    while digits.ends_with('0') {
        digits.pop();
        exponent = exponent
            .checked_add(1)
            .ok_or_else(|| anyhow!("YAML number exponent exceeds the supported range"))?;
    }
    Ok((negative, digits, exponent))
}

#[derive(Default)]
struct Audit {
    documents: usize,
    nodes: usize,
    containers: Vec<ContainerAudit>,
    declared: bool,
    error: Option<String>,
}

#[derive(Default)]
struct MappingAudit {
    expecting_key: bool,
    keys: HashSet<String>,
    pending_format_key: bool,
}

enum ContainerAudit {
    Mapping(MappingAudit),
    Sequence,
}

impl Audit {
    fn fail(&mut self, message: impl Into<String>) {
        self.error.get_or_insert_with(|| message.into());
    }
    fn node(&mut self) {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            self.fail(format!("description YAML exceeds {MAX_NODES} nodes"));
        }
    }
    fn parent_value(&mut self, complex: bool) {
        let mut invalid_key = false;
        if let Some(ContainerAudit::Mapping(mapping)) = self.containers.last_mut() {
            if mapping.expecting_key {
                invalid_key = complex;
                mapping.expecting_key = false;
            } else {
                mapping.expecting_key = true;
            }
        }
        if invalid_key {
            self.fail("YAML mapping keys must be strings");
        }
    }
    fn finish(self) -> Result<()> {
        if let Some(error) = self.error {
            Err(anyhow!(error))
        } else if self.documents != 1 {
            Err(anyhow!(
                "structured description must contain exactly one YAML document"
            ))
        } else {
            Ok(())
        }
    }
}

fn audit_number_limits(value: &str) -> Result<()> {
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.' | b'e' | b'E'))
    {
        return Ok(());
    }
    let value = value.strip_prefix(['+', '-']).unwrap_or(value);
    let (coefficient, exponent) = match value.split_once(['e', 'E']) {
        Some((coefficient, exponent)) => (
            coefficient,
            exponent.parse::<i32>().map_err(|_| {
                anyhow!("YAML numeric exponent is invalid or exceeds the supported range")
            })?,
        ),
        None => (value, 0),
    };
    let digits = coefficient.bytes().filter(u8::is_ascii_digit).count();
    if digits == 0 {
        return Ok(());
    }
    if digits > MAX_NUMBER_DIGITS || exponent.unsigned_abs() > MAX_NUMBER_EXPONENT as u32 {
        return Err(anyhow!(
            "YAML number exceeds precision limits; quote it as a string"
        ));
    }
    Ok(())
}

impl EventReceiver for Audit {
    fn on_event(&mut self, event: Event) {
        match event {
            Event::DocumentStart => self.documents += 1,
            Event::Alias(_) => self.fail("YAML aliases are not supported"),
            Event::Scalar(value, style, anchor, tag) => {
                self.node();
                if anchor != 0 || tag.is_some() {
                    self.fail("YAML anchors and tags are not supported");
                }
                if value.len() > MAX_SCALAR_BYTES {
                    self.fail(format!(
                        "description YAML scalar exceeds {MAX_SCALAR_BYTES} bytes"
                    ));
                }
                if style == TScalarStyle::Plain
                    && let Err(error) = audit_number_limits(&value)
                {
                    self.fail(error.to_string());
                }
                let mut duplicate = false;
                let is_root_mapping =
                    matches!(self.containers.as_slice(), [ContainerAudit::Mapping(_)]);
                if let Some(ContainerAudit::Mapping(mapping)) = self.containers.last_mut() {
                    if mapping.expecting_key {
                        let is_format_key = is_root_mapping && value == "format";
                        duplicate = !mapping.keys.insert(value);
                        mapping.pending_format_key = is_format_key;
                        mapping.expecting_key = false;
                    } else {
                        if is_root_mapping
                            && mapping.pending_format_key
                            && value == "jobworkerp-description"
                        {
                            self.declared = true;
                        }
                        mapping.pending_format_key = false;
                        mapping.expecting_key = true;
                    }
                }
                if duplicate {
                    self.fail("duplicate YAML mapping key");
                }
            }
            Event::MappingStart(anchor, tag) => {
                self.node();
                self.parent_value(true);
                if anchor != 0 || tag.is_some() {
                    self.fail("YAML anchors and tags are not supported");
                }
                self.containers.push(ContainerAudit::Mapping(MappingAudit {
                    expecting_key: true,
                    keys: HashSet::new(),
                    pending_format_key: false,
                }));
                if self.containers.len() > MAX_DEPTH {
                    self.fail(format!("description YAML exceeds depth {MAX_DEPTH}"));
                }
            }
            Event::SequenceStart(anchor, tag) => {
                self.node();
                self.parent_value(true);
                if anchor != 0 || tag.is_some() {
                    self.fail("YAML anchors and tags are not supported");
                }
                self.containers.push(ContainerAudit::Sequence);
                if self.containers.len() > MAX_DEPTH {
                    self.fail(format!("description YAML exceeds depth {MAX_DEPTH}"));
                }
            }
            Event::MappingEnd => {
                self.containers.pop();
            }
            Event::SequenceEnd => {
                self.containers.pop();
            }
            Event::Nothing | Event::StreamStart | Event::StreamEnd | Event::DocumentEnd => {}
        }
    }
}

fn has_declared_prefix(input: &str) -> bool {
    let input = input.strip_prefix('\u{feff}').unwrap_or(input);
    let mut saw_document_marker = false;
    for line in input.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if !saw_document_marker && trimmed == "---" {
            saw_document_marker = true;
            continue;
        }
        return line.starts_with("format:")
            && line["format:".len()..]
                .trim_start()
                .strip_prefix("jobworkerp-description")
                .is_some_and(|rest| {
                    rest.is_empty()
                        || rest.starts_with(char::is_whitespace)
                        || rest.starts_with('#')
                });
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_a_valid_description() {
        assert!(matches!(
            parse("format: jobworkerp-description\nversion: 1\ndescription: hello"),
            ParsedDescription::Structured(_)
        ));
    }
    #[test]
    fn declared_invalid_yaml_is_not_plain_text() {
        assert!(matches!(
            parse("format: jobworkerp-description\nversion: ["),
            ParsedDescription::InvalidDeclared(_)
        ));
    }
    #[test]
    fn ordinary_text_is_preserved_as_plain() {
        assert!(matches!(parse("ordinary prose"), ParsedDescription::Plain));
    }

    #[test]
    fn duplicate_keys_and_anchors_in_a_declared_document_are_rejected() {
        assert!(matches!(
            parse(
                "format: jobworkerp-description\nformat: jobworkerp-description\nversion: 1\ndescription: hello"
            ),
            ParsedDescription::InvalidDeclared(_)
        ));
        assert!(matches!(
            parse("format: jobworkerp-description\nversion: 1\ndescription: &text hello"),
            ParsedDescription::InvalidDeclared(_)
        ));
        assert!(matches!(
            parse("'format': jobworkerp-description\nversion: 1\ndescription: &text hello"),
            ParsedDescription::InvalidDeclared(_)
        ));
    }

    #[test]
    fn quoted_numeric_text_does_not_trigger_numeric_limits() {
        assert!(matches!(
            parse(
                "format: jobworkerp-description\nversion: 1\ndescription: hello\nparameters:\n  id:\n    description: value\n    examples: [\"1e10001\"]"
            ),
            ParsedDescription::Structured(_)
        ));
    }

    #[test]
    fn core_scalar_examples_preserve_strings_and_parse_prefixed_integers() {
        let ParsedDescription::Structured(description) = parse(
            "format: jobworkerp-description\nversion: 1\ndescription: hello\nparameters:\n  value:\n    description: value\n    examples: [on, off, yes, no, 012, 0o12, 0x10]",
        ) else {
            panic!("valid Core Schema document should be structured");
        };
        assert_eq!(
            description.parameters["value"]["examples"],
            serde_json::json!(["on", "off", "yes", "no", 12, 10, 16])
        );
    }

    #[test]
    fn integers_outside_json_number_range_are_rejected_without_rounding() {
        let parsed = parse(
            "format: jobworkerp-description\nversion: 1\ndescription: hello\nparameters:\n  value:\n    description: value\n    examples: [18446744073709551616]",
        );
        assert!(matches!(parsed, ParsedDescription::InvalidDeclared(_)));
    }

    #[test]
    fn examples_sequences_do_not_participate_in_mapping_key_audit() {
        for examples in ["[a, b, a]", "[{name: hello}, {name: world}]"] {
            let yaml = format!(
                "format: jobworkerp-description\nversion: 1\ndescription: hello\nparameters:\n  value:\n    description: value\n    examples: {examples}"
            );
            assert!(
                matches!(parse(&yaml), ParsedDescription::Structured(_)),
                "valid examples were rejected: {examples}"
            );
        }
    }
}
