//! PikPak resumable upload protocol and Alibaba OSS multipart transfer.
//! Independently implemented from Apache-2.0 pikpaktui protocol documentation/source.
use super::{
    types::{encoded, required},
    *,
};
use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::{collections::BTreeMap, io::Read, path::Path};

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
    file: Vec<u8>,
) -> Result<(String, Vec<u8>), QuarkError> {
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
    body.extend_from_slice(&file);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    Ok((format!("multipart/form-data; boundary={boundary}"), body))
}

pub struct UploadHttpRequest {
    pub method: &'static str,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
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
        let response = b
            .body(r.body)
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
struct Oss {
    endpoint: String,
    bucket: String,
    key: String,
    id: String,
    secret: String,
    token: String,
}
impl Oss {
    fn request(
        &self,
        method: &'static str,
        query: &str,
        body: Vec<u8>,
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
            body,
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
impl<'a, T: CloudHttpTransport + CloudUploadTransport, S: CredentialStore> PikPakClient<'a, T, S> {
    pub fn upload_file(&self, parent: &str, path: &Path) -> Result<CloudFile, QuarkError> {
        upload_pikpak_file(self.http, parent, path, |payload| {
            self.request(
                "POST",
                "https://api-drive.mypikpak.com/drive/v1/files".into(),
                Some(payload),
            )
        })
    }
}
/// File preparation and OSS transfer require no live account credentials.
pub fn upload_pikpak_file(
    http: &impl CloudUploadTransport,
    parent: &str,
    path: &Path,
    initialize: impl FnOnce(Value) -> Result<Value, QuarkError>,
) -> Result<CloudFile, QuarkError> {
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
    let init = initialize(
        json!({"kind":"drive#file","name":name,"size":size.to_string(),"hash":hash,"parent_id":if parent=="0"{""}else{parent},"upload_type":"UPLOAD_TYPE_RESUMABLE","objProvider":{"provider":"UPLOAD_TYPE_UNKNOWN"}}),
    )?;
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
    let mut input = std::fs::File::open(path).map_err(|_| file_error())?;
    let mut remaining = size;
    let mut number = 1;
    let mut completion = String::from("<CompleteMultipartUpload>");
    loop {
        let mut bytes = vec![0; remaining.min(10 * 1024 * 1024) as usize];
        input.read_exact(&mut bytes).map_err(|_| file_error())?;
        remaining -= bytes.len() as u64;
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
