use std::sync::Mutex;

use zeroize::Zeroize;

use crate::cloud::CredentialStore;

pub const PASSWORD_SECRET_NAME: &str = "privacy-download-password-v1";

#[derive(Default)]
pub struct PrivacySession {
    unlocked: Mutex<bool>,
}

impl PrivacySession {
    pub fn is_unlocked(&self) -> bool {
        self.unlocked.lock().map(|value| *value).unwrap_or(false)
    }

    pub fn lock(&self) {
        if let Ok(mut unlocked) = self.unlocked.lock() {
            *unlocked = false;
        }
    }

    pub fn unlock(
        &self,
        credentials: &dyn CredentialStore,
        password: String,
    ) -> Result<bool, String> {
        let valid = verify_password(credentials, password)?;
        if valid {
            *self
                .unlocked
                .lock()
                .map_err(|_| "隐私会话不可用".to_string())? = true;
        }
        Ok(valid)
    }
}

pub fn configure_password(
    credentials: &dyn CredentialStore,
    mut password: String,
) -> Result<(), String> {
    if password.trim().is_empty() {
        password.zeroize();
        return Err("隐私模式密码不能为空".into());
    }
    let result = credentials
        .save(PASSWORD_SECRET_NAME, password.as_bytes())
        .map_err(|_| "无法安全保存隐私模式密码".to_string());
    password.zeroize();
    result
}

pub fn password_is_configured(credentials: &dyn CredentialStore) -> Result<bool, String> {
    credentials
        .load(PASSWORD_SECRET_NAME)
        .map(|password| password.is_some())
        .map_err(|_| "无法读取隐私模式密码".to_string())
}

fn verify_password(
    credentials: &dyn CredentialStore,
    mut password: String,
) -> Result<bool, String> {
    let mut saved = credentials
        .load(PASSWORD_SECRET_NAME)
        .map_err(|_| "无法读取隐私模式密码".to_string())?
        .ok_or_else(|| "请先设置隐私模式密码".to_string())?;
    let is_match = constant_time_equal(saved.as_slice(), password.as_bytes());
    saved.zeroize();
    password.zeroize();
    Ok(is_match)
}

fn constant_time_equal(expected: &[u8], candidate: &[u8]) -> bool {
    let mut difference = expected.len() ^ candidate.len();
    let longest = expected.len().max(candidate.len());
    for index in 0..longest {
        difference |=
            usize::from(*expected.get(index).unwrap_or(&0) ^ *candidate.get(index).unwrap_or(&0));
    }
    difference == 0
}
