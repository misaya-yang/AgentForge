//! Offline evaluator; exits nonzero on absent/untrusted evidence or any regression.
use ai_platform_knowledge_ops::promotion;
use std::path::Path;
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["evaluator-sha256"] {
        println!("{}", promotion::evaluator_sha256());
        return;
    }
    if !(args.len() == 4 || (args.len() == 5 && args[4] == "--fixture-only")) {
        eprintln!(
            "usage: knowledge-eval-gate ROOT POLICY CANDIDATE EXPECTED_SOURCE_SHA [--fixture-only] | evaluator-sha256"
        );
        std::process::exit(2);
    }
    match promotion::evaluate(
        Path::new(&args[0]),
        Path::new(&args[1]),
        Path::new(&args[2]),
        &args[3],
        args.len() == 5,
        chrono::Utc::now(),
    ) {
        Ok(decision) => println!(
            "{}",
            serde_json::to_string_pretty(&decision).expect("finite result")
        ),
        Err(error) => {
            eprintln!("knowledge promotion denied: {error}");
            std::process::exit(1);
        }
    }
}
