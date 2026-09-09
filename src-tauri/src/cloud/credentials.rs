use super::QuarkError;
use std::{collections::BTreeMap, path::PathBuf, sync::Mutex};
pub trait CredentialStore: Send + Sync {
    fn save(&self, key: &str, value: &[u8]) -> Result<(), QuarkError>;
    fn load(&self, key: &str) -> Result<Option<Vec<u8>>, QuarkError>;
    fn remove(&self, key: &str) -> Result<(), QuarkError>;
}
#[derive(Default)]
pub struct MemoryCredentialStore(Mutex<BTreeMap<String, Vec<u8>>>);
impl CredentialStore for MemoryCredentialStore {
    fn save(&self, k: &str, v: &[u8]) -> Result<(), QuarkError> {
        self.0
            .lock()
            .map_err(|_| QuarkError::Storage)?
            .insert(k.into(), v.into());
        Ok(())
    }
    fn load(&self, k: &str) -> Result<Option<Vec<u8>>, QuarkError> {
        Ok(self
            .0
            .lock()
            .map_err(|_| QuarkError::Storage)?
            .get(k)
            .cloned())
    }
    fn remove(&self, k: &str) -> Result<(), QuarkError> {
        self.0.lock().map_err(|_| QuarkError::Storage)?.remove(k);
        Ok(())
    }
}
pub struct DpapiCredentialStore {
    root: PathBuf,
}
impl DpapiCredentialStore {
    pub fn new(root: PathBuf) -> Result<Self, QuarkError> {
        std::fs::create_dir_all(&root).map_err(|_| QuarkError::Storage)?;
        Ok(Self { root })
    }
    fn path(&self, key: &str) -> Result<PathBuf, QuarkError> {
        if !matches!(
            key,
            "quark" | "pikpak" | "baidu" | "tianyi" | "aliyun" | "xunlei" | "uc" | "ai-api-key"
        ) {
            return Err(QuarkError::Storage);
        }
        Ok(self.root.join(format!("{key}.bin")))
    }
}
impl CredentialStore for DpapiCredentialStore {
    fn save(&self, k: &str, v: &[u8]) -> Result<(), QuarkError> {
        let path = self.path(k)?;
        let bytes = protect(v, true)?;
        std::fs::write(path, bytes).map_err(|_| QuarkError::Storage)
    }
    fn load(&self, k: &str) -> Result<Option<Vec<u8>>, QuarkError> {
        match std::fs::read(self.path(k)?) {
            Ok(b) => protect(&b, false).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(QuarkError::Storage),
        }
    }
    fn remove(&self, k: &str) -> Result<(), QuarkError> {
        match std::fs::remove_file(self.path(k)?) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(QuarkError::Storage),
        }
    }
}
#[cfg(windows)]
fn protect(input: &[u8], encrypt: bool) -> Result<Vec<u8>, QuarkError> {
    use windows::Win32::{
        Foundation::{LocalFree, HLOCAL},
        Security::Cryptography::{
            CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
        },
    };
    let blob = CRYPT_INTEGER_BLOB {
        cbData: input.len().try_into().map_err(|_| QuarkError::Storage)?,
        pbData: input.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // DPAPI binds ciphertext to the current Windows user.
    unsafe {
        if encrypt {
            CryptProtectData(
                &blob,
                windows::core::PCWSTR::null(),
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &blob,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
        .map_err(|_| QuarkError::Storage)?;
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        Ok(bytes)
    }
}
#[cfg(not(windows))]
fn protect(_: &[u8], _: bool) -> Result<Vec<u8>, QuarkError> {
    Err(QuarkError::Storage)
}
