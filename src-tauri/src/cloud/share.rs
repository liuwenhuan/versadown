//! Share capability, service and encrypted row repository compatibility exports.
//! Do not serialize these internal models into IPC, task records, or logs.
use super::{CloudProvider, CredentialStore, DpapiCredentialStore, QuarkError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroizing;

pub mod capabilities;
mod models;
mod preflight;
pub mod repository;
pub mod service;
pub use capabilities::*;
pub use models::*;
#[allow(unused_imports)]
pub use preflight::*;
pub use repository::*;
pub use service::*;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
