//! Artifact migration for snapshot and page files.
//!
//! An artifact migration rewrites one format version N to N+1. The result
//! lists every layout change and carries one new content digest per object.

use oxkv_contracts::version::{ContentDigest, KeyEncodingVersion, SnapshotPageVersion};

use crate::error::OpsError;

/// The kind of one migratable artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MigratableArtifact {
    /// A shard snapshot file.
    Snapshot,
    /// A snapshot page file.
    Page,
}

impl MigratableArtifact {
    /// Returns the canonical artifact name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::Page => "page",
        }
    }
}

impl core::fmt::Display for MigratableArtifact {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// One layout change in the new format.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LayoutChange {
    /// The field or layout element that changes.
    pub field: String,
    /// The old form.
    pub before: String,
    /// The new form.
    pub after: String,
}

impl LayoutChange {
    /// Creates a layout change.
    #[must_use]
    pub fn new(
        field: impl Into<String>,
        before: impl Into<String>,
        after: impl Into<String>,
    ) -> Self {
        Self {
            field: field.into(),
            before: before.into(),
            after: after.into(),
        }
    }
}

/// One artifact file with a format version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactObject {
    /// The file name in the object store.
    pub name: String,
    /// The artifact kind.
    pub kind: MigratableArtifact,
    /// The current format version.
    pub format: SnapshotPageVersion,
    /// The key encoding annex version.
    pub key_encoding: KeyEncodingVersion,
    /// The immutable file bytes.
    pub bytes: Vec<u8>,
}

impl ArtifactObject {
    /// Creates an artifact object.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        kind: MigratableArtifact,
        format: SnapshotPageVersion,
        key_encoding: KeyEncodingVersion,
        bytes: Vec<u8>,
    ) -> Self {
        Self {
            name: name.into(),
            kind,
            format,
            key_encoding,
            bytes,
        }
    }

    /// Returns the content digest of the file bytes.
    #[must_use]
    pub fn content_digest(&self) -> ContentDigest {
        ContentDigest::of_bytes(&self.bytes)
    }
}

/// One migrated artifact with its old and new digests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigratedArtifact {
    /// The new artifact.
    pub object: ArtifactObject,
    /// The digest before the migration.
    pub old_digest: ContentDigest,
    /// The digest after the migration.
    pub new_digest: ContentDigest,
    /// Every layout change that the migration applied.
    pub changes: Vec<LayoutChange>,
}

/// The result of one artifact migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactMigrationReport {
    /// The source format version.
    pub from: SnapshotPageVersion,
    /// The target format version.
    pub to: SnapshotPageVersion,
    /// Every migrated artifact.
    pub objects: Vec<MigratedArtifact>,
    /// Every layout change in the source to target step.
    pub layout_changes: Vec<LayoutChange>,
}

impl ArtifactMigrationReport {
    /// Returns the new digest for one object name.
    #[must_use]
    pub fn digest_for(&self, name: &str) -> Option<ContentDigest> {
        self.objects
            .iter()
            .find(|migrated| migrated.object.name == name)
            .map(|migrated| migrated.new_digest)
    }
}

/// One artifact migration step from N to N+1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactMigrationPlan {
    /// The source format version.
    pub from: SnapshotPageVersion,
    /// The target format version.
    pub to: SnapshotPageVersion,
    /// Every layout change in the step.
    pub changes: Vec<LayoutChange>,
}

impl ArtifactMigrationPlan {
    /// Creates an artifact migration plan.
    ///
    /// # Errors
    ///
    /// Returns [`OpsError::MigrationIncomplete`] when the target version is not
    /// the next minor of the source version.
    pub fn new(
        from: SnapshotPageVersion,
        to: SnapshotPageVersion,
        changes: Vec<LayoutChange>,
    ) -> Result<Self, OpsError> {
        if from.major() != to.major() || from.minor().saturating_add(1) != to.minor() {
            return Err(OpsError::MigrationIncomplete {
                step: "artifact_plan",
            });
        }
        Ok(Self { from, to, changes })
    }

    /// Migrates one artifact file.
    ///
    /// # Errors
    ///
    /// Returns [`OpsError::UnsupportedFormat`] when the source major is
    /// unknown. Returns [`OpsError::MigrationIncomplete`] when the object is
    /// not at the source version.
    pub fn migrate(&self, object: &ArtifactObject) -> Result<MigratedArtifact, OpsError> {
        if object.format.major() != self.from.major() {
            return Err(OpsError::UnsupportedFormat {
                version: object.format,
            });
        }
        if object.format != self.from {
            return Err(OpsError::MigrationIncomplete {
                step: "artifact_source_version",
            });
        }
        let old_digest = object.content_digest();
        let new_bytes = self.rewrite_bytes(object);
        let new_digest = ContentDigest::of_bytes(&new_bytes);
        let new_object = ArtifactObject {
            name: object.name.clone(),
            kind: object.kind,
            format: self.to,
            key_encoding: object.key_encoding,
            bytes: new_bytes,
        };
        let mut changes = self.changes.clone();
        changes.sort();
        Ok(MigratedArtifact {
            object: new_object,
            old_digest,
            new_digest,
            changes,
        })
    }

    /// Migrates every artifact file.
    ///
    /// # Errors
    ///
    /// Returns the first error from [`Self::migrate`].
    pub fn migrate_all(
        &self,
        objects: &[ArtifactObject],
    ) -> Result<ArtifactMigrationReport, OpsError> {
        let mut migrated = Vec::with_capacity(objects.len());
        for object in objects {
            migrated.push(self.migrate(object)?);
        }
        let mut changes = self.changes.clone();
        changes.sort();
        Ok(ArtifactMigrationReport {
            from: self.from,
            to: self.to,
            objects: migrated,
            layout_changes: changes,
        })
    }

    fn rewrite_bytes(&self, object: &ArtifactObject) -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(b"oxkv/artifact-migration/v1\n");
        output.extend_from_slice(format!("from={}\n", self.from).as_bytes());
        output.extend_from_slice(format!("to={}\n", self.to).as_bytes());
        output.extend_from_slice(
            format!("key_encoding_version={}\n", object.key_encoding.get()).as_bytes(),
        );
        output.extend_from_slice(format!("kind={}\n", object.kind).as_bytes());
        output.extend_from_slice(format!("layout_changes={}\n", self.changes.len()).as_bytes());
        let mut changes = self.changes.clone();
        changes.sort();
        for change in changes {
            output.extend_from_slice(
                format!(
                    "change={}|{}|{}\n",
                    change.field, change.before, change.after
                )
                .as_bytes(),
            );
        }
        output.extend_from_slice(&object.bytes);
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> ArtifactMigrationPlan {
        ArtifactMigrationPlan::new(
            SnapshotPageVersion::new(1, 0),
            SnapshotPageVersion::new(1, 1),
            vec![
                LayoutChange::new("page_sequence", "absent", "u64"),
                LayoutChange::new("provenance", "v1 fields", "v1 fields plus key encoding"),
            ],
        )
        .expect("the plan is valid")
    }

    fn snapshot() -> ArtifactObject {
        ArtifactObject::new(
            "snap-1",
            MigratableArtifact::Snapshot,
            SnapshotPageVersion::new(1, 0),
            KeyEncodingVersion::new(1),
            b"snapshot bytes".to_vec(),
        )
    }

    #[test]
    fn a_migration_lists_every_layout_change_with_a_new_digest() {
        let migrated = plan().migrate(&snapshot()).expect("the migration succeeds");
        assert_eq!(migrated.object.format, SnapshotPageVersion::new(1, 1));
        assert_eq!(migrated.changes.len(), 2);
        assert_ne!(migrated.old_digest, migrated.new_digest);
        assert_eq!(migrated.new_digest, migrated.object.content_digest());
    }

    #[test]
    fn the_same_input_produces_the_same_new_digest() {
        let first = plan().migrate(&snapshot()).expect("the migration succeeds");
        let second = plan().migrate(&snapshot()).expect("the migration succeeds");
        assert_eq!(first.new_digest, second.new_digest);
        assert_eq!(first.object.bytes, second.object.bytes);
    }

    #[test]
    fn a_plan_requires_the_next_minor() {
        let error = ArtifactMigrationPlan::new(
            SnapshotPageVersion::new(1, 0),
            SnapshotPageVersion::new(1, 2),
            Vec::new(),
        )
        .expect_err("the plan is invalid");
        assert_eq!(error.code(), "ops.migration_incomplete");
    }

    #[test]
    fn an_unknown_major_is_refused() {
        let object = ArtifactObject::new(
            "snap-2",
            MigratableArtifact::Page,
            SnapshotPageVersion::new(2, 0),
            KeyEncodingVersion::new(1),
            Vec::new(),
        );
        let error = plan().migrate(&object).expect_err("the major is unknown");
        assert_eq!(error.code(), "format.unsupported_version");
    }

    #[test]
    fn a_report_names_every_object_digest() {
        let report = plan()
            .migrate_all(&[snapshot()])
            .expect("the migration succeeds");
        assert_eq!(report.objects.len(), 1);
        assert_eq!(
            report.digest_for("snap-1"),
            Some(report.objects[0].new_digest)
        );
        assert_eq!(report.digest_for("missing"), None);
    }
}
