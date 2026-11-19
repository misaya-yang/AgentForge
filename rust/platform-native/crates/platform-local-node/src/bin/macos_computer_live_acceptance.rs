fn main() {
    let mut args = std::env::args().skip(1);
    let result = (|| -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
        if args.next().as_deref() != Some("--report-dir") {
            return Err("--report-dir is required".into());
        }
        let path = std::path::PathBuf::from(args.next().ok_or("--report-dir requires a path")?);
        if args.next().is_some() {
            return Err("unexpected argument".into());
        }
        #[cfg(unix)]
        {
            ai_platform_local_node::macos_acceptance::run(&path)
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Err("native live acceptance is macOS-only".into())
        }
    })();
    let (code, value) = match result {
        Ok(v) => (0, v),
        Err(e) => (
            1,
            serde_json::json!({"schema_version":1,"overall_status":"failed","error_type":"AcceptanceError","error":e.to_string()}),
        ),
    };
    println!("{}", serde_json::to_string_pretty(&value).unwrap());
    std::process::exit(code)
}
