#[tokio::main]
async fn main() {
    let result = match ai_platform_eval::cli::Args::parse(std::env::args().skip(1), &[]) {
        Ok(args) => {
            if args.positional.first().is_some_and(|s| s == "live") {
                ai_platform_eval::live::run(args).await
            } else if args.positional.first().is_some_and(|s| s == "candidate") {
                ai_platform_eval::golden_cli::candidate(args)
            } else {
                ai_platform_eval::golden_cli::run(args)
            }
        }
        Err(e) => Err(e),
    };
    match result {
        Ok((v, code)) => {
            println!("{}", serde_json::to_string_pretty(&v).unwrap());
            std::process::exit(code)
        }
        Err(e) => {
            eprintln!("eval_golden failed: {e}");
            std::process::exit(1)
        }
    }
}
