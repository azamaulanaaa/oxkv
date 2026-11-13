//! Static checks for the semantic path.
//!
//! The check reads the crate source. It fails when a semantic module reads a
//! clock, entropy, environment, or locale directly. It fails when a platform
//! conditional appears on a semantic path. It fails on floating point and
//! `unsafe`.
//!
//! The test uses `std::fs` only inside this test target. The production build
//! never reads a file.

use std::fs;
use std::path::Path;

/// The semantic modules that must stay portable.
const SEMANTIC_MODULES: &[&str] = &[
    "artifact.rs",
    "core.rs",
    "encode.rs",
    "parity.rs",
    "verdict.rs",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rule {
    Clock,
    Entropy,
    Environment,
    Locale,
    Platform,
    Io,
    Thread,
    FloatingPoint,
    Unsafe,
}

impl Rule {
    const fn name(self) -> &'static str {
        match self {
            Self::Clock => "clock",
            Self::Entropy => "entropy",
            Self::Environment => "environment",
            Self::Locale => "locale",
            Self::Platform => "platform conditional",
            Self::Io => "I/O",
            Self::Thread => "thread",
            Self::FloatingPoint => "floating point",
            Self::Unsafe => "unsafe",
        }
    }
}

const FORBIDDEN: &[(&str, Rule)] = &[
    ("SystemTime", Rule::Clock),
    ("Instant::now", Rule::Clock),
    ("std::time", Rule::Clock),
    ("web_time", Rule::Clock),
    ("UNIX_EPOCH", Rule::Clock),
    ("getrandom", Rule::Entropy),
    ("rand::", Rule::Entropy),
    ("OsRng", Rule::Entropy),
    ("thread_rng", Rule::Entropy),
    ("SystemRandom", Rule::Entropy),
    ("std::env", Rule::Environment),
    ("env::var", Rule::Environment),
    ("std::fs", Rule::Environment),
    ("std::net", Rule::Environment),
    ("std::process", Rule::Environment),
    ("current_dir", Rule::Environment),
    ("home_dir", Rule::Environment),
    ("locale::", Rule::Locale),
    ("Locale", Rule::Locale),
    ("LC_ALL", Rule::Locale),
    ("LANG", Rule::Locale),
    ("setlocale", Rule::Locale),
    ("cfg(target_", Rule::Platform),
    ("cfg(not(target_", Rule::Platform),
    ("cfg!(target_", Rule::Platform),
    ("cfg(any(target_", Rule::Platform),
    ("cfg(all(target_", Rule::Platform),
    ("target_arch", Rule::Platform),
    ("target_os", Rule::Platform),
    ("target_family", Rule::Platform),
    ("target_endian", Rule::Platform),
    ("target_pointer_width", Rule::Platform),
    ("target_vendor", Rule::Platform),
    ("target_env", Rule::Platform),
    ("std::io", Rule::Io),
    ("File::open", Rule::Io),
    ("File::create", Rule::Io),
    ("std::thread", Rule::Thread),
    ("thread::spawn", Rule::Thread),
    ("f32", Rule::FloatingPoint),
    ("f64", Rule::FloatingPoint),
    ("unsafe", Rule::Unsafe),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Finding {
    line: usize,
    rule: Rule,
    pattern: &'static str,
}

fn check_source(source: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (index, line) in source.lines().enumerate() {
        for (pattern, rule) in FORBIDDEN {
            if line.contains(pattern) {
                findings.push(Finding {
                    line: index.saturating_add(1),
                    rule: *rule,
                    pattern,
                });
            }
        }
    }
    findings
}

fn assert_clean(module: &str, source: &str) {
    let findings = check_source(source);
    assert!(
        findings.is_empty(),
        "{module} has a forbidden semantic dependency: {findings:?}"
    );
}

#[test]
fn every_semantic_module_stays_portable() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for module in SEMANTIC_MODULES {
        let path = root.join(module);
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
        assert_clean(module, &source);
    }
}

#[test]
fn a_clock_read_fails_the_static_check() {
    let findings = check_source("fn f() { let _ = std::time::SystemTime::now(); }");
    assert_eq!(findings[0].rule, Rule::Clock);
}

#[test]
fn an_entropy_read_fails_the_static_check() {
    let findings = check_source("fn f() { getrandom::getrandom(&mut bytes); }");
    assert!(findings.iter().any(|finding| finding.rule == Rule::Entropy));
}

#[test]
fn an_environment_read_fails_the_static_check() {
    let findings = check_source("fn f() { let _ = std::env::var(\"HOME\"); }");
    assert!(
        findings
            .iter()
            .any(|finding| finding.rule == Rule::Environment)
    );
}

#[test]
fn a_locale_read_fails_the_static_check() {
    let findings = check_source("fn f() { locale::set_default(\"C\"); }");
    assert!(findings.iter().any(|finding| finding.rule == Rule::Locale));
}

#[test]
fn a_platform_condition_on_a_semantic_path_fails_the_static_check() {
    let findings = check_source("#[cfg(target_os = \"linux\")] fn f() {}");
    assert!(
        findings
            .iter()
            .any(|finding| finding.rule == Rule::Platform)
    );
}

#[test]
fn a_floating_point_field_fails_the_static_check() {
    let findings = check_source("fn f() { let _: f64 = 0.0; }");
    assert!(
        findings
            .iter()
            .any(|finding| finding.rule == Rule::FloatingPoint)
    );
}

#[test]
fn an_unsafe_block_fails_the_static_check() {
    let findings = check_source("fn f() { unsafe { } }");
    assert!(findings.iter().any(|finding| finding.rule == Rule::Unsafe));
}

#[test]
fn the_adapter_path_is_outside_the_semantic_check() {
    assert!(!SEMANTIC_MODULES.contains(&"adapter.rs"));
    let findings = check_source("fn f(clock: &dyn Clock) { let _ = clock.now_millis(); }");
    assert_eq!(findings, Vec::<Finding>::new());
}

#[test]
fn every_rule_has_a_name() {
    for rule in [
        Rule::Clock,
        Rule::Entropy,
        Rule::Environment,
        Rule::Locale,
        Rule::Platform,
        Rule::Io,
        Rule::Thread,
        Rule::FloatingPoint,
        Rule::Unsafe,
    ] {
        assert_ne!(rule.name(), "");
    }
}
