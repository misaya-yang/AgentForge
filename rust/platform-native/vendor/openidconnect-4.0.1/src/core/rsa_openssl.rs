//! Local RSA backend replacement. Inputs to sign/verify are PREHASHED JOSE
//! digests: EVP_PKEY_sign/verify must not hash them again. No RSA decryption API.
use super::CoreJwsSigningAlgorithm as Alg;
use openssl::{
    bn::BigNum,
    hash::MessageDigest,
    md::{Md, MdRef},
    pkey::{PKey, Private, Public},
    pkey_ctx::PkeyCtx,
    rsa::{Padding, Rsa},
    sign::RsaPssSaltlen,
};

pub(super) fn parameters(alg: &Alg) -> Result<(&'static MdRef, MessageDigest, bool), ()> {
    match alg {
        Alg::RsaSsaPkcs1V15Sha256 => Ok((Md::sha256(), MessageDigest::sha256(), false)),
        Alg::RsaSsaPkcs1V15Sha384 => Ok((Md::sha384(), MessageDigest::sha384(), false)),
        Alg::RsaSsaPkcs1V15Sha512 => Ok((Md::sha512(), MessageDigest::sha512(), false)),
        Alg::RsaSsaPssSha256 => Ok((Md::sha256(), MessageDigest::sha256(), true)),
        Alg::RsaSsaPssSha384 => Ok((Md::sha384(), MessageDigest::sha384(), true)),
        Alg::RsaSsaPssSha512 => Ok((Md::sha512(), MessageDigest::sha512(), true)),
        _ => Err(()),
    }
}
pub(super) fn public_key(n: &[u8], e: &[u8]) -> Result<PKey<Public>, ()> {
    // BigNum unsigned conversion accepts harmless DER/JWK leading zeroes.
    // Preserve RustCrypto's existing public exponent and 4096-bit size bounds.
    let n = BigNum::from_slice(n).map_err(|_| ())?;
    let e = BigNum::from_slice(e).map_err(|_| ())?;
    let exponent = e.to_vec();
    if n.num_bits() > 4096
        || !n.is_odd()
        || !e.is_odd()
        || e.ucmp(&n) != std::cmp::Ordering::Less
        || exponent.len() > 5
    {
        return Err(());
    }
    let exponent = exponent.iter().fold(0u64, |n, b| (n << 8) | u64::from(*b));
    if !(2..=((1u64 << 33) - 1)).contains(&exponent) {
        return Err(());
    }
    PKey::from_rsa(Rsa::from_public_components(n, e).map_err(|_| ())?).map_err(|_| ())
}
pub(super) fn verify(
    key: &PKey<Public>,
    alg: &Alg,
    digest: &[u8],
    signature: &[u8],
) -> Result<(), ()> {
    let (md, hash, pss) = parameters(alg)?;
    if digest.len() != hash.size() || signature.len() != key.size() {
        return Err(());
    }
    let mut ctx = PkeyCtx::new(key).map_err(|_| ())?;
    ctx.verify_init().map_err(|_| ())?;
    ctx.set_signature_md(md).map_err(|_| ())?;
    ctx.set_rsa_padding(if pss {
        Padding::PKCS1_PSS
    } else {
        Padding::PKCS1
    })
    .map_err(|_| ())?;
    if pss {
        ctx.set_rsa_mgf1_md(md).map_err(|_| ())?;
        ctx.set_rsa_pss_saltlen(RsaPssSaltlen::DIGEST_LENGTH)
            .map_err(|_| ())?;
    }
    if ctx.verify(digest, signature).map_err(|_| ())? {
        Ok(())
    } else {
        Err(())
    }
}
pub(super) fn sign(key: &Rsa<Private>, alg: &Alg, digest: &[u8]) -> Result<Vec<u8>, ()> {
    let (md, hash, pss) = parameters(alg)?;
    if digest.len() != hash.size() {
        return Err(());
    }
    let key = PKey::from_rsa(key.clone()).map_err(|_| ())?;
    let mut ctx = PkeyCtx::new(&key).map_err(|_| ())?;
    ctx.sign_init().map_err(|_| ())?;
    ctx.set_signature_md(md).map_err(|_| ())?;
    ctx.set_rsa_padding(if pss {
        Padding::PKCS1_PSS
    } else {
        Padding::PKCS1
    })
    .map_err(|_| ())?;
    if pss {
        ctx.set_rsa_mgf1_md(md).map_err(|_| ())?;
        ctx.set_rsa_pss_saltlen(RsaPssSaltlen::DIGEST_LENGTH)
            .map_err(|_| ())?;
    }
    let mut signature = Vec::new();
    ctx.sign_to_vec(digest, &mut signature).map_err(|_| ())?;
    Ok(signature)
}

// OpenSSL intentionally does not expose a per-signature deterministic RNG. This
// cfg(test)-only EMSA encoder preserves ALL upstream fixed-salt golden vectors.
// Private exponentiation remains OpenSSL's blinded RSA operation. Production
// never compiles this function and always uses EVP's random salt above.
#[cfg(test)]
pub(super) fn fixture_pss_sign(
    key: &Rsa<Private>,
    alg: &Alg,
    digest: &[u8],
    rng: &mut dyn super::jwk::RngClone,
) -> Result<Vec<u8>, ()> {
    let (_, hash, pss) = parameters(alg)?;
    if !pss || digest.len() != hash.size() {
        return Err(());
    }
    let bits = (key.n().num_bits() - 1) as usize;
    let len = bits.div_ceil(8);
    let hlen = hash.size();
    if len < hlen * 2 + 2 {
        return Err(());
    }
    let mut salt = vec![0u8; hlen];
    rng.fill_bytes(&mut salt);
    let mut prefix = vec![0u8; 8];
    prefix.extend_from_slice(digest);
    prefix.extend_from_slice(&salt);
    let h = openssl::hash::hash(hash, &prefix).map_err(|_| ())?;
    let mut db = vec![0u8; len - hlen - 1];
    let delimiter = db.len() - salt.len() - 1;
    db[delimiter] = 1;
    db[delimiter + 1..].copy_from_slice(&salt);
    for (counter, chunk) in db.chunks_mut(hlen).enumerate() {
        let mut seed = h.to_vec();
        seed.extend_from_slice(&(counter as u32).to_be_bytes());
        let mask = openssl::hash::hash(hash, &seed).map_err(|_| ())?;
        for (byte, mask) in chunk.iter_mut().zip(mask.iter()) {
            *byte ^= *mask;
        }
    }
    db[0] &= 0xffu8 >> (8 * len - bits);
    db.extend_from_slice(&h);
    db.push(0xbc);
    // RSA_NO_PADDING requires a full modulus-sized representative (including a
    // possible extra leading byte when emBits is a multiple of eight).
    let mut encoded = vec![0u8; key.size() as usize - db.len()];
    encoded.extend_from_slice(&db);
    let mut out = vec![0u8; key.size() as usize];
    let n = key
        .private_encrypt(&encoded, &mut out, Padding::NONE)
        .map_err(|_| ())?;
    out.truncate(n);
    Ok(out)
}
#[cfg(test)]
#[path = "rsa_openssl_tests.rs"]
mod tests;
