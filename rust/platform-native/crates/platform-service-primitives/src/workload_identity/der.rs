//! Minimal bounded DER extraction of URI subjectAltName, after WebPKI validates
//! chain/signatures/EKU/validity. This never implements certificate validation.
use super::{Error, Result, ServiceIdentity};
#[derive(Clone, Copy)]
struct Tlv<'a> {
    tag: u8,
    body: &'a [u8],
}
fn take<'a>(input: &mut &'a [u8]) -> Result<Tlv<'a>> {
    if input.len() < 2 {
        return Err(Error("workload_certificate_der"));
    }
    let tag = input[0];
    if tag & 0x1f == 0x1f {
        return Err(Error("workload_certificate_der"));
    }
    let first = input[1];
    let mut offset = 2;
    let length = if first < 128 {
        usize::from(first)
    } else {
        let count = usize::from(first & 0x7f);
        if count == 0 || count > 4 || input.len() < offset + count || input[offset] == 0 {
            return Err(Error("workload_certificate_der"));
        }
        let mut n = 0usize;
        for b in &input[offset..offset + count] {
            n = n
                .checked_mul(256)
                .and_then(|n| n.checked_add(usize::from(*b)))
                .ok_or(Error("workload_certificate_der"))?;
        }
        offset += count;
        if n < 128 {
            return Err(Error("workload_certificate_der"));
        }
        n
    };
    let end = offset
        .checked_add(length)
        .ok_or(Error("workload_certificate_der"))?;
    if end > input.len() {
        return Err(Error("workload_certificate_der"));
    }
    let value = Tlv {
        tag,
        body: &input[offset..end],
    };
    *input = &input[end..];
    Ok(value)
}
fn tagged<'a>(input: &mut &'a [u8], tag: u8) -> Result<Tlv<'a>> {
    let value = take(input)?;
    if value.tag != tag {
        return Err(Error("workload_certificate_der"));
    }
    Ok(value)
}
pub(super) fn identity(certificate: &[u8]) -> Result<ServiceIdentity> {
    if certificate.len() > 256 * 1024 {
        return Err(Error("workload_certificate_der"));
    }
    let mut input = certificate;
    let outer = tagged(&mut input, 0x30)?;
    if !input.is_empty() {
        return Err(Error("workload_certificate_der"));
    }
    let mut cert = outer.body;
    let tbs = tagged(&mut cert, 0x30)?;
    tagged(&mut cert, 0x30)?;
    tagged(&mut cert, 0x03)?;
    if !cert.is_empty() {
        return Err(Error("workload_certificate_der"));
    }
    let mut fields = tbs.body;
    if fields.first() == Some(&0xa0) {
        tagged(&mut fields, 0xa0)?;
    }
    for tag in [0x02, 0x30, 0x30, 0x30, 0x30, 0x30] {
        tagged(&mut fields, tag)?;
    }
    let mut extensions = None;
    while !fields.is_empty() {
        let field = take(&mut fields)?;
        match field.tag {
            0x81 | 0x82 => {}
            0xa3 if extensions.is_none() => extensions = Some(field.body),
            _ => return Err(Error("workload_certificate_der")),
        }
    }
    let mut explicit = extensions.ok_or(Error("workload_uri_san_missing"))?;
    let list = tagged(&mut explicit, 0x30)?;
    if !explicit.is_empty() {
        return Err(Error("workload_certificate_der"));
    }
    let mut list = list.body;
    let mut uri = None;
    let mut san_seen = false;
    while !list.is_empty() {
        let mut extension = tagged(&mut list, 0x30)?.body;
        let oid = tagged(&mut extension, 0x06)?;
        if extension.first() == Some(&0x01) {
            let critical = tagged(&mut extension, 0x01)?;
            if critical.body != [0xff] {
                return Err(Error("workload_certificate_der"));
            }
        }
        let value = tagged(&mut extension, 0x04)?;
        if !extension.is_empty() {
            return Err(Error("workload_certificate_der"));
        }
        if oid.body != [0x55, 0x1d, 0x11] {
            continue;
        }
        if san_seen {
            return Err(Error("workload_uri_san_ambiguous"));
        }
        san_seen = true;
        let mut inner = value.body;
        let names = tagged(&mut inner, 0x30)?;
        if !inner.is_empty() {
            return Err(Error("workload_certificate_der"));
        }
        let mut names = names.body;
        while !names.is_empty() {
            let name = take(&mut names)?;
            if name.tag == 0x86 {
                if uri.is_some() {
                    return Err(Error("workload_uri_san_ambiguous"));
                }
                let raw =
                    std::str::from_utf8(name.body).map_err(|_| Error("workload_spiffe_invalid"))?;
                uri = Some(ServiceIdentity::parse(raw)?);
            }
        }
    }
    uri.ok_or(Error("workload_uri_san_missing"))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_and_substring_certificate_impostors_fail() {
        for input in [
            &b"spiffe://agentforge.local/ns/platform/sa/gateway"[..],
            &[0x30, 0x80, 0, 0][..],
            &[0x30, 0x81, 1, 0][..],
            &[0x30, 0xff][..],
        ] {
            assert!(identity(input).is_err());
        }
    }
}
