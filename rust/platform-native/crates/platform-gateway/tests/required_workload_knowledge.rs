//! Production Gateway private dispatch over ephemeral mTLS plus the actual
//! Knowledge v2 domain signature and actor middleware. No database or service.
#![cfg(unix)]
use ai_platform_auth::workload::{CredentialKind, WorkloadAuth, WorkloadAuthentication};
use ai_platform_gateway::{
    knowledge::{KnowledgeActor, KnowledgeConfig, KnowledgeService},
    migration_host::bootstrap::workload::configure_outbound,
};
use ai_platform_service_primitives::{
    knowledge_auth::{GatewayAuth, ReplayStore, verify_gateway},
    knowledge_types::UserContext,
    workload_identity::{
        Mode, PeerRules, ServiceIdentity,
        listener::{WorkloadConnection, WorkloadListener, install_peer},
        secrets::CredentialFiles,
        tls::ServerCredentials,
    },
};
use axum::{Json, Router, extract::Request, middleware, routing::get};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
struct Fixture {
    root: PathBuf,
    serial: u64,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("workload-mtls-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(root.join("ca.cnf"),"[req]\ndistinguished_name=dn\nx509_extensions=ca\nprompt=no\n[dn]\nCN=Ephemeral AgentForge workload test CA\n[ca]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\n").unwrap();
        let mut fixture = Self { root, serial: 1 };
        fixture.command(&[
            "req",
            "-x509",
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:P-256",
            "-nodes",
            "-days",
            "2",
            "-keyout",
            "ca.key",
            "-out",
            "ca.pem",
            "-config",
            "ca.cnf",
        ]);
        fs::set_permissions(
            fixture.root.join("ca.key"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        fixture.serial += 1;
        fixture
    }
    fn command(&self, args: &[&str]) {
        assert!(
            Command::new("openssl")
                .args(args)
                .current_dir(&self.root)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .expect("openssl required for ephemeral TLS fixtures")
                .success(),
            "ephemeral certificate command failed"
        );
    }
    fn leaf(
        &mut self,
        workload: &str,
        generation: &str,
        identity: &str,
        usage: &str,
    ) -> CredentialFiles {
        let base = self.root.join(workload);
        let directory = base.join(generation);
        fs::create_dir_all(&directory).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        let prefix = format!("{workload}/{generation}");
        let key = format!("{prefix}/key.pem");
        let cert = format!("{prefix}/cert.pem");
        let csr = format!("{prefix}/cert.csr");
        let ext = format!("{prefix}/cert.ext");
        fs::write(self.root.join(&ext),format!("[leaf]\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage={usage}\nsubjectAltName=DNS:localhost,IP:127.0.0.1,URI:{identity}\n")).unwrap();
        self.command(&[
            "req",
            "-new",
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:P-256",
            "-nodes",
            "-keyout",
            &key,
            "-out",
            &csr,
            "-subj",
            "/CN=synthetic workload fixture",
        ]);
        self.command(&[
            "x509",
            "-req",
            "-in",
            &csr,
            "-CA",
            "ca.pem",
            "-CAkey",
            "ca.key",
            "-set_serial",
            &self.serial.to_string(),
            "-days",
            "1",
            "-out",
            &cert,
            "-extfile",
            &ext,
            "-extensions",
            "leaf",
        ]);
        self.serial += 1;
        fs::set_permissions(self.root.join(&key), fs::Permissions::from_mode(0o600)).unwrap();
        fs::copy(self.root.join("ca.pem"), directory.join("ca.pem")).unwrap();
        if !base.join("current").exists() {
            symlink(generation, base.join("current")).unwrap();
        }
        CredentialFiles::new(base).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn id(name: &str) -> ServiceIdentity {
    ServiceIdentity::parse(&format!("spiffe://agentforge.local/service/{name}")).unwrap()
}
#[tokio::test]
async fn gateway_private_dispatch_requires_both_pinned_workload_peer_and_signed_actor() {
    tokio::time::timeout(Duration::from_secs(25), async {
        let mut fixture = Fixture::new();
        let client = fixture.leaf("gateway", "g1", id("gateway").as_str(), "clientAuth");
        let server = fixture.leaf("knowledge", "g1", id("knowledge").as_str(), "serverAuth");
        let other = fixture.leaf("other", "g1", id("other").as_str(), "serverAuth");
        // Reserve an ephemeral private port; the production listener intentionally
        // rejects port zero so startup cannot accidentally choose an unknown port.
        let reserve = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reserve.local_addr().unwrap(); drop(reserve);
        let origin = format!("https://localhost:{}", address.port());
        let reserve = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let other_address = reserve.local_addr().unwrap(); drop(reserve);
        let other_origin = format!("https://localhost:{}", other_address.port());
        let env = BTreeMap::from([
            ("AI_PLATFORM_WORKLOAD_MTLS_MODE".into(), "required".into()),
            ("AI_PLATFORM_WORKLOAD_CREDENTIALS_DIR".into(), fixture.root.join("gateway").to_str().unwrap().into()),
            ("AI_PLATFORM_WORKLOAD_SPIFFE_ID".into(), id("gateway").as_str().into()),
            ("AI_PLATFORM_WORKLOAD_PEERS_JSON".into(), serde_json::json!({origin.clone():id("knowledge").as_str(), other_origin.clone():id("knowledge").as_str()}).to_string()),
        ]);
        configure_outbound(&env).unwrap();
        let _ = client; // Files are loaded by the real configured outbound adapter.
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let auth = Arc::new(GatewayAuth::signed_v2(BTreeMap::from([("domain".into(), "independent-knowledge-signing-key".into())]), ReplayStore::memory()).unwrap());
        let router = Router::new().route("/api/v1/knowledge/datasets", get(move |request: Request| { let calls=observed.clone(); async move {
            let transport = WorkloadAuthentication::require(request.extensions()).unwrap();
            assert_eq!(transport.kind(), CredentialKind::MutualTls);
            assert_eq!(transport.identity(), Some(&id("gateway")));
            assert!(!request.headers().contains_key("authorization"));
            assert!(!request.headers().contains_key("x-ai-platform-internal-token"));
            let actor = request.extensions().get::<UserContext>().unwrap();
            assert_eq!(actor.tenant_id, "tenant-one"); assert_eq!(actor.user_id, "guest_one");
            calls.fetch_add(1, Ordering::SeqCst);
            Json(serde_json::json!({"datasets":[]}))
        }})).layer(middleware::from_fn_with_state(auth, verify_gateway));
        let gate = Arc::new(WorkloadAuth::from_configuration(Mode::Required, None, PeerRules::new([id("gateway")]).unwrap()).unwrap());
        let router = gate.protect(router).layer(middleware::from_fn(install_peer));
        let listener = WorkloadListener::bind(address, ServerCredentials::new(server, id("knowledge"), PeerRules::new([id("gateway")]).unwrap(), Mode::Required).unwrap()).await.unwrap();
        let app = router.clone();
        let task = tokio::spawn(async move { axum::serve(listener, app.into_make_service_with_connect_info::<WorkloadConnection>()).await.unwrap(); });
        let listener = WorkloadListener::bind(other_address, ServerCredentials::new(other, id("other"), PeerRules::new([id("gateway")]).unwrap(), Mode::Required).unwrap()).await.unwrap();
        let other_task = tokio::spawn(async move { axum::serve(listener, router.into_make_service_with_connect_info::<WorkloadConnection>()).await.unwrap(); });
        let actor = KnowledgeActor::anonymous("tenant-one".into(), "guest_one".into()).unwrap();
        let service = |origin: &str, secret: &str| KnowledgeService::new(KnowledgeConfig::new(origin, "domain".into(), secret.into()).unwrap()).unwrap();
        assert!(service(&origin, "independent-knowledge-signing-key").visible_dataset_ids(&actor).await.unwrap().is_empty());
        assert!(service(&origin, "wrong-independent-signing-key").visible_dataset_ids(&actor).await.is_err());
        assert!(service(&other_origin, "independent-knowledge-signing-key").visible_dataset_ids(&actor).await.is_err());
        let plaintext = origin.replacen("https://localhost", "http://127.0.0.1", 1);
        assert!(service(&plaintext, "independent-knowledge-signing-key").visible_dataset_ids(&actor).await.is_err());
        let roots = reqwest::Certificate::from_pem(&fs::read(fixture.root.join("ca.pem")).unwrap()).unwrap();
        let anonymous = reqwest::Client::builder().no_proxy().add_root_certificate(roots).timeout(Duration::from_secs(3)).build().unwrap();
        assert!(anonymous.get(format!("{origin}/api/v1/knowledge/datasets")).header("x-spiffe-id", id("gateway").as_str()).send().await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        task.abort(); other_task.abort();
        let _ = task.await; let _ = other_task.await;
    }).await.expect("bounded required workload integration test");
}
