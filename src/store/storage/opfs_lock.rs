//! Cross-tab mutual exclusion for [`OpfsStorage`](super::OpfsStorage).
//!
//! # Why a lock at all
//!
//! OPFS exposes no compare-and-swap. A conditional write has to be a
//! read-then-write. Another tab of the same origin can write between the two
//! halves. [`OpfsStorage::put_opts`] therefore wraps its precondition check
//! and its write in a named
//! [Web Lock](https://developer.mozilla.org/en-US/docs/Web/API/Web_Locks_API).
//! Tabs of this origin then serialise per object. `delete` takes the same
//! lock. `get` does not need one. OPFS replaces a file atomically when a
//! writable stream closes. A reader then sees the old bytes or the new bytes.
//! A reader never sees a mix of both.
//!
//! # What Web Locks do and do not buy
//!
//! - The lock is advisory and per-origin. A tab that does not take the lock
//!   can still interleave with a locked section. Such a tab can be another
//!   library, a hand-written `navigator.storage` call, or an older oxkv
//!   build. The lock orders cooperating callers only.
//! - The lock does not fence. The single-writer fencing of the LSM layer still
//!   stops a stale writer from resurrecting an old manifest. The lock closes
//!   the read-then-write window inside one object only.
//! - The lock is scoped to the origin. For workers, the scope also covers the
//!   agent cluster. The browser releases a lock if its holder terminates during
//!   a request. That release wedges the waiters until then.
//!
//! # Degraded mode
//!
//! `navigator.locks` is absent on older Safari. It is also absent outside a
//! browser and in some test runners. In those environments the code falls back
//! to the historical read-then-write. That fallback is **best-effort, not
//! atomic**. [`unlocked_operation_count`] counts every operation that ran that
//! way. [`locks_available`] reports whether the lock is available at all.

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};

use js_sys::{Function, Promise};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast as _, JsValue};

use super::{ObjectPath, Result, js_await, js_err};

/// Prefix of every lock name this crate takes.
///
/// Web Locks live in one flat namespace that spans the origin. Every other
/// library on the page shares that namespace. A reserved prefix therefore
/// keeps this crate from colliding with a lock that some other code happens to
/// name after an object path.
const LOCK_NAMESPACE: &str = "oxkv/opfs/";

/// Number of guarded operations that ran without a lock (degraded mode).
///
/// This counter supports read-mostly observability for the fallback. See the
/// module documentation.
static UNLOCKED_OPERATIONS: AtomicU64 = AtomicU64::new(0);

/// Hand-declared bindings for the two Web Locks calls we make.
///
/// `web-sys` 0.3.103 has `Navigator::locks()` and
/// `LockManager::request_with_callback()`. The crate gates the whole extern
/// block behind `--cfg=web_sys_unstable_apis`. Using them would force every
/// consumer of this crate to build with that cfg. A build without that cfg
/// would also leave the locked path dead. The Web Locks API itself is stable.
/// The API ships in Chrome 69+, Firefox 96+ and Safari 15.4+. The code
/// therefore declares the two members that it needs. Delete this block and
/// switch back to `web-sys` on the day that the crate flips the flag.
/// See https://rustwasm.github.io/wasm-bindgen/web-sys/unstable-apis.html
#[wasm_bindgen::prelude::wasm_bindgen]
extern "C" {
    /// `window.navigator`, redeclared locally. A `#[wasm_bindgen(method,
    /// getter)]` expands to an inherent impl on its `this` type. The orphan
    /// rule forbids that impl on `web_sys::Navigator`. The value is only a JS
    /// object, so the code casts any navigator into this one.
    #[wasm_bindgen(
        extends = js_sys::Object,
        js_name = Navigator,
        typescript_type = "Navigator"
    )]
    pub type Navigator;

    /// `navigator.locks` getter. `undefined` where Web Locks is unsupported.
    #[wasm_bindgen(method, getter, js_class = "Navigator", js_name = "locks")]
    fn locks(this: &Navigator) -> LockManager;

    /// `navigator.locks`, the per-origin lock manager.
    #[wasm_bindgen(extends = js_sys::Object, js_name = LockManager, typescript_type = "LockManager")]
    pub type LockManager;

    /// `lockManager.request(name, callback)` acquires the named lock. The call
    /// runs `callback` with the lock. The call holds the lock until the
    /// promise that `callback` returns settles. The promise may settle through
    /// resolve *or* reject.
    #[wasm_bindgen(method, js_class = "LockManager", js_name = "request")]
    fn request_with_callback(
        this: &LockManager,
        name: &str,
        callback: &Function,
    ) -> js_sys::Promise;
}

/// Web Lock name for the OPFS object at `path`.
///
/// Scheme: [`LOCK_NAMESPACE`] followed by the canonical [`ObjectPath`]
/// string. For example, `oxkv/opfs/e000007/wal/00000042.log`.
///
/// - **Stable**. The name depends only on the path string. Therefore every
///   tab, every reload and every store prefix derives the same name without
///   coordination.
/// - **Per object and collision-free**. The mapping from path to name is
///   injective, because a constant prefix stands in front of the raw path. Two
///   distinct objects therefore never share a lock. Two prefixes also never
///   share a lock. A path that is a string-prefix of another path also never
///   shares a lock, for example `a/b` and `a/b/c`.
/// - **Namespaced**. `oxkv/opfs/` reserves the name against other libraries on
///   the same origin. The locks of those libraries live in the same flat
///   namespace.
fn lock_name(path: &ObjectPath) -> String {
    format!("{LOCK_NAMESPACE}{}", path.as_str())
}

/// The origin's [`LockManager`], or `None` when Web Locks is unavailable.
///
/// The function reads `navigator` off the global scope. It does not use
/// `web_sys::window()`. Web Locks are specified on `Navigator`. `Navigator`
/// exists on both the window global scope and the worker global scope. A call
/// to `window()` would hide the API in any scope that has no `Window` binding.
fn lock_manager() -> Option<LockManager> {
    let navigator: JsValue =
        js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("navigator")).ok()?;
    let manager = Navigator::from(navigator).locks();
    // Unsupported engines leave `navigator.locks` undefined. An
    // `instanceof` cast on that value would throw. The function screens the
    // value before returning the lock manager.
    manager.is_object().then_some(manager)
}

/// `true` when conditional writes are serialised by a Web Lock.
///
/// `false` means [`Storage::put_opts`](super::Storage::put_opts) runs its
/// preconditions read-then-write unguarded. The write is best-effort, not
/// atomic.
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
/// A [`LockManager`] holds the lock until the promise that its callback
/// returned settles. Therefore a guarded section that unwinds or is cancelled
/// must still settle that promise. A promise that stays pending forever wedges
/// every other caller on that name.
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
/// The browser grants the lock at one moment. The browser releases the lock
/// when the promise that the request callback returns settles. Therefore
/// `guarded` must *complete* before that promise settles. An early resolve
/// drops the lock while the write is still in flight. A promise that never
/// settles wedges every other tab that waits on the same name.
/// `with_object_lock` therefore keeps
/// the promise of the callback pending for exactly the lifetime of `guarded`.
/// The function carries the [`Result`] back through a shared slot. The
/// function does not carry the [`Result`] through a JS rejection. A rejection
/// can only carry a stringified error. Losing the variant would turn a
/// [`CasConflict`](crate::store::StoreError::CasConflict) into an opaque
/// backend error. That would break `probe_store`.
///
/// `guarded` runs with the lock already held. It must not take the same lock
/// name again. Web Locks are not reentrant, so that would deadlock.
///
/// The function falls back to running `guarded` unguarded when Web Locks is
/// unavailable. See the module documentation for what that does and does not
/// guarantee.
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

    // The code uses a oneshot, not a shared slot. The value crosses the
    // JS boundary exactly once. The shape `Arc<Mutex<Option<..>>>` needs
    // three `.expect("outcome slot")` panics, so a shared slot adds nothing.
    // Carrying `Result<T>` in Rust memory instead of
    // rejecting the JS promise is the part that matters. The code preserves
    // that part. A rejection can only carry a stringified error. Losing the
    // variant would turn `CasConflict` into an opaque backend error. That
    // would break `probe_store`.
    let (tx, rx) = futures::channel::oneshot::channel::<Result<T>>();
    // The executor of `Promise::new` is `FnMut`. The code therefore
    // moves the sender out on the single invocation. The code does not capture
    // the sender by reference.
    let mut tx = Some(tx);
    let mut guarded = Some(guarded);

    // The browser invokes the callback exactly once per
    // `request`. The callback must return a promise that settles only after
    // `guarded` has finished. The browser holds the lock for that promise.
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
                // Send the value before `_settle` drops. The browser
                // releases the lock when the promise settles. Therefore the
                // receiver must already hold the value by then. Dropping a
                // closed sender is fine. A caller that has gone away is not an
                // error here.
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
    /// whatever the host actually implements. Browsers and Node 24+ have
    /// `navigator.locks`. Node 22, which is the version that the CI `wasm`
    /// job installs, does not. That host is exactly where the fallback has to
    /// hold up. Therefore each test runs where its subject exists. Each test
    /// says so loudly when the subject does not exist, rather than passing
    /// vacuously.
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
        // Stable. The same path text always gives the same lock name.
        assert_eq!(
            lock_name(&path),
            lock_name(&ObjectPath::from("e000007/wal/00000042.log"))
        );
        // Namespaced, so the code cannot collide with the locks
        // of another library on the same origin.
        assert_eq!(lock_name(&path), "oxkv/opfs/e000007/wal/00000042.log");
    }

    #[wasm_bindgen_test]
    fn lock_name_is_distinct_per_object_and_prefix() {
        let base = ObjectPath::new("e000007/wal/00000042.log");
        // The code tests distinct objects. The list includes the
        // string-prefix traps that a truncating or joining scheme would
        // collapse.
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
        // A conflict that the locked section raises must reach the
        // caller as `CasConflict`. The conflict must not reach the caller as
        // an opaque backend error. `probe_store` matches on the variant. A JS
        // rejection could not carry the variant.
        let conflicted = with_object_lock(&path, || async {
            Err::<u32, _>(StoreError::CasConflict("stale".to_string()))
        })
        .await;
        // The call uses the same lock name as the failed call. The
        // lock has to have been released, or this call would never return.
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
            // The code must yield. The two `contend` futures are only
            // genuinely concurrent if the guarded section reaches an await
            // point. With a purely synchronous body, neither caller can
            // interleave inside it. This test would then pass even with
            // `with_object_lock` replaced by a bare `guarded().await`. The
            // sleep makes "no overlap" mean "mutual exclusion held". Without
            // the sleep, "no overlap" means only "nothing could ever overlap".
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
        // The sections do not overlap. A `start` may only follow the
        // `end` of the other caller. Holding the lock for the whole section
        // buys exactly this.
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
