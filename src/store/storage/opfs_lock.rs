//! Cross-tab mutual exclusion for [`OpfsStorage`](super::OpfsStorage).
//!
//! # Why a lock at all
//!
//! OPFS exposes no compare-and-swap: a conditional write has to be a
//! read-then-write, and another tab of the same origin can write between the
//! two halves. [`OpfsStorage::put_opts`] therefore wraps its
//! precondition-check *and* its write in a named
//! [Web Lock](https://developer.mozilla.org/en-US/docs/Web/API/Web_Locks_API)
//! so that tabs of this origin serialise per object. `delete` takes the same
//! lock; `get` does not need one because OPFS replaces a file atomically when
//! a writable stream closes (a reader sees the old bytes or the new ones, never
//! a mix).
//!
//! # What Web Locks do and do not buy
//!
//! - Advisory and per-origin: a tab that does not take the lock (another
//!   library, a hand-written `navigator.storage` call, an older oxkv build) can
//!   still interleave with a locked section. The lock only orders *cooperating*
//!   callers.
//! - It does not fence. The LSM layer's single-writer fencing is still what
//!   stops a stale writer from resurrecting an old manifest; the lock only
//!   closes the read-then-write window inside one object.
//! - It is scoped to the origin (and, for workers, the agent cluster): a lock
//!   is released if its holder is terminated mid-request, which wedges
//!   waiters until then.
//!
//! # Degraded mode
//!
//! `navigator.locks` is absent on older Safari, outside a browser, and in some
//! test runners. There we fall back to the historical read-then-write, which is
//! **best-effort, not atomic** — [`unlocked_operation_count`] counts every
//! operation that ran that way, and [`locks_available`] reports whether the
//! lock is available at all.

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};

use js_sys::{Function, Promise};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast as _, JsValue};

use super::{ObjectPath, Result, js_await, js_err};

/// Prefix of every lock name this crate takes.
///
/// Web Locks live in one flat, origin-wide namespace shared with every other
/// library on the page, so a reserved prefix keeps us from colliding with a
/// lock some other code happens to name after an object path.
const LOCK_NAMESPACE: &str = "oxkv/opfs/";

/// Number of guarded operations that ran without a lock (degraded mode).
///
/// Read-mostly observability for the fallback; see the module docs.
static UNLOCKED_OPERATIONS: AtomicU64 = AtomicU64::new(0);

/// Hand-declared bindings for the two Web Locks calls we make.
///
/// `web-sys` 0.3.103 *has* `Navigator::locks()` and
/// `LockManager::request_with_callback()`, but it gates the whole extern block
/// behind `--cfg=web_sys_unstable_apis`, so using them would force every
/// consumer of this crate to build with that cfg (and would leave the locked
/// path dead in any build that does not). The Web Locks API itself is stable
/// and shipped in Chrome 69+, Firefox 96+ and Safari 15.4+, so we declare the
/// two members we need. Delete this block and switch back to `web-sys` the day
/// it flips the flag — see https://rustwasm.github.io/wasm-bindgen/web-sys/unstable-apis.html
#[wasm_bindgen::prelude::wasm_bindgen]
extern "C" {
    /// `window.navigator`, redeclared locally: a `#[wasm_bindgen(method,
    /// getter)]` expands to an inherent impl on its `this` type, which the
    /// orphan rule forbids on `web_sys::Navigator`. The value is just a JS
    /// object, so any navigator is cast into this one.
    #[wasm_bindgen(
        extends = js_sys::Object,
        js_name = Navigator,
        typescript_type = "Navigator"
    )]
    pub type Navigator;

    /// `navigator.locks` getter. `undefined` where Web Locks is unsupported.
    #[wasm_bindgen(method, getter, js_class = "Navigator", js_name = "locks")]
    fn locks(this: &Navigator) -> LockManager;

    /// `navigator.locks` — the per-origin lock manager.
    #[wasm_bindgen(extends = js_sys::Object, js_name = LockManager, typescript_type = "LockManager")]
    pub type LockManager;

    /// `lockManager.request(name, callback)`: acquires the named lock, runs
    /// `callback` with it, and holds it until the promise `callback` returns
    /// settles (resolve *or* reject).
    #[wasm_bindgen(method, js_class = "LockManager", js_name = "request")]
    fn request_with_callback(
        this: &LockManager,
        name: &str,
        callback: &Function,
    ) -> js_sys::Promise;
}

/// Web Lock name for the OPFS object at `path`.
///
/// Scheme: [`LOCK_NAMESPACE`] + the canonical [`ObjectPath`] string, e.g.
/// `oxkv/opfs/e000007/wal/00000042.log`.
///
/// - **Stable** — it depends only on the path string, so every tab, reload and
///   store prefix derives the same name without coordination.
/// - **Per object and collision-free** — the mapping path → name is injective
///   (a constant prefix in front of the raw path), so two distinct objects,
///   two prefixes, or a path that is a string-prefix of another
///   (`a/b` vs `a/b/c`) never share a lock.
/// - **Namespaced** — `oxkv/opfs/` reserves the name against other libraries on
///   the same origin, whose locks live in the same flat namespace.
fn lock_name(path: &ObjectPath) -> String {
    format!("{LOCK_NAMESPACE}{}", path.as_str())
}

/// The origin's [`LockManager`], or `None` when Web Locks is unavailable.
///
/// Reads `navigator` off the global scope rather than through
/// `web_sys::window()`: Web Locks are specified on `Navigator`, which exists on
/// both window and worker global scopes, and `window()` would hide the API in
/// any scope that has no `Window` binding.
fn lock_manager() -> Option<LockManager> {
    let navigator: JsValue =
        js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("navigator")).ok()?;
    let manager = Navigator::from(navigator).locks();
    // Unsupported engines leave `navigator.locks` undefined; an `instanceof`
    // cast on that would throw, so screen the value before handing it back.
    manager.is_object().then_some(manager)
}

/// `true` when conditional writes are serialised by a Web Lock.
///
/// `false` means [`Storage::put_opts`](super::Storage::put_opts) is running its
/// preconditions read-then-write unguarded — best-effort, not atomic.
pub(super) fn locks_available() -> bool {
    lock_manager().is_some()
}

/// How many guarded operations have run without a lock since page load.
///
/// Non-zero in any environment where [`locks_available`] is `false`.
pub(super) fn unlocked_operation_count() -> u64 {
    UNLOCKED_OPERATIONS.load(Ordering::Relaxed)
}

/// Settles a Web Lock request's callback promise when dropped.
///
/// A [`LockManager`] holds the lock until the promise its callback returned
/// settles, so a guarded section that unwinds or is cancelled must still
/// settle it — a promise left pending forever wedges every other caller on
/// that name.
struct SettleOnDrop(Option<Function>);

impl Drop for SettleOnDrop {
    fn drop(&mut self) {
        if let Some(resolve) = self.0.take() {
            let _ = resolve.call0(&JsValue::UNDEFINED);
        }
    }
}

/// Runs `guarded` while holding the Web Lock named after `path`.
///
/// The lock is held from the moment the browser grants it until the promise
/// returned by the request callback settles, so `guarded` must *complete*
/// before that promise settles: resolving early would drop the lock while the
/// write is still in flight, and never settling would wedge every other tab
/// waiting on the same name. `with_object_lock` therefore keeps the callback's
/// promise pending for exactly the lifetime of `guarded`, and carries the
/// [`Result`] back through a shared slot rather than through a JS rejection —
/// a rejection could only carry a stringified error, and losing the variant
/// would turn a [`CasConflict`](crate::store::StoreError::CasConflict) into an
/// opaque backend error, breaking `probe_store`.
///
/// `guarded` runs with the lock already held: it must not take the same lock
/// name again (Web Locks are not reentrant, so that deadlocks).
///
/// Falls back to running `guarded` unguarded when Web Locks is unavailable —
/// see the module docs for what that does and does not guarantee.
pub(super) async fn with_object_lock<T, F, Fut>(path: &ObjectPath, guarded: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Fut + 'static,
    Fut: Future<Output = Result<T>> + 'static,
{
    let Some(manager) = lock_manager() else {
        UNLOCKED_OPERATIONS.fetch_add(1, Ordering::Relaxed);
        return guarded().await;
    };

    // A oneshot, not a shared slot: the value crosses the JS boundary exactly
    // once, so `Arc<Mutex<Option<..>>>` plus three `.expect("outcome slot")`
    // panics bought nothing. Carrying `Result<T>` in Rust memory rather than
    // rejecting the JS promise is the load-bearing part, and that is preserved:
    // a rejection could only carry a stringified error, and losing the variant
    // would turn `CasConflict` into an opaque backend error, breaking
    // `probe_store`.
    let (tx, rx) = futures::channel::oneshot::channel::<Result<T>>();
    // `Promise::new`'s executor is `FnMut`, so the sender is moved out on the
    // single invocation rather than captured by reference.
    let mut tx = Some(tx);
    let mut guarded = Some(guarded);

    // The callback is invoked exactly once per `request`, and must return a
    // promise that settles only after `guarded` has finished: that promise is
    // what the browser holds the lock for.
    let callback = Closure::once_into_js(move || -> Promise {
        Promise::new(&mut |resolve, _reject| {
            let guarded = guarded
                .take()
                .expect("Web Locks invokes a request callback at most once");
            let resolve = resolve.clone();
            let tx = tx
                .take()
                .expect("Promise executor runs exactly once per request");
            wasm_bindgen_futures::spawn_local(async move {
                let _settle = SettleOnDrop(Some(resolve));
                let result = guarded().await;
                // Send BEFORE `_settle` drops: the browser releases the lock
                // when the promise settles, so the receiver must already hold
                // the value by then. Dropping a closed sender is fine — the
                // caller having gone away is not an error here.
                let _ = tx.send(result);
            });
        })
    });

    let request = manager.request_with_callback(&lock_name(path), callback.unchecked_ref());
    let requested = js_await(request).await;
    let outcome = rx.await.unwrap_or_else(|_| match requested {
        Ok(_) => Err(js_err(
            "OPFS Web Lock callback did not run",
            JsValue::from_str("request resolved without a result"),
        )),
        Err(e) => Err(js_err("OPFS Web Lock request failed", e)),
    });
    drop(callback);
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::StoreError;
    use std::sync::{Arc, Mutex};
    use wasm_bindgen_test::wasm_bindgen_test;

    /// These tests drive the Web Locks bridge in [`with_object_lock`] against
    /// whatever the host actually implements: browsers and Node 24+ have
    /// `navigator.locks`, Node 22 (the version CI's `wasm` job installs) does
    /// not — and that host is exactly where the fallback has to hold up. Each
    /// test therefore runs where its subject exists, and says so loudly when it
    /// does not, rather than passing vacuously.
    fn announce_skip_if(test: &str, needed: bool) -> bool {
        if !needed {
            crate::wasm::announce_skip(
                test,
                "this host has no navigator.locks: the bridge runs on browsers and Node 24+, the fallback runs here (e.g. CI's Node 22)",
            );
        }
        !needed
    }

    #[wasm_bindgen_test]
    fn lock_name_is_stable_and_namespaced() {
        let path = ObjectPath::new("e000007/wal/00000042.log");
        // Stable: same path text in, same lock name out, every time.
        assert_eq!(
            lock_name(&path),
            lock_name(&ObjectPath::from("e000007/wal/00000042.log"))
        );
        // Namespaced so we cannot collide with another library's locks on the
        // same origin.
        assert_eq!(lock_name(&path), "oxkv/opfs/e000007/wal/00000042.log");
    }

    #[wasm_bindgen_test]
    fn lock_name_is_distinct_per_object_and_prefix() {
        let base = ObjectPath::new("e000007/wal/00000042.log");
        // Distinct objects, including the string-prefix traps that a
        // truncating or joining scheme would collapse.
        for other in [
            "e000007/wal/00000043.log",
            "e000007/wal/42.log",
            "e000007/wal/00000042.log.tmp",
            "e000007/wal/00000042.log/child",
            "e000007",
            "e000008/wal/00000042.log",
        ] {
            assert_ne!(
                lock_name(&base),
                lock_name(&ObjectPath::new(other)),
                "{other} must not share a lock with the base object"
            );
        }
        // Store prefixes are part of the object path, hence part of the name.
        assert_ne!(
            lock_name(&ObjectPath::new("store-a/probe/canary")),
            lock_name(&ObjectPath::new("store-b/probe/canary"))
        );
    }

    #[wasm_bindgen_test]
    fn lock_detection_reports_exactly_what_the_host_offers() {
        let host_has_locks =
            js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("navigator"))
                .ok()
                .and_then(|nav| js_sys::Reflect::get(&nav, &JsValue::from_str("locks")).ok())
                .is_some_and(|locks| locks.is_object());
        assert_eq!(
            locks_available(),
            host_has_locks,
            "detection must never claim a lock the host cannot grant"
        );
    }

    #[wasm_bindgen_test]
    async fn lock_bridge_runs_the_guarded_section_and_returns_its_value() {
        if announce_skip_if(
            "lock_bridge_runs_the_guarded_section_and_returns_its_value",
            locks_available(),
        ) {
            return;
        }
        let path = ObjectPath::new("bridge/happy");
        let ran = with_object_lock(&path, || async { Ok::<_, StoreError>(7_u32) }).await;
        assert_eq!(ran.expect("the guarded section ran"), 7);
    }

    #[wasm_bindgen_test]
    async fn lock_bridge_preserves_the_error_variant_and_releases_the_lock() {
        if announce_skip_if(
            "lock_bridge_preserves_the_error_variant_and_releases_the_lock",
            locks_available(),
        ) {
            return;
        }
        let path = ObjectPath::new("bridge/error");
        // A conflict raised inside the locked section must reach the caller as
        // `CasConflict`, not as an opaque backend error: `probe_store` matches
        // on the variant, and a JS rejection could not carry it.
        let conflicted = with_object_lock(&path, || async {
            Err::<u32, _>(StoreError::CasConflict("stale".to_string()))
        })
        .await;
        // The same lock name as the failed call: it has to have been released,
        // or this would never return.
        let after = with_object_lock(&path, || async { Ok::<_, StoreError>(1_u32) }).await;
        assert_eq!(
            conflicted.expect_err("the guarded section's error propagates"),
            StoreError::CasConflict("stale".to_string())
        );
        assert_eq!(after.expect("lock free again after an error"), 1);
    }

    /// Runs one guarded section that records when it starts and when it ends.
    async fn contend(tag: &'static str, trace: Arc<Mutex<Vec<String>>>) -> Result<&'static str> {
        let path = ObjectPath::new("bridge/shared");
        with_object_lock(&path, move || async move {
            trace.lock().expect("trace").push(format!("{tag}-start"));
            let _section = Section {
                tag,
                trace: Arc::clone(&trace),
            };
            // MUST yield. The two `contend` futures are only genuinely concurrent
            // if the guarded section reaches an await point: with a purely
            // synchronous body neither caller can interleave inside it, so this
            // test passed even with `with_object_lock` replaced by a bare
            // `guarded().await`. The sleep is what makes "no overlap" mean
            // "mutual exclusion held" instead of "nothing could ever overlap".
            crate::store::sleep(std::time::Duration::from_millis(1)).await;
            Ok(tag)
        })
        .await
    }

    /// Drops at the end of a guarded section, error path included.
    struct Section {
        tag: &'static str,
        trace: Arc<Mutex<Vec<String>>>,
    }

    impl Drop for Section {
        fn drop(&mut self) {
            self.trace
                .lock()
                .expect("trace")
                .push(format!("{}-end", self.tag));
        }
    }

    #[wasm_bindgen_test]
    async fn lock_bridge_serialises_callers_that_contend_for_one_name() {
        if announce_skip_if(
            "lock_bridge_serialises_callers_that_contend_for_one_name",
            locks_available(),
        ) {
            return;
        }
        let trace: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let (first, second) = futures::future::join(
            contend("a", Arc::clone(&trace)),
            contend("b", Arc::clone(&trace)),
        )
        .await;
        let entries = trace.lock().expect("trace").clone();
        first.expect("first section ran");
        second.expect("second section ran");
        assert_eq!(entries.len(), 4, "both sections ran exactly once");
        // No overlap: a `start` may only follow the other caller's `end` —
        // exactly what holding the lock for the whole section buys.
        let mut inside = false;
        for entry in &entries {
            if entry.ends_with("-start") {
                assert!(!inside, "guarded sections overlapped: {entries:?}");
                inside = true;
            } else {
                inside = false;
            }
        }
    }

    #[wasm_bindgen_test]
    async fn missing_lock_manager_degrades_and_counts() {
        if announce_skip_if(
            "missing_lock_manager_degrades_and_counts",
            !locks_available(),
        ) {
            return;
        }
        let before = unlocked_operation_count();
        let ran = with_object_lock(&ObjectPath::new("bridge/degraded"), || async {
            Ok::<_, StoreError>(3_u32)
        })
        .await;
        let after = unlocked_operation_count();
        assert_eq!(ran.expect("the guarded section still runs"), 3);
        assert_eq!(
            after,
            before + 1,
            "the degraded path must be observable, not silent"
        );
    }
}
