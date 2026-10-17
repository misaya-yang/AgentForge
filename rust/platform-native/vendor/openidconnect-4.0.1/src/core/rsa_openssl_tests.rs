//! Downstream regressions in addition to the unchanged upstream golden vectors.
use super::*;
use crate::{core::CoreRsaPrivateSigningKey, JsonWebKey, PrivateSigningKey};
use openssl::sign::Signer;
const PEM: &str = include_str!("../../tests/fixtures/upstream-rsa-private.pem");
fn algorithms() -> [Alg; 6] {
    [
        Alg::RsaSsaPkcs1V15Sha256,
        Alg::RsaSsaPkcs1V15Sha384,
        Alg::RsaSsaPkcs1V15Sha512,
        Alg::RsaSsaPssSha256,
        Alg::RsaSsaPssSha384,
        Alg::RsaSsaPssSha512,
    ]
}
#[test]
fn all_six_algorithms_interoperate_with_independent_streaming_evp() {
    let private = CoreRsaPrivateSigningKey::from_pem(PEM, None).unwrap();
    let public = private.as_verification_key();
    let rsa = Rsa::private_key_from_pem(PEM.as_bytes()).unwrap();
    let pkey = PKey::from_rsa(rsa).unwrap();
    let msg = b"eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJ1c2VyIn0";
    for alg in algorithms() {
        let (_, md, pss) = parameters(&alg).unwrap();
        let mut reference = Signer::new(md, &pkey).unwrap();
        reference
            .set_rsa_padding(if pss {
                Padding::PKCS1_PSS
            } else {
                Padding::PKCS1
            })
            .unwrap();
        if pss {
            reference.set_rsa_mgf1_md(md).unwrap();
            reference
                .set_rsa_pss_saltlen(RsaPssSaltlen::DIGEST_LENGTH)
                .unwrap();
        }
        reference.update(msg).unwrap();
        let signature = reference.sign_to_vec().unwrap();
        public.verify_signature(&alg, msg, &signature).unwrap();
        assert!(public
            .verify_signature(&alg, b"tampered", &signature)
            .is_err());
        let own = private.sign(&alg, msg).unwrap();
        let mut verifier = openssl::sign::Verifier::new(md, &pkey).unwrap();
        verifier
            .set_rsa_padding(if pss {
                Padding::PKCS1_PSS
            } else {
                Padding::PKCS1
            })
            .unwrap();
        if pss {
            verifier.set_rsa_mgf1_md(md).unwrap();
            verifier
                .set_rsa_pss_saltlen(RsaPssSaltlen::DIGEST_LENGTH)
                .unwrap();
        }
        verifier.update(msg).unwrap();
        assert!(verifier.verify(&own).unwrap());
        let mut corrupt = own;
        corrupt[0] ^= 1;
        assert!(public.verify_signature(&alg, msg, &corrupt).is_err());
    }
}
#[test]
fn prehashed_verification_does_not_hash_twice_and_accepts_unsigned_leading_zeroes() {
    let rsa = Rsa::private_key_from_pem(PEM.as_bytes()).unwrap();
    let alg = Alg::RsaSsaPkcs1V15Sha256;
    let digest = openssl::hash::hash(MessageDigest::sha256(), b"JOSE signing input").unwrap();
    let signature = sign(&rsa, &alg, &digest).unwrap();
    let mut n = vec![0, 0];
    n.extend_from_slice(&rsa.n().to_vec());
    let mut e = vec![0, 0];
    e.extend_from_slice(&rsa.e().to_vec());
    let public = public_key(&n, &e).unwrap();
    verify(&public, &alg, &digest, &signature).unwrap();
    let double = openssl::hash::hash(MessageDigest::sha256(), &digest).unwrap();
    assert!(verify(&public, &alg, &double, &signature).is_err());
    assert!(sign(&rsa, &alg, b"unhashed").is_err());
    for (n, e) in [
        (vec![0], vec![3]),
        (vec![4], vec![3]),
        (rsa.n().to_vec(), vec![2]),
        (rsa.n().to_vec(), vec![2, 0, 0, 0, 1]),
    ] {
        assert!(public_key(&n, &e).is_err());
    }
}
#[test]
fn pss_requires_digest_sized_salt_matching_mgf_and_algorithm() {
    let rsa = Rsa::private_key_from_pem(PEM.as_bytes()).unwrap();
    let public = public_key(&rsa.n().to_vec(), &rsa.e().to_vec()).unwrap();
    let pkey = PKey::from_rsa(rsa).unwrap();
    let msg = b"PSS protocol parameters";
    for alg in [
        Alg::RsaSsaPssSha256,
        Alg::RsaSsaPssSha384,
        Alg::RsaSsaPssSha512,
    ] {
        let (_, md, _) = parameters(&alg).unwrap();
        let digest = openssl::hash::hash(md, msg).unwrap();
        for wrong_mgf in [false, true] {
            let mut signer = Signer::new(md, &pkey).unwrap();
            signer.set_rsa_padding(Padding::PKCS1_PSS).unwrap();
            signer
                .set_rsa_mgf1_md(if wrong_mgf { MessageDigest::sha1() } else { md })
                .unwrap();
            signer
                .set_rsa_pss_saltlen(if wrong_mgf {
                    RsaPssSaltlen::DIGEST_LENGTH
                } else {
                    RsaPssSaltlen::custom(0)
                })
                .unwrap();
            signer.update(msg).unwrap();
            let signature = signer.sign_to_vec().unwrap();
            assert!(verify(&public, &alg, &digest, &signature).is_err());
        }
    }
}
