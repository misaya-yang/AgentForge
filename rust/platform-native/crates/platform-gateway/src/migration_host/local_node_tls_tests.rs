use super::*;
use rustls::pki_types::ServerName;
use std::process::{Command, Stdio};

struct Certificates(PathBuf);
impl Certificates {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("local-node-tls-unit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::write(root.join("ca.conf"),"[req]\ndistinguished_name=dn\nx509_extensions=ca\nprompt=no\n[dn]\nCN=AgentForge ephemeral unit CA\n[ca]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\n").unwrap();
        std::fs::write(
            root.join("server.conf"),
            "[req]\ndistinguished_name=dn\nprompt=no\n[dn]\nCN=localhost\n",
        )
        .unwrap();
        std::fs::write(root.join("server.ext"),"[server]\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:localhost,IP:127.0.0.1\n").unwrap();
        for arguments in [
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
                    .args(arguments)
                    .current_dir(&root)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .unwrap()
                    .success(),
                "ephemeral TLS fixture certificate generation failed"
            );
        }
        Self(root)
    }
    async fn listener(&self) -> GatewayHttpsListener {
        GatewayHttpsListener::bind(&GatewayHttpsConfiguration {
            address: "127.0.0.1:0".parse().unwrap(),
            certificate_path: self.0.join("server.pem"),
            private_key_path: self.0.join("server.key"),
        })
        .await
        .unwrap()
    }
    fn connector(&self) -> tokio_rustls::TlsConnector {
        let mut roots = rustls::RootCertStore::empty();
        for cert in CertificateDer::pem_file_iter(self.0.join("ca.pem")).unwrap() {
            roots.add(cert.unwrap()).unwrap();
        }
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        tokio_rustls::TlsConnector::from(Arc::new(config))
    }
}
impl Drop for Certificates {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn stalled_tcp_peer_does_not_block_a_verified_companion_tls_handshake() {
    let certs = Certificates::new();
    let mut listener = certs.listener().await;
    let address = listener.local_addr().unwrap();
    let stalled = TcpStream::connect(address).await.unwrap();
    let server = tokio::spawn(async move { listener.accept().await });
    let client = certs.connector().connect(
        ServerName::try_from("localhost").unwrap(),
        TcpStream::connect(address).await.unwrap(),
    );
    let client = tokio::time::timeout(Duration::from_secs(2), client)
        .await
        .expect("silent first peer blocked a real verified TLS client")
        .unwrap();
    let (accepted, peer) = tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
    assert!(peer.ip().is_loopback());
    drop((client, accepted, stalled));
}

#[tokio::test]
async fn hostname_verification_failure_does_not_poison_the_listener() {
    let certs = Certificates::new();
    let mut listener = certs.listener().await;
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { listener.accept().await });
    let connector = certs.connector();
    let wrong = connector.connect(
        ServerName::try_from("wrong-host.invalid").unwrap(),
        TcpStream::connect(address).await.unwrap(),
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(2), wrong)
            .await
            .unwrap()
            .is_err()
    );
    let good = connector.connect(
        ServerName::try_from("localhost").unwrap(),
        TcpStream::connect(address).await.unwrap(),
    );
    let good = tokio::time::timeout(Duration::from_secs(2), good)
        .await
        .unwrap()
        .unwrap();
    let accepted = tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
    drop((good, accepted));
}
