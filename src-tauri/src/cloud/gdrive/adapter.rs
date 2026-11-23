//! Share adapter for public Google Drive links: inspect lists the shared item
//! (a file, an exported native document or a whole folder tree) and resolve
//! returns an address aria2 can fetch without cookies or tokens.

use super::failure::GdriveFailure;
use super::html::{FolderEntry, FolderEntryKind};
use super::link::{parse_google_link, GoogleLink, GoogleLinkKind, NativeKind};
use super::public::{PublicDrive, PublicFile};
use crate::cloud::share::{
    CloudShareEntry, CloudSharePlan, CloudShareResolvedEntry, CloudShareRootKind, ShareInspect,
    ShareInspection, ShareResolution, ShareResolve,
};
use crate::cloud::{CloudHttpTransport, CloudProvider, QuarkError, ResolvedCloudDownload};
use serde_json::{json, Map, Value};
use std::collections::HashSet;

/// Defensive bounds for a hostile or enormous public folder.
const MAX_FOLDERS: usize = 400;
const MAX_FILES: usize = 5_000;
const MAX_DEPTH: usize = 16;

pub struct GdriveShareAdapter<'a> {
    pub http: &'a dyn CloudHttpTransport,
}

/// Exports get the extension of their format unless the name already has it.
fn export_name(name: &str, kind: NativeKind) -> String {
    let extension = kind.export_format();
    let suffix = format!(".{extension}");
    if name.to_ascii_lowercase().ends_with(&suffix) {
        name.to_owned()
    } else {
        format!("{name}{suffix}")
    }
}

fn canonical_share_id(link: &GoogleLink, folder: bool) -> String {
    format!("{}/{}", if folder { "folder" } else { "file" }, link.id)
}

struct FolderWalk {
    entries: Vec<CloudShareEntry>,
    native: Map<String, Value>,
    seen: HashSet<String>,
    folders: usize,
}

impl GdriveShareAdapter<'_> {
    fn public(&self) -> PublicDrive<'_> {
        PublicDrive { http: self.http }
    }

    fn file_plan(
        &self,
        link: &GoogleLink,
        native: Option<NativeKind>,
    ) -> Result<CloudSharePlan, QuarkError> {
        let drive = self.public();
        let (name, size) = match native {
            Some(kind) => {
                let PublicFile { name, .. } = drive.resolve_native(kind, &link.id)?;
                (export_name(&name.unwrap_or_else(|| link.id.clone()), kind), None)
            }
            None => {
                let file = drive.resolve_file(&link.id, link.resource_key.as_deref())?;
                (file.name.unwrap_or_else(|| link.id.clone()), file.size)
            }
        };
        let share_id = canonical_share_id(link, false);
        let entry = CloudShareEntry::new(&share_id, link.id.clone(), name, vec![], size);
        let mut context = json!({
            "kind": "file",
            "id": link.id,
            "native": native.map(NativeKind::key),
        });
        if let Some(key) = &link.resource_key {
            context["resourceKey"] = json!(key);
        }
        let mut plan = CloudSharePlan::new(
            CloudProvider::Gdrive,
            share_id,
            None,
            vec![entry],
            context,
        );
        plan.root_kind = CloudShareRootKind::File {
            entry_id: link.id.clone(),
        };
        Ok(plan)
    }

    fn walk_folder(
        &self,
        walk: &mut FolderWalk,
        share_id: &str,
        folder: &str,
        resource_key: Option<&str>,
        directory: &[String],
    ) -> Result<(), QuarkError> {
        if directory.len() > MAX_DEPTH || walk.folders >= MAX_FOLDERS {
            return Err(QuarkError::Gdrive(GdriveFailure::FolderTooLarge));
        }
        walk.folders += 1;
        let listing: Vec<FolderEntry> = self.public().list_folder(folder, resource_key)?;
        for entry in listing {
            match entry.kind {
                FolderEntryKind::Folder => {
                    if !walk.seen.insert(entry.id.clone()) {
                        continue;
                    }
                    let mut nested = directory.to_vec();
                    nested.push(entry.name.clone());
                    self.walk_folder(walk, share_id, &entry.id, None, &nested)?;
                }
                FolderEntryKind::File | FolderEntryKind::Native(_) => {
                    if !walk.seen.insert(entry.id.clone()) {
                        continue;
                    }
                    if walk.entries.len() >= MAX_FILES {
                        return Err(QuarkError::Gdrive(GdriveFailure::FolderTooLarge));
                    }
                    let name = match entry.kind {
                        FolderEntryKind::Native(kind) => {
                            walk.native.insert(entry.id.clone(), json!(kind.key()));
                            export_name(&entry.name, kind)
                        }
                        _ => entry.name.clone(),
                    };
                    walk.entries.push(CloudShareEntry::new(
                        share_id,
                        entry.id,
                        name,
                        directory.to_vec(),
                        None,
                    ));
                }
            }
        }
        Ok(())
    }

    fn folder_plan(&self, link: &GoogleLink) -> Result<CloudSharePlan, QuarkError> {
        let share_id = canonical_share_id(link, true);
        let mut walk = FolderWalk {
            entries: Vec::new(),
            native: Map::new(),
            seen: HashSet::from([link.id.clone()]),
            folders: 0,
        };
        self.walk_folder(&mut walk, &share_id, &link.id, link.resource_key.as_deref(), &[])?;
        let mut context = json!({"kind": "folder", "id": link.id, "natives": walk.native});
        if let Some(key) = &link.resource_key {
            context["resourceKey"] = json!(key);
        }
        let mut plan = CloudSharePlan::new(
            CloudProvider::Gdrive,
            share_id,
            None,
            walk.entries,
            context,
        );
        plan.root_kind = CloudShareRootKind::Directory;
        Ok(plan)
    }

    /// `open?id=` / `uc?id=`: a file unless Google only knows it as a folder.
    fn ambiguous_plan(&self, link: &GoogleLink) -> Result<CloudSharePlan, QuarkError> {
        match self.file_plan(link, None) {
            Err(first @ QuarkError::Gdrive(GdriveFailure::NotFound | GdriveFailure::AccessDenied)) => {
                self.folder_plan(link).map_err(|_| first)
            }
            other => other,
        }
    }
}

impl ShareInspect for GdriveShareAdapter<'_> {
    fn inspect(
        &self,
        candidate: &crate::parser::CloudShareCandidate,
    ) -> Result<ShareInspection, QuarkError> {
        if candidate.provider != CloudProvider::Gdrive {
            return Err(QuarkError::InvalidResponse);
        }
        let link = parse_google_link(&candidate.url)?;
        let plan = match link.kind {
            GoogleLinkKind::File => self.file_plan(&link, None)?,
            GoogleLinkKind::Native(kind) => self.file_plan(&link, Some(kind))?,
            GoogleLinkKind::Folder => self.folder_plan(&link)?,
            GoogleLinkKind::FileOrFolder => self.ambiguous_plan(&link)?,
        };
        Ok(ShareInspection::Plan(plan))
    }
}

/// How the stored plan says an entry must be fetched.
fn entry_native_kind(entry: &CloudShareResolvedEntry) -> Result<Option<NativeKind>, QuarkError> {
    let context = &entry.plan.provider_context;
    let id = entry.entry.entry_id.as_str();
    let key = match context["kind"].as_str() {
        Some("file") if context["id"].as_str() == Some(id) => context["native"].as_str(),
        Some("folder") => context["natives"][id].as_str(),
        _ => return Err(QuarkError::InvalidResponse),
    };
    match key {
        None => Ok(None),
        Some(key) => NativeKind::from_key(key)
            .map(Some)
            .ok_or(QuarkError::InvalidResponse),
    }
}

impl ShareResolve for GdriveShareAdapter<'_> {
    fn resolve(&self, entry: &CloudShareResolvedEntry) -> Result<ShareResolution, QuarkError> {
        if entry.provider != CloudProvider::Gdrive || entry.plan.provider != CloudProvider::Gdrive {
            return Err(QuarkError::InvalidResponse);
        }
        let id = entry.entry.entry_id.as_str();
        if !super::link::valid_id(id) {
            return Err(QuarkError::InvalidResponse);
        }
        let context = &entry.plan.provider_context;
        // The root file's key applies to the root file only.
        let resource_key = (context["kind"] == "file")
            .then(|| context["resourceKey"].as_str())
            .flatten();
        let drive = self.public();
        let file = match entry_native_kind(entry)? {
            Some(kind) => drive.resolve_native(kind, id)?,
            None => drive.resolve_file(id, resource_key)?,
        };
        Ok(ShareResolution::Direct(ResolvedCloudDownload {
            url: file.url,
            name: entry.entry.name.clone(),
            headers: Vec::new(),
            transform: None,
        }))
    }
}

#[cfg(test)]
mod tests;
