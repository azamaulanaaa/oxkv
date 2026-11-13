//! The scope declaration as data.
//!
//! A scope names one or more user table prefixes, a policy selector, a
//! projection, an ordering, and the admission limits. The declaration has a
//! canonical byte form. The identity covers those bytes (see [`crate::identity`]).
//!
//! Device and hydration budgets are separate from the declaration. Two clients
//! with different devices must compute one identity for one question.

use core::fmt;

use oxkv_contracts::keyspace::{KEY_ENCODING_VERSION, SYSTEM_MARKER, TableName};
use oxkv_contracts::version::{PolicyVersion, PrefixBounds};

use crate::error::{AdmissionLimit, ScopeError};

/// One user table prefix.
///
/// The prefix is derived from a user table name. The form is
/// `0x01 || table-name || 0x00`. An authority system namespace is not valid.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TablePrefix {
    table: TableName,
    bytes: Vec<u8>,
    bounds: PrefixBounds,
}

impl TablePrefix {
    /// Parses one user table name and derives its prefix.
    ///
    /// # Errors
    ///
    /// Returns [`ScopeError::SystemNamespace`] for a name that starts with
    /// `_`. Returns [`ScopeError::InvalidTableName`] for a grammar mismatch.
    pub fn parse(name: &str) -> Result<Self, ScopeError> {
        if name.starts_with('_') {
            return Err(ScopeError::SystemNamespace {
                name: name.to_owned(),
            });
        }
        let table = TableName::parse_user(name).map_err(|_| ScopeError::InvalidTableName {
            name: name.to_owned(),
        })?;
        let mut bytes = Vec::with_capacity(name.len() + 2);
        bytes.push(KEY_ENCODING_VERSION);
        bytes.extend_from_slice(table.as_str().as_bytes());
        bytes.push(0x00);
        if bytes.contains(&SYSTEM_MARKER) {
            return Err(ScopeError::SystemNamespace {
                name: name.to_owned(),
            });
        }
        let mut upper = bytes.clone();
        if let Some(last) = upper.last_mut() {
            *last = 0x01;
        }
        let bounds = PrefixBounds::new(bytes.clone(), upper);
        Ok(Self {
            table,
            bytes,
            bounds,
        })
    }

    /// Returns the table name.
    #[must_use]
    pub fn table(&self) -> &TableName {
        &self.table
    }

    /// Returns the raw prefix bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the inclusive lower bound and the exclusive upper bound.
    #[must_use]
    pub fn bounds(&self) -> &PrefixBounds {
        &self.bounds
    }
}

impl fmt::Display for TablePrefix {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.table, formatter)
    }
}

/// A rule set selector.
///
/// The selector names a rule set. The policy version does not enter the
/// selector. A consumer pins the pair `(identity, policy version)` for an
/// immutable answer.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RuleSetSelector(String);

impl RuleSetSelector {
    /// Parses one rule set selector.
    ///
    /// The grammar is `[a-z][a-z0-9_.-]*`. The maximum length is 128 bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ScopeError::InvalidRuleSetSelector`] for a grammar mismatch.
    pub fn new(value: &str) -> Result<Self, ScopeError> {
        let valid_length = !value.is_empty() && value.len() <= 128;
        let mut bytes = value.bytes();
        let valid_first = bytes.next().is_some_and(|byte| byte.is_ascii_lowercase());
        let valid_rest = bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'.' | b'-')
        });
        if valid_length && valid_first && valid_rest {
            Ok(Self(value.to_owned()))
        } else {
            Err(ScopeError::InvalidRuleSetSelector {
                value: value.to_owned(),
            })
        }
    }

    /// Returns the selector text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RuleSetSelector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A policy selector for one scope.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PolicySelector {
    /// Select a rule set by name. A policy edit keeps the identity stable.
    RuleSet(RuleSetSelector),
    /// Reference one canonical policy AST. A policy edit changes the identity.
    Canonical(PolicyVersion),
}

/// A field name.
///
/// The grammar is `[a-z][a-z0-9_]*`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FieldName(String);

impl FieldName {
    /// Parses one field name.
    ///
    /// # Errors
    ///
    /// Returns [`ScopeError::InvalidFieldName`] for a grammar mismatch.
    pub fn new(value: &str) -> Result<Self, ScopeError> {
        let mut bytes = value.bytes();
        let valid_first = bytes.next().is_some_and(|byte| byte.is_ascii_lowercase());
        let valid_rest =
            bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
        if valid_first && valid_rest {
            Ok(Self(value.to_owned()))
        } else {
            Err(ScopeError::InvalidFieldName {
                field: value.to_owned(),
            })
        }
    }

    /// Returns the field name text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for FieldName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum ProjectionKind {
    All,
    Fields(Vec<FieldName>),
}

/// A projection selector.
///
/// The selector names every field that the scope materializes. The canonical
/// form sorts and deduplicates the field list.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProjectionSelector(ProjectionKind);

impl ProjectionSelector {
    /// Selects every declared field.
    #[must_use]
    pub const fn all() -> Self {
        Self(ProjectionKind::All)
    }

    /// Selects a field list.
    ///
    /// The method sorts and deduplicates the fields. An empty list is invalid.
    ///
    /// # Errors
    ///
    /// Returns [`ScopeError::InvalidFieldName`] for a grammar mismatch.
    pub fn fields(fields: impl IntoIterator<Item = FieldName>) -> Result<Self, ScopeError> {
        let mut fields: Vec<FieldName> = fields.into_iter().collect();
        fields.sort();
        fields.dedup();
        if fields.is_empty() {
            return Err(ScopeError::InvalidFieldName {
                field: String::new(),
            });
        }
        Ok(Self(ProjectionKind::Fields(fields)))
    }

    /// Returns true when the selector names every field.
    #[must_use]
    pub const fn is_all(&self) -> bool {
        matches!(self.0, ProjectionKind::All)
    }

    /// Returns the selected fields, or `None` for every field.
    #[must_use]
    pub fn fields_slice(&self) -> Option<&[FieldName]> {
        match &self.0 {
            ProjectionKind::All => None,
            ProjectionKind::Fields(fields) => Some(fields),
        }
    }
}

/// The declared order of one scope query.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Ordering {
    /// Canonical key ascending.
    CanonicalKey,
    /// Declared sort field ascending, then canonical key ascending.
    Field(FieldName),
}

/// The admission limits of one scope.
///
/// The limits apply at subscribe. An over-limit scope fails with
/// `scope.over_limit` and transfers no data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[allow(clippy::struct_field_names)]
pub struct AdmissionLimits {
    max_members: u64,
    max_bytes: u64,
    max_shards: u64,
}

impl AdmissionLimits {
    /// Creates admission limits.
    #[must_use]
    pub const fn new(max_members: u64, max_bytes: u64, max_shards: u64) -> Self {
        Self {
            max_members,
            max_bytes,
            max_shards,
        }
    }

    /// Creates limits with no practical bound.
    #[must_use]
    pub const fn unbounded() -> Self {
        Self::new(u64::MAX, u64::MAX, u64::MAX)
    }

    /// Returns the maximum member count.
    #[must_use]
    pub const fn max_members(self) -> u64 {
        self.max_members
    }

    /// Returns the maximum member byte count.
    #[must_use]
    pub const fn max_bytes(self) -> u64 {
        self.max_bytes
    }

    /// Returns the maximum shard count.
    #[must_use]
    pub const fn max_shards(self) -> u64 {
        self.max_shards
    }
}

/// The observed size of one scope at subscribe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ScopeStats {
    members: u64,
    bytes: u64,
    shards: u64,
}

impl ScopeStats {
    /// Creates scope statistics.
    #[must_use]
    pub const fn new(members: u64, bytes: u64, shards: u64) -> Self {
        Self {
            members,
            bytes,
            shards,
        }
    }

    /// Returns the member count.
    #[must_use]
    pub const fn members(self) -> u64 {
        self.members
    }

    /// Returns the member byte count.
    #[must_use]
    pub const fn bytes(self) -> u64 {
        self.bytes
    }

    /// Returns the shard count.
    #[must_use]
    pub const fn shards(self) -> u64 {
        self.shards
    }
}

/// The device and hydration budgets of one replica.
///
/// The budgets do not enter the scope identity. A phone and a desktop can
/// declare one question and compute one identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscribeBudget {
    device_bytes: u64,
    hydration_overhead_bytes: u64,
}

impl SubscribeBudget {
    /// Creates a subscribe budget.
    #[must_use]
    pub const fn new(device_bytes: u64, hydration_overhead_bytes: u64) -> Self {
        Self {
            device_bytes,
            hydration_overhead_bytes,
        }
    }

    /// Returns the device byte budget.
    #[must_use]
    pub const fn device_bytes(self) -> u64 {
        self.device_bytes
    }

    /// Returns the hydration overhead byte budget.
    #[must_use]
    pub const fn hydration_overhead_bytes(self) -> u64 {
        self.hydration_overhead_bytes
    }
}

/// The admission mode of one scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdmissionMode {
    /// The scope fits the device and hydration budgets.
    Exact,
    /// The scope is larger than the device. It is not exact offline.
    Degraded,
}

impl AdmissionMode {
    /// Returns the mode name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Degraded => "degraded",
        }
    }
}

/// The reason for a degraded admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DegradationReason {
    /// The scope does not fit the device budget.
    DeviceBudget,
    /// The scope does not fit the hydration budget.
    HydrationBudget,
}

impl DegradationReason {
    /// Returns the reason name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::DeviceBudget => "device_budget",
            Self::HydrationBudget => "hydration_budget",
        }
    }
}

/// The result of a subscribe admission check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AdmissionReport {
    mode: AdmissionMode,
    stats: ScopeStats,
    hydration_peak_bytes: u64,
    reason: Option<DegradationReason>,
}

impl AdmissionReport {
    /// Returns the admission mode.
    #[must_use]
    pub const fn mode(self) -> AdmissionMode {
        self.mode
    }

    /// Returns the observed scope statistics.
    #[must_use]
    pub const fn stats(self) -> ScopeStats {
        self.stats
    }

    /// Returns the hydration peak byte count.
    #[must_use]
    pub const fn hydration_peak_bytes(self) -> u64 {
        self.hydration_peak_bytes
    }

    /// Returns the degradation reason.
    #[must_use]
    pub const fn reason(self) -> Option<DegradationReason> {
        self.reason
    }

    /// Returns true only for an exact admission.
    #[must_use]
    pub const fn claims_exact_offline(self) -> bool {
        matches!(self.mode, AdmissionMode::Exact)
    }

    const fn exact(stats: ScopeStats, hydration_peak_bytes: u64) -> Self {
        Self {
            mode: AdmissionMode::Exact,
            stats,
            hydration_peak_bytes,
            reason: None,
        }
    }

    const fn degraded(
        stats: ScopeStats,
        hydration_peak_bytes: u64,
        reason: DegradationReason,
    ) -> Self {
        Self {
            mode: AdmissionMode::Degraded,
            stats,
            hydration_peak_bytes,
            reason: Some(reason),
        }
    }
}

/// One scope declaration.
///
/// The declaration stores its prefixes in canonical order. The input order
/// does not change the identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScopeDeclaration {
    prefixes: Vec<TablePrefix>,
    selector: PolicySelector,
    projection: ProjectionSelector,
    ordering: Ordering,
    limits: AdmissionLimits,
}

impl ScopeDeclaration {
    /// Creates a scope declaration.
    ///
    /// The method sorts and deduplicates the prefixes. It rejects an empty
    /// prefix list.
    ///
    /// # Errors
    ///
    /// Returns [`ScopeError::EmptyScope`] when the prefix list is empty.
    pub fn new(
        prefixes: impl IntoIterator<Item = TablePrefix>,
        selector: PolicySelector,
        projection: ProjectionSelector,
        ordering: Ordering,
        limits: AdmissionLimits,
    ) -> Result<Self, ScopeError> {
        let mut prefixes: Vec<TablePrefix> = prefixes.into_iter().collect();
        if prefixes.is_empty() {
            return Err(ScopeError::EmptyScope);
        }
        prefixes.sort();
        prefixes.dedup();
        Ok(Self {
            prefixes,
            selector,
            projection,
            ordering,
            limits,
        })
    }

    /// Returns the table prefixes in canonical order.
    #[must_use]
    pub fn prefixes(&self) -> &[TablePrefix] {
        &self.prefixes
    }

    /// Returns the policy selector.
    #[must_use]
    pub const fn selector(&self) -> &PolicySelector {
        &self.selector
    }

    /// Returns the projection selector.
    #[must_use]
    pub const fn projection(&self) -> &ProjectionSelector {
        &self.projection
    }

    /// Returns the declared ordering.
    #[must_use]
    pub const fn ordering(&self) -> &Ordering {
        &self.ordering
    }

    /// Returns the admission limits.
    #[must_use]
    pub const fn limits(&self) -> AdmissionLimits {
        self.limits
    }

    /// Returns the canonical declaration bytes.
    ///
    /// The form is stable across hosts and targets. It uses big-endian
    /// integers, length prefixes, and canonical order.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"oxkv/scope/declaration/v1");
        bytes.push(0x00);
        push_u64(
            &mut bytes,
            u64::try_from(self.prefixes.len()).unwrap_or(u64::MAX),
        );
        for prefix in &self.prefixes {
            push_bytes(&mut bytes, prefix.as_bytes());
        }
        match &self.selector {
            PolicySelector::RuleSet(selector) => {
                bytes.push(0x00);
                push_bytes(&mut bytes, selector.as_str().as_bytes());
            }
            PolicySelector::Canonical(policy_version) => {
                bytes.push(0x01);
                bytes.extend_from_slice(policy_version.as_bytes());
            }
        }
        match &self.projection.0 {
            ProjectionKind::All => bytes.push(0x00),
            ProjectionKind::Fields(fields) => {
                bytes.push(0x01);
                push_u64(&mut bytes, u64::try_from(fields.len()).unwrap_or(u64::MAX));
                for field in fields {
                    push_bytes(&mut bytes, field.as_str().as_bytes());
                }
            }
        }
        match &self.ordering {
            Ordering::CanonicalKey => bytes.push(0x00),
            Ordering::Field(field) => {
                bytes.push(0x01);
                push_bytes(&mut bytes, field.as_str().as_bytes());
            }
        }
        for limit in [
            self.limits.max_members,
            self.limits.max_bytes,
            self.limits.max_shards,
        ] {
            push_u64(&mut bytes, limit);
        }
        bytes
    }

    fn check_limits(&self, stats: ScopeStats) -> Result<(), ScopeError> {
        let checks = [
            (
                AdmissionLimit::Members,
                self.limits.max_members,
                stats.members,
            ),
            (AdmissionLimit::Bytes, self.limits.max_bytes, stats.bytes),
            (AdmissionLimit::Shards, self.limits.max_shards, stats.shards),
        ];
        for (limit, limit_value, observed) in checks {
            if observed > limit_value {
                return Err(ScopeError::OverLimit {
                    limit,
                    limit_value,
                    observed,
                });
            }
        }
        Ok(())
    }

    /// Checks one subscribe for exact admission.
    ///
    /// The method checks the admission limits, the device budget, and the
    /// hydration budget. An over-limit scope transfers no data.
    ///
    /// # Errors
    ///
    /// Returns [`ScopeError::OverLimit`] for an admission limit. Returns
    /// [`ScopeError::DeviceBudgetExceeded`] for the device budget. Returns
    /// [`ScopeError::HydrationBudgetExceeded`] for the hydration budget.
    pub fn admit(
        &self,
        stats: ScopeStats,
        budget: SubscribeBudget,
    ) -> Result<AdmissionReport, ScopeError> {
        self.check_limits(stats)?;
        let hydration_peak_bytes = stats.bytes.saturating_mul(2);
        if hydration_peak_bytes > budget.device_bytes {
            return Err(ScopeError::DeviceBudgetExceeded {
                required: hydration_peak_bytes,
                available: budget.device_bytes,
            });
        }
        let overhead = hydration_peak_bytes.saturating_sub(stats.bytes);
        if overhead > budget.hydration_overhead_bytes {
            return Err(ScopeError::HydrationBudgetExceeded {
                required: overhead,
                limit: budget.hydration_overhead_bytes,
            });
        }
        Ok(AdmissionReport::exact(stats, hydration_peak_bytes))
    }

    /// Checks one subscribe and permits a degraded result.
    ///
    /// The method still rejects an over-limit scope. A scope over a device or
    /// hydration budget returns a degraded report. A degraded scope does not
    /// claim exact offline.
    ///
    /// # Errors
    ///
    /// Returns [`ScopeError::OverLimit`] for an admission limit.
    pub fn admit_allow_degraded(
        &self,
        stats: ScopeStats,
        budget: SubscribeBudget,
    ) -> Result<AdmissionReport, ScopeError> {
        match self.admit(stats, budget) {
            Ok(report) => Ok(report),
            Err(ScopeError::DeviceBudgetExceeded { required, .. }) => Ok(
                AdmissionReport::degraded(stats, required, DegradationReason::DeviceBudget),
            ),
            Err(ScopeError::HydrationBudgetExceeded { .. }) => {
                let peak = stats.bytes.saturating_mul(2);
                Ok(AdmissionReport::degraded(
                    stats,
                    peak,
                    DegradationReason::HydrationBudget,
                ))
            }
            Err(error) => Err(error),
        }
    }
}

fn push_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn push_bytes(bytes: &mut Vec<u8>, value: &[u8]) {
    push_u64(bytes, u64::try_from(value.len()).unwrap_or(u64::MAX));
    bytes.extend_from_slice(value);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefix(name: &str) -> TablePrefix {
        TablePrefix::parse(name).expect("the table prefix is valid")
    }

    fn declaration() -> ScopeDeclaration {
        ScopeDeclaration::new(
            vec![prefix("accounts"), prefix("ledger_entry")],
            PolicySelector::RuleSet(RuleSetSelector::new("default").expect("valid selector")),
            ProjectionSelector::all(),
            Ordering::CanonicalKey,
            AdmissionLimits::unbounded(),
        )
        .expect("the declaration is valid")
    }

    #[test]
    fn an_authority_system_namespace_is_rejected() {
        assert_eq!(
            TablePrefix::parse("_catalog"),
            Err(ScopeError::SystemNamespace {
                name: "_catalog".to_owned()
            })
        );
        assert_eq!(
            TablePrefix::parse("_catalog").unwrap_err().code(),
            "manifest.reserved_table_name"
        );
    }

    #[test]
    fn the_prefix_uses_the_key_encoding_form() {
        let prefix = prefix("accounts");
        assert_eq!(
            prefix.as_bytes(),
            &[
                KEY_ENCODING_VERSION,
                b'a',
                b'c',
                b'c',
                b'o',
                b'u',
                b'n',
                b't',
                b's',
                0x00
            ]
        );
        assert_eq!(prefix.bounds().lower(), prefix.as_bytes());
        assert_eq!(prefix.bounds().upper().last(), Some(&0x01));
    }

    #[test]
    fn the_input_order_does_not_change_the_canonical_bytes() {
        let first = ScopeDeclaration::new(
            vec![prefix("accounts"), prefix("ledger_entry")],
            PolicySelector::RuleSet(RuleSetSelector::new("default").expect("valid selector")),
            ProjectionSelector::all(),
            Ordering::CanonicalKey,
            AdmissionLimits::unbounded(),
        )
        .expect("the declaration is valid");
        let second = ScopeDeclaration::new(
            vec![
                prefix("ledger_entry"),
                prefix("accounts"),
                prefix("accounts"),
            ],
            PolicySelector::RuleSet(RuleSetSelector::new("default").expect("valid selector")),
            ProjectionSelector::all(),
            Ordering::CanonicalKey,
            AdmissionLimits::unbounded(),
        )
        .expect("the declaration is valid");
        assert_eq!(first, second);
        assert_eq!(first.canonical_bytes(), second.canonical_bytes());
    }

    #[test]
    fn an_empty_prefix_list_fails() {
        assert_eq!(
            ScopeDeclaration::new(
                Vec::new(),
                PolicySelector::RuleSet(RuleSetSelector::new("default").expect("valid selector")),
                ProjectionSelector::all(),
                Ordering::CanonicalKey,
                AdmissionLimits::unbounded(),
            ),
            Err(ScopeError::EmptyScope)
        );
    }

    #[test]
    fn a_bad_selector_or_field_fails() {
        assert_eq!(
            RuleSetSelector::new("Default"),
            Err(ScopeError::InvalidRuleSetSelector {
                value: "Default".to_owned()
            })
        );
        assert_eq!(
            FieldName::new("1field"),
            Err(ScopeError::InvalidFieldName {
                field: "1field".to_owned()
            })
        );
        assert!(ProjectionSelector::fields(Vec::new()).is_err());
    }

    #[test]
    fn the_projection_fields_are_canonical() {
        let first = ProjectionSelector::fields([
            FieldName::new("title").expect("valid field"),
            FieldName::new("body").expect("valid field"),
            FieldName::new("title").expect("valid field"),
        ])
        .expect("the projection is valid");
        let second = ProjectionSelector::fields([
            FieldName::new("body").expect("valid field"),
            FieldName::new("title").expect("valid field"),
        ])
        .expect("the projection is valid");
        assert_eq!(first, second);
        assert_eq!(
            first
                .fields_slice()
                .expect("the selector names fields")
                .iter()
                .map(FieldName::as_str)
                .collect::<Vec<_>>(),
            vec!["body", "title"]
        );
    }

    #[test]
    fn an_over_limit_scope_fails_before_any_budget_check() {
        let declaration = ScopeDeclaration::new(
            vec![prefix("accounts")],
            PolicySelector::RuleSet(RuleSetSelector::new("default").expect("valid selector")),
            ProjectionSelector::all(),
            Ordering::CanonicalKey,
            AdmissionLimits::new(1, 10, 1),
        )
        .expect("the declaration is valid");
        let error = declaration
            .admit(ScopeStats::new(2, 1, 1), SubscribeBudget::new(100, 100))
            .unwrap_err();
        assert_eq!(
            error,
            ScopeError::OverLimit {
                limit: AdmissionLimit::Members,
                limit_value: 1,
                observed: 2
            }
        );
        assert_eq!(error.code(), "scope.over_limit");
    }

    #[test]
    fn a_device_and_hydration_budget_fail_exactly() {
        let declaration = declaration();
        assert_eq!(
            declaration.admit(ScopeStats::new(1, 60, 1), SubscribeBudget::new(100, 100)),
            Err(ScopeError::DeviceBudgetExceeded {
                required: 120,
                available: 100
            })
        );
        assert_eq!(
            declaration.admit(ScopeStats::new(1, 40, 1), SubscribeBudget::new(100, 10)),
            Err(ScopeError::HydrationBudgetExceeded {
                required: 40,
                limit: 10
            })
        );
    }

    #[test]
    fn a_degraded_scope_does_not_claim_exact_offline() {
        let declaration = declaration();
        let report = declaration
            .admit_allow_degraded(ScopeStats::new(1, 60, 1), SubscribeBudget::new(100, 100))
            .expect("the degraded admission is allowed");
        assert_eq!(report.mode(), AdmissionMode::Degraded);
        assert_eq!(report.reason(), Some(DegradationReason::DeviceBudget));
        assert!(!report.claims_exact_offline());
    }

    #[test]
    fn a_fitting_scope_is_exact() {
        let declaration = declaration();
        let report = declaration
            .admit(ScopeStats::new(1, 20, 1), SubscribeBudget::new(100, 20))
            .expect("the scope fits");
        assert_eq!(report.mode(), AdmissionMode::Exact);
        assert_eq!(report.hydration_peak_bytes(), 40);
        assert!(report.claims_exact_offline());
    }
}
