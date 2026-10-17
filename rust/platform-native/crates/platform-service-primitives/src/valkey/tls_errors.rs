//! Typed transport policy; never classify certificate failures by Display.
use super::ValkeyError;
use std::{error::Error, io, sync::Arc};
#[derive(Default)]
struct Causes {
    tls: bool,
    network: bool,
}
fn causes(error: &(dyn Error + 'static), depth: usize, out: &mut Causes) {
    // Bounded traversal also handles hostile/cyclic custom error sources.
    if depth >= 16 {
        return;
    }
    if error.is::<rustls::Error>() || error.is::<rustls::pki_types::InvalidDnsNameError>() {
        out.tls = true;
        return;
    }
    // redis1.7 stores its Internal error in Arc<dyn Error+Send+Sync> and returns
    // &Arc as source. Arc::source forwards to the inner source, skipping the
    // inner error itself; ordinary source traversal alone can lose rustls::Error.
    if let Some(inner) = error.downcast_ref::<Arc<dyn Error + Send + Sync>>() {
        causes(inner.as_ref(), depth + 1, out);
    }
    if let Some(inner) = error.downcast_ref::<io::Error>() {
        if let Some(custom) = inner.get_ref() {
            causes(custom, depth + 1, out);
        }
        out.network |= matches!(
            inner.kind(),
            io::ErrorKind::ConnectionRefused
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::NotConnected
                | io::ErrorKind::TimedOut
                | io::ErrorKind::WouldBlock
                | io::ErrorKind::UnexpectedEof
                | io::ErrorKind::BrokenPipe
                | io::ErrorKind::AddrNotAvailable
                | io::ErrorKind::NetworkDown
                | io::ErrorKind::NetworkUnreachable
                | io::ErrorKind::HostUnreachable
        );
    }
    if let Some(source) = error.source() {
        causes(source, depth + 1, out);
    }
}
/// Network outage/timeouts and explicit cache OOM allow acceleration bypass. Unknown
/// TLS/protocol/configuration failures are conservatively fatal ServerPolicy.
pub fn classify(error: &redis::RedisError) -> ValkeyError {
    let mut chain = Causes::default();
    causes(error, 0, &mut chain);
    if chain.tls {
        return ValkeyError::ServerPolicy;
    }
    if error.kind() == redis::ErrorKind::AuthenticationFailed
        || matches!(
            error.kind(),
            redis::ErrorKind::Server(redis::ServerErrorKind::NoPerm)
        )
        || matches!(error.code(), Some("NOAUTH" | "WRONGPASS"))
    {
        return ValkeyError::Authentication;
    }
    // OOM is an explicit RESP server error code, not an error-message heuristic.
    if error.code() == Some("OOM") {
        return ValkeyError::Pressure;
    }
    if error.kind() == redis::ErrorKind::InvalidClientConfig {
        return ValkeyError::Configuration;
    }
    if error.is_timeout() {
        return ValkeyError::Timeout;
    }
    if chain.network
        || matches!(
            error.kind(),
            redis::ErrorKind::Server(
                redis::ServerErrorKind::BusyLoading
                    | redis::ServerErrorKind::TryAgain
                    | redis::ServerErrorKind::ClusterDown
                    | redis::ServerErrorKind::MasterDown
            )
        )
    {
        return ValkeyError::Transport;
    }
    ValkeyError::ServerPolicy
}
pub fn cache_failure_is_optional(error: ValkeyError) -> bool {
    matches!(
        error,
        ValkeyError::Transport | ValkeyError::Timeout | ValkeyError::Pressure
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    fn via_io(error: rustls::Error) -> redis::RedisError {
        io::Error::new(io::ErrorKind::InvalidData, error).into()
    }
    #[test]
    fn unknown_ca_is_fatal_through_real_redis_io_arc_chain() {
        let error = via_io(rustls::Error::InvalidCertificate(
            rustls::CertificateError::UnknownIssuer,
        ));
        assert_eq!(classify(&error), ValkeyError::ServerPolicy);
        assert_eq!(classify(&error.clone()), ValkeyError::ServerPolicy);
        assert!(!cache_failure_is_optional(classify(&error)));
    }
    #[test]
    #[allow(deprecated)]
    fn wrong_san_and_direct_tls_policy_are_fatal() {
        for error in [
            via_io(rustls::Error::InvalidCertificate(
                rustls::CertificateError::NotValidForName,
            )),
            redis::RedisError::from(rustls::Error::NoCertificatesPresented),
        ] {
            assert_eq!(classify(&error), ValkeyError::ServerPolicy);
        }
    }
    #[test]
    fn dns_name_policy_error_is_not_an_outage() {
        let bad = rustls::pki_types::ServerName::try_from("not a hostname").unwrap_err();
        assert_eq!(
            classify(&redis::RedisError::from(bad)),
            ValkeyError::ServerPolicy
        );
    }
    #[test]
    fn boxed_tls_cause_overrides_even_outer_timeout() {
        let tls: Arc<dyn Error + Send + Sync> = Arc::new(rustls::Error::InvalidCertificate(
            rustls::CertificateError::UnknownIssuer,
        ));
        let io: redis::RedisError = io::Error::new(io::ErrorKind::TimedOut, tls).into();
        assert_eq!(classify(&io), ValkeyError::ServerPolicy);
    }
    #[test]
    fn network_errors_remain_optional_without_string_matching() {
        for kind in [
            io::ErrorKind::ConnectionRefused,
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::UnexpectedEof,
        ] {
            let error = redis::RedisError::from(io::Error::new(
                kind,
                "TLS certificate hostname error text must not affect classification",
            ));
            assert_eq!(classify(&error), ValkeyError::Transport);
            assert!(cache_failure_is_optional(classify(&error)));
        }
        assert_eq!(
            classify(&redis::RedisError::from(io::Error::from(
                io::ErrorKind::TimedOut
            ))),
            ValkeyError::Timeout
        );
    }
    #[test]
    fn opaque_io_and_protocol_failures_fail_closed() {
        for error in [
            redis::RedisError::from(io::Error::new(io::ErrorKind::InvalidData, "opaque")),
            redis::RedisError::from(io::Error::other("opaque DNS or TLS cause")),
            redis::RedisError::from((
                redis::ErrorKind::UnexpectedReturnType,
                "unexpected response",
            )),
        ] {
            assert_eq!(classify(&error), ValkeyError::ServerPolicy);
        }
    }
    #[test]
    fn only_outage_and_timeout_may_bypass_cache() {
        for e in [
            ValkeyError::Configuration,
            ValkeyError::CredentialFile,
            ValkeyError::Authentication,
            ValkeyError::ServerPolicy,
            ValkeyError::CacheScope,
        ] {
            assert!(!cache_failure_is_optional(e));
        }
        assert!(cache_failure_is_optional(ValkeyError::Transport));
        assert!(cache_failure_is_optional(ValkeyError::Timeout));
        // Only the optional cache uses this policy. Coordination propagates OOM
        // directly and cannot issue a session, rate or lease admission on error.
        assert!(cache_failure_is_optional(ValkeyError::Pressure));
    }
}
