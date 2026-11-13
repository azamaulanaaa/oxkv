//! Quotas and token-bucket rate limits.
//!
//! Admission rejects before work starts. A quota reject is a `Rejected`
//! receipt. A rate limit reject is a named error with no receipt.
//! Time is an injected [`Timestamp`].

use std::collections::BTreeMap;

use oxkv_contracts::version::ShardId;

use crate::error::OpsError;
use crate::maintenance::Timestamp;

/// One quota resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum QuotaResource {
    /// Stored bytes.
    StoredBytes,
    /// Log bytes.
    LogBytes,
    /// In-flight requests.
    InFlightRequests,
    /// Query cost.
    QueryCost,
    /// Event backlog.
    EventBacklog,
    /// Replicas.
    Replicas,
}

impl QuotaResource {
    /// Every quota resource.
    pub const ALL: [Self; 6] = [
        Self::StoredBytes,
        Self::LogBytes,
        Self::InFlightRequests,
        Self::QueryCost,
        Self::EventBacklog,
        Self::Replicas,
    ];

    /// Returns the canonical resource name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::StoredBytes => "stored_bytes",
            Self::LogBytes => "log_bytes",
            Self::InFlightRequests => "in_flight_requests",
            Self::QueryCost => "query_cost",
            Self::EventBacklog => "event_backlog",
            Self::Replicas => "replicas",
        }
    }

    /// Returns true when the resource has a sync and event reserve.
    #[must_use]
    pub const fn is_reserved(self) -> bool {
        matches!(self, Self::StoredBytes | Self::LogBytes)
    }
}

impl core::fmt::Display for QuotaResource {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The quota limits of one shard.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QuotaLimits {
    /// Stored bytes.
    pub stored_bytes: u64,
    /// Log bytes.
    pub log_bytes: u64,
    /// In-flight requests.
    pub in_flight_requests: u64,
    /// Query cost.
    pub query_cost: u64,
    /// Event backlog.
    pub event_backlog: u64,
    /// Replicas.
    pub replicas: u64,
}

impl QuotaLimits {
    /// Creates the quota limits.
    #[must_use]
    pub const fn new(
        stored_bytes: u64,
        log_bytes: u64,
        in_flight_requests: u64,
        query_cost: u64,
        event_backlog: u64,
        replicas: u64,
    ) -> Self {
        Self {
            stored_bytes,
            log_bytes,
            in_flight_requests,
            query_cost,
            event_backlog,
            replicas,
        }
    }

    fn limit_for(&self, resource: QuotaResource) -> u64 {
        match resource {
            QuotaResource::StoredBytes => self.stored_bytes,
            QuotaResource::LogBytes => self.log_bytes,
            QuotaResource::InFlightRequests => self.in_flight_requests,
            QuotaResource::QueryCost => self.query_cost,
            QuotaResource::EventBacklog => self.event_backlog,
            QuotaResource::Replicas => self.replicas,
        }
    }
}

/// The current usage of one shard.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QuotaUsage {
    /// Stored bytes.
    pub stored_bytes: u64,
    /// Log bytes.
    pub log_bytes: u64,
    /// In-flight requests.
    pub in_flight_requests: u64,
    /// Query cost.
    pub query_cost: u64,
    /// Event backlog.
    pub event_backlog: u64,
    /// Replicas.
    pub replicas: u64,
}

impl QuotaUsage {
    /// Creates the usage values.
    #[must_use]
    pub const fn new(
        stored_bytes: u64,
        log_bytes: u64,
        in_flight_requests: u64,
        query_cost: u64,
        event_backlog: u64,
        replicas: u64,
    ) -> Self {
        Self {
            stored_bytes,
            log_bytes,
            in_flight_requests,
            query_cost,
            event_backlog,
            replicas,
        }
    }

    fn used_for(&self, resource: QuotaResource) -> u64 {
        match resource {
            QuotaResource::StoredBytes => self.stored_bytes,
            QuotaResource::LogBytes => self.log_bytes,
            QuotaResource::InFlightRequests => self.in_flight_requests,
            QuotaResource::QueryCost => self.query_cost,
            QuotaResource::EventBacklog => self.event_backlog,
            QuotaResource::Replicas => self.replicas,
        }
    }
}

/// The resource amount that one request asks for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QuotaRequest {
    /// Stored bytes.
    pub stored_bytes: u64,
    /// Log bytes.
    pub log_bytes: u64,
    /// In-flight requests.
    pub in_flight_requests: u64,
    /// Query cost.
    pub query_cost: u64,
    /// Event backlog.
    pub event_backlog: u64,
    /// Replicas.
    pub replicas: u64,
}

impl QuotaRequest {
    /// Creates a request amount.
    #[must_use]
    pub const fn new(
        stored_bytes: u64,
        log_bytes: u64,
        in_flight_requests: u64,
        query_cost: u64,
        event_backlog: u64,
        replicas: u64,
    ) -> Self {
        Self {
            stored_bytes,
            log_bytes,
            in_flight_requests,
            query_cost,
            event_backlog,
            replicas,
        }
    }

    /// Creates a write request that adds stored bytes.
    #[must_use]
    pub const fn write(stored_bytes: u64) -> Self {
        Self {
            stored_bytes,
            in_flight_requests: 1,
            ..Self::zero()
        }
    }

    /// Creates a read request with a query cost.
    #[must_use]
    pub const fn read(query_cost: u64) -> Self {
        Self {
            query_cost,
            ..Self::zero()
        }
    }

    /// Creates a zero request.
    #[must_use]
    pub const fn zero() -> Self {
        Self {
            stored_bytes: 0,
            log_bytes: 0,
            in_flight_requests: 0,
            query_cost: 0,
            event_backlog: 0,
            replicas: 0,
        }
    }

    fn amount_for(&self, resource: QuotaResource) -> u64 {
        match resource {
            QuotaResource::StoredBytes => self.stored_bytes,
            QuotaResource::LogBytes => self.log_bytes,
            QuotaResource::InFlightRequests => self.in_flight_requests,
            QuotaResource::QueryCost => self.query_cost,
            QuotaResource::EventBacklog => self.event_backlog,
            QuotaResource::Replicas => self.replicas,
        }
    }
}

/// The bytes that sync and events reserve.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QuotaReserve {
    /// Stored bytes reserved for sync.
    pub sync_bytes: u64,
    /// Log bytes reserved for events.
    pub event_bytes: u64,
}

impl QuotaReserve {
    /// Creates a reserve.
    #[must_use]
    pub const fn new(sync_bytes: u64, event_bytes: u64) -> Self {
        Self {
            sync_bytes,
            event_bytes,
        }
    }

    fn amount_for(&self, resource: QuotaResource) -> u64 {
        match resource {
            QuotaResource::StoredBytes => self.sync_bytes,
            QuotaResource::LogBytes => self.event_bytes,
            QuotaResource::InFlightRequests
            | QuotaResource::QueryCost
            | QuotaResource::EventBacklog
            | QuotaResource::Replicas => 0,
        }
    }
}

/// The quota policy of one shard.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QuotaPolicy {
    /// The limits.
    pub limits: QuotaLimits,
    /// The sync and event reserve.
    pub reserve: QuotaReserve,
}

impl QuotaPolicy {
    /// Creates a quota policy.
    #[must_use]
    pub const fn new(limits: QuotaLimits, reserve: QuotaReserve) -> Self {
        Self { limits, reserve }
    }

    /// Checks one request at admission.
    ///
    /// # Errors
    ///
    /// Returns [`OpsError::QuotaExceeded`] for the first exceeded resource.
    pub fn check(&self, usage: &QuotaUsage, request: &QuotaRequest) -> Result<(), OpsError> {
        for resource in QuotaResource::ALL {
            let limit = self
                .limits
                .limit_for(resource)
                .saturating_sub(self.reserve.amount_for(resource));
            let used = usage
                .used_for(resource)
                .saturating_add(request.amount_for(resource));
            if used > limit {
                return Err(OpsError::QuotaExceeded {
                    resource,
                    limit,
                    used,
                });
            }
        }
        Ok(())
    }
}

/// One operation class for a rate limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OperationClass {
    /// A read.
    Read,
    /// A write.
    Write,
    /// A sync diff.
    Sync,
    /// An event delivery.
    Event,
    /// A control action.
    Control,
}

impl OperationClass {
    /// Returns the canonical class name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Sync => "sync",
            Self::Event => "event",
            Self::Control => "control",
        }
    }
}

impl core::fmt::Display for OperationClass {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// One rate limit subject.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RateLimitSubject {
    /// One shard.
    Shard(ShardId),
    /// One client principal.
    Client(String),
}

impl core::fmt::Display for RateLimitSubject {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Shard(shard_id) => write!(formatter, "shard={shard_id:?}"),
            Self::Client(client) => write!(formatter, "client={client}"),
        }
    }
}

/// One token bucket identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RateLimitKey {
    /// The subject.
    pub subject: RateLimitSubject,
    /// The operation class.
    pub class: OperationClass,
}

impl RateLimitKey {
    /// Creates a shard rate limit key.
    #[must_use]
    pub fn shard(shard_id: ShardId, class: OperationClass) -> Self {
        Self {
            subject: RateLimitSubject::Shard(shard_id),
            class,
        }
    }

    /// Creates a client rate limit key.
    #[must_use]
    pub fn client(client: impl Into<String>, class: OperationClass) -> Self {
        Self {
            subject: RateLimitSubject::Client(client.into()),
            class,
        }
    }
}

impl core::fmt::Display for RateLimitKey {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{} class={}", self.subject, self.class)
    }
}

/// One token bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenBucket {
    capacity: u64,
    refill_per_second: u64,
    tokens: u64,
    updated_at: Timestamp,
}

impl TokenBucket {
    /// Creates a full token bucket.
    #[must_use]
    pub const fn new(capacity: u64, refill_per_second: u64, now: Timestamp) -> Self {
        Self {
            capacity,
            refill_per_second,
            tokens: capacity,
            updated_at: now,
        }
    }

    /// Returns the current token count without a refill.
    #[must_use]
    pub const fn tokens(&self) -> u64 {
        self.tokens
    }

    /// Refills the bucket to the current time.
    pub fn refill(&mut self, now: Timestamp) {
        if now <= self.updated_at || self.refill_per_second == 0 {
            return;
        }
        if self.tokens >= self.capacity {
            self.updated_at = now;
            return;
        }
        let elapsed = now.millis().saturating_sub(self.updated_at.millis());
        let added = elapsed
            .saturating_mul(self.refill_per_second)
            .saturating_div(1_000);
        if added == 0 {
            return;
        }
        self.tokens = self.tokens.saturating_add(added).min(self.capacity);
        let consumed_millis = added
            .saturating_mul(1_000)
            .saturating_div(self.refill_per_second);
        self.updated_at = self.updated_at.plus(consumed_millis);
    }

    /// Tries to take one cost from the bucket.
    pub fn try_acquire(&mut self, now: Timestamp, cost: u64) -> bool {
        self.refill(now);
        if self.tokens < cost {
            return false;
        }
        self.tokens = self.tokens.saturating_sub(cost);
        true
    }
}

/// The token buckets of one node.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RateLimiter {
    buckets: BTreeMap<RateLimitKey, TokenBucket>,
}

impl RateLimiter {
    /// Creates an empty rate limiter.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one bucket.
    pub fn configure(&mut self, key: RateLimitKey, bucket: TokenBucket) {
        self.buckets.insert(key, bucket);
    }

    /// Returns one bucket.
    #[must_use]
    pub fn bucket(&self, key: &RateLimitKey) -> Option<&TokenBucket> {
        self.buckets.get(key)
    }

    /// Checks one request at admission.
    ///
    /// # Errors
    ///
    /// Returns [`OpsError::RateLimited`] when the bucket is empty.
    pub fn admit(&mut self, key: &RateLimitKey, now: Timestamp, cost: u64) -> Result<(), OpsError> {
        let Some(bucket) = self.buckets.get_mut(key) else {
            return Ok(());
        };
        if bucket.try_acquire(now, cost) {
            return Ok(());
        }
        Err(OpsError::RateLimited { key: key.clone() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> QuotaLimits {
        QuotaLimits::new(100, 100, 4, 50, 20, 2)
    }

    #[test]
    fn a_quota_breach_rejects_at_admission() {
        let policy = QuotaPolicy::new(limits(), QuotaReserve::default());
        let error = policy
            .check(
                &QuotaUsage::new(95, 0, 0, 0, 0, 0),
                &QuotaRequest::write(10),
            )
            .expect_err("the quota is exceeded");
        assert_eq!(error.code(), "ops.quota_exceeded");
        assert_eq!(
            error.receipt_state(),
            Some(oxkv_contracts::receipt::ReceiptState::Rejected)
        );
    }

    #[test]
    fn a_reserve_protects_sync_bytes() {
        let policy = QuotaPolicy::new(limits(), QuotaReserve::new(20, 10));
        assert!(
            policy
                .check(&QuotaUsage::new(75, 0, 0, 0, 0, 0), &QuotaRequest::write(5))
                .is_ok()
        );
        let error = policy
            .check(&QuotaUsage::new(80, 0, 0, 0, 0, 0), &QuotaRequest::write(1))
            .expect_err("the reserve is protected");
        assert_eq!(
            error,
            OpsError::QuotaExceeded {
                resource: QuotaResource::StoredBytes,
                limit: 80,
                used: 81
            }
        );
    }

    #[test]
    fn a_rate_limit_applies_per_shard_client_and_class() {
        let mut limiter = RateLimiter::new();
        let shard = RateLimitKey::shard(ShardId::new(vec![1]), OperationClass::Write);
        let client = RateLimitKey::client("alice", OperationClass::Write);
        let read = RateLimitKey::shard(ShardId::new(vec![1]), OperationClass::Read);
        limiter.configure(shard.clone(), TokenBucket::new(1, 1, Timestamp::new(0)));
        limiter.configure(client.clone(), TokenBucket::new(1, 0, Timestamp::new(0)));
        limiter.configure(read.clone(), TokenBucket::new(1, 0, Timestamp::new(0)));
        assert!(limiter.admit(&shard, Timestamp::new(0), 1).is_ok());
        let error = limiter
            .admit(&shard, Timestamp::new(0), 1)
            .expect_err("the shard bucket is empty");
        assert_eq!(error.code(), "ops.rate_limited");
        assert!(limiter.admit(&client, Timestamp::new(0), 1).is_ok());
        assert!(limiter.admit(&client, Timestamp::new(0), 1).is_err());
        assert!(limiter.admit(&read, Timestamp::new(0), 1).is_ok());
    }

    #[test]
    fn a_bucket_refills_from_injected_time() {
        let mut bucket = TokenBucket::new(2, 1, Timestamp::new(0));
        assert!(bucket.try_acquire(Timestamp::new(0), 2));
        assert!(!bucket.try_acquire(Timestamp::new(500), 1));
        assert!(bucket.try_acquire(Timestamp::new(1_000), 1));
        assert_eq!(bucket.tokens(), 0);
        assert!(bucket.try_acquire(Timestamp::new(3_000), 2));
    }

    #[test]
    fn an_unconfigured_bucket_admits() {
        let mut limiter = RateLimiter::new();
        let key = RateLimitKey::client("bob", OperationClass::Sync);
        assert!(limiter.admit(&key, Timestamp::new(0), 100).is_ok());
    }
}
