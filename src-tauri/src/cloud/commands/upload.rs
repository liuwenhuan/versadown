use super::*;

pub(crate) fn select_cloud_upload_paths(
    descriptor: &ProviderDescriptor,
    paths: Option<Vec<String>>,
    pick_files: impl FnOnce(String) -> Result<Vec<PathBuf>, QuarkError>,
) -> Result<Vec<PathBuf>, QuarkError> {
    match paths {
        Some(paths) => Ok(paths.into_iter().map(PathBuf::from).collect()),
        None => pick_files(format!(
            "选择上传到 {} 的文件",
            descriptor.upload_display_name
        )),
    }
}

#[tauri::command]
pub async fn cloud_upload_files(
    app: AppHandle,
    provider: CloudProvider,
    parent_id: String,
    paths: Option<Vec<String>>,
) -> Result<Vec<CloudFile>, QuarkError> {
    let picker = app.clone();
    let descriptor = ProviderRegistry::global().get(provider);
    // The native picker may stay open indefinitely; it must hold no session lock.
    let paths = tauri::async_runtime::spawn_blocking(move || {
        select_cloud_upload_paths(descriptor, paths, |title| {
            picker
                .dialog()
                .file()
                .set_title(title)
                .blocking_pick_files()
                .unwrap_or_default()
                .into_iter()
                .map(|path| {
                    path.into_path()
                        .map_err(|_| QuarkError::InvalidInput("无法访问选中的文件".into()))
                })
                .collect()
        })
    })
    .await
    .map_err(|_| QuarkError::InvalidInput("文件选择中断".into()))??;
    tauri::async_runtime::spawn_blocking(move || {
        let s = app.state::<Arc<CloudState>>();
        let h = ReqwestTransport::new()?;
        let mut files = Vec::new();
        for path in paths {
            let file = ProviderRegistry::global()
                .driver(provider, &s.store, &h)
                .upload(&s, &h, &parent_id, &path)?;
            files.push(file);
        }
        Ok(files)
    })
    .await
    .map_err(|_| QuarkError::Transport("上传任务中断".into()))?
}

impl CloudState {
    pub fn upload_pikpak_file(
        &self,
        http: &(impl CloudHttpTransport + CloudUploadTransport),
        parent: &str,
        path: &Path,
    ) -> Result<CloudFile, QuarkError> {
        crate::cloud::upload_pikpak_file(http, parent, path, |payload| {
            self.initialize_pikpak_upload(http, payload)
        })
    }
    fn initialize_pikpak_upload(
        &self,
        http: &impl CloudHttpTransport,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, QuarkError> {
        const URL: &str = "https://api-drive.mypikpak.com/drive/v1/files";
        let mut session = self.with_provider(CloudProvider::Pikpak, |store| {
            PikPakClient::new(http, store).session_snapshot()
        })?;
        let mut access_retried = false;
        let mut captcha_retried = false;
        loop {
            // Both upload-init HTTPS and the subsequent OSS transfers are outside the session lock.
            let result = PikPakClient::new(http, &self.store).send_snapshot(
                "POST",
                URL,
                payload.clone(),
                &session,
            );
            match result {
                Err(QuarkError::AuthorizationExpired) if !access_retried => {
                    session = self.with_provider(CloudProvider::Pikpak, |store| {
                        PikPakClient::new(http, store)
                            .recover_snapshot(&session, false, "POST", URL)
                    })?;
                    access_retried = true;
                }
                Err(QuarkError::CaptchaRequired) if !captcha_retried => {
                    session = self.with_provider(CloudProvider::Pikpak, |store| {
                        PikPakClient::new(http, store).recover_snapshot(&session, true, "POST", URL)
                    })?;
                    captcha_retried = true;
                }
                Err(QuarkError::CaptchaRequired) => {
                    return Err(QuarkError::WebAuthorizationRequired)
                }
                other => return other,
            }
        }
    }
}
