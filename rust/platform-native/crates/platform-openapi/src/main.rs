#[allow(dead_code)]
#[path = "../../platform-gateway-foundation/src/native_contract.rs"]
mod native_contract;
fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("arc01") {
        let doc = ai_platform_openapi::document::extend_document(native_contract::document());
        return std::process::ExitCode::from(ai_platform_openapi::arc01::run(&args[1..], &doc));
    }
    if args.first().map(String::as_str) == Some("gateway-kb-boundary") {
        return std::process::ExitCode::from(ai_platform_openapi::kb_boundary::run(&args[1..]));
    }
    if args.first().map(String::as_str) == Some("kb-release-evidence") {
        return std::process::ExitCode::from(ai_platform_openapi::kb_release::run(&args[1..]));
    }
    if args.first().map(String::as_str) == Some("snapshot") {
        return std::process::ExitCode::from(ai_platform_openapi::snapshot::run(&args[1..]));
    }
    if args.first().map(String::as_str) == Some("gate") {
        return std::process::ExitCode::from(ai_platform_openapi::cli::run_gate(&args[1..]));
    }
    if std::env::args().skip(1).collect::<Vec<_>>() != ["--native-only"] {
        eprintln!(
            "Usage: ai-platform-openapi --native-only (bounded, incomplete Gateway contract)"
        );
        return std::process::ExitCode::from(2);
    }
    let document = ai_platform_openapi::document::extend_document(native_contract::document());
    println!("{}", serde_json::to_string_pretty(&document).unwrap());
    std::process::ExitCode::SUCCESS
}
