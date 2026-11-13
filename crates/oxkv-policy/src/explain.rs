//! Stable explanation bytes for one evaluation.
//!
//! The bytes use fixed little-endian integers and no platform-dependent value.
//! Native and `wasm32-unknown-unknown` produce the same bytes for the same
//! evaluation.

use crate::eval::{Evaluation, EvaluationStatus};

const EXPLANATION_MAGIC: &[u8] = b"oxkv/policy-explain/v1\x00";

impl Evaluation {
    /// Returns the canonical explanation bytes.
    ///
    /// The bytes carry the row key, the rule identity, the status, the reason
    /// code, the evaluated path, and any budget failure detail.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(EXPLANATION_MAGIC);
        write_bytes(&mut out, &self.row_key);
        write_text(&mut out, self.rule.as_str());
        out.push(status_tag(self.status));
        if let EvaluationStatus::BudgetExhausted(failure) = self.status {
            out.push(failure.budget.tag());
            out.extend_from_slice(&failure.limit.to_le_bytes());
            out.extend_from_slice(&failure.used.to_le_bytes());
            out.extend_from_slice(&failure.depth.to_le_bytes());
        }
        write_text(&mut out, self.reason.text());
        match &self.path {
            Some(path) => {
                out.push(1);
                out.push(path.root().tag());
                write_count(
                    &mut out,
                    u64::try_from(path.segments().len()).unwrap_or(u64::MAX),
                );
                for segment in path.segments() {
                    write_text(&mut out, segment);
                }
            }
            None => out.push(0),
        }
        out
    }
}

const fn status_tag(status: EvaluationStatus) -> u8 {
    match status {
        EvaluationStatus::Allow => 0,
        EvaluationStatus::Deny => 1,
        EvaluationStatus::BudgetExhausted(_) => 2,
    }
}

fn write_count(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_text(out: &mut Vec<u8>, value: &str) {
    write_bytes(out, value.as_bytes());
}

fn write_bytes(out: &mut Vec<u8>, value: &[u8]) {
    write_count(out, u64::try_from(value.len()).unwrap_or(u64::MAX));
    out.extend_from_slice(value);
}
