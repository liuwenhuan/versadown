//! Microsoft OneDrive (personal accounts only).
//!
//! Two independent paths share this module:
//!
//! * Public share links (`1drv.ms`, `onedrive.live.com`) are read anonymously
//!   with the same short-lived "badger" token the OneDrive web page requests
//!   for signed-out visitors. No account is involved (`anonymous`, `adapter`).
//! * The user's own drive uses OAuth2 authorization code + PKCE against the
//!   `consumers` tenant with a loopback redirect, then Microsoft Graph
//!   (`oauth`, `loopback`, `client`, `session`).
//!
//! Tokens and pre-authorized download URLs are secrets: they never reach logs,
//! errors or the UI.

mod adapter;
mod anonymous;
mod client;
mod failure;
mod link;
mod loopback;
mod oauth;
mod session;
#[cfg(test)]
mod test_support;

pub(crate) use adapter::OnedriveShareAdapter;
pub(crate) use client::OnedriveClient;
pub use failure::OnedriveFailure;
pub(crate) use link::is_share_link;
pub(crate) use loopback::{begin_authorization, complete_authorization};
pub(crate) use oauth::oauth_configured;
