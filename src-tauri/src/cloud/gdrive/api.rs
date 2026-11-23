//! Google Drive API v3 (read-only scope) for the signed-in drive.
//!
//! Every request carries the user's access token. A rejected token (401) is
//! reported as `AuthorizationExpired`, which makes the authorization
//! supervisor renew it through [`GdriveClient::refresh_session`]; only a
//! client that owns its store (no supervisor snapshot) renews on its own.

use super::failure::GdriveFailure;
use super::link::valid_id;
use super::oauth::{refresh_tokens, OAuthClient};
use super::session::{now, GdriveSession};
use crate::cloud::{
    CloudAccount, CloudAuthMethod, CloudDirectory, CloudFile, CloudFileKind, CloudHttpRequest,
    CloudHttpTransport, CloudProvider, CloudStorageCapacity, CredentialStore, QuarkError,
    ResolvedCloudDownload,
};
use serde_json::Value;
use std::collections::BTreeMap;

const API: &str = "https://www.googleapis.com/drive/v3";
const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
const SHORTCUT_MIME: &str = "application/vnd.google-apps.shortcut";
const NATIVE_MIME_PREFIX: &str = "application/vnd.google-apps.";

/// Virtual folders of the root listing; the UI names them.
pub const SHARED_WITH_ME_ID: &str = "__versa_down_gdrive_shared_with_me__";
pub const SHARED_DRIVES_ID: &str = "__versa_down_gdrive_shared_drives__";

const PAGE_SIZE: &str = "1000";
const MAX_PAGES: usize = 100;

pub struct GdriveClient<'a> {
    http: &'a dyn CloudHttpTransport,
    store: &'a dyn CredentialStore,
    /// Resolved lazily: only a renewal needs the build's OAuth client.
    oauth: Option<OAuthClient>,
}

fn map_error(status: u16, body: &str) -> QuarkError {
    let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let reason = value["error"]["errors"][0]["reason"]
        .as_str()
        .or_else(|| value["error"]["status"].as_str())
        .unwrap_or_default();
    match (status, reason) {
        (401, _) => QuarkError::AuthorizationExpired,
        (_, "downloadQuotaExceeded") => QuarkError::Gdrive(GdriveFailure::QuotaExceeded),
        (_, "userRateLimitExceeded" | "rateLimitExceeded" | "sharingRateLimitExceeded")
        | (429, _) => QuarkError::Gdrive(GdriveFailure::RateLimited),
        (403, "insufficientFilePermissions" | "forbidden" | "cannotDownloadFile"
            | "appNotAuthorizedToFile" | "PERMISSION_DENIED") => {
            QuarkError::Gdrive(GdriveFailure::AccessDenied)
        }
        (404, _) => QuarkError::SourceFileMissing,
        _ => QuarkError::Provider(format!("Google Drive 请求失败（HTTP {status}）")),
    }
}

fn parent_query(parent: &str) -> Result<String, QuarkError> {
    if parent == "root" || valid_id(parent) {
        Ok(format!("'{parent}' in parents and trashed = false"))
    } else {
        Err(QuarkError::InvalidInput("无效的 Google Drive 目录".into()))
    }
}

fn file_from(item: &Value, parent: &str) -> Option<CloudFile> {
    let mut id = item["id"].as_str()?;
    let mut mime = item["mimeType"].as_str().unwrap_or_default();
    // A shortcut stands for its target: browse into it or download it.
    if mime == SHORTCUT_MIME {
        id = item["shortcutDetails"]["targetId"].as_str()?;
        mime = item["shortcutDetails"]["targetMimeType"]
            .as_str()
            .unwrap_or_default();
    }
    Some(CloudFile {
        id: id.to_owned(),
        parent_id: parent.to_owned(),
        name: item["name"].as_str()?.to_owned(),
        kind: if mime == FOLDER_MIME {
            CloudFileKind::Folder
        } else {
            CloudFileKind::File
        },
        size: item["size"].as_str().and_then(|size| size.parse().ok()),
        modified_at: item["modifiedTime"].as_str().map(str::to_owned),
    })
}

impl<'a> GdriveClient<'a> {
    pub fn new(http: &'a dyn CloudHttpTransport, store: &'a dyn CredentialStore) -> Self {
        Self {
            http,
            store,
            oauth: None,
        }
    }

    pub fn with_oauth_client(mut self, client: OAuthClient) -> Self {
        self.oauth = Some(client);
        self
    }

    fn oauth_client(&self) -> Result<OAuthClient, QuarkError> {
        match &self.oauth {
            Some(client) => Ok(client.clone()),
            None => OAuthClient::from_build(),
        }
    }

    /// Exchange the stored refresh token for a new access token.
    pub fn refresh_session(&self) -> Result<(), QuarkError> {
        self.refresh(GdriveSession::load(self.store)?).map(|_| ())
    }

    fn refresh(&self, mut session: GdriveSession) -> Result<GdriveSession, QuarkError> {
        let tokens = refresh_tokens(self.http, &self.oauth_client()?, &session.refresh_token)?;
        session.access_token = tokens.access_token;
        if let Some(rotated) = tokens.refresh_token {
            session.refresh_token = rotated;
        }
        session.expires_at = now() + tokens.expires_in;
        session.save(self.store)?;
        Ok(session)
    }

    /// GET a JSON document. A client that owns its store renews an expiring
    /// token itself and retries a rejected one once; inside an authorization
    /// supervisor snapshot a rejection is handed upward untouched.
    fn get(&self, url: &str) -> Result<Value, QuarkError> {
        let own_renewal = self.store.automatic_session_recovery();
        let mut session = GdriveSession::load(self.store)?;
        let mut renewed = false;
        if own_renewal && session.needs_refresh() {
            session = self.refresh(session)?;
            renewed = true;
        }
        loop {
            let response = self.http.send(CloudHttpRequest {
                method: "GET",
                url: url.to_owned(),
                headers: BTreeMap::from([(
                    "Authorization".to_owned(),
                    format!("Bearer {}", session.access_token),
                )]),
                body: None,
            })?;
            if response.status == 401 && own_renewal && !renewed {
                session = self.refresh(session)?;
                renewed = true;
                continue;
            }
            if !(200..300).contains(&response.status) {
                return Err(map_error(response.status, &response.body));
            }
            return serde_json::from_str(&response.body).map_err(|_| QuarkError::InvalidResponse);
        }
    }

    fn url(path: &str, params: &[(&str, &str)]) -> String {
        url::Url::parse_with_params(&format!("{API}{path}"), params)
            .expect("static endpoint")
            .into()
    }

    pub fn account(&self) -> Result<CloudAccount, QuarkError> {
        let about = self.get(&Self::url(
            "/about",
            &[("fields", "user(permissionId,displayName,emailAddress)")],
        ))?;
        let user = &about["user"];
        let id = user["permissionId"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or(QuarkError::InvalidResponse)?;
        let display_name = user["emailAddress"]
            .as_str()
            .or_else(|| user["displayName"].as_str())
            .unwrap_or(id);
        Ok(CloudAccount {
            provider: CloudProvider::Gdrive,
            id: id.to_owned(),
            display_name: display_name.to_owned(),
            auth_method: CloudAuthMethod::Web,
        })
    }

    pub fn storage_capacity(&self) -> Result<CloudStorageCapacity, QuarkError> {
        let about = self.get(&Self::url("/about", &[("fields", "storageQuota")]))?;
        let quota = &about["storageQuota"];
        let number = |name: &str| quota[name].as_str().and_then(|value| value.parse::<u64>().ok());
        let used = number("usage").ok_or(QuarkError::InvalidResponse)?;
        // The limit is absent for unlimited (Workspace) storage.
        Ok(match number("limit") {
            Some(total) => CloudStorageCapacity::Limited {
                total_bytes: total,
                used_bytes: used,
            },
            None => CloudStorageCapacity::Unlimited { used_bytes: used },
        })
    }

    /// One page after another until Google reports no further page.
    fn list_pages(
        &self,
        path: &str,
        params: &[(&str, &str)],
        mut accept: impl FnMut(&Value),
    ) -> Result<(), QuarkError> {
        let mut token: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut query: Vec<(&str, &str)> = params.to_vec();
            if let Some(token) = &token {
                query.push(("pageToken", token));
            }
            let page = self.get(&Self::url(path, &query))?;
            accept(&page);
            match page["nextPageToken"].as_str().filter(|next| !next.is_empty()) {
                Some(next) => token = Some(next.to_owned()),
                None => return Ok(()),
            }
        }
        // Absurdly long listings are cut rather than looping on a bad cursor.
        Ok(())
    }

    pub fn list(&self, parent: &str) -> Result<CloudDirectory, QuarkError> {
        let mut items = Vec::new();
        if parent == SHARED_DRIVES_ID {
            self.list_pages(
                "/drives",
                &[
                    ("pageSize", "100"),
                    ("fields", "nextPageToken,drives(id,name)"),
                ],
                |page| {
                    items.extend(page["drives"].as_array().into_iter().flatten().filter_map(
                        |drive| {
                            Some(CloudFile {
                                id: drive["id"].as_str()?.to_owned(),
                                parent_id: SHARED_DRIVES_ID.to_owned(),
                                name: drive["name"].as_str()?.to_owned(),
                                kind: CloudFileKind::Folder,
                                size: None,
                                modified_at: None,
                            })
                        },
                    ));
                },
            )?;
            return Ok(CloudDirectory {
                id: parent.to_owned(),
                items,
            });
        }
        let query = if parent == SHARED_WITH_ME_ID {
            "sharedWithMe = true and trashed = false".to_owned()
        } else {
            parent_query(parent)?
        };
        let fields = "nextPageToken,files(id,name,mimeType,size,modifiedTime,shortcutDetails(targetId,targetMimeType))";
        let mut params = vec![
            ("q", query.as_str()),
            ("pageSize", PAGE_SIZE),
            ("orderBy", "folder,name"),
            ("fields", fields),
            ("supportsAllDrives", "true"),
            ("includeItemsFromAllDrives", "true"),
        ];
        // Children of a shared drive (or of a folder inside one) are only
        // returned when every corpus is searched.
        if parent != "root" && parent != SHARED_WITH_ME_ID {
            params.push(("corpora", "allDrives"));
        }
        self.list_pages("/files", &params, |page| {
            items.extend(
                page["files"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|item| file_from(item, parent)),
            );
        })?;
        Ok(CloudDirectory {
            id: parent.to_owned(),
            items,
        })
    }

    /// The media endpoint with the access token as a header. Only the local
    /// range gateway ever receives that header; aria2 never does.
    pub fn download(&self, file_id: &str) -> Result<ResolvedCloudDownload, QuarkError> {
        if !valid_id(file_id) {
            return Err(QuarkError::InvalidInput("无效的 Google Drive 文件".into()));
        }
        let meta = self.get(&Self::url(
            &format!("/files/{file_id}"),
            &[
                ("fields", "id,name,mimeType"),
                ("supportsAllDrives", "true"),
            ],
        ))?;
        let mime = meta["mimeType"].as_str().unwrap_or_default();
        if mime == FOLDER_MIME {
            return Err(QuarkError::InvalidInput("无法下载文件夹".into()));
        }
        // Native documents have no bytes; the API's export is capped and
        // cannot be resumed. Their public share link exports them instead.
        if mime.starts_with(NATIVE_MIME_PREFIX) {
            return Err(QuarkError::Gdrive(GdriveFailure::NativeNotDownloadable));
        }
        let name = meta["name"]
            .as_str()
            .filter(|name| !name.is_empty())
            .ok_or(QuarkError::InvalidResponse)?
            .to_owned();
        let session = GdriveSession::load(self.store)?;
        Ok(ResolvedCloudDownload {
            url: Self::url(
                &format!("/files/{file_id}"),
                &[("alt", "media"), ("supportsAllDrives", "true")],
            ),
            name,
            headers: vec![(
                "Authorization".into(),
                format!("Bearer {}", session.access_token),
            )],
            transform: None,
        })
    }
}

#[cfg(test)]
mod tests;
