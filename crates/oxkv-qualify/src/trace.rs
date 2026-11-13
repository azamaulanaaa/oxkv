//! One normalized trace.
//!
//! A trace records ordered steps. Each step has a canonical operation name and
//! a sorted field map. The canonical text is stable across targets. The trace
//! feeds both the report and the narration.

use core::fmt::Write as _;

use oxkv_contracts::version::Hash32;

use crate::canonical::hex;

/// One normalized step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceStep {
    sequence: u32,
    operation: String,
    fields: std::collections::BTreeMap<String, String>,
}

impl TraceStep {
    /// Returns the sequence number. It starts at zero.
    #[must_use]
    pub const fn sequence(&self) -> u32 {
        self.sequence
    }

    /// Returns the canonical operation name.
    #[must_use]
    pub fn operation(&self) -> &str {
        &self.operation
    }

    /// Returns one field value.
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str)
    }

    /// Returns every field in canonical order.
    #[must_use]
    pub fn fields(&self) -> &std::collections::BTreeMap<String, String> {
        &self.fields
    }

    /// Returns the canonical step text.
    #[must_use]
    pub fn canonical_text(&self) -> String {
        let mut text = String::new();
        let _ = write!(text, "{}|{}", self.sequence, self.operation);
        for (name, value) in &self.fields {
            let _ = write!(text, "|{name}={value}");
        }
        text
    }
}

/// An ordered, normalized trace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NormalizedTrace {
    steps: Vec<TraceStep>,
}

impl NormalizedTrace {
    /// Creates an empty trace.
    #[must_use]
    pub fn new() -> Self {
        Self { steps: Vec::new() }
    }

    /// Appends one step.
    ///
    /// The operation and the field names are normalized to lowercase ASCII
    /// tokens. A character outside the token set becomes `_`. Field names are
    /// sorted. A field value loses a line break and a control byte.
    pub fn record(&mut self, operation: &str, fields: &[(&str, String)]) {
        let sequence = u32::try_from(self.steps.len()).unwrap_or(u32::MAX);
        let mut map = std::collections::BTreeMap::new();
        for (name, value) in fields {
            map.insert(normalize_token(name), normalize_value(value));
        }
        self.steps.push(TraceStep {
            sequence,
            operation: normalize_token(operation),
            fields: map,
        });
    }

    /// Returns every step.
    #[must_use]
    pub fn steps(&self) -> &[TraceStep] {
        &self.steps
    }

    /// Returns the number of steps.
    #[must_use]
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Returns true when the trace has no step.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Returns the canonical text.
    ///
    /// Each step ends with a line feed. The text has no trailing blank line.
    #[must_use]
    pub fn canonical_text(&self) -> String {
        let mut text = String::new();
        for step in &self.steps {
            text.push_str(&step.canonical_text());
            text.push('\n');
        }
        text
    }

    /// Returns the canonical bytes.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        self.canonical_text().into_bytes()
    }

    /// Returns the BLAKE3 digest of the canonical bytes.
    #[must_use]
    pub fn digest(&self) -> Hash32 {
        Hash32::of(&self.canonical_bytes())
    }

    /// Returns the digest as lowercase hexadecimal text.
    #[must_use]
    pub fn digest_hex(&self) -> String {
        hex(self.digest().as_bytes())
    }
}

fn normalize_token(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        let valid =
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'.' || byte == b'_';
        if valid {
            out.push(char::from(byte));
        } else if byte.is_ascii_uppercase() {
            out.push(char::from(byte.to_ascii_lowercase()));
        } else {
            out.push('_');
        }
    }
    out
}

fn normalize_value(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        if character == '\n' || character == '\r' || character == '|' {
            out.push('_');
        } else if !character.is_control() {
            out.push(character);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::NormalizedTrace;

    #[test]
    fn a_trace_normalizes_a_name_and_a_value() {
        let mut trace = NormalizedTrace::new();
        trace.record("Write Row", &[("Key", "a\nb|c".to_owned())]);
        assert_eq!(trace.steps()[0].operation(), "write_row");
        assert_eq!(trace.steps()[0].field("key"), Some("a_b_c"));
    }

    #[test]
    fn field_order_does_not_change_the_digest() {
        let mut left = NormalizedTrace::new();
        left.record("step", &[("b", "2".to_owned()), ("a", "1".to_owned())]);
        let mut right = NormalizedTrace::new();
        right.record("step", &[("a", "1".to_owned()), ("b", "2".to_owned())]);
        assert_eq!(left.digest(), right.digest());
        assert_eq!(left.canonical_text(), right.canonical_text());
    }

    #[test]
    fn a_step_sequence_is_monotonic() {
        let mut trace = NormalizedTrace::new();
        trace.record("one", &[]);
        trace.record("two", &[]);
        assert_eq!(trace.steps()[0].sequence(), 0);
        assert_eq!(trace.steps()[1].sequence(), 1);
        assert_eq!(trace.len(), 2);
    }
}
