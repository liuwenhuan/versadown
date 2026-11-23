//! Google Drive seen from outside the crate: registry contract, link routing,
//! and the credential boundary of signed-in downloads (the bearer token may
//! reach the local range gateway but never aria2).
use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::Response,
    routing::get,
    Router,
};
use std::sync::{Arc, Mutex};
use versadown_lib::cloud::range_gateway::RangeGateway;
use versadown_lib::cloud::{
    CloudError, CloudHttpRequest, CloudHttpResponse, CloudHttpTransport, CloudProvider,
    MemoryCredentialStore, ProviderRegistry,
};
use versadown_lib::extract_cloud_share_candidate;

const ID: &str = "1l_5RK28JRL19wpT22B-DY9We3TVXnnQQ";

#[test]
fn descriptor_is_a_read_only_account_provider_whose_public_links_need_no_account() {
    let registry = ProviderRegistry::global();
    let gdrive = registry.get(CloudProvider::Gdrive);
    assert!(gdrive.account);
    assert!(!gdrive.public_shares_need_session);
    // Browse and download only: the OAuth scope is drive.readonly.
    let capabilities = &gdrive.capabilities;
    assert!(capabilities.list && capabilities.download && capabilities.share_inspect);
    assert!(
        !capabilities.create_directory
            && !capabilities.rename
            && !capabilities.r#move
            && !capabilities.copy
            && !capabilities.trash
            && !capabilities.delete
            && !capabilities.restore
            && !capabilities.upload
    );
    assert!(!capabilities.share_create && !capabilities.share_list && !capabilities.share_cancel);
    assert!(!capabilities.share_transfer && !capabilities.recorded_transfer_resolve);
    // Sign-in is the browser OAuth flow, enabled only when the build carries
    // an OAuth client.
    assert!(gdrive.authorization.web);
    assert_eq!(
        gdrive.authorization_configured,
        versadown_lib::cloud::gdrive::oauth_client_configured()
    );
    assert!(!gdrive.authorization.password);
    let catalog = serde_json::to_value(registry.catalog()).unwrap();
    let entry = catalog
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["provider"] == "gdrive")
        .unwrap();
    assert_eq!(entry["account"], true);
    assert_eq!(entry["rootDirectoryId"], "root");
}

#[test]
fn every_google_link_shape_is_routed_to_the_provider_and_lookalikes_are_not() {
    let registry = ProviderRegistry::global();
    for url in [
        format!("https://drive.google.com/file/d/{ID}/view?usp=sharing"),
        format!("https://drive.google.com/open?id={ID}"),
        format!("https://drive.google.com/uc?export=download&id={ID}"),
        format!("https://drive.usercontent.google.com/download?id={ID}&export=download&confirm=t"),
        format!("https://docs.google.com/document/d/{ID}/edit"),
        format!("https://docs.google.com/spreadsheets/d/{ID}/edit#gid=0"),
        format!("https://drive.google.com/drive/folders/{ID}?usp=sharing"),
    ] {
        let parsed = url::Url::parse(&url).unwrap();
        assert_eq!(
            registry.for_share_url(&parsed).unwrap().provider,
            CloudProvider::Gdrive,
            "{url}"
        );
        let candidate = extract_cloud_share_candidate(&url, None)
            .unwrap_or_else(|error| panic!("{url}: {error}"))
            .unwrap();
        assert_eq!(candidate.provider, CloudProvider::Gdrive);
        // The query is the id: it must reach the adapter untouched.
        assert_eq!(candidate.url, url);
    }
    for url in [
        format!("https://drive.google.com.evil.example/file/d/{ID}/view"),
        format!("https://example.com/open?id={ID}"),
        format!("https://storage.googleapis.com/bucket/{ID}"),
    ] {
        assert_eq!(extract_cloud_share_candidate(&url, None), Ok(None), "{url}");
    }
}

#[test]
fn a_google_link_inside_pasted_text_is_found_and_unsupported_pages_are_refused() {
    let text = format!("资料在这里\nhttps://drive.google.com/file/d/{ID}/view?usp=sharing\n谢谢");
    let candidate = extract_cloud_share_candidate(&text, None)
        .unwrap()
        .unwrap();
    assert_eq!(candidate.provider, CloudProvider::Gdrive);
    // A form or the Drive home page is a Google link but nothing to download.
    for url in [
        "https://docs.google.com/forms/d/1l_5RK28JRL19wpT22B-DY9We3TVXnnQQ/viewform",
        "https://drive.google.com/drive/my-drive",
    ] {
        assert!(extract_cloud_share_candidate(url, None).is_err(), "{url}");
    }
}

#[test]
fn public_share_capabilities_inspect_and_resolve_but_never_transfer() {
    struct NoNetwork;
    impl CloudHttpTransport for NoNetwork {
        fn send(&self, _: CloudHttpRequest) -> Result<CloudHttpResponse, CloudError> {
            panic!("building share capabilities must not touch the network")
        }
    }
    let store = MemoryCredentialStore::default();
    let shares =
        ProviderRegistry::global().share_capabilities(CloudProvider::Gdrive, &store, &NoNetwork);
    assert!(shares.inspect.is_some() && shares.resolve.is_some());
    assert!(shares.transfer.is_none() && shares.recorded.is_none());
}

#[test]
fn personal_drive_operations_need_a_session_and_mutations_are_unsupported() {
    struct NoNetwork;
    impl CloudHttpTransport for NoNetwork {
        fn send(&self, _: CloudHttpRequest) -> Result<CloudHttpResponse, CloudError> {
            panic!("no session means no request")
        }
    }
    let store = MemoryCredentialStore::default();
    let mut driver = ProviderRegistry::global().driver(CloudProvider::Gdrive, &store, &NoNetwork);
    assert!(matches!(driver.account(), Err(CloudError::NotAuthorized)));
    assert!(matches!(
        driver.list_directory("root"),
        Err(CloudError::NotAuthorized)
    ));
    assert!(matches!(
        driver.create_directory("root", "x"),
        Err(CloudError::ProviderUnsupported)
    ));
    assert!(matches!(
        driver.trash(&["a".to_owned()]),
        Err(CloudError::ProviderUnsupported)
    ));
    // No stored refresh token: nothing to renew silently.
    assert!(!driver.auth().refresh_session().unwrap());
}

// ----------------------------------------------------------------------
// The bearer token and the local gateway

/// The `Authorization` and `Range` headers of each upstream request.
type SeenRequests = Arc<Mutex<Vec<(Option<String>, Option<String>)>>>;

#[derive(Clone)]
struct Upstream {
    seen: SeenRequests,
}

const PAYLOAD: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// Behaves like `files.get?alt=media`: bearer protected, ranged, 401 when the
/// token is not the current one.
async fn drive_media(State(upstream): State<Upstream>, headers: HeaderMap) -> Response {
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let range = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    upstream
        .seen
        .lock()
        .unwrap()
        .push((authorization.clone(), range.clone()));
    if authorization.as_deref() != Some("Bearer CURRENT-TOKEN") {
        return Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(Body::empty())
            .unwrap();
    }
    let (start, end) = range
        .as_deref()
        .and_then(|value| value.strip_prefix("bytes="))
        .and_then(|value| value.split_once('-'))
        .map(|(start, end)| {
            (
                start.parse::<usize>().unwrap(),
                end.parse::<usize>().unwrap().min(PAYLOAD.len() - 1),
            )
        })
        .expect("the gateway always asks for a range");
    Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{}", PAYLOAD.len()),
        )
        .header(header::CONTENT_LENGTH, end - start + 1)
        .body(Body::from(PAYLOAD[start..=end].to_vec()))
        .unwrap()
}

async fn serve_upstream() -> (String, Upstream) {
    let upstream = Upstream {
        seen: Arc::default(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/drive/v3/files/{id}", get(drive_media))
        .with_state(upstream.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}/drive/v3/files/{ID}?alt=media"), upstream)
}

#[tokio::test]
async fn the_bearer_token_reaches_the_upstream_through_the_gateway_but_not_the_aria2_options() {
    let (upstream_url, upstream) = serve_upstream().await;
    let gateway = RangeGateway::start().await.unwrap();
    let task = uuid::Uuid::new_v4();
    let registration = gateway
        .register(
            task,
            upstream_url,
            vec![("Authorization".into(), "Bearer CURRENT-TOKEN".into())],
            None,
        )
        .unwrap();
    // What aria2 is given is only this loopback address; the header stays here.
    assert!(registration.url().starts_with("http://127.0.0.1:"));
    assert!(!registration.url().contains("CURRENT-TOKEN"));

    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let response = client
        .get(registration.url())
        .header(header::RANGE, "bytes=4-9")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(&response.bytes().await.unwrap()[..], &PAYLOAD[4..=9]);
    let seen = upstream.seen.lock().unwrap();
    assert_eq!(seen[0].0.as_deref(), Some("Bearer CURRENT-TOKEN"));
}

#[tokio::test]
async fn an_expired_token_surfaces_as_a_refreshable_403_and_a_new_token_resumes_the_same_endpoint() {
    let (upstream_url, _) = serve_upstream().await;
    let gateway = RangeGateway::start().await.unwrap();
    let task = uuid::Uuid::new_v4();
    let stale = gateway
        .register(
            task,
            upstream_url.clone(),
            vec![("Authorization".into(), "Bearer EXPIRED-TOKEN".into())],
            None,
        )
        .unwrap();
    let endpoint = stale.url().to_owned();
    let mut stale = stale;
    stale.commit();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let rejected = client
        .get(&endpoint)
        .header(header::RANGE, "bytes=0-3")
        .send()
        .await
        .unwrap();
    // aria2 turns this into error 22, which re-resolves the download.
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);

    let mut renewed = gateway
        .register_refreshing(
            task,
            upstream_url,
            vec![("Authorization".into(), "Bearer CURRENT-TOKEN".into())],
            None,
        )
        .unwrap();
    assert_eq!(renewed.url(), endpoint, "the task keeps its local address");
    renewed.commit();
    let resumed = client
        .get(&endpoint)
        .header(header::RANGE, "bytes=10-12")
        .send()
        .await
        .unwrap();
    assert_eq!(resumed.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(&resumed.bytes().await.unwrap()[..], &PAYLOAD[10..=12]);
}
