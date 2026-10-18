use super::*;
use ai_platform_client::device_auth::DeviceLogin;
use serde::Deserialize;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    version: u32,
    origin: String,
    #[serde(default = "client_id")]
    client_id: String,
}
fn client_id() -> String {
    "agentforge-cli".into()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialRequest {
    operation: String,
    origin: String,
    client_id: String,
}
pub async fn credential_stdio() -> Result<(), Box<dyn std::error::Error>> {
    if std::io::stdin().is_terminal() || std::io::stdout().is_terminal() {
        return Err("Credential protocol requires private pipes".into());
    }
    let mut bytes = Vec::new();
    std::io::stdin().take(16385).read_to_end(&mut bytes)?;
    if bytes.len() > 16384 {
        return Err("Credential request exceeds limit".into());
    }
    let request: CredentialRequest = serde_json::from_slice(&bytes)?;
    if request.operation != "access_token" {
        return Err("Unknown credential operation".into());
    }
    let session = DeviceLogin::new(&request.origin, &request.client_id)?
        .access_token()
        .await?;
    println!(
        "{}",
        json!({"access_token":session.token,"expires_at":session.expires_at,"tenant_id":session.tenant_id,"user_id":session.user_id})
    );
    Ok(())
}
pub fn expand_profile(args: Vec<String>) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let Some(index) = args.iter().position(|a| a == "--config") else {
        return Ok(args);
    };
    let path = args.get(index + 1).ok_or("--config requires a file")?;
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 16384 {
        return Err("Unsafe config file".into());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(16385)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 16384 {
        return Err("Config exceeds size limit".into());
    }
    let profile: Profile = serde_json::from_slice(&bytes)?;
    if profile.version != 1 || profile.client_id != "agentforge-cli" {
        return Err("Invalid config profile".into());
    }
    Client::new(&profile.origin)?;
    if args.iter().any(|a| a == "--origin") {
        return Err("Do not combine config origin with --origin".into());
    }
    let mut rest = args;
    rest.drain(index..index + 2);
    rest.extend(["--origin".into(), profile.origin]);
    Ok(rest)
}
pub async fn auth(args: &[String]) -> Result<u8, Box<dyn std::error::Error>> {
    let action = args
        .first()
        .map(String::as_str)
        .ok_or("Use auth login|status|logout")?;
    let mut origin = None;
    let mut client = "agentforge-cli".to_owned();
    let mut seen_client = false;
    let mut iter = args[1..].iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--origin" if origin.is_none() => {
                origin = Some(iter.next().ok_or("--origin requires a value")?.clone())
            }
            "--client-id" if !seen_client => {
                seen_client = true;
                client = iter.next().ok_or("--client-id requires a value")?.clone()
            }
            _ => return Err("Unknown or duplicate auth option".into()),
        }
    }
    let login = DeviceLogin::new(origin.as_deref().ok_or("--origin is required")?, &client)?;
    let value = match action {
        "login" => {
            let verifier = DeviceLogin::verifier();
            let challenge = login.begin(&verifier).await?;
            println!(
                "{}",
                json!({"event":"device_authorization","verification_uri":challenge.verification_uri,"user_code":challenge.user_code,"expires_in":challenge.expires_in})
            );
            std::io::stdout().flush()?;
            login.complete(challenge, &verifier).await?
        }
        "status" => login.status()?,
        "logout" => login.logout().await?,
        _ => return Err("Use auth login|status|logout".into()),
    };
    println!("{value}");
    Ok(0)
}
