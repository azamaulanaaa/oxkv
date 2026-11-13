#![allow(missing_docs)]

//! Shared support for the hook golden vectors.

use std::collections::BTreeMap;

use oxkv_contracts::version::{KeyVersion, ShardId};
use oxkv_hooks::{
    CommitOutcome, CommittedView, EventDraft, EventKind, HookBudget, HookError, HookPipeline, Key,
    PostWriteHook, PreWriteHook, PriorRow, RuleId, StagedOp, Transaction, ValidatorView,
};
use serde_json::Value;

/// One golden case.
pub(crate) struct GoldenCase {
    pub(crate) name: String,
    pub(crate) expected: Value,
}

/// Reads one golden case.
pub(crate) fn case(name: &str) -> GoldenCase {
    let text = include_str!("../vectors/hooks.json");
    let value: Value = serde_json::from_str(text).expect("the golden file is valid JSON");
    let case = value["cases"]
        .as_array()
        .expect("the golden file has a case list")
        .iter()
        .find(|case| case["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("the golden case {name} is absent"));
    GoldenCase {
        name: name.to_owned(),
        expected: case["expected"].clone(),
    }
}

/// Returns one string field from a golden case.
pub(crate) fn expected_str<'a>(case: &'a GoldenCase, field: &str) -> &'a str {
    case.expected[field]
        .as_str()
        .unwrap_or_else(|| panic!("the case {} has no {field} field", case.name))
}

/// Returns one boolean field from a golden case.
pub(crate) fn expected_bool(case: &GoldenCase, field: &str) -> bool {
    case.expected[field]
        .as_bool()
        .unwrap_or_else(|| panic!("the case {} has no {field} field", case.name))
}

/// Returns one integer field from a golden case.
pub(crate) fn expected_u64(case: &GoldenCase, field: &str) -> u64 {
    case.expected[field]
        .as_u64()
        .unwrap_or_else(|| panic!("the case {} has no {field} field", case.name))
}

/// Returns one string array field from a golden case.
pub(crate) fn expected_strings(case: &GoldenCase, field: &str) -> Vec<String> {
    case.expected[field]
        .as_array()
        .unwrap_or_else(|| panic!("the case {} has no {field} field", case.name))
        .iter()
        .map(|value| {
            value
                .as_str()
                .expect("the golden array holds strings")
                .to_owned()
        })
        .collect()
}

/// Returns one integer array field from a golden case.
pub(crate) fn expected_u64s(case: &GoldenCase, field: &str) -> Vec<u64> {
    case.expected[field]
        .as_array()
        .unwrap_or_else(|| panic!("the case {} has no {field} field", case.name))
        .iter()
        .map(|value| value.as_u64().expect("the golden array holds integers"))
        .collect()
}

/// Returns the lowercase hexadecimal form of a byte slice.
pub(crate) fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(HEX[usize::from(byte >> 4)]));
        text.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    text
}

/// A pre-write rule that accepts every operation.
pub(crate) struct AllowAll {
    rule_id: RuleId,
}

impl AllowAll {
    pub(crate) fn new() -> Self {
        Self {
            rule_id: RuleId::new("rule.allow_all"),
        }
    }
}

impl PreWriteHook for AllowAll {
    fn rule_id(&self) -> &RuleId {
        &self.rule_id
    }

    fn validate(&self, _view: &ValidatorView<'_>) -> Result<(), HookError> {
        Ok(())
    }
}

/// A pre-write rule that rejects one key.
pub(crate) struct RejectForbidden {
    rule_id: RuleId,
}

impl RejectForbidden {
    pub(crate) fn new() -> Self {
        Self {
            rule_id: RuleId::new("rule.reject_forbidden"),
        }
    }
}

impl PreWriteHook for RejectForbidden {
    fn rule_id(&self) -> &RuleId {
        &self.rule_id
    }

    fn validate(&self, view: &ValidatorView<'_>) -> Result<(), HookError> {
        if view.current_key().as_bytes() == b"forbidden" {
            return Err(HookError::unknown_field(
                "key",
                "The key is not declared by the rule.",
            ));
        }
        Ok(())
    }
}

/// A pre-write rule that reads a key outside the envelope.
pub(crate) struct ReadOutside {
    rule_id: RuleId,
}

impl ReadOutside {
    pub(crate) fn new() -> Self {
        Self {
            rule_id: RuleId::new("rule.read_outside"),
        }
    }
}

impl PreWriteHook for ReadOutside {
    fn rule_id(&self) -> &RuleId {
        &self.rule_id
    }

    fn validate(&self, view: &ValidatorView<'_>) -> Result<(), HookError> {
        view.value(&Key::from("outside")).map(|_value| ())
    }
}

/// A pre-write rule that checks the validator view exception.
pub(crate) struct CheckPreValue {
    rule_id: RuleId,
}

impl CheckPreValue {
    pub(crate) fn new() -> Self {
        Self {
            rule_id: RuleId::new("rule.check_pre_value"),
        }
    }
}

impl PreWriteHook for CheckPreValue {
    fn rule_id(&self) -> &RuleId {
        &self.rule_id
    }

    fn validate(&self, view: &ValidatorView<'_>) -> Result<(), HookError> {
        if view.current_key().as_bytes() != b"alpha" {
            return Ok(());
        }
        if view.value(view.current_key())? != Some(b"old".as_slice()) {
            return Err(HookError::invariant(
                "The validator did not read the pre-transaction value.",
            ));
        }
        if view.staged_value() != Some(b"3".as_slice()) {
            return Err(HookError::invariant(
                "The validator did not read the final staged value.",
            ));
        }
        Ok(())
    }
}

/// A post-write rule that emits one change notification.
pub(crate) struct EmitChanged {
    rule_id: RuleId,
}

impl EmitChanged {
    pub(crate) fn new() -> Self {
        Self {
            rule_id: RuleId::new("rule.emit_changed"),
        }
    }
}

impl PostWriteHook for EmitChanged {
    fn rule_id(&self) -> &RuleId {
        &self.rule_id
    }

    fn derive(&self, view: &CommittedView<'_>) -> Result<Option<EventDraft>, HookError> {
        let mut payload = view.key().as_bytes().to_vec();
        match view.value_after() {
            Some(value) => {
                payload.push(b'=');
                payload.extend_from_slice(value);
            }
            None => payload.extend_from_slice(b"=deleted"),
        }
        Ok(Some(EventDraft::new(
            EventKind::new("key_changed"),
            payload,
        )))
    }
}

/// A post-write rule that emits one audit notification.
pub(crate) struct EmitAudit {
    rule_id: RuleId,
}

impl EmitAudit {
    pub(crate) fn new() -> Self {
        Self {
            rule_id: RuleId::new("rule.emit_audit"),
        }
    }
}

impl PostWriteHook for EmitAudit {
    fn rule_id(&self) -> &RuleId {
        &self.rule_id
    }

    fn derive(&self, view: &CommittedView<'_>) -> Result<Option<EventDraft>, HookError> {
        Ok(Some(EventDraft::new(
            EventKind::new("audit"),
            view.key().as_bytes().to_vec(),
        )))
    }
}

/// A post-write rule that fails on one key.
pub(crate) struct FailOnKey {
    rule_id: RuleId,
}

impl FailOnKey {
    pub(crate) fn new() -> Self {
        Self {
            rule_id: RuleId::new("rule.fail_on_key"),
        }
    }
}

impl PostWriteHook for FailOnKey {
    fn rule_id(&self) -> &RuleId {
        &self.rule_id
    }

    fn derive(&self, view: &CommittedView<'_>) -> Result<Option<EventDraft>, HookError> {
        if view.key().as_bytes() == b"fail" {
            return Err(HookError::invariant("The post-write rule failed."));
        }
        Ok(None)
    }
}

/// Builds the ordered pipeline with two post-write rules.
pub(crate) fn ordered_pipeline() -> HookPipeline {
    let mut builder = HookPipeline::builder();
    builder
        .register_pre(AllowAll::new())
        .expect("the rule is unique");
    builder
        .register_post(EmitChanged::new())
        .expect("the rule is unique");
    builder
        .register_post(EmitAudit::new())
        .expect("the rule is unique");
    builder.build()
}

/// Builds a pipeline that rejects one key before the commit.
pub(crate) fn rejecting_pipeline() -> HookPipeline {
    let mut builder = HookPipeline::builder();
    builder
        .register_pre(RejectForbidden::new())
        .expect("the rule is unique");
    builder
        .register_post(EmitChanged::new())
        .expect("the rule is unique");
    builder.build()
}

/// Builds a pipeline with one failing post-write rule.
pub(crate) fn failing_pipeline() -> HookPipeline {
    let mut builder = HookPipeline::builder();
    builder
        .register_pre(AllowAll::new())
        .expect("the rule is unique");
    builder
        .register_post(EmitChanged::new())
        .expect("the rule is unique");
    builder
        .register_post(FailOnKey::new())
        .expect("the rule is unique");
    builder.build()
}

/// Builds a pipeline that reads outside the envelope.
pub(crate) fn outside_pipeline() -> HookPipeline {
    let mut builder = HookPipeline::builder();
    builder
        .register_pre(ReadOutside::new())
        .expect("the rule is unique");
    builder.build()
}

/// Builds a pipeline that checks the validator view exception.
pub(crate) fn pre_value_pipeline() -> HookPipeline {
    let mut builder = HookPipeline::builder();
    builder
        .register_pre(CheckPreValue::new())
        .expect("the rule is unique");
    builder.build()
}

/// Builds a pipeline with a small value budget.
pub(crate) fn value_budget_pipeline() -> HookPipeline {
    let mut builder = HookPipeline::builder().with_budget(HookBudget {
        value_bytes: 2,
        ..HookBudget::default()
    });
    builder
        .register_pre(AllowAll::new())
        .expect("the rule is unique");
    builder.build()
}

/// Builds a pipeline with no step budget.
pub(crate) fn step_budget_pipeline() -> HookPipeline {
    let mut builder = HookPipeline::builder().with_budget(HookBudget {
        steps: 0,
        ..HookBudget::default()
    });
    builder
        .register_pre(AllowAll::new())
        .expect("the rule is unique");
    builder.build()
}

/// Builds a pipeline with a small batch budget.
pub(crate) fn batch_budget_pipeline() -> HookPipeline {
    let mut builder = HookPipeline::builder().with_budget(HookBudget {
        batch_ops: 1,
        ..HookBudget::default()
    });
    builder
        .register_pre(AllowAll::new())
        .expect("the rule is unique");
    builder.build()
}

/// Builds a pipeline with a small event payload budget.
pub(crate) fn payload_budget_pipeline() -> HookPipeline {
    let mut builder = HookPipeline::builder().with_budget(HookBudget {
        event_bytes: 1,
        ..HookBudget::default()
    });
    builder
        .register_post(EmitChanged::new())
        .expect("the rule is unique");
    builder.build()
}

/// Returns one transaction with three operations.
pub(crate) fn sample_transaction() -> Transaction {
    Transaction::from_ops(
        ShardId::new(vec![1]),
        vec![
            StagedOp::put(Key::from("alpha"), b"1".to_vec(), KeyVersion::new(0), None),
            StagedOp::put(Key::from("beta"), b"2".to_vec(), KeyVersion::new(0), None),
            StagedOp::put(Key::from("alpha"), b"3".to_vec(), KeyVersion::new(0), None),
        ],
    )
    .expect("the batch is valid")
}

/// Returns one transaction with a prior value and a later staged value.
pub(crate) fn prior_transaction() -> Transaction {
    let prior = prior_row(b"old", 4);
    Transaction::from_ops(
        ShardId::new(vec![1]),
        vec![
            StagedOp::put(
                Key::from("alpha"),
                b"1".to_vec(),
                KeyVersion::new(4),
                Some(prior.clone()),
            ),
            StagedOp::put(
                Key::from("alpha"),
                b"3".to_vec(),
                KeyVersion::new(4),
                Some(prior),
            ),
        ],
    )
    .expect("the batch is valid")
}

/// Returns one transaction with a forbidden key.
pub(crate) fn rejected_transaction() -> Transaction {
    Transaction::from_ops(
        ShardId::new(vec![1]),
        vec![
            StagedOp::put(Key::from("keep"), b"1".to_vec(), KeyVersion::new(0), None),
            StagedOp::put(
                Key::from("forbidden"),
                b"2".to_vec(),
                KeyVersion::new(0),
                None,
            ),
        ],
    )
    .expect("the batch is valid")
}

/// Returns one transaction with a failing post-write key.
pub(crate) fn failing_transaction() -> Transaction {
    Transaction::from_ops(
        ShardId::new(vec![1]),
        vec![
            StagedOp::put(Key::from("keep"), b"1".to_vec(), KeyVersion::new(0), None),
            StagedOp::put(Key::from("fail"), b"2".to_vec(), KeyVersion::new(0), None),
            StagedOp::put(Key::from("after"), b"3".to_vec(), KeyVersion::new(0), None),
        ],
    )
    .expect("the batch is valid")
}

/// Returns one transaction with a value above the small budget.
pub(crate) fn large_value_transaction() -> Transaction {
    Transaction::from_ops(
        ShardId::new(vec![1]),
        vec![StagedOp::put(
            Key::from("alpha"),
            b"123".to_vec(),
            KeyVersion::new(0),
            None,
        )],
    )
    .expect("the batch is valid")
}

/// Returns the pre-transaction state of one transaction.
pub(crate) fn pre_state(transaction: &Transaction) -> BTreeMap<Key, Option<Vec<u8>>> {
    transaction
        .pre_state()
        .iter()
        .map(|(key, prior)| (key.clone(), prior.as_ref().map(|row| row.value().to_vec())))
        .collect()
}

/// Applies one outcome to a test store.
pub(crate) fn apply(outcome: &CommitOutcome, state: &mut BTreeMap<Key, Option<Vec<u8>>>) {
    for (key, value) in outcome.state() {
        state.insert(key.clone(), value.clone());
    }
}

/// Returns one prior row with a modest key version.
pub(crate) fn prior_row(value: &[u8], key_version: u64) -> PriorRow {
    PriorRow::new(value.to_vec(), KeyVersion::new(key_version))
}
