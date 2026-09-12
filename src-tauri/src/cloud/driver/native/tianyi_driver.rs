use super::*;
pub(super) struct TianyiDriver<'a> {
    pub(super) store: StoreRef<'a>,
    pub(super) http: HttpRef<'a>,
}
impl ProviderBackend for TianyiDriver<'_> {}
impl DriveOps for TianyiDriver<'_> {
    fn account(&mut self) -> Result<CloudAccount, CloudError> {
        let provider = CloudProvider::Tianyi;
        let store = &self.store;
        let http = &self.http;
        if store.load(provider.key())?.is_none() {
            return Err(CloudError::NotAuthorized);
        }
        {
            let session: tianyi::TianyiSession =
                load_provider_session(CloudProvider::Tianyi, store)?;
            let client = tianyi::TianyiClient::new(session.clone(), http);
            if session.auth_method == CloudAuthMethod::Web {
                client.web_account()
            } else {
                client.account()
            }
        }
    }
    fn list_directory(&mut self, parent_id: &str) -> Result<CloudDirectory, CloudError> {
        let provider = CloudProvider::Tianyi;
        let store = &self.store;
        let http = &self.http;
        if is_provider_recycle_bin(provider, parent_id) {
            return {
                tianyi::TianyiClient::new(
                    load_provider_session(CloudProvider::Tianyi, store)?,
                    http,
                )
                .list_recycle_bin()
            };
        }
        {
            tianyi::TianyiClient::new(load_provider_session(CloudProvider::Tianyi, store)?, http)
                .directory(parent_id)
        }
    }
    fn create_directory(&mut self, parent_id: &str, name: &str) -> Result<CloudFile, CloudError> {
        let store = &self.store;
        let http = &self.http;
        let h = http;
        {
            let client =
                tianyi::TianyiClient::new(load_provider_session(CloudProvider::Tianyi, store)?, h);
            client.mkdir(&parent_id, &name)?;
            find_created_directory(client.directory(&parent_id)?, &name)
        }
    }
    fn rename(&mut self, file_id: &str, name: &str) -> Result<(), CloudError> {
        let store = &self.store;
        let http = &self.http;
        let h = http;
        tianyi::TianyiClient::new(load_provider_session(CloudProvider::Tianyi, store)?, h)
            .rename(&file_id, &name)
    }
    fn move_files(
        &mut self,
        file_ids: &[String],
        target_parent_id: &str,
    ) -> Result<(), CloudError> {
        let store = &self.store;
        let http = &self.http;
        let h = http;
        tianyi::TianyiClient::new(load_provider_session(CloudProvider::Tianyi, store)?, h)
            .move_files(&file_ids, &target_parent_id)
    }
    fn copy_files(
        &mut self,
        file_ids: &[String],
        target_parent_id: &str,
    ) -> Result<(), CloudError> {
        let store = &self.store;
        let http = &self.http;
        let h = http;
        tianyi::TianyiClient::new(load_provider_session(CloudProvider::Tianyi, store)?, h)
            .copy_files(&file_ids, &target_parent_id)
    }
    fn trash(&mut self, file_ids: &[String]) -> Result<(), CloudError> {
        let store = &self.store;
        let http = &self.http;
        let h = http;
        {
            tianyi::TianyiClient::new(load_provider_session(CloudProvider::Tianyi, store)?, h)
                .trash(&file_ids)
        }
    }
    fn delete(&mut self, file_ids: &[String]) -> Result<(), CloudError> {
        let store = &self.store;
        let http = &self.http;
        {
            let client = tianyi::TianyiClient::new(
                load_provider_session(CloudProvider::Tianyi, store)?,
                http,
            );
            let recycle_bin = client.list_recycle_bin()?;
            validate_recycle_entry_ids(file_ids, &recycle_bin)?;
            client.delete_recycle_entries(file_ids)
        }
    }
    fn restore(&mut self, file_ids: &[String]) -> Result<(), CloudError> {
        let store = &self.store;
        let http = &self.http;
        {
            let client = tianyi::TianyiClient::new(
                load_provider_session(CloudProvider::Tianyi, store)?,
                http,
            );
            let recycle_bin = client.list_recycle_bin()?;
            validate_recycle_entry_ids(file_ids, &recycle_bin)?;
            client.restore_recycle_entries(file_ids)
        }
    }
}
impl DownloadOps for TianyiDriver<'_> {
    fn resolve_download(&mut self, file_id: &str) -> Result<ResolvedCloudDownload, CloudError> {
        let store = &self.store;
        let http = &self.http;
        {
            let download = tianyi::TianyiClient::new(
                load_provider_session(CloudProvider::Tianyi, store)?,
                http,
            )
            .download(file_id)?;
            Ok(ResolvedCloudDownload {
                url: download.url,
                name: download.name,
                headers: download.headers.into_iter().collect(),
            })
        }
    }
}
impl ProviderAuth for TianyiDriver<'_> {
    fn web_authorization_recipe(&self) -> WebAuthorizationRecipe {
        WebAuthorizationRecipe {
            browser_context: true,
            ..Default::default()
        }
    }
    fn complete_web(&mut self, input: &WebAuthorizationInput) -> Result<CloudAccount, CloudError> {
        let store = &self.store;
        let http = &self.http;
        {
            let mut session = tianyi::parse_tianyi_web_session(&input.cookies)?;
            session.apply_browser_context(&input.browser_context.clone().unwrap_or_default());
            let account = finalize_tianyi_web_authorization(
                crate::cloud::ProviderRegistry::global().get(CloudProvider::Tianyi),
                session,
                store,
                http,
            )?;
            Ok(account)
        }
    }
    fn login_password(
        &mut self,
        username: &str,
        password: &str,
    ) -> Result<NativeLoginOutcome, CloudError> {
        let provider = CloudProvider::Tianyi;
        let store = &self.store;
        let http = &self.http;
        let native_result = tianyi::TianyiClient::login_password(username, password, http);
        complete_native_password_login(native_result, |session| {
            let client = tianyi::TianyiClient::new(session.clone(), http);
            let account = client.account()?;
            client.directory(ProviderRegistry::global().get(provider).root_directory_id)?;
            save_provider_session(CloudProvider::Tianyi, store, &session)?;
            Ok(account)
        })
    }
    fn recover_password(
        &mut self,
        credential: &CloudPasswordCredential,
    ) -> Result<(CloudAccount, Vec<u8>), CloudError> {
        let http = &self.http;
        Ok({
            let session = tianyi::TianyiClient::login_password(
                credential.username(),
                credential.password(),
                http,
            )?;
            let client = tianyi::TianyiClient::new(session.clone(), http);
            let account = client.account()?;
            (
                account,
                serde_json::to_vec(&session).map_err(|_| CloudError::Storage)?,
            )
        })
    }
    fn validate_browser_password(
        &mut self,
        cookies: &[String],
        browser_context: Option<&serde_json::Value>,
        report_stage: &mut dyn FnMut(&'static str),
    ) -> Result<(CloudAccount, Vec<u8>), CloudError> {
        let provider = CloudProvider::Tianyi;
        let http = &self.http;
        Ok({
            let mut session = tianyi::parse_tianyi_web_session(cookies)?;
            if let Some(context) = browser_context {
                session.apply_browser_context(context);
            }
            if !session.has_browser_context() {
                report_stage("等待官网浏览器标识");
                return Err(QuarkError::AuthorizationPending);
            }
            session.auth_method = CloudAuthMethod::Password;
            let client = tianyi::TianyiClient::new(session.clone(), http);
            // A generic "tianyi-web" account is not enough for auto-renewal.
            report_stage("账号信息校验");
            let account = client.account()?;
            report_stage("目录访问校验");
            client.directory(ProviderRegistry::global().get(provider).root_directory_id)?;
            (
                account,
                serde_json::to_vec(&session).map_err(|_| QuarkError::Storage)?,
            )
        })
    }
    fn web_session_snapshot(
        &mut self,
        input: &WebAuthorizationInput,
    ) -> Result<Option<Vec<u8>>, CloudError> {
        let cookies = &input.cookies;
        Ok(Some({
            let mut session = tianyi::parse_tianyi_web_session(&cookies)?;
            session.apply_browser_context(&input.browser_context.clone().unwrap_or_default());
            serde_json::to_vec(&session).map_err(|_| QuarkError::Storage)?
        }))
    }
}
impl UploadOps for TianyiDriver<'_> {
    fn upload(
        &mut self,
        s: &CloudState,
        upload_http: &dyn UploadHttp,
        parent_id: &str,
        path: &Path,
    ) -> Result<CloudFile, CloudError> {
        let h = UploadHttpRef(upload_http);
        Ok(s.with_provider(CloudProvider::Tianyi, |store| {
            run_cloud_work(
                CloudProvider::Tianyi,
                SessionWorkKind::Mutation,
                store,
                &h,
                || {
                    let client = tianyi::TianyiClient::new(
                        load_provider_session(CloudProvider::Tianyi, store)?,
                        &h,
                    );
                    client.upload_file(&parent_id, &path, &h)
                },
            )
        })?)
    }
}
