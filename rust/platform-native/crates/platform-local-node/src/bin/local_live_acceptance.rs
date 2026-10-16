#[tokio::main(flavor = "current_thread")]
async fn main() {
    #[cfg(unix)]
    let result = ai_platform_local_node::acceptance::run().await;
    #[cfg(not(unix))]
    let result: Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> =
        Err("local acceptance requires Unix".into());
    let (code, value) = match result {
        Ok(value) => (0, value),
        Err(error) => (
            1,
            serde_json::json!({"schema_version":1,"harness":"local_live_acceptance","overall_status":"failed","error_type":"AcceptanceError","error":error.to_string()}),
        ),
    };
    println!("{}", serde_json::to_string_pretty(&value).unwrap());
    std::process::exit(code);
}
