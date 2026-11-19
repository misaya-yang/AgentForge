//! Explicit U-PE-04 workflow; no live activity without `run` and accepted inputs.
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match ai_platform_tools::performance_baseline::command(&args) {
        Ok(report) => {
            println!("{}", serde_json::to_string_pretty(&report).unwrap());
            if args.first().is_some_and(|a| a == "run")
                && report["status"] != "COMPLETE_BASELINE_NO_SLO_EVALUATION"
            {
                std::process::exit(2);
            }
        }
        Err(code) => {
            eprintln!("performance baseline refused: {code}");
            std::process::exit(2);
        }
    }
}
