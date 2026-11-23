use super::*;
use crate::cloud::share::provider_share_adapter;
use crate::cloud::{CloudHttpDetail, CloudHttpRequest, CloudHttpResponse, MemoryCredentialStore};
use crate::parser::CloudShareCandidate;
use std::sync::Mutex;

const FILE_ID: &str = "1l_5RK28JRL19wpT22B-DY9We3TVXnnQQ";
const FOLDER_ID: &str = "15uNXeRBIhVvZJIhL4yTw4IsStMhUaaxl";
const SUB_ID: &str = "1oow464Z4Et95RAwtRZn6dLOFb_YYpLwL";
const DOC_ID: &str = "1TFYNzuZJTgNGzGmjraZ58ZVOh9_YoKeBnU-opWgXQL4";

/// Scripted Google: the first rule whose text the request URL contains wins.
struct Google {
    rules: Vec<(String, CloudHttpDetail)>,
    requests: Mutex<Vec<CloudHttpRequest>>,
}

impl CloudHttpTransport for Google {
    fn send(&self, _: CloudHttpRequest) -> Result<crate::cloud::CloudHttpResponse, QuarkError> {
        panic!("the public flow must use the detailed transport")
    }
    fn send_detailed(&self, request: CloudHttpRequest) -> Result<CloudHttpDetail, QuarkError> {
        self.requests.lock().unwrap().push(request.clone());
        // A `range:` / `plain:` prefix restricts a rule to requests with or
        // without a Range header.
        let ranged = request.headers.contains_key("Range");
        let mut detail = self
            .rules
            .iter()
            .find(|(needle, _)| match needle.split_once(':') {
                Some(("range", rest)) => ranged && request.url.contains(rest),
                Some(("plain", rest)) => !ranged && request.url.contains(rest),
                _ => request.url.contains(needle.as_str()),
            })
            .map(|(_, detail)| detail.clone())
            .unwrap_or_else(|| reply(404, &[], "text/html", "not found"));
        if detail.final_url.is_empty() {
            detail.final_url = request.url;
        }
        Ok(detail)
    }
}

fn reply(status: u16, headers: &[(&str, &str)], content_type: &str, body: &str) -> CloudHttpDetail {
    let mut all: Vec<(String, String)> = headers
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    all.push(("content-type".into(), content_type.into()));
    CloudHttpDetail {
        response: CloudHttpResponse {
            status,
            body: body.into(),
            set_cookies: vec![],
        },
        headers: all,
        final_url: String::new(),
    }
}

fn file_head(name: &str, total: u64) -> CloudHttpDetail {
    reply(
        206,
        &[
            ("content-disposition", &format!(r#"attachment; filename="{name}""#)),
            ("content-range", &format!("bytes 0-0/{total}")),
        ],
        "application/octet-stream",
        "",
    )
}

fn scan_page(id: &str) -> CloudHttpDetail {
    reply(
        200,
        &[],
        "text/html; charset=utf-8",
        &format!(
            r#"<form id="download-form" action="https://drive.usercontent.google.com/download" method="get"><input type="hidden" name="id" value="{id}"><input type="hidden" name="export" value="download"><input type="hidden" name="confirm" value="t"><input type="hidden" name="uuid" value="u-1"></form>"#
        ),
    )
}

fn google(rules: Vec<(String, CloudHttpDetail)>) -> Google {
    Google {
        rules,
        requests: Mutex::new(vec![]),
    }
}

fn candidate(url: String) -> CloudShareCandidate {
    CloudShareCandidate {
        provider: CloudProvider::Gdrive,
        url,
        access_code: None,
    }
}

fn inspect(http: &Google, url: String) -> Result<CloudSharePlan, QuarkError> {
    let credentials = MemoryCredentialStore::default();
    let adapter = provider_share_adapter(CloudProvider::Gdrive, &credentials, http).unwrap();
    match adapter.inspect(&candidate(url))? {
        ShareInspection::Plan(plan) => Ok(plan),
        ShareInspection::AccessCodeRequired => panic!("Google never asks for an access code"),
    }
}

fn resolve(
    http: &Google,
    plan: &CloudSharePlan,
    entry: &CloudShareEntry,
) -> Result<ResolvedCloudDownload, QuarkError> {
    let credentials = MemoryCredentialStore::default();
    let adapter = provider_share_adapter(CloudProvider::Gdrive, &credentials, http).unwrap();
    match adapter.resolve(&CloudShareResolvedEntry {
        provider: CloudProvider::Gdrive,
        plan: plan.clone(),
        entry: entry.clone(),
    })? {
        ShareResolution::Direct(download) => Ok(download),
        ShareResolution::TransferRequired(_) => panic!("Google never transfers"),
    }
}

fn expect_failure<T>(result: Result<T, QuarkError>, expected: GdriveFailure) {
    match result {
        Err(QuarkError::Gdrive(failure)) => assert_eq!(failure, expected),
        Err(other) => panic!("unexpected error {other}"),
        Ok(_) => panic!("expected {expected:?}"),
    }
}

fn file_link() -> String {
    format!("https://drive.google.com/file/d/{FILE_ID}/view?usp=sharing")
}

#[test]
fn a_small_public_file_is_inspected_with_its_name_and_size_and_resolves_to_the_bytes_endpoint() {
    let http = google(vec![(FILE_ID.into(), file_head("spam.txt", 5))]);
    let plan = inspect(&http, file_link()).unwrap();
    assert_eq!(plan.entries.len(), 1);
    assert_eq!(plan.entries[0].name, "spam.txt");
    assert_eq!(plan.entries[0].size, Some(5));
    assert!(matches!(plan.root_kind, CloudShareRootKind::File { .. }));

    let download = resolve(&http, &plan, &plan.entries[0].clone()).unwrap();
    assert_eq!(
        download.url,
        format!("https://drive.usercontent.google.com/download?id={FILE_ID}&export=download")
    );
    assert_eq!(download.name, "spam.txt");
    assert!(download.headers.is_empty() && download.transform.is_none());
    // The probe asks for one byte only and for English pages.
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests[0].headers.get("Range").unwrap(), "bytes=0-0");
    assert!(requests[0].headers.get("Accept-Language").unwrap().starts_with("en"));
}

#[test]
fn a_large_file_follows_the_virus_scan_form_and_resolves_to_the_confirmed_address() {
    let http = google(vec![
        ("uuid=u-1".into(), file_head("big.npz", 498_881_336)),
        (FILE_ID.into(), scan_page(FILE_ID)),
    ]);
    let plan = inspect(&http, file_link()).unwrap();
    assert_eq!(plan.entries[0].name, "big.npz");
    assert_eq!(plan.entries[0].size, Some(498_881_336));
    let download = resolve(&http, &plan, &plan.entries[0].clone()).unwrap();
    assert_eq!(
        download.url,
        format!("https://drive.usercontent.google.com/download?id={FILE_ID}&export=download&confirm=t&uuid=u-1")
    );
}

#[test]
fn a_page_that_keeps_asking_for_confirmation_is_an_invalid_response() {
    let http = google(vec![(FILE_ID.into(), scan_page(FILE_ID))]);
    assert!(matches!(
        inspect(&http, file_link()),
        Err(QuarkError::InvalidResponse)
    ));
}

#[test]
fn quota_login_and_missing_files_have_their_own_errors() {
    let quota = reply(
        200,
        &[],
        "text/html",
        r#"<p class="uc-error-caption">Sorry</p><p class="uc-error-subcaption">Too many users have viewed or downloaded this file recently.</p>"#,
    );
    expect_failure(
        inspect(&google(vec![(FILE_ID.into(), quota)]), file_link()),
        GdriveFailure::QuotaExceeded,
    );
    let mut sign_in = reply(200, &[], "text/html", "<title>Sign in</title>");
    sign_in.final_url = "https://accounts.google.com/v3/signin/identifier?continue=x".into();
    expect_failure(
        inspect(&google(vec![(FILE_ID.into(), sign_in)]), file_link()),
        GdriveFailure::AccessDenied,
    );
    expect_failure(
        inspect(&google(vec![]), file_link()),
        GdriveFailure::NotFound,
    );
    let forbidden = reply(403, &[], "text/html", "denied");
    expect_failure(
        inspect(&google(vec![(FILE_ID.into(), forbidden)]), file_link()),
        GdriveFailure::AccessDenied,
    );
}

#[test]
fn an_empty_file_is_probed_again_without_a_range_after_google_refuses_byte_zero() {
    // Real behaviour: the one-byte range of a 0-byte file is an HTML 416 page.
    let refusal = reply(416, &[], "text/html; charset=utf-8", "<title>Error 416 (Bad Request)!!1</title>");
    let empty = reply(
        200,
        &[
            ("content-disposition", r#"attachment; filename="empty.txt""#),
            ("content-length", "0"),
        ],
        "application/octet-stream",
        "",
    );
    let http = google(vec![
        (format!("range:{FILE_ID}"), refusal),
        (format!("plain:{FILE_ID}"), empty),
    ]);
    let plan = inspect(&http, file_link()).unwrap();
    assert_eq!(plan.entries[0].name, "empty.txt");
    assert_eq!(plan.entries[0].size, Some(0));
    assert_eq!(http.requests.lock().unwrap().len(), 2);
}

#[test]
fn native_documents_are_exported_and_named_with_the_export_extension() {
    let export = reply(
        200,
        &[("content-disposition", r#"attachment; filename="notes""#)],
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "",
    );
    let http = google(vec![(format!("/document/d/{DOC_ID}/export"), export)]);
    let url = format!("https://docs.google.com/document/d/{DOC_ID}/edit?usp=sharing");
    let plan = inspect(&http, url).unwrap();
    assert_eq!(plan.entries[0].name, "notes.docx");
    assert_eq!(plan.entries[0].size, None);
    let download = resolve(&http, &plan, &plan.entries[0].clone()).unwrap();
    assert_eq!(
        download.url,
        format!("https://docs.google.com/document/d/{DOC_ID}/export?format=docx")
    );
    // The resolved export address is not mistaken for a share page.
    crate::cloud::share::validate_direct_url(&download.url).unwrap();
}

fn folder_page(entries: &str) -> CloudHttpDetail {
    reply(
        200,
        &[],
        "text/html",
        &format!(r#"<div id="flip-contents" class="flip-contents"><div class="flip-entries">{entries}</div></div>"#),
    )
}

fn entry(id: &str, href: &str, name: &str) -> String {
    format!(
        r#"<div class="flip-entry" id="entry-{id}"><div class="flip-entry-info"><a href="{href}"><div class="flip-entry-title">{name}</div></a></div></div>"#
    )
}

#[test]
fn a_public_folder_is_walked_with_nested_directories_and_exported_documents() {
    let root = [
        entry(SUB_ID, &format!("https://drive.google.com/drive/folders/{SUB_ID}"), "deep"),
        entry(DOC_ID, &format!("https://docs.google.com/document/d/{DOC_ID}/edit"), "notes"),
        entry("1cKSdgtWrPgvEsBGmjWALOH33taGyVXKb", "https://drive.google.com/file/d/1cKSdgtWrPgvEsBGmjWALOH33taGyVXKb/view", "spam.txt"),
    ]
    .concat();
    let nested = entry(
        "1Tcyw0gPGJEKZsQxnNzfSe5ZuTcBcM8NY",
        "https://drive.google.com/file/d/1Tcyw0gPGJEKZsQxnNzfSe5ZuTcBcM8NY/view",
        "spam4.txt",
    );
    let http = google(vec![
        (format!("embeddedfolderview?id={SUB_ID}"), folder_page(&nested)),
        (format!("embeddedfolderview?id={FOLDER_ID}"), folder_page(&root)),
    ]);
    let plan = inspect(
        &http,
        format!("https://drive.google.com/drive/folders/{FOLDER_ID}?usp=sharing"),
    )
    .unwrap();
    assert!(matches!(plan.root_kind, CloudShareRootKind::Directory));
    let mut listing: Vec<(String, Vec<String>)> = plan
        .entries
        .iter()
        .map(|entry| (entry.name.clone(), entry.relative_directory.clone()))
        .collect();
    listing.sort();
    assert_eq!(
        listing,
        vec![
            ("notes.docx".to_owned(), vec![]),
            ("spam.txt".to_owned(), vec![]),
            ("spam4.txt".to_owned(), vec!["deep".to_owned()]),
        ]
    );
    // Entries that are native documents are exported when resolved.
    let doc = plan.entries.iter().find(|entry| entry.entry_id == DOC_ID).unwrap();
    let export = reply(
        200,
        &[("content-disposition", r#"attachment; filename="notes.docx""#)],
        "application/octet-stream",
        "",
    );
    let exports = google(vec![(format!("/document/d/{DOC_ID}/export"), export)]);
    let download = resolve(&exports, &plan, doc).unwrap();
    assert!(download.url.ends_with("/export?format=docx"));
    assert_eq!(download.name, "notes.docx");
}

#[test]
fn a_folder_opened_through_an_open_link_is_recognised_after_the_file_probe_fails() {
    let page = folder_page(&entry(
        "1cKSdgtWrPgvEsBGmjWALOH33taGyVXKb",
        "https://drive.google.com/file/d/1cKSdgtWrPgvEsBGmjWALOH33taGyVXKb/view",
        "spam.txt",
    ));
    let http = google(vec![(format!("embeddedfolderview?id={FOLDER_ID}"), page)]);
    let plan = inspect(&http, format!("https://drive.google.com/open?id={FOLDER_ID}")).unwrap();
    assert!(matches!(plan.root_kind, CloudShareRootKind::Directory));
    assert_eq!(plan.entries.len(), 1);
    // Neither file nor folder: the first error stays.
    expect_failure(
        inspect(&google(vec![]), format!("https://drive.google.com/open?id={FOLDER_ID}")),
        GdriveFailure::NotFound,
    );
}

#[test]
fn a_private_folder_reports_missing_access_not_an_empty_listing() {
    let mut sign_in = reply(200, &[], "text/html", "<title>Sign in</title>");
    sign_in.final_url = "https://accounts.google.com/v3/signin/identifier".into();
    expect_failure(
        inspect(
            &google(vec![("embeddedfolderview".into(), sign_in)]),
            format!("https://drive.google.com/drive/folders/{FOLDER_ID}"),
        ),
        GdriveFailure::AccessDenied,
    );
}

#[test]
fn an_unrecognised_folder_page_is_reported_as_a_changed_page() {
    // The embedded folder view is a web page, not a published API: a page
    // without its list markers, or an unexpected status, means it changed.
    let url = || format!("https://drive.google.com/drive/folders/{FOLDER_ID}");
    let no_markers = reply(200, &[], "text/html", "<html><body>redesigned</body></html>");
    expect_failure(
        inspect(&google(vec![("embeddedfolderview".into(), no_markers)]), url()),
        GdriveFailure::FolderPageChanged,
    );
    let teapot = reply(418, &[], "text/html", "<html></html>");
    expect_failure(
        inspect(&google(vec![("embeddedfolderview".into(), teapot)]), url()),
        GdriveFailure::FolderPageChanged,
    );
}

#[test]
fn a_folder_beyond_the_defensive_limits_is_refused() {
    // Every folder contains one more folder: the walk must stop on its own.
    let rules = (0..40)
        .map(|index| {
            let id = format!("FOLDER{index:04}XXXX");
            let next = format!("FOLDER{:04}XXXX", index + 1);
            (
                format!("embeddedfolderview?id={id}"),
                folder_page(&entry(&next, &format!("https://drive.google.com/drive/folders/{next}"), "d")),
            )
        })
        .collect();
    let http = google(rules);
    expect_failure(
        inspect(&http, "https://drive.google.com/drive/folders/FOLDER0000XXXX".into()),
        GdriveFailure::FolderTooLarge,
    );
}

#[test]
fn unsupported_links_fail_before_any_request() {
    let http = google(vec![]);
    expect_failure(
        inspect(&http, "https://drive.google.com/drive/my-drive".into()),
        GdriveFailure::LinkUnsupported,
    );
    assert!(http.requests.lock().unwrap().is_empty());
}

#[test]
fn resolved_download_addresses_pass_the_share_page_check_but_share_pages_do_not() {
    use crate::cloud::share::validate_direct_url;
    for url in [
        format!("https://drive.usercontent.google.com/download?id={FILE_ID}&export=download"),
        format!("https://drive.usercontent.google.com/download?id={FILE_ID}&export=download&confirm=t&uuid=u-1"),
        format!("https://docs.google.com/spreadsheets/d/{DOC_ID}/export?format=xlsx"),
    ] {
        validate_direct_url(&url).unwrap_or_else(|error| panic!("{url}: {error}"));
    }
    // A page that merely describes the file is never accepted as the file.
    for url in [
        format!("https://drive.google.com/file/d/{FILE_ID}/view"),
        format!("https://drive.google.com/open?id={FILE_ID}"),
        format!("https://drive.google.com/drive/folders/{FOLDER_ID}"),
        format!("https://docs.google.com/document/d/{DOC_ID}/edit"),
        format!("https://drive.usercontent.google.com/open?id={FILE_ID}"),
    ] {
        assert!(validate_direct_url(&url).is_err(), "{url}");
    }
}
