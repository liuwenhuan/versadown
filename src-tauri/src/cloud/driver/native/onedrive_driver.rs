use super::*;
use crate::cloud::onedrive::OnedriveClient;

/// Read-only personal OneDrive. Sign-in itself (browser + loopback) is driven
/// by the command layer; the driver only renews and reads.
pub(super) struct OnedriveDriver<'a> {
    pub(super) store: StoreRef<'a>,
    pub(super) http: HttpRef<'a>,
}

impl OnedriveDriver<'_> {
    fn client(&self) -> OnedriveClient<'_> {
        OnedriveClient::new(&self.http, &self.store)
    }
}

impl ProviderBackend for OnedriveDriver<'_> {}
impl DriveOps for OnedriveDriver<'_> {
    fn account(&mut self) -> Result<CloudAccount, CloudError> {
        self.client().account()
    }
    fn storage_capacity(&mut self) -> Result<CloudStorageCapacity, CloudError> {
        self.client().storage_capacity()
    }
    fn list_directory(&mut self, parent_id: &str) -> Result<CloudDirectory, CloudError> {
        self.client().list(parent_id)
    }
}
impl DownloadOps for OnedriveDriver<'_> {
    fn resolve_download(&mut self, file_id: &str) -> Result<ResolvedCloudDownload, CloudError> {
        let (url, name) = self.client().download(file_id)?;
        Ok(ResolvedCloudDownload {
            url,
            name,
            // The URL is pre-authorized; sending the Graph token would leak it.
            headers: Vec::new(),
            transform: None,
        })
    }
}
impl ShareOps for OnedriveDriver<'_> {}
impl UploadOps for OnedriveDriver<'_> {}
impl ProviderAuth for OnedriveDriver<'_> {
    fn refresh_session(&mut self) -> Result<bool, CloudError> {
        self.client().refresh_session()
    }
}
