use super::*;
use std::process::{Command, Stdio};
struct Certificates(PathBuf);
impl Certificates {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("sdk-worker-tls-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::write(root.join("ca.conf"),"[req]\ndistinguished_name=dn\nx509_extensions=ca\nprompt=no\n[dn]\nCN=AgentForge ephemeral SDK unit CA\n[ca]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\n").unwrap();
        std::fs::write(
            root.join("server.conf"),
            "[req]\ndistinguished_name=dn\nprompt=no\n[dn]\nCN=sdk-agent-worker\n",
        )
        .unwrap();
        std::fs::write(root.join("server.ext"),"[server]\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:sdk-agent-worker\n").unwrap();
        for args in [
            vec![
                "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-keyout", "ca.key",
                "-out", "ca.pem", "-config", "ca.conf",
            ],
            vec![
                "req",
                "-new",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-keyout",
                "server.key",
                "-out",
                "server.csr",
                "-config",
                "server.conf",
            ],
            vec![
                "x509",
                "-req",
                "-in",
                "server.csr",
                "-CA",
                "ca.pem",
                "-CAkey",
                "ca.key",
                "-CAcreateserial",
                "-days",
                "1",
                "-out",
                "server.pem",
                "-extfile",
                "server.ext",
                "-extensions",
                "server",
            ],
        ] {
            assert!(
                Command::new("openssl")
                    .args(args)
                    .current_dir(&root)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .unwrap()
                    .success(),
                "ephemeral certificate fixture creation failed"
            );
        }
        Self(root.canonicalize().unwrap())
    }
    fn config(&self) -> Transport {
        Transport {
            address: "127.0.0.1:0".parse().unwrap(),
            certificate: Some(self.0.join("server.pem")),
            key: Some(self.0.join("server.key")),
            ca: Some(self.0.join("ca.pem")),
        }
    }
}
impl Drop for Certificates {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[tokio::test]
async fn actual_tls_accepts_trusted_dns_and_rejects_wrong_name_untrusted_and_plaintext() {
    let certs = Certificates::new();
    let mut config = certs.config();
    let listener = config.bind().await.unwrap();
    config.address = listener.local_addr().unwrap();
    let router = axum::Router::new().route(
        "/health/ready",
        axum::routing::get(|| async { axum::Json(serde_json::json!({"ready":true})) }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let (client, url) = config.health().unwrap();
    assert!(client.get(url).send().await.unwrap().status().is_success());
    let untrusted = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .resolve("sdk-agent-worker", config.address)
        .build()
        .unwrap();
    assert!(
        untrusted
            .get(format!(
                "https://sdk-agent-worker:{}/health/ready",
                config.address.port()
            ))
            .send()
            .await
            .is_err()
    );
    let root =
        reqwest::Certificate::from_pem(&file(&certs.0.join("ca.pem"), false).unwrap()).unwrap();
    let wrong_name = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .add_root_certificate(root)
        .resolve("wrong-worker", config.address)
        .build()
        .unwrap();
    assert!(
        wrong_name
            .get(format!(
                "https://wrong-worker:{}/health/ready",
                config.address.port()
            ))
            .send()
            .await
            .is_err()
    );
    assert!(
        reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap()
            .get(format!("http://{}/health/ready", config.address))
            .send()
            .await
            .is_err()
    );
    server.abort();
    let _ = server.await;
}
#[test]
fn bad_ca_key_pair_and_key_permissions_fail_before_binding() {
    let certs = Certificates::new();
    let mut config = certs.config();
    assert!(config.tls().is_ok());
    config.key = Some(certs.0.join("ca.key"));
    assert!(config.tls().is_err());
    config = certs.config();
    let bad = certs.0.join("malformed.pem");
    std::fs::write(&bad, "not a certificate").unwrap();
    config.ca = Some(bad);
    assert!(config.tls().is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        config = certs.config();
        std::fs::set_permissions(
            config.key.as_ref().unwrap(),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(config.tls().is_err());
        let symlink = certs.0.join("linked.pem");
        std::os::unix::fs::symlink(certs.0.join("ca.pem"), &symlink).unwrap();
        assert!(file(&symlink, false).is_err());
    }
}
