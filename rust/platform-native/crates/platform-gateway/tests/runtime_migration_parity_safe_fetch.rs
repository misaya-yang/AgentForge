//! Source safe-fetch rejection assertions. Every fetch terminates before network I/O.
use ai_platform_gateway::{
    connector_oauth_http::{ConnectorDnsFuture, ConnectorDnsResolver, oauth_form_safe_address},
    connector_user_types::ConnectorHttpError,
    migration_content::{image_adapters::PinnedImageFetch, images::ImageReferenceFetch},
};
use std::{
    net::IpAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
struct Resolver {
    answers: Option<Vec<IpAddr>>,
    calls: AtomicUsize,
}
impl ConnectorDnsResolver for Resolver {
    fn resolve<'a>(&'a self, _: &'a str, _: u16) -> ConnectorDnsFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.answers
                .clone()
                .ok_or(ConnectorHttpError::RejectedDestination)
        })
    }
}
#[test]
fn all_original_unsafe_and_public_ip_rows() {
    for address in [
        "127.0.0.1",
        "127.0.0.5",
        "10.0.0.1",
        "10.255.255.255",
        "192.168.1.1",
        "172.16.0.1",
        "172.31.255.255",
        "169.254.169.254",
        "0.0.0.0",
        "::1",
        "fe80::1",
        "fc00::1",
        "::ffff:127.0.0.1",
        "::ffff:10.0.0.1",
        "::ffff:169.254.169.254",
        "garbage",
    ] {
        assert!(
            !address.parse().ok().is_some_and(oauth_form_safe_address),
            "{address}"
        );
    }
    for address in ["8.8.8.8", "1.1.1.1", "8.8.8.8", "2001:4860:4860::8888"] {
        assert!(
            oauth_form_safe_address(address.parse().unwrap()),
            "{address}"
        );
    }
}
#[tokio::test]
async fn mixed_private_empty_and_failed_dns_reject_before_http() {
    for answers in [
        Some(vec![
            "8.8.8.8".parse().unwrap(),
            "10.0.0.1".parse().unwrap(),
        ]),
        Some(vec![]),
        None,
    ] {
        let resolver = Arc::new(Resolver {
            answers,
            calls: AtomicUsize::new(0),
        });
        let fetch = PinnedImageFetch {
            resolver: resolver.clone(),
            ..Default::default()
        };
        assert!(
            fetch
                .fetch("https://fixture.invalid/image", 100, Duration::from_secs(1))
                .await
                .is_err()
        );
        assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn allowed_host_exact_subdomain_case_and_overlap_match_source() {
    for (host, allowed) in [
        ("amazonaws.com", true),
        ("bucket.s3.amazonaws.com", true),
        ("S3.AmazonAWS.COM", true),
        ("evilamazonaws.com", false),
    ] {
        let resolver = Arc::new(Resolver {
            answers: Some(vec!["10.0.0.1".parse().unwrap()]),
            calls: AtomicUsize::new(0),
        });
        let fetch = PinnedImageFetch {
            resolver: resolver.clone(),
            allowed_hosts: Some(["amazonaws.com".into()].into()),
            ..Default::default()
        };
        assert!(
            fetch
                .fetch(
                    &format!("https://{host}/image"),
                    100,
                    Duration::from_secs(1)
                )
                .await
                .is_err()
        );
        assert_eq!(
            resolver.calls.load(Ordering::SeqCst),
            usize::from(allowed),
            "host allowlist must precede DNS; allowed host still fails private-IP guard"
        );
    }
}
#[tokio::test]
async fn non_http_scheme_and_private_literal_reject_without_dns() {
    let resolver = Arc::new(Resolver {
        answers: None,
        calls: AtomicUsize::new(0),
    });
    let fetch = PinnedImageFetch {
        resolver: resolver.clone(),
        ..Default::default()
    };
    for url in [
        "file:///etc/passwd",
        "ftp://fixture.invalid/image",
        "http://127.0.0.1/image",
        "http://169.254.169.254/image",
        "http://[::1]/image",
    ] {
        assert!(fetch.fetch(url, 100, Duration::from_secs(1)).await.is_err());
    }
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 0);
}
