#[tokio::main]
async fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if let Err(error) = ai_platform_knowledge_ops::run(&args).await {
        eprintln!("knowledge operator command failed: {error}");
        std::process::exit(2);
    }
}
