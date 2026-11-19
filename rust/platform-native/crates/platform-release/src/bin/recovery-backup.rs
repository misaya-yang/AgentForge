//! Explicit, opt-in local backup and isolated recovery drill command.
#[path = "../recovery_backup.rs"]
mod recovery_backup;
#[tokio::main]
async fn main() {
    match recovery_backup::run(std::env::args().skip(1).collect()).await {
        Ok(receipt) => println!("{receipt}"),
        Err(error) => {
            eprintln!("recovery-backup: {error}");
            std::process::exit(1);
        }
    }
}
