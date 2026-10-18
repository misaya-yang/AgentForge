//! Explicit managed-child chaos execution. No deployment or arbitrary PID control.
use ai_platform_gateway_foundation::multi_instance::chaos::{Plan, execute};
#[tokio::main]
async fn main() -> std::process::ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 5 || args[0] != "--execute" || args[1] != "--plan" || args[3] != "--out" {
        eprintln!(
            "usage: gateway-ha-chaos --execute --plan PRIVATE_PLAN_FILE --out NEW_EXTERNAL_DIRECTORY"
        );
        return std::process::ExitCode::from(2);
    }
    let result = match Plan::read(std::path::Path::new(&args[2])) {
        Ok(plan) => execute(plan, std::path::Path::new(&args[4])).await,
        Err(error) => Err(error),
    };
    match result {
        Ok(receipt) => {
            println!(
                "chaos checks passed={} kill9_count={} retirement=false",
                receipt.passed, receipt.kill9_count
            );
            std::process::ExitCode::from(if receipt.passed { 0 } else { 1 })
        }
        Err(error) => {
            eprintln!("Gateway HA chaos: {error}");
            std::process::ExitCode::from(2)
        }
    }
}
