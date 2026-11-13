//! The index declaration in the manifest.
//!
//! An [`IndexManifest`] declares the indexed fields, the tokenizer identity of
//! each field, the positions flag, the filter and search flags, the scorer and
//! its integer parameters, the overhead budget, and the token budget.

use oxkv_contracts::version::Hash32;

use crate::canonical;
use crate::error::IndexError;
use crate::posting::{FlagDecl, FlagSet};
use crate::score::ScorerSpec;
use crate::tokenizer::{TokenBudget, TokenizerIdentity};

/// One indexed field declaration.
#[allow(clippy::struct_excessive_bools)] // The manifest capabilities are independent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldSpec {
    name: String,
    tokenizer: TokenizerIdentity,
    indexed: bool,
    searchable: bool,
    filterable: bool,
    positions: bool,
    filter_flags: FlagSet,
    search_flags: FlagSet,
}

impl FieldSpec {
    /// Creates a searchable field that stores no positions.
    #[must_use]
    pub fn new(name: impl Into<String>, tokenizer: TokenizerIdentity) -> Self {
        Self {
            name: name.into(),
            tokenizer,
            indexed: true,
            searchable: true,
            filterable: false,
            positions: false,
            filter_flags: FlagSet::default(),
            search_flags: FlagSet::default(),
        }
    }

    /// Sets the indexed flag.
    #[must_use]
    pub const fn with_indexed(mut self, indexed: bool) -> Self {
        self.indexed = indexed;
        self
    }

    /// Sets the searchable flag.
    #[must_use]
    pub const fn with_searchable(mut self, searchable: bool) -> Self {
        self.searchable = searchable;
        self
    }

    /// Sets the filterable flag.
    #[must_use]
    pub const fn with_filterable(mut self, filterable: bool) -> Self {
        self.filterable = filterable;
        self
    }

    /// Sets the positions flag.
    #[must_use]
    pub const fn with_positions(mut self, positions: bool) -> Self {
        self.positions = positions;
        self
    }

    /// Sets the filter flag declarations.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::InvalidManifest`] when a flag name or bit repeats.
    pub fn with_filter_flags(mut self, declarations: Vec<FlagDecl>) -> Result<Self, IndexError> {
        self.filter_flags = FlagSet::new(declarations)?;
        Ok(self)
    }

    /// Sets the search flag declarations.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::InvalidManifest`] when a flag name or bit repeats.
    pub fn with_search_flags(mut self, declarations: Vec<FlagDecl>) -> Result<Self, IndexError> {
        self.search_flags = FlagSet::new(declarations)?;
        Ok(self)
    }

    /// Returns the field name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the field tokenizer identity.
    #[must_use]
    pub const fn tokenizer(&self) -> TokenizerIdentity {
        self.tokenizer
    }

    /// Returns true when the field is indexed.
    #[must_use]
    pub const fn indexed(&self) -> bool {
        self.indexed
    }

    /// Returns true when the field is searchable.
    #[must_use]
    pub const fn searchable(&self) -> bool {
        self.searchable
    }

    /// Returns true when the field is filterable.
    #[must_use]
    pub const fn filterable(&self) -> bool {
        self.filterable
    }

    /// Returns true when the field stores token positions.
    #[must_use]
    pub const fn positions(&self) -> bool {
        self.positions
    }

    /// Returns the filter flag declarations.
    #[must_use]
    pub const fn filter_flags(&self) -> &FlagSet {
        &self.filter_flags
    }

    /// Returns the search flag declarations.
    #[must_use]
    pub const fn search_flags(&self) -> &FlagSet {
        &self.search_flags
    }

    /// Checks the field declaration.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::InvalidManifest`] when the name is empty or a
    /// capability contradicts another declaration.
    pub fn validate(&self) -> Result<(), IndexError> {
        if self.name.is_empty() {
            return Err(IndexError::InvalidManifest {
                reason: "a field name is empty".to_owned(),
            });
        }
        self.tokenizer.validate()?;
        if !self.indexed && (self.searchable || self.filterable) {
            return Err(IndexError::InvalidManifest {
                reason: format!(
                    "the field {} is searchable or filterable but not indexed",
                    self.name
                ),
            });
        }
        if self.positions && !self.indexed {
            return Err(IndexError::InvalidManifest {
                reason: format!(
                    "the field {} stores positions but is not indexed",
                    self.name
                ),
            });
        }
        if !self.filterable && !self.filter_flags.is_empty() {
            return Err(IndexError::InvalidManifest {
                reason: format!(
                    "the field {} declares filter flags but is not filterable",
                    self.name
                ),
            });
        }
        if !self.searchable && !self.search_flags.is_empty() {
            return Err(IndexError::InvalidManifest {
                reason: format!(
                    "the field {} declares search flags but is not searchable",
                    self.name
                ),
            });
        }
        Ok(())
    }

    fn encode(&self, output: &mut Vec<u8>) {
        canonical::text(output, &self.name);
        self.tokenizer.encode(output);
        canonical::bool_byte(output, self.indexed);
        canonical::bool_byte(output, self.searchable);
        canonical::bool_byte(output, self.filterable);
        canonical::bool_byte(output, self.positions);
        encode_flags(output, &self.filter_flags);
        encode_flags(output, &self.search_flags);
    }
}

fn encode_flags(output: &mut Vec<u8>, flags: &FlagSet) {
    let declarations = flags.declarations();
    canonical::u32(
        output,
        u32::try_from(declarations.len()).unwrap_or(u32::MAX),
    );
    for declaration in declarations {
        canonical::text(output, declaration.name());
        canonical::u8(output, declaration.bit());
    }
}

/// The index declaration in the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexManifest {
    fields: Vec<FieldSpec>,
    default_tokenizer: TokenizerIdentity,
    scorer: ScorerSpec,
    overhead_budget: u64,
    token_budget: TokenBudget,
}

impl IndexManifest {
    /// Creates an index manifest.
    ///
    /// The constructor sorts the fields by name and checks the declaration.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::InvalidManifest`] when the field list is empty,
    /// two fields share a name, or a field declaration is invalid.
    pub fn new(
        fields: Vec<FieldSpec>,
        default_tokenizer: TokenizerIdentity,
        scorer: ScorerSpec,
        overhead_budget: u64,
        token_budget: TokenBudget,
    ) -> Result<Self, IndexError> {
        if fields.is_empty() {
            return Err(IndexError::InvalidManifest {
                reason: "the manifest has no field".to_owned(),
            });
        }
        let mut fields = fields;
        fields.sort_by(|left, right| left.name.cmp(&right.name));
        let mut previous: Option<&str> = None;
        let mut indexed = false;
        for field in &fields {
            if previous == Some(field.name()) {
                return Err(IndexError::DuplicateName {
                    name: field.name().to_owned(),
                });
            }
            field.validate()?;
            indexed |= field.indexed;
            previous = Some(field.name());
        }
        if !indexed {
            return Err(IndexError::InvalidManifest {
                reason: "the manifest has no indexed field".to_owned(),
            });
        }
        default_tokenizer.validate()?;
        scorer.validate()?;
        Ok(Self {
            fields,
            default_tokenizer,
            scorer,
            overhead_budget,
            token_budget,
        })
    }

    /// Returns the fields in canonical name order.
    #[must_use]
    pub fn fields(&self) -> &[FieldSpec] {
        &self.fields
    }

    /// Returns one field declaration.
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&FieldSpec> {
        self.fields
            .binary_search_by(|field| field.name().cmp(name))
            .ok()
            .map(|index| &self.fields[index])
    }

    /// Returns the default tokenizer identity.
    #[must_use]
    pub const fn default_tokenizer(&self) -> TokenizerIdentity {
        self.default_tokenizer
    }

    /// Returns the scorer declaration.
    #[must_use]
    pub const fn scorer(&self) -> ScorerSpec {
        self.scorer
    }

    /// Returns the overhead budget in bytes.
    #[must_use]
    pub const fn overhead_budget(&self) -> u64 {
        self.overhead_budget
    }

    /// Returns the token budget.
    #[must_use]
    pub const fn token_budget(&self) -> TokenBudget {
        self.token_budget
    }

    /// Returns a deterministic fingerprint of every manifest field.
    #[must_use]
    pub fn fingerprint(&self) -> Hash32 {
        let mut bytes = Vec::new();
        canonical::text(&mut bytes, "oxkv/index-manifest/v1");
        canonical::u32(
            &mut bytes,
            u32::try_from(self.fields.len()).unwrap_or(u32::MAX),
        );
        for field in &self.fields {
            field.encode(&mut bytes);
        }
        self.default_tokenizer.encode(&mut bytes);
        self.scorer.encode(&mut bytes);
        canonical::u64(&mut bytes, self.overhead_budget);
        canonical::u32(&mut bytes, self.token_budget.max_tokens_per_field);
        canonical::u32(&mut bytes, self.token_budget.max_token_bytes);
        Hash32::of(&bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokenizer::TokenizerId;

    fn field(name: &str) -> FieldSpec {
        FieldSpec::new(name, TokenizerIdentity::v1(TokenizerId::UnicodeWord))
    }

    fn manifest(fields: Vec<FieldSpec>, scorer: ScorerSpec) -> IndexManifest {
        IndexManifest::new(
            fields,
            TokenizerIdentity::v1(TokenizerId::UnicodeWord),
            scorer,
            u64::MAX,
            TokenBudget::DEFAULT,
        )
        .expect("the manifest is valid")
    }

    #[test]
    fn the_manifest_sorts_fields_and_finds_them() {
        let manifest = manifest(
            vec![field("title"), field("body")],
            ScorerSpec::new(1200, 750, 8).expect("the scorer is valid"),
        );
        let names: Vec<&str> = manifest.fields().iter().map(FieldSpec::name).collect();
        assert_eq!(names, ["body", "title"]);
        assert!(manifest.field("title").is_some());
        assert!(manifest.field("missing").is_none());
    }

    #[test]
    fn a_duplicate_field_name_fails() {
        let error = IndexManifest::new(
            vec![field("title"), field("title")],
            TokenizerIdentity::unicode_word(),
            ScorerSpec::new(1200, 750, 8).expect("the scorer is valid"),
            u64::MAX,
            TokenBudget::DEFAULT,
        )
        .expect_err("the duplicate is rejected");
        assert_eq!(error.code(), "manifest.duplicate_name");
    }

    #[test]
    fn the_fingerprint_covers_the_scorer_and_fields() {
        let first = manifest(
            vec![field("title")],
            ScorerSpec::new(1200, 750, 8).expect("the scorer is valid"),
        );
        let second = manifest(
            vec![field("title")],
            ScorerSpec::new(1200, 750, 9).expect("the scorer is valid"),
        );
        let third = manifest(
            vec![field("body")],
            ScorerSpec::new(1200, 750, 8).expect("the scorer is valid"),
        );
        assert_ne!(first.fingerprint(), second.fingerprint());
        assert_ne!(first.fingerprint(), third.fingerprint());
        assert_eq!(
            first.fingerprint(),
            manifest(
                vec![field("title")],
                ScorerSpec::new(1200, 750, 8).expect("the scorer is valid")
            )
            .fingerprint()
        );
    }

    #[test]
    fn a_flag_contradiction_fails() {
        let flag = FlagDecl::new("published", 0).expect("the flag is valid");
        let field = field("title")
            .with_filter_flags(vec![flag])
            .expect("the flags are valid");
        assert_eq!(
            field.validate(),
            Err(IndexError::InvalidManifest {
                reason: "the field title declares filter flags but is not filterable".to_owned()
            })
        );
    }
}
