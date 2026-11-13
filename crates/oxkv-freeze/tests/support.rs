//! Support policy, format removal, and downgrade golden vectors.

use oxkv_contracts::version::SnapshotPageVersion;
use oxkv_freeze::{
    FIXES_AFTER_NEXT_MAJOR_MONTHS, FormatGuard, REMOVAL_NOTICE_MONTHS, RemovalPlan,
    SECURITY_FIXES_LAST_MINOR_MONTHS, SUPPORTED_MINORS, SupportError, SupportPolicy,
};
use serde_json::Value;

fn golden_support() -> Value {
    serde_json::from_str(include_str!("vectors/support.json")).expect("the vector file is JSON")
}

fn parse_format(text: &str) -> SnapshotPageVersion {
    text.parse()
        .unwrap_or_else(|error| panic!("the format version {text} is invalid: {error}"))
}

fn removal_error_name(error: SupportError) -> &'static str {
    match error {
        SupportError::InvalidPolicy { .. } => "invalid_policy",
        SupportError::RemovalNotAtMajor => "removal_not_at_major",
        SupportError::NoticeTooShort { .. } => "notice_too_short",
        SupportError::MigrationRequired => "migration_required",
        SupportError::DowngradeNotAllowed { .. } => "downgrade_not_allowed",
    }
}

#[test]
fn the_support_window_is_published() {
    let policy = SupportPolicy::published();
    assert_eq!(policy.fixes_after_next_major_months, 24);
    assert_eq!(policy.security_fixes_last_minor_months, 12);
    assert_eq!(policy.supported_minors, 2);
    assert_eq!(policy.removal_notice_months, 12);
    assert!(policy.requires_automated_migration);
    assert!(!policy.allows_downgrade);
    assert_eq!(SupportPolicy::PUBLISHED_IN, "docs/specs/freeze.md");
    policy.validate().expect("the published policy is valid");

    let document = include_str!("../../../docs/specs/freeze.md");
    for phrase in [
        "24 months",
        "12 months",
        "Two minors",
        "ops.downgrade_not_allowed",
        "12 months of notice",
        "automated migration",
    ] {
        assert!(
            document.contains(phrase),
            "the published policy omits {phrase}"
        );
    }
}

#[test]
fn the_support_constants_match_the_policy() {
    assert_eq!(FIXES_AFTER_NEXT_MAJOR_MONTHS, 24);
    assert_eq!(SECURITY_FIXES_LAST_MINOR_MONTHS, 12);
    assert_eq!(SUPPORTED_MINORS, 2);
    assert_eq!(REMOVAL_NOTICE_MONTHS, 12);
}

#[test]
fn a_downgrade_after_the_first_new_format_write_is_refused() {
    let mut guard = FormatGuard::new();
    assert_eq!(guard.check_open(SnapshotPageVersion::new(1, 0)), Ok(()));
    guard.record_write(SnapshotPageVersion::new(1, 1));
    let error = guard
        .check_open(SnapshotPageVersion::new(1, 0))
        .expect_err("the older format cannot read the newer write");
    assert_eq!(error.code(), Some("ops.downgrade_not_allowed"));
    assert_eq!(SupportError::DOWNGRADE_CODE, "ops.downgrade_not_allowed");
    assert!(oxkv_contracts::error::lookup(error.code().expect("the error has a code")).is_some());
}

#[test]
fn golden_policy_is_pinned() {
    let file = golden_support();
    assert_eq!(file["contract"], "freeze-support/v1");
    let expected = &file["policy"];
    let policy = SupportPolicy {
        fixes_after_next_major_months: u16::try_from(
            expected["fixes_after_next_major_months"]
                .as_u64()
                .expect("the policy has a fix window"),
        )
        .expect("the fix window fits u16"),
        security_fixes_last_minor_months: u16::try_from(
            expected["security_fixes_last_minor_months"]
                .as_u64()
                .expect("the policy has a security window"),
        )
        .expect("the security window fits u16"),
        supported_minors: u16::try_from(
            expected["supported_minors"]
                .as_u64()
                .expect("the policy has a minor count"),
        )
        .expect("the minor count fits u16"),
        removal_notice_months: u16::try_from(
            expected["removal_notice_months"]
                .as_u64()
                .expect("the policy has a notice window"),
        )
        .expect("the notice window fits u16"),
        requires_automated_migration: expected["requires_automated_migration"]
            .as_bool()
            .expect("the policy has a migration flag"),
        allows_downgrade: expected["allows_downgrade"]
            .as_bool()
            .expect("the policy has a downgrade flag"),
    };
    assert_eq!(policy, SupportPolicy::published());
    assert_eq!(
        expected["published_in"]
            .as_str()
            .expect("the policy has a path"),
        SupportPolicy::PUBLISHED_IN
    );
}

#[test]
fn golden_removal_cases_are_pinned() {
    let file = golden_support();
    let policy = SupportPolicy::published();
    let cases = file["removal_cases"]
        .as_array()
        .expect("the vector has removal cases");
    for case in cases {
        let plan = RemovalPlan {
            at_major: case["at_major"]
                .as_bool()
                .expect("the case has a major flag"),
            notice_months: u16::try_from(
                case["notice_months"]
                    .as_u64()
                    .expect("the case has a notice window"),
            )
            .expect("the notice window fits u16"),
            automated_migration: case["automated_migration"]
                .as_bool()
                .expect("the case has a migration flag"),
        };
        let accepted = case["accepted"]
            .as_bool()
            .expect("the case has an acceptance flag");
        match policy.can_remove_format(plan) {
            Ok(()) => assert!(accepted, "{case:?}"),
            Err(error) => {
                assert!(!accepted, "{case:?}");
                assert_eq!(
                    removal_error_name(error),
                    case["error"].as_str().expect("the case has an error"),
                    "{case:?}"
                );
            }
        }
    }
}

#[test]
fn golden_downgrade_cases_are_pinned() {
    let file = golden_support();
    let cases = file["downgrade_cases"]
        .as_array()
        .expect("the vector has downgrade cases");
    for case in cases {
        let mut guard = FormatGuard::new();
        if let Some(written) = case["written"].as_str() {
            guard.record_write(parse_format(written));
        }
        let target = parse_format(case["target"].as_str().expect("the case has a target"));
        let accepted = case["accepted"]
            .as_bool()
            .expect("the case has an acceptance flag");
        match guard.check_open(target) {
            Ok(()) => assert!(accepted, "{case:?}"),
            Err(error) => {
                assert!(!accepted, "{case:?}");
                assert_eq!(
                    error.code(),
                    Some(
                        case["error_code"]
                            .as_str()
                            .expect("the case has an error code")
                    ),
                    "{case:?}"
                );
            }
        }
    }
}
