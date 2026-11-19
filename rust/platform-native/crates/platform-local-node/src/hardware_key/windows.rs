//! Windows CNG Platform Crypto Provider. Software KSPs are never selected.
use super::*;
use sha2::{Digest, Sha256};
use std::{ffi::c_void, ptr, sync::Mutex};
#[link(name = "ncrypt")]
unsafe extern "system" {
    fn NCryptOpenStorageProvider(out: *mut usize, name: *const u16, flags: u32) -> i32;
    fn NCryptOpenKey(
        provider: usize,
        out: *mut usize,
        name: *const u16,
        legacy: u32,
        flags: u32,
    ) -> i32;
    fn NCryptCreatePersistedKey(
        provider: usize,
        out: *mut usize,
        algorithm: *const u16,
        name: *const u16,
        legacy: u32,
        flags: u32,
    ) -> i32;
    fn NCryptSetProperty(
        object: usize,
        name: *const u16,
        input: *const u8,
        len: u32,
        flags: u32,
    ) -> i32;
    fn NCryptGetProperty(
        object: usize,
        name: *const u16,
        output: *mut u8,
        len: u32,
        result: *mut u32,
        flags: u32,
    ) -> i32;
    fn NCryptFinalizeKey(key: usize, flags: u32) -> i32;
    fn NCryptExportKey(
        key: usize,
        export: usize,
        blob: *const u16,
        params: *const c_void,
        output: *mut u8,
        len: u32,
        result: *mut u32,
        flags: u32,
    ) -> i32;
    fn NCryptSignHash(
        key: usize,
        padding: *const c_void,
        hash: *const u8,
        hash_len: u32,
        signature: *mut u8,
        len: u32,
        result: *mut u32,
        flags: u32,
    ) -> i32;
    fn NCryptFreeObject(object: usize) -> i32;
}
const SILENT: u32 = 0x40;
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}
struct Handle(usize);
impl Drop for Handle {
    fn drop(&mut self) {
        if self.0 != 0 {
            unsafe {
                NCryptFreeObject(self.0);
            }
        }
    }
}
pub struct Signer {
    key: Handle,
    _provider: Handle,
    public: String,
    gate: Mutex<()>,
}
pub fn open(name: &str, create: bool) -> Result<Signer> {
    let mut provider = Handle(0);
    if unsafe {
        NCryptOpenStorageProvider(
            &mut provider.0,
            wide("Microsoft Platform Crypto Provider").as_ptr(),
            0,
        )
    } != 0
    {
        return Err(Error::Denied("TPM CNG provider unavailable"));
    }
    let mut implementation = 0u32;
    let mut used = 0;
    if unsafe {
        NCryptGetProperty(
            provider.0,
            wide("Impl Type").as_ptr(),
            (&mut implementation as *mut u32).cast(),
            4,
            &mut used,
            0,
        )
    } != 0
        || used != 4
        || implementation & 1 == 0
    {
        return Err(Error::Denied("hardware TPM provider required"));
    }
    let mut key = Handle(0);
    let status = unsafe {
        if create {
            NCryptCreatePersistedKey(
                provider.0,
                &mut key.0,
                wide("ECDSA_P256").as_ptr(),
                wide(name).as_ptr(),
                0,
                SILENT,
            )
        } else {
            NCryptOpenKey(provider.0, &mut key.0, wide(name).as_ptr(), 0, SILENT)
        }
    };
    if status != 0 {
        return Err(Error::Denied("TPM CNG key unavailable"));
    }
    if create {
        let export_policy = 0u32;
        if unsafe {
            NCryptSetProperty(
                key.0,
                wide("Export Policy").as_ptr(),
                (&export_policy as *const u32).cast(),
                4,
                0,
            )
        } != 0
            || unsafe { NCryptFinalizeKey(key.0, SILENT) } != 0
        {
            return Err(Error::Denied("non-exportable TPM key creation failed"));
        }
    }
    let mut size = 0u32;
    // Test an actual bounded export, not only a size query (some KSPs allow
    // length queries even when material export is forbidden).
    let mut private = [0u8; 512];
    let exported = unsafe {
        NCryptExportKey(
            key.0,
            0,
            wide("ECCPRIVATEBLOB").as_ptr(),
            ptr::null(),
            private.as_mut_ptr(),
            512,
            &mut size,
            SILENT,
        )
    };
    private.fill(0);
    if exported == 0 {
        return Err(Error::Denied("TPM private key export must be refused"));
    }
    let mut blob = [0u8; 72];
    if unsafe {
        NCryptExportKey(
            key.0,
            0,
            wide("ECCPUBLICBLOB").as_ptr(),
            ptr::null(),
            blob.as_mut_ptr(),
            72,
            &mut size,
            0,
        )
    } != 0
        || size != 72
        || u32::from_le_bytes(blob[..4].try_into().unwrap()) != 0x31534345
        || u32::from_le_bytes(blob[4..8].try_into().unwrap()) != 32
    {
        return Err(Error::Integrity);
    }
    let mut point = vec![4];
    point.extend_from_slice(&blob[8..]);
    Ok(Signer {
        key,
        _provider: provider,
        public: URL_SAFE_NO_PAD.encode(point),
        gate: Mutex::new(()),
    })
}
impl PairingSigner for Signer {
    fn algorithm(&self) -> &str {
        "ES256"
    }
    fn key_class(&self) -> &str {
        "windows-tpm-cng-p256"
    }
    fn public_key(&self) -> &str {
        &self.public
    }
    fn sign(&self, payload: &[u8]) -> Result<String> {
        let _guard = self.gate.lock().map_err(|_| Error::Integrity)?;
        let hash = Sha256::digest(payload);
        let mut signature = [0u8; 64];
        let mut used = 0;
        if unsafe {
            NCryptSignHash(
                self.key.0,
                ptr::null(),
                hash.as_ptr(),
                32,
                signature.as_mut_ptr(),
                64,
                &mut used,
                SILENT,
            )
        } != 0
            || used != 64
        {
            return Err(Error::Denied("TPM CNG signing unavailable"));
        }
        Ok(URL_SAFE_NO_PAD.encode(super::raw_signature_to_der(&signature)?))
    }
}
