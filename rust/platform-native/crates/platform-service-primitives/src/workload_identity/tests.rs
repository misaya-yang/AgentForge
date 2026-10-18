use super::*;
#[test]
fn spiffe_identity_is_exact_canonical_and_not_a_dns_or_header_hint() {
    assert!(ServiceIdentity::parse("spiffe://agentforge.local/ns/platform/sa/runtime").is_ok());
    for raw in [
        "https://agentforge.local/runtime",
        "spiffe://AgentForge.local/runtime",
        "spiffe://agentforge.local:443/runtime",
        "spiffe://user@agentforge.local/runtime",
        "spiffe://agentforge.local/runtime?admin=true",
        "spiffe://agentforge.local/runtime#x",
        "spiffe://agentforge.local/ns/../runtime",
        "spiffe://agentforge.local/ns/%2e%2e/runtime",
        "spiffe://agentforge.local//runtime",
        "spiffe://agentforge.local/",
    ] {
        assert!(
            ServiceIdentity::parse(raw).is_err(),
            "unexpected identity syntax accepted"
        );
    }
    let id = ServiceIdentity::parse("spiffe://agentforge.local/ns/platform/sa/runtime").unwrap();
    let rules = PeerRules::new([id.clone()]).unwrap();
    assert!(rules.admits(&id));
    assert!(!rules.admits(
        &ServiceIdentity::parse("spiffe://agentforge.local/ns/platform/sa/runtime-admin").unwrap()
    ));
}
#[cfg(unix)]
mod certificate_tests {
    use super::super::{
        secrets::{CredentialFiles, environment_secret, read_file},
        tls::{ClientCredentials, ServerCredentials},
    };
    use super::*;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, pem::PemObject};
    use std::{
        collections::BTreeMap,
        fs,
        io::Cursor,
        os::unix::fs::{PermissionsExt, symlink},
        path::{Path, PathBuf},
        process::{Command, Stdio},
        sync::Arc,
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
        fn switch(&self, workload: &str, generation: &str) {
            let base = self.root.join(workload);
            let next = base.join(format!(".next-{}", self.serial));
            symlink(generation, &next).unwrap();
            fs::rename(next, base.join("current")).unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    fn id(name: &str) -> ServiceIdentity {
        ServiceIdentity::parse(&format!("spiffe://agentforge.local/ns/platform/sa/{name}")).unwrap()
    }
    fn server(files: CredentialFiles, mode: Mode) -> ServerCredentials {
        ServerCredentials::new(
            files,
            id("gateway"),
            PeerRules::new([id("runtime")]).unwrap(),
            mode,
        )
        .unwrap()
    }
    fn handshake(
        client: rustls::ClientConfig,
        server: Arc<rustls::ServerConfig>,
        host: &str,
    ) -> std::result::Result<(rustls::ClientConnection, rustls::ServerConnection), ()> {
        let mut client = rustls::ClientConnection::new(
            Arc::new(client),
            ServerName::try_from(host.to_owned()).map_err(|_| ())?,
        )
        .map_err(|_| ())?;
        let mut server = rustls::ServerConnection::new(server).map_err(|_| ())?;
        for _ in 0..32 {
            let mut bytes = Vec::new();
            client.write_tls(&mut bytes).map_err(|_| ())?;
            if !bytes.is_empty() {
                server.read_tls(&mut Cursor::new(bytes)).map_err(|_| ())?;
                server.process_new_packets().map_err(|_| ())?;
            }
            let mut bytes = Vec::new();
            server.write_tls(&mut bytes).map_err(|_| ())?;
            if !bytes.is_empty() {
                client.read_tls(&mut Cursor::new(bytes)).map_err(|_| ())?;
                client.process_new_packets().map_err(|_| ())?;
            }
            if !client.is_handshaking() && !server.is_handshaking() {
                return Ok((client, server));
            }
        }
        Err(())
    }
    fn anonymous_client(files: &CredentialFiles) -> rustls::ClientConfig {
        let generation = files.generation().unwrap();
        let bytes = read_file(&generation.join("ca.pem"), false).unwrap();
        let mut roots = rustls::RootCertStore::empty();
        for cert in CertificateDer::pem_slice_iter(bytes.expose()) {
            roots.add(cert.unwrap()).unwrap();
        }
        rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth()
    }
    #[test]
    fn real_tls_verifies_chain_dns_exact_identity_eku_and_client_key_possession() {
        let mut fixture = Fixture::new();
        let sf = fixture.leaf("gateway", "g1", id("gateway").as_str(), "serverAuth");
        let cf = fixture.leaf("runtime", "g1", id("runtime").as_str(), "clientAuth");
        let service = server(sf.clone(), Mode::Required);
        let client = ClientCredentials::new(cf.clone(), id("runtime"));
        let (_, connection) = handshake(
            client.load(id("gateway")).unwrap(),
            service.load().unwrap(),
            "localhost",
        )
        .unwrap();
        assert_eq!(
            service
                .verified_peer(&connection)
                .unwrap()
                .unwrap()
                .identity(),
            &id("runtime")
        );
        assert!(
            handshake(
                client.load(id("knowledge")).unwrap(),
                service.load().unwrap(),
                "localhost"
            )
            .is_err()
        );
        assert!(
            handshake(
                client.load_with_dns(id("gateway")).unwrap(),
                service.load().unwrap(),
                "wrong-host.invalid"
            )
            .is_err()
        );
        let other = fixture.leaf("other", "g1", id("knowledge").as_str(), "clientAuth");
        assert!(
            handshake(
                ClientCredentials::new(other, id("knowledge"))
                    .load(id("gateway"))
                    .unwrap(),
                service.load().unwrap(),
                "localhost"
            )
            .is_err()
        );
        let wrong_eku = fixture.leaf("wrong-eku", "g1", id("runtime").as_str(), "serverAuth");
        assert!(
            handshake(
                ClientCredentials::new(wrong_eku, id("runtime"))
                    .load(id("gateway"))
                    .unwrap(),
                service.load().unwrap(),
                "localhost"
            )
            .is_err()
        );
        let mut unrelated = Fixture::new();
        let other_client = unrelated.leaf("runtime", "g1", id("runtime").as_str(), "clientAuth");
        fs::copy(
            fixture.root.join("ca.pem"),
            other_client.generation().unwrap().join("ca.pem"),
        )
        .unwrap();
        assert!(
            handshake(
                ClientCredentials::new(other_client, id("runtime"))
                    .load(id("gateway"))
                    .unwrap(),
                service.load().unwrap(),
                "localhost"
            )
            .is_err()
        );
        assert!(handshake(anonymous_client(&sf), service.load().unwrap(), "localhost").is_err());
        #[derive(Debug)]
        struct FutureTime;
        impl rustls::time_provider::TimeProvider for FutureTime {
            fn current_time(&self) -> Option<rustls::pki_types::UnixTime> {
                Some(rustls::pki_types::UnixTime::since_unix_epoch(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        + std::time::Duration::from_secs(3 * 86400),
                ))
            }
        }
        let mut expired = client.load(id("gateway")).unwrap();
        expired.time_provider = Arc::new(FutureTime);
        assert!(handshake(expired, service.load().unwrap(), "localhost").is_err());
    }
    #[test]
    fn standard_uri_only_svid_authenticates_without_a_dns_identity_fallback() {
        let mut fixture = Fixture::new();
        let files = fixture.leaf("gateway", "g1", id("gateway").as_str(), "serverAuth");
        let cf = fixture.leaf("runtime", "g1", id("runtime").as_str(), "clientAuth");
        fs::write(fixture.root.join("gateway/g1/cert.ext"),format!("[leaf]\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\nsubjectAltName=URI:{}\n",id("gateway").as_str())).unwrap();
        fixture.command(&[
            "x509",
            "-req",
            "-in",
            "gateway/g1/cert.csr",
            "-CA",
            "ca.pem",
            "-CAkey",
            "ca.key",
            "-set_serial",
            "9001",
            "-days",
            "1",
            "-out",
            "gateway/g1/cert.pem",
            "-extfile",
            "gateway/g1/cert.ext",
            "-extensions",
            "leaf",
        ]);
        let client = ClientCredentials::new(cf, id("runtime"));
        let service = server(files, Mode::Required);
        assert!(
            handshake(
                client.load(id("gateway")).unwrap(),
                service.load().unwrap(),
                "service.agentforge.svc.cluster.local"
            )
            .is_ok()
        );
        assert!(
            handshake(
                client.load_with_dns(id("gateway")).unwrap(),
                service.load().unwrap(),
                "service.agentforge.svc.cluster.local"
            )
            .is_err()
        );
        assert!(
            handshake(
                client.load(id("knowledge")).unwrap(),
                service.load().unwrap(),
                "service.agentforge.svc.cluster.local"
            )
            .is_err()
        );
    }
    #[test]
    fn dual_accepts_anonymous_tls_but_required_does_not_and_bad_certs_cannot_downgrade() {
        let mut fixture = Fixture::new();
        let sf = fixture.leaf("gateway", "g1", id("gateway").as_str(), "serverAuth");
        let dual = server(sf.clone(), Mode::Dual);
        let (_, connection) =
            handshake(anonymous_client(&sf), dual.load().unwrap(), "localhost").unwrap();
        assert!(dual.verified_peer(&connection).unwrap().is_none());
        let bad = fixture.leaf("bad", "g1", id("knowledge").as_str(), "clientAuth");
        assert!(
            handshake(
                ClientCredentials::new(bad, id("knowledge"))
                    .load(id("gateway"))
                    .unwrap(),
                dual.load().unwrap(),
                "localhost"
            )
            .is_err()
        );
    }
    #[test]
    fn rotation_changes_leaf_reloads_roots_and_invalid_generation_fails_closed() {
        let mut fixture = Fixture::new();
        let sf = fixture.leaf("gateway", "g1", id("gateway").as_str(), "serverAuth");
        let cf = fixture.leaf("runtime", "g1", id("runtime").as_str(), "clientAuth");
        let service = server(sf.clone(), Mode::Required);
        let client = ClientCredentials::new(cf, id("runtime"));
        let (first, _) = handshake(
            client.load(id("gateway")).unwrap(),
            service.load().unwrap(),
            "localhost",
        )
        .unwrap();
        let first = first.peer_certificates().unwrap()[0].as_ref().to_vec();
        fixture.leaf("gateway", "g2", id("gateway").as_str(), "serverAuth");
        fixture.switch("gateway", "g2");
        let (second, _) = handshake(
            client.load(id("gateway")).unwrap(),
            service.load().unwrap(),
            "localhost",
        )
        .unwrap();
        assert_ne!(first, second.peer_certificates().unwrap()[0].as_ref());
        fs::write(
            fixture.root.join("gateway/g2/key.pem"),
            "corrupt rotated fixture",
        )
        .unwrap();
        assert!(service.load().is_err());
        fixture.switch("gateway", "g1");
        assert!(service.load().is_ok());
        let mut replacement = Fixture::new();
        let next_sf = replacement.leaf("gateway", "g1", id("gateway").as_str(), "serverAuth");
        let next_cf = replacement.leaf("runtime", "g1", id("runtime").as_str(), "clientAuth");
        let next = server(next_sf, Mode::Required);
        assert!(
            handshake(
                client.load(id("gateway")).unwrap(),
                next.load().unwrap(),
                "localhost"
            )
            .is_err()
        );
        assert!(
            handshake(
                ClientCredentials::new(next_cf, id("runtime"))
                    .load(id("gateway"))
                    .unwrap(),
                next.load().unwrap(),
                "localhost"
            )
            .is_ok()
        );
        fs::copy(
            replacement.root.join("ca.pem"),
            fixture.root.join("runtime/g1/ca.pem"),
        )
        .unwrap();
        assert!(
            handshake(
                client.load(id("gateway")).unwrap(),
                next.load().unwrap(),
                "localhost"
            )
            .is_err()
        );
    }
    #[test]
    fn private_files_ambiguous_san_mismatched_keys_and_generation_escape_are_rejected() {
        let mut fixture = Fixture::new();
        let files = fixture.leaf("gateway", "g1", id("gateway").as_str(), "serverAuth");
        let service = server(files.clone(), Mode::Required);
        let key = fixture.root.join("gateway/g1/key.pem");
        fs::set_permissions(&key, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(service.load().is_err());
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
        let link = fixture.root.join("key-link");
        symlink(&key, &link).unwrap();
        assert!(read_file(&link, true).is_err());
        assert!(read_file(Path::new("relative"), true).is_err());
        let env = BTreeMap::from([
            ("TOKEN".into(), "inline-private-value".into()),
            ("TOKEN_FILE".into(), key.to_str().unwrap().into()),
        ]);
        assert!(environment_secret(&env, "TOKEN").is_err());
        let ambiguous = fixture.leaf(
            "ambiguous",
            "g1",
            &format!("{},URI:{}", id("gateway").as_str(), id("runtime").as_str()),
            "serverAuth",
        );
        assert!(server(ambiguous, Mode::Required).load().is_err());
        let other = fixture.leaf("other", "g1", id("gateway").as_str(), "serverAuth");
        fs::copy(other.generation().unwrap().join("key.pem"), &key).unwrap();
        assert!(service.load().is_err());
        fs::remove_file(fixture.root.join("gateway/current")).unwrap();
        symlink(std::env::temp_dir(), fixture.root.join("gateway/current")).unwrap();
        assert!(files.generation().is_err());
    }
    #[test]
    fn certificate_der_parser_rejects_non_spiffe_san_despite_valid_ca_signature() {
        let mut fixture = Fixture::new();
        let files = fixture.leaf(
            "bad",
            "g1",
            "https://agentforge.local/runtime",
            "clientAuth",
        );
        let certificate = CertificateDer::from_pem_slice(
            read_file(&files.generation().unwrap().join("cert.pem"), false)
                .unwrap()
                .expose(),
        )
        .unwrap();
        assert!(super::super::der::identity(certificate.as_ref()).is_err());
        let _ = PrivateKeyDer::from_pem_slice(
            read_file(&files.generation().unwrap().join("key.pem"), true)
                .unwrap()
                .expose(),
        )
        .unwrap();
    }
}
