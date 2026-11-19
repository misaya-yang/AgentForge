//! Trusted-proxy client-IP resolution, migrated from `src/core/client_ip.py`.
//!
//! Peer identity is established before examining forwarding headers. Trust is
//! explicit, address-family specific, and traversed nearest-to-farthest in XFF.
//! Default construction never consults the environment; only `from_environment`
//! opts into the existing environment-variable contract. This module changes no
//! network configuration and does not install middleware or trust any proxy.
use axum::http::HeaderMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

pub const DEFAULT_TRUSTED_PROXY_CIDRS: [&str; 2] = ["127.0.0.0/8", "::1/128"];

#[derive(Clone, Debug)]
enum Network {
    V4 { address: u32, mask: u32 },
    V6 { address: u128, mask: u128 },
}

/// Parsed immutable configuration. An explicitly empty or entirely malformed
/// configuration trusts nobody; it never falls back to loopback implicitly.
#[derive(Clone, Debug)]
pub struct TrustedProxies {
    networks: Vec<Network>,
}
impl Default for TrustedProxies {
    fn default() -> Self {
        Self::from_cidrs(DEFAULT_TRUSTED_PROXY_CIDRS)
    }
}
impl TrustedProxies {
    /// Comma-separated CIDRs or individual addresses. Invalid entries are ignored,
    /// matching Python ip_network(..., strict=False), including masked-off host bits.
    pub fn parse(raw: &str) -> Self {
        Self::from_cidrs([raw])
    }

    pub fn from_cidrs<'a>(cidrs: impl IntoIterator<Item = &'a str>) -> Self {
        Self {
            networks: cidrs
                .into_iter()
                .flat_map(|value| value.split(','))
                .filter_map(|value| Network::parse(python_strip(value)))
                .collect(),
        }
    }

    /// Explicit opt-in only. Missing/blank environment configuration uses the
    /// Python default; a nonempty malformed value still produces an empty allowlist.
    pub fn from_environment() -> Self {
        let configured = std::env::var_os("GATEWAY_TRUSTED_PROXY_CIDRS");
        Self::from_environment_value(
            configured
                .as_deref()
                .map(|value| value.to_string_lossy())
                .as_deref(),
        )
    }

    fn from_environment_value(configured: Option<&str>) -> Self {
        match configured
            .map(python_strip)
            .filter(|value| !value.is_empty())
        {
            Some(value) => Self::parse(value),
            None => Self::default(),
        }
    }

    pub fn contains(&self, address: IpAddr) -> bool {
        self.networks
            .iter()
            .any(|network| match (network, address) {
                (
                    Network::V4 {
                        address: network,
                        mask,
                    },
                    IpAddr::V4(address),
                ) => u32::from(address) & mask == *network,
                (
                    Network::V6 {
                        address: network,
                        mask,
                    },
                    IpAddr::V6(address),
                ) => u128::from(address) & mask == *network,
                _ => false,
            })
    }
}
impl Network {
    fn parse(value: &str) -> Option<Self> {
        let (address, prefix) = match value.split_once('/') {
            Some((address, prefix)) => (address, Some(prefix)),
            None => (value, None),
        };
        // Do not trim the address/prefix independently: whitespace beside the
        // slash is not accepted by Python's network parser.
        match ParsedIp::strict(address)?.address {
            IpAddr::V4(address) => {
                let length = match prefix {
                    None => 32,
                    Some(prefix) => decimal_prefix(prefix, 32).or_else(|| dotted_prefix(prefix))?,
                };
                let mask = mask4(length);
                Some(Self::V4 {
                    address: u32::from(address) & mask,
                    mask,
                })
            }
            IpAddr::V6(address) => {
                let length = prefix.map_or(Some(128), |prefix| decimal_prefix(prefix, 128))?;
                let mask = if length == 0 {
                    0
                } else {
                    u128::MAX << (128 - length)
                };
                Some(Self::V6 {
                    address: u128::from(address) & mask,
                    mask,
                })
            }
        }
    }
}
fn mask4(length: u32) -> u32 {
    if length == 0 {
        0
    } else {
        u32::MAX << (32 - length)
    }
}
fn decimal_prefix(value: &str, maximum: u32) -> Option<u32> {
    // Python 3.12's default integer-string limit is 4300 digits. Reject longer
    // prefixes rather than accept a network the source may have discarded.
    if value.is_empty() || value.len() > 4300 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.bytes().try_fold(0_u32, |prefix, byte| {
        prefix
            .checked_mul(10)?
            .checked_add(u32::from(byte - b'0'))
            .filter(|value| *value <= maximum)
    })
}
fn dotted_prefix(value: &str) -> Option<u32> {
    let mask = u32::from(value.parse::<Ipv4Addr>().ok()?);
    let prefix = mask.leading_ones();
    // All-zero/all-one masks are netmasks, not hostmasks, as in ipaddress.
    if mask == mask4(prefix) {
        return Some(prefix);
    }
    let inverted = !mask;
    let prefix = inverted.leading_ones();
    (inverted == mask4(prefix)).then_some(prefix)
}

#[derive(Debug)]
struct ParsedIp {
    address: IpAddr,
    scope: Option<String>,
}
impl ParsedIp {
    fn parse(value: &str) -> Option<Self> {
        Self::strict(python_strip(value))
    }
    fn strict(value: &str) -> Option<Self> {
        if value.contains('/') {
            return None;
        }
        let (address, scope) = match value.split_once('%') {
            Some((address, scope)) if !scope.is_empty() && !scope.contains('%') => {
                (address, Some(scope.to_owned()))
            }
            Some(_) => return None,
            None => (value, None),
        };
        let address: IpAddr = address.parse().ok()?;
        if scope.is_some() && !address.is_ipv6() {
            return None;
        }
        Some(Self { address, scope })
    }
    fn canonical(&self) -> String {
        let mut value = match self.address {
            IpAddr::V4(address) => address.to_string(),
            IpAddr::V6(address) => canonical_v6(address),
        };
        if let Some(scope) = &self.scope {
            value.push('%');
            value.push_str(scope);
        }
        value
    }
}

/// Deterministic Python 3.12.14 representation, independent of Rust Display
/// changes: only IPv4-mapped IPv6 uses dotted decimal; compatible IPv6 stays hex.
fn canonical_v6(address: Ipv6Addr) -> String {
    let segments = address.segments();
    if segments[..5] == [0; 5] && segments[5] == 0xffff {
        return format!(
            "::ffff:{}",
            Ipv4Addr::from((u32::from(segments[6]) << 16) | u32::from(segments[7]))
        );
    }
    let mut best_start = 0;
    let mut best_length = 0;
    let mut index = 0;
    while index < 8 {
        if segments[index] != 0 {
            index += 1;
            continue;
        }
        let start = index;
        while index < 8 && segments[index] == 0 {
            index += 1;
        }
        if index - start > best_length {
            best_start = start;
            best_length = index - start;
        }
    }
    let join = |values: &[u16]| {
        values
            .iter()
            .map(|value| format!("{value:x}"))
            .collect::<Vec<_>>()
            .join(":")
    };
    if best_length < 2 {
        return join(&segments);
    }
    format!(
        "{}::{}",
        join(&segments[..best_start]),
        join(&segments[best_start + best_length..])
    )
}
fn python_strip(value: &str) -> &str {
    value.trim_matches(|character: char| {
        character.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&character)
    })
}
fn latin1(value: &[u8]) -> String {
    value.iter().map(|byte| char::from(*byte)).collect()
}

/// HeaderMap keeps HTTP header-name case-insensitivity and first-value semantics.
/// Header values use Latin-1 decoding, matching Starlette/Python, not UTF-8 lossiness.
/// None selects the built-in loopback allowlist without reading the environment.
pub fn get_client_ip(
    headers: &HeaderMap,
    direct_client_host: Option<&str>,
    trusted: Option<&TrustedProxies>,
) -> String {
    resolve(
        |name| {
            headers
                .get(name)
                .map(|value| latin1(value.as_bytes()))
                .unwrap_or_default()
        },
        direct_client_host,
        trusted,
    )
}

/// ASGI's dict(scope["headers"]) keeps the last value for an exact duplicate
/// byte-name, while retaining the first key's position for case-insensitive lookup.
/// This adapter preserves that subtle difference from HeaderMap/Request handling.
pub fn get_client_ip_from_scope_headers(
    headers: &[(&[u8], &[u8])],
    direct_client_host: Option<&str>,
    trusted: Option<&TrustedProxies>,
) -> String {
    resolve(
        |name| {
            headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name.as_bytes()))
                .and_then(|(key, _)| headers.iter().rev().find(|(candidate, _)| candidate == key))
                .map(|(_, value)| latin1(value))
                .unwrap_or_default()
        },
        direct_client_host,
        trusted,
    )
}
fn resolve(
    header: impl Fn(&str) -> String,
    direct_client_host: Option<&str>,
    trusted: Option<&TrustedProxies>,
) -> String {
    let Some(direct) = direct_client_host.and_then(ParsedIp::parse) else {
        return direct_client_host
            .filter(|value| !value.is_empty())
            .unwrap_or("unknown")
            .to_owned();
    };
    let defaults = TrustedProxies::default();
    let trusted = trusted.unwrap_or(&defaults);
    if !trusted.contains(direct.address) {
        return direct.canonical();
    }
    let forwarded = header("x-forwarded-for");
    let mut leftmost = None;
    for address in forwarded.rsplit(',').filter_map(ParsedIp::parse) {
        if !trusted.contains(address.address) {
            return address.canonical();
        }
        leftmost = Some(address);
    }
    if let Some(address) = leftmost {
        return address.canonical();
    }
    if let Some(address) = ParsedIp::parse(&header("x-real-ip")) {
        return address.canonical();
    }
    direct.canonical()
}

