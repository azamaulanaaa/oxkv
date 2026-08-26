//! OpenTelemetry instrumentation for stores.
//!
//! This module provides [`OtelStore`], a decorator that wraps any backend
//! implementing [`Store`](super::Store) and emits OpenTelemetry traces and
//! metrics around every operation. It follows the same decorator idiom as
//! [`HookStore`](super::HookStore), so it composes transparently with the rest
//! of the crate — including transactions, the extension traits
//! ([`GetSetExt`](super::GetSetExt), [`StoreExt`](super::StoreExt)) and other
//! decorators:
//!
//! ```text
//! OtelStore::new(HookStore::new(RedbStore::new()?))
//! ```
//!
//! # Plug-in architecture
//!
//! The crate depends only on the [OpenTelemetry API
//! crate](https://docs.rs/opentelemetry), not on any SDK or exporter. Telemetry
//! is emitted through the global tracer and meter providers, so applications
//! choose (or omit) an SDK, protocol exporter and sampling strategy without any
//! involvement from this crate:
//!
//! ```no_run
//! use oxkv::{BTreeStore, OtelStore};
//!
//! // Application-side setup: install SDK providers before the first store
//! // operation (SDK crates are application dependencies).
//! // opentelemetry_sdk::trace::SdkTracerProvider ... global::set_tracer_provider(...)
//!
//! let mut store = OtelStore::new(BTreeStore::default());
//! ```
//!
//! When no provider is installed (the default), all telemetry calls resolve to
//! no-op implementations and the decorator is a thin pass-through.
//!
//! # What is recorded
//!
//! Every operation produces one *span* named after it (`get`, `has`, `set`,
//! `delete`, `gets`, `begin_tx`, `commit`, `rollback`):
//!
//! - `db.system = "oxkv"` and `db.operation.name` identify the store and
//!   operation.
//! - `oxkv.key` carries the key of single-key operations.
//! - `oxkv.existed` marks single-key writes/deletes that hit an existing key.
//! - `oxkv.items` carries how many entries a range read returned.
//! - Failures record an `exception` event and set the span status to `Error`.
//! - A transaction's `commit`/`rollback` spans are children of its `begin_tx`
//!   span; other spans are rooted at the caller's current span, so store
//!   activity nests inside application traces naturally.
//!
//! Metrics are reported under the meter `"oxkv"`:
//!
//! - `oxkv.store.operations`: monotonic counter with `db.operation.name` and
//!   `oxkv.outcome` (`ok`/`error`) attributes.
//! - `oxkv.store.operation.duration`: seconds histogram per operation.
//!
//! Note that `oxkv.key` is intentionally high-cardinality; drop it with a
//! processor-side attribute filter if you export to a system that charges per
//! time series.
//!
//! # Example
//!
//! ```rust
//! #![cfg(feature = "otel")]
//! use oxkv::{BTreeStore, GetSet, GetSetExt, OtelStore};
//!
//! #[tokio::main(flavor = "current_thread")]
//! async fn main() {
//!     // No SDK installed here: spans and metrics are silently discarded,
//!     // while the store behaves exactly like the wrapped BTreeStore.
//!     let mut store = OtelStore::new(BTreeStore::default());
//!
//!     store.set("greeting", &serde_json::json!({ "hello": "world" }))
//!         .await
//!         .unwrap();
//!     assert!(store.has("greeting").await.unwrap());
//! }
//! ```

use std::future::Future;
use std::sync::LazyLock;
use std::time::Instant;

use async_trait::async_trait;
use opentelemetry::global::{BoxedSpan, BoxedTracer};
use opentelemetry::metrics::{Counter, Histogram};
use opentelemetry::trace::{Span as _, SpanBuilder, SpanContext, Status, TraceContextExt, Tracer};
use opentelemetry::{Context, KeyValue as OtelKv, global};

use super::{Direction, GetSet, KeyValue, Result, Store, Transaction};

/// The value recorded for the `db.system` span attribute.
const DB_SYSTEM: &str = "oxkv";

/// Span/metric attribute carrying the key of single-key operations.
const ATTR_KEY: &str = "oxkv.key";

/// Metric attribute distinguishing successful from failed operations.
const ATTR_OUTCOME: &str = "oxkv.outcome";

/// Value of [`ATTR_OUTCOME`] for successful operations.
const OUTCOME_OK: &str = "ok";

/// Value of [`ATTR_OUTCOME`] for failed operations.
const OUTCOME_ERROR: &str = "error";

/// Span/metric attribute carrying whether the key existed before the write.
const ATTR_EXISTED: &str = "oxkv.existed";

/// Span attribute carrying the number of entries a range read returned.
const ATTR_ITEMS: &str = "oxkv.items";

/// Operation names, used verbatim as span names and metric attributes.
mod ops {
    pub(super) const GET: &str = "get";
    pub(super) const HAS: &str = "has";
    pub(super) const SET: &str = "set";
    pub(super) const DELETE: &str = "delete";
    pub(super) const GETS: &str = "gets";
    pub(super) const BEGIN_TX: &str = "begin_tx";
    pub(super) const COMMIT: &str = "commit";
    pub(super) const ROLLBACK: &str = "rollback";
}

/// Global tracer resolved once against whichever tracer provider is installed
/// at first use. Providers installed afterwards are not picked up; install
/// them before issuing store operations.
static TRACER: LazyLock<BoxedTracer> = LazyLock::new(|| global::tracer(env!("CARGO_PKG_NAME")));

/// Global metric instruments, resolved together with the meter provider at
/// first use (same caveat as [`TRACER`]).
static TELEMETRY: LazyLock<Telemetry> = LazyLock::new(Telemetry::new);

struct Telemetry {
    operations: Counter<u64>,
    duration: Histogram<f64>,
}

impl Telemetry {
    fn new() -> Self {
        let meter = global::meter(env!("CARGO_PKG_NAME"));
        Self {
            operations: meter
                .u64_counter("oxkv.store.operations")
                .with_description("Number of key-value store operations.")
                .build(),
            duration: meter
                .f64_histogram("oxkv.store.operation.duration")
                .with_unit("s")
                .with_description("Wall-clock duration of key-value store operations.")
                .build(),
        }
    }
}

/** A non-recording span that only carries a parent [`SpanContext`].
 *
 * Used to parent `commit`/`rollback` spans at their transaction's `begin_tx`
 * span after that span has ended. Equivalent to the SDK test utilities'
 * `TestSpan`, implemented locally to avoid a testing-only dependency.
 */
struct ParentSpan(SpanContext);

impl opentelemetry::trace::Span for ParentSpan {
    fn add_event_with_timestamp<T>(
        &mut self,
        _name: T,
        _timestamp: std::time::SystemTime,
        _attributes: Vec<OtelKv>,
    ) where
        T: Into<std::borrow::Cow<'static, str>>,
    {
    }

    fn span_context(&self) -> &SpanContext {
        &self.0
    }

    fn is_recording(&self) -> bool {
        false
    }

    fn set_attribute(&mut self, _attribute: OtelKv) {}

    fn set_status(&mut self, _status: Status) {}

    fn update_name<T>(&mut self, _new_name: T)
    where
        T: Into<std::borrow::Cow<'static, str>>,
    {
    }

    fn add_link(&mut self, _span_context: SpanContext, _attributes: Vec<OtelKv>) {}

    fn end_with_timestamp(&mut self, _timestamp: std::time::SystemTime) {}
}

/// Starts a span for `operation`, parented at the caller's current span or at
/// `parent` when given (used so commit/rollback nest under their transaction).
fn start_span(operation: &'static str, parent: Option<&SpanContext>) -> BoxedSpan {
    let builder = SpanBuilder::from_name(operation);
    let mut span = match parent {
        None => TRACER.build_with_context(builder, &Context::current()),
        Some(parent) => {
            let cx = Context::current_with_span(ParentSpan(parent.clone()));
            TRACER.build_with_context(builder, &cx)
        }
    };
    span.set_attributes([
        OtelKv::new("db.system", DB_SYSTEM),
        OtelKv::new("db.operation.name", operation),
    ]);
    span
}

/// Records completion of one operation in both metrics and (on failure) the
/// span's status and exception event.
fn finish<T>(span: &mut BoxedSpan, outcome: &Result<T>, operation: &'static str, started: Instant) {
    let elapsed = started.elapsed().as_secs_f64();
    let outcome_value = match outcome {
        Ok(_) => OUTCOME_OK,
        Err(err) => {
            span.record_error(err);
            span.set_status(Status::error(err.to_string()));
            OUTCOME_ERROR
        }
    };

    TELEMETRY.operations.add(
        1,
        &[
            OtelKv::new("db.operation.name", operation),
            OtelKv::new(ATTR_OUTCOME, outcome_value),
        ],
    );
    TELEMETRY
        .duration
        .record(elapsed, &[OtelKv::new("db.operation.name", operation)]);
}

/// Runs `fut` inside a span named `operation`.
///
/// The span is created before polling starts and ends when the wrapper returns
/// (via drop), so it covers the entire async wait. `annotate` may attach
/// result-derived attributes while the span is still recording.
async fn instrumented<F, T>(
    operation: &'static str,
    key: Option<&str>,
    parent: Option<&SpanContext>,
    fut: F,
    annotate: impl FnOnce(&mut BoxedSpan, &T),
) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    let mut span = start_span(operation, parent);
    if let Some(key) = key {
        span.set_attribute(OtelKv::new(ATTR_KEY, key.to_string()));
    }

    let started = Instant::now();
    let outcome = fut.await;

    if let Ok(value) = &outcome {
        annotate(&mut span, value);
    }
    finish(&mut span, &outcome, operation, started);

    outcome
}

/// Pass-through annotation used where no result-derived attributes apply.
fn no_annotation<T>(_: &mut BoxedSpan, _: &T) {}

/// A [`Store`] decorator emitting OpenTelemetry traces and metrics.
///
/// Wrapping any backend, it intercepts every operation to produce one span per
/// call plus counter/histogram records (see the crate-level `otel` feature
/// docs). Because
/// it implements [`Store`] itself, it composes transparently with the rest of
/// the crate.
///
/// # Example
///
/// ```rust
/// use oxkv::{BTreeStore, GetSet, OtelStore};
///
/// # #[tokio::main(flavor = "current_thread")] async fn main() {
/// let mut store = OtelStore::new(BTreeStore::default());
/// assert_eq!(store.get_bytes("k").await.unwrap(), None);
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct OtelStore<S> {
    inner: S,
}

impl<S> OtelStore<S> {
    /// Wraps `inner` so every operation emits OpenTelemetry signals.
    ///
    /// Telemetry flows through the global OpenTelemetry providers; see the
    /// crate-level feature documentation for wiring guidance.
    #[must_use]
    pub fn new(inner: S) -> Self {
        Self { inner }
    }

    /// Returns the wrapped store, consuming this decorator.
    #[must_use]
    pub fn into_inner(self) -> S {
        self.inner
    }

    /// Borrows the wrapped store.
    #[must_use]
    pub fn inner(&self) -> &S {
        &self.inner
    }
}

#[async_trait]
impl<S: GetSet + Send + Sync> GetSet for OtelStore<S> {
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        instrumented(
            ops::GET,
            Some(key),
            None,
            self.inner.get_bytes(key),
            no_annotation,
        )
        .await
    }

    async fn has(&self, key: &str) -> Result<bool> {
        instrumented(
            ops::HAS,
            Some(key),
            None,
            self.inner.has(key),
            |span, existed| {
                span.set_attribute(OtelKv::new(ATTR_EXISTED, *existed));
            },
        )
        .await
    }

    async fn delete(&mut self, key: &str) -> Result<bool> {
        instrumented(
            ops::DELETE,
            Some(key),
            None,
            self.inner.delete(key),
            |span, deleted| {
                span.set_attribute(OtelKv::new(ATTR_EXISTED, *deleted));
            },
        )
        .await
    }

    async fn set_bytes(&mut self, key: &str, value: &[u8]) -> Result<Option<Vec<u8>>> {
        instrumented(
            ops::SET,
            Some(key),
            None,
            self.inner.set_bytes(key, value),
            |span, prev| {
                // `true` marks an update of an existing key, `false` an insert.
                span.set_attribute(OtelKv::new(ATTR_EXISTED, prev.is_some()));
            },
        )
        .await
    }

    async fn gets_bytes(
        &self,
        limit: Option<u32>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>> {
        instrumented(
            ops::GETS,
            None,
            None,
            self.inner.gets_bytes(limit, direction, cursor),
            |span, entries| {
                let count = i64::try_from(entries.len()).unwrap_or(i64::MAX);
                span.set_attribute(OtelKv::new(ATTR_ITEMS, count));
            },
        )
        .await
    }
}

#[async_trait]
impl<S: Store + Send + Sync> Store for OtelStore<S>
where
    S::Transaction: Send + Sync,
{
    type Transaction = OtelTx<S::Transaction>;

    fn begin_tx(&mut self) -> Result<Self::Transaction> {
        let mut span = start_span(ops::BEGIN_TX, None);
        let started = Instant::now();
        let outcome = self.inner.begin_tx();

        match &outcome {
            Ok(_) => {}
            Err(err) => {
                span.record_error(err);
                span.set_status(Status::error(err.to_string()));
            }
        }
        finish(&mut span, &outcome, ops::BEGIN_TX, started);

        outcome.map(|tx| OtelTx {
            inner: tx,
            tx_context: Some(span.span_context().clone()),
        })
    }
}

/// An [`OtelStore`] transaction that instruments staging operations and makes
/// `commit`/`rollback` child spans of their originating `begin_tx`.
///
/// Produced by [`Store::begin_tx`] on an [`OtelStore`]; otherwise behaves
/// exactly like the wrapped transaction.
#[derive(Debug)]
pub struct OtelTx<T> {
    inner: T,
    /// Trace context of the `begin_tx` span, used to parent terminal spans.
    tx_context: Option<SpanContext>,
}

#[async_trait]
impl<T: GetSet + Send + Sync> GetSet for OtelTx<T> {
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        instrumented(
            ops::GET,
            Some(key),
            None,
            self.inner.get_bytes(key),
            no_annotation,
        )
        .await
    }

    async fn has(&self, key: &str) -> Result<bool> {
        instrumented(
            ops::HAS,
            Some(key),
            None,
            self.inner.has(key),
            |span, existed| {
                span.set_attribute(OtelKv::new(ATTR_EXISTED, *existed));
            },
        )
        .await
    }

    async fn delete(&mut self, key: &str) -> Result<bool> {
        instrumented(
            ops::DELETE,
            Some(key),
            None,
            self.inner.delete(key),
            |span, deleted| {
                span.set_attribute(OtelKv::new(ATTR_EXISTED, *deleted));
            },
        )
        .await
    }

    async fn set_bytes(&mut self, key: &str, value: &[u8]) -> Result<Option<Vec<u8>>> {
        instrumented(
            ops::SET,
            Some(key),
            None,
            self.inner.set_bytes(key, value),
            |span, prev| {
                span.set_attribute(OtelKv::new(ATTR_EXISTED, prev.is_some()));
            },
        )
        .await
    }

    async fn gets_bytes(
        &self,
        limit: Option<u32>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>> {
        instrumented(
            ops::GETS,
            None,
            None,
            self.inner.gets_bytes(limit, direction, cursor),
            |span, entries| {
                let count = i64::try_from(entries.len()).unwrap_or(i64::MAX);
                span.set_attribute(OtelKv::new(ATTR_ITEMS, count));
            },
        )
        .await
    }
}

#[async_trait]
impl<T: Transaction + Send + Sync> Transaction for OtelTx<T> {
    async fn commit(mut self) -> Result<()> {
        let parent = self.tx_context.take().map(std::borrow::Cow::Owned);
        instrumented(
            ops::COMMIT,
            None,
            parent.as_deref(),
            self.inner.commit(),
            no_annotation,
        )
        .await
    }

    async fn rollback(mut self) -> Result<()> {
        let parent = self.tx_context.take().map(std::borrow::Cow::Owned);
        instrumented(
            ops::ROLLBACK,
            None,
            parent.as_deref(),
            self.inner.rollback(),
            no_annotation,
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// Tests
//
// Delegation tests run against whatever global providers happen to be
// installed (none, in CI): they verify the decorator preserves semantics.
// Emission tests install a real SDK provider exactly once via `fixture()` and
// filter exported data by per-test unique keys, so parallel tests cannot
// interfere with each other's assertions.
//
// `await_holding_lock` is intentional: `telemetry_lock` serializes each test's
// whole body because telemetry flows through process-global providers. Each
// test runs on its own single-threaded tokio runtime, so no other task can be
// blocked behind the guard while it is held across an await point.
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::await_holding_lock)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};

    use opentelemetry::trace::Status;
    use opentelemetry_sdk::error::OTelSdkResult;
    use opentelemetry_sdk::metrics::data::MetricData;
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};
    use opentelemetry_sdk::trace::{SdkTracerProvider, SpanData, SpanExporter};

    use super::*;
    use crate::store::{BTreeStore, StoreError};

    /// In-memory span sink implementing the SDK exporter trait; spans land in
    /// it synchronously because we register it behind a simple processor.
    #[derive(Clone, Debug, Default)]
    struct SharedSpanExporter(Arc<Mutex<Vec<SpanData>>>);

    impl SpanExporter for SharedSpanExporter {
        async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
            self.0.lock().unwrap().extend(batch);
            Ok(())
        }
    }

    struct Fixture {
        spans: SharedSpanExporter,
        metrics: InMemoryMetricExporter,
        meter_provider: SdkMeterProvider,
    }

    /// Installs the global SDK providers exactly once and returns shared sinks.
    ///
    /// Every test in this module must call this *before* its first store
    /// operation, so the lazy globals bind to the real providers regardless of
    /// which test runs first.
    fn fixture() -> &'static Fixture {
        static FIXTURE: OnceLock<Fixture> = OnceLock::new();
        FIXTURE.get_or_init(|| {
            let spans = SharedSpanExporter::default();
            let tracer_provider = SdkTracerProvider::builder()
                .with_simple_exporter(spans.clone())
                .build();
            global::set_tracer_provider(tracer_provider);

            let metrics = InMemoryMetricExporter::default();
            let reader = PeriodicReader::builder(metrics.clone()).build();
            let meter_provider = SdkMeterProvider::builder().with_reader(reader).build();
            global::set_meter_provider(meter_provider.clone());

            Fixture {
                spans,
                metrics,
                meter_provider,
            }
        })
    }

    /// All exported spans whose `oxkv.key` attribute equals `key`.
    fn spans_for_key(key: &str) -> Vec<SpanData> {
        fixture()
            .spans
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|s| {
                s.attributes
                    .iter()
                    .any(|kv| kv.key.as_str() == ATTR_KEY && kv.value.as_str() == key)
            })
            .cloned()
            .collect()
    }

    /// Current number of exported spans, for before/after snapshots.
    fn exported_len() -> usize {
        fixture().spans.0.lock().unwrap().len()
    }

    /// Spans exported at index `start` or later.
    fn exported_since(start: usize) -> Vec<SpanData> {
        fixture().spans.0.lock().unwrap()[start..].to_vec()
    }

    /// Serializes every test in this module.
    ///
    /// Telemetry flows through process-global providers shared by all tests,
    /// so concurrent store operations would interleave into each other's
    /// exporter snapshots. Holding this lock for the entire test body makes
    /// before/after snapshots exact. The suite is fast enough that losing
    /// intra-module parallelism is irrelevant.
    fn telemetry_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn attr<'a>(attrs: impl IntoIterator<Item = &'a OtelKv>, key: &str) -> Option<String> {
        attrs
            .into_iter()
            .find(|kv| kv.key.as_str() == key)
            .map(|kv| kv.value.as_str().to_string())
    }

    fn name_of(span: &SpanData) -> String {
        span.name.to_string()
    }

    // -- delegation semantics ------------------------------------------------

    type StoreUnderTest = OtelStore<BTreeStore>;

    fn store() -> StoreUnderTest {
        OtelStore::new(BTreeStore::default())
    }

    #[tokio::test]
    async fn test_crud_round_trip_matches_inner_semantics() {
        let _guard = telemetry_lock();
        fixture();
        let mut s = store();

        // Insert returns None, update returns previous.
        assert_eq!(s.set_bytes("k", b"v1").await.unwrap(), None);
        assert_eq!(s.set_bytes("k", b"v2").await.unwrap(), Some(b"v1".to_vec()));

        assert_eq!(s.get_bytes("k").await.unwrap(), Some(b"v2".to_vec()));
        assert!(s.has("k").await.unwrap());

        assert!(s.delete("k").await.unwrap());
        assert!(!s.has("k").await.unwrap());
        assert!(!s.delete("k").await.unwrap());
    }

    #[tokio::test]
    async fn test_gets_bytes_paginates_like_inner_store() {
        let _guard = telemetry_lock();
        fixture();
        let mut s = store();
        for i in 0..5 {
            s.set_bytes(&format!("key{i}"), b"v").await.unwrap();
        }

        let page = s
            .gets_bytes(Some(2), Direction::Next, (None, None))
            .await
            .unwrap();
        let keys: Vec<&str> = page.iter().map(|kv| kv.key.as_str()).collect();
        assert_eq!(keys, ["key0", "key1"]);

        let rev = s
            .gets_bytes(None, Direction::Prev, (Some("key4".into()), None))
            .await
            .unwrap();
        let keys: Vec<&str> = rev.iter().map(|kv| kv.key.as_str()).collect();
        assert_eq!(keys, ["key4", "key3", "key2", "key1", "key0"]);
    }

    #[tokio::test]
    async fn test_transaction_commit_makes_writes_visible() {
        let _guard = telemetry_lock();
        fixture();
        let mut s = store();

        let mut tx = s.begin_tx().unwrap();
        tx.set_bytes("committed", b"yes").await.unwrap();
        // Not visible until commit.
        assert_eq!(s.get_bytes("committed").await.unwrap(), None);
        tx.commit().await.unwrap();

        assert_eq!(
            s.get_bytes("committed").await.unwrap(),
            Some(b"yes".to_vec())
        );
    }

    #[tokio::test]
    async fn test_transaction_rollback_discards_writes() {
        let _guard = telemetry_lock();
        fixture();
        let mut s = store();

        let mut tx = s.begin_tx().unwrap();
        tx.set_bytes("rolled-back", b"nope").await.unwrap();
        tx.rollback().await.unwrap();

        assert_eq!(s.get_bytes("rolled-back").await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_into_inner_exposes_wrapped_store() {
        let _guard = telemetry_lock();
        fixture();
        let s = store();
        drop(s.into_inner()); // consumes the decorator without touching otel
    }

    // -- telemetry emission --------------------------------------------------

    /// A `GetSet` implementation whose writes always fail, to exercise the
    /// error-recording path deterministically.
    struct FailingWrites {
        reads: Mutex<HashMap<String, Vec<u8>>>,
    }

    impl FailingWrites {
        fn new() -> Self {
            Self {
                reads: Mutex::new(HashMap::new()),
            }
        }
    }

    #[async_trait]
    impl GetSet for FailingWrites {
        async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
            Ok(self.reads.lock().unwrap().get(key).cloned())
        }

        async fn has(&self, key: &str) -> Result<bool> {
            Ok(self.reads.lock().unwrap().contains_key(key))
        }

        async fn delete(&mut self, _key: &str) -> Result<bool> {
            Err(StoreError::Other("delete refused".into()))
        }

        async fn set_bytes(&mut self, _key: &str, _value: &[u8]) -> Result<Option<Vec<u8>>> {
            Err(StoreError::Other("write refused".into()))
        }

        async fn gets_bytes(
            &self,
            _limit: Option<u32>,
            _direction: Direction,
            _cursor: (Option<String>, Option<String>),
        ) -> Result<Vec<KeyValue>> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn test_successful_operations_emit_spans_and_metrics() {
        const KEY: &str = "otel-emission-success";
        let _guard = telemetry_lock();
        fixture();
        let mut s = store();

        s.set_bytes(KEY, b"v").await.unwrap();
        s.get_bytes(KEY).await.unwrap();
        s.has(KEY).await.unwrap();

        let keyed_names: Vec<String> = spans_for_key(KEY).iter().map(name_of).collect();
        for expected in ["set", "get", "has"] {
            assert!(
                keyed_names.iter().any(|name| name == expected),
                "expected `{expected}` span among {keyed_names:?}"
            );
        }

        // Transaction spans carry no key, so isolate them with a snapshot:
        // everything appended after `before` must contain our begin_tx/commit
        // pair, linked by parent span id.
        let before = exported_len();
        let mut tx = s.begin_tx().unwrap();
        tx.set_bytes(KEY, b"v2").await.unwrap();
        tx.commit().await.unwrap();
        let tx_spans: Vec<String> = exported_since(before).iter().map(name_of).collect();
        for expected in ["set", "begin_tx", "commit"] {
            assert!(
                tx_spans.iter().any(|name| name == expected),
                "expected `{expected}` span among {tx_spans:?}"
            );
        }

        let begin = exported_since(before)
            .iter()
            .find(|s| name_of(s) == "begin_tx")
            .expect("begin_tx span present")
            .span_context
            .span_id();
        let commit_linked = exported_since(before)
            .iter()
            .any(|s| name_of(s) == "commit" && s.parent_span_id == begin);
        assert!(commit_linked, "commit span must be parented at begin_tx");
    }

    #[tokio::test]
    async fn test_failed_operations_record_error_status_and_outcome() {
        const KEY: &str = "otel-emission-failure";
        let _guard = telemetry_lock();
        fixture();
        let mut s = OtelStore::new(FailingWrites::new());

        assert!(s.set_bytes(KEY, b"v").await.is_err());

        let spans = spans_for_key(KEY);
        assert_eq!(spans.len(), 1);
        let span = &spans[0];
        assert_eq!(name_of(span), "set");
        assert!(
            matches!(&span.status, Status::Error { .. }),
            "expected Error status, got {:?}",
            span.status
        );
        // Exception event was attached alongside the status.
        assert!(span.events.iter().any(|e| e.name == "exception"));
    }

    #[tokio::test]
    async fn test_operations_emit_counter_and_histogram_series() {
        const KEY: &str = "otel-metrics-check";
        let _guard = telemetry_lock();
        fixture();
        let mut s = store();

        s.set_bytes(KEY, b"v").await.unwrap();
        s.get_bytes(KEY).await.unwrap();
        assert!(s.delete(KEY).await.unwrap(), "delete of existing key");

        fixture().meter_provider.force_flush().unwrap();
        let finished = fixture().metrics.get_finished_metrics().unwrap();
        assert!(
            !finished.is_empty(),
            "in-memory exporter should hold flushed metrics"
        );

        let mut saw_counter_ok = false;
        let mut saw_histogram = false;
        for rm in finished {
            for sm in rm.scope_metrics() {
                for metric in sm.metrics() {
                    match metric.data() {
                        opentelemetry_sdk::metrics::data::AggregatedMetrics::U64(
                            MetricData::Sum(sum),
                        ) => {
                            assert_eq!(metric.name(), "oxkv.store.operations");
                            for point in sum.data_points() {
                                let op = attr(point.attributes(), "db.operation.name");
                                if point.value() > 0 && op.as_deref() == Some("get") {
                                    saw_counter_ok = true;
                                }
                            }
                        }
                        opentelemetry_sdk::metrics::data::AggregatedMetrics::F64(
                            MetricData::Histogram(hist),
                        ) => {
                            let total_count: u64 = hist
                                .data_points()
                                .map(opentelemetry_sdk::metrics::data::HistogramDataPoint::count)
                                .sum();
                            if metric.name() == "oxkv.store.operation.duration" && total_count > 0 {
                                saw_histogram = true;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        assert!(saw_counter_ok, "counter series with operation=get missing");
        assert!(saw_histogram, "duration histogram series missing");
    }

    #[tokio::test]
    async fn test_gets_records_item_count_on_span() {
        let _guard = telemetry_lock();
        fixture();
        // `gets` spans carry no key, so isolate via a start-of-test snapshot:
        // under the telemetry lock, everything appended below is ours.
        let before = exported_len();
        let mut s = store();
        for i in 0..3 {
            s.set_bytes(&format!("count{i}"), b"v").await.unwrap();
        }
        let found = s
            .gets_bytes(None, Direction::Next, (None, None))
            .await
            .unwrap();
        assert_eq!(found.len(), 3);

        let counts: Vec<Option<String>> = exported_since(before)
            .iter()
            .filter(|s| name_of(s) == "gets")
            .map(|s| attr(&s.attributes, ATTR_ITEMS))
            .collect();
        assert_eq!(counts.len(), 1, "expected one gets span, saw {counts:?}");
        assert_eq!(
            counts[0].as_deref(),
            Some("3"),
            "gets span should carry the returned item count"
        );
    }
}
