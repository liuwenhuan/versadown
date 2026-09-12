use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const LATEST_RELEASE_URL: &str = "https://versadown.aigutta.com/releases/latest.json";
pub const DOWNLOAD_PAGE_URL: &str = "https://versadown.aigutta.com/download.html";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppUpdate {
    pub version: String,
    pub notes: String,
    pub download_url: String,
}

#[derive(Deserialize)]
struct ReleaseManifest {
    version: String,
    #[serde(default)]
    notes: String,
}

pub fn fetch_app_update(current_version: &str) -> Result<Option<AppUpdate>, String> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(format!("VersaDown/{current_version}"))
        .build()
        .map_err(|error| format!("无法创建版本检测请求：{error}"))?;
    let manifest = client
        .get(LATEST_RELEASE_URL)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|error| format!("无法获取版本信息：{error}"))?
        .json::<ReleaseManifest>()
        .map_err(|error| format!("版本信息格式无效：{error}"))?;
    app_update_from_manifest(current_version, manifest)
}

fn app_update_from_manifest(
    current_version: &str,
    manifest: ReleaseManifest,
) -> Result<Option<AppUpdate>, String> {
    let current = parse_version(current_version).ok_or_else(|| "当前版本格式无效".to_string())?;
    let latest = parse_version(&manifest.version).ok_or_else(|| "最新版本格式无效".to_string())?;
    if latest <= current {
        return Ok(None);
    }
    let notes = manifest.notes.trim();
    Ok(Some(AppUpdate {
        version: manifest.version.trim().to_string(),
        notes: if notes.is_empty() {
            "查看本次更新内容。".into()
        } else {
            notes.into()
        },
        download_url: DOWNLOAD_PAGE_URL.into(),
    }))
}

fn parse_version(value: &str) -> Option<[u64; 3]> {
    let value = value
        .trim()
        .strip_prefix('v')
        .or_else(|| value.trim().strip_prefix('V'))
        .unwrap_or(value.trim());
    let mut parts = value.split('.');
    let parse_part = |part: Option<&str>| part?.parse::<u64>().ok();
    let parsed = [
        parse_part(parts.next())?,
        parse_part(parts.next())?,
        parse_part(parts.next())?,
    ];
    if parts.next().is_some() {
        None
    } else {
        Some(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_an_update_only_for_a_newer_semantic_version() {
        let update = app_update_from_manifest(
            "0.1.0",
            ReleaseManifest {
                version: "0.2.0".into(),
                notes: "改进更新体验".into(),
            },
        )
        .unwrap()
        .unwrap();

        assert_eq!(update.version, "0.2.0");
        assert_eq!(update.notes, "改进更新体验");
        assert_eq!(update.download_url, DOWNLOAD_PAGE_URL);
        assert!(app_update_from_manifest(
            "0.2.0",
            ReleaseManifest {
                version: "0.2.0".into(),
                notes: String::new()
            }
        )
        .unwrap()
        .is_none());
        assert!(app_update_from_manifest(
            "0.3.0",
            ReleaseManifest {
                version: "0.2.9".into(),
                notes: String::new()
            }
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn rejects_a_non_semantic_release_version() {
        assert!(app_update_from_manifest(
            "0.1.0",
            ReleaseManifest {
                version: "next".into(),
                notes: String::new()
            }
        )
        .is_err());
    }
}
