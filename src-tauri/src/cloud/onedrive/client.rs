//! Microsoft Graph client for the signed-in user's own OneDrive (read only).

use super::failure::OnedriveFailure;
use super::link::valid_resource_id;
use super::oauth::{self, OAuthConfig};
use super::session::OnedriveSession;
use crate::cloud::{
    CloudAccount, CloudAuthMethod, CloudDirectory, CloudFile, CloudFileKind, CloudHttpRequest,
    CloudHttpTransport, CloudProvider, CloudStorageCapacity, CredentialStore, QuarkError,
};
use serde_json::Value;
use std::collections::BTreeMap;

/// Ids of the personal drive's top folder: the `root` alias or an item id.
pub(crate) const ROOT_ID: &str = "root";
/// Upper bound on pages followed for one folder (200 items per page).
const MAX_PAGES: usize = 500;
/// Refresh a little before the access token actually lapses.
const EXPIRY_MARGIN_SECS: i64 = 60;

pub(crate) struct OnedriveClient<'a> {
    http: &'a dyn CloudHttpTransport,
    store: &'a dyn CredentialStore,
    config: OAuthConfig,
}

fn busy() -> QuarkError {
    QuarkError::Onedrive(OnedriveFailure::ServiceBusy)
}

impl<'a> OnedriveClient<'a> {
    /// Reading needs no application id; only renewing does, and it reports
    /// that itself.
    pub fn new(http: &'a dyn CloudHttpTransport, store: &'a dyn CredentialStore) -> Self {
        Self::with_config(http, store, OAuthConfig::for_build())
    }

    pub fn with_config(
        http: &'a dyn CloudHttpTransport,
        store: &'a dyn CredentialStore,
        config: OAuthConfig,
    ) -> Self {
        Self {
            http,
            store,
            config,
        }
    }

    fn session(&self) -> Result<OnedriveSession, QuarkError> {
        OnedriveSession::load(self.store)?.ok_or(QuarkError::NotAuthorized)
    }

    /// Exchange the refresh token and persist the rotated pair. Returns false
    /// when there is nothing to refresh with.
    pub fn refresh_session(&self) -> Result<bool, QuarkError> {
        let Some(session) = OnedriveSession::load(self.store)? else {
            return Ok(false);
        };
        self.renew(&session)?;
        Ok(true)
    }

    fn renew(&self, session: &OnedriveSession) -> Result<OnedriveSession, QuarkError> {
        let renewed = oauth::refresh(self.http, &self.config, &session.refresh_token)?;
        renewed.save(self.store)?;
        Ok(renewed)
    }

    /// GET with the bearer token. Snapshot stores delegate renewal to the
    /// authorization supervisor, so there a 401 is reported instead of retried.
    fn get(&self, url: &str) -> Result<Value, QuarkError> {
        let own_renewal = self.store.automatic_session_recovery();
        let mut session = self.session()?;
        if own_renewal && session.expires_within(EXPIRY_MARGIN_SECS) {
            session = self.renew(&session)?;
        }
        let mut renewed = false;
        loop {
            let response = self.http.send(CloudHttpRequest {
                method: "GET",
                url: url.to_owned(),
                headers: BTreeMap::from([(
                    "Authorization".into(),
                    format!("Bearer {}", session.access_token),
                )]),
                body: None,
            })?;
            match response.status {
                200 => {
                    return serde_json::from_str(&response.body)
                        .map_err(|_| QuarkError::InvalidResponse)
                }
                401 if own_renewal && !renewed => {
                    session = self.renew(&session)?;
                    renewed = true;
                }
                401 => return Err(QuarkError::AuthorizationExpired),
                403 => return Err(QuarkError::NotAuthorized),
                404 => return Err(QuarkError::SourceFileMissing),
                429 | 500..=599 => return Err(busy()),
                _ => return Err(QuarkError::InvalidResponse),
            }
        }
    }

    pub fn account(&self) -> Result<CloudAccount, QuarkError> {
        let drive = self.get(&format!("{}/me/drive", self.config.graph_base))?;
        let id = drive["owner"]["user"]["id"]
            .as_str()
            .or_else(|| drive["id"].as_str())
            .filter(|id| !id.is_empty())
            .ok_or(QuarkError::InvalidResponse)?;
        let display_name = drive["owner"]["user"]["displayName"]
            .as_str()
            .filter(|name| !name.is_empty())
            .unwrap_or("OneDrive");
        Ok(CloudAccount {
            provider: CloudProvider::Onedrive,
            id: id.to_owned(),
            display_name: display_name.to_owned(),
            auth_method: CloudAuthMethod::Web,
        })
    }

    pub fn storage_capacity(&self) -> Result<CloudStorageCapacity, QuarkError> {
        let drive = self.get(&format!("{}/me/drive", self.config.graph_base))?;
        CloudStorageCapacity::from_json(&drive["quota"]["total"], &drive["quota"]["used"], false)
    }

    fn children_url(&self, parent: &str) -> Result<String, QuarkError> {
        let base = &self.config.graph_base;
        let select = "$select=id,name,size,file,folder,lastModifiedDateTime&$top=200";
        if parent == ROOT_ID {
            return Ok(format!("{base}/me/drive/root/children?{select}"));
        }
        if !valid_resource_id(parent) {
            return Err(QuarkError::InvalidInput("文件夹标识无效".into()));
        }
        Ok(format!("{base}/me/drive/items/{parent}/children?{select}"))
    }

    pub fn list(&self, parent: &str) -> Result<CloudDirectory, QuarkError> {
        let mut next = Some(self.children_url(parent)?);
        let mut items = Vec::new();
        for _ in 0..MAX_PAGES {
            let Some(url) = next.take() else {
                return Ok(CloudDirectory {
                    id: parent.to_owned(),
                    items,
                });
            };
            let page = self.get(&url)?;
            for value in page["value"]
                .as_array()
                .ok_or(QuarkError::InvalidResponse)?
            {
                if let Some(file) = parse_file(value, parent) {
                    items.push(file);
                }
            }
            next = match page["@odata.nextLink"].as_str() {
                Some(link) => Some(self.graph_link(link)?),
                None => None,
            };
        }
        Err(QuarkError::InvalidResponse)
    }

    /// The bearer token must never be sent to a host other than Graph.
    fn graph_link(&self, link: &str) -> Result<String, QuarkError> {
        link.starts_with(&format!("{}/", self.config.graph_base))
            .then(|| link.to_owned())
            .ok_or(QuarkError::InvalidResponse)
    }

    /// A fresh pre-authorized download URL (valid for about an hour, so it is
    /// requested again whenever a transfer needs one) and the file name.
    pub fn download(&self, file_id: &str) -> Result<(String, String), QuarkError> {
        if !valid_resource_id(file_id) {
            return Err(QuarkError::InvalidInput("文件标识无效".into()));
        }
        let item = self.get(&format!(
            "{}/me/drive/items/{file_id}",
            self.config.graph_base
        ))?;
        if item.get("folder").is_some() {
            return Err(QuarkError::InvalidInput("无法下载文件夹本身".into()));
        }
        let url = item["@microsoft.graph.downloadUrl"]
            .as_str()
            .filter(|url| url.starts_with("https://"))
            .ok_or(QuarkError::InvalidResponse)?;
        let name = item["name"]
            .as_str()
            .filter(|name| !name.is_empty())
            .ok_or(QuarkError::InvalidResponse)?;
        Ok((url.to_owned(), name.to_owned()))
    }
}

fn parse_file(value: &Value, parent: &str) -> Option<CloudFile> {
    let id = value["id"].as_str().filter(|id| valid_resource_id(id))?;
    let name = value["name"].as_str().filter(|name| !name.is_empty())?;
    let kind = if value.get("folder").is_some() {
        CloudFileKind::Folder
    } else if value.get("file").is_some() {
        CloudFileKind::File
    } else {
        return None;
    };
    Some(CloudFile {
        id: id.to_owned(),
        parent_id: parent.to_owned(),
        name: name.to_owned(),
        size: (kind == CloudFileKind::File)
            .then(|| value["size"].as_u64())
            .flatten(),
        kind,
        modified_at: value["lastModifiedDateTime"].as_str().map(str::to_owned),
    })
}

#[cfg(test)]
mod tests;
