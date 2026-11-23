use super::*;
use crate::cloud::gdrive::test_support::{rule, ScriptedGoogle};
use crate::cloud::MemoryCredentialStore;

const FILE_ID: &str = "1l_5RK28JRL19wpT22B-DY9We3TVXnnQQ";

fn oauth() -> OAuthClient {
    OAuthClient {
        id: "client".into(),
        secret: Some("secret".into()),
    }
}

fn session(expires_in: i64) -> GdriveSession {
    GdriveSession {
        access_token: "ACCESS-1".into(),
        refresh_token: "REFRESH-1".into(),
        expires_at: now() + expires_in,
        account_id: "perm-1".into(),
        display_name: "user@example.com".into(),
    }
}

fn signed_in(expires_in: i64) -> MemoryCredentialStore {
    let store = MemoryCredentialStore::default();
    session(expires_in).save(&store).unwrap();
    store
}

/// What AuthSupervisor hands to work: a private copy that must not renew.
struct Snapshot(MemoryCredentialStore);
impl CredentialStore for Snapshot {
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

fn refreshed(access: &str) -> String {
    format!(r#"{{"access_token":"{access}","expires_in":3600,"token_type":"Bearer"}}"#)
}

fn authorization_of(request: &CloudHttpRequest) -> &str {
    request.headers.get("Authorization").unwrap()
}

fn query_of(request: &CloudHttpRequest) -> std::collections::HashMap<String, String> {
    url::Url::parse(&request.url)
        .unwrap()
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

#[test]
fn account_identity_is_the_permission_id_and_shows_the_email() {
    let http = ScriptedGoogle::new(vec![rule(
        "/about",
        vec![(
            200,
            r#"{"user":{"permissionId":"1234567890","displayName":"Ada","emailAddress":"ada@example.com"}}"#,
        )],
    )]);
    let account = GdriveClient::new(&http, &signed_in(3600)).account().unwrap();
    assert_eq!(account.id, "1234567890");
    assert_eq!(account.display_name, "ada@example.com");
    assert_eq!(account.provider, CloudProvider::Gdrive);
    let request = &http.requests.lock().unwrap()[0];
    assert_eq!(authorization_of(request), "Bearer ACCESS-1");
    assert!(request.url.contains("fields=user"));
}

#[test]
fn storage_capacity_distinguishes_limited_and_unlimited_drives() {
    let limited = ScriptedGoogle::new(vec![rule(
        "/about",
        vec![(200, r#"{"storageQuota":{"limit":"16106127360","usage":"5368709120"}}"#)],
    )]);
    assert_eq!(
        GdriveClient::new(&limited, &signed_in(3600)).storage_capacity().unwrap(),
        CloudStorageCapacity::Limited {
            total_bytes: 16_106_127_360,
            used_bytes: 5_368_709_120
        }
    );
    let unlimited = ScriptedGoogle::new(vec![rule("/about", vec![(200, r#"{"storageQuota":{"usage":"42"}}"#)])]);
    assert_eq!(
        GdriveClient::new(&unlimited, &signed_in(3600)).storage_capacity().unwrap(),
        CloudStorageCapacity::Unlimited { used_bytes: 42 }
    );
}

#[test]
fn listing_follows_every_page_and_maps_files_folders_and_shortcuts() {
    let page_one = r#"{"nextPageToken":"PAGE-2","files":[
        {"id":"FOLDERID1","name":"Docs","mimeType":"application/vnd.google-apps.folder","modifiedTime":"2024-01-02T03:04:05.000Z"},
        {"id":"FILEID001","name":"a.txt","mimeType":"text/plain","size":"12","modifiedTime":"2024-01-03T00:00:00.000Z"}]}"#;
    let page_two = r#"{"files":[
        {"id":"SHORTCUT1","name":"link to folder","mimeType":"application/vnd.google-apps.shortcut","shortcutDetails":{"targetId":"TARGETDIR1","targetMimeType":"application/vnd.google-apps.folder"}},
        {"id":"SHORTCUT2","name":"link to file","mimeType":"application/vnd.google-apps.shortcut","shortcutDetails":{"targetId":"TARGETFIL1","targetMimeType":"video/mp4"}},
        {"id":"NONAME001","mimeType":"text/plain"}]}"#;
    let http = ScriptedGoogle::new(vec![rule("/files?", vec![(200, page_one), (200, page_two)])]);
    let directory = GdriveClient::new(&http, &signed_in(3600)).list("root").unwrap();
    assert_eq!(directory.id, "root");
    let described: Vec<_> = directory
        .items
        .iter()
        .map(|item| (item.id.as_str(), item.name.as_str(), item.kind, item.size))
        .collect();
    assert_eq!(
        described,
        vec![
            ("FOLDERID1", "Docs", CloudFileKind::Folder, None),
            ("FILEID001", "a.txt", CloudFileKind::File, Some(12)),
            ("TARGETDIR1", "link to folder", CloudFileKind::Folder, None),
            ("TARGETFIL1", "link to file", CloudFileKind::File, None),
        ]
    );
    assert_eq!(directory.items[0].modified_at.as_deref(), Some("2024-01-02T03:04:05.000Z"));
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let first = query_of(&requests[0]);
    assert_eq!(first["q"], "'root' in parents and trashed = false");
    assert_eq!(first["pageSize"], "1000");
    assert_eq!(first["supportsAllDrives"], "true");
    assert!(!first.contains_key("pageToken") && !first.contains_key("corpora"));
    assert_eq!(query_of(&requests[1])["pageToken"], "PAGE-2");
}

#[test]
fn folders_of_shared_drives_are_searched_across_all_drives() {
    let http = ScriptedGoogle::new(vec![rule("/files?", vec![(200, r#"{"files":[]}"#)])]);
    GdriveClient::new(&http, &signed_in(3600)).list("0AAbcdEFGhijkUk9PVA").unwrap();
    let query = query_of(&http.requests.lock().unwrap()[0]);
    assert_eq!(query["corpora"], "allDrives");
    assert_eq!(query["includeItemsFromAllDrives"], "true");
    assert_eq!(query["q"], "'0AAbcdEFGhijkUk9PVA' in parents and trashed = false");
}

#[test]
fn shared_with_me_and_shared_drives_are_virtual_folders() {
    let http = ScriptedGoogle::new(vec![
        rule("/drives?", vec![(200, r#"{"drives":[{"id":"0ADRIVEID01","name":"Team"}]}"#)]),
        rule("/files?", vec![(200, r#"{"files":[{"id":"SHAREDFIL1","name":"x.bin","mimeType":"application/zip","size":"7"}]}"#)]),
    ]);
    let store = signed_in(3600);
    let client = GdriveClient::new(&http, &store);
    let drives = client.list(SHARED_DRIVES_ID).unwrap();
    assert_eq!(drives.items.len(), 1);
    assert_eq!(drives.items[0].kind, CloudFileKind::Folder);
    assert_eq!(drives.items[0].id, "0ADRIVEID01");
    let shared = client.list(SHARED_WITH_ME_ID).unwrap();
    assert_eq!(shared.items[0].parent_id, SHARED_WITH_ME_ID);
    let requests = http.requests_to("/files?");
    assert_eq!(
        query_of(&requests[0])["q"],
        "sharedWithMe = true and trashed = false"
    );
    // Never trust a folder id into the query.
    assert!(matches!(
        client.list("x' or name contains 'secret"),
        Err(QuarkError::InvalidInput(_))
    ));
}

#[test]
fn an_expiring_token_is_renewed_before_the_request_and_kept_in_the_store() {
    let http = ScriptedGoogle::new(vec![
        rule("oauth2.googleapis.com/token", vec![(200, &refreshed("ACCESS-2"))]),
        rule("/files?", vec![(200, r#"{"files":[]}"#)]),
    ]);
    let store = signed_in(10);
    GdriveClient::new(&http, &store)
        .with_oauth_client(oauth())
        .list("root")
        .unwrap();
    assert_eq!(authorization_of(&http.requests_to("/files?")[0]), "Bearer ACCESS-2");
    let stored = GdriveSession::load(&store).unwrap();
    assert_eq!(stored.access_token, "ACCESS-2");
    // Google sent no new refresh token: the old one stays.
    assert_eq!(stored.refresh_token, "REFRESH-1");
    assert!(stored.expires_at > now() + 3000);
}

#[test]
fn a_rejected_token_is_renewed_once_and_the_request_is_retried() {
    let http = ScriptedGoogle::new(vec![
        rule("oauth2.googleapis.com/token", vec![(200, &refreshed("ACCESS-2"))]),
        rule(
            "/about",
            vec![
                (401, r#"{"error":{"code":401,"status":"UNAUTHENTICATED"}}"#),
                (200, r#"{"user":{"permissionId":"p","emailAddress":"a@b.c"}}"#),
            ],
        ),
    ]);
    let store = signed_in(3600);
    let account = GdriveClient::new(&http, &store)
        .with_oauth_client(oauth())
        .account()
        .unwrap();
    assert_eq!(account.id, "p");
    let attempts = http.requests_to("/about");
    assert_eq!(authorization_of(&attempts[0]), "Bearer ACCESS-1");
    assert_eq!(authorization_of(&attempts[1]), "Bearer ACCESS-2");
    assert_eq!(http.requests_to("oauth2.googleapis.com/token").len(), 1);
}

#[test]
fn a_second_rejection_after_renewal_is_reported_instead_of_looping() {
    let http = ScriptedGoogle::new(vec![
        rule("oauth2.googleapis.com/token", vec![(200, &refreshed("ACCESS-2"))]),
        rule("/about", vec![(401, "{}")]),
    ]);
    let result = GdriveClient::new(&http, &signed_in(3600))
        .with_oauth_client(oauth())
        .account();
    assert!(matches!(result, Err(QuarkError::AuthorizationExpired)));
    assert_eq!(http.requests_to("/about").len(), 2);
}

#[test]
fn inside_a_supervisor_snapshot_a_rejection_is_handed_upward_without_renewing() {
    let http = ScriptedGoogle::new(vec![rule("/about", vec![(401, "{}")])]);
    let store = Snapshot(signed_in(10));
    let result = GdriveClient::new(&http, &store).account();
    assert!(matches!(result, Err(QuarkError::AuthorizationExpired)));
    assert_eq!(http.requests.lock().unwrap().len(), 1, "no token request");
}

#[test]
fn a_revoked_refresh_token_requires_a_new_authorization() {
    let http = ScriptedGoogle::new(vec![rule(
        "oauth2.googleapis.com/token",
        vec![(400, r#"{"error":"invalid_grant"}"#)],
    )]);
    let store = signed_in(3600);
    let result = GdriveClient::new(&http, &store)
        .with_oauth_client(oauth())
        .refresh_session();
    assert!(matches!(result, Err(QuarkError::AuthorizationExpired)));
    // The unusable session stays until the user re-authorizes or disconnects.
    assert!(GdriveSession::load(&store).is_ok());
}

#[test]
fn refresh_session_stores_the_renewed_token_for_the_supervisor() {
    let http = ScriptedGoogle::new(vec![rule(
        "oauth2.googleapis.com/token",
        vec![(200, r#"{"access_token":"ACCESS-9","refresh_token":"REFRESH-9","expires_in":1800}"#)],
    )]);
    let store = signed_in(-5);
    GdriveClient::new(&http, &store)
        .with_oauth_client(oauth())
        .refresh_session()
        .unwrap();
    let stored = GdriveSession::load(&store).unwrap();
    assert_eq!((stored.access_token.as_str(), stored.refresh_token.as_str()), ("ACCESS-9", "REFRESH-9"));
    assert_eq!(stored.account_id, "perm-1");
    assert!(!format!("{stored:?}").contains("ACCESS-9"));
}

#[test]
fn download_hands_the_bearer_token_over_as_a_header_only() {
    let http = ScriptedGoogle::new(vec![rule(
        "/files/",
        vec![(200, r#"{"id":"1l_5RK28JRL19wpT22B-DY9We3TVXnnQQ","name":"movie.mkv","mimeType":"video/x-matroska"}"#)],
    )]);
    let resolved = GdriveClient::new(&http, &signed_in(3600)).download(FILE_ID).unwrap();
    assert_eq!(resolved.name, "movie.mkv");
    assert_eq!(
        resolved.url,
        format!("https://www.googleapis.com/drive/v3/files/{FILE_ID}?alt=media&supportsAllDrives=true")
    );
    assert!(!resolved.url.contains("ACCESS-1"));
    assert_eq!(
        resolved.headers,
        vec![("Authorization".to_owned(), "Bearer ACCESS-1".to_owned())]
    );
    assert!(resolved.transform.is_none());
}

#[test]
fn native_documents_and_folders_are_not_downloadable_through_the_api() {
    let native = ScriptedGoogle::new(vec![rule(
        "/files/",
        vec![(200, r#"{"id":"x","name":"Notes","mimeType":"application/vnd.google-apps.document"}"#)],
    )]);
    assert!(matches!(
        GdriveClient::new(&native, &signed_in(3600)).download(FILE_ID),
        Err(QuarkError::Gdrive(GdriveFailure::NativeNotDownloadable))
    ));
    let folder = ScriptedGoogle::new(vec![rule(
        "/files/",
        vec![(200, r#"{"id":"x","name":"Dir","mimeType":"application/vnd.google-apps.folder"}"#)],
    )]);
    assert!(matches!(
        GdriveClient::new(&folder, &signed_in(3600)).download(FILE_ID),
        Err(QuarkError::InvalidInput(_))
    ));
    assert!(matches!(
        GdriveClient::new(&folder, &signed_in(3600)).download("../etc/passwd"),
        Err(QuarkError::InvalidInput(_))
    ));
}

#[test]
fn api_errors_have_their_own_user_facing_failures() {
    let case = |status: u16, body: &str| {
        let http = ScriptedGoogle::new(vec![rule("/files/", vec![(status, body)])]);
        GdriveClient::new(&http, &signed_in(3600)).download(FILE_ID).err().expect("an error")
    };
    assert!(matches!(
        case(403, r#"{"error":{"errors":[{"reason":"downloadQuotaExceeded"}]}}"#),
        QuarkError::Gdrive(GdriveFailure::QuotaExceeded)
    ));
    assert!(matches!(
        case(403, r#"{"error":{"errors":[{"reason":"userRateLimitExceeded"}]}}"#),
        QuarkError::Gdrive(GdriveFailure::RateLimited)
    ));
    assert!(matches!(case(429, "{}"), QuarkError::Gdrive(GdriveFailure::RateLimited)));
    assert!(matches!(
        case(403, r#"{"error":{"errors":[{"reason":"insufficientFilePermissions"}]}}"#),
        QuarkError::Gdrive(GdriveFailure::AccessDenied)
    ));
    assert!(matches!(case(404, "{}"), QuarkError::SourceFileMissing));
    match case(500, "<html>boom ACCESS-1</html>") {
        QuarkError::Provider(message) => assert!(!message.contains("ACCESS-1")),
        other => panic!("{other}"),
    }
}

#[test]
fn without_a_stored_session_the_account_is_not_authorized() {
    let http = ScriptedGoogle::new(vec![]);
    assert!(matches!(
        GdriveClient::new(&http, &MemoryCredentialStore::default()).account(),
        Err(QuarkError::NotAuthorized)
    ));
}
