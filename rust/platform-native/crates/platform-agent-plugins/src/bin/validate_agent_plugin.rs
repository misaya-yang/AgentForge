use ai_platform_agent_plugins::load_agent_plugin;
fn main() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 1 {
        eprintln!("Usage: validate-agent-plugin PLUGIN_DIRECTORY");
        return std::process::ExitCode::from(2);
    }
    let (payload, code) = match load_agent_plugin(&args[0]) {
        Ok(package) => (package.validation_payload(), 0),
        Err(error) => (serde_json::json!({"valid":false,"code":error.code}), 1),
    };
    println!("{}", payload);
    std::process::ExitCode::from(code)
}
