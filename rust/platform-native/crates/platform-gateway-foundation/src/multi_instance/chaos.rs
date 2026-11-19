//! Opt-in, source-bound chaos runner. It only kills children it created; it
//! cannot kill arbitrary PIDs, control Docker, deploy, or retire a release ADR.
use super::{Error, hash};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub schema: String,
    pub source_sha256: String,
    pub origins: Vec<String>,
    pub credential_file: PathBuf,
    pub ca_file: Option<PathBuf>,
    pub steps: Vec<Step>,
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    Spawn {
        label: String,
        program: PathBuf,
        sha256: String,
        args: Vec<String>,
        stdin_file: Option<PathBuf>,
    },
    Kill9 {
        label: String,
    },
    Probe {
        origin: usize,
        method: String,
        path: String,
        body: Option<Value>,
        status: u16,
        assertions: Vec<Assertion>,
        capture: Option<String>,
        capture_pointer: Option<String>,
    },
    Wait {
        milliseconds: u64,
    },
    EqualCaptures {
        left: String,
        right: String,
    },
    Load {
        path: String,
        requests: u32,
        concurrency: usize,
        status: u16,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assertion {
    pub pointer: String,
    pub equals: Value,
}
#[derive(Serialize)]
pub struct Receipt {
    pub schema: String,
    pub source_sha256: String,
    pub passed: bool,
    pub steps: Vec<Value>,
    pub kill9_count: u64,
    pub elapsed_ms: u64,
    pub single_instance_assumption_retired: bool,
}
fn origin(value: &str) -> Result<reqwest::Url, Error> {
    let url = reqwest::Url::parse(value).map_err(|_| Error::Invalid)?;
    let loopback = url
        .host_str()
        .and_then(|host| {
            host.trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .ok()
        })
        .is_some_and(|ip| ip.is_loopback());
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
        || value.ends_with('/')
        || !(url.scheme() == "https" || url.scheme() == "http" && loopback)
    {
        return Err(Error::Invalid);
    }
    Ok(url)
}
impl Plan {
    pub fn read(path: &Path) -> Result<Self, Error> {
        let bytes =
            ai_platform_service_primitives::workload_identity::secrets::read_file(path, true)
                .map_err(|_| Error::Invalid)?;
        let plan: Self = serde_json::from_slice(bytes.expose()).map_err(|_| Error::Invalid)?;
        plan.validate()?;
        Ok(plan)
    }
    pub fn validate(&self) -> Result<(), Error> {
        if self.schema != "agentforge/gateway-ha-chaos/v1"
            || self.source_sha256.len() != 64
            || !self.source_sha256.bytes().all(|b| b.is_ascii_hexdigit())
            || self.origins.len() < 2
            || self.origins.len() > 8
            || self.steps.is_empty()
            || self.steps.len() > 10000
            || !self.credential_file.is_absolute()
        {
            return Err(Error::Invalid);
        }
        let mut origins = BTreeSet::new();
        for value in &self.origins {
            if !origins.insert(origin(value)?.origin().ascii_serialization()) {
                return Err(Error::Invalid);
            }
        }
        let mut labels = BTreeSet::new();
        for step in &self.steps {
            match step {
                Step::Spawn {
                    label,
                    program,
                    sha256,
                    args,
                    stdin_file,
                } => {
                    super::bounded(label)?;
                    if stdin_file.as_ref().is_some_and(|file| !file.is_absolute()) {
                        return Err(Error::Invalid);
                    }
                    if !labels.insert(label)
                        || !program.is_absolute()
                        || sha256.len() != 64
                        || !sha256.bytes().all(|b| b.is_ascii_hexdigit())
                        || args.len() > 32
                        || args
                            .iter()
                            .any(|arg| arg.len() > 1024 || arg.chars().any(char::is_control))
                    {
                        return Err(Error::Invalid);
                    }
                }
                Step::Kill9 { label } => {
                    if !labels.contains(label) {
                        return Err(Error::Invalid);
                    }
                }
                Step::Probe {
                    origin: index,
                    method,
                    path,
                    status,
                    assertions,
                    ..
                } => {
                    if *index >= self.origins.len()
                        || !matches!(method.as_str(), "GET" | "POST" | "DELETE" | "PUT" | "PATCH")
                        || !path.starts_with('/')
                        || path.starts_with("//")
                        || path.contains(['#', '\\'])
                        || !(100..=599).contains(status)
                        || assertions
                            .iter()
                            .any(|a| !a.pointer.is_empty() && !a.pointer.starts_with('/'))
                    {
                        return Err(Error::Invalid);
                    }
                }
                Step::Wait { milliseconds } => {
                    if *milliseconds > 60000 {
                        return Err(Error::Invalid);
                    }
                }
                Step::EqualCaptures { left, right } => {
                    super::bounded(left)?;
                    super::bounded(right)?;
                }
                Step::Load {
                    path,
                    requests,
                    concurrency,
                    status,
                } => {
                    if !path.starts_with('/')
                        || path.starts_with("//")
                        || path.contains(['#', '\\'])
                        || *requests == 0
                        || *requests > 100000
                        || *concurrency == 0
                        || *concurrency > 128
                        || !(100..=599).contains(status)
                    {
                        return Err(Error::Invalid);
                    }
                }
            }
        }
        Ok(())
    }
}
struct Children(BTreeMap<String, Child>);
impl Drop for Children {
    fn drop(&mut self) {
        for child in self.0.values_mut() {
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
}
fn private_log(path: &Path) -> Result<fs::File, Error> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(|_| Error::Unavailable)
}
fn private_input(path: &Path) -> Result<fs::File, Error> {
    if !path.is_absolute() {
        return Err(Error::Invalid);
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| Error::Invalid)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 4 * 1024 * 1024 {
        return Err(Error::Invalid);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(Error::Invalid);
        }
    }
    #[cfg(not(unix))]
    return Err(Error::Invalid);
    let file = fs::File::open(path).map_err(|_| Error::Unavailable)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let opened = file.metadata().map_err(|_| Error::Unavailable)?;
        if metadata.dev() != opened.dev() || metadata.ino() != opened.ino() {
            return Err(Error::Invalid);
        }
    }
    Ok(file)
}
async fn response_bytes(response: reqwest::Response, limit: usize) -> Result<Vec<u8>, Error> {
    use futures_util::StreamExt;
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| Error::Unavailable)?;
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err(Error::Invalid);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
fn digest_file(path: &Path) -> Result<String, Error> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    if !fs::symlink_metadata(path)
        .map_err(|_| Error::Invalid)?
        .is_file()
    {
        return Err(Error::Invalid);
    }
    let mut file = fs::File::open(path).map_err(|_| Error::Unavailable)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer).map_err(|_| Error::Unavailable)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
/// The output directory must be new. Private child logs and bodies are never
/// copied into the summary; captures retain only body digests for comparisons.
pub async fn execute(plan: Plan, output: &Path) -> Result<Receipt, Error> {
    plan.validate()?;
    if !output.is_absolute() || output.exists() {
        return Err(Error::Invalid);
    }
    fs::create_dir_all(output).map_err(|_| Error::Unavailable)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(output, fs::Permissions::from_mode(0o700))
            .map_err(|_| Error::Unavailable)?;
    }
    let credential = ai_platform_service_primitives::workload_identity::secrets::read_file(
        &plan.credential_file,
        true,
    )
    .map_err(|_| Error::Invalid)?;
    let token = std::str::from_utf8(credential.expose())
        .map_err(|_| Error::Invalid)?
        .trim_end_matches(['\r', '\n']);
    if token.is_empty() || token.chars().any(char::is_whitespace) {
        return Err(Error::Invalid);
    }
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(30));
    if let Some(path) = &plan.ca_file {
        let ca = ai_platform_service_primitives::workload_identity::secrets::read_file(path, false)
            .map_err(|_| Error::Invalid)?;
        let certs =
            reqwest::Certificate::from_pem_bundle(ca.expose()).map_err(|_| Error::Invalid)?;
        if certs.is_empty() {
            return Err(Error::Invalid);
        }
        builder = builder.tls_certs_only(certs);
    }
    let client = builder.build().map_err(|_| Error::Invalid)?;
    let mut children = Children(BTreeMap::new());
    let mut captures = BTreeMap::<String, String>::new();
    let mut results = Vec::new();
    let mut kills = 0;
    let started = Instant::now();
    let mut passed = true;
    for (index, step) in plan.steps.iter().enumerate() {
        let outcome: Result<Value,Error> = async {
            match step {
                Step::Spawn{label,program,sha256,args,stdin_file} => {
                    if digest_file(program)? != *sha256 {return Err(Error::Invalid)}
                    let stdout=private_log(&output.join(format!("child-{index}.stdout.private")))?;
                    let stderr=private_log(&output.join(format!("child-{index}.stderr.private")))?;
                    let stdin=if let Some(file)=stdin_file{Stdio::from(private_input(file)?)}else{Stdio::null()};
                    let child=Command::new(program).args(args).stdin(stdin).stdout(stdout).stderr(stderr).spawn().map_err(|_|Error::Unavailable)?;
                    children.0.insert(label.clone(),child);
                    Ok(json!({"kind":"spawn","label_sha256":hash(label),"binary_sha256":sha256,"passed":true}))
                },
                Step::Kill9{label} => {
                    let child=children.0.get_mut(label).ok_or(Error::Invalid)?;
                    if child.try_wait().map_err(|_|Error::Unavailable)?.is_some(){return Err(Error::Conflict)}
                    child.kill().map_err(|_|Error::Unavailable)?;let _=child.wait().map_err(|_|Error::Unavailable)?;kills+=1;
                    Ok(json!({"kind":"kill9","label_sha256":hash(label),"passed":true}))
                },
                Step::Probe{origin:index,method,path,body,status,assertions,capture,capture_pointer} => {
                    let endpoint=format!("{}{}",plan.origins[*index],path);
                    let target=reqwest::Url::parse(&endpoint).map_err(|_|Error::Invalid)?;
                    if target.origin()!=origin(&plan.origins[*index])?.origin(){return Err(Error::Invalid)}
                    let mut request=client.request(reqwest::Method::from_bytes(method.as_bytes()).map_err(|_|Error::Invalid)?,target).bearer_auth(token);
                    if let Some(body)=body{request=request.json(body)}
                    let response=request.send().await.map_err(|_|Error::Unavailable)?;let received=response.status().as_u16();
                    if received!=*status{return Ok(json!({"kind":"probe","status":received,"expected_status":status,"passed":false}))}
                    let bytes=response_bytes(response,8*1024*1024).await?;
                    let value=if assertions.is_empty()&&capture_pointer.is_none(){None}else{Some(serde_json::from_slice::<Value>(&bytes).map_err(|_|Error::Invalid)?)};
                    for assertion in assertions {if value.as_ref().and_then(|v|v.pointer(&assertion.pointer))!=Some(&assertion.equals){return Err(Error::Conflict)}}
                    let digest=hash(std::str::from_utf8(&bytes).map_err(|_|Error::Invalid)?);
                    if let Some(label)=capture{let captured=if let Some(pointer)=capture_pointer{hash(&value.as_ref().and_then(|value|value.pointer(pointer)).ok_or(Error::Conflict)?.to_string())}else{digest.clone()};if captures.insert(label.clone(),captured).is_some(){return Err(Error::Conflict)}}
                    Ok(json!({"kind":"probe","origin_index":index,"method":method,"path_sha256":hash(path),"status":received,"body_sha256":digest,"assertions":assertions.len(),"passed":true}))
                },
                Step::Wait{milliseconds} => {tokio::time::sleep(Duration::from_millis(*milliseconds)).await;Ok(json!({"kind":"wait","milliseconds":milliseconds,"passed":true}))},
                Step::EqualCaptures{left,right} => {if captures.get(left).is_none()||captures.get(left)!=captures.get(right){return Err(Error::Conflict)}Ok(json!({"kind":"equal_captures","passed":true}))},
                Step::Load{path,requests,concurrency,status} => {
                    use futures_util::{stream,StreamExt};
                    let start=Instant::now();
                    let outcomes=stream::iter(0..*requests).map(|number| {
                        let client=&client;
                        let endpoint=format!("{}{}",plan.origins[number as usize%plan.origins.len()],path);
                        async move {
                            let start=Instant::now();
                            let result=async {
                                let response=client.get(endpoint).bearer_auth(token).send().await.map_err(|_|Error::Unavailable)?;
                                if response.status().as_u16()!=*status{return Err(Error::Conflict)}
                                response_bytes(response,2*1024*1024).await?;Ok::<(),Error>(())
                            }.await;
                            (result.is_ok(),start.elapsed().as_micros().min(u128::from(u64::MAX)) as u64)
                        }
                    }).buffer_unordered(*concurrency).collect::<Vec<_>>().await;
                    let mut latencies=outcomes.iter().map(|(_,latency)|*latency).collect::<Vec<_>>();latencies.sort_unstable();
                    let failed=outcomes.iter().filter(|(ok,_)|!*ok).count();let seconds=start.elapsed().as_secs_f64();
                    Ok(json!({"kind":"load","requests":requests,"concurrency":concurrency,"failed":failed,"completed_requests_per_second":f64::from(*requests)/seconds.max(0.000001),"p95_latency_us":latencies[(latencies.len()*95/100).min(latencies.len()-1)],"passed":failed==0}))
                },
            }
        }.await;
        match outcome {
            Ok(result) => {
                let ok = result.get("passed") == Some(&Value::Bool(true));
                results.push(result);
                if !ok {
                    passed = false;
                    break;
                }
            }
            Err(error) => {
                results.push(json!({"step":index,"passed":false,"error":error.to_string()}));
                passed = false;
                break;
            }
        }
    }
    let receipt = Receipt {
        schema: "agentforge/gateway-ha-chaos-receipt/v1".into(),
        source_sha256: plan.source_sha256,
        passed,
        steps: results,
        kill9_count: kills,
        elapsed_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        single_instance_assumption_retired: false,
    };
    let bytes = serde_json::to_vec_pretty(&receipt).map_err(|_| Error::Invalid)?;
    use std::io::Write;
    private_log(&output.join("receipt.json"))?
        .write_all(&bytes)
        .map_err(|_| Error::Unavailable)?;
    Ok(receipt)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chaos_never_targets_arbitrary_processes_or_leaks_credentials_to_a_different_origin() {
        let plan = Plan {
            schema: "agentforge/gateway-ha-chaos/v1".into(),
            source_sha256: "a".repeat(64),
            origins: vec![
                "http://127.0.0.1:18081".into(),
                "http://127.0.0.1:18082".into(),
            ],
            credential_file: "/private/synthetic-token".into(),
            ca_file: None,
            steps: vec![Step::Kill9 {
                label: "not-created-by-runner".into(),
            }],
        };
        assert!(plan.validate().is_err());
        for raw in [
            "http://remote.example",
            "https://user:secret@example.test",
            "https://example.test/path",
        ] {
            assert!(origin(raw).is_err());
        }
    }
}
