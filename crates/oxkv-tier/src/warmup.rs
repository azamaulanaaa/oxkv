//! Bounded warm-up with a resumable cursor.
//!
//! A warm-up promotes at most the declared budget. A warm-up that needs more
//! returns [`TierError::WarmupBudgetExceeded`] and a [`WarmupCursor`] that
//! names the next item. It never promotes a partial page.

use oxkv_contracts::version::ContentDigest;

use crate::error::TierError;
use crate::movement::Movement;
use crate::tier::{ArtifactId, Tier, TierStack};

/// One artifact in a warm-up request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WarmupItem {
    artifact: ArtifactId,
    content_digest: ContentDigest,
}

impl WarmupItem {
    /// Creates one warm-up item.
    #[must_use]
    pub const fn new(artifact: ArtifactId, content_digest: ContentDigest) -> Self {
        Self {
            artifact,
            content_digest,
        }
    }

    /// Returns the artifact identity.
    #[must_use]
    pub const fn artifact(&self) -> &ArtifactId {
        &self.artifact
    }

    /// Returns the expected content digest.
    #[must_use]
    pub const fn content_digest(&self) -> ContentDigest {
        self.content_digest
    }
}

/// A resumable warm-up position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WarmupCursor {
    next_index: usize,
}

impl WarmupCursor {
    /// Creates a cursor at one item index.
    #[must_use]
    pub const fn new(next_index: usize) -> Self {
        Self { next_index }
    }

    /// Returns a cursor at the start of a request.
    #[must_use]
    pub const fn start() -> Self {
        Self { next_index: 0 }
    }

    /// Returns the next item index.
    #[must_use]
    pub const fn next_index(self) -> usize {
        self.next_index
    }
}

impl Default for WarmupCursor {
    fn default() -> Self {
        Self::start()
    }
}

/// A warm-up byte and artifact budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WarmupBudget {
    max_artifacts: usize,
    max_bytes: usize,
}

impl WarmupBudget {
    /// Creates a warm-up budget.
    #[must_use]
    pub const fn new(max_artifacts: usize, max_bytes: usize) -> Self {
        Self {
            max_artifacts,
            max_bytes,
        }
    }

    /// Returns the maximum number of promoted artifacts.
    #[must_use]
    pub const fn max_artifacts(self) -> usize {
        self.max_artifacts
    }

    /// Returns the maximum number of promoted bytes.
    #[must_use]
    pub const fn max_bytes(self) -> usize {
        self.max_bytes
    }
}

/// The result of one bounded warm-up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WarmupOutcome {
    cursor: WarmupCursor,
    promoted: Vec<Movement>,
    promoted_bytes: usize,
    halted: Option<TierError>,
    complete: bool,
}

impl WarmupOutcome {
    /// Returns the cursor for the next call.
    #[must_use]
    pub const fn cursor(&self) -> WarmupCursor {
        self.cursor
    }

    /// Returns every completed movement.
    #[must_use]
    pub fn promoted(&self) -> &[Movement] {
        &self.promoted
    }

    /// Returns the promoted byte count.
    #[must_use]
    pub const fn promoted_bytes(&self) -> usize {
        self.promoted_bytes
    }

    /// Returns the named stop reason.
    #[must_use]
    pub const fn halted(&self) -> Option<TierError> {
        self.halted
    }

    /// Returns true when every requested item was promoted.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.complete
    }

    /// Returns the named stop reason as a contract code.
    #[must_use]
    pub const fn stop_code(&self) -> Option<&'static str> {
        match self.halted {
            Some(error) => Some(error.code()),
            None => None,
        }
    }
}

impl TierStack {
    /// Promotes items to RAM under a budget.
    ///
    /// The function stops before the item that would cross the budget. It
    /// returns that item index in the cursor.
    ///
    /// # Errors
    ///
    /// The function reports a stop through [`WarmupOutcome::halted`]. A stop is
    /// not a panic and it keeps every completed movement.
    #[must_use]
    pub fn warmup(
        &mut self,
        items: &[WarmupItem],
        cursor: WarmupCursor,
        budget: WarmupBudget,
    ) -> WarmupOutcome {
        let mut index = cursor.next_index().min(items.len());
        let mut promoted = Vec::new();
        let mut promoted_bytes: usize = 0;
        let mut halted = None;

        while index < items.len() {
            if promoted.len() >= budget.max_artifacts() {
                halted = Some(TierError::WarmupBudgetExceeded);
                break;
            }
            let item = &items[index];
            let item_len = match self.artifact_len(item.artifact()) {
                Ok(len) => len,
                Err(error) => {
                    halted = Some(error);
                    break;
                }
            };
            if promoted_bytes.saturating_add(item_len) > budget.max_bytes() {
                halted = Some(TierError::WarmupBudgetExceeded);
                break;
            }
            match self.move_artifact(item.artifact(), item.content_digest(), Tier::Ram) {
                Ok(movement) => {
                    promoted_bytes = promoted_bytes.saturating_add(movement.byte_len());
                    promoted.push(movement);
                    index += 1;
                }
                Err(error) => {
                    halted = Some(error);
                    break;
                }
            }
        }

        WarmupOutcome {
            cursor: WarmupCursor::new(index),
            promoted,
            promoted_bytes,
            halted,
            complete: halted.is_none() && index == items.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use oxkv_contracts::version::{ContentDigest, PageSequence, ShardId};

    use crate::error::TierError;
    use crate::tier::{ArtifactId, Tier, TierStack};
    use crate::warmup::{WarmupBudget, WarmupCursor, WarmupItem};

    fn artifact(page: u64) -> ArtifactId {
        ArtifactId::new(ShardId::new(vec![1]), 2, PageSequence::new(page))
    }

    fn item(page: u64, bytes: &[u8]) -> WarmupItem {
        WarmupItem::new(artifact(page), ContentDigest::of_bytes(bytes))
    }

    fn stack_with(items: &[(u64, &[u8])]) -> TierStack {
        let mut stack = TierStack::memory();
        let driver = stack
            .driver_mut(Tier::Object)
            .expect("the memory stack has every tier");
        for (page, bytes) in items {
            driver.put(artifact(*page), bytes.to_vec());
        }
        stack
    }

    #[test]
    fn a_warmup_over_budget_returns_a_cursor() {
        let mut stack = stack_with(&[(1, b"aaaa"), (2, b"bbbb")]);
        let items = [item(1, b"aaaa"), item(2, b"bbbb")];
        let outcome = stack.warmup(&items, WarmupCursor::start(), WarmupBudget::new(8, 4));
        assert_eq!(outcome.halted(), Some(TierError::WarmupBudgetExceeded));
        assert_eq!(
            outcome.stop_code(),
            Some("replication.warmup_budget_exceeded")
        );
        assert_eq!(outcome.cursor().next_index(), 1);
        assert_eq!(outcome.promoted().len(), 1);
        assert_eq!(outcome.promoted_bytes(), 4);
    }

    #[test]
    fn a_warmup_resumes_from_the_cursor() {
        let mut stack = stack_with(&[(1, b"aaaa"), (2, b"bbbb")]);
        let items = [item(1, b"aaaa"), item(2, b"bbbb")];
        let first = stack.warmup(&items, WarmupCursor::start(), WarmupBudget::new(1, 8));
        assert_eq!(first.cursor().next_index(), 1);
        let second = stack.warmup(&items, first.cursor(), WarmupBudget::new(1, 8));
        assert_eq!(second.cursor().next_index(), 2);
        assert!(second.is_complete());
        assert_eq!(second.promoted().len(), 1);
        assert_eq!(
            stack.read_from(&artifact(1), Tier::Ram),
            Ok(b"aaaa".to_vec())
        );
        assert_eq!(
            stack.read_from(&artifact(2), Tier::Ram),
            Ok(b"bbbb".to_vec())
        );
    }

    #[test]
    fn an_oversized_item_is_not_promoted() {
        let mut stack = stack_with(&[(1, b"abcdef")]);
        let items = [item(1, b"abcdef")];
        let outcome = stack.warmup(&items, WarmupCursor::start(), WarmupBudget::new(1, 4));
        assert_eq!(outcome.halted(), Some(TierError::WarmupBudgetExceeded));
        assert_eq!(outcome.cursor().next_index(), 0);
        assert_eq!(outcome.promoted().len(), 0);
    }

    #[test]
    fn an_empty_request_is_complete() {
        let mut stack = TierStack::memory();
        let outcome = stack.warmup(&[], WarmupCursor::start(), WarmupBudget::new(1, 1));
        assert!(outcome.is_complete());
        assert_eq!(outcome.cursor(), WarmupCursor::start());
    }
}
