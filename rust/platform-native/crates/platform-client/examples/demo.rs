//! Native translation of sdk/python/demo.py. Running this contacts the configured gateway.
use ai_platform_client::{
    sdk::{AssistantClient, AuthConfig},
    sdk_models::ChatRequest,
};
use std::io::Write;
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let key = std::env::var("GATEWAY_API_KEY")
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or("GATEWAY_API_KEY must be set")?;
    let mut config = AuthConfig::new(key);
    config.base_url =
        std::env::var("GATEWAY_BASE_URL").unwrap_or_else(|_| "http://localhost:8080".into());
    let mut client = AssistantClient::new(config)?;
    println!("{}\n1. 普通对话\n{}", "=".repeat(50), "=".repeat(50));
    println!(
        "回答: {}\n",
        client
            .chat
            .send(ChatRequest::new("用一句话介绍这个平台适合解决什么问题"))
            .await?
            .content
    );
    println!(
        "{}\n2. 流式对话（逐字输出）\n{}",
        "=".repeat(50),
        "=".repeat(50)
    );
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let (_cancel, cancel) = tokio::sync::watch::channel(false);
    let stream = client.chat.stream(
        ChatRequest::new("用三句话解释知识库问答如何工作"),
        tx,
        cancel,
    );
    let consume = async {
        while let Some(event) = rx.recv().await {
            let event = event?;
            if event.is_text() {
                print!("{}", event.text());
                std::io::stdout()
                    .flush()
                    .map_err(|_| ai_platform_client::Error::Response)?;
            }
            if event.is_done() {
                break;
            }
        }
        Ok::<(), ai_platform_client::Error>(())
    };
    let (a, b) = tokio::join!(stream, consume);
    a?;
    b?;
    println!("\n");
    println!("{}\n3. 知识库问答\n{}", "=".repeat(50), "=".repeat(50));
    let datasets = client.knowledge.list_datasets().await?;
    println!(
        "可用知识库: {:?}",
        datasets.iter().map(|d| &d.name).collect::<Vec<_>>()
    );
    if let Some(first) = datasets.first() {
        let reply = client
            .knowledge
            .ask(
                "What is the ruling on Zakat?",
                vec![first.dataset_id.clone()],
                ChatRequest::new(""),
            )
            .await?;
        println!(
            "回答: {}...\n",
            reply.content.chars().take(200).collect::<String>()
        );
    }
    println!("{}\n4. 可用工具\n{}", "=".repeat(50), "=".repeat(50));
    for tool in client.tools.list_tools(100, 0).await? {
        println!(
            "  - {} ({})",
            tool.name,
            tool.category.as_deref().unwrap_or("None")
        );
    }
    client.close().await?;
    println!("\n✅ Demo 完成!");
    Ok(())
}
