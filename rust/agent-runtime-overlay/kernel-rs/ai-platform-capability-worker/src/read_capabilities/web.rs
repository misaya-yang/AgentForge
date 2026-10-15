//! SSRF validation for public web fetches: URL checks, DNS pinning, IP class.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use reqwest::Url;

use super::ReadCapabilityError;

pub(super) async fn validated_public_url(value: &str) -> Result<Url, ReadCapabilityError> {
    let url = Url::parse(value).map_err(|_| ReadCapabilityError::SsrfBlocked)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(ReadCapabilityError::SsrfBlocked);
    }
    let host = url.host_str().ok_or(ReadCapabilityError::SsrfBlocked)?;
    if host.eq_ignore_ascii_case("localhost") {
        return Err(ReadCapabilityError::SsrfBlocked);
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        if !is_public_ip(ip) {
            return Err(ReadCapabilityError::SsrfBlocked);
        }
        return Ok(url);
    }
    let port = url
        .port_or_known_default()
        .ok_or(ReadCapabilityError::SsrfBlocked)?;
    let addresses: Vec<_> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| ReadCapabilityError::DnsFailed)?
        .collect();
    if addresses.is_empty() || addresses.iter().any(|address| !is_public_ip(address.ip())) {
        return Err(ReadCapabilityError::SsrfBlocked);
    }
    Ok(url)
}

pub(super) fn resolved_peer_ip(
    remote: Option<IpAddr>,
    pinned: &[std::net::SocketAddr],
) -> Option<IpAddr> {
    // `resolve_to_addrs` pins the TCP peer. Only when the HTTP stack leaves
    // `remote_addr()` empty do we fall back to the already-validated pin. An
    // *observed* peer is never overridden: if the connection actually landed on
    // a private address, that observation must reach the caller's `SsrfBlocked`
    // branch so a future pinning/proxy regression stays a hard block rather
    // than being masked into a silent public pass.
    match remote {
        Some(ip) => Some(ip),
        None => pinned
            .iter()
            .map(std::net::SocketAddr::ip)
            .find(|ip| is_public_ip(*ip)),
    }
}

pub(super) fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_ipv4(ip),
        IpAddr::V6(ip) => is_public_ipv6(ip),
    }
}

fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, _, _] = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_documentation()
        || a == 0
        || (a == 100 && (64..=127).contains(&b))
        || (a == 192 && b == 0)
        || (a == 198 && matches!(b, 18 | 19))
        || a >= 240)
}

fn is_public_ipv6(ip: Ipv6Addr) -> bool {
    // A v4-mapped (`::ffff:a.b.c.d`), IPv4-compatible (`::a.b.c.d`), or 6to4
    // (`2002:a.b.c.d::`) address can carry a private IPv4 inside a nominally
    // public IPv6 shape, and Linux `connect()` on such a sockaddr reaches the
    // embedded IPv4 host. Classify the mapped form by its embedded v4 and block
    // the compat/6to4 transition ranges outright, or a public domain serving an
    // attacker-chosen AAAA record bypasses every SSRF check (metadata/localhost).
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_ipv4(v4);
    }
    let seg = ip.segments();
    if seg[0] == 0x2002 {
        // 6to4 (2002::/16) — embeds an IPv4 destination; web_fetch never needs it.
        return false;
    }
    if seg[0] == 0 && seg[1] == 0 && seg[2] == 0 && seg[3] == 0 && seg[4] == 0 {
        // ::/96 — the deprecated IPv4-compatible form (and `::`, already covered).
        return false;
    }
    let first = seg[0];
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (first & 0xfe00) == 0xfc00
        || (first & 0xffc0) == 0xfe80
        || (seg[0] == 0x2001 && seg[1] == 0x0db8))
}

pub(super) fn strip_html(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut in_tag = false;
    for character in input.chars() {
        match character {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                output.push(' ');
            }
            _ if !in_tag => output.push(character),
            _ => {}
        }
    }
    output.split_whitespace().collect::<Vec<_>>().join(" ")
}
