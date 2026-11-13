//! The one hook pipeline.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use oxkv_contracts::version::{CommitVersion, Hash32, ShardId};

use crate::batch::{Key, Transaction};
use crate::budget::HookBudget;
use crate::error::{HookError, PipelineBuildError};
use crate::event::{EventKind, Notification, NotificationSeed};
use crate::post::{CommittedView, PostWriteHook};
use crate::pre::{PreWriteHook, ValidatorView};
use crate::rule::RuleId;

/// The ordered build inputs for one hook pipeline.
pub struct HookPipelineBuilder {
    pre: Vec<Arc<dyn PreWriteHook>>,
    post: Vec<Arc<dyn PostWriteHook>>,
    budget: HookBudget,
}

impl HookPipelineBuilder {
    /// Creates an empty builder with the default budget.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pre: Vec::new(),
            post: Vec::new(),
            budget: HookBudget::default(),
        }
    }

    /// Sets the resource budget.
    #[must_use]
    pub const fn with_budget(mut self, budget: HookBudget) -> Self {
        self.budget = budget;
        self
    }

    /// Registers one pre-write rule.
    ///
    /// # Errors
    ///
    /// Returns `internal.invariant_violation` when the rule id is empty or
    /// already registered in the pre-write phase.
    pub fn register_pre(
        &mut self,
        hook: impl PreWriteHook + 'static,
    ) -> Result<(), PipelineBuildError> {
        let rule_id = hook.rule_id().clone();
        check_registration(&rule_id, self.pre.iter().map(|existing| existing.rule_id()))?;
        self.pre.push(Arc::new(hook));
        Ok(())
    }

    /// Registers one post-write rule.
    ///
    /// # Errors
    ///
    /// Returns `internal.invariant_violation` when the rule id is empty or
    /// already registered in the post-write phase.
    pub fn register_post(
        &mut self,
        hook: impl PostWriteHook + 'static,
    ) -> Result<(), PipelineBuildError> {
        let rule_id = hook.rule_id().clone();
        check_registration(
            &rule_id,
            self.post.iter().map(|existing| existing.rule_id()),
        )?;
        self.post.push(Arc::new(hook));
        Ok(())
    }

    /// Builds the pipeline.
    #[must_use]
    pub fn build(self) -> HookPipeline {
        let fingerprint = compute_pipeline_fingerprint(&self.pre, &self.post, self.budget);
        HookPipeline {
            pre: self.pre,
            post: self.post,
            budget: self.budget,
            fingerprint,
        }
    }
}

impl Default for HookPipelineBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for HookPipelineBuilder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let pre: Vec<&str> = self
            .pre
            .iter()
            .map(|hook| hook.rule_id().as_str())
            .collect();
        let post: Vec<&str> = self
            .post
            .iter()
            .map(|hook| hook.rule_id().as_str())
            .collect();
        formatter
            .debug_struct("HookPipelineBuilder")
            .field("pre", &pre)
            .field("post", &post)
            .field("budget", &self.budget)
            .finish()
    }
}

fn check_registration<'a>(
    rule_id: &RuleId,
    existing: impl Iterator<Item = &'a RuleId>,
) -> Result<(), PipelineBuildError> {
    if rule_id.is_empty() {
        return Err(PipelineBuildError::empty(rule_id.clone()));
    }
    if existing.into_iter().any(|candidate| candidate == rule_id) {
        return Err(PipelineBuildError::duplicate(rule_id.clone()));
    }
    Ok(())
}

/// One ordered pipeline with pre-write and post-write rules.
#[derive(Clone)]
pub struct HookPipeline {
    pre: Vec<Arc<dyn PreWriteHook>>,
    post: Vec<Arc<dyn PostWriteHook>>,
    budget: HookBudget,
    fingerprint: Hash32,
}

impl HookPipeline {
    /// Creates an empty pipeline builder.
    #[must_use]
    pub fn builder() -> HookPipelineBuilder {
        HookPipelineBuilder::new()
    }

    /// Returns the pre-write rule ids in registration order.
    pub fn pre_rule_ids(&self) -> impl Iterator<Item = &RuleId> {
        self.pre.iter().map(|hook| hook.rule_id())
    }

    /// Returns the post-write rule ids in registration order.
    pub fn post_rule_ids(&self) -> impl Iterator<Item = &RuleId> {
        self.post.iter().map(|hook| hook.rule_id())
    }

    /// Returns the resource budget.
    #[must_use]
    pub const fn budget(&self) -> HookBudget {
        self.budget
    }

    /// Runs pre-write validation.
    ///
    /// # Errors
    ///
    /// Returns the first named hook failure, or a named budget error.
    pub fn validate<'a>(
        &self,
        transaction: &'a Transaction,
    ) -> Result<ValidatedBatch<'a>, HookError> {
        self.check_batch(transaction)?;
        let mut steps = 0_u32;
        for (index, op) in transaction.ops().iter().enumerate() {
            let op_index = to_u32(index);
            for hook in &self.pre {
                self.consume_step(&mut steps)?;
                let view = ValidatorView::new(transaction, index);
                hook.validate(&view)
                    .map_err(|error| error.attach(hook.rule_id(), op_index, op.key()))?;
            }
        }
        Ok(ValidatedBatch {
            transaction,
            batch_fingerprint: transaction.fingerprint(),
            pipeline_fingerprint: self.fingerprint,
        })
    }

    /// Runs post-write derivation inside the commit step.
    ///
    /// # Errors
    ///
    /// Returns the first named hook failure, or a named budget error. A
    /// failure produces no commit outcome.
    pub fn commit(
        &self,
        validated: ValidatedBatch<'_>,
        commit_version: CommitVersion,
    ) -> Result<CommitOutcome, HookError> {
        if validated.pipeline_fingerprint != self.fingerprint {
            return Err(HookError::invariant(
                "The validation proof belongs to another pipeline.",
            ));
        }
        let transaction = validated.transaction;
        let final_state = transaction.staged_state();
        let mut notifications = Vec::new();
        let mut steps = 0_u32;
        let mut emission_index = 0_u32;
        for (index, op) in transaction.ops().iter().enumerate() {
            let op_index = to_u32(index);
            for hook in &self.post {
                self.consume_step(&mut steps)?;
                let view = CommittedView::new(transaction, final_state, index, commit_version);
                let draft = hook
                    .derive(&view)
                    .map_err(|error| error.attach(hook.rule_id(), op_index, op.key()))?;
                if let Some(draft) = draft {
                    if draft.kind().is_empty() {
                        return Err(HookError::invariant("The event kind is empty.").attach(
                            hook.rule_id(),
                            op_index,
                            op.key(),
                        ));
                    }
                    self.check_event(
                        &notifications,
                        draft.payload().len(),
                        hook,
                        op_index,
                        op.key(),
                    )?;
                    let kind = EventKind::new(draft.kind().as_str());
                    notifications.push(Notification::new(NotificationSeed {
                        shard_id: transaction.shard_id().clone(),
                        commit_version,
                        key: op.key().clone(),
                        kind,
                        rule_id: hook.rule_id().clone(),
                        emission_index,
                        op_index,
                        payload: draft.payload().to_vec(),
                    }));
                    emission_index = emission_index
                        .checked_add(1)
                        .ok_or_else(|| HookError::invariant("The emission index is too large."))?;
                }
            }
        }
        let state = final_state.clone();
        Ok(CommitOutcome {
            shard_id: transaction.shard_id().clone(),
            commit_version,
            state,
            notifications,
            batch_fingerprint: validated.batch_fingerprint,
        })
    }

    /// Runs validation and then the commit step.
    ///
    /// # Errors
    ///
    /// Returns the first named hook failure, or a named budget error.
    pub fn execute(
        &self,
        transaction: &Transaction,
        commit_version: CommitVersion,
    ) -> Result<CommitOutcome, HookError> {
        let validated = self.validate(transaction)?;
        self.commit(validated, commit_version)
    }

    fn check_batch(&self, transaction: &Transaction) -> Result<(), HookError> {
        if transaction.op_count() > max_as_usize(self.budget.batch_ops) {
            return Err(HookError::batch_too_large(
                "The batch exceeds the operation budget.",
            ));
        }
        for (index, op) in transaction.ops().iter().enumerate() {
            if let Some(value) = op.value()
                && value.len() > max_as_usize(self.budget.value_bytes)
            {
                return Err(HookError::value_too_large(
                    "value",
                    "The put value exceeds the value budget.",
                )
                .with_op_index(to_u32(index))
                .with_key(op.key().clone()));
            }
        }
        Ok(())
    }

    fn consume_step(&self, steps: &mut u32) -> Result<(), HookError> {
        if *steps >= self.budget.steps {
            return Err(HookError::budget_exhausted(
                "The hook step budget is exhausted.",
            ));
        }
        *steps += 1;
        Ok(())
    }

    fn check_event(
        &self,
        notifications: &[Notification],
        payload_len: usize,
        hook: &Arc<dyn PostWriteHook>,
        op_index: u32,
        key: &Key,
    ) -> Result<(), HookError> {
        if notifications.len() >= max_as_usize(self.budget.events) {
            return Err(HookError::budget_exhausted(
                "The notification count exceeds the event budget.",
            )
            .attach(hook.rule_id(), op_index, key));
        }
        if payload_len > max_as_usize(self.budget.event_bytes) {
            return Err(HookError::event_payload_too_large(
                "The notification payload exceeds the event budget.",
            )
            .attach(hook.rule_id(), op_index, key));
        }
        Ok(())
    }
}

impl Default for HookPipeline {
    fn default() -> Self {
        Self::builder().build()
    }
}

impl fmt::Debug for HookPipeline {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let pre: Vec<&str> = self
            .pre
            .iter()
            .map(|hook| hook.rule_id().as_str())
            .collect();
        let post: Vec<&str> = self
            .post
            .iter()
            .map(|hook| hook.rule_id().as_str())
            .collect();
        formatter
            .debug_struct("HookPipeline")
            .field("pre", &pre)
            .field("post", &post)
            .field("budget", &self.budget)
            .field("fingerprint", &self.fingerprint)
            .finish()
    }
}

/// The proof that one transaction passed pre-write validation.
#[derive(Clone, Copy)]
pub struct ValidatedBatch<'a> {
    transaction: &'a Transaction,
    batch_fingerprint: Hash32,
    pipeline_fingerprint: Hash32,
}

impl<'a> ValidatedBatch<'a> {
    /// Returns the validated transaction.
    #[must_use]
    pub const fn transaction(&self) -> &'a Transaction {
        self.transaction
    }

    /// Returns the batch fingerprint.
    #[must_use]
    pub const fn batch_fingerprint(&self) -> Hash32 {
        self.batch_fingerprint
    }
}

/// The committed state and ordered notifications of one accepted batch.
///
/// The value exists only after all post-write rules succeed. A post-write
/// failure returns an error and no outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitOutcome {
    shard_id: ShardId,
    commit_version: CommitVersion,
    state: BTreeMap<Key, Option<Vec<u8>>>,
    notifications: Vec<Notification>,
    batch_fingerprint: Hash32,
}

impl CommitOutcome {
    /// Returns the shard identifier.
    #[must_use]
    pub const fn shard_id(&self) -> &ShardId {
        &self.shard_id
    }

    /// Returns the commit version.
    #[must_use]
    pub const fn commit_version(&self) -> CommitVersion {
        self.commit_version
    }

    /// Returns the final committed value for every touched key.
    #[must_use]
    pub const fn state(&self) -> &BTreeMap<Key, Option<Vec<u8>>> {
        &self.state
    }

    /// Returns the committed value of one touched key.
    ///
    /// The result is `None` when the key is absent or untouched. Use
    /// [`state`](Self::state) to tell the two cases apart.
    #[must_use]
    pub fn applied_value(&self, key: &Key) -> Option<&[u8]> {
        self.state.get(key).and_then(Option::as_deref)
    }

    /// Returns the ordered notifications.
    #[must_use]
    pub fn notifications(&self) -> &[Notification] {
        &self.notifications
    }

    /// Returns the number of notifications.
    #[must_use]
    pub fn notification_count(&self) -> usize {
        self.notifications.len()
    }

    /// Returns the canonical bytes of every notification in order.
    #[must_use]
    pub fn notification_bytes(&self) -> Vec<Vec<u8>> {
        self.notifications
            .iter()
            .map(Notification::to_bytes)
            .collect()
    }

    /// Returns the batch fingerprint.
    #[must_use]
    pub const fn batch_fingerprint(&self) -> Hash32 {
        self.batch_fingerprint
    }
}

fn to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn max_as_usize(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

fn compute_pipeline_fingerprint(
    pre: &[Arc<dyn PreWriteHook>],
    post: &[Arc<dyn PostWriteHook>],
    budget: HookBudget,
) -> Hash32 {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"oxkv-hooks-pipeline/v1\0");
    for hook in pre {
        bytes.push(1);
        push_len(&mut bytes, hook.rule_id().as_str().len());
        bytes.extend_from_slice(hook.rule_id().as_str().as_bytes());
    }
    for hook in post {
        bytes.push(2);
        push_len(&mut bytes, hook.rule_id().as_str().len());
        bytes.extend_from_slice(hook.rule_id().as_str().as_bytes());
    }
    for value in [
        budget.steps,
        budget.events,
        budget.event_bytes,
        budget.value_bytes,
        budget.batch_ops,
    ] {
        bytes.extend_from_slice(&value.to_be_bytes());
    }
    Hash32::of(&bytes)
}

fn push_len(bytes: &mut Vec<u8>, len: usize) {
    bytes.extend_from_slice(&u64::try_from(len).unwrap_or(u64::MAX).to_be_bytes());
}
