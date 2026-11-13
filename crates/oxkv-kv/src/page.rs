//! One page of ordered entries and its continuation cursor.

use crate::cursor::Cursor;
use crate::entry::Entry;

/// One bounded page of entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    entries: Vec<Entry>,
    next: Option<Cursor>,
}

impl Page {
    pub(crate) const fn new(entries: Vec<Entry>, next: Option<Cursor>) -> Self {
        Self { entries, next }
    }

    /// Returns the entries in scan order.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Returns the continuation cursor when more entries remain.
    #[must_use]
    pub const fn next_cursor(&self) -> Option<&Cursor> {
        self.next.as_ref()
    }

    /// Returns true when the page has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
