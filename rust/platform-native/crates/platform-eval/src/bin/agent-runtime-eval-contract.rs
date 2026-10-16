use serde_json::{Value, json};
use std::io::Read;
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1)
    }
}
fn run() -> ai_platform_eval::Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args == ["--help"] || args == ["-h"] {
        println!(
            "agent-runtime-eval-contract [--input PATH|-] (JSON {{thread,events}}; stdin by default)"
        );
        return Ok(());
    }
    let path = match args.as_slice() {
        [] => "-",
        [p] => p.as_str(),
        [flag, p] if flag == "--input" => p.as_str(),
        _ => return Err("invalid arguments".into()),
    };
    let mut bytes = vec![];
    if path == "-" {
        std::io::stdin().read_to_end(&mut bytes)?;
    } else {
        bytes = std::fs::read(path)?;
    }
    let v: Value = serde_json::from_slice(&bytes)?;
    let thread = v
        .get("thread")
        .filter(|v| v.is_object())
        .ok_or("thread must be an object")?;
    let events = v["events"].as_array().ok_or("events must be an array")?;
    let failures = ai_platform_eval::runtime_contract::validate_runtime_observation(thread, events);
    println!(
        "{}",
        json!({"passed":failures.is_empty(),"failures":failures})
    );
    if !failures.is_empty() {
        return Err("Runtime observation contract failed".into());
    }
    Ok(())
}
