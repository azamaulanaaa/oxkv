//! The offline one-command runner.
//!
//! Run `cargo run -p oxkv-qualify -- --tier pull_request` to print the report
//! and the narration. The command starts no service and uses no network.

use std::process::ExitCode;

use oxkv_qualify::suite::Suite;
use oxkv_qualify::tiers::policy;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        print_help();
        return ExitCode::SUCCESS;
    }
    let tier = tier_from(&arguments);
    let Some(policy) = policy(&tier) else {
        eprintln!("The tier {tier} is not in the policy.");
        return ExitCode::FAILURE;
    };
    if !policy.native_vectors {
        eprintln!("The tier {tier} does not run native vectors.");
        return ExitCode::FAILURE;
    }

    let outcome = Suite::new().run();
    println!("Tier: {}", policy.name);
    println!("Browser targets: {}", policy.browser_targets.name());
    println!();
    print!("{}", outcome.report.render());
    println!();
    println!("{}", outcome.narration.text());
    println!("Fuzz probes: {}", outcome.fuzz.cases);
    println!("Fuzz digest: {}", outcome.fuzz.digest);

    if outcome.passed() {
        ExitCode::SUCCESS
    } else {
        eprintln!("The qualification suite failed.");
        ExitCode::FAILURE
    }
}

fn tier_from(arguments: &[String]) -> String {
    let mut iterator = arguments.iter();
    while let Some(argument) = iterator.next() {
        if argument == "--tier"
            && let Some(value) = iterator.next()
        {
            return value.clone();
        }
        if let Some(value) = argument.strip_prefix("--tier=") {
            return value.to_owned();
        }
    }
    "pull_request".to_owned()
}

fn print_help() {
    println!("Usage: oxkv-qualify [--tier pull_request|nightly|release_candidate]");
    println!();
    println!("This command runs the offline qualification suite.");
    println!("It prints one report and one narration from one normalized trace.");
    println!("It starts no service and uses no network.");
}
