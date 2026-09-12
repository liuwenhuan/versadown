use alldownloader_lib::cloud::{QuarkError, UploadBody, UploadControl, UploadHttpRequest};
use std::io::Read;

// Test transports consume tiny protocol fixtures while the source still exists.
// Production never collects a FileSlice; the independent sparse-file test reads
// its 512 MiB source using a 32-byte buffer.
pub fn consume(mut request: UploadHttpRequest) -> Result<UploadHttpRequest, QuarkError> {
    assert!(
        request.body.len() <= 32 * 1024 * 1024,
        "fixture exceeded recording budget"
    );
    let mut bytes = Vec::new();
    request
        .body
        .reader(UploadControl::default())?
        .read_to_end(&mut bytes)
        .map_err(|_| QuarkError::InvalidInput("本地文件在上传过程中发生变化".into()))?;
    request.body = UploadBody::Bytes(bytes);
    Ok(request)
}
#[allow(dead_code)]
pub fn bytes(body: &UploadBody) -> &[u8] {
    match body {
        UploadBody::Bytes(bytes) => bytes,
        _ => panic!("transport must consume its body"),
    }
}
