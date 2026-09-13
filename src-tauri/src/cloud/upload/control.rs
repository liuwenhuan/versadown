use super::{CloudProvider, QuarkError, UploadControl};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};

pub(crate) fn upload_cancelled() -> QuarkError {
    QuarkError::InvalidInput("上传已取消".into())
}
pub(crate) fn is_upload_cancelled(error: &QuarkError) -> bool {
    matches!(error, QuarkError::InvalidInput(message) if message == "上传已取消")
}

#[derive(Default, Clone)]
pub(crate) struct UploadRegistry(Arc<Mutex<HashMap<String, (CloudProvider, UploadControl)>>>);
pub(crate) struct UploadRegistration {
    registry: UploadRegistry,
    id: String,
    control: UploadControl,
}

impl UploadRegistry {
    pub fn register(
        &self,
        provider: CloudProvider,
        id: &str,
        control: UploadControl,
    ) -> Result<UploadRegistration, QuarkError> {
        if id.trim().is_empty() || id.len() > 128 {
            return Err(QuarkError::InvalidInput("上传任务标识无效".into()));
        }
        let mut registry = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if registry.contains_key(id) {
            return Err(QuarkError::InvalidInput("上传任务标识已存在".into()));
        }
        registry.insert(id.to_owned(), (provider, control.clone()));
        Ok(UploadRegistration {
            registry: self.clone(),
            id: id.to_owned(),
            control,
        })
    }
    pub fn apply(&self, provider: CloudProvider, id: &str, action: &str) -> Result<(), QuarkError> {
        let control = {
            let registry = self.0.lock().unwrap_or_else(|error| error.into_inner());
            let (owner, control) = registry
                .get(id)
                .ok_or_else(|| QuarkError::InvalidInput("上传任务已结束或不存在".into()))?;
            if *owner != provider {
                return Err(QuarkError::InvalidInput("上传任务的网盘不匹配".into()));
            }
            control.clone()
        };
        // Never hold the registry while interacting with a worker's control.
        match action {
            "pause" => control.pause(),
            "resume" => control.resume(),
            "cancel" => {
                control.cancel();
                Ok(())
            }
            _ => Err(QuarkError::InvalidInput("不支持的上传控制操作".into())),
        }
    }
}

impl Drop for UploadRegistration {
    fn drop(&mut self) {
        self.control.cancel();
        self.registry
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&self.id);
    }
}

pub(crate) fn upload_registry() -> &'static UploadRegistry {
    static REGISTRY: OnceLock<UploadRegistry> = OnceLock::new();
    REGISTRY.get_or_init(UploadRegistry::default)
}

#[cfg(test)]
mod upload_control_tests {
    use super::*;

    #[test]
    fn upload_control_registry_isolates_ids_rejects_wrong_provider_and_cleans_up() {
        let registry = UploadRegistry::default();
        let first = UploadControl::default();
        let second = UploadControl::default();
        let registration = registry
            .register(CloudProvider::Xunlei, "first", first.clone())
            .unwrap();
        let _other = registry
            .register(CloudProvider::Xunlei, "second", second.clone())
            .unwrap();
        assert!(registry
            .register(CloudProvider::Quark, "first", UploadControl::default())
            .is_err());
        assert!(registry
            .apply(CloudProvider::Quark, "first", "pause")
            .is_err());
        assert!(registry
            .apply(CloudProvider::Xunlei, "first", "invalid")
            .is_err());
        registry
            .apply(CloudProvider::Xunlei, "first", "pause")
            .unwrap();
        assert!(first.is_paused());
        assert!(!second.is_paused());
        registry
            .apply(CloudProvider::Xunlei, "first", "cancel")
            .unwrap();
        assert!(first.is_cancelled());
        assert!(!second.is_cancelled());
        assert!(registry
            .apply(CloudProvider::Xunlei, "first", "resume")
            .is_err());
        drop(registration);
        assert!(registry
            .apply(CloudProvider::Xunlei, "first", "cancel")
            .is_err());
    }
}
