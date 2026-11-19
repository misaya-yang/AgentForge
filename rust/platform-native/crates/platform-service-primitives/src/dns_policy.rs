//! DNS resolution/address predicate only, without connector OAuth/broker policy.
use std::{future::Future,pin::Pin,net::{IpAddr,ToSocketAddrs}};
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum ConnectorHttpError {RejectedDestination,Transport,SourceUnhandled}
pub type ConnectorDnsFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<IpAddr>, ConnectorHttpError>> + Send + 'a>>;

/// Trusted injected resolver; the adapter, not the resolver, checks every answer.
pub trait ConnectorDnsResolver: Send + Sync {
    fn resolve<'a>(&'a self, hostname: &'a str, port: u16) -> ConnectorDnsFuture<'a>;
}

#[derive(Clone, Copy, Default, Debug)]
pub struct NativeConnectorDnsResolver;

impl ConnectorDnsResolver for NativeConnectorDnsResolver {
    fn resolve<'a>(&'a self, hostname: &'a str, port: u16) -> ConnectorDnsFuture<'a> {
        Box::pin(async move {
            let hostname = hostname.to_owned();
            tokio::task::spawn_blocking(move || {
                (hostname.as_str(), port)
                    .to_socket_addrs()
                    .map(|addresses| addresses.map(|address| address.ip()).collect())
            })
            .await
            .map_err(|_| ConnectorHttpError::RejectedDestination)?
            .map_err(|_| ConnectorHttpError::RejectedDestination)
        })
    }
}

/// Source's explicit unsafe predicate, which is not Python `is_global`.
pub fn oauth_form_safe_address(address: IpAddr) -> bool {
    let address = match address {
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(address),
        _ => address,
    };
    if let IpAddr::V4(ip) = address
        && u32::from(ip) >> 22 == 0x64400000u32 >> 22
    {
        return true; // Source permits shared address space, 100.64.0.0/10.
    }
    mcp_public_address(address)
}


pub fn mcp_public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let n = u32::from(ip);
            if matches!(n, 0xc0000009 | 0xc000000a) {
                return true;
            }
            ![
                (0u32, 8),
                (0x0a000000, 8),
                (0x64400000, 10),
                (0x7f000000, 8),
                (0xa9fe0000, 16),
                (0xac100000, 12),
                (0xc0000000, 24),
                (0xc0000200, 24),
                (0xc0a80000, 16),
                (0xc6120000, 15),
                (0xc6336400, 24),
                (0xcb007100, 24),
                (0xe0000000, 4),
                (0xf0000000, 4),
            ]
            .iter()
            .any(|&(base, bits)| n >> (32 - bits) == base >> (32 - bits))
        }
        IpAddr::V6(ip) => {
            let n = u128::from(ip);
            let has = |base: u128, bits: u32| n >> (128 - bits) == base >> (128 - bits);
            // Python marks addresses outside these non-reserved prefixes as
            // reserved. A global-looking v4 mapping must not bypass that check.
            if n >> 125 != 1 {
                return false;
            } // 2000::/3
            let exception = has((0x20010001u128 << 96) | 1, 128)
                || has((0x20010001u128 << 96) | 2, 128)
                || has(0x20010003u128 << 96, 32)
                || has(0x200100040112u128 << 80, 48)
                || has(0x20010020u128 << 96, 28)
                || has(0x20010030u128 << 96, 28);
            exception
                || ![
                    (0x20010000u128 << 96, 23),
                    (0x20010db8u128 << 96, 32),
                    (0x2002u128 << 112, 16),
                    (0x3fffu128 << 112, 20),
                ]
                .iter()
                .any(|&(base, bits)| has(base, bits))
        }
    }
}
