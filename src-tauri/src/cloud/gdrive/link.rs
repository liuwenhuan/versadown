//! Recognition of Google Drive / Docs links.
//!
//! The file or folder id sits in a different place for every link shape, so
//! recognition is shape based rather than a single path prefix.

use super::failure::GdriveFailure;
use crate::cloud::QuarkError;
use url::Url;

/// Google-native documents have no bytes to download; they are exported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeKind {
    Document,
    Spreadsheet,
    Presentation,
    Drawing,
}

impl NativeKind {
    /// Stable key: the product path segment of docs.google.com, which is also
    /// what share plans store.
    pub fn key(self) -> &'static str {
        match self {
            Self::Document => "document",
            Self::Spreadsheet => "spreadsheets",
            Self::Presentation => "presentation",
            Self::Drawing => "drawings",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "document" => Some(Self::Document),
            "spreadsheets" => Some(Self::Spreadsheet),
            "presentation" => Some(Self::Presentation),
            "drawings" => Some(Self::Drawing),
            _ => None,
        }
    }

    /// Office formats keep the content editable; drawings only have image and
    /// PDF exports.
    pub fn export_format(self) -> &'static str {
        match self {
            Self::Document => "docx",
            Self::Spreadsheet => "xlsx",
            Self::Presentation => "pptx",
            Self::Drawing => "pdf",
        }
    }

    pub fn export_url(self, id: &str) -> String {
        format!(
            "https://docs.google.com/{}/d/{id}/export?format={}",
            self.key(),
            self.export_format()
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GoogleLinkKind {
    File,
    Folder,
    Native(NativeKind),
    /// `open?id=` and `uc?id=` links name a file or a folder without saying
    /// which; the inspector decides.
    FileOrFolder,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GoogleLink {
    pub kind: GoogleLinkKind,
    pub id: String,
    /// Older shared items need this key in addition to the id.
    pub resource_key: Option<String>,
}

pub fn valid_id(id: &str) -> bool {
    (8..=128).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn query_value(url: &Url, name: &str) -> Option<String> {
    url.query_pairs()
        .find_map(|(key, value)| (key == name).then(|| value.into_owned()))
}

/// The segment following `marker` in the path (`/d/<id>`, `/folders/<id>`).
fn id_after<'a>(segments: &[&'a str], marker: &str) -> Option<&'a str> {
    let position = segments.iter().position(|segment| *segment == marker)?;
    segments.get(position + 1).copied()
}

/// Parse a Google Drive / Docs link into the item it names.
pub fn parse_google_link(raw: &str) -> Result<GoogleLink, QuarkError> {
    let unsupported = || QuarkError::Gdrive(GdriveFailure::LinkUnsupported);
    let url = Url::parse(raw).map_err(|_| unsupported())?;
    let host = url.host_str().ok_or_else(unsupported)?;
    let segments: Vec<&str> = url
        .path_segments()
        .map(|segments| segments.filter(|segment| !segment.is_empty()).collect())
        .unwrap_or_default();
    let resource_key = query_value(&url, "resourcekey").filter(|key| {
        !key.is_empty()
            && key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    });
    let (kind, id) = match (host, segments.first().copied()) {
        ("drive.google.com" | "docs.google.com", Some("file")) => (
            GoogleLinkKind::File,
            id_after(&segments, "d").map(str::to_owned),
        ),
        ("drive.google.com", Some("drive" | "folders" | "folderview")) => {
            let by_path = id_after(&segments, "folders").map(str::to_owned);
            let by_query = (segments.first() == Some(&"folderview"))
                .then(|| query_value(&url, "id"))
                .flatten();
            (GoogleLinkKind::Folder, by_path.or(by_query))
        }
        ("drive.google.com", Some("open" | "uc"))
        | ("docs.google.com", Some("uc"))
        | ("drive.usercontent.google.com", Some("download")) => {
            let kind = if host == "drive.usercontent.google.com" {
                GoogleLinkKind::File
            } else {
                GoogleLinkKind::FileOrFolder
            };
            (kind, query_value(&url, "id"))
        }
        ("docs.google.com", Some(product)) => {
            let native = NativeKind::from_key(product).ok_or_else(unsupported)?;
            // `/d/e/2PACX-...` is a "published to the web" token, not a file id.
            let id = id_after(&segments, "d").filter(|id| *id != "e");
            (GoogleLinkKind::Native(native), id.map(str::to_owned))
        }
        _ => return Err(unsupported()),
    };
    let id = id.filter(|id| valid_id(id)).ok_or_else(unsupported)?;
    Ok(GoogleLink {
        kind,
        id,
        resource_key,
    })
}

/// Used by the share-URL router: does this URL name a downloadable item?
pub fn share_link_id(url: &Url) -> Option<String> {
    parse_google_link(url.as_str()).ok().map(|link| link.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "1l_5RK28JRL19wpT22B-DY9We3TVXnnQQ";

    #[test]
    fn every_file_link_shape_yields_the_id() {
        for url in [
            format!("https://drive.google.com/file/d/{ID}/view?usp=sharing"),
            format!("https://drive.google.com/file/d/{ID}"),
            format!("https://drive.google.com/file/u/1/d/{ID}/edit"),
            format!("https://docs.google.com/file/d/{ID}/edit"),
            format!("https://drive.usercontent.google.com/download?id={ID}&export=download"),
        ] {
            let link = parse_google_link(&url).unwrap();
            assert_eq!(
                (link.kind, link.id.as_str()),
                (GoogleLinkKind::File, ID),
                "{url}"
            );
        }
    }

    #[test]
    fn open_and_uc_links_do_not_say_whether_they_are_folders() {
        for url in [
            format!("https://drive.google.com/open?id={ID}"),
            format!("https://drive.google.com/uc?export=download&id={ID}"),
            format!("https://docs.google.com/uc?id={ID}"),
        ] {
            let link = parse_google_link(&url).unwrap();
            assert_eq!(link.kind, GoogleLinkKind::FileOrFolder, "{url}");
            assert_eq!(link.id, ID);
        }
    }

    #[test]
    fn folder_links_use_path_or_query_ids() {
        for url in [
            format!("https://drive.google.com/drive/folders/{ID}?usp=sharing"),
            format!("https://drive.google.com/drive/u/0/folders/{ID}"),
            format!("https://drive.google.com/drive/mobile/folders/{ID}"),
            format!("https://drive.google.com/folderview?id={ID}"),
        ] {
            let link = parse_google_link(&url).unwrap();
            assert_eq!(
                (link.kind, link.id.as_str()),
                (GoogleLinkKind::Folder, ID),
                "{url}"
            );
        }
    }

    #[test]
    fn native_documents_are_recognised_by_product() {
        for (product, kind) in [
            ("document", NativeKind::Document),
            ("spreadsheets", NativeKind::Spreadsheet),
            ("presentation", NativeKind::Presentation),
            ("drawings", NativeKind::Drawing),
        ] {
            let link =
                parse_google_link(&format!("https://docs.google.com/{product}/d/{ID}/edit"))
                    .unwrap();
            assert_eq!(link.kind, GoogleLinkKind::Native(kind));
            assert_eq!(link.id, ID);
        }
        assert_eq!(
            NativeKind::Document.export_url(ID),
            format!("https://docs.google.com/document/d/{ID}/export?format=docx")
        );
    }

    #[test]
    fn resource_key_is_kept_and_validated() {
        let link = parse_google_link(&format!(
            "https://drive.google.com/file/d/{ID}/view?resourcekey=0-AbC_d"
        ))
        .unwrap();
        assert_eq!(link.resource_key.as_deref(), Some("0-AbC_d"));
        let link = parse_google_link(&format!(
            "https://drive.google.com/file/d/{ID}/view?resourcekey=a%26b"
        ))
        .unwrap();
        assert_eq!(link.resource_key, None);
    }

    #[test]
    fn pages_without_a_downloadable_item_are_unsupported() {
        for url in [
            "https://drive.google.com/",
            "https://drive.google.com/drive/my-drive",
            "https://drive.google.com/drive/u/0/shared-with-me",
            "https://docs.google.com/forms/d/1l_5RK28JRL19wpT22B-DY9We3TVXnnQQ/edit",
            "https://docs.google.com/document/d/e/2PACX-1vQabcdefghijklmnop/pub",
            "https://drive.google.com/file/d/short/view",
            "https://drive.google.com/open",
            "https://example.com/file/d/1l_5RK28JRL19wpT22B-DY9We3TVXnnQQ",
        ] {
            assert!(
                matches!(
                    parse_google_link(url),
                    Err(QuarkError::Gdrive(GdriveFailure::LinkUnsupported))
                ),
                "{url}"
            );
        }
    }
}
