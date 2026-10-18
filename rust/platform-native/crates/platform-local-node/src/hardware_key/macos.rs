use super::*;
use security_framework::{
    access_control::{ProtectionMode, SecAccessControl},
    item::{ItemSearchOptions, KeyClass, Location, Reference, SearchResult},
    key::{Algorithm, GenerateKeyOptions, KeyType, SecKey, Token},
};
// Framework constants verify that a reopened key is actually Secure Enclave-backed.
use std::ffi::c_void;
#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecAttrTokenID: *const c_void;
    static kSecAttrTokenIDSecureEnclave: *const c_void;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFEqual(a: *const c_void, b: *const c_void) -> u8;
}
pub struct Signer {
    key: SecKey,
    public: String,
}
pub fn open(name: &str, create: bool) -> Result<Signer> {
    let key = if create {
        let access = SecAccessControl::create_with_protection(
            Some(ProtectionMode::AccessibleWhenUnlockedThisDeviceOnly),
            1 << 30,
        )
        .map_err(|_| Error::Denied("Secure Enclave access control unavailable"))?;
        let mut options = GenerateKeyOptions::default();
        options
            .set_key_type(KeyType::ec())
            .set_size_in_bits(256)
            .set_label(name)
            .set_token(Token::SecureEnclave)
            .set_location(Location::DataProtectionKeychain)
            .set_access_control(access);
        SecKey::new(&options)
            .map_err(|_| Error::Denied("Secure Enclave key generation unavailable"))?
    } else {
        let found = ItemSearchOptions::new()
            .ignore_legacy_keychains()
            .key_class(KeyClass::private())
            .label(name)
            .load_refs(true)
            .search()
            .map_err(|_| Error::Denied("Secure Enclave key unavailable"))?;
        let mut keys = found.into_iter().filter_map(|v| match v {
            SearchResult::Ref(Reference::Key(key)) => Some(key),
            _ => None,
        });
        let key = keys
            .next()
            .ok_or(Error::Denied("Secure Enclave key missing"))?;
        if keys.next().is_some() {
            return Err(Error::Integrity);
        }
        key
    };
    let attrs = key.attributes();
    let hardware = unsafe {
        attrs
            .find(kSecAttrTokenID)
            .is_some_and(|token| CFEqual(*token, kSecAttrTokenIDSecureEnclave) != 0)
    };
    if !hardware || key.external_representation().is_some() {
        return Err(Error::Denied("non-exportable Secure Enclave key required"));
    }
    let public = key
        .public_key()
        .and_then(|k| k.external_representation())
        .ok_or(Error::Integrity)?;
    Ok(Signer {
        key,
        public: URL_SAFE_NO_PAD.encode(public.to_vec()),
    })
}
impl PairingSigner for Signer {
    fn algorithm(&self) -> &str {
        "ES256"
    }
    fn key_class(&self) -> &str {
        "macos-secure-enclave-p256"
    }
    fn public_key(&self) -> &str {
        &self.public
    }
    fn sign(&self, payload: &[u8]) -> Result<String> {
        self.key
            .create_signature(Algorithm::ECDSASignatureMessageX962SHA256, payload)
            .map(|s| URL_SAFE_NO_PAD.encode(s))
            .map_err(|_| Error::Denied("Secure Enclave signature unavailable"))
    }
}
