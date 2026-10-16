#[tokio::main]
async fn main() {
    let result = match ai_platform_eval::cli::Args::parse(std::env::args().skip(1), &["no-write"]) {
        Ok(args) => ai_platform_eval::runtime_regression::run(args).await,
        Err(e) => Err(e),
    };
    match result {
        Ok((v, code)) => {
            println!("{}", serde_json::to_string_pretty(&v).unwrap());
            std::process::exit(code)
        }
        Err(e) => {
            eprintln!("assistant_runtime_regression failed: {e}");
            std::process::exit(1)
        }
    }
}
