fn main() {
    let result = ai_platform_eval::cli::Args::parse(
        std::env::args().skip(1),
        &["enable-external-judge-gate"],
    )
    .and_then(ai_platform_eval::cli::rag_cli);
    match result {
        Ok((value, code)) => {
            println!("{}", serde_json::to_string_pretty(&value).unwrap());
            std::process::exit(code)
        }
        Err(e) => {
            eprintln!("eval_rag failed: {e}");
            std::process::exit(1)
        }
    }
}
