//! Read-only planning or explicit PG16→18 isolated cold-copy rehearsal.
#[path = "../pg_upgrade_rehearsal.rs"]
mod pg_upgrade_rehearsal;
#[tokio::main]
async fn main() {
    match pg_upgrade_rehearsal::run(std::env::args().skip(1).collect()).await {
        Ok(v) => println!("{v}"),
        Err(e) => {
            eprintln!("pg-major-rehearsal: {e}");
            std::process::exit(1);
        }
    }
}
