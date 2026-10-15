//! Explicit interactive CLI. No credential discovery; the user supplies a dedicated directory.
//! Proposed location after root releases binary discovery freeze: src/bin/chatgpt-plan.rs.
use ai_platform_gateway::migration_host::chatgpt_plan::{
    Account, AuthorizationAttempt, ChatGptPlanClient, FunctionNamespace, LoopbackLogin,
    MessageRole, ModelCatalog, ProtectedStore, ResponseInput, TextMessage,
};
use serde::Deserialize;
use std::{
    io::{self, BufRead, Read, Write},
    sync::Arc,
};

#[derive(Deserialize)]
struct BridgeRequest {
    model: Option<String>,
    input: Vec<ResponseInput>,
    instructions: Option<String>,
    #[serde(default)]
    tools: Vec<FunctionNamespace>,
}
fn select_account(store: &Arc<ProtectedStore>, client_id: Option<&str>) -> Result<Account, String> {
    let accounts = store.accounts().map_err(|e| e.to_string())?;
    let selected=match client_id {
        Some(id)=>accounts.iter().find(|a|a.client_id==id).ok_or("The selected ChatGPT account is not saved.")?,
        None if accounts.len()==1=>&accounts[0],
        _=>return Err("Choose a saved client ID using the accounts command; no account is selected automatically when multiple exist.".into()),
    };
    store.load(selected).map_err(|e| e.to_string())
}
fn choose_model(catalog: &ModelCatalog, requested: Option<&str>) -> Result<String, String> {
    if let Some(slug) = requested {
        return catalog
            .models
            .iter()
            .find(|m| m.slug == slug)
            .map(|m| m.slug.clone())
            .ok_or("Requested model is not in this account's catalog.".into());
    }
    catalog.preferred_luna().map(|m|m.slug.clone()).ok_or("Luna is not listed for this account. Choose an available model explicitly; billing/model fallback is disabled.".into())
}
fn read_line(reader: &mut impl BufRead) -> Result<Option<String>, String> {
    let mut line = String::new();
    let count = reader
        .take(1024 * 1024 + 1)
        .read_line(&mut line)
        .map_err(|_| "Could not read input.".to_owned())?;
    if count > 1024 * 1024 {
        return Err("Request is too large.".into());
    }
    Ok(if count == 0 { None } else { Some(line) })
}
async fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        return Err("Usage: chatgpt-plan <login|accounts|models|ask|bridge|logout> <private-directory> [client-id]. login supports --headless. ask reads a prompt from stdin. bridge reads one JSON request per stdin line and emits structured events; tools are executed by the caller only.".into());
    }
    let action = &args[0];
    if !["login", "accounts", "models", "ask", "bridge", "logout"].contains(&action.as_str()) {
        return Err("Unknown command.".into());
    }
    let store = ProtectedStore::open(&args[1]).map_err(|e| e.to_string())?;
    let client_id = args
        .get(2)
        .filter(|v| !v.starts_with("--"))
        .map(String::as_str);
    let client = ChatGptPlanClient::new().map_err(|e| e.to_string())?;
    if action == "accounts" {
        for identity in store.accounts().map_err(|e| e.to_string())? {
            println!(
                "{}\t{}",
                identity.client_id,
                identity.email.as_deref().unwrap_or("ChatGPT account")
            );
        }
        return Ok(());
    }
    if action == "login" {
        let returning = if client_id.is_some() {
            Some(select_account(&store, client_id)?)
        } else {
            None
        };
        let listener = LoopbackLogin::bind(0).await.map_err(|e| e.to_string())?;
        let attempt = AuthorizationAttempt::new(
            store.host_id(),
            "AI Platform",
            listener.redirect_uri(),
            returning.as_ref(),
        )
        .await
        .map_err(|e| e.to_string())?
        .without_login_hints();
        if args.iter().any(|a| a == "--headless") {
            // Contains state/nonce/challenge, never ID/access/refresh token or PKCE verifier.
            eprintln!(
                "Open this link in a browser on this host and approve only if you intend to connect ChatGPT plan usage:\n{}",
                attempt.authorization_url().expose()
            );
        } else {
            #[cfg(target_os = "macos")]
            let opener = "open";
            #[cfg(not(target_os = "macos"))]
            let opener = "xdg-open";
            let status = std::process::Command::new(opener)
                .arg(attempt.authorization_url().expose())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map_err(|_| {
                    "Could not open the browser. Run login with --headless on this host.".to_owned()
                })?;
            if !status.success() {
                return Err(
                    "Could not open the browser. Run login with --headless on this host.".into(),
                );
            }
            eprintln!("Complete sign-in in your browser. Waiting up to ten minutes.");
        }
        let callback = listener.wait_callback().await.map_err(|e| e.to_string())?;
        let account = client
            .exchange(attempt, callback.expose())
            .await
            .map_err(|e| e.to_string())?
            .persist(store)
            .await
            .map_err(|e| e.to_string())?;
        println!(
            "Connected registration {}. ChatGPT plan permission: {}.",
            account.identity().client_id,
            account.plan_enabled().await
        );
        return Ok(());
    }
    let account = select_account(&store, client_id)?;
    if action == "logout" {
        client.sign_out(&account).await.map_err(|_|"Local sign-out finished, but remote revocation or durable local cleanup was not confirmed. Check ChatGPT Settings and the protected store before reconnecting.".to_owned())?;
        println!("Signed out. Registration and host identity retained.");
        return Ok(());
    }
    let catalog = client.models(&account).await.map_err(|e| e.to_string())?;
    if action == "models" {
        for model in &catalog.models {
            println!("{}\t{}", model.slug, model.display_name);
        }
        return Ok(());
    }
    if action == "ask" {
        let model = choose_model(&catalog, None)?;
        eprintln!("Using account-returned model: {model}");
        let prompt = read_line(&mut io::stdin().lock())?.ok_or("A prompt is required.")?;
        let mut stream = client
            .responses(
                &account,
                &catalog,
                &model,
                vec![TextMessage {
                    role: MessageRole::User,
                    content: prompt,
                }],
                None,
            )
            .await
            .map_err(|e| e.to_string())?;
        while let Some(event) = stream
            .next()
            .await
            .map_err(|e| format!("{}; code={}", e, e.code().unwrap_or("none")))?
        {
            if event.event_type == "response.output_text.delta" {
                if let Some(delta) = event.data.get("delta").and_then(|v| v.as_str()) {
                    print!("{delta}");
                    io::stdout()
                        .flush()
                        .map_err(|_| "Could not write output.")?;
                }
            }
        }
        println!();
        return Ok(());
    }
    // A bounded explicit bridge, not an agent loop. Each line is one authorized inference.
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    while let Some(line) = read_line(&mut reader)? {
        let request: BridgeRequest =
            serde_json::from_str(&line).map_err(|_| "Invalid bridge request.")?;
        let model = choose_model(&catalog, request.model.as_deref())?;
        let mut stream = client
            .responses_with_tools(
                &account,
                &catalog,
                &model,
                request.input,
                request.instructions.as_deref(),
                request.tools,
            )
            .await
            .map_err(|e| e.to_string())?;
        while let Some(event) = stream
            .next()
            .await
            .map_err(|e| format!("{}; code={}", e, e.code().unwrap_or("none")))?
        {
            println!(
                "{}",
                serde_json::to_string(&event).map_err(|_| "Could not encode event.")?
            );
            io::stdout()
                .flush()
                .map_err(|_| "Could not write output.")?;
        }
    }
    Ok(())
}
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
