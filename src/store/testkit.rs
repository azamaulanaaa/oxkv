//! Helpers for tests and examples.
//!
//! The helpers in this module make parallel tests easy to isolate. They add
//! no production behaviour.

use std::sync::atomic::{AtomicU64, Ordering};

/// Counter behind [`unique_prefix`].
///
/// The counter is process-wide. It uses no wall-clock time, because the
/// `std::time` clocks panic on `wasm32`.
static PREFIX_CTR: AtomicU64 = AtomicU64::new(0);

/// Returns a prefix that is unique within this process.
///
/// The prefix has the shape `{label}-{n}`. Each call returns a different
/// value, even when two calls use the same `label`. Parallel tests in one
/// process therefore do not collide.
///
/// Uniqueness is per process only. Two processes can return the same value.
/// A caller that needs cross-process uniqueness must add its own scope, such
/// as a run id or a bucket name.
///
/// # Examples
///
/// ```
/// use oxkv::testkit::unique_prefix;
///
/// let a = unique_prefix("suite");
/// let b = unique_prefix("suite");
/// assert_ne!(a, b);
/// ```
#[must_use]
pub fn unique_prefix(label: &str) -> String {
    let n = PREFIX_CTR.fetch_add(1, Ordering::SeqCst);
    format!("{label}-{n}")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::unique_prefix;

    /// Repeated calls with the same label must return distinct values.
    #[test]
    fn unique_prefix_returns_distinct_values_for_the_same_label() {
        let first = unique_prefix("same-label");
        let second = unique_prefix("same-label");
        assert_ne!(first, second);
        assert!(first.starts_with("same-label-"));
        assert!(second.starts_with("same-label-"));
    }
}
