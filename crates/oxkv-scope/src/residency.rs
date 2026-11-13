//! Replica residency.
//!
//! Residency is separate from membership. A page may leave the replica when it
//! is out of scope or superseded. An in-scope logical member never leaves.

use std::collections::BTreeMap;

use oxkv_contracts::version::{CommitVersion, PageId, PrefixBounds, ShardId};

use crate::error::ResidencyError;
use crate::membership::Membership;

/// One resident page.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResidencyPage {
    /// The deterministic page identity.
    pub page_id: PageId,
    /// The shard that owns the page.
    pub shard_id: ShardId,
    /// The page key range.
    pub bounds: PrefixBounds,
    /// The page commit version.
    pub commit_version: CommitVersion,
    /// The keys in the page that are logical members.
    pub member_keys: Vec<Vec<u8>>,
    /// True when a newer page replaces this page.
    pub superseded: bool,
}

impl ResidencyPage {
    /// Creates one resident page.
    #[must_use]
    pub const fn new(
        page_id: PageId,
        shard_id: ShardId,
        bounds: PrefixBounds,
        commit_version: CommitVersion,
        member_keys: Vec<Vec<u8>>,
    ) -> Self {
        Self {
            page_id,
            shard_id,
            bounds,
            commit_version,
            member_keys,
            superseded: false,
        }
    }
}

/// The reason that a page leaves the residency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EvictionReason {
    /// The page is outside the scope prefixes.
    OutOfScope,
    /// A newer page replaces the page.
    Superseded,
}

impl EvictionReason {
    /// Returns the reason name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::OutOfScope => "out_of_scope",
            Self::Superseded => "superseded",
        }
    }
}

/// One evicted page.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EvictedPage {
    /// The page identity.
    pub page_id: PageId,
    /// The eviction reason.
    pub reason: EvictionReason,
}

/// The resident pages of one replica.
///
/// The type holds page metadata only. It does not hold storage bytes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReplicaResidency {
    pages: BTreeMap<PageId, ResidencyPage>,
}

impl ReplicaResidency {
    /// Creates an empty residency.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the page count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pages.len()
    }

    /// Returns true when no page is resident.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    /// Inserts one page. The method returns the previous page with the identity.
    #[must_use]
    pub fn insert(&mut self, page: ResidencyPage) -> Option<ResidencyPage> {
        self.pages.insert(page.page_id, page)
    }

    /// Returns one page.
    #[must_use]
    pub fn page(&self, page_id: &PageId) -> Option<&ResidencyPage> {
        self.pages.get(page_id)
    }

    /// Marks one page as superseded.
    ///
    /// # Errors
    ///
    /// Returns [`ResidencyError::PageNotFound`] when the page is absent.
    pub fn mark_superseded(&mut self, page_id: &PageId) -> Result<(), ResidencyError> {
        let page = self
            .pages
            .get_mut(page_id)
            .ok_or(ResidencyError::PageNotFound { page_id: *page_id })?;
        page.superseded = true;
        Ok(())
    }

    /// Evicts one page.
    ///
    /// The method allows an out-of-scope page and a superseded page. It rejects
    /// a current in-scope page. It does not change membership.
    ///
    /// # Errors
    ///
    /// Returns [`ResidencyError::PageNotFound`] when the page is absent.
    /// Returns [`ResidencyError::InScopeMember`] when a current page holds an
    /// in-scope member. Returns [`ResidencyError::NotEvictable`] when a current
    /// page is in scope and holds no member.
    pub fn evict(
        &mut self,
        page_id: &PageId,
        scope_prefixes: &[PrefixBounds],
        membership: &Membership,
    ) -> Result<EvictedPage, ResidencyError> {
        let page = self
            .pages
            .get(page_id)
            .ok_or(ResidencyError::PageNotFound { page_id: *page_id })?;
        if page.superseded {
            self.pages.remove(page_id);
            return Ok(EvictedPage {
                page_id: *page_id,
                reason: EvictionReason::Superseded,
            });
        }
        if let Some(key) = page.member_keys.iter().find(|key| membership.contains(key)) {
            return Err(ResidencyError::InScopeMember {
                page_id: *page_id,
                key: key.clone(),
            });
        }
        if !is_out_of_scope(&page.bounds, scope_prefixes) {
            return Err(ResidencyError::NotEvictable { page_id: *page_id });
        }
        self.pages.remove(page_id);
        Ok(EvictedPage {
            page_id: *page_id,
            reason: EvictionReason::OutOfScope,
        })
    }

    /// Evicts every out-of-scope page that holds no in-scope member.
    ///
    /// The method does not evict a superseded page. Use [`Self::evict`] for one
    /// page with an explicit reason.
    #[must_use]
    pub fn evict_out_of_scope(
        &mut self,
        scope_prefixes: &[PrefixBounds],
        membership: &Membership,
    ) -> Vec<EvictedPage> {
        let candidates: Vec<PageId> = self
            .pages
            .values()
            .filter(|page| !page.superseded)
            .filter(|page| is_out_of_scope(&page.bounds, scope_prefixes))
            .filter(|page| !page.member_keys.iter().any(|key| membership.contains(key)))
            .map(|page| page.page_id)
            .collect();
        candidates
            .into_iter()
            .filter_map(|page_id| {
                self.pages.remove(&page_id)?;
                Some(EvictedPage {
                    page_id,
                    reason: EvictionReason::OutOfScope,
                })
            })
            .collect()
    }
}

fn is_out_of_scope(bounds: &PrefixBounds, scope_prefixes: &[PrefixBounds]) -> bool {
    !scope_prefixes
        .iter()
        .any(|prefix| bounds_intersect(bounds, prefix))
}

fn bounds_intersect(left: &PrefixBounds, right: &PrefixBounds) -> bool {
    !left.is_empty()
        && !right.is_empty()
        && left.lower() < right.upper()
        && right.lower() < left.upper()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::declaration::TablePrefix;
    use crate::diff::{DiffInterval, DiffRecord};
    use crate::membership::ScopeMaterialization;
    use oxkv_contracts::version::{
        KeyVersion, PolicyVersion, ProjectionEpoch, ProjectionWatermarks, ScopeIdentity,
        ScopeVersionVector, VersionVector,
    };

    fn membership() -> Membership {
        let mut membership = Membership::new(ScopeMaterialization::new(
            ScopeIdentity::from_bytes([4; 32]),
            PolicyVersion::from_bytes([1; 32]),
            ProjectionEpoch::new(1),
            ProjectionWatermarks::new(),
        ));
        let mut vector = VersionVector::new();
        vector.set(ShardId::new(vec![1]), CommitVersion::new(1));
        membership
            .apply(
                &DiffInterval::new(
                    membership.identity(),
                    1,
                    PolicyVersion::from_bytes([1; 32]),
                    ScopeVersionVector::new(),
                    ScopeVersionVector::from_vector(vector),
                    vec![DiffRecord::add(
                        b"accounts/1".to_vec(),
                        KeyVersion::new(1),
                        10,
                    )],
                )
                .expect("the interval is valid"),
            )
            .expect("the interval applies");
        membership
    }

    fn page(bounds: PrefixBounds, member_keys: Vec<Vec<u8>>, byte: u8) -> ResidencyPage {
        ResidencyPage::new(
            PageId::from_bytes([byte; 32]),
            ShardId::new(vec![1]),
            bounds,
            CommitVersion::new(1),
            member_keys,
        )
    }

    #[test]
    fn an_out_of_scope_page_leaves_without_a_membership_change() {
        let membership = membership();
        let mut residency = ReplicaResidency::new();
        let out_of_scope = TablePrefix::parse("other_table")
            .expect("valid table")
            .bounds()
            .clone();
        let _ = residency.insert(page(out_of_scope, Vec::new(), 1));
        let before = membership.len();
        let evicted = residency
            .evict(
                &PageId::from_bytes([1; 32]),
                &[accounts_bounds()],
                &membership,
            )
            .expect("the page leaves");
        assert_eq!(evicted.reason, EvictionReason::OutOfScope);
        assert_eq!(membership.len(), before);
        assert!(residency.is_empty());
    }

    #[test]
    fn an_in_scope_member_page_does_not_leave() {
        let membership = membership();
        let mut residency = ReplicaResidency::new();
        let _ = residency.insert(page(accounts_bounds(), vec![b"accounts/1".to_vec()], 2));
        let error = residency
            .evict(
                &PageId::from_bytes([2; 32]),
                &[accounts_bounds()],
                &membership,
            )
            .unwrap_err();
        assert_eq!(
            error,
            ResidencyError::InScopeMember {
                page_id: PageId::from_bytes([2; 32]),
                key: b"accounts/1".to_vec()
            }
        );
        assert_eq!(error.code(), "replication.only_copy_eviction");
        assert_eq!(membership.len(), 1);
        assert!(membership.contains(b"accounts/1"));
    }

    #[test]
    fn a_superseded_page_leaves() {
        let membership = membership();
        let mut residency = ReplicaResidency::new();
        let page = page(accounts_bounds(), vec![b"accounts/1".to_vec()], 3);
        let page_id = page.page_id;
        let _ = residency.insert(page);
        residency
            .mark_superseded(&page_id)
            .expect("the page is marked");
        let evicted = residency
            .evict(&page_id, &[accounts_bounds()], &membership)
            .expect("the page leaves");
        assert_eq!(evicted.reason, EvictionReason::Superseded);
        assert_eq!(membership.len(), 1);
    }

    fn accounts_bounds() -> PrefixBounds {
        TablePrefix::parse("accounts")
            .expect("valid table")
            .bounds()
            .clone()
    }
}
