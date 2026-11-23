//! Parsers for the few HTML pages and headers of Google's public download
//! flow. Nothing here is hard-coded to one confirmation token: the virus-scan
//! page's form is read as served, so its parameter names and values may change.

use super::failure::GdriveFailure;
use super::link::NativeKind;
use regex::Regex;
use std::sync::OnceLock;
use url::Url;

/// Hosts a confirmation form or link may point at.
const TRUSTED_HOSTS: [&str; 3] = [
    "drive.usercontent.google.com",
    "drive.google.com",
    "docs.google.com",
];

fn pattern(cell: &'static OnceLock<Regex>, source: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(source).expect("static pattern"))
}

/// Decode the HTML entities that appear in Drive pages.
pub fn decode_entities(text: &str) -> String {
    static ENTITY: OnceLock<Regex> = OnceLock::new();
    pattern(&ENTITY, r"&(#x[0-9a-fA-F]+|#[0-9]+|[a-zA-Z]+);")
        .replace_all(text, |captures: &regex::Captures| {
            let entity = &captures[1];
            let decoded = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some('\u{a0}'),
                _ => entity
                    .strip_prefix("#x")
                    .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                    .or_else(|| entity.strip_prefix('#').and_then(|dec| dec.parse().ok()))
                    .and_then(char::from_u32),
            };
            decoded.map_or_else(|| captures[0].to_owned(), String::from)
        })
        .into_owned()
}

fn attribute(tag: &str, name: &str) -> Option<String> {
    let source = format!(r#"(?is)\b{}\s*=\s*(?:"([^"]*)"|'([^']*)')"#, regex::escape(name));
    let captures = Regex::new(&source).ok()?.captures(tag)?;
    let value = captures.get(1).or_else(|| captures.get(2))?;
    Some(decode_entities(value.as_str()))
}

/// A virus-scan confirmation: the request that streams the file anyway.
#[derive(Debug, PartialEq, Eq)]
pub struct ConfirmRequest {
    pub url: String,
}

fn trusted(url: &Url) -> bool {
    url.scheme() == "https"
        && url
            .host_str()
            .is_some_and(|host| TRUSTED_HOSTS.contains(&host))
}

/// Read the "Download anyway" form (current pages) or link (older pages).
/// `page_url` resolves relative targets.
pub fn parse_confirm_request(html: &str, page_url: &str) -> Option<ConfirmRequest> {
    static FORM: OnceLock<Regex> = OnceLock::new();
    static INPUT: OnceLock<Regex> = OnceLock::new();
    static LINK: OnceLock<Regex> = OnceLock::new();
    let base = Url::parse(page_url).ok()?;
    if let Some(form) = pattern(
        &FORM,
        r#"(?is)<form\b([^>]*\bid\s*=\s*["']download-form["'][^>]*)>(.*?)</form>"#,
    )
    .captures(html)
    {
        let action = attribute(&form[1], "action")?;
        let mut target = base.join(&action).ok()?;
        if !trusted(&target) {
            return None;
        }
        let mut pairs = Vec::new();
        for input in pattern(&INPUT, r"(?is)<input\b[^>]*>").find_iter(&form[2]) {
            let tag = input.as_str();
            if attribute(tag, "type").is_some_and(|kind| kind.eq_ignore_ascii_case("hidden")) {
                if let Some(name) = attribute(tag, "name") {
                    pairs.push((name, attribute(tag, "value").unwrap_or_default()));
                }
            }
        }
        if pairs.is_empty() {
            return None;
        }
        target.set_query(None);
        target.query_pairs_mut().extend_pairs(pairs);
        return Some(ConfirmRequest { url: target.into() });
    }
    let link = pattern(
        &LINK,
        r#"(?is)<a\b([^>]*\bid\s*=\s*["']uc-download-link["'][^>]*)>"#,
    )
    .captures(html)?;
    let target = base.join(&attribute(&link[1], "href")?).ok()?;
    trusted(&target).then(|| ConfirmRequest { url: target.into() })
}

/// An error page of the download flow ("Sorry, you can't view or download
/// this file at this time"). Quota pages are told apart by their English
/// sentence; the request asks for English so this text is stable.
pub fn classify_error_page(html: &str) -> Option<GdriveFailure> {
    // Every Drive page ships the same stylesheet, which names these classes
    // even on a healthy confirmation page: only an element using one counts.
    static ELEMENT: OnceLock<Regex> = OnceLock::new();
    if !pattern(
        &ELEMENT,
        r#"(?i)<[a-z0-9]+\b[^>]*\bclass\s*=\s*["'][^"']*\buc-error-(?:sub)?caption\b"#,
    )
    .is_match(html)
    {
        return None;
    }
    let lower = html.to_ascii_lowercase();
    Some(
        if lower.contains("too many users") || lower.contains("download quota") {
            GdriveFailure::QuotaExceeded
        } else {
            GdriveFailure::AccessDenied
        },
    )
}

/// Sign-in interstitials and redirects.
pub fn is_sign_in_page(final_url: &str) -> bool {
    Url::parse(final_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .is_some_and(|host| host == "accounts.google.com")
}

/// The file name of a `Content-Disposition` header, preferring RFC 5987.
pub fn content_disposition_name(value: &str) -> Option<String> {
    static EXTENDED: OnceLock<Regex> = OnceLock::new();
    static PLAIN: OnceLock<Regex> = OnceLock::new();
    if let Some(captures) = pattern(&EXTENDED, r#"(?i)filename\*\s*=\s*[^']*'[^']*'([^;]+)"#)
        .captures(value)
    {
        let decoded = percent_encoding::percent_decode_str(captures[1].trim())
            .decode_utf8()
            .ok()?;
        if !decoded.is_empty() {
            return Some(decoded.into_owned());
        }
    }
    let captures = pattern(
        &PLAIN,
        r#"(?i)filename\s*=\s*(?:"((?:[^"\\]|\\.)*)"|([^;]+))"#,
    )
    .captures(value)?;
    let raw = captures.get(1).or_else(|| captures.get(2))?.as_str().trim();
    let name = raw.replace("\\\"", "\"").replace("\\\\", "\\");
    (!name.is_empty()).then_some(name)
}

/// Total size from `Content-Range: bytes 0-0/<total>` (or `bytes */<total>`).
pub fn content_range_total(value: &str) -> Option<u64> {
    value.rsplit_once('/')?.1.trim().parse().ok()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderEntryKind {
    Folder,
    File,
    Native(NativeKind),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderEntry {
    pub id: String,
    pub name: String,
    pub kind: FolderEntryKind,
}

fn entry_kind(href: &str) -> Option<FolderEntryKind> {
    let url = Url::parse(href).ok()?;
    let segments: Vec<&str> = url.path_segments()?.collect();
    match url.host_str()? {
        "drive.google.com" => match segments.first().copied() {
            Some("drive") if segments.contains(&"folders") => Some(FolderEntryKind::Folder),
            Some("file") => Some(FolderEntryKind::File),
            _ => None,
        },
        "docs.google.com" => segments
            .first()
            .and_then(|product| NativeKind::from_key(product))
            .map(FolderEntryKind::Native),
        _ => None,
    }
}

/// The entries of an `embeddedfolderview` page. Items of kinds that cannot be
/// downloaded (forms, shortcuts to unknown targets, sites) are left out.
pub fn parse_folder_entries(html: &str) -> Vec<FolderEntry> {
    static ENTRY: OnceLock<Regex> = OnceLock::new();
    static TITLE: OnceLock<Regex> = OnceLock::new();
    static HREF: OnceLock<Regex> = OnceLock::new();
    let entry = pattern(
        &ENTRY,
        r#"<div class="flip-entry" id="entry-([A-Za-z0-9_-]+)""#,
    );
    let starts: Vec<(usize, String)> = entry
        .captures_iter(html)
        .filter_map(|captures| Some((captures.get(0)?.start(), captures[1].to_owned())))
        .collect();
    let title = pattern(&TITLE, r#"(?s)class="flip-entry-title">(.*?)</div>"#);
    let href = pattern(&HREF, r#"<a href="([^"]+)""#);
    let mut entries = Vec::new();
    for (index, (start, id)) in starts.iter().enumerate() {
        let end = starts
            .get(index + 1)
            .map_or(html.len(), |(next, _)| *next);
        let block = &html[*start..end];
        let (Some(name), Some(link)) = (title.captures(block), href.captures(block)) else {
            continue;
        };
        let Some(kind) = entry_kind(&decode_entities(&link[1])) else {
            continue;
        };
        entries.push(FolderEntry {
            id: id.clone(),
            name: decode_entities(name[1].trim()),
            kind,
        });
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCAN_PAGE: &str = r#"<html><body><div class="uc-main"><p class="uc-warning-caption">Google Drive can't scan this file for viruses.</p><p><a href="/open?id=FILEID123">big &amp; heavy.npz</a> (476M)</p><form id="download-form" action="https://drive.usercontent.google.com/download" method="get"><input type="submit" id="uc-download-link" value="Download anyway"/><input type="hidden" name="id" value="FILEID123"><input type="hidden" name="export" value="download"><input type="hidden" name="confirm" value="t"><input type="hidden" name="uuid" value="a0388201-dc24-4fa3-abe3-ce4034963f2a"></form></div></body></html>"#;
    const PAGE_URL: &str = "https://drive.usercontent.google.com/download?id=FILEID123";

    #[test]
    fn confirm_form_is_followed_with_whatever_parameters_it_carries() {
        let request = parse_confirm_request(SCAN_PAGE, PAGE_URL).unwrap();
        assert_eq!(
            request.url,
            "https://drive.usercontent.google.com/download?id=FILEID123&export=download&confirm=t&uuid=a0388201-dc24-4fa3-abe3-ce4034963f2a"
        );
        // A different parameter set is passed through untouched.
        let changed = SCAN_PAGE.replace("name=\"confirm\" value=\"t\"", "name=\"token\" value=\"x y\"");
        let request = parse_confirm_request(&changed, PAGE_URL).unwrap();
        assert!(request.url.contains("token=x+y"), "{}", request.url);
        assert!(!request.url.contains("confirm="));
    }

    #[test]
    fn legacy_confirm_link_is_supported() {
        let html = r#"<a id="uc-download-link" class="x" href="/uc?export=download&amp;confirm=AbC1&amp;id=FILEID123">Download anyway</a>"#;
        let request =
            parse_confirm_request(html, "https://drive.google.com/uc?id=FILEID123").unwrap();
        assert_eq!(
            request.url,
            "https://drive.google.com/uc?export=download&confirm=AbC1&id=FILEID123"
        );
    }

    #[test]
    fn confirm_targets_outside_google_are_refused() {
        let html = SCAN_PAGE.replace(
            "https://drive.usercontent.google.com/download",
            "https://evil.example/download",
        );
        assert_eq!(parse_confirm_request(&html, PAGE_URL), None);
        assert_eq!(parse_confirm_request("<html></html>", PAGE_URL), None);
    }

    #[test]
    fn error_pages_distinguish_quota_from_missing_permission() {
        let quota = r#"<p class="uc-error-caption">Sorry, you can't view or download this file at this time.</p><p class="uc-error-subcaption">Too many users have viewed or downloaded this file recently. Please try accessing the file again later.</p>"#;
        let denied = r#"<p class="uc-error-caption">Sorry, you can't view or download this file at this time.</p><p class="uc-error-subcaption">You need permission.</p>"#;
        assert_eq!(classify_error_page(quota), Some(GdriveFailure::QuotaExceeded));
        assert_eq!(classify_error_page(denied), Some(GdriveFailure::AccessDenied));
        assert_eq!(classify_error_page(SCAN_PAGE), None);
    }

    #[test]
    fn a_confirmation_page_whose_stylesheet_names_the_error_classes_is_not_an_error() {
        // Real pages carry one shared stylesheet for every outcome.
        let page = SCAN_PAGE.replace(
            "<html>",
            "<html><head><style>.uc-error-caption{font-size:16px}.uc-error-subcaption{color:#666}</style></head>",
        );
        assert_eq!(classify_error_page(&page), None);
        assert!(parse_confirm_request(&page, PAGE_URL).is_some());
    }

    #[test]
    fn sign_in_redirects_are_detected_by_host_only() {
        assert!(is_sign_in_page(
            "https://accounts.google.com/v3/signin/identifier?continue=x"
        ));
        assert!(!is_sign_in_page(
            "https://drive.usercontent.google.com/download?next=accounts.google.com"
        ));
        assert!(!is_sign_in_page(""));
    }

    #[test]
    fn content_disposition_prefers_the_unicode_name() {
        assert_eq!(
            content_disposition_name(r#"attachment; filename="spam.txt""#).as_deref(),
            Some("spam.txt")
        );
        assert_eq!(
            content_disposition_name(
                r#"attachment; filename="r_sum_.docx"; filename*=UTF-8''r%C3%A9sum%C3%A9.docx"#
            )
            .as_deref(),
            Some("résumé.docx")
        );
        assert_eq!(
            content_disposition_name(r#"attachment; filename="a \"b\".txt""#).as_deref(),
            Some(r#"a "b".txt"#)
        );
        assert_eq!(content_disposition_name("inline"), None);
    }

    #[test]
    fn content_range_gives_the_total() {
        assert_eq!(content_range_total("bytes 0-0/498881336"), Some(498881336));
        assert_eq!(content_range_total("bytes */0"), Some(0));
        assert_eq!(content_range_total("bytes 0-0/*"), None);
    }

    #[test]
    fn folder_entries_keep_names_kinds_and_skip_unsupported_items() {
        let html = r#"<div class="flip-entries"><div class="flip-entry" id="entry-FOLDER001" tabindex="0"><div class="flip-entry-info"><a href="https://drive.google.com/drive/folders/FOLDER001" target="_blank"><div class="flip-entry-title">deep &amp; folder</div></a></div></div><div class="flip-entry" id="entry-DOC00001"><div class="flip-entry-info"><a href="https://docs.google.com/document/d/DOC00001/edit?usp=drive_web"><div class="flip-entry-title">notes.docx</div></a></div></div><div class="flip-entry" id="entry-FORM0001"><div class="flip-entry-info"><a href="https://docs.google.com/forms/d/FORM0001/edit"><div class="flip-entry-title">survey</div></a></div></div><div class="flip-entry" id="entry-FILE0001"><div class="flip-entry-info"><a href="https://drive.google.com/file/d/FILE0001/view?usp=drive_web"><div class="flip-entry-title">it&#39;s.txt</div></a></div></div></div>"#;
        let entries = parse_folder_entries(html);
        assert_eq!(
            entries,
            vec![
                FolderEntry {
                    id: "FOLDER001".into(),
                    name: "deep & folder".into(),
                    kind: FolderEntryKind::Folder
                },
                FolderEntry {
                    id: "DOC00001".into(),
                    name: "notes.docx".into(),
                    kind: FolderEntryKind::Native(NativeKind::Document)
                },
                FolderEntry {
                    id: "FILE0001".into(),
                    name: "it's.txt".into(),
                    kind: FolderEntryKind::File
                },
            ]
        );
    }
}
