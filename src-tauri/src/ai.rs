use std::time::Duration;

use serde::Deserialize;
use url::Url;

pub const API_KEY_SECRET_NAME: &str = "ai-api-key";

#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<ModelRecord>,
}

#[derive(Deserialize)]
struct ModelRecord {
    id: String,
}

pub fn normalize_base_url(value: &str) -> Result<String, String> {
    let mut url = Url::parse(value.trim()).map_err(|_| "AI 服务地址无效".to_string())?;
    if !matches!(url.scheme(), "https" | "http") || url.host_str().is_none() {
        return Err("AI 服务地址必须是 HTTP 或 HTTPS 地址".into());
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("AI 服务地址不能包含账号、查询参数或片段".into());
    }
    let path = url.path().trim_end_matches('/').to_string();
    url.set_path(&path);
    Ok(url.to_string().trim_end_matches('/').to_string())
}

pub fn models_endpoint(base_url: &str) -> Result<String, String> {
    Ok(format!("{}/models", normalize_base_url(base_url)?))
}

pub async fn fetch_models(base_url: &str, api_key: &str) -> Result<Vec<String>, String> {
    if api_key.trim().is_empty() {
        return Err("请先输入或保存 AI API Key".into());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .build()
        .map_err(|_| "无法创建 AI 服务连接".to_string())?;
    let response = client
        .get(models_endpoint(base_url)?)
        .bearer_auth(api_key)
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                format!("请求 AI 服务超时：{error}")
            } else if error.is_connect() {
                format!("无法连接 AI 服务：{error}")
            } else {
                format!("请求 AI 服务失败：{error}")
            }
        })?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_default()
            .trim()
            .chars()
            .take(512)
            .collect::<String>();
        return Err(if body.is_empty() {
            format!("AI 服务返回 HTTP {}", status.as_u16())
        } else {
            format!("AI 服务返回 HTTP {}：{body}", status.as_u16())
        });
    }
    let mut models = response
        .json::<ModelsResponse>()
        .await
        .map_err(|error| format!("AI 服务返回的模型列表无效：{error}"))?
        .data
        .into_iter()
        .map(|model| model.id.trim().to_string())
        .filter(|model| !model.is_empty())
        .collect::<Vec<_>>();
    models.sort_unstable();
    models.dedup();
    if models.is_empty() {
        return Err("AI 服务未返回可用模型".into());
    }
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_endpoint_keeps_openai_compatible_v1_path() {
        assert_eq!(
            models_endpoint("https://gateway.example.test/v1/").unwrap(),
            "https://gateway.example.test/v1/models"
        );
    }

    #[test]
    fn base_url_rejects_credentials_and_query_parameters() {
        assert!(normalize_base_url("https://key@gateway.example.test/v1").is_err());
        assert!(normalize_base_url("https://gateway.example.test/v1?token=secret").is_err());
    }

    #[tokio::test]
    async fn fetch_models_uses_the_compatible_models_route_and_bearer_key() {
        use axum::{
            extract::State,
            http::{HeaderMap, StatusCode},
            routing::get,
            Json, Router,
        };
        use serde_json::json;
        use std::{
            future::IntoFuture,
            sync::{Arc, Mutex},
        };

        async fn models(
            State(authorization): State<Arc<Mutex<Option<String>>>>,
            headers: HeaderMap,
        ) -> Result<Json<serde_json::Value>, StatusCode> {
            *authorization.lock().unwrap() = headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            Ok(Json(
                json!({"data":[{"id":"gpt-5-mini"},{"id":"gpt-4.1-mini"},{"id":"gpt-5-mini"}]}),
            ))
        }

        let authorization = Arc::new(Mutex::new(None));
        let app = Router::new()
            .route("/v1/models", get(models))
            .with_state(Arc::clone(&authorization));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(axum::serve(listener, app).into_future());

        let models = fetch_models(&format!("http://{address}/v1"), "local-test-key")
            .await
            .unwrap();

        assert_eq!(models, vec!["gpt-4.1-mini", "gpt-5-mini"]);
        assert_eq!(
            authorization.lock().unwrap().as_deref(),
            Some("Bearer local-test-key")
        );
        server.abort();
    }

    #[tokio::test]
    async fn fetch_models_keeps_the_server_error_detail() {
        use axum::{http::StatusCode, routing::get, Router};
        use std::future::IntoFuture;

        async fn models() -> (StatusCode, &'static str) {
            (StatusCode::UNAUTHORIZED, "invalid api key")
        }

        let app = Router::new().route("/v1/models", get(models));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(axum::serve(listener, app).into_future());

        let error = fetch_models(&format!("http://{address}/v1"), "bad-key")
            .await
            .unwrap_err();

        assert_eq!(error, "AI 服务返回 HTTP 401：invalid api key");
        server.abort();
    }
}
