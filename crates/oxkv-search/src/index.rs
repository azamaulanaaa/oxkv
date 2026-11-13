//! Index and tokenizer checks for the index crate.
//!
//! Essay 18 owns the index build and the tokenizer. This module exposes the
//! named errors and the small checks that the coordinator runs. The module
//! does not build an index and does not tokenize text.

use oxkv_contracts::version::{ContentDigest, ShardId, TokenizerVersions};

use crate::error::SearchError;

/// Checks one shard index digest.
///
/// # Errors
///
/// Returns [`SearchError::IndexMismatch`] when the found digest differs from
/// the expected digest.
pub fn verify_index_digest(
    shard_id: &ShardId,
    expected: ContentDigest,
    found: Option<ContentDigest>,
) -> Result<(), SearchError> {
    if found == Some(expected) {
        Ok(())
    } else {
        Err(SearchError::IndexMismatch {
            shard_id: shard_id.clone(),
            expected: Some(expected),
            found,
        })
    }
}

/// Checks a tokenizer version map against the expected map.
///
/// Every expected field must exist in the found map with the same version. An
/// extra found field is allowed.
///
/// # Errors
///
/// Returns [`SearchError::TokenizerUnknown`] when a field is absent. Returns
/// [`SearchError::TokenizerMismatch`] when a field version differs.
pub fn verify_tokenizer_versions(
    expected: &TokenizerVersions,
    found: &TokenizerVersions,
) -> Result<(), SearchError> {
    for (field, expected_version) in expected.iter() {
        match found.get(field) {
            None => {
                return Err(SearchError::TokenizerUnknown {
                    field: field.to_owned(),
                });
            }
            Some(found_version) if found_version != expected_version => {
                return Err(SearchError::TokenizerMismatch {
                    field: field.to_owned(),
                    expected: expected_version,
                    found: found_version,
                });
            }
            Some(_) => {}
        }
    }
    Ok(())
}

/// A token count budget for one field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenBudget {
    limit: u64,
    used: u64,
}

impl TokenBudget {
    /// Creates a token budget.
    #[must_use]
    pub const fn new(limit: u64) -> Self {
        Self { limit, used: 0 }
    }

    /// Creates a token budget with no practical limit.
    #[must_use]
    pub const fn unlimited() -> Self {
        Self::new(u64::MAX)
    }

    /// Returns the token limit.
    #[must_use]
    pub const fn limit(&self) -> u64 {
        self.limit
    }

    /// Returns the used token count.
    #[must_use]
    pub const fn used(&self) -> u64 {
        self.used
    }

    /// Charges a token count against the budget.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::TokenBudgetExceeded`] when the charge passes the
    /// limit. The used count does not change.
    pub fn charge(&mut self, tokens: u64) -> Result<(), SearchError> {
        let required = self.used.saturating_add(tokens);
        if required > self.limit {
            Err(SearchError::TokenBudgetExceeded {
                required,
                limit: self.limit,
            })
        } else {
            self.used = required;
            Ok(())
        }
    }
}
