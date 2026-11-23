//! Account-less access to publicly shared Drive items through Google's own
//! web endpoints (no API key): `drive.usercontent.google.com/download` for
//! files, `docs.google.com/<product>/d/<id>/export` for native documents and
//! `drive.google.com/embeddedfolderview` for folder listings.
//!
//! Every request goes through the application's transport (proxy settings
//! apply) and asks for English pages so that error text is matchable.

use super::failure::GdriveFailure;
use super::html::{
    classify_error_page, content_disposition_name, content_range_total, is_sign_in_page,
    parse_confirm_request, parse_folder_entries, FolderEntry,
};
use super::link::NativeKind;
use crate::cloud::{CloudHttpDetail, CloudHttpRequest, CloudHttpTransport, QuarkError};
use std::collections::BTreeMap;

const DOWNLOAD_ENDPOINT: &str = "https://drive.usercontent.google.com/download";
const FOLDER_ENDPOINT: &str = "https://drive.google.com/embeddedfolderview";

/// A resolved public item: the final address aria2 can fetch without cookies.
#[derive(Debug, PartialEq, Eq)]
pub struct PublicFile {
    pub url: String,
    pub name: Option<String>,
    /// Absent for exports, whose length is unknown until streamed.
    pub size: Option<u64>,
}

pub struct PublicDrive<'a> {
    pub http: &'a dyn CloudHttpTransport,
}

fn probe_request(url: String) -> CloudHttpRequest {
    CloudHttpRequest {
        method: "GET",
        url,
        headers: BTreeMap::from([
            // One byte is enough to read the headers; a server that ignores
            // the range is not buffered either (see `send_detailed`).
            ("Range".to_owned(), "bytes=0-0".to_owned()),
            ("Accept-Language".to_owned(), "en-US,en;q=0.8".to_owned()),
        ]),
        body: None,
    }
}

fn page_request(url: String) -> CloudHttpRequest {
    CloudHttpRequest {
        method: "GET",
        url,
        headers: BTreeMap::from([("Accept-Language".to_owned(), "en-US,en;q=0.8".to_owned())]),
        body: None,
    }
}

fn is_html(detail: &CloudHttpDetail) -> bool {
    detail
        .header("content-type")
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("text/html"))
}

/// What a probe of a download endpoint found.
enum Probe {
    File(PublicFile),
    Confirm(String),
}

impl PublicDrive<'_> {
    fn download_url(id: &str, resource_key: Option<&str>) -> String {
        let mut url = format!("{DOWNLOAD_ENDPOINT}?id={id}&export=download");
        if let Some(key) = resource_key {
            url.push_str("&resourcekey=");
            url.push_str(key);
        }
        url
    }

    fn interpret(&self, url: String, detail: CloudHttpDetail) -> Result<Probe, QuarkError> {
        let status = detail.response.status;
        if is_sign_in_page(&detail.final_url) {
            return Err(QuarkError::Gdrive(GdriveFailure::AccessDenied));
        }
        if is_html(&detail) {
            let body = &detail.response.body;
            // A confirmation form wins: it is an offer to download, never a refusal.
            if let Some(confirm) = parse_confirm_request(body, &url) {
                return Ok(Probe::Confirm(confirm.url));
            }
            if let Some(failure) = classify_error_page(body) {
                return Err(QuarkError::Gdrive(failure));
            }
            return Err(match status {
                404 => QuarkError::Gdrive(GdriveFailure::NotFound),
                401 | 403 => QuarkError::Gdrive(GdriveFailure::AccessDenied),
                429 => QuarkError::Gdrive(GdriveFailure::QuotaExceeded),
                _ => QuarkError::InvalidResponse,
            });
        }
        match status {
            200 | 206 => {}
            401 | 403 => return Err(QuarkError::Gdrive(GdriveFailure::AccessDenied)),
            404 => return Err(QuarkError::Gdrive(GdriveFailure::NotFound)),
            429 => return Err(QuarkError::Gdrive(GdriveFailure::QuotaExceeded)),
            _ => return Err(QuarkError::InvalidResponse),
        }
        let name = detail.header("content-disposition").and_then(content_disposition_name);
        // A range reply states the total; a plain 200 states the length.
        let size = detail
            .header("content-range")
            .and_then(content_range_total)
            .or_else(|| {
                (status == 200)
                    .then(|| detail.header("content-length")?.trim().parse().ok())
                    .flatten()
            });
        Ok(Probe::File(PublicFile { url, name, size }))
    }

    fn probe(&self, url: String) -> Result<Probe, QuarkError> {
        let mut detail = self.http.send_detailed(probe_request(url.clone()))?;
        // An empty file has no byte 0: Google answers the one-byte range with
        // a 416 error page instead of the file's headers. Ask without a range
        // (the body is empty, so nothing large is transferred).
        if detail.response.status == 416 {
            detail = self.http.send_detailed(page_request(url.clone()))?;
        }
        self.interpret(url, detail)
    }

    /// Resolve a file id to an address aria2 can download, following the
    /// virus-scan confirmation page of large files.
    pub fn resolve_file(
        &self,
        id: &str,
        resource_key: Option<&str>,
    ) -> Result<PublicFile, QuarkError> {
        let url = Self::download_url(id, resource_key);
        match self.probe(url)? {
            Probe::File(file) => Ok(file),
            Probe::Confirm(confirmed) => match self.probe(confirmed)? {
                Probe::File(file) => Ok(file),
                // A second confirmation page means the form did not take.
                Probe::Confirm(_) => Err(QuarkError::InvalidResponse),
            },
        }
    }

    /// Native Google documents are exported to an Office format.
    pub fn resolve_native(&self, kind: NativeKind, id: &str) -> Result<PublicFile, QuarkError> {
        let url = kind.export_url(id);
        let detail = self.http.send_detailed(probe_request(url.clone()))?;
        match self.interpret(url, detail)? {
            Probe::File(mut file) => {
                // Exports are generated on the fly: a range reply says nothing
                // about the finished document's size.
                file.size = None;
                Ok(file)
            }
            Probe::Confirm(_) => Err(QuarkError::InvalidResponse),
        }
    }

    /// The entries of one public folder. A missing or private folder (Google
    /// answers 404 for both) is `NotFound`.
    pub fn list_folder(
        &self,
        id: &str,
        resource_key: Option<&str>,
    ) -> Result<Vec<FolderEntry>, QuarkError> {
        let mut url = format!("{FOLDER_ENDPOINT}?id={id}");
        if let Some(key) = resource_key {
            url.push_str("&resourcekey=");
            url.push_str(key);
        }
        let detail = self.http.send_detailed(page_request(url))?;
        if is_sign_in_page(&detail.final_url) {
            return Err(QuarkError::Gdrive(GdriveFailure::AccessDenied));
        }
        match detail.response.status {
            200 => {}
            401 | 403 => return Err(QuarkError::Gdrive(GdriveFailure::AccessDenied)),
            404 => return Err(QuarkError::Gdrive(GdriveFailure::NotFound)),
            429 => return Err(QuarkError::Gdrive(GdriveFailure::QuotaExceeded)),
            _ => return Err(QuarkError::Gdrive(GdriveFailure::FolderPageChanged)),
        }
        if !detail.response.body.contains("flip-entries")
            && !detail.response.body.contains("flip-contents")
        {
            return Err(QuarkError::Gdrive(GdriveFailure::FolderPageChanged));
        }
        Ok(parse_folder_entries(&detail.response.body))
    }
}
