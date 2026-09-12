use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;
use url::Url;

use crate::cloud::CloudProvider;

#[derive(Clone, Eq, PartialEq)]
pub struct CloudShareCandidate {
    pub provider: CloudProvider,
    pub url: String,
    pub access_code: Option<String>,
}

impl fmt::Debug for CloudShareCandidate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CloudShareCandidate")
            .field("provider", &self.provider)
            .field("url", &"<redacted>")
            .field("access_code", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ShareInputError {
    #[error("一次只能解析一个网盘分享链接")]
    MultipleLinks,
    #[error("访问码格式无效")]
    InvalidAccessCode,
}

impl CloudShareCandidate {
    fn from_candidate(candidate: Candidate) -> Option<Self> {
        let mut url = Url::parse(&candidate.uri).ok()?;
        let provider = cloud_provider_for_url(&url)?;
        let access_code = access_code_from_query(&url);
        let remaining_query = url
            .query_pairs()
            .filter(|(name, _)| !is_access_code_parameter(name))
            .map(|(name, value)| (name.into_owned(), value.into_owned()))
            .collect::<Vec<_>>();
        {
            let mut query = url.query_pairs_mut();
            query.clear();
            query.extend_pairs(remaining_query.iter().map(|(name, value)| (name, value)));
        }
        Some(Self {
            provider,
            url: url.into(),
            access_code,
        })
    }

    fn with_access_code(mut self, access_code: Option<&str>) -> Result<Self, ShareInputError> {
        self.access_code = access_code.map(normalize_access_code).transpose()?;
        Ok(self)
    }
}

pub fn extract_cloud_share_candidate(
    input: &str,
    entered_access_code: Option<&str>,
) -> Result<Option<CloudShareCandidate>, ShareInputError> {
    let values = extract_candidates(input)
        .into_iter()
        .filter_map(CloudShareCandidate::from_candidate)
        .collect::<Vec<_>>();
    match values.as_slice() {
        [] => Ok(None),
        [value] => {
            let inferred_access_code = value
                .access_code
                .clone()
                .or_else(|| access_code_from_text(input));
            Ok(Some(value.clone().with_access_code(
                entered_access_code.or(inferred_access_code.as_deref()),
            )?))
        }
        _ => Err(ShareInputError::MultipleLinks),
    }
}

fn cloud_provider_for_url(url: &Url) -> Option<CloudProvider> {
    crate::cloud::ProviderRegistry::global()
        .for_share_url(url)
        .map(|descriptor| descriptor.provider)
}

fn access_code_from_query(url: &Url) -> Option<String> {
    url.query_pairs()
        .find(|(name, _)| is_access_code_parameter(name))
        .map(|(_, value)| value.into_owned())
}

fn is_access_code_parameter(name: &str) -> bool {
    matches!(name.to_ascii_lowercase().as_str(), "pwd" | "passcode")
}

fn access_code_from_text(input: &str) -> Option<String> {
    Regex::new(r"(?i)(?:提取码|访问码)\s*[:：]\s*([a-z0-9]+)")
        .expect("static access code pattern")
        .captures(input)
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str().to_string())
}

fn normalize_access_code(value: &str) -> Result<String, ShareInputError> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|character| character.is_ascii_alphanumeric())
    {
        return Err(ShareInputError::InvalidAccessCode);
    }
    Ok(value.to_string())
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateKind {
    Direct,
    Ed2k,
    Ftp,
    Hls,
    Dash,
    Magnet,
    Torrent,
    Thunder,
    QuarkShare,
    PikPakShare,
    Text,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Candidate {
    pub kind: CandidateKind,
    pub uri: String,
    pub auto_start: bool,
}

pub fn classify_input(input: &str) -> Candidate {
    let uri = input.trim().to_string();
    let lower = uri.to_lowercase();
    let path_without_query = lower.split(['?', '#']).next().unwrap_or(&lower);
    let share_provider = Url::parse(&uri)
        .ok()
        .as_ref()
        .and_then(|url| crate::cloud::ProviderRegistry::global().for_candidate_url(url))
        .map(|d| d.provider);
    let kind = if valid_ed2k_file_link(&uri) {
        CandidateKind::Ed2k
    } else if lower.starts_with("ed2k:") {
        CandidateKind::Text
    } else if lower.starts_with("magnet:") {
        CandidateKind::Magnet
    } else if lower.starts_with("thunder://") {
        CandidateKind::Thunder
    } else if lower.starts_with("ftp://") {
        CandidateKind::Ftp
    } else if share_provider == Some(CloudProvider::Quark) {
        CandidateKind::QuarkShare
    } else if share_provider == Some(CloudProvider::Pikpak) {
        CandidateKind::PikPakShare
    } else if path_without_query.ends_with(".torrent") {
        CandidateKind::Torrent
    } else if (lower.starts_with("http://") || lower.starts_with("https://"))
        && path_without_query.ends_with(".m3u8")
    {
        CandidateKind::Hls
    } else if (lower.starts_with("http://") || lower.starts_with("https://"))
        && path_without_query.ends_with(".mpd")
    {
        CandidateKind::Dash
    } else if lower.starts_with("http://") || lower.starts_with("https://") {
        CandidateKind::Direct
    } else {
        CandidateKind::Text
    };
    let auto_start = matches!(
        kind,
        CandidateKind::Direct
            | CandidateKind::Ed2k
            | CandidateKind::Ftp
            | CandidateKind::Hls
            | CandidateKind::Dash
            | CandidateKind::Thunder
    );
    Candidate {
        kind,
        uri,
        auto_start,
    }
}

pub fn extract_candidates(text: &str) -> Vec<Candidate> {
    let pattern = Regex::new(
        r"(?i)(ed2k://\|file\|[^\r\n<>]*?\|/|magnet:\?[^\s<>]+|thunder://[^\s<>]+|ftp://[^\s<>]+|https?://[^\s<>]+)",
    )
    .expect("static candidate pattern");
    pattern
        .find_iter(text)
        .map(|match_| {
            classify_input(
                match_
                    .as_str()
                    .trim_end_matches(|character: char| ".,;；，。)】]}".contains(character)),
            )
        })
        .collect()
}

fn valid_ed2k_file_link(value: &str) -> bool {
    let parts = value.split('|').collect::<Vec<_>>();
    parts.len() >= 6
        && parts[0].eq_ignore_ascii_case("ed2k://")
        && parts[1].eq_ignore_ascii_case("file")
        && !parts[2].trim().is_empty()
        && parts[3].parse::<u64>().is_ok()
        && parts[4].len() == 32
        && parts[4].bytes().all(|byte| byte.is_ascii_hexdigit())
        && parts.last() == Some(&"/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::CloudProvider;

    #[test]
    fn cloud_share_candidate_debug_keeps_provider_but_redacts_url_and_access_code() {
        let candidate = CloudShareCandidate {
            provider: CloudProvider::Baidu,
            url: "https://pan.baidu.com/s/private-share-secret".into(),
            access_code: Some("FAKE-CODE-SECRET".into()),
        };

        assert_eq!(candidate.clone(), candidate);
        let debug = format!("{candidate:?}");
        assert!(debug.contains("Baidu"));
        for secret in ["private-share-secret", "FAKE-CODE-SECRET", "pan.baidu.com"] {
            assert!(
                !debug.contains(secret),
                "debug output leaked {secret}: {debug}"
            );
        }
    }

    #[test]
    fn recognizes_all_provider_hosts_and_strips_pwd_from_canonical_url() {
        for (input, provider) in [
            ("https://pan.quark.cn/s/a?pwd=1234", CloudProvider::Quark),
            (
                "https://mypikpak.com/s/a 提取码: 1234",
                CloudProvider::Pikpak,
            ),
            ("https://pan.baidu.com/s/a?pwd=1234", CloudProvider::Baidu),
            (
                "https://cloud.189.cn/t/a 访问码：1234",
                CloudProvider::Tianyi,
            ),
            ("https://www.alipan.com/s/a?pwd=1234", CloudProvider::Aliyun),
            ("https://pan.xunlei.com/s/a?pwd=1234", CloudProvider::Xunlei),
            ("https://drive.uc.cn/s/a?pwd=1234", CloudProvider::Uc),
        ] {
            let value = extract_cloud_share_candidate(input, None).unwrap().unwrap();
            assert_eq!(value.provider, provider);
            assert_eq!(value.access_code.as_deref(), Some("1234"));
            assert!(!value.url.contains("pwd="));
        }
    }

    #[test]
    fn rejects_multiple_share_urls() {
        assert!(matches!(
            extract_cloud_share_candidate(
                "https://pan.baidu.com/s/a https://pan.xunlei.com/s/b",
                None
            ),
            Err(ShareInputError::MultipleLinks)
        ));
    }
}
