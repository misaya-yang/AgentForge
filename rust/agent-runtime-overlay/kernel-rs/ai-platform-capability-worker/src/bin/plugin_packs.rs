//! Offline, deployment-owner pack management. Never connects to a service.
use std::path::PathBuf;
use ai_platform_capability_worker::plugin_packs::{self, TrustPolicy};
use clap::{Parser, Subcommand};
use serde::Serialize;

#[derive(Parser)]
#[command(version, about = "Manage pinned Agent Plugin packs for compiled Worker executors")]
struct Args { #[command(subcommand)] command: Command }
#[derive(Subcommand)]
enum Command {
    /// Print exact compiled descriptor hashes for an operator-reviewed allowlist.
    Fingerprints { #[arg(long)] worker_binary: PathBuf },
    /// Import a pinned local USTAR artifact. Does not activate it.
    Install {
        #[arg(long)] store: PathBuf, #[arg(long)] trust: PathBuf,
        #[arg(long)] id: String, #[arg(long)] version: String, #[arg(long)] archive: PathBuf,
    },
    /// Stage a complete replacement active set; an empty set disables managed IDs.
    Activate {
        #[arg(long)] store: PathBuf, #[arg(long)] trust: PathBuf,
        #[arg(long = "package")] packages: Vec<String>,
        #[arg(long)] expected_generation: u64,
        #[arg(long, default_value_t = false)] writes_enabled: bool,
    },
    /// Stage a prior pack set under current trust in a new activation generation.
    Rollback {
        #[arg(long)] store: PathBuf, #[arg(long)] trust: PathBuf,
        #[arg(long)] activation_id: String, #[arg(long)] expected_generation: u64,
        #[arg(long, default_value_t = false)] writes_enabled: bool,
    },
    /// Read the durable desired pointer after success, conflict or uncertain I/O.
    Status {
        #[arg(long)] store: PathBuf, #[arg(long)] trust: PathBuf,
        #[arg(long, default_value_t = false)] writes_enabled: bool,
    },
    /// Validate a staged generation. This is not a running Worker health check.
    Inspect {
        #[arg(long)] store: PathBuf, #[arg(long)] trust: PathBuf,
        #[arg(long)] activation: PathBuf,
        #[arg(long, default_value_t = false)] writes_enabled: bool,
    },
}
fn print(value: &impl Serialize) -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", serde_json::to_string_pretty(value)?); Ok(())
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    match Args::parse().command {
        Command::Fingerprints { worker_binary } => print(&serde_json::json!({
            "worker_binary_sha256": plugin_packs::executable_hash(&worker_binary)?,
            "capabilities": plugin_packs::compiled_fingerprints()?,
        })),
        Command::Install { store, trust, id, version, archive } => {
            let policy = TrustPolicy::load(&trust)?;
            print(&serde_json::json!({"status":"installed_inactive", "package":plugin_packs::install(&store, &policy, &id, &version, &archive)?}))
        }
        Command::Activate { store, trust, packages, expected_generation, writes_enabled } => {
            let selected = packages.iter().map(|value| value.split_once('@')
                .map(|(id, version)| (id.to_owned(), version.to_owned()))
                .ok_or(plugin_packs::PackError::Invalid)).collect::<Result<Vec<_>, _>>()?;
            print(&plugin_packs::activate(&store, &TrustPolicy::load(&trust)?, &selected, expected_generation, writes_enabled)?)
        }
        Command::Rollback { store, trust, activation_id, expected_generation, writes_enabled } =>
            print(&plugin_packs::rollback(&store, &TrustPolicy::load(&trust)?, &activation_id, expected_generation, writes_enabled)?),
        Command::Status { store, trust, writes_enabled } =>
            print(&plugin_packs::desired_activation(&store, &TrustPolicy::load(&trust)?, writes_enabled)?),
        Command::Inspect { store, trust, activation, writes_enabled } => {
            let active = plugin_packs::load_activation(&store, &TrustPolicy::load(&trust)?, &activation, writes_enabled)?;
            print(&serde_json::json!({"status":"validated_not_observed_running", "activation":active.receipt}))
        }
    }
}
