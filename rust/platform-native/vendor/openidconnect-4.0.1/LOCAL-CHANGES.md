# Local RSA backend patch for openidconnect 4.0.1

This is the complete cached crates.io openidconnect 4.0.1 distribution with a
local, explicitly maintained RSA backend patch. It is not an upstream fixed
release and does not rename or impersonate another crate/version. Package name,
version, public OIDC API and algorithm set remain unchanged.

`UPSTREAM-SOURCE.json` records SHA256 for every original file, the registry and
repository URLs, and the original `.crate` SHA256. `upstream-source.crate` retains
the complete byte-exact public upstream distribution. All original source,
fixtures, tests and LICENSE remain present. The archive SHA256 is
`0d8c6709ba2ea764bbed26bce1adf3c10517113ddea6f2d4196e4851757ef2b2`.

## Production patch

The normalized Cargo.toml and Cargo.toml.orig replace the `rsa 0.9` dependency with
the already cached `openssl 0.10.81`. No workspace manifest, lockfile, deny policy,
advisory entry, enterprise business code, or SSO algorithm allowlist was changed.
The original upstream Cargo.lock files remain historical distribution contents;
the primary must resolve the active workspace graph and verify that RustCrypto
rsa no longer occurs. Those nested historical lockfiles do not claim remediation.

- CoreJsonWebKey RSA verification still receives upstream's already-computed
  SHA-256/384/512 digest. EVP_PKEY_verify verifies that digest directly; it never
  hashes it a second time. CoreRsaPrivateSigningKey keeps the upstream digest
  calculation and uses EVP_PKEY_sign with the same explicit signature digest.
- RS256/384/512 use PKCS1 v1.5. PS256/384/512 use PSS, matching MGF1 digest and
  digest-length salt, for signing AND verification. Auto/max salt acceptance is
  not used. Signatures must have exactly the modulus byte length.
- Unsigned OpenSSL BigNums normalize leading-zero JWK n/e. Existing RSA public
  bounds remain: odd modulus <=4096 bits, odd 2..2^33-1 exponent, exponent < n.
- Private keys remain PKCS1 PEM inputs and exported public JWKs contain n/e only.
  OpenSSL validates private key consistency; no private key bytes cross the API.
- Other algorithms, PKCE, state, nonce, issuer, audience, claims, discovery and
  OAuth transport logic are unchanged. No RSA decryption interface is introduced.

## Upstream fixed-RNG golden tests

OpenSSL does not expose per-signature deterministic RNG injection. The upstream
crate-private `from_pem_internal(TestRng)` exists only under cfg(test). For those
original PSS golden vectors, cfg(test)-only EMSA-PSS encoding consumes the exact
supplied RNG and invokes OpenSSL's blinded RSA private operation. It is not
compiled into production and does not replace production PSS randomness.

All upstream expected signature strings and assertions are retained. The only
jwk/tests.rs edits replace rsa's rand_core re-export with rand's identical Error
re-export. The prehashed crypto helper test's padding selector now names the
unchanged Core JWS algorithm instead of the removed RustCrypto padding type.

Additional regressions exercise the actual production constructor/backend:
- all six algorithms against independent streaming EVP Signer/Verifier;
- tampered messages/signatures and prehashed-vs-double-hashed input;
- valid leading-zero modulus/exponent and malformed public key rejection;
- PS256/384/512 reject zero-length salt and mismatched MGF1 SHA-1.

## Existing RP certification test skips

The 21 upstream `#[ignore]` attributes in `rp_certification_code.rs` and `rp_certification_dynamic.rs` remain in place and now state their live-service preconditions. The code flow cases use the public OpenID RP certification service for discovery, client registration, and authorization/token/UserInfo journeys; the dynamic registration case creates remote client state. Only ignore-reason strings changed: test bodies and assertions are untouched, and the full upstream tests remain available for explicit runs.

## Verification / activation

Only source-preservation/SHA checks and rustfmt parsing/checks ran in this worker.
No Cargo, Docker, network download, provider or OIDC journey ran. The primary must
attach this path as the workspace crates.io patch, resolve/update the real lock,
compile production and tests, execute the complete retained upstream tests plus
new regressions, run downstream auth/Gateway tests and rerun cargo-deny. Until
then this is implemented source, not a compiled or accepted advisory remediation.

`LOCAL-RSA-BACKEND.patch` is the exact source/manifest/test patch relative to the
upstream distribution, including new backend files and the extracted public test
fixture. Receipt and provenance files are intentionally outside that source diff.
