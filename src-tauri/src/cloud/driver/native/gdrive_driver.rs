use super::*;
use crate::cloud::gdrive::api::GdriveClient;

/// The signed-in Google drive: browsing and downloading only (the OAuth scope
/// is `drive.readonly`), so every mutating operation keeps its
/// `ProviderUnsupported` default.
pub(super) struct GdriveDriver<'a> {
    pub(super) store: StoreRef<'a>,
    pub(super) http: HttpRef<'a>,
}

impl GdriveDriver<'_> {
    fn client(&self) -> GdriveClient<'_> {
        GdriveClient::new(&self.http, &self.store)
    }
}

impl ProviderBackend for GdriveDriver<'_> {}
impl DriveOps for GdriveDriver<'_> {
    fn account(&mut self) -> Result<CloudAccount, CloudError> {
        self.client().account()
    }
    fn storage_capacity(&mut self) -> Result<CloudStorageCapacity, CloudError> {
        self.client().storage_capacity()
    }
    fn list_directory(&mut self, parent: &str) -> Result<CloudDirectory, CloudError> {
        self.client().list(parent)
    }
}
impl DownloadOps for GdriveDriver<'_> {
    fn resolve_download(&mut self, file: &str) -> Result<ResolvedCloudDownload, CloudError> {
        self.client().download(file)
    }
}
impl ShareOps for GdriveDriver<'_> {}
impl UploadOps for GdriveDriver<'_> {}
impl ProviderAuth for GdriveDriver<'_> {
    fn refresh_session(&mut self) -> Result<bool, CloudError> {
        if self.store.load(CloudProvider::Gdrive.key())?.is_none() {
            return Ok(false);
        }
        self.client().refresh_session()?;
        Ok(true)
    }
}
