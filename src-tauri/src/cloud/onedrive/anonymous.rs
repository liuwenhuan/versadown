//! Anonymous reads of public personal-OneDrive shares.
//!
//! Since personal accounts moved to a SharePoint backend the classic
//! `api.onedrive.com/v1.0/shares` endpoint refuses signed-out requests. The
//! OneDrive web page instead asks `api-badgerp.svc.ms` for an anonymous
//! "badger" token and reads the share through `my.microsoftpersonalcontent.com`;
//! this client does exactly that. The `Prefer: autoredeem` header makes the
//! first share request bind the share to the token, after which the items
//! below it can be read by drive and item id.

use super::failure::OnedriveFailure;
use super::link::{share_id, valid_resource_id};
use crate::cloud::{CloudHttpRequest, CloudHttpTransport, QuarkError};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use zeroize::Zeroizing;

/// Fixed identifiers of the OneDrive web application's anonymous session.
const BADGER_APP_UUID: &str = "5cbed6ac-a083-4e14-b191-b4ba07653de2";
const BADGER_APP_ID: &str = "1141147648";

/// Upper bound on pages followed for one folder (200 items per page).
const MAX_PAGES: usize = 500;

#[derive(Clone, Debug)]
pub(crate) struct AnonymousEndpoints {
    pub token_url: String,
    pub api_base: String,
}

impl Default for AnonymousEndpoints {
    fn default() -> Self {
        Self {
            token_url: "https://api-badgerp.svc.ms/v1.0/token".into(),
            api_base: "https://my.microsoftpersonalcontent.com/_api/v2.0".into(),
        }
    }
}

/// One item of a share listing, reduced to what the adapter needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SharedItem {
    pub id: String,
    pub name: String,
    pub size: Option<u64>,
    pub folder: bool,
    pub drive_id: Option<String>,
}

pub(crate) struct AnonymousClient<'a> {
    http: &'a dyn CloudHttpTransport,
    endpoints: &'a AnonymousEndpoints,
    token: std::cell::RefCell<Option<Zeroizing<String>>>,
}

fn busy() -> QuarkError {
    QuarkError::Onedrive(OnedriveFailure::ServiceBusy)
}

/// The unofficial web interface answered unexpectedly (status or shape).
fn interface_changed() -> QuarkError {
    QuarkError::Onedrive(OnedriveFailure::AnonymousInterfaceChanged)
}

fn unavailable() -> QuarkError {
    QuarkError::Onedrive(OnedriveFailure::LinkUnavailable)
}

/// Map a failed share/item response. Provider text is never surfaced.
fn status_error(status: u16) -> QuarkError {
    match status {
        400 => QuarkError::Onedrive(OnedriveFailure::LinkUnsupported),
        401 | 403 | 404 | 410 => unavailable(),
        429 | 500..=599 => busy(),
        _ => interface_changed(),
    }
}

impl<'a> AnonymousClient<'a> {
    pub fn new(http: &'a dyn CloudHttpTransport, endpoints: &'a AnonymousEndpoints) -> Self {
        Self {
            http,
            endpoints,
            token: std::cell::RefCell::new(None),
        }
    }

    fn fetch_token(&self) -> Result<Zeroizing<String>, QuarkError> {
        let response = self.http.send(CloudHttpRequest {
            method: "POST",
            url: self.endpoints.token_url.clone(),
            headers: BTreeMap::from([
                ("Content-Type".into(), "application/json".into()),
                ("AppId".into(), BADGER_APP_ID.into()),
            ]),
            body: Some(json!({ "appId": BADGER_APP_UUID }).to_string()),
        })?;
        match response.status {
            200 => {}
            429 | 500..=599 => return Err(busy()),
            _ => return Err(interface_changed()),
        }
        let value: Value = serde_json::from_str(&response.body).map_err(|_| interface_changed())?;
        value["token"]
            .as_str()
            .filter(|token| !token.is_empty())
            .map(|token| Zeroizing::new(token.to_owned()))
            .ok_or_else(interface_changed)
    }

    fn token(&self) -> Result<Zeroizing<String>, QuarkError> {
        if let Some(token) = self.token.borrow().as_ref() {
            return Ok(token.clone());
        }
        let token = self.fetch_token()?;
        *self.token.borrow_mut() = Some(token.clone());
        Ok(token)
    }

    fn get(&self, url: &str) -> Result<Value, QuarkError> {
        // A token only lives for this client; one refetch covers a token that
        // the service has already dropped.
        for attempt in 0..2 {
            let token = self.token()?;
            let response = self.http.send(CloudHttpRequest {
                method: "GET",
                url: url.to_owned(),
                headers: BTreeMap::from([
                    ("Authorization".into(), format!("Badger {}", token.as_str())),
                    ("Prefer".into(), "autoredeem".into()),
                ]),
                body: None,
            })?;
            if response.status == 401 && attempt == 0 {
                *self.token.borrow_mut() = None;
                continue;
            }
            if response.status != 200 {
                return Err(status_error(response.status));
            }
            return serde_json::from_str(&response.body).map_err(|_| interface_changed());
        }
        Err(unavailable())
    }

    /// Resolve the share link itself (redeeming it for this token).
    pub fn share_root(&self, normalized_link: &str) -> Result<SharedItem, QuarkError> {
        let url = format!(
            "{}/shares/{}/driveitem",
            self.endpoints.api_base,
            share_id(normalized_link)
        );
        parse_item(&self.get(&url)?).ok_or_else(interface_changed)
    }

    /// Children of a folder inside an already redeemed share, following
    /// `@odata.nextLink` pages.
    pub fn children(&self, drive_id: &str, item_id: &str) -> Result<Vec<SharedItem>, QuarkError> {
        check_ids(drive_id, item_id)?;
        let mut next = Some(format!(
            "{}/drives/{drive_id}/items/{item_id}/children",
            self.endpoints.api_base
        ));
        let mut items = Vec::new();
        for _ in 0..MAX_PAGES {
            let Some(url) = next.take() else {
                return Ok(items);
            };
            let page = self.get(&url)?;
            for value in page["value"].as_array().ok_or_else(interface_changed)? {
                // Shortcuts to other drives and OneNote packages are neither
                // files nor folders of this share.
                if value.get("remoteItem").is_some() {
                    continue;
                }
                if let Some(item) = parse_item(value) {
                    items.push(item);
                }
            }
            next = match page["@odata.nextLink"].as_str() {
                Some(link) => Some(self.same_service(link)?),
                None => None,
            };
        }
        Err(QuarkError::Onedrive(OnedriveFailure::ShareTooLarge))
    }

    /// A paging link may only lead back to the API this client talks to: the
    /// badger token must not be sent anywhere else.
    fn same_service(&self, link: &str) -> Result<String, QuarkError> {
        link.starts_with(&format!("{}/", self.endpoints.api_base))
            .then(|| link.to_owned())
            .ok_or(QuarkError::InvalidResponse)
    }

    /// A fresh pre-authorized URL for one file of a share. The share must be
    /// redeemed first (`share_root`) on this client.
    pub fn download_url(&self, drive_id: &str, item_id: &str) -> Result<String, QuarkError> {
        check_ids(drive_id, item_id)?;
        let url = format!(
            "{}/drives/{drive_id}/items/{item_id}",
            self.endpoints.api_base
        );
        let item = self.get(&url)?;
        let download = item["@content.downloadUrl"]
            .as_str()
            .filter(|url| url.starts_with("https://"))
            .ok_or_else(interface_changed)?;
        Ok(download.to_owned())
    }
}

fn check_ids(drive_id: &str, item_id: &str) -> Result<(), QuarkError> {
    if valid_resource_id(drive_id) && valid_resource_id(item_id) {
        Ok(())
    } else {
        Err(QuarkError::InvalidResponse)
    }
}

fn parse_item(value: &Value) -> Option<SharedItem> {
    let id = value["id"].as_str().filter(|id| valid_resource_id(id))?;
    let folder = value.get("folder").is_some();
    if !folder && value.get("file").is_none() {
        return None;
    }
    let name = value["name"].as_str().filter(|name| !name.is_empty())?;
    Some(SharedItem {
        id: id.to_owned(),
        name: name.to_owned(),
        size: value["size"].as_u64(),
        folder,
        drive_id: value["parentReference"]["driveId"]
            .as_str()
            .filter(|id| valid_resource_id(id))
            .map(str::to_owned),
    })
}
