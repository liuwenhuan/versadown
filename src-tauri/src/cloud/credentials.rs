use super::QuarkError;
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};
pub trait CredentialStore: Send + Sync {
    /// Snapshot consumers delegate renewal to AuthSupervisor.
    fn automatic_session_recovery(&self) -> bool {
        true
    }
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
    #[cfg(test)]
    publish_failpoint: Option<fn(&std::path::Path, &std::path::Path) -> std::io::Result<()>>,
}

pub(crate) const BAIDU_BROWSER_PASSWORD_PREFERENCE_KEY: &str =
    "cloud-password-baidu-browser-preferred";
pub(crate) const BAIDU_BROWSER_PROFILE_USERNAME_KEY: &str =
    "cloud-password-baidu-browser-profile-username";

pub(crate) fn password_profile_reset_key(provider: super::CloudProvider) -> Option<&'static str> {
    match provider {
        super::CloudProvider::Baidu => Some("cloud-password-profile-reset-baidu"),
        super::CloudProvider::Tianyi => Some("cloud-password-profile-reset-tianyi"),
        _ => None,
    }
}

pub fn credential_key_is_supported(key: &str) -> bool {
    matches!(
        key,
        "quark"
            | "pikpak"
            | "baidu"
            | "tianyi"
            | "aliyun"
            | "xunlei"
            | "uc"
            | "ai-api-key"
            | "cloud-share-plans-v1"
            | "cloud-password-pikpak"
            | "cloud-password-baidu"
            | "cloud-password-tianyi"
            | "cloud-password-draft-pikpak"
            | "cloud-password-draft-baidu"
            | "cloud-password-draft-tianyi"
            | BAIDU_BROWSER_PASSWORD_PREFERENCE_KEY
            | BAIDU_BROWSER_PROFILE_USERNAME_KEY
            | "cloud-password-profile-reset-baidu"
            | "cloud-password-profile-reset-tianyi"
            | "cloud-auth-maintenance-quark"
            | "cloud-auth-maintenance-pikpak"
            | "cloud-auth-maintenance-baidu"
            | "cloud-auth-maintenance-tianyi"
            | "cloud-auth-maintenance-aliyun"
            | "cloud-auth-maintenance-xunlei"
            | "cloud-auth-maintenance-uc"
    )
}

impl DpapiCredentialStore {
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
    pub fn new(root: PathBuf) -> Result<Self, QuarkError> {
        std::fs::create_dir_all(&root).map_err(|_| QuarkError::Storage)?;
        Ok(Self {
            root,
            #[cfg(test)]
            publish_failpoint: None,
        })
    }
    #[cfg(test)]
    fn with_publish_failpoint(
        root: PathBuf,
        publish_failpoint: fn(&std::path::Path, &std::path::Path) -> std::io::Result<()>,
    ) -> Result<Self, QuarkError> {
        std::fs::create_dir_all(&root).map_err(|_| QuarkError::Storage)?;
        Ok(Self {
            root,
            publish_failpoint: Some(publish_failpoint),
        })
    }
    fn path(&self, key: &str) -> Result<PathBuf, QuarkError> {
        if !credential_key_is_supported(key) {
            return Err(QuarkError::Storage);
        }
        Ok(self.root.join(format!("{key}.bin")))
    }
    fn create_temp_file(&self, target: &Path) -> Result<(PathBuf, std::fs::File), QuarkError> {
        let parent = target.parent().ok_or(QuarkError::Storage)?;
        let filename = target
            .file_name()
            .ok_or(QuarkError::Storage)?
            .to_string_lossy();
        for _ in 0..16 {
            let temp = parent.join(format!(".{filename}.tmp-{}", uuid::Uuid::new_v4()));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
            {
                Ok(file) => return Ok((temp, file)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(QuarkError::Storage),
            }
        }
        Err(QuarkError::Storage)
    }
    fn write_temp_file(&self, target: &Path, ciphertext: &[u8]) -> Result<PathBuf, QuarkError> {
        let (temp, mut file) = self.create_temp_file(target)?;
        let result = file.write_all(ciphertext).and_then(|()| file.sync_all());
        drop(file);
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
            return Err(QuarkError::Storage);
        }
        Ok(temp)
    }
    fn publish(&self, temp: &Path, target: &Path) -> std::io::Result<()> {
        #[cfg(test)]
        if let Some(publish_failpoint) = self.publish_failpoint {
            return publish_failpoint(temp, target);
        }
        publish_file(temp, target)
    }
}
impl CredentialStore for DpapiCredentialStore {
    fn save(&self, k: &str, v: &[u8]) -> Result<(), QuarkError> {
        let path = self.path(k)?;
        let bytes = protect(v, true)?;
        let temp = self.write_temp_file(&path, &bytes)?;
        match self.publish(&temp, &path) {
            Ok(()) => Ok(()),
            Err(_) => {
                let _ = std::fs::remove_file(temp);
                Err(QuarkError::Storage)
            }
        }
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
pub(crate) fn protect(input: &[u8], encrypt: bool) -> Result<Vec<u8>, QuarkError> {
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
pub(crate) fn protect(_: &[u8], _: bool) -> Result<Vec<u8>, QuarkError> {
    Err(QuarkError::Storage)
}

#[cfg(windows)]
fn publish_file(temp: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        core::PCWSTR,
        Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        },
    };

    let temp = temp
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let target = target
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    unsafe {
        MoveFileExW(
            PCWSTR(temp.as_ptr()),
            PCWSTR(target.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
        .map_err(|_| std::io::Error::last_os_error())
    }
}

#[cfg(not(windows))]
fn publish_file(temp: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::rename(temp, target)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::{
        io,
        sync::atomic::{AtomicBool, Ordering},
    };

    static TEMP_CONTAINS_PLAINTEXT: AtomicBool = AtomicBool::new(false);

    fn fail_after_inspecting_temp(temp: &std::path::Path, _: &std::path::Path) -> io::Result<()> {
        let ciphertext = std::fs::read(temp)?;
        TEMP_CONTAINS_PLAINTEXT.store(
            ciphertext
                .windows(b"new-plan-secret".len())
                .any(|bytes| bytes == b"new-plan-secret"),
            Ordering::SeqCst,
        );
        Err(io::Error::other("publish failed"))
    }

    #[test]
    fn dpapi_publish_failure_cannot_replace_existing_journal_or_leave_a_plaintext_temp() {
        TEMP_CONTAINS_PLAINTEXT.store(false, Ordering::SeqCst);
        let directory = tempfile::tempdir().unwrap();
        let original = DpapiCredentialStore::new(directory.path().into()).unwrap();
        original
            .save("cloud-share-plans-v1", b"old-plan-secret")
            .unwrap();
        let failing = DpapiCredentialStore::with_publish_failpoint(
            directory.path().into(),
            fail_after_inspecting_temp,
        )
        .unwrap();

        assert!(matches!(
            failing.save("cloud-share-plans-v1", b"new-plan-secret"),
            Err(QuarkError::Storage)
        ));
        assert_eq!(
            original.load("cloud-share-plans-v1").unwrap(),
            Some(b"old-plan-secret".to_vec())
        );
        assert!(!TEMP_CONTAINS_PLAINTEXT.load(Ordering::SeqCst));
        assert!(std::fs::read_dir(directory.path())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp-")));
    }

    #[test]
    fn dpapi_successful_overwrite_replaces_the_journal_value() {
        let directory = tempfile::tempdir().unwrap();
        let store = DpapiCredentialStore::new(directory.path().into()).unwrap();
        store
            .save("cloud-share-plans-v1", b"old-plan-secret")
            .unwrap();
        store
            .save("cloud-share-plans-v1", b"new-plan-secret")
            .unwrap();

        assert_eq!(
            store.load("cloud-share-plans-v1").unwrap(),
            Some(b"new-plan-secret".to_vec())
        );
    }

    #[test]
    fn dpapi_encrypts_and_restores_password_login_drafts() {
        let directory = tempfile::tempdir().unwrap();
        let store = DpapiCredentialStore::new(directory.path().into()).unwrap();
        for provider in ["pikpak", "baidu", "tianyi"] {
            let key = format!("cloud-password-draft-{provider}");
            store.save(&key, b"username-and-password").unwrap();
            assert_eq!(
                store.load(&key).unwrap(),
                Some(b"username-and-password".to_vec())
            );
            let raw = std::fs::read(directory.path().join(format!("{key}.bin"))).unwrap();
            assert!(!raw
                .windows(b"username-and-password".len())
                .any(|bytes| bytes == b"username-and-password"));
        }
    }

    #[test]
    fn dpapi_concurrent_different_keys_keep_each_credential_readable() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().to_path_buf();
        std::thread::scope(|scope| {
            let quark_root = root.clone();
            scope.spawn(move || {
                let store = DpapiCredentialStore::new(quark_root).unwrap();
                store.save("quark", b"quark-secret").unwrap();
            });
            scope.spawn(move || {
                let store = DpapiCredentialStore::new(root).unwrap();
                store.save("pikpak", b"pikpak-secret").unwrap();
            });
        });
        let store = DpapiCredentialStore::new(directory.path().into()).unwrap();

        assert_eq!(store.load("quark").unwrap(), Some(b"quark-secret".to_vec()));
        assert_eq!(
            store.load("pikpak").unwrap(),
            Some(b"pikpak-secret".to_vec())
        );
    }
}
