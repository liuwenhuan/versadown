use alldownloader_lib::{resolve_magnet_torrent, MagnetMetadataSettings};
use axum::{
    extract::Json,
    http::{header::USER_AGENT, HeaderMap, StatusCode},
    routing::{get, post},
    Router,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

const MAGNET: &str = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567";

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}")
}

#[tokio::test]
async fn resolves_metadata_service_result_to_a_torrent_for_local_aria2() {
    let app = Router::new()
        .route(
            "/api/v1/metadata",
            post(|Json(request): Json<Value>| async move {
                assert_eq!(request["magnet_uri"], MAGNET);
                Json(json!({
                    "info_hash": "0123456789abcdef0123456789abcdef01234567",
                    "name": "fixture",
                    "size": 13,
                    "files": [],
                    "download_url": "/download/0123456789abcdef0123456789abcdef01234567"
                }))
            }),
        )
        .route(
            "/download/0123456789abcdef0123456789abcdef01234567",
            get(|| async { (StatusCode::OK, b"torrent-bytes".to_vec()) }),
        );
    let base_url = serve(app).await;

    let result = resolve_magnet_torrent(
        &MagnetMetadataSettings {
            enabled: true,
            base_url,
        },
        MAGNET,
    )
    .await
    .unwrap();

    let resolved = result.expect("metadata service should return its cached torrent");
    assert_eq!(resolved.torrent, "dG9ycmVudC1ieXRlcw==");
    assert_eq!(resolved.title.as_deref(), Some("fixture"));
}

#[tokio::test]
async fn identifies_as_alldownloader_when_requesting_metadata_service() {
    let observed_user_agent = Arc::new(Mutex::new(None));
    let captured_user_agent = Arc::clone(&observed_user_agent);
    let app = Router::new()
        .route(
            "/api/v1/metadata",
            post(move |headers: HeaderMap| {
                let captured_user_agent = Arc::clone(&captured_user_agent);
                async move {
                    *captured_user_agent.lock().unwrap() = headers
                        .get(USER_AGENT)
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_owned);
                    Json(json!({
                        "download_url": "/download/0123456789abcdef0123456789abcdef01234567"
                    }))
                }
            }),
        )
        .route(
            "/download/0123456789abcdef0123456789abcdef01234567",
            get(|| async { (StatusCode::OK, b"torrent-bytes".to_vec()) }),
        );
    let base_url = serve(app).await;

    let result = resolve_magnet_torrent(
        &MagnetMetadataSettings {
            enabled: true,
            base_url,
        },
        MAGNET,
    )
    .await;

    assert!(result.is_ok());
    assert_eq!(
        observed_user_agent.lock().unwrap().as_deref(),
        Some("AllDownloader/0.1")
    );
}

#[tokio::test]
async fn returns_no_torrent_when_the_metadata_service_has_no_cached_download() {
    let app = Router::new().route(
        "/api/v1/metadata",
        post(|| async {
            Json(json!({
                "info_hash": "0123456789abcdef0123456789abcdef01234567",
                "name": "fixture",
                "size": 13,
                "files": []
            }))
        }),
    );
    let base_url = serve(app).await;

    let result = resolve_magnet_torrent(
        &MagnetMetadataSettings {
            enabled: true,
            base_url,
        },
        MAGNET,
    )
    .await
    .unwrap();

    assert_eq!(result, None);
}

#[tokio::test]
async fn rewrites_a_service_default_localhost_download_url_to_the_configured_server() {
    let app = Router::new()
        .route(
            "/api/v1/metadata",
            post(|| async {
                Json(json!({
                    "info_hash": "0123456789abcdef0123456789abcdef01234567",
                    "name": "fixture",
                    "size": 13,
                    "files": [],
                    "download_url": "http://localhost:8080/download/0123456789abcdef0123456789abcdef01234567"
                }))
            }),
        )
        .route(
            "/download/0123456789abcdef0123456789abcdef01234567",
            get(|| async { (StatusCode::OK, b"torrent-bytes".to_vec()) }),
        );
    let base_url = serve(app).await;

    let result = resolve_magnet_torrent(
        &MagnetMetadataSettings {
            enabled: true,
            base_url,
        },
        MAGNET,
    )
    .await
    .unwrap();

    let resolved = result.expect("rewritten download URL should return its torrent");
    assert_eq!(resolved.torrent, "dG9ycmVudC1ieXRlcw==");
    assert_eq!(resolved.title.as_deref(), Some("fixture"));
}
