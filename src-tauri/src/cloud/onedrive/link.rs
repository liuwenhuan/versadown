//! Recognising and normalising OneDrive share links.
//!
//! Microsoft's share API accepts the whole link, base64url-encoded behind a
//! `u!` prefix, so no short-link expansion is needed: the link is passed on
//! as the user pasted it (minus the fragment and the `e` tracking parameter).

use crate::cloud::QuarkError;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use url::Url;

const SHORT_HOST: &str = "1drv.ms";
const WEB_HOST: &str = "onedrive.live.com";

/// Whether `url` has the shape of a OneDrive share link. Used by the input
/// parser, so it must stay cheap and free of network access.
pub(crate) fn is_share_link(url: &Url) -> bool {
    if url.password().is_some() || !url.username().is_empty() || url.port().is_some() {
        return false;
    }
    match url.host_str() {
        Some(SHORT_HOST) => short_link_path(url.path()),
        Some(WEB_HOST) => web_link(url),
        _ => false,
    }
}

/// `/u/s!TOKEN`, `/f/s!TOKEN`, or the newer `/t/c/<cid>/<token>`: one type
/// letter followed by at least one token segment.
fn short_link_path(path: &str) -> bool {
    let mut segments = path.trim_start_matches('/').split('/');
    let kind = segments.next().unwrap_or_default();
    if kind.len() != 1 || !kind.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return false;
    }
    let rest: Vec<&str> = segments.collect();
    !rest.is_empty()
        && rest.iter().all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'!'))
        })
}

fn web_link(url: &Url) -> bool {
    // `/:u:/g/personal/...` (expanded share), or the classic `redir`, `embed`,
    // `download` and root pages that carry the item in the query string.
    if url.path().starts_with("/:") {
        return true;
    }
    matches!(
        url.path(),
        "/" | "/redir" | "/embed" | "/download" | "/view.aspx"
    ) && url
        .query_pairs()
        .any(|(name, _)| matches!(&*name, "resid" | "id" | "redeem"))
}

/// The link to hand to the share API: no fragment, no `e` tracking parameter.
pub(crate) fn normalized_link(raw: &str) -> Result<String, QuarkError> {
    let mut url = Url::parse(raw.trim()).map_err(|_| QuarkError::InvalidResponse)?;
    if !is_share_link(&url) {
        return Err(QuarkError::InvalidResponse);
    }
    url.set_fragment(None);
    let kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(name, _)| name != "e")
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect();
    if kept.is_empty() {
        url.set_query(None);
    } else {
        url.query_pairs_mut().clear().extend_pairs(kept);
    }
    Ok(url.into())
}

/// The `u!`-prefixed share id used by the share API.
pub(crate) fn share_id(normalized: &str) -> String {
    format!("u!{}", URL_SAFE_NO_PAD.encode(normalized.as_bytes()))
}

/// Item and drive ids go into URL paths; only the characters OneDrive uses
/// for them are allowed so a tampered plan cannot redirect a request.
pub(crate) fn valid_resource_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'!' | b'_' | b'-' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(raw: &str) -> bool {
        is_share_link(&Url::parse(raw).unwrap())
    }

    #[test]
    fn recognises_current_and_classic_share_links() {
        for raw in [
            "https://1drv.ms/u/s!AbCdEf_gh-IJ",
            "https://1drv.ms/f/s!AbCdEf_gh-IJ?e=XyZ123",
            "https://1drv.ms/t/c/0123456789abcdef/EaBcDeFgHi",
            "https://onedrive.live.com/:u:/g/personal/ABCDEF0123456789/s!AbCd?resid=ABCDEF0123456789!114",
            "https://onedrive.live.com/redir?resid=ABCDEF0123456789!114&authkey=!Abc",
            "https://onedrive.live.com/?cid=abcdef0123456789&id=ABCDEF0123456789!114&authkey=!Abc",
            "https://onedrive.live.com/embed?resid=ABCDEF0123456789!114&authkey=!Abc",
        ] {
            assert!(link(raw), "{raw}");
        }
    }

    #[test]
    fn rejects_pages_and_lookalikes() {
        for raw in [
            "https://1drv.ms/",
            "https://1drv.ms/u",
            "https://1drv.ms/abc/s!Token",
            "https://onedrive.live.com/about/en-us/",
            "https://onedrive.live.com/",
            "https://onedrive.live.com.evil.example/redir?resid=1",
            "https://evil.example/1drv.ms/u/s!Token",
            "https://user:pw@1drv.ms/u/s!Token",
            "https://1drv.ms:8443/u/s!Token",
        ] {
            assert!(!link(raw), "{raw}");
        }
    }

    #[test]
    fn normalization_drops_fragment_and_tracking_parameter_only() {
        assert_eq!(
            normalized_link("https://1drv.ms/u/s!AbC?e=track#frag").unwrap(),
            "https://1drv.ms/u/s!AbC"
        );
        assert_eq!(
            normalized_link("https://onedrive.live.com/redir?resid=A!1&authkey=!Z&e=x").unwrap(),
            "https://onedrive.live.com/redir?resid=A%211&authkey=%21Z"
        );
        assert!(normalized_link("https://example.com/u/s!AbC").is_err());
    }

    #[test]
    fn share_id_is_url_safe_base64_without_padding() {
        // Fixture from the verified live exchange: the link of a public file.
        assert_eq!(
            share_id("https://1drv.ms/u/s!Aus8VCZ_C_33ck0gvo5jfoWBOPo"),
            "u!aHR0cHM6Ly8xZHJ2Lm1zL3UvcyFBdXM4VkNaX0NfMzNjazBndm81amZvV0JPUG8"
        );
    }

    #[test]
    fn resource_ids_cannot_escape_the_path() {
        assert!(valid_resource_id("56B9F9C97F261712!10698"));
        assert!(!valid_resource_id("../x"));
        assert!(!valid_resource_id("a/b"));
        assert!(!valid_resource_id("a?b"));
        assert!(!valid_resource_id(""));
    }
}
