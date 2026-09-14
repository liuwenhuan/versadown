//! PikPak resumable upload protocol and Alibaba OSS multipart transfer.
//! Independently implemented from Apache-2.0 pikpaktui protocol documentation/source.
use super::{
    types::{encoded, required},
    *,
};
use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha1::{Digest, Sha1, Sha1Core};
use std::{collections::BTreeMap, io::Read, path::Path};
mod body;
pub use body::*;
mod control;
pub(crate) use control::{is_upload_cancelled, upload_cancelled, upload_registry};
mod progress;
pub use progress::CloudUploadProgress;
pub(crate) use progress::{report_upload_progress, UploadProgressTracker};

/// Validates a server-provided transfer endpoint before file bytes are sent.
/// Scheme-relative endpoints are normalised to HTTPS; credentials and fragments
/// are never valid for a temporary transfer endpoint.
pub(crate) fn https_transfer_url(value: &str) -> Result<String, QuarkError> {
    let value = value.trim();
    let value = value
        .strip_prefix("//")
        .map(|rest| format!("https://{rest}"))
        .unwrap_or_else(|| value.to_owned());
    let url = url::Url::parse(&value).map_err(|_| QuarkError::InvalidResponse)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(QuarkError::InvalidResponse);
    }
    Ok(url.into())
}

/// Builds a binary multipart body without ever stringifying file bytes.
/// Field names and filenames must be header-safe; values remain body data.
pub(crate) fn multipart_file(
    fields: &[(&str, String)],
    file_field: &str,
    file_name: &str,
    file: UploadBody,
) -> Result<(String, UploadBody), QuarkError> {
    let safe_header = |value: &str| !value.is_empty() && !value.contains(['\r', '\n']);
    if !safe_header(file_field)
        || !safe_header(file_name)
        || fields.iter().any(|(name, _)| !safe_header(name))
    {
        return Err(QuarkError::InvalidInput("文件名无效".into()));
    }
    let boundary = format!("----AllDownloader{}", uuid::Uuid::new_v4().simple());
    let mut body = Vec::new();
    for (name, value) in fields {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n")
                .as_bytes(),
        );
        body.extend_from_slice(value.as_bytes());
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{file_field}\"; filename=\"{file_name}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    Ok((
        format!("multipart/form-data; boundary={boundary}"),
        UploadBody::Multipart {
            prefix: body,
            file: Box::new(file),
            suffix: format!("\r\n--{boundary}--\r\n").into_bytes(),
        },
    ))
}

pub struct UploadHttpRequest {
    pub method: &'static str,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: UploadBody,
}
pub struct UploadHttpResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: String,
}
pub trait CloudUploadTransport: Send + Sync {
    fn send_upload(&self, request: UploadHttpRequest) -> Result<UploadHttpResponse, QuarkError>;
}
impl CloudUploadTransport for ReqwestTransport {
    fn send_upload(&self, r: UploadHttpRequest) -> Result<UploadHttpResponse, QuarkError> {
        let mut b = self
            .client
            .request(
                r.method.parse().map_err(|_| QuarkError::InvalidResponse)?,
                &r.url,
            )
            .timeout(std::time::Duration::from_secs(300));
        for (k, v) in r.headers {
            b = b.header(k, v)
        }
        let length = r.body.len();
        let reader = r.body.reader(UploadControl::default())?;
        let response = b
            .header(reqwest::header::CONTENT_LENGTH, length)
            .body(reqwest::blocking::Body::sized(reader, length))
            .send()
            .map_err(|_| QuarkError::Transport("上传分片连接失败，请重试".into()))?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(k, v)| {
                v.to_str()
                    .ok()
                    .map(|v| (k.as_str().to_owned(), v.to_owned()))
            })
            .collect();
        let body = response.text().map_err(|_| QuarkError::InvalidResponse)?;
        Ok(UploadHttpResponse {
            status,
            headers,
            body,
        })
    }
}
fn file_error() -> QuarkError {
    QuarkError::InvalidInput("无法读取选中的本地文件".into())
}
pub fn pikpak_hash(path: &Path) -> Result<String, QuarkError> {
    let size = std::fs::metadata(path).map_err(|_| file_error())?.len();
    let chunk = if size < 128 * 1024 * 1024 {
        256 * 1024
    } else if size < 256 * 1024 * 1024 {
        512 * 1024
    } else if size < 512 * 1024 * 1024 {
        1024 * 1024
    } else {
        2 * 1024 * 1024
    };
    let mut file = std::fs::File::open(path).map_err(|_| file_error())?;
    let mut remaining = size;
    let mut hashes = String::new();
    loop {
        let mut bytes = vec![0; remaining.min(chunk) as usize];
        file.read_exact(&mut bytes).map_err(|_| file_error())?;
        hashes.push_str(&format!("{:X}", Sha1::digest(&bytes)));
        remaining = remaining.saturating_sub(chunk);
        if remaining == 0 {
            break;
        }
    }
    Ok(format!("{:X}", Sha1::digest(hashes.as_bytes())))
}
pub(crate) struct Oss {
    pub(crate) endpoint: String,
    pub(crate) bucket: String,
    pub(crate) key: String,
    pub(crate) id: String,
    pub(crate) secret: String,
    pub(crate) token: String,
}
impl Oss {
    pub(crate) fn request(
        &self,
        method: &'static str,
        query: &str,
        body: impl Into<UploadBody>,
    ) -> Result<UploadHttpRequest, QuarkError> {
        let date = httpdate::fmt_http_date(std::time::SystemTime::now());
        let canonical = format!(
            "{method}\n\napplication/octet-stream\n{date}\nx-oss-security-token:{}\n/{}/{}{query}",
            self.token, self.bucket, self.key
        );
        let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(self.secret.as_bytes())
            .map_err(|_| QuarkError::InvalidResponse)?;
        mac.update(canonical.as_bytes());
        let signature =
            base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
        let endpoint = self
            .endpoint
            .trim_end_matches('/')
            .trim_start_matches("https://");
        let mut url = url::Url::parse(&format!("https://{endpoint}"))
            .map_err(|_| QuarkError::InvalidResponse)?;
        if url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() {
            return Err(QuarkError::InvalidResponse);
        }
        url.set_path(&self.key);
        url.set_query(query.strip_prefix('?'));
        Ok(UploadHttpRequest {
            method,
            url: url.into(),
            headers: BTreeMap::from([
                ("Date".into(), date),
                (
                    "Authorization".into(),
                    format!("OSS {}:{signature}", self.id),
                ),
                ("Content-Type".into(), "application/octet-stream".into()),
                ("x-oss-security-token".into(), self.token.clone()),
            ]),
            body: body.into(),
        })
    }
}
fn checked(response: UploadHttpResponse) -> Result<UploadHttpResponse, QuarkError> {
    if !(200..300).contains(&response.status) || response.body.contains("<Error>") {
        Err(QuarkError::Provider(format!(
            "上传服务拒绝请求（HTTP {}），请重试",
            response.status
        )))
    } else {
        Ok(response)
    }
}
fn xml_tag(body: &str, tag: &str) -> Result<String, QuarkError> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let s = body.find(&open).ok_or(QuarkError::InvalidResponse)? + open.len();
    let e = body[s..].find(&close).ok_or(QuarkError::InvalidResponse)? + s;
    Ok(body[s..e].into())
}
fn xml_escape(v: &str) -> String {
    v.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
/// File preparation and OSS transfer require no live account credentials.
pub fn upload_pikpak_file(
    http: &impl CloudUploadTransport,
    parent: &str,
    path: &Path,
    initialize: impl FnOnce(Value) -> Result<Value, QuarkError>,
) -> Result<CloudFile, QuarkError> {
    let snapshot = UploadFileSnapshot::capture(path)?;
    let metadata = std::fs::symlink_metadata(path).map_err(|_| file_error())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(QuarkError::InvalidInput("请选择普通文件上传".into()));
    }
    let name = path
        .file_name()
        .ok_or_else(file_error)?
        .to_string_lossy()
        .into_owned();
    let size = metadata.len();
    let hash = pikpak_hash(path)?;
    snapshot.validate()?;
    let init = initialize(
        json!({"kind":"drive#file","name":name,"size":size.to_string(),"hash":hash,"parent_id":if parent=="0"{""}else{parent},"upload_type":"UPLOAD_TYPE_RESUMABLE","objProvider":{"provider":"UPLOAD_TYPE_UNKNOWN"}}),
    )?;
    snapshot.validate()?;
    let file = parse_pikpak_file(&init["file"], parent)?;
    if init["file"]["phase"] == "PHASE_TYPE_COMPLETE" {
        return Ok(file);
    }
    let p = &init["resumable"]["params"];
    let oss = Oss {
        endpoint: required(p, "endpoint")?,
        bucket: required(p, "bucket")?,
        key: required(p, "key")?,
        id: required(p, "access_key_id")?,
        secret: required(p, "access_key_secret")?,
        token: required(p, "security_token")?,
    };
    let started = checked(http.send_upload(oss.request("POST", "?uploads", Vec::new())?)?)?;
    let id = xml_tag(&started.body, "UploadId")?;
    let mut remaining = size;
    let mut number = 1;
    let mut completion = String::from("<CompleteMultipartUpload>");
    loop {
        let length = remaining.min(10 * 1024 * 1024);
        let bytes = snapshot.slice(size - remaining, length)?;
        remaining -= length;
        let part = checked(http.send_upload(oss.request(
            "PUT",
            &format!("?partNumber={number}&uploadId={}", encoded(&id)),
            bytes,
        )?)?)?;
        let etag = part
            .headers
            .get("etag")
            .filter(|e| !e.is_empty())
            .ok_or(QuarkError::InvalidResponse)?;
        completion.push_str(&format!(
            "<Part><PartNumber>{number}</PartNumber><ETag>{}</ETag></Part>",
            xml_escape(etag)
        ));
        number += 1;
        if remaining == 0 {
            break;
        }
    }
    completion.push_str("</CompleteMultipartUpload>");
    snapshot.validate()?;
    let done = checked(http.send_upload(oss.request(
        "POST",
        &format!("?uploadId={}", encoded(&id)),
        completion.into_bytes(),
    )?)?)?;
    if !done.body.contains("<CompleteMultipartUploadResult") {
        return Err(QuarkError::InvalidResponse);
    }
    Ok(file)
}

const CLOUD_DRIVE_DEFAULT_PART_SIZE: usize = 4 * 1024 * 1024;

pub(crate) struct CloudDriveUploadProfile {
    pub(crate) object_host: &'static str,
    pub(crate) object_store_user_agent: &'static str,
}

/// Runs the CloudDrive browser upload protocol shared by Quark and UC.
///
/// The adapters retain ownership of their authenticated API requests and
/// headers.  This helper only processes the short-lived transfer contract on
/// the stack, validates the object-store origin before bytes are sent, and
/// never serializes or logs its authorization values.
pub(crate) fn upload_cloud_drive_file(
    http: &impl CloudUploadTransport,
    parent: &str,
    path: &Path,
    profile: CloudDriveUploadProfile,
    pre_upload: impl FnOnce(Value) -> Result<Value, QuarkError>,
    update_hash: impl FnOnce(Value) -> Result<Value, QuarkError>,
    authorize: impl Fn(Value) -> Result<Value, QuarkError>,
    finish: impl FnOnce(Value) -> Result<Value, QuarkError>,
) -> Result<CloudFile, QuarkError> {
    let snapshot = UploadFileSnapshot::capture(path)?;
    let (name, size, modified_at) = cloud_drive_file_metadata(path)?;
    let (md5, sha1) = cloud_drive_hashes(path, size)?;
    snapshot.validate()?;
    let content_type = "application/octet-stream";
    let pre = pre_upload(json!({
        "ccp_hash_update": true,
        "parallel_upload": true,
        "pdir_fid": parent,
        "dir_name": "",
        "file_name": name,
        "format_type": content_type,
        "l_created_at": modified_at,
        "l_updated_at": modified_at,
        "size": size,
    }))?;
    let pre_data = pre.get("data").ok_or(QuarkError::InvalidResponse)?;
    snapshot.validate()?;
    let task_id = required(pre_data, "task_id")?;
    let hash = update_hash(json!({"task_id":task_id,"md5":md5,"sha1":sha1}))?;
    let hash_data = hash.get("data").ok_or(QuarkError::InvalidResponse)?;
    snapshot.validate()?;
    if hash_data.get("finish").and_then(Value::as_bool) == Some(true) {
        return cloud_drive_file(hash_data, parent, &name, size);
    }

    let contract = CloudDriveUploadContract {
        task_id,
        auth_info: required(pre_data, "auth_info")?,
        upload_id: required(pre_data, "upload_id")?,
        object_url: cloud_drive_object_url(
            &required(pre_data, "bucket")?,
            &required(pre_data, "obj_key")?,
            &required(pre_data, "upload_url")?,
            profile.object_host,
        )?,
        bucket: required(pre_data, "bucket")?,
        object_key: required(pre_data, "obj_key")?,
        callback: pre_data
            .get("callback")
            .filter(|value| !value.is_null())
            .map(cloud_drive_callback)
            .transpose()?,
        part_size: cloud_drive_part_size(&pre)?,
    };
    if let Some(callback) = &contract.callback {
        checked_temporary_header(callback)?;
    }

    let mut remaining = size;
    let mut parts = Vec::new();
    let mut number = 1_u64;
    let uploaded_digest = UploadDigest::default();
    while remaining > 0 || number == 1 {
        let length = remaining.min(contract.part_size as u64);
        let mut bytes = snapshot.slice(size - remaining, length)?;
        if let UploadBody::FileSlice { digest, .. } = &mut bytes {
            *digest = Some(uploaded_digest.clone());
        }
        remaining = remaining.saturating_sub(length);
        let query = format!(
            "partNumber={number}&uploadId={}",
            encoded(&contract.upload_id)
        );
        let hash_context = (number > 1)
            .then(|| uploaded_digest.quark_hash_context())
            .transpose()?;
        let request = cloud_drive_authorized_request(
            &contract,
            "PUT",
            &query,
            content_type,
            bytes,
            None,
            hash_context.as_deref(),
            profile.object_store_user_agent,
            &authorize,
        )?;
        let response = checked(http.send_upload(request)?)?;
        let etag = checked_etag(&response)?;
        parts.push((number, etag));
        number += 1;
        if remaining == 0 {
            break;
        }
    }
    snapshot.validate()?;
    uploaded_digest.verify(&md5, &sha1)?;
    let mut manifest = String::from("<CompleteMultipartUpload>");
    for (number, etag) in parts {
        manifest.push_str(&format!(
            "<Part><PartNumber>{number}</PartNumber><ETag>{}</ETag></Part>",
            xml_escape(&etag)
        ));
    }
    manifest.push_str("</CompleteMultipartUpload>");
    let manifest = manifest.into_bytes();
    let completed = checked(http.send_upload(cloud_drive_authorized_request(
        &contract,
        "POST",
        &format!("uploadId={}", encoded(&contract.upload_id)),
        "application/xml",
        manifest.into(),
        contract.callback.as_deref(),
        None,
        profile.object_store_user_agent,
        &authorize,
    )?)?)?;
    if !cloud_drive_completion_succeeded(&completed.body) {
        return Err(QuarkError::InvalidResponse);
    }
    snapshot.validate()?;
    let final_response = finish(json!({
        "task_id":contract.task_id,
        "obj_key":contract.object_key,
    }))?;
    cloud_drive_file(
        final_response
            .get("data")
            .ok_or(QuarkError::InvalidResponse)?,
        parent,
        &name,
        size,
    )
}

fn cloud_drive_completion_succeeded(body: &str) -> bool {
    body.contains("CompleteMultipartUploadResult")
        || serde_json::from_str::<Value>(body)
            .ok()
            .and_then(|value| {
                value
                    .get("Status")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .is_some_and(|status| status.eq_ignore_ascii_case("OK"))
}

struct CloudDriveUploadContract {
    task_id: String,
    auth_info: String,
    upload_id: String,
    object_url: String,
    bucket: String,
    object_key: String,
    callback: Option<String>,
    part_size: usize,
}

fn cloud_drive_file_metadata(path: &Path) -> Result<(String, u64, u64), QuarkError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| file_error())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(QuarkError::InvalidInput("请选择普通文件上传".into()));
    }
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| {
            !value.is_empty()
                && !value.contains(['/', '\\', '\r', '\n'])
                && !value.chars().any(char::is_control)
        })
        .ok_or_else(|| QuarkError::InvalidInput("文件名无效".into()))?
        .to_owned();
    let modified_at = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis())
        .and_then(|millis| u64::try_from(millis).ok())
        .unwrap_or(0);
    Ok((name, metadata.len(), modified_at))
}

fn cloud_drive_hashes(path: &Path, expected_size: u64) -> Result<(String, String), QuarkError> {
    let mut source = std::fs::File::open(path).map_err(|_| file_error())?;
    let mut md5 = md5::Context::new();
    let mut sha1 = Sha1::new();
    let mut count = 0_u64;
    let mut buffer = vec![0; 256 * 1024];
    loop {
        let read = source.read(&mut buffer).map_err(|_| file_error())?;
        if read == 0 {
            break;
        }
        count = count
            .checked_add(read as u64)
            .ok_or(QuarkError::InvalidInput("文件过大，无法上传".into()))?;
        md5.consume(&buffer[..read]);
        sha1.update(&buffer[..read]);
    }
    if count != expected_size {
        return Err(QuarkError::InvalidInput("本地文件在上传前发生变化".into()));
    }
    Ok((
        format!("{:x}", md5.finalize()),
        format!("{:x}", sha1.finalize()),
    ))
}

fn cloud_drive_object_url(
    bucket: &str,
    key: &str,
    upload_url: &str,
    object_host: &str,
) -> Result<String, QuarkError> {
    if bucket.is_empty()
        || bucket.len() > 63
        || bucket.starts_with('-')
        || bucket.ends_with('-')
        || !bucket
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || key.is_empty()
        || key.len() > 2048
        || key.starts_with('/')
        || key.chars().any(char::is_control)
    {
        return Err(QuarkError::InvalidResponse);
    }
    let upload_url = upload_url.trim();
    let upload_url = upload_url
        .strip_prefix("//")
        .map(|rest| format!("https://{rest}"))
        .unwrap_or_else(|| upload_url.to_owned());
    let mut url = url::Url::parse(&upload_url).map_err(|_| QuarkError::InvalidResponse)?;
    let expected_host = format!("{bucket}.{object_host}");
    let host = url.host_str().ok_or(QuarkError::InvalidResponse)?;
    // Quark's current pre-upload response still returns this fixed provider
    // endpoint as http. Upgrade only the already allowlisted object host;
    // arbitrary insecure endpoints remain rejected by the shared validator.
    if url.scheme() == "http"
        && url.port().is_none()
        && (host == object_host || host == expected_host)
    {
        url.set_scheme("https")
            .map_err(|_| QuarkError::InvalidResponse)?;
    }
    let transfer_url = https_transfer_url(url.as_str())?;
    let mut url = url::Url::parse(&transfer_url).map_err(|_| QuarkError::InvalidResponse)?;
    if url.path() != "/" || url.query().is_some() || !matches!(url.port(), None | Some(443)) {
        return Err(QuarkError::InvalidResponse);
    }
    let host = url.host_str().ok_or(QuarkError::InvalidResponse)?;
    if host != object_host && host != expected_host {
        return Err(QuarkError::InvalidResponse);
    }
    if host == object_host {
        url.set_host(Some(&expected_host))
            .map_err(|_| QuarkError::InvalidResponse)?;
    }
    url.set_path(key);
    https_transfer_url(url.as_str())
}

fn cloud_drive_callback(value: &Value) -> Result<String, QuarkError> {
    let callback = serde_json::to_string(value).map_err(|_| QuarkError::InvalidResponse)?;
    let callback = base64::engine::general_purpose::STANDARD.encode(callback);
    checked_temporary_header(&callback)?;
    Ok(callback)
}

fn cloud_drive_part_size(pre: &Value) -> Result<usize, QuarkError> {
    let part_size = pre
        .pointer("/metadata/part_size")
        .and_then(Value::as_u64)
        .unwrap_or(CLOUD_DRIVE_DEFAULT_PART_SIZE as u64);
    if !(1024 * 1024..=64 * 1024 * 1024).contains(&part_size) {
        return Err(QuarkError::InvalidResponse);
    }
    usize::try_from(part_size).map_err(|_| QuarkError::InvalidResponse)
}

/// The CloudDrive object store requires every part after the first to carry
/// the SHA-1 state of the bytes already accepted. Its JSON shape mirrors the
/// browser's OpenSSL-compatible SHA-1 context.
#[derive(Clone)]
struct QuarkSha1Context {
    state: [u32; 5],
    total_bytes: u64,
    pending: Vec<u8>,
}

impl Default for QuarkSha1Context {
    fn default() -> Self {
        Self {
            state: [
                0x6745_2301,
                0xEFCD_AB89,
                0x98BA_DCFE,
                0x1032_5476,
                0xC3D2_E1F0,
            ],
            total_bytes: 0,
            pending: Vec::new(),
        }
    }
}

impl QuarkSha1Context {
    fn update(&mut self, mut bytes: &[u8]) -> Result<(), QuarkError> {
        self.total_bytes = self
            .total_bytes
            .checked_add(bytes.len() as u64)
            .ok_or(QuarkError::InvalidResponse)?;
        if !self.pending.is_empty() {
            let needed = 64 - self.pending.len();
            let take = needed.min(bytes.len());
            self.pending.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if self.pending.len() == 64 {
                let block: [u8; 64] = self.pending.as_slice().try_into().unwrap();
                self.compress(block);
                self.pending.clear();
            }
        }
        while bytes.len() >= 64 {
            let (block, rest) = bytes.split_at(64);
            self.compress(block.try_into().unwrap());
            bytes = rest;
        }
        self.pending.extend_from_slice(bytes);
        Ok(())
    }

    fn compress(&mut self, block: [u8; 64]) {
        let block = sha1::digest::core_api::Block::<Sha1Core>::clone_from_slice(&block);
        sha1::compress(&mut self.state, std::slice::from_ref(&block));
    }

    fn encoded(&self) -> Result<String, QuarkError> {
        // Quark's server-issued part sizes are MiB-aligned, so every preceding
        // part must leave a fully compressed SHA-1 state for the next request.
        if !self.pending.is_empty() {
            return Err(QuarkError::InvalidResponse);
        }
        let bits = self
            .total_bytes
            .checked_mul(8)
            .ok_or(QuarkError::InvalidResponse)?;
        let value = json!({
            "hash_type": "sha1",
            "h0": self.state[0].to_string(),
            "h1": self.state[1].to_string(),
            "h2": self.state[2].to_string(),
            "h3": self.state[3].to_string(),
            "h4": self.state[4].to_string(),
            "Nl": (bits as u32).to_string(),
            "Nh": ((bits >> 32) as u32).to_string(),
            "data": "",
            "num": "0",
        });
        let encoded = base64::engine::general_purpose::STANDARD
            .encode(serde_json::to_vec(&value).map_err(|_| QuarkError::InvalidResponse)?);
        checked_temporary_header(&encoded)?;
        Ok(encoded)
    }
}

fn cloud_drive_authorized_request(
    contract: &CloudDriveUploadContract,
    method: &'static str,
    query: &str,
    content_type: &str,
    body: UploadBody,
    callback: Option<&str>,
    hash_context: Option<&str>,
    object_store_user_agent: &str,
    authorize: &impl Fn(Value) -> Result<Value, QuarkError>,
) -> Result<UploadHttpRequest, QuarkError> {
    let date = httpdate::fmt_http_date(std::time::SystemTime::now());
    let content_md5 = if method == "POST" {
        let UploadBody::Bytes(bytes) = &body else {
            return Err(QuarkError::InvalidResponse);
        };
        Some(base64::engine::general_purpose::STANDARD.encode(md5::compute(bytes).0))
    } else {
        None
    };
    let mut oss_headers = String::new();
    if let Some(callback) = callback {
        checked_temporary_header(callback)?;
        oss_headers.push_str(&format!("x-oss-callback:{callback}\n"));
    }
    oss_headers.push_str(&format!("x-oss-date:{date}\n"));
    if let Some(hash_context) = hash_context {
        checked_temporary_header(hash_context)?;
        oss_headers.push_str(&format!("x-oss-hash-ctx:{hash_context}\n"));
    }
    checked_temporary_header(object_store_user_agent)?;
    oss_headers.push_str(&format!("x-oss-user-agent:{object_store_user_agent}\n"));
    let auth = authorize(json!({
        "task_id":&contract.task_id,
        "auth_info":&contract.auth_info,
        "auth_meta":format!("{method}\n{}\n{content_type}\n{date}\n{oss_headers}/{}/{}?{query}", content_md5.as_deref().unwrap_or(""), contract.bucket, contract.object_key),
    }))?;
    let auth_key = required(
        auth.get("data").ok_or(QuarkError::InvalidResponse)?,
        "auth_key",
    )?;
    checked_temporary_header(&auth_key)?;
    let mut url = url::Url::parse(&contract.object_url).map_err(|_| QuarkError::InvalidResponse)?;
    url.set_query(Some(query));
    let url = https_transfer_url(url.as_str())?;
    let mut headers = BTreeMap::from([
        ("Content-Type".into(), content_type.into()),
        ("x-oss-date".into(), date),
        ("x-oss-user-agent".into(), object_store_user_agent.into()),
        ("Authorization".into(), auth_key),
    ]);
    if let Some(content_md5) = content_md5 {
        checked_temporary_header(&content_md5)?;
        headers.insert("Content-MD5".into(), content_md5);
    }
    if let Some(callback) = callback {
        checked_temporary_header(callback)?;
        headers.insert("x-oss-callback".into(), callback.into());
    }
    if let Some(hash_context) = hash_context {
        checked_temporary_header(hash_context)?;
        headers.insert("x-oss-hash-ctx".into(), hash_context.into());
    }
    Ok(UploadHttpRequest {
        method,
        url,
        headers,
        body,
    })
}

fn checked_temporary_header(value: &str) -> Result<(), QuarkError> {
    if value.is_empty()
        || value.len() > 16 * 1024
        || value.contains(['\r', '\n'])
        || value.chars().any(char::is_control)
    {
        return Err(QuarkError::InvalidResponse);
    }
    Ok(())
}

fn checked_etag(response: &UploadHttpResponse) -> Result<String, QuarkError> {
    let etag = response
        .headers
        .iter()
        .find(|(name, value)| name.eq_ignore_ascii_case("etag") && !value.is_empty())
        .map(|(_, value)| value.clone())
        .ok_or(QuarkError::InvalidResponse)?;
    checked_temporary_header(&etag)?;
    Ok(etag)
}

fn cloud_drive_file(
    value: &Value,
    parent: &str,
    fallback_name: &str,
    fallback_size: u64,
) -> Result<CloudFile, QuarkError> {
    let value = value.get("file").unwrap_or(value);
    let id = value
        .get("fid")
        .or_else(|| value.get("file_id"))
        .or_else(|| value.get("id"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(QuarkError::InvalidResponse)?;
    Ok(CloudFile {
        id: id.into(),
        parent_id: parent.into(),
        name: value
            .get("file_name")
            .or_else(|| value.get("name"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or(fallback_name)
            .into(),
        kind: "file".into(),
        size: value
            .get("size")
            .and_then(Value::as_u64)
            .or(Some(fallback_size)),
        modified_at: None,
    })
}
