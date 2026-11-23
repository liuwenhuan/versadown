//! Share adapter for public personal-OneDrive links: inspect lists the files,
//! resolve returns a fresh pre-authorized URL (they expire after about an hour,
//! so it is requested again for every download, never stored).

use super::anonymous::{AnonymousClient, AnonymousEndpoints, SharedItem};
use super::failure::OnedriveFailure;
use super::link::{normalized_link, valid_resource_id};
use crate::cloud::share::{
    CloudShareEntry, CloudSharePlan, CloudShareResolvedEntry, CloudShareRootKind, ShareInspect,
    ShareInspection, ShareResolution, ShareResolve,
};
use crate::cloud::{CloudHttpTransport, CloudProvider, QuarkError, ResolvedCloudDownload};
use serde_json::json;

/// Folder trees deeper or wider than this are refused rather than truncated.
const MAX_DEPTH: usize = 32;
const MAX_FILES: usize = 20_000;

pub struct OnedriveShareAdapter<'a> {
    pub http: &'a dyn CloudHttpTransport,
    pub endpoints: AnonymousEndpoints,
}

impl<'a> OnedriveShareAdapter<'a> {
    pub fn new(http: &'a dyn CloudHttpTransport) -> Self {
        Self {
            http,
            endpoints: AnonymousEndpoints::default(),
        }
    }
}

/// Identifies the shared item without leaking more than the link itself.
fn canonical_share_id(link: &str) -> String {
    format!("onedrive/{link}")
}

/// Path segments may not climb out of the destination folder.
fn safe_segment(name: &str) -> String {
    match name {
        "." | ".." => "_".into(),
        other => other.to_owned(),
    }
}

struct Listing {
    files: Vec<(SharedItem, Vec<String>)>,
}

fn collect_files(
    client: &AnonymousClient<'_>,
    drive_id: &str,
    folder_id: &str,
    directory: &[String],
    listing: &mut Listing,
) -> Result<(), QuarkError> {
    if directory.len() >= MAX_DEPTH {
        return Err(QuarkError::Onedrive(OnedriveFailure::ShareTooLarge));
    }
    for item in client.children(drive_id, folder_id)? {
        if item.folder {
            let mut next = directory.to_vec();
            next.push(safe_segment(&item.name));
            collect_files(client, drive_id, &item.id, &next, listing)?;
        } else {
            if listing.files.len() >= MAX_FILES {
                return Err(QuarkError::Onedrive(OnedriveFailure::ShareTooLarge));
            }
            listing.files.push((item, directory.to_vec()));
        }
    }
    Ok(())
}

impl ShareInspect for OnedriveShareAdapter<'_> {
    fn inspect(
        &self,
        candidate: &crate::parser::CloudShareCandidate,
    ) -> Result<ShareInspection, QuarkError> {
        if candidate.provider != CloudProvider::Onedrive {
            return Err(QuarkError::InvalidResponse);
        }
        let link = normalized_link(&candidate.url)
            .map_err(|_| QuarkError::Onedrive(OnedriveFailure::LinkUnsupported))?;
        let client = AnonymousClient::new(self.http, &self.endpoints);
        let root = client.share_root(&link)?;
        let drive_id = root.drive_id.clone().ok_or(QuarkError::Onedrive(
            OnedriveFailure::AnonymousInterfaceChanged,
        ))?;
        let share_id = canonical_share_id(&link);
        let context = json!({"link": link, "drive": drive_id, "root": root.id});

        let (entries, root_kind) = if root.folder {
            let mut listing = Listing { files: Vec::new() };
            collect_files(&client, &drive_id, &root.id, &[], &mut listing)?;
            listing
                .files
                .sort_by(|(left, left_dir), (right, right_dir)| {
                    (left_dir, &left.name, &left.id).cmp(&(right_dir, &right.name, &right.id))
                });
            let entries = listing
                .files
                .into_iter()
                .map(|(item, directory)| {
                    CloudShareEntry::new(&share_id, item.id, item.name, directory, item.size)
                })
                .collect();
            (entries, CloudShareRootKind::Directory)
        } else {
            let entry = CloudShareEntry::new(
                &share_id,
                root.id.clone(),
                root.name.clone(),
                vec![],
                root.size,
            );
            (vec![entry], CloudShareRootKind::File { entry_id: root.id })
        };
        let mut plan =
            CloudSharePlan::new(CloudProvider::Onedrive, share_id, None, entries, context);
        plan.root_kind = root_kind;
        Ok(ShareInspection::Plan(plan))
    }
}

impl ShareResolve for OnedriveShareAdapter<'_> {
    fn resolve(&self, entry: &CloudShareResolvedEntry) -> Result<ShareResolution, QuarkError> {
        if entry.provider != CloudProvider::Onedrive
            || entry.plan.provider != CloudProvider::Onedrive
        {
            return Err(QuarkError::InvalidResponse);
        }
        let context = &entry.plan.provider_context;
        let link = context["link"]
            .as_str()
            .ok_or(QuarkError::InvalidResponse)?;
        let drive_id = context["drive"]
            .as_str()
            .ok_or(QuarkError::InvalidResponse)?;
        let item_id = entry.entry.entry_id.as_str();
        if !valid_resource_id(drive_id) || !valid_resource_id(item_id) {
            return Err(QuarkError::InvalidResponse);
        }
        let client = AnonymousClient::new(self.http, &self.endpoints);
        // A new token must redeem the share before it may read items below it.
        client.share_root(&normalized_link(link)?)?;
        let url = client.download_url(drive_id, item_id)?;
        Ok(ShareResolution::Direct(ResolvedCloudDownload {
            url,
            name: entry.entry.name.clone(),
            headers: Vec::new(),
            transform: None,
        }))
    }
}

#[cfg(test)]
mod tests;
