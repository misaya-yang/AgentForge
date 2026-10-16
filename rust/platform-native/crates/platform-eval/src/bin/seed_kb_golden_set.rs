fn main() {
    match ai_platform_eval::cli::Args::parse(std::env::args().skip(1), &["check", "force"])
        .and_then(ai_platform_eval::seed::run)
    {
        Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap()),
        Err(e) => {
            eprintln!("seed_kb_golden_set failed: {e}");
            std::process::exit(1)
        }
    }
}
