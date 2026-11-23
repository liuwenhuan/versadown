use super::*;
use crate::cloud::onedrive::link::share_id;
use crate::cloud::onedrive::test_support::ScriptedHttp;
use crate::parser::CloudShareCandidate;
use serde_json::{json, Value};

const API: &str = "https://my.microsoftpersonalcontent.com/_api/v2.0";
const TOKEN_URL: &str = "https://api-badgerp.svc.ms/v1.0/token";
const DRIVE: &str = "ABCDEF0123456789";
const FILE_LINK: &str = "https://1drv.ms/u/s!AbCdEf_gh";
const FOLDER_LINK: &str = "https://1drv.ms/f/s!ZyXwVu_ts?e=track";

fn candidate(url: &str) -> CloudShareCandidate {
    CloudShareCandidate {
        provider: CloudProvider::Onedrive,
        url: url.into(),
        access_code: None,
    }
}

fn file_json(id: &str, name: &str, size: u64) -> Value {
    json!({
        "id": format!("{DRIVE}!{id}"), "name": name, "size": size,
        "file": {"mimeType": "application/octet-stream"},
        "parentReference": {"driveId": DRIVE},
        "@content.downloadUrl": format!("https://dl.example/{id}?tempauth=SECRET-{id}"),
    })
}

fn folder_json(id: &str, name: &str) -> Value {
    json!({
        "id": format!("{DRIVE}!{id}"), "name": name, "size": 0,
        "folder": {"childCount": 2},
        "parentReference": {"driveId": DRIVE},
    })
}

fn root_route(http: ScriptedHttp, link: &str, status: u16, body: Value) -> ScriptedHttp {
    let normalized = crate::cloud::onedrive::link::normalized_link(link).unwrap();
    http.on(
        "GET",
        &format!("{API}/shares/{}/driveitem", share_id(&normalized)),
        vec![(status, body)],
    )
    .on("POST", TOKEN_URL, vec![(200, json!({"token": "TOKEN-1"}))])
}

fn inspect(http: &ScriptedHttp, url: &str) -> Result<CloudSharePlan, QuarkError> {
    let adapter = OnedriveShareAdapter::new(http);
    match adapter.inspect(&candidate(url))? {
        ShareInspection::Plan(plan) => Ok(plan),
        ShareInspection::AccessCodeRequired => panic!("OneDrive never asks for an access code"),
    }
}

fn resolve(
    http: &ScriptedHttp,
    plan: &CloudSharePlan,
    entry: &CloudShareEntry,
) -> Result<ShareResolution, QuarkError> {
    OnedriveShareAdapter::new(http).resolve(&CloudShareResolvedEntry {
        provider: CloudProvider::Onedrive,
        plan: plan.clone(),
        entry: entry.clone(),
    })
}

fn failure(result: Result<impl Sized, QuarkError>, expected: OnedriveFailure) {
    match result {
        Err(QuarkError::Onedrive(actual)) => assert_eq!(actual, expected),
        Err(other) => panic!("unexpected error {other}"),
        Ok(_) => panic!("expected {expected:?}"),
    }
}

#[test]
fn single_file_share_is_inspected_and_resolved_with_a_fresh_url() {
    let item = file_json("114", "model.pth", 1234);
    let http = root_route(ScriptedHttp::new(), FILE_LINK, 200, item.clone()).on(
        "GET",
        &format!("{API}/drives/{DRIVE}/items/{DRIVE}!114"),
        vec![(200, item)],
    );
    let plan = inspect(&http, FILE_LINK).unwrap();

    assert_eq!(plan.provider, CloudProvider::Onedrive);
    assert_eq!(plan.entries.len(), 1);
    assert_eq!(plan.entries[0].name, "model.pth");
    assert_eq!(plan.entries[0].size, Some(1234));
    assert!(plan.entries[0].relative_directory.is_empty());
    assert!(matches!(plan.root_kind, CloudShareRootKind::File { .. }));

    let ShareResolution::Direct(download) = resolve(&http, &plan, &plan.entries[0]).unwrap() else {
        panic!("OneDrive never transfers");
    };
    assert_eq!(download.url, "https://dl.example/114?tempauth=SECRET-114");
    assert_eq!(download.name, "model.pth");
    // The pre-authorized URL needs no extra headers and no transform.
    assert!(download.headers.is_empty() && download.transform.is_none());

    // Every operation fetches its own anonymous token and redeems the share
    // before reading items below it.
    assert_eq!(http.sent("POST", TOKEN_URL).len(), 2);
    let item_request = &http.sent("GET", &format!("{API}/drives/"))[0];
    assert_eq!(item_request.headers["Authorization"], "Badger TOKEN-1");
    assert_eq!(item_request.headers["Prefer"], "autoredeem");
}

#[test]
fn folder_share_is_listed_recursively_across_pages() {
    let root = folder_json("1", "root");
    let sub = folder_json("3", "sub");
    let http = root_route(ScriptedHttp::new(), FOLDER_LINK, 200, root)
        .on(
            "GET",
            &format!("{API}/drives/{DRIVE}/items/{DRIVE}!1/children"),
            vec![
                (
                    200,
                    json!({
                        "value": [file_json("2", "b.txt", 20), sub],
                        "@odata.nextLink": format!("{API}/drives/{DRIVE}/items/{DRIVE}!1/children?$skiptoken=P2")
                    }),
                ),
                (
                    200,
                    json!({"value": [
                        file_json("4", "a.txt", 10),
                        // A shortcut into another drive and a OneNote package
                        // are not files of this share.
                        {"id": "X!9", "name": "shortcut", "remoteItem": {}, "file": {}},
                        {"id": "X!8", "name": "notebook", "package": {"type": "oneNote"}},
                    ]}),
                ),
            ],
        )
        .on(
            "GET",
            &format!("{API}/drives/{DRIVE}/items/{DRIVE}!3/children"),
            vec![(200, json!({"value": [file_json("5", "deep.bin", 5)]}))],
        );

    let plan = inspect(&http, FOLDER_LINK).unwrap();
    assert!(matches!(plan.root_kind, CloudShareRootKind::Directory));
    let listing: Vec<(String, String)> = plan
        .entries
        .iter()
        .map(|entry| (entry.relative_directory.join("/"), entry.name.clone()))
        .collect();
    assert_eq!(
        listing,
        [
            ("".to_owned(), "a.txt".to_owned()),
            ("".to_owned(), "b.txt".to_owned()),
            ("sub".to_owned(), "deep.bin".to_owned()),
        ]
    );
    // The tracking parameter never reaches the share API.
    assert!(plan.provider_context["link"] == "https://1drv.ms/f/s!ZyXwVu_ts");
    assert_eq!(
        http.sent(
            "GET",
            &format!("{API}/drives/{DRIVE}/items/{DRIVE}!1/children")
        )
        .len(),
        2
    );
}

#[test]
fn paging_links_to_other_hosts_are_not_followed() {
    let http = root_route(
        ScriptedHttp::new(),
        FOLDER_LINK,
        200,
        folder_json("1", "root"),
    )
    .on(
        "GET",
        &format!("{API}/drives/{DRIVE}/items/{DRIVE}!1/children"),
        vec![(
            200,
            json!({"value": [], "@odata.nextLink": "https://evil.example/steal"}),
        )],
    );
    assert!(matches!(
        inspect(&http, FOLDER_LINK),
        Err(QuarkError::InvalidResponse)
    ));
    assert!(http.sent("GET", "https://evil.example").is_empty());
}

#[test]
fn unavailable_unsupported_and_busy_links_have_distinct_failures() {
    for (status, expected) in [
        (404, OnedriveFailure::LinkUnavailable),
        (403, OnedriveFailure::LinkUnavailable),
        (400, OnedriveFailure::LinkUnsupported),
        (429, OnedriveFailure::ServiceBusy),
        (503, OnedriveFailure::ServiceBusy),
    ] {
        let http = root_route(ScriptedHttp::new(), FILE_LINK, status, json!({"error": {}}));
        failure(inspect(&http, FILE_LINK), expected);
    }
}

#[test]
fn unexpected_anonymous_interface_answers_are_reported_as_a_changed_interface() {
    // Unexpected status on the share request.
    let http = root_route(ScriptedHttp::new(), FILE_LINK, 405, json!({}));
    failure(
        inspect(&http, FILE_LINK),
        OnedriveFailure::AnonymousInterfaceChanged,
    );
    // A 200 whose body is not an item.
    let http = root_route(ScriptedHttp::new(), FILE_LINK, 200, json!({"unexpected": 1}));
    failure(
        inspect(&http, FILE_LINK),
        OnedriveFailure::AnonymousInterfaceChanged,
    );
    // The token endpoint refusing, or answering without a token.
    let http = ScriptedHttp::new().on("POST", TOKEN_URL, vec![(403, json!({}))]);
    failure(
        inspect(&http, FILE_LINK),
        OnedriveFailure::AnonymousInterfaceChanged,
    );
    let http = ScriptedHttp::new().on("POST", TOKEN_URL, vec![(200, json!({"nope": 1}))]);
    failure(
        inspect(&http, FILE_LINK),
        OnedriveFailure::AnonymousInterfaceChanged,
    );
    // Throttling of the token endpoint stays a plain "busy".
    let http = ScriptedHttp::new().on("POST", TOKEN_URL, vec![(503, json!({}))]);
    failure(inspect(&http, FILE_LINK), OnedriveFailure::ServiceBusy);
}

#[test]
fn a_token_the_service_dropped_is_fetched_once_more() {
    let item = file_json("114", "model.pth", 1);
    let normalized = crate::cloud::onedrive::link::normalized_link(FILE_LINK).unwrap();
    let http = ScriptedHttp::new()
        .on(
            "POST",
            TOKEN_URL,
            vec![
                (200, json!({"token": "OLD"})),
                (200, json!({"token": "NEW"})),
            ],
        )
        .on(
            "GET",
            &format!("{API}/shares/{}/driveitem", share_id(&normalized)),
            vec![(401, json!({})), (200, item)],
        );
    inspect(&http, FILE_LINK).unwrap();
    let requests = http.sent("GET", API);
    assert_eq!(requests[0].headers["Authorization"], "Badger OLD");
    assert_eq!(requests[1].headers["Authorization"], "Badger NEW");
}

#[test]
fn tokens_and_signed_urls_never_appear_in_errors() {
    let http = root_route(ScriptedHttp::new(), FILE_LINK, 404, json!({"error": {}}));
    let error = inspect(&http, FILE_LINK).unwrap_err();
    for rendered in [error.to_string(), format!("{error:?}")] {
        assert!(!rendered.contains("TOKEN-1") && !rendered.contains("SECRET"));
    }
}

#[test]
fn a_tampered_plan_cannot_redirect_requests() {
    let item = file_json("114", "model.pth", 1);
    let http = root_route(ScriptedHttp::new(), FILE_LINK, 200, item);
    let mut plan = inspect(&http, FILE_LINK).unwrap();
    let sent_before = http.requests.lock().unwrap().len();
    plan.provider_context["drive"] = json!("../evil");
    assert!(matches!(
        resolve(&http, &plan, &plan.entries[0]),
        Err(QuarkError::InvalidResponse)
    ));
    assert_eq!(http.requests.lock().unwrap().len(), sent_before);
}

#[test]
fn wrong_provider_candidates_are_rejected() {
    let http = ScriptedHttp::new();
    let mut other = candidate(FILE_LINK);
    other.provider = CloudProvider::Mega;
    assert!(OnedriveShareAdapter::new(&http).inspect(&other).is_err());
    assert!(http.requests.lock().unwrap().is_empty());
}
