mod runtime_recovery_common;
use runtime_recovery_common::*;
#[tokio::main]
async fn main() {
    if run().await.is_err() {
        eprintln!("Runtime recovery observation failed (credentials and driver details withheld)");
        std::process::exit(1)
    }
}
async fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let (mut id, mut path) = (None, None);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--run-id" => id = Some(args.next().ok_or("missing run id")?.parse()?),
            "--output" => {
                path = Some(std::path::PathBuf::from(
                    args.next().ok_or("missing output")?,
                ))
            }
            "--help" | "-h" => {
                println!("runtime_recovery_observe --run-id UUID --output PATH");
                return Ok(());
            }
            _ => return Err("unknown argument".into()),
        }
    }
    observe(
        id.ok_or("--run-id required")?,
        &path.ok_or("--output required")?,
    )
    .await?;
    Ok(())
}
