//! Exact cgroup usage from the selected Docker engine. CLI MemPerc uses each
//! container's limit and is deliberately never used for daemon capacity math.
use super::{Result, command, docker, ensure, strings};
use std::{io::Write, process::Stdio};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Snapshot {
    pub total: u64,
    pub used: u64,
    pub available: u64,
    pub containers: usize,
}
fn white(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}
fn string_end(bytes: &[u8], mut i: usize) -> Result<usize> {
    ensure(bytes.get(i) == Some(&b'"'), "engine JSON string invalid")?;
    i += 1;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            return Ok(i + 1);
        }
        if bytes[i] == b'\\' {
            i += 1;
            ensure(i < bytes.len(), "engine JSON escape invalid")?;
        }
        i += 1;
    }
    Err("engine JSON string incomplete".into())
}
fn value_end(bytes: &[u8], i: usize, depth: usize) -> Result<usize> {
    ensure(depth < 64 && i < bytes.len(), "engine JSON bound exceeded")?;
    match bytes[i] {
        b'"' => string_end(bytes, i),
        b'{' | b'[' => {
            let close = if bytes[i] == b'{' { b'}' } else { b']' };
            let mut pos = i + 1;
            while pos < bytes.len() {
                pos = white(bytes, pos);
                ensure(pos < bytes.len(), "engine JSON collection incomplete")?;
                if bytes.get(pos) == Some(&close) {
                    return Ok(pos + 1);
                }
                if bytes[pos] == b',' || bytes[pos] == b':' {
                    pos += 1;
                    continue;
                }
                pos = value_end(bytes, pos, depth + 1)?;
            }
            Err("engine JSON collection incomplete".into())
        }
        _ => {
            let mut pos = i;
            while pos < bytes.len() && !b",]} \r\n\t".contains(&bytes[pos]) {
                pos += 1;
            }
            ensure(pos > i, "engine JSON scalar invalid")?;
            Ok(pos)
        }
    }
}
fn field<'a>(raw: &'a str, key: &str) -> Result<&'a str> {
    let bytes = raw.as_bytes();
    let mut i = white(bytes, 0);
    ensure(bytes.get(i) == Some(&b'{'), "engine JSON object required")?;
    i += 1;
    loop {
        i = white(bytes, i);
        if bytes.get(i) == Some(&b'}') {
            return Err("engine memory field missing".into());
        }
        let end = string_end(bytes, i)?;
        let candidate = &raw[i + 1..end - 1];
        i = white(bytes, end);
        ensure(bytes.get(i) == Some(&b':'), "engine JSON field invalid")?;
        i = white(bytes, i + 1);
        let end = value_end(bytes, i, 0)?;
        if candidate == key {
            return Ok(&raw[i..end]);
        }
        i = white(bytes, end);
        ensure(
            bytes.get(i) == Some(&b',') || bytes.get(i) == Some(&b'}'),
            "engine JSON separator invalid",
        )?;
        if bytes[i] == b',' {
            i += 1;
        }
    }
}
fn usage(raw: &str) -> Result<u64> {
    field(field(raw, "memory_stats")?, "usage")?
        .parse::<u64>()
        .map_err(|_| "exact engine memory usage unavailable".into())
}
fn body(raw: &[u8]) -> Result<String> {
    ensure(
        raw.len() <= 2 * 1024 * 1024,
        "engine stats response exceeds bound",
    )?;
    let split = raw
        .windows(4)
        .position(|s| s == b"\r\n\r\n")
        .ok_or("engine HTTP header missing")?;
    let headers = std::str::from_utf8(&raw[..split]).map_err(|_| "engine HTTP encoding invalid")?;
    ensure(
        headers.lines().next().is_some_and(|v| {
            v.starts_with("HTTP/1.") && v.split_whitespace().nth(1) == Some("200")
        }),
        "engine memory request rejected",
    )?;
    let mut bytes = raw[split + 4..].to_vec();
    if headers.lines().any(|line| {
        line.to_ascii_lowercase().starts_with("transfer-encoding:")
            && line.to_ascii_lowercase().contains("chunked")
    }) {
        let mut out = Vec::new();
        let mut pos = 0;
        loop {
            let line = bytes[pos..]
                .windows(2)
                .position(|v| v == b"\r\n")
                .ok_or("engine chunk header incomplete")?
                + pos;
            let size = std::str::from_utf8(&bytes[pos..line])
                .map_err(|_| "engine chunk encoding invalid")?;
            let size = usize::from_str_radix(size.split(';').next().unwrap_or(""), 16)
                .map_err(|_| "engine chunk size invalid")?;
            pos = line + 2;
            if size == 0 {
                break;
            }
            ensure(
                pos.checked_add(size + 2)
                    .is_some_and(|end| end <= bytes.len())
                    && out.len() + size <= 2 * 1024 * 1024,
                "engine chunk exceeds response bound",
            )?;
            out.extend(&bytes[pos..pos + size]);
            pos += size;
            ensure(
                &bytes[pos..pos + 2] == b"\r\n",
                "engine chunk terminator invalid",
            )?;
            pos += 2;
        }
        bytes = out;
    }
    String::from_utf8(bytes).map_err(|_| "engine stats JSON encoding invalid".into())
}
fn query(id: &str, api: &str) -> Result<u64> {
    ensure(
        !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "engine container identity invalid",
    )?;
    let mut c = command("docker", &strings(&["system", "dial-stdio"]));
    c.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c
        .spawn()
        .map_err(|_| "engine stats transport unavailable")?;
    let request = format!(
        "GET /v{api}/containers/{id}/stats?stream=false&one-shot=true HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    );
    // Keep stdin open until the engine has closed its response. Closing the
    // half-stream early cancels in-flight requests in Docker Desktop's proxy.
    let mut input = child.stdin.take().ok_or("engine stats input unavailable")?;
    input
        .write_all(request.as_bytes())
        .map_err(|_| "engine stats request failed")?;
    input.flush().map_err(|_| "engine stats request failed")?;
    let output = child
        .wait_with_output()
        .map_err(|_| "engine stats transport failed")?;
    drop(input);
    ensure(
        output.status.success(),
        "engine stats transport rejected; output withheld",
    )?;
    usage(&body(&output.stdout)?)
}
pub(super) fn snapshot() -> Result<Snapshot> {
    let total = docker(
        &strings(&["info", "--format", "{{.MemTotal}}"]),
        "Docker memory inventory",
    )?
    .trim()
    .parse::<u64>()
    .map_err(|_| "Docker memory unavailable")?;
    let api = docker(
        &strings(&["version", "--format", "{{.Server.APIVersion}}"]),
        "engine API version",
    )?;
    let api = api.trim();
    ensure(
        !api.is_empty() && api.bytes().all(|b| b.is_ascii_digit() || b == b'.'),
        "engine API version invalid",
    )?;
    let ids = docker(
        &strings(&["ps", "--no-trunc", "--format", "{{.ID}}"]),
        "running memory inventory",
    )?;
    let mut used = 0u64;
    let mut containers = 0;
    for id in ids.lines().filter(|s| !s.is_empty()) {
        used = used
            .checked_add(query(id, api)?)
            .ok_or("engine memory sum overflow")?;
        containers += 1;
    }
    ensure(
        total > 0 && used <= total,
        "engine memory observation inconsistent",
    )?;
    Ok(Snapshot {
        total,
        used,
        available: total - used,
        containers,
    })
}
pub(super) fn admit(snapshot: Snapshot, database_only: bool, requested: u64) -> Result<()> {
    // Docker Desktop's nominal 4 GB configuration exposes less than 4 GiB.
    // The database lane has its own 1.5 GB floor and does not admit core services.
    let floor = if database_only {
        1_500_000_000
    } else {
        4_000_000_000
    };
    ensure(
        snapshot.total >= floor,
        "Docker nominal capacity insufficient for selected phase",
    )?;
    ensure(
        snapshot.used as u128 * 100 < snapshot.total as u128 * 80,
        "Docker actual usage reached 80 percent; startup stopped without killing services",
    )?;
    // Count all current containers, including unrelated/build work. Keep room for
    // daemon/kernel overhead and the next bounded process before starting it.
    let required = requested
        .checked_add(256_000_000)
        .ok_or("memory reserve overflow")?;
    ensure(
        snapshot.available >= required,
        "Docker actual headroom insufficient for next serial service",
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_usage_is_not_a_container_limit_percentage() {
        let raw = r#"{"memory_stats":{"stats":{"usage":999,"cache":50},"usage":134217728,"limit":268435456},"other":{"usage":777}}"#;
        assert_eq!(usage(raw).unwrap(), 134217728);
        assert!(usage(r#"{"memory_stats":{"limit":1000}}"#).is_err());
        assert!(usage(r#"{"memory_stats":{"usage":"50%"}}"#).is_err());
    }
    #[test]
    fn fixed_and_chunked_engine_responses_agree() {
        let json = r#"{"memory_stats":{"usage":42}}"#;
        let plain = format!("HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{json}");
        let chunked = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{json}\r\n0\r\n\r\n",
            json.len()
        );
        for raw in [plain, chunked] {
            assert_eq!(usage(&body(raw.as_bytes()).unwrap()).unwrap(), 42);
        }
        assert!(body(b"HTTP/1.1 403 Forbidden\r\n\r\n{}").is_err());
        assert!(body(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nffff\r\n{}").is_err());
    }
    #[test]
    fn nominal_four_gb_and_database_only_have_distinct_real_budgets() {
        let actual = Snapshot {
            total: 4_107_141_120,
            used: 512_000_000,
            available: 3_595_141_120,
            containers: 2,
        };
        admit(actual, false, 512 * 1024 * 1024).unwrap();
        let small = Snapshot {
            total: 2_000_000_000,
            used: 256_000_000,
            available: 1_744_000_000,
            containers: 1,
        };
        admit(small, true, 512 * 1024 * 1024).unwrap();
        assert!(admit(small, false, 0).is_err());
        let busy = Snapshot {
            total: 4_107_141_120,
            used: 3_400_000_000,
            available: 707_141_120,
            containers: 8,
        };
        assert!(admit(busy, true, 0).is_err());
        let little = Snapshot {
            total: 4_107_141_120,
            used: 3_000_000_000,
            available: 1_107_141_120,
            containers: 8,
        };
        assert!(admit(little, false, 1024 * 1024 * 1024).is_err());
    }
    #[test]
    #[ignore = "read-only live Docker engine observation; explicit operator run only"]
    fn live_engine_snapshot() {
        super::super::ownership(&std::env::current_dir().unwrap()).unwrap();
        let observed = snapshot().unwrap();
        assert!(observed.total > 0);
        admit(observed, true, 256 * 1024 * 1024).unwrap();
        admit(observed, false, 512 * 1024 * 1024).unwrap();
        assert_eq!(observed.available, observed.total - observed.used);
        println!(
            "daemon_total_bytes={} actual_container_usage_bytes={} available_bytes={} containers={}",
            observed.total, observed.used, observed.available, observed.containers
        );
    }
}
