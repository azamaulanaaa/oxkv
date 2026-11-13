//! A native authority runner.
//!
//! The binary opens one data directory, loads the saved state when it exists,
//! commits one write, saves the state, and prints the receipt.
//!
//! The binary uses local mode with the structural signer and the file witness
//! outbox. Production mode is unavailable in this tree because there is no
//! real checkpoint signer.
//!
//! The binary needs a native target. On `wasm32`, `main` prints one message
//! and returns a failure code.

use std::process::ExitCode;

#[cfg(not(target_arch = "wasm32"))]
use std::fmt::Write as _;
#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
use oxkv_contracts::receipt::{IdempotencyKey, RequestId};
#[cfg(not(target_arch = "wasm32"))]
use oxkv_contracts::version::{
    KeyVersion, ManifestDigest, MapVersion, PolicyVersion, ProjectionEpoch, ProjectionWatermarks,
    ShardId,
};
#[cfg(not(target_arch = "wasm32"))]
use oxkv_security::audit::{CheckpointSigner, StructuralSigner};
#[cfg(not(target_arch = "wasm32"))]
use oxkv_security::secret::KeyIdentifier;
#[cfg(not(target_arch = "wasm32"))]
use oxkv_write::{
    AllowAll, Authority, AuthorityConfig, AuthorityInitError, AuthorityStateError, ClientId,
    FileWitnessOutbox, Key, Mutation, STATE_FILE, SubmitError, WriteRequest,
};

/// The usage text.
#[cfg(not(target_arch = "wasm32"))]
const USAGE: &str = "\
Usage: oxkv-authority [OPTIONS] <DATA_DIR>

Commit one write and print the receipt.

Arguments:
  <DATA_DIR>              The data directory

Options:
  --base <VERSION>        The base key version (default: 0)
  --key <KEY>             The key text (default: authority-demo)
  --value <VALUE>         The value text (default: one)
  --idempotency <KEY>     The idempotency key (default: authority-demo-1)
  --production            Refuse local mode and exit with an error
  -h, --help              Print this text
";

/// An error from the command line runner.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, thiserror::Error)]
enum CliError {
    /// The caller gave no data directory.
    #[error("Give one data directory. Use --help for usage.")]
    MissingDataDirectory,
    /// An option has no value.
    #[error("The option {0} needs a value.")]
    MissingOptionValue(&'static str),
    /// An argument is unknown.
    #[error("The argument {0} is unknown. Use --help for usage.")]
    UnknownArgument(String),
    /// The base version is not an unsigned integer.
    #[error("The base version is not an unsigned integer: {0}.")]
    InvalidBase(String),
    /// The caller asked for production mode.
    #[error(
        "Production mode requires a real CheckpointSigner. This build has no production signer. Use local mode."
    )]
    ProductionUnsupported,
    /// A file operation failed.
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// The authority state operation failed.
    #[error("{0}")]
    State(#[from] AuthorityStateError),
    /// The authority startup failed.
    #[error("{0}")]
    Init(#[from] AuthorityInitError),
    /// The write submission failed.
    #[error("{0}")]
    Submit(#[from] SubmitError),
}

/// One parsed command.
#[cfg(not(target_arch = "wasm32"))]
enum Command {
    /// Print the usage text.
    Help,
    /// Commit one write.
    Run(Options),
}

/// The options of one run.
#[cfg(not(target_arch = "wasm32"))]
struct Options {
    data_dir: PathBuf,
    base: u64,
    key: String,
    value: String,
    idempotency: String,
    production: bool,
}

#[cfg(not(target_arch = "wasm32"))]
impl Options {
    /// Parses the command line arguments.
    fn parse(arguments: impl Iterator<Item = String>) -> Result<Command, CliError> {
        let mut arguments = arguments;
        let mut data_dir = None;
        let mut base = 0_u64;
        let mut key = String::from("authority-demo");
        let mut value = String::from("one");
        let mut idempotency = String::from("authority-demo-1");
        let mut production = false;

        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "-h" | "--help" => return Ok(Command::Help),
                "--production" => production = true,
                "--base" => {
                    let text = arguments
                        .next()
                        .ok_or(CliError::MissingOptionValue("--base"))?;
                    base = text
                        .parse()
                        .map_err(|_| CliError::InvalidBase(text.clone()))?;
                }
                "--key" => {
                    key = arguments
                        .next()
                        .ok_or(CliError::MissingOptionValue("--key"))?;
                }
                "--value" => {
                    value = arguments
                        .next()
                        .ok_or(CliError::MissingOptionValue("--value"))?;
                }
                "--idempotency" => {
                    idempotency = arguments
                        .next()
                        .ok_or(CliError::MissingOptionValue("--idempotency"))?;
                }
                other if other.starts_with('-') => {
                    return Err(CliError::UnknownArgument(other.to_owned()));
                }
                path => {
                    if data_dir.is_some() {
                        return Err(CliError::UnknownArgument(path.to_owned()));
                    }
                    data_dir = Some(PathBuf::from(path));
                }
            }
        }

        Ok(Command::Run(Self {
            data_dir: data_dir.ok_or(CliError::MissingDataDirectory)?,
            base,
            key,
            value,
            idempotency,
            production,
        }))
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn main() -> ExitCode {
    eprintln!("The oxkv-authority binary needs a native target.");
    ExitCode::FAILURE
}

/// Runs one command.
#[cfg(not(target_arch = "wasm32"))]
fn run() -> Result<(), CliError> {
    let command = Options::parse(std::env::args().skip(1))?;
    let Command::Run(options) = command else {
        println!("{USAGE}");
        return Ok(());
    };
    if options.production {
        return Err(CliError::ProductionUnsupported);
    }

    std::fs::create_dir_all(&options.data_dir)?;
    let witness = FileWitnessOutbox::open(options.data_dir.join("witness"))?;
    let config = config();
    let signer: Arc<dyn CheckpointSigner> =
        Arc::new(StructuralSigner::new(config.checkpoint_key().clone()));
    let mut authority = Authority::open(config, signer, witness)?;
    if options.data_dir.join(STATE_FILE).exists() {
        authority.load_state(&options.data_dir)?;
    }

    let request = WriteRequest::new(
        RequestId::from_bytes([0x42; 32]),
        ClientId::new(b"oxkv-authority".to_vec()),
        IdempotencyKey::new(options.idempotency.into_bytes()),
        [Mutation::put(
            Key::new(options.key.into_bytes()),
            authority.shard_id().clone(),
            KeyVersion::new(options.base),
            options.value.into_bytes(),
        )],
    );
    let outcome = authority.submit(&request, &AllowAll, &[])?;
    authority.save_state(&options.data_dir)?;
    print_receipt(&outcome);
    Ok(())
}

/// Returns the local authority configuration.
#[cfg(not(target_arch = "wasm32"))]
fn config() -> AuthorityConfig {
    AuthorityConfig::new(
        ShardId::new(b"authority-bin".to_vec()),
        PolicyVersion::from_bytes([4; 32]),
        ManifestDigest::from_bytes([5; 32]),
        MapVersion::new(6),
        ProjectionEpoch::new(7),
        ProjectionWatermarks::new(),
        KeyIdentifier::new("authority-local-checkpoint"),
    )
}

/// Prints one receipt in a stable text form.
#[cfg(not(target_arch = "wasm32"))]
fn print_receipt(outcome: &oxkv_write::SubmitOutcome) {
    let receipt = outcome.receipt();
    let audit = receipt.audit();
    println!("outcome.duplicate={}", outcome.is_duplicate());
    println!("receipt.state={}", receipt.state().name());
    println!("receipt.id={}", hex(receipt.receipt_id().as_bytes()));
    println!(
        "receipt.request_id={}",
        hex(receipt.request_id().as_bytes())
    );
    println!(
        "receipt.idempotency_key={}",
        hex(receipt.idempotency_key().as_bytes())
    );
    match receipt.commit_version() {
        Some(version) => println!("receipt.commit_version={}", version.get()),
        None => println!("receipt.commit_version=none"),
    }
    match receipt.current_key_version() {
        Some(version) => println!("receipt.current_key_version={}", version.get()),
        None => println!("receipt.current_key_version=none"),
    }
    println!("receipt.reason={}", receipt.reason_code().unwrap_or("none"));
    println!("receipt.audit_sequence={}", audit.audit_sequence.get());
    println!("receipt.audit_record_hash={}", audit.record_hash);
}

/// Returns the lowercase hexadecimal form of one byte string.
#[cfg(not(target_arch = "wasm32"))]
fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        write!(text, "{byte:02x}").expect("writing to a String cannot fail");
    }
    text
}
