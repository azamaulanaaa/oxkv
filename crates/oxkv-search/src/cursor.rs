//! Search cursor tokens and cursor validation.
//!
//! A [`SearchCursor`] carries the pin, the query fingerprint, and the last
//! sort tuple. The pin lifetime is the cursor expiry. A changed query, scope,
//! sort, or pin fails with [`SearchError::CursorInvalid`]. An expired pin fails
//! with [`SearchError::PinExpired`].

use oxkv_contracts::cursor::{CursorError, CursorToken, SearchCursor};

use crate::error::{CursorInvalidReason, SearchError};
use crate::hit::{QueryOrder, SortPosition};
use crate::pin::{PinRetention, SearchPin};

/// Encodes a search cursor as one opaque token.
#[must_use]
pub fn encode_cursor(cursor: &SearchCursor) -> Vec<u8> {
    cursor.to_token()
}

/// Decodes one opaque search cursor token.
///
/// # Errors
///
/// Returns [`SearchError::CursorInvalid`] when the token is malformed,
/// tampered, from another cursor type, or from an unknown token version.
pub fn decode_cursor(token: &[u8]) -> Result<SearchCursor, SearchError> {
    SearchCursor::from_token(token).map_err(map_cursor_error)
}

const fn map_cursor_error(error: CursorError) -> SearchError {
    let reason = match error {
        CursorError::Invalid => CursorInvalidReason::Malformed,
        CursorError::UnsupportedVersion { .. } => CursorInvalidReason::UnsupportedVersion,
        CursorError::TypeMismatch { .. } => CursorInvalidReason::TypeMismatch,
        CursorError::Expired { .. } => CursorInvalidReason::Expired,
    };
    SearchError::CursorInvalid { reason }
}

/// Validates a cursor against a request pin, order, and retention floor.
///
/// # Errors
///
/// Returns [`SearchError::CursorInvalid`] when the pin, query fingerprint, or
/// sort tuple does not match the request. Returns [`SearchError::PinExpired`]
/// when the cursor pin is below the retention floor.
pub fn validate_cursor(
    cursor: &SearchCursor,
    pin: &SearchPin,
    order: QueryOrder,
    retention: &PinRetention,
) -> Result<(), SearchError> {
    if cursor.query_fingerprint != pin.query_fingerprint() {
        return Err(SearchError::CursorInvalid {
            reason: CursorInvalidReason::QueryMismatch,
        });
    }
    if cursor.pin.as_vector() != pin.version_vector().as_vector() {
        return Err(SearchError::CursorInvalid {
            reason: CursorInvalidReason::PinMismatch,
        });
    }
    SortPosition::from_sort_tuple(order, &cursor.sort_tuple)?;
    if !cursor.is_readable_at(retention.oldest_readable()) {
        return Err(SearchError::PinExpired {
            requested: cursor.pin.clone(),
            oldest_readable: retention.oldest_readable().clone(),
        });
    }
    Ok(())
}
