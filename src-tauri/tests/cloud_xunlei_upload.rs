#[path = "support/upload.rs"]
mod upload_support;
use versadown_lib::cloud::{
    xunlei::{XunleiClient, XunleiClientProfile, XunleiSession},
    CloudHttpRequest, CloudHttpResponse, CloudHttpTransport, CloudUploadTransport, QuarkError,
    UploadHttpRequest, UploadHttpResponse,
};
use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha1::Sha1;
use std::{collections::VecDeque, io::Write, sync::Mutex};

struct UploadFixture {
    response: Value,
    metadata: Mutex<VecDeque<Value>>,
    requests: Mutex<Vec<CloudHttpRequest>>,
    uploads: Mutex<Vec<UploadHttpRequest>>,
}

impl UploadFixture {
    fn new(response: Value) -> Self {
        let mut ready_file = response["file"].clone();
        ready_file["phase"] = json!("PHASE_TYPE_COMPLETE");
        Self {
            response,
            metadata: Mutex::new(VecDeque::from([ready_file])),
            requests: Mutex::new(Vec::new()),
            uploads: Mutex::new(Vec::new()),
        }
    }
}

impl CloudHttpTransport for UploadFixture {
    fn send(&self, request: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        let metadata_request = request.method == "GET";
        self.requests.lock().unwrap().push(request);
        let body = if metadata_request {
            self.metadata
                .lock()
                .unwrap()
                .pop_front()
                .ok_or(QuarkError::InvalidResponse)?
        } else {
            self.response.clone()
        };
        Ok(CloudHttpResponse {
            status: 200,
            body: body.to_string(),
            ..Default::default()
        })
    }
}

impl CloudUploadTransport for UploadFixture {
    fn send_upload(&self, request: UploadHttpRequest) -> Result<UploadHttpResponse, QuarkError> {
        let request = upload_support::consume(request)?;
        let response = match (request.method, request.url.contains("uploads")) {
            ("POST", true) => UploadHttpResponse { status: 200, headers: Default::default(), body: "<InitiateMultipartUploadResult><UploadId>upload-1</UploadId></InitiateMultipartUploadResult>".into() },
            ("PUT", _) => UploadHttpResponse { status: 200, headers: [("ETag".into(), "\"part-etag\"".into())].into(), body: String::new() },
            _ => UploadHttpResponse { status: 200, headers: Default::default(), body: "<CompleteMultipartUploadResult/>".into() },
        };
        self.uploads.lock().unwrap().push(request);
        Ok(response)
    }
}

fn session() -> XunleiSession {
    let mut session = XunleiSession::with_access(
        "synthetic-access",
        "synthetic-refresh",
        XunleiClientProfile {
            client_id: "Xqp0kJBXWhwaTpB6".into(),
            client_secret: String::new(),
            device_id: "synthetic-device".into(),
            client_version: "1.0.0".into(),
            package_name: "pan.xunlei.com".into(),
            user_agent: "Synthetic Browser".into(),
        },
    );
    session.user_id = "synthetic-user".into();
    session
}

fn complete_file() -> Value {
    json!({"file":{"id":"uploaded-file","parent_id":"parent","name":"uploaded.bin","kind":"drive#file","size":12,"phase":"PHASE_TYPE_COMPLETE"},"upload_type":"UPLOAD_TYPE_UNKNOWN"})
}

fn resumable(provider: &str, endpoint: &str) -> Value {
    json!({"file":{"id":"uploaded-file","parent_id":"parent","name":"uploaded.bin","kind":"drive#file","size":12,"phase":"PHASE_TYPE_RUNNING"},"upload_type":"UPLOAD_TYPE_RESUMABLE","resumable":{"provider":provider,"kind":"drive#resumable","params":{"access_key_id":"upload-id","access_key_secret":"upload-secret","bucket":"test-bucket","endpoint":endpoint,"expiration":"2030-01-01T00:00:00Z","key":"objects/upload.bin","security_token":"upload-token"}}})
}

// A complete instant-upload response has no object-storage credentials.
#[test]
fn accepts_complete_upload_without_requesting_object_storage() {
    let mut local = tempfile::NamedTempFile::new().unwrap();
    local.write_all(b"xunlei-bytes").unwrap();
    let http = UploadFixture::new(complete_file());
    let file = XunleiClient::new(session(), &http)
        .upload_file("parent", local.path())
        .unwrap();
    assert_eq!(file.id, "uploaded-file");
    assert!(http.uploads.lock().unwrap().is_empty());
}

// The modern webpage advertises only upload backends it can execute.
#[test]
fn modern_upload_negotiates_the_supported_aliyun_backend() {
    let local = tempfile::NamedTempFile::new().unwrap();
    let http = UploadFixture::new(complete_file());
    let client = XunleiClient::new(session(), &http);
    let _ = client.upload_file("parent", local.path());
    let _ = client.upload_init("parent", "upload.bin", 12, "synthetic-gcid");
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    for request in requests.iter() {
        let body: Value = serde_json::from_str(request.body.as_ref().unwrap()).unwrap();
        assert_eq!(
            body["resumable"]["supported_providers"],
            json!(["PROVIDER_ALIYUN"])
        );
    }
}

// Applying AWS signing to an OSS response (or doubling its bucket host) breaks upload.
#[test]
fn aliyun_contract_uses_oss_signing_and_completes_binary_upload() {
    for (endpoint, expected_url) in [
        (
            "https://oss-cn-test.aliyuncs.com",
            "https://test-bucket.oss-cn-test.aliyuncs.com/objects/upload.bin?uploads",
        ),
        (
            "https://test-bucket.oss-cn-test.aliyuncs.com",
            "https://test-bucket.oss-cn-test.aliyuncs.com/objects/upload.bin?uploads",
        ),
        (
            "oss-cn-test.aliyuncs.com",
            "https://test-bucket.oss-cn-test.aliyuncs.com/objects/upload.bin?uploads",
        ),
        (
            "test-bucket.oss-cn-test.aliyuncs.com",
            "https://test-bucket.oss-cn-test.aliyuncs.com/objects/upload.bin?uploads",
        ),
        (
            "oss-upload.xunlei.example",
            "https://test-bucket.oss-upload.xunlei.example/objects/upload.bin?uploads",
        ),
    ] {
        let mut local = tempfile::NamedTempFile::new().unwrap();
        local.write_all(b"xunlei-bytes").unwrap();
        let http = UploadFixture::new(resumable("PROVIDER_ALIYUN", endpoint));
        let file = XunleiClient::new(session(), &http)
            .upload_file("parent", local.path())
            .unwrap();
        assert_eq!(file.id, "uploaded-file");
        let uploads = http.uploads.lock().unwrap();
        assert_eq!(uploads.len(), 3);
        assert_eq!(uploads[0].url, expected_url);
        assert_eq!(upload_support::bytes(&uploads[1].body), b"xunlei-bytes");
        assert!(
            String::from_utf8_lossy(upload_support::bytes(&uploads[2].body))
                .contains("<ETag>&quot;part-etag&quot;</ETag>")
        );
        let canonical = format!("POST\n\napplication/octet-stream\n{}\nx-oss-security-token:upload-token\n/test-bucket/objects/upload.bin?uploads", uploads[0].headers["Date"]);
        let mut mac = Hmac::<Sha1>::new_from_slice(b"upload-secret").unwrap();
        mac.update(canonical.as_bytes());
        let signature =
            base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
        assert_eq!(
            uploads[0].headers["Authorization"],
            format!("OSS upload-id:{signature}")
        );
        assert!(!uploads[0].headers.contains_key("x-amz-security-token"));
    }
}

#[test]
fn aws_contract_signs_the_streamed_payload_digest_and_completes() {
    let mut local = tempfile::NamedTempFile::new().unwrap();
    local.write_all(b"hello").unwrap();
    let http = UploadFixture::new(resumable("aws", "https://s3.example.com"));
    XunleiClient::new(session(), &http)
        .upload_file("parent", local.path())
        .unwrap();
    let uploads = http.uploads.lock().unwrap();
    assert_eq!(uploads.len(), 3);
    assert_eq!(upload_support::bytes(&uploads[1].body), b"hello");
    assert_eq!(
        uploads[1].headers["x-amz-content-sha256"],
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    );
    assert!(uploads[1].headers["Authorization"].starts_with("AWS4-HMAC-SHA256 "));
}

#[test]
fn upload_rejects_unsafe_or_unrecognized_schemeless_endpoints() {
    for (provider, endpoint) in [
        ("aws", "oss-cn-test.aliyuncs.com"),
        ("PROVIDER_ALIYUN", "http://oss-cn-test.aliyuncs.com"),
        ("PROVIDER_ALIYUN", "//oss-cn-test.aliyuncs.com"),
        ("PROVIDER_ALIYUN", "user@oss-cn-test.aliyuncs.com"),
        ("PROVIDER_ALIYUN", "oss-cn-test.aliyuncs.com/path"),
        ("PROVIDER_ALIYUN", "oss-cn-test.aliyuncs.com?query=1"),
        ("PROVIDER_ALIYUN", "oss-cn-test.aliyuncs.com#fragment"),
        ("PROVIDER_ALIYUN", "oss_upload.xunlei.example"),
        ("PROVIDER_ALIYUN", "-oss-upload.xunlei.example"),
        ("PROVIDER_ALIYUN", "oss-upload..xunlei.example"),
    ] {
        let local = tempfile::NamedTempFile::new().unwrap();
        let http = UploadFixture::new(resumable(provider, endpoint));
        assert!(XunleiClient::new(session(), &http)
            .upload_file("parent", local.path())
            .is_err());
        assert!(http.uploads.lock().unwrap().is_empty());
    }
}

#[test]
fn modern_upload_waits_for_pending_file_to_become_complete() {
    let mut local = tempfile::NamedTempFile::new().unwrap();
    local.write_all(b"xunlei-bytes").unwrap();
    let response = resumable("PROVIDER_ALIYUN", "oss-upload.xunlei.example");
    let http = UploadFixture::new(response.clone());
    let mut pending = response["file"].clone();
    pending["phase"] = json!("PHASE_TYPE_PENDING");
    let mut ready = pending.clone();
    ready["phase"] = json!("PHASE_TYPE_COMPLETE");
    ready["name"] = json!("processed.bin");
    *http.metadata.lock().unwrap() = VecDeque::from([pending, ready]);

    let file = XunleiClient::new(session(), &http)
        .upload_file("parent", local.path())
        .unwrap();
    assert_eq!(file.name, "processed.bin");
    let requests = http.requests.lock().unwrap();
    let reads: Vec<_> = requests
        .iter()
        .filter(|request| request.method == "GET")
        .collect();
    assert_eq!(reads.len(), 2);
    assert!(reads
        .iter()
        .all(|request| request.url == "https://api-pan.xunlei.com/drive/v1/files/uploaded-file"));
}

#[test]
fn modern_upload_stops_when_cloud_processing_fails() {
    let mut local = tempfile::NamedTempFile::new().unwrap();
    local.write_all(b"xunlei-bytes").unwrap();
    let response = resumable("PROVIDER_ALIYUN", "oss-upload.xunlei.example");
    let http = UploadFixture::new(response.clone());
    let mut failed = response["file"].clone();
    failed["phase"] = json!("PHASE_TYPE_ERROR");
    *http.metadata.lock().unwrap() = VecDeque::from([failed]);

    let error = XunleiClient::new(session(), &http)
        .upload_file("parent", local.path())
        .unwrap_err();
    assert!(matches!(error, QuarkError::Provider(message) if message.contains("处理失败")));
    assert_eq!(
        http.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.method == "GET")
            .count(),
        1
    );
}

#[test]
fn unsupported_storage_provider_is_rejected_before_sending_file_bytes() {
    let local = tempfile::NamedTempFile::new().unwrap();
    let http = UploadFixture::new(resumable("PROVIDER_TENGXUNYUN", "https://cos.example"));
    assert!(XunleiClient::new(session(), &http)
        .upload_file("parent", local.path())
        .is_err());
    assert!(http.uploads.lock().unwrap().is_empty());
}

#[test]
fn incomplete_unknown_upload_is_not_reported_as_complete() {
    let local = tempfile::NamedTempFile::new().unwrap();
    let mut response = complete_file();
    response["file"]["phase"] = json!("PHASE_TYPE_RUNNING");
    let http = UploadFixture::new(response);
    assert!(XunleiClient::new(session(), &http)
        .upload_file("parent", local.path())
        .is_err());
}

// At 128 MiB + one byte GCID must switch from 256 KiB to 512 KiB blocks.
// Literal digest independently calculated from 256 zero-filled 512 KiB blocks
// followed by one zero byte, combining the raw inner SHA-1 digests.
#[test]
fn gcid_switches_block_size_immediately_above_128_mib() {
    let local = tempfile::NamedTempFile::new().unwrap();
    local.as_file().set_len(128 * 1024 * 1024 + 1).unwrap();
    let http = UploadFixture::new(complete_file());
    let _ = XunleiClient::new(session(), &http).upload_file("parent", local.path());
    let requests = http.requests.lock().unwrap();
    let body: Value = serde_json::from_str(requests[0].body.as_ref().unwrap()).unwrap();
    assert_eq!(body["hash"], "07A062B60C0BCCDEBA1F220FD6D1B28326AFF01B");
}
