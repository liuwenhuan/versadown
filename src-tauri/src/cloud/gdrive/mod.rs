//! Google Drive.
//!
//! Two independent paths share this module:
//!
//! * Public links (no account, no API key): [`GdriveShareAdapter`] reads
//!   Google's own web endpoints, see [`public`].
//! * The signed-in drive (OAuth 2.0 desktop client with PKCE and a loopback
//!   redirect, Drive API v3 read-only): see [`oauth`] and [`api`].

mod adapter;
pub mod api;
mod failure;
mod html;
mod link;
pub mod oauth;
mod public;
pub mod session;
#[cfg(test)]
mod test_support;

pub use adapter::GdriveShareAdapter;
pub use failure::GdriveFailure;
pub use link::share_link_id;

/// Google OAuth client of this build (a desktop client: its secret is not
/// confidential, but the repository does not carry real values either).
pub(crate) const OAUTH_CLIENT_ID: Option<&str> = option_env!("VERSADOWN_GOOGLE_CLIENT_ID");
pub(crate) const OAUTH_CLIENT_SECRET: Option<&str> = option_env!("VERSADOWN_GOOGLE_CLIENT_SECRET");

/// Whether this build can sign in to Google at all.
pub fn oauth_client_configured() -> bool {
    OAUTH_CLIENT_ID.is_some_and(|id| !id.trim().is_empty())
}

/// Addresses that the public flow itself produces (the bytes endpoint and
/// native-document exports). They look like share links but are the file, so
/// the "a share page is never a download" check must let them through.
pub fn is_resolved_download_url(url: &url::Url) -> bool {
    if url.scheme() != "https" {
        return false;
    }
    let has_pair = |name: &str, value: &str| {
        url.query_pairs()
            .any(|(key, found)| key == name && found == value)
    };
    match (url.host_str(), url.path()) {
        (Some("drive.usercontent.google.com"), "/download") => has_pair("export", "download"),
        (Some("docs.google.com"), path) => {
            path.ends_with("/export") && url.query_pairs().any(|(key, _)| key == "format")
        }
        _ => false,
    }
}
