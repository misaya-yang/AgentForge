//! Offline executable identity, before an event loop, runtime or client is created.

pub fn handle_args() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    let arg = args.next()?;
    if args.next().is_some() {
        eprintln!("Expected no arguments or one of --help, --version, --package-info");
        return Some(2);
    }
    if arg == "--help" || arg == "-h" {
        println!(
            "AI Platform native desktop\n\nUsage: ai-platform-desktop [--help|--version|--package-info]\n\nNo arguments opens the Gateway sign-in window.\nThese flags run offline without opening a window or reading credentials."
        );
    } else if arg == "--version" || arg == "-V" {
        println!("ai-platform-desktop {}", env!("CARGO_PKG_VERSION"));
    } else if arg == "--package-info" {
        let info = serde_json::json!({
            "schema_version": "ai-platform-desktop/build-info/v1",
            "package": env!("CARGO_PKG_NAME"),
            "version": env!("CARGO_PKG_VERSION"),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "source_commit": option_env!("AI_PLATFORM_DESKTOP_SOURCE_COMMIT").unwrap_or("unrecorded"),
            "source_digest": option_env!("AI_PLATFORM_DESKTOP_SOURCE_DIGEST").unwrap_or("unrecorded"),
            "rust_target": option_env!("AI_PLATFORM_DESKTOP_RUST_TARGET").unwrap_or("unrecorded"),
        });
        println!("{info}");
    } else {
        // Do not repeat arbitrary input, which may include a mistakenly supplied secret.
        eprintln!("Unknown argument. Run ai-platform-desktop --help");
        return Some(2);
    }
    Some(0)
}
