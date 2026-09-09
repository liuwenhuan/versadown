use serde::Deserialize;
use thiserror::Error;

use crate::{downloader::load_torrent_base64, DownloadError, MagnetMetadataSettings};

const METADATA_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
const METADATA_USER_AGENT: &str = "AllDownloader/0.1";

#[derive(Debug, Error)]
pub enum MagnetMetadataError {
    #[error("磁链解析服务地址无效")]
    InvalidBaseUrl,
    #[error("无法创建磁链解析请求：{0}")]
    Client(String),
    #[error("磁链解析服务不可用：{0}")]
    Request(String),
    #[error("磁链解析服务返回 HTTP {0}")]
    ResponseStatus(u16),
    #[error("磁链解析服务返回的数据无效：{0}")]
    InvalidResponse(String),
    #[error("磁链解析服务返回了不受信任的种子地址")]
    UnsafeTorrentUrl,
    #[error(transparent)]
    Torrent(#[from] DownloadError),
}

#[derive(Deserialize)]
struct MetadataResponse {
    #[serde(default)]
    download_url: String,
}

/// Resolve a magnet through the configured metadata service. A successful metadata
/// lookup without a cached torrent is represented by `Ok(None)` so the caller can
/// transparently continue with local aria2 DHT discovery.
pub async fn resolve_magnet_torrent(
    settings: &MagnetMetadataSettings,
    magnet: &str,
) -> Result<Option<String>, MagnetMetadataError> {
    if !settings.enabled {
        return Ok(None);
    }
    let base_url = normalized_base_url(&settings.base_url)?;
    let endpoint = base_url
        .join("api/v1/metadata")
        .map_err(|_| MagnetMetadataError::InvalidBaseUrl)?;
    let client = reqwest::Client::builder()
        .user_agent(METADATA_USER_AGENT)
        .connect_timeout(METADATA_TIMEOUT.min(std::time::Duration::from_secs(8)))
        .read_timeout(METADATA_TIMEOUT)
        .timeout(METADATA_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
        .map_err(|error| MagnetMetadataError::Client(error.to_string()))?;
    let response = client
        .post(endpoint)
        .json(&serde_json::json!({ "magnet_uri": magnet }))
        .send()
        .await
        .map_err(|error| MagnetMetadataError::Request(error.to_string()))?;
    if !response.status().is_success() {
        return Err(MagnetMetadataError::ResponseStatus(
            response.status().as_u16(),
        ));
    }
    let metadata = response
        .json::<MetadataResponse>()
        .await
        .map_err(|error| MagnetMetadataError::InvalidResponse(error.to_string()))?;
    if metadata.download_url.trim().is_empty() {
        return Ok(None);
    }
    let torrent_url = resolve_download_url(&base_url, metadata.download_url.trim())?;
    Ok(Some(load_torrent_base64(torrent_url.as_str()).await?))
}

fn normalized_base_url(value: &str) -> Result<url::Url, MagnetMetadataError> {
    let mut url = url::Url::parse(value.trim()).map_err(|_| MagnetMetadataError::InvalidBaseUrl)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(MagnetMetadataError::InvalidBaseUrl);
    }
    url.set_query(None);
    url.set_fragment(None);
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url)
}

fn resolve_download_url(base_url: &url::Url, value: &str) -> Result<url::Url, MagnetMetadataError> {
    let resolved = base_url
        .join(value)
        .map_err(|_| MagnetMetadataError::InvalidResponse("下载地址无效".into()))?;
    if !matches!(resolved.scheme(), "http" | "https") {
        return Err(MagnetMetadataError::UnsafeTorrentUrl);
    }
    if resolved.scheme() == base_url.scheme()
        && resolved.host_str() == base_url.host_str()
        && resolved.port_or_known_default() == base_url.port_or_known_default()
    {
        return Ok(resolved);
    }
    // The upstream service defaults DOWNLOAD_BASE_URL to localhost. Reuse only
    // its well-known cache path on the server the user actually configured;
    // never follow a cross-origin address supplied in the response.
    let localhost = matches!(resolved.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
    if localhost {
        if resolved.path().starts_with("/download/") {
            let mut rewritten = base_url
                .join(resolved.path())
                .map_err(|_| MagnetMetadataError::InvalidResponse("下载地址无效".into()))?;
            rewritten.set_query(resolved.query());
            return Ok(rewritten);
        }
        return Err(MagnetMetadataError::UnsafeTorrentUrl);
    }
    if resolved.scheme() == "https"
        && resolved.host_str().is_some()
        && resolved.username().is_empty()
        && resolved.password().is_none()
    {
        return Ok(resolved);
    }
    Err(MagnetMetadataError::UnsafeTorrentUrl)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_an_external_https_torrent_mirror_returned_by_the_metadata_service() {
        let base_url = normalized_base_url("http://peer.example.test:8080").unwrap();

        let resolved = resolve_download_url(
            &base_url,
            "https://torrent-mirror.example.test/cache/example.torrent",
        )
        .unwrap();

        assert_eq!(
            resolved.as_str(),
            "https://torrent-mirror.example.test/cache/example.torrent"
        );
    }
}
