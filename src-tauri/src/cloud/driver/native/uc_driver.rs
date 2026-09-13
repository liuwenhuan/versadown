use super::*;
pub(super) struct UcDriver<'a> {
    pub(super) store: StoreRef<'a>,
    pub(super) http: HttpRef<'a>,
}
impl ProviderBackend for UcDriver<'_> {}
impl DriveOps for UcDriver<'_> {
    fn account(&mut self) -> Result<CloudAccount, CloudError> {
        let provider = CloudProvider::Uc;
        let store = &self.store;
        let http = &self.http;
        if store.load(provider.key())?.is_none() {
            return Err(CloudError::NotAuthorized);
        }
        {
            let session = load_provider_session(CloudProvider::Uc, store)?;
            // A browser-granted UC session is sufficient to restore its tab.
            // File operations still validate the session against UC when the
            // user opens it, but an internal member probe must not hide a
            // previously authorized account at startup.
            uc::UcClient::new(session, http).web_account()
        }
    }
    fn list_directory(&mut self, parent_id: &str) -> Result<CloudDirectory, CloudError> {
        let provider = CloudProvider::Uc;
        let store = &self.store;
        let http = &self.http;
        if is_provider_recycle_bin(provider, parent_id) {
            return {
                uc::UcClient::new(load_provider_session(CloudProvider::Uc, store)?, http)
                    .list_recycle_bin()
            };
        }
        {
            uc::UcClient::new(load_provider_session(CloudProvider::Uc, store)?, http)
                .directory(parent_id)
        }
    }
    fn create_directory(&mut self, parent_id: &str, name: &str) -> Result<CloudFile, CloudError> {
        let store = &self.store;
        let http = &self.http;
        let h = http;
        {
            let client = uc::UcClient::new(load_provider_session(CloudProvider::Uc, store)?, h);
            let created = client.mkdir(&parent_id, &name)?;
            for attempt in 0..20 {
                if let Some(directory) = client
                    .directory(&parent_id)?
                    .items
                    .into_iter()
                    .find(|file| file.kind == "folder" && file.id == created.id)
                {
                    return Ok(directory);
                }
                if attempt < 19 {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
            }
            Err(CloudError::InvalidResponse)
        }
    }
    fn rename(&mut self, file_id: &str, name: &str) -> Result<(), CloudError> {
        let store = &self.store;
        let http = &self.http;
        let h = http;
        {
            uc::UcClient::new(load_provider_session(CloudProvider::Uc, store)?, h)
                .rename(&file_id, &name)
                .map(|_| ())
        }
    }
    fn move_files(
        &mut self,
        file_ids: &[String],
        target_parent_id: &str,
    ) -> Result<(), CloudError> {
        let store = &self.store;
        let http = &self.http;
        let h = http;
        {
            uc::UcClient::new(load_provider_session(CloudProvider::Uc, store)?, h)
                .move_files(&file_ids, &target_parent_id)
                .map(|_| ())
        }
    }
    fn copy_files(
        &mut self,
        file_ids: &[String],
        target_parent_id: &str,
    ) -> Result<(), CloudError> {
        let store = &self.store;
        let http = &self.http;
        let h = http;
        {
            uc::UcClient::new(load_provider_session(CloudProvider::Uc, store)?, h)
                .copy_files(&file_ids, &target_parent_id)
                .map(|_| ())
        }
    }
    fn trash(&mut self, file_ids: &[String]) -> Result<(), CloudError> {
        let store = &self.store;
        let http = &self.http;
        let h = http;
        uc::UcClient::new(load_provider_session(CloudProvider::Uc, store)?, h)
            .trash(&file_ids)
            .map(|_| ())
    }
    fn delete(&mut self, file_ids: &[String]) -> Result<(), CloudError> {
        let store = &self.store;
        let http = &self.http;
        {
            let client = uc::UcClient::new(load_provider_session(CloudProvider::Uc, store)?, http);
            let recycle_bin = client.list_recycle_bin()?;
            validate_recycle_entry_ids(file_ids, &recycle_bin)?;
            client.delete_recycle_entries(file_ids)
        }
    }
    fn restore(&mut self, file_ids: &[String]) -> Result<(), CloudError> {
        let store = &self.store;
        let http = &self.http;
        {
            let client = uc::UcClient::new(load_provider_session(CloudProvider::Uc, store)?, http);
            let recycle_bin = client.list_recycle_bin()?;
            validate_recycle_entry_ids(file_ids, &recycle_bin)?;
            client.restore_recycle_entries(file_ids)
        }
    }
}
impl DownloadOps for UcDriver<'_> {
    fn resolve_download(&mut self, file_id: &str) -> Result<ResolvedCloudDownload, CloudError> {
        let store = &self.store;
        let http = &self.http;
        {
            let download =
                uc::UcClient::new(load_provider_session(CloudProvider::Uc, store)?, http)
                    .download(file_id)?;
            Ok(ResolvedCloudDownload {
                url: download.url,
                name: download.name,
                headers: download.headers.into_iter().collect(),
            })
        }
    }
}
impl ProviderAuth for UcDriver<'_> {
    fn complete_web(&mut self, input: &WebAuthorizationInput) -> Result<CloudAccount, CloudError> {
        let store = &self.store;
        let http = &self.http;
        {
            let session = uc::parse_uc_web_session(&input.cookies)?;
            let account = finalize_uc_web_authorization(
                crate::cloud::ProviderRegistry::global().get(CloudProvider::Uc),
                session,
                store,
                http,
            )?;
            Ok(account)
        }
    }
    fn web_session_snapshot(
        &mut self,
        input: &WebAuthorizationInput,
    ) -> Result<Option<Vec<u8>>, CloudError> {
        let cookies = &input.cookies;
        Ok(Some(
            serde_json::to_vec(&uc::parse_uc_web_session(&cookies)?)
                .map_err(|_| QuarkError::Storage)?,
        ))
    }
}
impl UploadOps for UcDriver<'_> {
    fn upload(
        &mut self,
        s: &CloudState,
        upload_http: &dyn UploadHttp,
        parent_id: &str,
        path: &Path,
    ) -> Result<CloudFile, CloudError> {
        let h = UploadHttpRef(upload_http);
        s.with_upload_session(CloudProvider::Uc, &h, |store| {
            uc::UcClient::new(load_provider_session(CloudProvider::Uc, store)?, &h)
                .upload_file(&parent_id, &path, &h)
        })
    }
}
