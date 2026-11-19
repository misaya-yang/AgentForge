//! Unix descriptor-relative containment; never canonicalize a model-supplied path.
use crate::{Error,Result};
use std::{ffi::CString,fs::File,io::Read,os::{fd::{AsRawFd,FromRawFd},unix::fs::MetadataExt},path::{Path,PathBuf}};
use unicode_normalization::UnicodeNormalization;
use unicode_casefold::UnicodeCaseFold;
pub struct Grant {pub id:String,pub tenant_id:String,pub user_id:String,root:PathBuf,device:u64,inode:u64,capabilities:std::collections::HashSet<String>,expires_at:Option<f64>,revoked:bool}
impl Grant {
 pub fn issue(id:String,root:&Path,tenant:String,user:String,capabilities:&[&str],ttl:Option<f64>)->Result<Self>{let meta=std::fs::symlink_metadata(root)?;if !root.is_absolute()||!meta.is_dir()||meta.file_type().is_symlink()||id.is_empty()||!id.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)||id.len()>200||!id.bytes().all(|b|b.is_ascii_alphanumeric()||b"._:-".contains(&b))||capabilities.is_empty()||capabilities.iter().any(|c|!["list","read","search","watch","write","rollback"].contains(c))||capabilities.contains(&"rollback")&&!capabilities.contains(&"write")||ttl.is_some_and(|v|!v.is_finite()||v<=0.){return Err(Error::Denied("invalid directory grant"))}let mut capabilities:std::collections::HashSet<String>=capabilities.iter().map(|s|s.to_string()).collect();if capabilities.contains("write"){capabilities.insert("rollback".into());}Ok(Self{id,tenant_id:tenant,user_id:user,root:root.canonicalize()?,device:meta.dev(),inode:meta.ino(),capabilities,expires_at:ttl.map(|v|crate::now()+v),revoked:false})}
 pub fn revoke(&mut self){self.revoked=true;}
 pub fn authorize(&self,capability:&str,tenant:&str,user:&str)->Result<()> {if self.revoked||self.expires_at.is_some_and(|t|crate::now()>=t)||!self.capabilities.contains(capability)||self.tenant_id!=tenant||self.user_id!=user{return Err(Error::Denied("directory grant scope"))}self.root_fd()?;Ok(())}
 fn root_fd(&self)->Result<File>{if self.revoked||self.expires_at.is_some_and(|t|crate::now()>=t){return Err(Error::Denied("directory grant unavailable"))}use std::os::unix::fs::OpenOptionsExt;let f=std::fs::OpenOptions::new().read(true).custom_flags(libc::O_DIRECTORY|libc::O_CLOEXEC|libc::O_NOFOLLOW).open(&self.root)?;let m=f.metadata()?;if m.dev()!=self.device||m.ino()!=self.inode{return Err(Error::Stale)}Ok(f)}
 pub fn open_dir(&self,path:&str)->Result<File>{let mut fd=self.root_fd()?;for p in safe_parts(path)?{fd=open_at(&fd,p,libc::O_RDONLY|libc::O_DIRECTORY,0)?;}Ok(fd)}
 pub(crate) fn parent(&self,path:&str)->Result<(File,CString)>{let parts=safe_parts(path)?;let (name,dirs)=parts.split_last().ok_or(Error::Denied("file path required"))?;let mut fd=self.root_fd()?;for dir in dirs {fd=open_at(&fd,dir,libc::O_RDONLY|libc::O_DIRECTORY,0)?;}Ok((fd,CString::new(*name).map_err(|_|Error::Denied("invalid path"))?))}
 pub fn read(&self,path:&str,limit:usize)->Result<Vec<u8>>{self.read_with_metadata(path,limit).map(|(bytes,_)|bytes)}
 pub fn read_with_metadata(&self,path:&str,limit:usize)->Result<(Vec<u8>,std::fs::Metadata)>{let(parent,name)=self.parent(path)?;let f=open_at(&parent,name.to_str().map_err(|_|Error::Denied("path encoding"))?,libc::O_RDONLY|libc::O_NONBLOCK,0)?;let m=f.metadata()?;if !m.is_file()||m.nlink()!=1||m.len()>limit as u64{return Err(Error::Denied("single-link regular file budget"))}let mut bytes=Vec::new();f.take(limit.saturating_add(1) as u64).read_to_end(&mut bytes)?;if bytes.len()>limit{return Err(Error::Denied("file exceeds read budget"))}Ok((bytes,m))}
}
fn promoted_open_mode(mode:u32)->Result<libc::c_uint>{
 let mode:libc::mode_t=mode.try_into().map_err(|_|Error::Denied("file mode is not representable"))?;
 mode.try_into().map_err(|_|Error::Denied("file mode is not representable"))
}
pub(crate) fn open_at(parent:&File,name:&str,flags:i32,mode:u32)->Result<File>{let name=CString::new(name).map_err(|_|Error::Denied("invalid path"))?;let mode=promoted_open_mode(mode)?;let fd=unsafe{libc::openat(parent.as_raw_fd(),name.as_ptr(),flags|libc::O_CLOEXEC|libc::O_NOFOLLOW,mode)};if fd<0{return Err(Error::Storage)}Ok(unsafe{File::from_raw_fd(fd)})}
pub fn safe_parts(path:&str)->Result<Vec<&str>>{if path.contains(['\0','\\',':'])||path.starts_with('/')||path.nfc().collect::<String>()!=path{return Err(Error::Denied("unsafe path"))}let parts:Vec<_>=path.split('/').filter(|s|!s.is_empty()&&*s!=".").collect();for p in &parts{let s=p.chars().case_fold().collect::<String>();if *p==".."||[".ssh",".gnupg",".aws",".azure",".kube",".docker","wallet","wallets","keychains","id_rsa","id_dsa","id_ecdsa","id_ed25519","credentials","credentials.json","application_default_credentials.json",".npmrc",".pypirc",".netrc",".git-credentials","pip.conf","nuget.config","login data","cookies","key4.db","logins.json","wallet.dat","seed","seed.txt","seed-phrase.txt","seed_phrase.txt","mnemonic","mnemonic.txt"].contains(&s.as_str())||s==".env"||s.starts_with(".env.")||[".pem",".p12",".pfx",".key"].iter().any(|x|s.ends_with(x))||["seed-phrase","seed_phrase","mnemonic"].iter().any(|x|s.contains(x)){return Err(Error::Denied("sensitive or unsafe path"))}}Ok(parts)}
#[cfg(test)]mod tests{use super::*;#[test]fn blocks_escape_and_secrets(){for p in ["../x","/tmp/x","C:\\x","a/.env","a/.ssh/key","wallet.dat","key.pem","e\u{301}.txt"]{assert!(safe_parts(p).is_err(),"{p}");}assert_eq!(safe_parts("src/main.rs").unwrap(),vec!["src","main.rs"]);}}

#[cfg(test)]
mod open_at_mode_tests {
    use super::*;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("open-at-mode-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn open_at_matches_other_unix_file_creation_permissions() {
        let temp = TempDir::new();
        let requested_mode = 0o640;
        let reference = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(requested_mode)
            .open(temp.0.join("reference"))
            .unwrap();
        let parent = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&temp.0)
            .unwrap();
        let created = open_at(
            &parent,
            "created",
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            requested_mode,
        )
        .unwrap();

        let expected = reference.metadata().unwrap().permissions().mode() & 0o7777;
        let actual = created.metadata().unwrap().permissions().mode() & 0o7777;
        assert_eq!(actual, expected);
    }

    #[test]
    fn open_at_mode_conversion_rejects_unrepresentable_values() {
        let max_mode = u32::try_from(libc::mode_t::MAX).unwrap_or(u32::MAX);
        if max_mode < u32::MAX {
            assert!(promoted_open_mode(max_mode + 1).is_err());
        }
        assert_eq!(promoted_open_mode(0o640).unwrap(), 0o640);
    }
}

#[derive(Default)]pub struct DirectoryGrantStore{grants:std::collections::HashMap<String,Grant>}
impl DirectoryGrantStore{
 pub fn issue(&mut self,root:&Path,capabilities:&[&str],ttl:Option<f64>,tenant:&str,user:&str,id:Option<&str>)->Result<&Grant>{let id=id.map(str::to_owned).unwrap_or_else(||format!("grant_{}",uuid::Uuid::new_v4().simple()));if self.grants.contains_key(&id){return Err(Error::Denied("duplicate grant identity"))}let grant=Grant::issue(id.clone(),root,tenant.into(),user.into(),capabilities,ttl)?;self.grants.insert(id.clone(),grant);self.grants.get(&id).ok_or(Error::Integrity)}
 pub fn get(&self,id:&str,capability:&str,tenant:&str,user:&str)->Result<&Grant>{let grant=self.grants.get(id).ok_or(Error::Denied("directory grant unavailable"))?;grant.authorize(capability,tenant,user)?;Ok(grant)}
 pub fn revoke(&mut self,id:&str){if let Some(grant)=self.grants.get_mut(id){grant.revoke();}}
 pub fn active(&self)->Vec<&Grant>{self.grants.values().filter(|g|g.root_fd().is_ok()).collect()}
}
