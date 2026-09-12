use super::*;
pub(super) struct XunleiDriver<'a> {
    pub(super) store: StoreRef<'a>,
    pub(super) http: HttpRef<'a>,
}
impl ProviderBackend for XunleiDriver<'_> {}
impl DriveOps for XunleiDriver<'_> {
    fn account(&mut self) -> Result<CloudAccount, CloudError> {
        let provider = CloudProvider::Xunlei;
        let store = &self.store;
        let http = &self.http;
        if store.load(provider.key())?.is_none() {
            return Err(CloudError::NotAuthorized);
        }
        with_xunlei_client(store, http, |client| {
            if client.session().auth_method == CloudAuthMethod::Web
                && client.session().profile.client_secret.is_empty()
            {
                // Validate the root before restoring the workspace. The helper
                // also preserves modern refresh rotation when this probe fails.
                client.directory(ProviderRegistry::global().get(provider).root_directory_id)?;
                return client.web_account();
            }
            client.account()
        })
    }
    fn list_directory(&mut self, parent_id: &str) -> Result<CloudDirectory, CloudError> {
        let provider = CloudProvider::Xunlei;
        let store = &self.store;
        let http = &self.http;
        if is_provider_recycle_bin(provider, parent_id) {
            return with_xunlei_client(store, http, |client| client.list_recycle_bin());
        }
        {
            with_xunlei_client(store, http, |client| client.directory(parent_id))
        }
    }
    fn create_directory(&mut self, parent_id: &str, name: &str) -> Result<CloudFile, CloudError> {
        let store = &self.store;
        let http = &self.http;
        let h = http;
        with_xunlei_client(store, h, |client| {
            client.mkdir(&parent_id, &name)?;
            find_created_directory(client.directory(&parent_id)?, &name)
        })
    }
    fn rename(&mut self, file_id: &str, name: &str) -> Result<(), CloudError> {
        let store = &self.store;
        let http = &self.http;
        let h = http;
        {
            with_xunlei_client(store, h, |client| client.rename(&file_id, &name))
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
        with_xunlei_client(store, h, |client| {
            client.move_files(&file_ids, &target_parent_id)
        })
    }
    fn copy_files(
        &mut self,
        file_ids: &[String],
        target_parent_id: &str,
    ) -> Result<(), CloudError> {
        let store = &self.store;
        let http = &self.http;
        let h = http;
        with_xunlei_client(store, h, |client| {
            client.copy_files(&file_ids, &target_parent_id)
        })
    }
    fn trash(&mut self, file_ids: &[String]) -> Result<(), CloudError> {
        let store = &self.store;
        let http = &self.http;
        let h = http;
        with_xunlei_client(store, h, |client| client.trash(&file_ids))
    }
    fn delete(&mut self, file_ids: &[String]) -> Result<(), CloudError> {
        let store = &self.store;
        let http = &self.http;
        with_xunlei_client(store, http, |client| {
            let recycle_bin = client.list_recycle_bin()?;
            validate_recycle_entry_ids(file_ids, &recycle_bin)?;
            client.delete_recycle_entries(file_ids)
        })
    }
    fn restore(&mut self, file_ids: &[String]) -> Result<(), CloudError> {
        let store = &self.store;
        let http = &self.http;
        with_xunlei_client(store, http, |client| {
            let recycle_bin = client.list_recycle_bin()?;
            validate_recycle_entry_ids(file_ids, &recycle_bin)?;
            client.restore_recycle_entries(file_ids)
        })
    }
}
impl DownloadOps for XunleiDriver<'_> {
    fn resolve_download(&mut self, file_id: &str) -> Result<ResolvedCloudDownload, CloudError> {
        let store = &self.store;
        let http = &self.http;
        with_xunlei_client(store, http, |client| {
            let (url, name) = client.download(file_id)?;
            // Keep the authenticated browser's public identity on the CDN
            // request, but never forward bearer/captcha/cookies off the API.
            let session = client.session();
            let mut headers = Vec::new();
            if !session.profile.user_agent.is_empty() {
                headers.push(("User-Agent".into(), session.profile.user_agent.clone()));
            }
            if session.profile.is_modern_web() {
                headers.push(("Referer".into(), "https://pan.xunlei.com/".into()));
            }
            Ok(ResolvedCloudDownload { url, name, headers })
        })
    }
}
impl ProviderAuth for XunleiDriver<'_> {
    fn web_authorization_recipe(&self) -> WebAuthorizationRecipe {
        WebAuthorizationRecipe {
            token_script: Some(XUNLEI_WEB_TOKENS_JS),
            asynchronous_tokens: true,
            ..Default::default()
        }
    }
    fn complete_web(&mut self, input: &WebAuthorizationInput) -> Result<CloudAccount, CloudError> {
        let store = &self.store;
        let http = &self.http;
        {
            let (session, captured_context) =
                parse_xunlei_web_authorization(input.storage.clone())?;
            let account = finalize_xunlei_web_authorization(
                crate::cloud::ProviderRegistry::global().get(CloudProvider::Xunlei),
                session,
                captured_context,
                store,
                http,
            )?;
            Ok(account)
        }
    }
}
impl UploadOps for XunleiDriver<'_> {
    fn upload(
        &mut self,
        s: &CloudState,
        upload_http: &dyn UploadHttp,
        parent_id: &str,
        path: &Path,
    ) -> Result<CloudFile, CloudError> {
        let h = UploadHttpRef(upload_http);
        Ok(s.with_provider(CloudProvider::Xunlei, |store| {
            run_cloud_work(
                CloudProvider::Xunlei,
                SessionWorkKind::Mutation,
                store,
                &h,
                || {
                    let session = load_provider_session(CloudProvider::Xunlei, store)?;
                    let client = xunlei::XunleiClient::new(session, &h);
                    let file = client.upload_file(&parent_id, &path)?;
                    save_provider_session(CloudProvider::Xunlei, store, &client.session())?;
                    Ok(file)
                },
            )
        })?)
    }
}
