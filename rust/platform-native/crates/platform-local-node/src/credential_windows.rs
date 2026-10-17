//! Current-user DPAPI sealed blobs in Windows Credential Manager; no disk fallback.
use super::{SecureCredentialStore, validate_name};
use crate::{Error, Result};
use std::{ffi::c_void, ptr};
#[repr(C)]
struct Blob {
    len: u32,
    data: *mut u8,
}
#[repr(C)]
struct Credential {
    flags: u32,
    kind: u32,
    target: *mut u16,
    comment: *mut u16,
    written: [u32; 2],
    len: u32,
    blob: *mut u8,
    persist: u32,
    attributes: u32,
    attribute: *mut c_void,
    alias: *mut u16,
    user: *mut u16,
}
#[link(name = "Advapi32")]
unsafe extern "system" {
    fn CredReadW(target: *const u16, kind: u32, flags: u32, out: *mut *mut Credential) -> i32;
    fn CredWriteW(value: *const Credential, flags: u32) -> i32;
    fn CredFree(value: *mut c_void);
}
#[link(name = "Crypt32")]
unsafe extern "system" {
    fn CryptProtectData(
        input: *const Blob,
        description: *const u16,
        entropy: *const Blob,
        reserved: *mut c_void,
        prompt: *mut c_void,
        flags: u32,
        output: *mut Blob,
    ) -> i32;
    fn CryptUnprotectData(
        input: *const Blob,
        description: *mut *mut u16,
        entropy: *const Blob,
        reserved: *mut c_void,
        prompt: *mut c_void,
        flags: u32,
        output: *mut Blob,
    ) -> i32;
}
#[link(name = "Kernel32")]
unsafe extern "system" {
    fn LocalFree(value: *mut c_void) -> *mut c_void;
    fn GetLastError() -> u32;
}
pub struct WindowsCredentialStore {
    service: String,
}
impl WindowsCredentialStore {
    pub fn new(service: String) -> Result<Self> {
        validate_name(&service)?;
        Ok(Self { service })
    }
    fn target(&self, reference: &str) -> Result<Vec<u16>> {
        validate_name(reference)?;
        Ok(format!("AgentForge/{}/{}\0", self.service, reference)
            .encode_utf16()
            .collect())
    }
    fn entropy(&self) -> Vec<u8> {
        format!("AgentForge credential v1\0{}", self.service).into_bytes()
    }
}
fn crypt(secret: &[u8], entropy: &[u8], protect: bool) -> Result<Vec<u8>> {
    let input = Blob {
        len: secret.len() as u32,
        data: secret.as_ptr() as *mut u8,
    };
    let entropy = Blob {
        len: entropy.len() as u32,
        data: entropy.as_ptr() as *mut u8,
    };
    let mut output = Blob {
        len: 0,
        data: ptr::null_mut(),
    };
    // CurrentUser scope: deliberately omit CRYPTPROTECT_LOCAL_MACHINE. UI forbidden.
    let ok = unsafe {
        if protect {
            CryptProtectData(
                &input,
                ptr::null(),
                &entropy,
                ptr::null_mut(),
                ptr::null_mut(),
                1,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                ptr::null_mut(),
                &entropy,
                ptr::null_mut(),
                ptr::null_mut(),
                1,
                &mut output,
            )
        }
    };
    if ok == 0 {
        return Err(Error::Denied("Windows protected credential unavailable"));
    }
    if output.data.is_null() || output.len > 16384 {
        if !output.data.is_null() {
            unsafe {
                LocalFree(output.data.cast());
            }
        }
        return Err(Error::Integrity);
    }
    let result = unsafe { std::slice::from_raw_parts(output.data, output.len as usize).to_vec() };
    unsafe {
        ptr::write_bytes(output.data, 0, output.len as usize);
        LocalFree(output.data.cast());
    }
    Ok(result)
}
impl SecureCredentialStore for WindowsCredentialStore {
    fn ready(&self) -> bool {
        crypt(&[0; 32], &self.entropy(), true)
            .and_then(|sealed| crypt(&sealed, &self.entropy(), false))
            .is_ok_and(|plain| plain == [0; 32])
    }
    fn load(&self, reference: &str) -> Result<Option<Vec<u8>>> {
        let target = self.target(reference)?;
        let mut value = ptr::null_mut();
        if unsafe { CredReadW(target.as_ptr(), 1, 0, &mut value) } == 0 {
            return if unsafe { GetLastError() } == 1168 {
                Ok(None)
            } else {
                Err(Error::Denied("Windows credential read failed"))
            };
        }
        if value.is_null() {
            return Err(Error::Integrity);
        }
        let result = unsafe {
            let credential = &*value;
            if credential.kind != 1
                || credential.len == 0
                || credential.len > 2560
                || credential.blob.is_null()
            {
                Err(Error::Integrity)
            } else {
                crypt(
                    std::slice::from_raw_parts(credential.blob, credential.len as usize),
                    &self.entropy(),
                    false,
                )
            }
        };
        unsafe {
            CredFree(value.cast());
        }
        result.map(Some)
    }
    fn store(&self, reference: &str, secret: &[u8]) -> Result<()> {
        if secret.is_empty() || secret.len() > 2048 {
            return Err(Error::Denied("Windows credential size"));
        }
        let mut target = self.target(reference)?;
        let mut sealed = crypt(secret, &self.entropy(), true)?;
        if sealed.len() > 2560 {
            return Err(Error::Denied("Windows credential size"));
        }
        let value = Credential {
            flags: 0,
            kind: 1,
            target: target.as_mut_ptr(),
            comment: ptr::null_mut(),
            written: [0; 2],
            len: sealed.len() as u32,
            blob: sealed.as_mut_ptr(),
            persist: 2,
            attributes: 0,
            attribute: ptr::null_mut(),
            alias: ptr::null_mut(),
            user: ptr::null_mut(),
        };
        let ok = unsafe { CredWriteW(&value, 0) };
        sealed.fill(0);
        if ok == 0 {
            return Err(Error::Denied("Windows credential store failed"));
        }
        if self.load(reference)?.as_deref() != Some(secret) {
            return Err(Error::Integrity);
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scope_and_reference_are_unambiguous() {
        let store = WindowsCredentialStore::new("identity.v1".into()).unwrap();
        assert!(store.target("bad/reference").is_err());
        assert!(store.target("bad\0reference").is_err());
        assert_ne!(
            store.entropy(),
            WindowsCredentialStore::new("credential.v2".into())
                .unwrap()
                .entropy()
        );
    }
}
