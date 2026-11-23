use super::*;
use crate::cloud::onedrive::session::now_secs;
use crate::cloud::onedrive::test_support::ScriptedHttp;
use crate::cloud::MemoryCredentialStore;
use serde_json::{json, Value};

const GRAPH: &str = "https://graph.microsoft.com/v1.0";
const TOKEN_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
const CLIENT_ID: &str = "11111111-2222-3333-4444-555555555555";

fn config() -> OAuthConfig {
    OAuthConfig::with_client_id(CLIENT_ID)
}

fn store_with(access: &str, refresh: &str, expires_in: i64) -> MemoryCredentialStore {
    let store = MemoryCredentialStore::default();
    OnedriveSession {
        access_token: access.into(),
        refresh_token: refresh.into(),
        expires_at: now_secs() + expires_in,
    }
    .save(&store)
    .unwrap();
    store
}

fn stored(store: &dyn CredentialStore) -> OnedriveSession {
    OnedriveSession::load(store).unwrap().unwrap()
}

fn token_reply(access: &str, refresh: &str) -> (u16, Value) {
    (
        200,
        json!({"access_token": access, "refresh_token": refresh, "expires_in": 3600}),
    )
}

fn client<'a>(http: &'a ScriptedHttp, store: &'a dyn CredentialStore) -> OnedriveClient<'a> {
    OnedriveClient::with_config(http, store, config())
}

fn item(id: &str, name: &str, folder: bool, size: u64) -> Value {
    if folder {
        json!({"id": id, "name": name, "size": size, "folder": {"childCount": 1},
               "lastModifiedDateTime": "2026-01-02T03:04:05Z"})
    } else {
        json!({"id": id, "name": name, "size": size, "file": {},
               "lastModifiedDateTime": "2026-01-02T03:04:05Z"})
    }
}

/// A store as the authorization supervisor hands out: it renews nothing itself.
struct SnapshotStore(MemoryCredentialStore);
impl CredentialStore for SnapshotStore {
    fn automatic_session_recovery(&self) -> bool {
        false
    }
    fn save(&self, key: &str, value: &[u8]) -> Result<(), QuarkError> {
        self.0.save(key, value)
    }
    fn load(&self, key: &str) -> Result<Option<Vec<u8>>, QuarkError> {
        self.0.load(key)
    }
    fn remove(&self, key: &str) -> Result<(), QuarkError> {
        self.0.remove(key)
    }
}

#[test]
fn listing_follows_next_links_and_maps_kinds() {
    let http = ScriptedHttp::new().on(
        "GET",
        &format!("{GRAPH}/me/drive/root/children"),
        vec![
            (
                200,
                json!({"value": [item("A!1", "Docs", true, 10), item("A!2", "a.txt", false, 5)],
                       "@odata.nextLink": format!("{GRAPH}/me/drive/root/children?$skiptoken=P2")}),
            ),
            (
                200,
                json!({"value": [item("A!3", "b.txt", false, 7),
                                 {"id": "A!4", "name": "notebook", "package": {}}]}),
            ),
        ],
    );
    let store = store_with("AT", "RT", 3600);
    let directory = client(&http, &store).list("root").unwrap();

    assert_eq!(directory.id, "root");
    let rows: Vec<_> = directory
        .items
        .iter()
        .map(|file| (file.name.as_str(), file.kind, file.size))
        .collect();
    assert_eq!(
        rows,
        [
            ("Docs", CloudFileKind::Folder, None),
            ("a.txt", CloudFileKind::File, Some(5)),
            ("b.txt", CloudFileKind::File, Some(7)),
        ]
    );
    assert_eq!(directory.items[1].parent_id, "root");
    assert_eq!(
        directory.items[1].modified_at.as_deref(),
        Some("2026-01-02T03:04:05Z")
    );
    let requests = http.sent("GET", GRAPH);
    assert_eq!(requests.len(), 2);
    assert!(requests
        .iter()
        .all(|request| request.headers["Authorization"] == "Bearer AT"));
}

#[test]
fn subfolders_are_listed_by_item_id_and_bad_ids_are_refused() {
    let http = ScriptedHttp::new().on(
        "GET",
        &format!("{GRAPH}/me/drive/items/A!1/children"),
        vec![(200, json!({"value": []}))],
    );
    let store = store_with("AT", "RT", 3600);
    assert!(client(&http, &store).list("A!1").unwrap().items.is_empty());
    assert!(client(&http, &store).list("../me").is_err());
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}

#[test]
fn next_links_to_other_hosts_never_receive_the_token() {
    let http = ScriptedHttp::new().on(
        "GET",
        &format!("{GRAPH}/me/drive/root/children"),
        vec![(
            200,
            json!({"value": [], "@odata.nextLink": "https://evil.example/page2"}),
        )],
    );
    let store = store_with("AT", "RT", 3600);
    assert!(matches!(
        client(&http, &store).list("root"),
        Err(QuarkError::InvalidResponse)
    ));
    assert!(http.sent("GET", "https://evil.example").is_empty());
}

#[test]
fn a_401_renews_the_token_once_and_retries() {
    let http = ScriptedHttp::new()
        .on(
            "GET",
            &format!("{GRAPH}/me/drive/root/children"),
            vec![(401, json!({})), (200, json!({"value": []}))],
        )
        .on("POST", TOKEN_URL, vec![token_reply("AT2", "RT2")]);
    let store = store_with("AT1", "RT1", 3600);
    client(&http, &store).list("root").unwrap();

    let requests = http.sent("GET", GRAPH);
    assert_eq!(requests[0].headers["Authorization"], "Bearer AT1");
    assert_eq!(requests[1].headers["Authorization"], "Bearer AT2");
    // The rotated pair replaced the old one.
    let session = stored(&store);
    assert_eq!(
        (
            session.access_token.as_str(),
            session.refresh_token.as_str()
        ),
        ("AT2", "RT2")
    );
    let form = http.sent("POST", TOKEN_URL)[0].body.clone().unwrap();
    assert!(form.contains("grant_type=refresh_token") && form.contains("refresh_token=RT1"));
    assert!(form.contains(&format!("client_id={CLIENT_ID}")));
}

#[test]
fn a_second_401_is_reported_instead_of_looping() {
    let http = ScriptedHttp::new()
        .on("GET", &format!("{GRAPH}/me/drive"), vec![(401, json!({}))])
        .on("POST", TOKEN_URL, vec![token_reply("AT2", "RT2")]);
    let store = store_with("AT1", "RT1", 3600);
    assert!(matches!(
        client(&http, &store).account(),
        Err(QuarkError::AuthorizationExpired)
    ));
    assert_eq!(http.sent("POST", TOKEN_URL).len(), 1);
}

#[test]
fn an_expired_access_token_is_renewed_before_the_request() {
    let http = ScriptedHttp::new()
        .on(
            "GET",
            &format!("{GRAPH}/me/drive"),
            vec![(200, json!({"id": "D1"}))],
        )
        .on("POST", TOKEN_URL, vec![token_reply("AT2", "RT2")]);
    let store = store_with("AT1", "RT1", 10);
    client(&http, &store).account().unwrap();
    assert_eq!(
        http.sent("GET", GRAPH)[0].headers["Authorization"],
        "Bearer AT2"
    );
}

#[test]
fn snapshot_stores_leave_renewal_to_the_supervisor() {
    let http = ScriptedHttp::new().on("GET", &format!("{GRAPH}/me/drive"), vec![(401, json!({}))]);
    let store = SnapshotStore(store_with("AT1", "RT1", 3600));
    assert!(matches!(
        client(&http, &store).account(),
        Err(QuarkError::AuthorizationExpired)
    ));
    // No token request: the supervisor asks `refresh_session` explicitly.
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}

#[test]
fn refresh_session_rotates_the_pair_and_reports_missing_sessions() {
    let http = ScriptedHttp::new().on("POST", TOKEN_URL, vec![token_reply("AT2", "RT2")]);
    let store = store_with("AT1", "RT1", 3600);
    assert!(client(&http, &store).refresh_session().unwrap());
    assert_eq!(stored(&store).refresh_token, "RT2");
    assert!(stored(&store).expires_at > now_secs() + 3000);

    let empty = MemoryCredentialStore::default();
    assert!(!client(&http, &empty).refresh_session().unwrap());
}

#[test]
fn a_revoked_grant_surfaces_as_expired_authorization() {
    let http = ScriptedHttp::new().on(
        "POST",
        TOKEN_URL,
        vec![(
            400,
            json!({"error": "invalid_grant", "error_description": "AADSTS70008"}),
        )],
    );
    let store = store_with("AT1", "RT1", 3600);
    let error = client(&http, &store).refresh_session().unwrap_err();
    assert!(matches!(error, QuarkError::AuthorizationExpired));
    // The stored session is left for the supervisor to deal with.
    assert_eq!(stored(&store).refresh_token, "RT1");
}

#[test]
fn renewal_without_an_application_id_is_a_clear_failure() {
    let http = ScriptedHttp::new();
    let store = store_with("AT1", "RT1", 3600);
    let client = OnedriveClient::with_config(&http, &store, OAuthConfig::with_client_id(""));
    assert!(matches!(
        client.refresh_session(),
        Err(QuarkError::Onedrive(OnedriveFailure::ClientIdMissing))
    ));
    assert!(http.requests.lock().unwrap().is_empty());
}

#[test]
fn download_returns_a_fresh_signed_url_and_never_a_folder() {
    let http = ScriptedHttp::new()
        .on(
            "GET",
            &format!("{GRAPH}/me/drive/items/A!2"),
            vec![(
                200,
                json!({"id": "A!2", "name": "a.txt", "file": {},
                       "@microsoft.graph.downloadUrl": "https://dl.example/a?tempauth=S"}),
            )],
        )
        .on(
            "GET",
            &format!("{GRAPH}/me/drive/items/A!1"),
            vec![(200, json!({"id": "A!1", "name": "Docs", "folder": {}}))],
        )
        .on(
            "GET",
            &format!("{GRAPH}/me/drive/items/A!9"),
            vec![(404, json!({}))],
        );
    let store = store_with("AT", "RT", 3600);
    let client = client(&http, &store);
    assert_eq!(
        client.download("A!2").unwrap(),
        (
            "https://dl.example/a?tempauth=S".to_owned(),
            "a.txt".to_owned()
        )
    );
    assert!(matches!(
        client.download("A!1"),
        Err(QuarkError::InvalidInput(_))
    ));
    assert!(matches!(
        client.download("A!9"),
        Err(QuarkError::SourceFileMissing)
    ));
}

#[test]
fn account_and_capacity_come_from_the_drive_resource() {
    let drive = json!({
        "id": "D1", "driveType": "personal",
        "owner": {"user": {"id": "U-42", "displayName": "Ada"}},
        "quota": {"total": 5_368_709_120u64, "used": 1_048_576u64},
    });
    let http = ScriptedHttp::new().on("GET", &format!("{GRAPH}/me/drive"), vec![(200, drive)]);
    let store = store_with("AT", "RT", 3600);
    let account = client(&http, &store).account().unwrap();
    assert_eq!(
        (
            account.provider,
            account.id.as_str(),
            account.display_name.as_str()
        ),
        (CloudProvider::Onedrive, "U-42", "Ada")
    );
    assert_eq!(
        client(&http, &store).storage_capacity().unwrap(),
        CloudStorageCapacity::Limited {
            total_bytes: 5_368_709_120,
            used_bytes: 1_048_576
        }
    );
}

#[test]
fn without_a_session_nothing_is_sent() {
    let http = ScriptedHttp::new();
    let store = MemoryCredentialStore::default();
    assert!(matches!(
        client(&http, &store).account(),
        Err(QuarkError::NotAuthorized)
    ));
    assert!(http.requests.lock().unwrap().is_empty());
}

#[test]
fn throttling_and_server_errors_are_reported_as_busy() {
    for status in [429, 503] {
        let http = ScriptedHttp::new().on("GET", GRAPH, vec![(status, json!({}))]);
        let store = store_with("AT", "RT", 3600);
        assert!(matches!(
            client(&http, &store).account(),
            Err(QuarkError::Onedrive(OnedriveFailure::ServiceBusy))
        ));
    }
}

#[test]
fn sessions_never_print_their_tokens() {
    let session = stored(&store_with("SECRET-AT", "SECRET-RT", 60));
    assert!(!format!("{session:?}").contains("SECRET"));
}
