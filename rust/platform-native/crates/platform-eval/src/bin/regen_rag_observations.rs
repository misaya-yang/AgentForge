#[tokio::main]
async fn main() {
    let result = match ai_platform_eval::cli::Args::parse(
        std::env::args().skip(1),
        &["retrieval-only", "force", "dry-run"],
    ) {
        Ok(args) => ai_platform_eval::record::run(args).await,
        Err(e) => Err(e),
    };
    match result {
        Ok((v, code)) => {
            println!("{}", serde_json::to_string_pretty(&v).unwrap());
            std::process::exit(code)
        }
        Err(e) => {
            eprintln!("regen_rag_observations failed: {e}");
            std::process::exit(1)
        }
    }
}
