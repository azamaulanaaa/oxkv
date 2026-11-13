//! The sans-IO sync session state machine.

use std::collections::{BTreeMap, VecDeque};

use oxkv_contracts::cursor::{CursorError, CursorToken, ScopeGeneration, SyncCursor};
use oxkv_contracts::version::{
    DescriptorDigest, Hash32, ManifestDigest, ManifestRevision, MapVersion, ScopeIdentity,
    ScopeVersionVector, SyncWireVersion, VersionVector,
};

use crate::codec::Encoder;
use crate::cursor::{CursorContext, CursorDecision, CursorPolicy, cursor_error_code};
use crate::diff::{DiffBatch, DiffEntry, key_in_scope};
use crate::error::ErrorCode;
use crate::frame::{
    Ack, CloseFrame, Credit, Frame, ProtocolErrorFrame, Resume, Resumed, SessionHello,
    SessionReady, SnapshotComplete, SnapshotOffer, SnapshotPage, SnapshotTail, Subscribe,
    Subscribed,
};
use crate::types::{Capabilities, Limits, SubscriptionId};

/// The session phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    /// The session needs a `SessionHello`.
    AwaitHello,
    /// The session negotiated one hello.
    Ready,
    /// The session is closed.
    Closed,
}

/// The accepted session parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Negotiated {
    /// The deterministic session identity.
    pub session_id: Hash32,
    /// The chosen sync wire version.
    pub sync_version: SyncWireVersion,
    /// The chosen manifest digest.
    pub manifest_digest: ManifestDigest,
    /// The chosen shard map version.
    pub map_version: MapVersion,
    /// The capability intersection.
    pub capabilities: Capabilities,
    /// The negotiated limits.
    pub limits: Limits,
}

/// The authority configuration for one session.
#[derive(Clone)]
pub struct SessionConfig {
    /// The RPC descriptor identity.
    pub descriptor_digest: DescriptorDigest,
    /// The authority manifest digest.
    pub manifest_digest: ManifestDigest,
    /// The authority manifest revision.
    pub manifest_revision: ManifestRevision,
    /// The lowest manifest revision that the authority accepts.
    pub minimum_manifest_revision: ManifestRevision,
    /// The authority shard map version.
    pub map_version: MapVersion,
    /// The lowest supported sync wire version.
    pub minimum_sync_version: SyncWireVersion,
    /// The highest supported sync wire version.
    pub maximum_sync_version: SyncWireVersion,
    /// The supported capabilities.
    pub capabilities: Capabilities,
    /// The authority limits.
    pub limits: Limits,
    /// The cursor policy and retention window.
    pub cursor: CursorPolicy,
}

/// A change batch that the authority data layer produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffsReady {
    /// The subscription identity.
    pub subscription_id: SubscriptionId,
    /// The entries in sync order.
    pub entries: Vec<DiffEntry>,
}

/// A snapshot that the authority data layer produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotReady {
    /// The subscription identity.
    pub subscription_id: SubscriptionId,
    /// The snapshot offer.
    pub offer: SnapshotOffer,
    /// The pages in sequence order.
    pub pages: Vec<SnapshotPage>,
    /// The first diff version after the snapshot.
    pub tail_start_version: ScopeVersionVector,
}

/// One input to the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// An incoming frame.
    Incoming(Frame),
    /// The transport closed.
    Disconnected,
    /// The authority data layer produced diffs.
    DiffsReady(DiffsReady),
    /// The authority data layer produced a snapshot.
    SnapshotReady(SnapshotReady),
}

/// One output action from the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Send one frame.
    Send(Frame),
    /// Send one close frame and close the transport.
    Close(CloseFrame),
}

#[derive(Debug, Clone)]
struct SubscriptionState {
    scope_identity: ScopeIdentity,
    scope_generation: ScopeGeneration,
    prefixes: Vec<Vec<u8>>,
    applied: ScopeVersionVector,
    next_sequence: u64,
    snapshot_required: bool,
    max_batch_entries: u32,
}

impl SubscriptionState {
    fn context(&self) -> CursorContext {
        CursorContext::new(self.scope_identity, self.scope_generation)
    }
}

/// The authority side sans-IO session.
pub struct Session {
    config: SessionConfig,
    phase: SessionPhase,
    negotiated: Option<Negotiated>,
    subscriptions: BTreeMap<SubscriptionId, SubscriptionState>,
    pending: VecDeque<Frame>,
    credit_bytes: u64,
    credit_messages: u32,
}

impl Session {
    /// Creates a session in the [`SessionPhase::AwaitHello`] phase.
    #[must_use]
    pub fn new(config: SessionConfig) -> Self {
        Self {
            config,
            phase: SessionPhase::AwaitHello,
            negotiated: None,
            subscriptions: BTreeMap::new(),
            pending: VecDeque::new(),
            credit_bytes: 0,
            credit_messages: 0,
        }
    }

    /// Returns the session phase.
    #[must_use]
    pub const fn phase(&self) -> SessionPhase {
        self.phase
    }

    /// Returns the negotiated parameters.
    #[must_use]
    pub const fn negotiated(&self) -> Option<&Negotiated> {
        self.negotiated.as_ref()
    }

    /// Returns the number of frames that wait for credit.
    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Returns true when the session holds a frame for lack of credit.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Processes one event and returns the output commands in order.
    pub fn on_event(&mut self, event: Event) -> Vec<Command> {
        let mut commands = Vec::new();
        match event {
            Event::Incoming(frame) => self.on_frame(frame, &mut commands),
            Event::Disconnected => {
                self.phase = SessionPhase::Closed;
                self.pending.clear();
            }
            Event::DiffsReady(diffs) => self.on_diffs_ready(&diffs, &mut commands),
            Event::SnapshotReady(snapshot) => self.on_snapshot_ready(&snapshot, &mut commands),
        }
        commands
    }

    fn on_frame(&mut self, frame: Frame, commands: &mut Vec<Command>) {
        if self.phase == SessionPhase::Closed {
            return;
        }
        match (&self.phase, frame) {
            (SessionPhase::AwaitHello, Frame::SessionHello(hello)) => {
                self.on_hello(&hello, commands);
            }
            (SessionPhase::AwaitHello, _) => {
                push_error(
                    self.config.cursor.snapshot_available,
                    ErrorCode::NegotiationRequired,
                    "Send SessionHello before a data call.",
                    commands,
                );
            }
            (SessionPhase::Ready, Frame::SessionHello(_)) => {
                push_error(
                    self.config.cursor.snapshot_available,
                    ErrorCode::NegotiationRequired,
                    "The session is already negotiated.",
                    commands,
                );
                commands.push(Command::Close(CloseFrame::new(
                    ErrorCode::NegotiationRequired,
                    "The session cannot renegotiate.",
                )));
                self.phase = SessionPhase::Closed;
            }
            (SessionPhase::Ready, frame) => self.on_ready_frame(frame, commands),
            (SessionPhase::Closed, _) => {}
        }
    }

    fn on_hello(&mut self, hello: &SessionHello, commands: &mut Vec<Command>) {
        if hello.sync_version < self.config.minimum_sync_version
            || hello.sync_version > self.config.maximum_sync_version
        {
            let error = ProtocolErrorFrame::new(ErrorCode::UnsupportedVersion)
                .with_detail("The sync wire version is unsupported.")
                .with_supported_range(
                    self.config.minimum_sync_version.get(),
                    self.config.maximum_sync_version.get(),
                );
            commands.push(Command::Send(Frame::Error(error)));
            commands.push(Command::Close(CloseFrame::new(
                ErrorCode::UnsupportedVersion,
                "The sync wire version is unsupported.",
            )));
            self.phase = SessionPhase::Closed;
            return;
        }
        if hello.descriptor_digest != self.config.descriptor_digest {
            self.reject_hello(
                ErrorCode::RpcDescriptorMismatch,
                "The RPC descriptor does not match.",
                commands,
            );
            return;
        }
        if hello.manifest_digest != self.config.manifest_digest
            || hello.manifest_revision < self.config.minimum_manifest_revision
            || hello.map_version != self.config.map_version
        {
            self.reject_hello(
                ErrorCode::IncompatibleManifest,
                "The client manifest is not compatible.",
                commands,
            );
            return;
        }
        let unknown = hello
            .required_capabilities
            .missing_from(&self.config.capabilities);
        if !unknown.is_empty() {
            self.reject_hello(
                ErrorCode::UnknownCapability,
                &format!(
                    "The client requires unknown capabilities: {}.",
                    unknown.join(", ")
                ),
                commands,
            );
            return;
        }
        let requested = hello
            .required_capabilities
            .union(&hello.optional_capabilities);
        let capabilities = requested.intersection(&self.config.capabilities);
        let limits = hello.limits.intersection(self.config.limits);
        let session_id = session_identity(hello);
        let negotiated = Negotiated {
            session_id,
            sync_version: hello.sync_version,
            manifest_digest: hello.manifest_digest,
            map_version: hello.map_version,
            capabilities: capabilities.clone(),
            limits,
        };
        let ready = SessionReady {
            session_id,
            descriptor_digest: self.config.descriptor_digest,
            sync_version: hello.sync_version,
            manifest_digest: self.config.manifest_digest,
            manifest_revision: self.config.manifest_revision,
            map_version: self.config.map_version,
            capabilities,
            limits,
        };
        self.negotiated = Some(negotiated);
        self.phase = SessionPhase::Ready;
        commands.push(Command::Send(Frame::SessionReady(ready)));
    }

    fn reject_hello(&mut self, code: ErrorCode, detail: &str, commands: &mut Vec<Command>) {
        commands.push(Command::Send(Frame::Error(
            ProtocolErrorFrame::new(code)
                .with_detail(detail)
                .with_snapshot_fallback(self.config.cursor.snapshot_available),
        )));
        commands.push(Command::Close(CloseFrame::new(code, detail)));
        self.phase = SessionPhase::Closed;
    }

    fn on_ready_frame(&mut self, frame: Frame, commands: &mut Vec<Command>) {
        match frame {
            Frame::Subscribe(subscribe) => self.on_subscribe(subscribe, commands),
            Frame::Resume(resume) => self.on_resume(&resume, commands),
            Frame::Ack(ack) => self.on_ack(&ack, commands),
            Frame::Credit(credit) => {
                self.add_credit(credit);
                self.flush(commands);
            }
            Frame::Keepalive(keepalive) => {
                commands.push(Command::Send(Frame::Keepalive(keepalive)));
            }
            Frame::ReceiptResume(_) => {}
            Frame::Close(close) => {
                self.phase = SessionPhase::Closed;
                self.pending.clear();
                commands.push(Command::Close(close));
            }
            _ => {
                push_error(
                    self.config.cursor.snapshot_available,
                    ErrorCode::RpcUnknownMethod,
                    "The frame is not valid in the ready phase.",
                    commands,
                );
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn on_subscribe(&mut self, subscribe: Subscribe, commands: &mut Vec<Command>) {
        if subscribe.prefixes.is_empty() {
            push_error(
                self.config.cursor.snapshot_available,
                ErrorCode::ScopeRejected,
                "The scope has no prefix. An unbounded scope is rejected.",
                commands,
            );
            return;
        }
        if u32::try_from(subscribe.prefixes.len()).unwrap_or(u32::MAX)
            > self.config.limits.max_scope_prefixes
        {
            push_error(
                self.config.cursor.snapshot_available,
                ErrorCode::ScopeRejected,
                "The scope has too many prefixes.",
                commands,
            );
            return;
        }
        if u32::try_from(subscribe.required_indexes.len()).unwrap_or(u32::MAX)
            > self.config.limits.max_scope_prefixes
        {
            push_error(
                self.config.cursor.snapshot_available,
                ErrorCode::ScopeRejected,
                "The scope requires too many indexes.",
                commands,
            );
            return;
        }
        if u32::try_from(subscribe.policy_payload.len()).unwrap_or(u32::MAX)
            > self.config.limits.max_message_bytes
        {
            push_error(
                self.config.cursor.snapshot_available,
                ErrorCode::ScopeRejected,
                "The policy payload is too large.",
                commands,
            );
            return;
        }
        if self.subscriptions.contains_key(&subscribe.subscription_id) {
            push_error(
                self.config.cursor.snapshot_available,
                ErrorCode::ScopeRejected,
                "The subscription identity is already active.",
                commands,
            );
            return;
        }
        let context = CursorContext::new(subscribe.scope_identity, subscribe.scope_generation);
        let starting_version = if subscribe.snapshot_requested || subscribe.cursor.is_none() {
            if !self.config.cursor.snapshot_available {
                push_error(
                    self.config.cursor.snapshot_available,
                    ErrorCode::SnapshotUnavailable,
                    "The authority has no snapshot for this scope.",
                    commands,
                );
                return;
            }
            ScopeVersionVector::new()
        } else if let Some(token) = subscribe.cursor.as_deref() {
            match self.config.cursor.evaluate(&context, token) {
                Ok(CursorDecision::Resume(cursor)) => cursor.version_vector,
                Ok(CursorDecision::Expired) => {
                    self.push_cursor_error(
                        ErrorCode::CursorExpired,
                        "The cursor is expired.",
                        commands,
                    );
                    return;
                }
                Ok(CursorDecision::GapTooLarge) => {
                    self.push_cursor_error(
                        ErrorCode::GapTooLarge,
                        "The resume gap is too large.",
                        commands,
                    );
                    return;
                }
                Err(error) => {
                    self.push_cursor_error(
                        cursor_error_code(error),
                        cursor_detail(error),
                        commands,
                    );
                    return;
                }
            }
        } else {
            ScopeVersionVector::new()
        };
        let snapshot_required = subscribe.snapshot_requested || subscribe.cursor.is_none();
        let max_batch_entries = if subscribe.max_batch_entries == 0 {
            self.config.limits.max_batch_entries
        } else {
            subscribe
                .max_batch_entries
                .min(self.config.limits.max_batch_entries)
        };
        let state = SubscriptionState {
            scope_identity: subscribe.scope_identity,
            scope_generation: subscribe.scope_generation,
            prefixes: subscribe.prefixes,
            applied: starting_version.clone(),
            next_sequence: 0,
            snapshot_required,
            max_batch_entries,
        };
        let cursor = issue_cursor(&self.config.cursor, &state).to_token();
        let subscribed = Subscribed {
            subscription_id: subscribe.subscription_id,
            scope_identity: subscribe.scope_identity,
            effective_policy_version: subscribe.policy_version,
            scope_generation: subscribe.scope_generation,
            starting_version,
            cursor,
            snapshot_required,
            oldest_readable: self.config.cursor.retention_floor.clone(),
        };
        self.subscriptions.insert(subscribe.subscription_id, state);
        commands.push(Command::Send(Frame::Subscribed(subscribed)));
    }

    fn on_resume(&mut self, resume: &Resume, commands: &mut Vec<Command>) {
        let Some(state) = self.subscriptions.get(&resume.subscription_id) else {
            push_error(
                self.config.cursor.snapshot_available,
                ErrorCode::ScopeRejected,
                "The subscription is unknown.",
                commands,
            );
            return;
        };
        let context = state.context();
        match self.config.cursor.evaluate(&context, &resume.cursor) {
            Ok(CursorDecision::Resume(cursor)) => {
                let merged = merge_max(&state.applied, &cursor.version_vector);
                let Some(state) = self.subscriptions.get_mut(&resume.subscription_id) else {
                    return;
                };
                state.applied = merged.clone();
                let cursor = issue_cursor(&self.config.cursor, state).to_token();
                let resumed = Resumed {
                    subscription_id: resume.subscription_id,
                    starting_version: merged,
                    cursor,
                    oldest_readable: self.config.cursor.retention_floor.clone(),
                };
                commands.push(Command::Send(Frame::Resumed(resumed)));
            }
            Ok(CursorDecision::Expired) => {
                self.push_cursor_error(
                    ErrorCode::CursorExpired,
                    "The cursor is expired.",
                    commands,
                );
            }
            Ok(CursorDecision::GapTooLarge) => {
                self.push_cursor_error(
                    ErrorCode::GapTooLarge,
                    "The resume gap is too large.",
                    commands,
                );
            }
            Err(error) => {
                self.push_cursor_error(cursor_error_code(error), cursor_detail(error), commands);
            }
        }
    }

    fn on_ack(&mut self, ack: &Ack, commands: &mut Vec<Command>) {
        let Some(state) = self.subscriptions.get_mut(&ack.subscription_id) else {
            push_error(
                self.config.cursor.snapshot_available,
                ErrorCode::ScopeRejected,
                "The subscription is unknown.",
                commands,
            );
            return;
        };
        if ack.sequence.get() >= state.next_sequence {
            push_error(
                self.config.cursor.snapshot_available,
                ErrorCode::GapTooLarge,
                "The acknowledgement names an unsent sequence.",
                commands,
            );
            return;
        }
        if !ack
            .applied_version
            .as_vector()
            .entails(state.applied.as_vector())
        {
            push_error(
                self.config.cursor.snapshot_available,
                ErrorCode::CursorInvalid,
                "The observed version decreased.",
                commands,
            );
            return;
        }
        state.applied = ack.applied_version.clone();
    }

    fn on_diffs_ready(&mut self, diffs: &DiffsReady, commands: &mut Vec<Command>) {
        let Some(state) = self.subscriptions.get_mut(&diffs.subscription_id) else {
            push_error(
                self.config.cursor.snapshot_available,
                ErrorCode::ScopeRejected,
                "The subscription is unknown.",
                commands,
            );
            return;
        };
        if u32::try_from(diffs.entries.len()).unwrap_or(u32::MAX) > state.max_batch_entries {
            push_error(
                self.config.cursor.snapshot_available,
                ErrorCode::RateLimited,
                "The diff batch exceeds the negotiated limit.",
                commands,
            );
            return;
        }
        let mut previous: Option<(&[u8], &[u8], u64)> = None;
        let mut expected_sequence = state.next_sequence;
        for entry in &diffs.entries {
            if entry.subscription_id != diffs.subscription_id
                || entry.scope_generation != state.scope_generation
            {
                push_error(
                    self.config.cursor.snapshot_available,
                    ErrorCode::ScopeRejected,
                    "The diff entry belongs to another subscription.",
                    commands,
                );
                return;
            }
            if !key_in_scope(&entry.key, &state.prefixes) {
                push_error(
                    self.config.cursor.snapshot_available,
                    ErrorCode::ScopeRejected,
                    "The diff entry is outside the scope.",
                    commands,
                );
                return;
            }
            if entry.sequence.get() != expected_sequence {
                push_error(
                    self.config.cursor.snapshot_available,
                    ErrorCode::GapTooLarge,
                    "The diff stream has a sequence gap.",
                    commands,
                );
                return;
            }
            if let Some((previous_shard, previous_key, previous_sequence)) = previous {
                if (entry.shard_id.as_bytes(), entry.key.as_slice())
                    < (previous_shard, previous_key)
                {
                    push_error(
                        self.config.cursor.snapshot_available,
                        ErrorCode::ScopeRejected,
                        "The diff batch is not in sync order.",
                        commands,
                    );
                    return;
                }
                if entry.sequence.get() != previous_sequence.saturating_add(1) {
                    push_error(
                        self.config.cursor.snapshot_available,
                        ErrorCode::GapTooLarge,
                        "The diff stream has a sequence gap.",
                        commands,
                    );
                    return;
                }
            }
            previous = Some((
                entry.shard_id.as_bytes(),
                entry.key.as_slice(),
                entry.sequence.get(),
            ));
            expected_sequence = expected_sequence.saturating_add(1);
        }
        state.next_sequence = expected_sequence;
        if !diffs.entries.is_empty() {
            let frame =
                Frame::DiffBatch(DiffBatch::new(diffs.subscription_id, diffs.entries.clone()));
            self.send_stream(frame, commands);
        }
    }

    fn on_snapshot_ready(&mut self, snapshot: &SnapshotReady, commands: &mut Vec<Command>) {
        let Some(state) = self.subscriptions.get_mut(&snapshot.subscription_id) else {
            push_error(
                self.config.cursor.snapshot_available,
                ErrorCode::ScopeRejected,
                "The subscription is unknown.",
                commands,
            );
            return;
        };
        if !state.snapshot_required {
            push_error(
                self.config.cursor.snapshot_available,
                ErrorCode::ScopeRejected,
                "The subscription did not request a snapshot.",
                commands,
            );
            return;
        }
        if snapshot.offer.subscription_id != snapshot.subscription_id
            || u32::try_from(snapshot.pages.len()).unwrap_or(u32::MAX) != snapshot.offer.page_count
            || snapshot.offer.page_count > self.config.limits.max_snapshot_pages
        {
            push_error(
                self.config.cursor.snapshot_available,
                ErrorCode::SnapshotUnavailable,
                "The snapshot offer and the page set disagree.",
                commands,
            );
            return;
        }
        let mut expected_sequence = 0_u64;
        for page in &snapshot.pages {
            if page.subscription_id != snapshot.subscription_id
                || page.page_sequence.get() != expected_sequence
                || !page.verify_integrity()
            {
                push_error(
                    self.config.cursor.snapshot_available,
                    ErrorCode::SnapshotUnavailable,
                    "The snapshot page is out of sequence or corrupt.",
                    commands,
                );
                return;
            }
            expected_sequence = expected_sequence.saturating_add(1);
        }
        state.snapshot_required = false;
        state.applied = snapshot.tail_start_version.clone();
        state.next_sequence = 0;
        let cursor = issue_cursor(&self.config.cursor, state).to_token();
        let tail = SnapshotTail {
            subscription_id: snapshot.subscription_id,
            snapshot_version: snapshot.offer.snapshot_version.clone(),
            cursor: cursor.clone(),
            start_version: snapshot.tail_start_version.clone(),
        };
        let complete = SnapshotComplete {
            subscription_id: snapshot.subscription_id,
            snapshot_version: snapshot.offer.snapshot_version.clone(),
            cursor,
        };
        self.send_stream(Frame::SnapshotOffer(snapshot.offer.clone()), commands);
        for page in &snapshot.pages {
            self.send_stream(Frame::SnapshotPage(page.clone()), commands);
        }
        self.send_stream(Frame::SnapshotTail(tail), commands);
        self.send_stream(Frame::SnapshotComplete(complete), commands);
    }

    fn push_cursor_error(&self, code: ErrorCode, detail: &str, commands: &mut Vec<Command>) {
        push_error(
            self.config.cursor.snapshot_available,
            code,
            detail,
            commands,
        );
    }

    fn add_credit(&mut self, credit: Credit) {
        self.credit_bytes = self.credit_bytes.saturating_add(credit.bytes);
        self.credit_messages = self.credit_messages.saturating_add(credit.messages);
    }

    fn send_stream(&mut self, frame: Frame, commands: &mut Vec<Command>) {
        self.pending.push_back(frame);
        self.flush(commands);
    }

    fn flush(&mut self, commands: &mut Vec<Command>) {
        while let Some(frame) = self.pending.front() {
            let bytes = u64::try_from(frame.encode().len()).unwrap_or(u64::MAX);
            if self.credit_messages == 0 || self.credit_bytes < bytes {
                return;
            }
            self.credit_messages -= 1;
            self.credit_bytes -= bytes;
            if let Some(frame) = self.pending.pop_front() {
                commands.push(Command::Send(frame));
            }
        }
    }
}

fn issue_cursor(policy: &CursorPolicy, state: &SubscriptionState) -> SyncCursor {
    policy.signer.issue(
        &state.context(),
        state.applied.clone(),
        policy.retention_floor.clone(),
    )
}

fn push_error(
    snapshot_available: bool,
    code: ErrorCode,
    detail: &str,
    commands: &mut Vec<Command>,
) {
    let error = ProtocolErrorFrame::new(code)
        .with_detail(detail)
        .with_snapshot_fallback(snapshot_available);
    commands.push(Command::Send(Frame::Error(error)));
}

fn session_identity(hello: &SessionHello) -> Hash32 {
    let mut encoder = Encoder::new();
    encoder.digest(hello.descriptor_digest.as_bytes());
    encoder.u16(hello.sync_version.get());
    encoder.digest(hello.manifest_digest.as_bytes());
    encoder.u64(hello.manifest_revision.get());
    encoder.u64(hello.map_version.get());
    hello.required_capabilities.encode(&mut encoder);
    hello.optional_capabilities.encode(&mut encoder);
    hello.limits.encode(&mut encoder);
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"oxkv/session-id/v1\0");
    hasher.update(&encoder.finish());
    Hash32::from_bytes(*hasher.finalize().as_bytes())
}

fn merge_max(left: &ScopeVersionVector, right: &ScopeVersionVector) -> ScopeVersionVector {
    let mut vector = VersionVector::new();
    for (shard_id, version) in left.as_vector().iter() {
        vector.set(shard_id.clone(), version);
    }
    for (shard_id, version) in right.as_vector().iter() {
        let next = vector
            .get(shard_id)
            .map_or(version, |current| current.max(version));
        vector.set(shard_id.clone(), next);
    }
    ScopeVersionVector::from_vector(vector)
}

fn cursor_detail(error: CursorError) -> &'static str {
    match error {
        CursorError::Invalid => "The cursor is invalid.",
        CursorError::UnsupportedVersion { .. } => "The cursor token version is unsupported.",
        CursorError::TypeMismatch { .. } => "The cursor type does not match the scope.",
        CursorError::Expired { .. } => "The cursor is expired.",
    }
}
