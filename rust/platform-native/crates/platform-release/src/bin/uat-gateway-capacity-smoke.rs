#[tokio::main]
async fn main() {
    match ai_platform_release::capacity::smoke().await {
        Ok(code) => std::process::exit(code),
        Err(_) => {
            eprintln!("Capacity smoke failed; check options and service availability.");
            std::process::exit(1)
        }
    }
}
