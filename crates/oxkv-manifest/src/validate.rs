//! Structural manifest validation.
//!
//! Validation checks the schema, the declarations, and the resolution rules.
//! It does not read the shard map and it does not check placement.

use std::collections::BTreeSet;

use oxkv_contracts::error::lookup as lookup_error;
use oxkv_contracts::keyspace::{KeyspaceError, validate_user_table_name};

use crate::error::ManifestError;
use crate::schema::{DIGEST_ALGORITHM, Manifest, RULE_COST_BUDGET, SCHEMA_VERSION};

impl Manifest {
    /// Validates the manifest schema and the declaration graph.
    ///
    /// The method does not require a digest. Use [`Manifest::verify_digest`]
    /// for the digest check.
    ///
    /// # Errors
    ///
    /// Returns one named [`ManifestError`] for the first invalid declaration.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.schema.version != SCHEMA_VERSION {
            return Err(ManifestError::UnsupportedSchema {
                schema: self.schema.version.clone(),
            });
        }
        if self.schema.revision < 0 {
            return Err(invalid("manifest.schema.revision", &self.schema.revision));
        }

        unique_names(self.tables.iter().map(|table| table.name.as_str()), "table")?;
        unique_names(
            self.indexes.iter().map(|index| index.name.as_str()),
            "index",
        )?;
        unique_names(
            self.rules
                .declarations
                .iter()
                .map(|rule| rule.name.as_str()),
            "rule",
        )?;
        unique_names(self.facts.iter().map(|fact| fact.name.as_str()), "fact")?;
        unique_names(
            self.operations
                .iter()
                .map(|operation| operation.name.as_str()),
            "operation",
        )?;
        unique_names(self.events.iter().map(|event| event.kind.as_str()), "event")?;
        unique_names(
            self.control.iter().map(|operation| operation.name.as_str()),
            "control",
        )?;

        for (index, table) in self.tables.iter().enumerate() {
            Self::validate_table(table, index)?;
        }
        for (index, index_decl) in self.indexes.iter().enumerate() {
            self.validate_index(index_decl, index)?;
        }
        self.validate_rules()?;
        self.validate_facts()?;
        for (index, operation) in self.operations.iter().enumerate() {
            self.validate_operation(operation, index)?;
        }
        for (index, event) in self.events.iter().enumerate() {
            self.validate_event(event, index)?;
        }
        for code in &self.receipts.reason_codes {
            validate_error_code("manifest.receipts.reason_codes", code)?;
        }
        if self.receipts.retry_classes.is_empty() {
            return Err(invalid(
                "manifest.receipts.retry_classes",
                "at least one retry class is required",
            ));
        }
        if self.codecs.digest_algorithm != DIGEST_ALGORITHM {
            return Err(ManifestError::InvalidValue {
                path: "manifest.codecs.digest_algorithm".to_owned(),
                value: self.codecs.digest_algorithm.clone(),
            });
        }
        if self.projection.epoch_rule.is_empty() {
            return Err(invalid("manifest.projection.epoch_rule", "empty"));
        }
        if self.projection.watermark_rule.is_empty() {
            return Err(invalid("manifest.projection.watermark_rule", "empty"));
        }
        for operation in &self.control {
            if !operation.name.starts_with("control.") {
                return Err(ManifestError::InvalidValue {
                    path: "manifest.control[].name".to_owned(),
                    value: operation.name.clone(),
                });
            }
            validate_dotted_identifier("manifest.control[].name", &operation.name)?;
            for code in &operation.errors {
                validate_error_code("manifest.control[].errors", code)?;
            }
        }
        Ok(())
    }

    /// Validates the manifest and checks its digest.
    ///
    /// # Errors
    ///
    /// Returns the first structural [`ManifestError`], or
    /// [`ManifestError::ArtifactDrift`] when the digest is absent or wrong.
    pub fn validate_sealed(&self) -> Result<(), ManifestError> {
        self.validate()?;
        self.verify_digest()
    }

    /// Returns one table by name.
    #[must_use]
    pub fn table(&self, name: &str) -> Option<&crate::schema::Table> {
        self.tables.iter().find(|table| table.name == name)
    }

    /// Validates one table declaration.
    fn validate_table(table: &crate::schema::Table, index: usize) -> Result<(), ManifestError> {
        let path = format!("manifest.tables[{index}].name");
        validate_user_name(&path, &table.name)?;
        if table.key_schema.is_empty() {
            return Err(ManifestError::KeySchemaMissing {
                table: table.name.clone(),
            });
        }
        unique_names(
            table
                .key_schema
                .iter()
                .map(|component| component.name.as_str()),
            "key field",
        )?;
        unique_names(
            table.fields.iter().map(|field| field.name.as_str()),
            "field",
        )?;
        let mut names = BTreeSet::new();
        for component in &table.key_schema {
            validate_identifier(&format!("{path}.key_schema"), &component.name)?;
            if !names.insert(component.name.as_str()) {
                return Err(ManifestError::DuplicateName {
                    kind: "field",
                    name: component.name.clone(),
                });
            }
        }
        for field in &table.fields {
            validate_identifier(&format!("{path}.fields"), &field.name)?;
            if !names.insert(field.name.as_str()) {
                return Err(ManifestError::DuplicateName {
                    kind: "field",
                    name: field.name.clone(),
                });
            }
        }
        if table.limits.max_key_bytes <= 0 {
            return Err(invalid(
                "manifest.tables[].limits.max_key_bytes",
                "not positive",
            ));
        }
        if table.limits.max_value_bytes <= 0 {
            return Err(invalid(
                "manifest.tables[].limits.max_value_bytes",
                "not positive",
            ));
        }
        Ok(())
    }

    /// Validates one index declaration.
    fn validate_index(
        &self,
        index: &crate::schema::Index,
        position: usize,
    ) -> Result<(), ManifestError> {
        let path = format!("manifest.indexes[{position}].name");
        validate_identifier(&path, &index.name)?;
        let Some(table) = self.table(&index.table) else {
            return Err(ManifestError::IndexUnknownField {
                index: index.name.clone(),
                field: index.table.clone(),
            });
        };
        if index.fields.is_empty() {
            return Err(ManifestError::IndexUnknownField {
                index: index.name.clone(),
                field: "<empty>".to_owned(),
            });
        }
        for field in &index.fields {
            if !table.has_field(field) {
                return Err(ManifestError::IndexUnknownField {
                    index: index.name.clone(),
                    field: field.clone(),
                });
            }
        }
        if !index.filter && !index.search {
            return Err(ManifestError::IndexScopeUnreachable {
                index: index.name.clone(),
            });
        }
        if index.overhead_budget < 0 {
            return Err(invalid(
                "manifest.indexes[].overhead_budget",
                &index.overhead_budget,
            ));
        }
        Ok(())
    }

    /// Validates the rule section.
    fn validate_rules(&self) -> Result<(), ManifestError> {
        if !self.rules.declarations.is_empty() && self.rules.catalog.is_none() {
            return Err(ManifestError::PolicyCatalogMissing);
        }
        for rule in &self.rules.declarations {
            validate_identifier("manifest.rules.declarations[].name", &rule.name)?;
            let Some((table_name, field_name)) = split_path(&rule.selector) else {
                return Err(ManifestError::RuleUnknownPath {
                    rule: rule.name.clone(),
                    path: rule.selector.clone(),
                });
            };
            let Some(table) = self.table(table_name) else {
                return Err(ManifestError::RuleUnknownPath {
                    rule: rule.name.clone(),
                    path: rule.selector.clone(),
                });
            };
            if !table.has_field(field_name) {
                return Err(ManifestError::RuleUnknownPath {
                    rule: rule.name.clone(),
                    path: rule.selector.clone(),
                });
            }
            for fact in &rule.facts {
                if !self.facts.iter().any(|declared| declared.name == *fact) {
                    return Err(ManifestError::RuleUnknownPath {
                        rule: rule.name.clone(),
                        path: fact.clone(),
                    });
                }
            }
            if !(0..=RULE_COST_BUDGET).contains(&rule.cost_bound) {
                return Err(ManifestError::RuleUnbounded {
                    rule: rule.name.clone(),
                    cost: rule.cost_bound,
                    budget: RULE_COST_BUDGET,
                });
            }
        }
        Ok(())
    }

    /// Validates the fact declarations.
    fn validate_facts(&self) -> Result<(), ManifestError> {
        for fact in &self.facts {
            validate_identifier("manifest.facts[].name", &fact.name)?;
            if fact.owner.is_empty() {
                return Err(invalid("manifest.facts[].owner", "empty"));
            }
            if fact.fan_in_cap <= 0 {
                return Err(invalid("manifest.facts[].fan_in_cap", &fact.fan_in_cap));
            }
            if fact.staleness_bound < 0 {
                return Err(invalid(
                    "manifest.facts[].staleness_bound",
                    &fact.staleness_bound,
                ));
            }
        }
        Ok(())
    }

    /// Validates one operation.
    fn validate_operation(
        &self,
        operation: &crate::schema::Operation,
        position: usize,
    ) -> Result<(), ManifestError> {
        validate_dotted_identifier(
            &format!("manifest.operations[{position}].name"),
            &operation.name,
        )?;
        if self.table(&operation.target).is_none() {
            return Err(ManifestError::OperationUnknownField {
                operation: operation.name.clone(),
                field: operation.target.clone(),
            });
        }
        for field in &operation.request {
            self.check_operation_field(operation, field)?;
        }
        for field in &operation.response {
            self.check_operation_field(operation, field)?;
        }
        for code in &operation.errors {
            validate_error_code("manifest.operations[].errors", code)?;
        }
        Ok(())
    }

    /// Checks one operation field path.
    fn check_operation_field(
        &self,
        operation: &crate::schema::Operation,
        field: &str,
    ) -> Result<(), ManifestError> {
        let Some((table_name, field_name)) = split_path(field) else {
            return Err(ManifestError::OperationUnknownField {
                operation: operation.name.clone(),
                field: field.to_owned(),
            });
        };
        let Some(table) = self.table(table_name) else {
            return Err(ManifestError::OperationUnknownField {
                operation: operation.name.clone(),
                field: field.to_owned(),
            });
        };
        if table_name != operation.target {
            return Err(ManifestError::OperationCrossShard {
                operation: operation.name.clone(),
                field: field.to_owned(),
            });
        }
        if !table.has_field(field_name) {
            return Err(ManifestError::OperationUnknownField {
                operation: operation.name.clone(),
                field: field.to_owned(),
            });
        }
        Ok(())
    }

    /// Validates one event.
    fn validate_event(
        &self,
        event: &crate::schema::Event,
        position: usize,
    ) -> Result<(), ManifestError> {
        validate_identifier(&format!("manifest.events[{position}].kind"), &event.kind)?;
        let Some(table) = self.table(&event.table) else {
            return Err(invalid(
                &format!("manifest.events[{position}].table"),
                &event.table,
            ));
        };
        for field in &event.payload {
            let Some((table_name, field_name)) = split_path(field) else {
                return Err(invalid(
                    &format!("manifest.events[{position}].payload"),
                    field,
                ));
            };
            if table_name != event.table || !table.has_field(field_name) {
                return Err(invalid(
                    &format!("manifest.events[{position}].payload"),
                    field,
                ));
            }
        }
        for field in &event.required {
            let Some((table_name, field_name)) = split_path(field) else {
                return Err(invalid(
                    &format!("manifest.events[{position}].required"),
                    field,
                ));
            };
            if table_name != event.table || !table.has_field(field_name) {
                return Err(invalid(
                    &format!("manifest.events[{position}].required"),
                    field,
                ));
            }
        }
        Ok(())
    }
}

/// Returns an invalid value error.
fn invalid(path: &str, value: &(impl ToString + ?Sized)) -> ManifestError {
    ManifestError::InvalidValue {
        path: path.to_owned(),
        value: value.to_string(),
    }
}

/// Checks one name for duplicates.
fn unique_names<'a>(
    names: impl Iterator<Item = &'a str>,
    kind: &'static str,
) -> Result<(), ManifestError> {
    let mut seen = BTreeSet::new();
    for name in names {
        if !seen.insert(name) {
            return Err(ManifestError::DuplicateName {
                kind,
                name: name.to_owned(),
            });
        }
    }
    Ok(())
}

/// Validates one user table name.
fn validate_user_name(path: &str, name: &str) -> Result<(), ManifestError> {
    match validate_user_table_name(name) {
        Ok(()) => Ok(()),
        Err(KeyspaceError::ReservedTableName { .. }) => Err(ManifestError::ReservedTableName {
            name: name.to_owned(),
        }),
        Err(_) => Err(ManifestError::InvalidValue {
            path: path.to_owned(),
            value: name.to_owned(),
        }),
    }
}

/// Validates one identifier.
fn validate_identifier(path: &str, name: &str) -> Result<(), ManifestError> {
    let bytes = name.as_bytes();
    let valid = !bytes.is_empty()
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .skip(1)
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_');
    if valid {
        Ok(())
    } else {
        Err(ManifestError::InvalidValue {
            path: path.to_owned(),
            value: name.to_owned(),
        })
    }
}

/// Validates one dotted identifier.
fn validate_dotted_identifier(path: &str, name: &str) -> Result<(), ManifestError> {
    if name.is_empty() {
        return Err(ManifestError::InvalidValue {
            path: path.to_owned(),
            value: name.to_owned(),
        });
    }
    for segment in name.split('.') {
        validate_identifier(path, segment)?;
    }
    Ok(())
}

/// Splits one `table.field` path.
fn split_path(path: &str) -> Option<(&str, &str)> {
    let (table, field) = path.split_once('.')?;
    if table.is_empty() || field.is_empty() || field.contains('.') {
        return None;
    }
    Some((table, field))
}

/// Validates one error code against the registry.
fn validate_error_code(path: &str, code: &str) -> Result<(), ManifestError> {
    if lookup_error(code).is_some() {
        Ok(())
    } else {
        Err(ManifestError::InvalidValue {
            path: path.to_owned(),
            value: code.to_owned(),
        })
    }
}
