use super::{
    http::response_json,
    types::{encoded, required},
    *,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const QUARK_DESKTOP_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) quark-cloud-drive/2.5.56 Chrome/100.0.4896.160 Electron/18.3.5.12-a038f7b798 Safari/537.36 Channel/pckk_other_ch";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct QuarkFile {
    pub id: String,
    pub name: String,
    pub is_folder: bool,
    pub size: Option<u64>,
}
pub fn parse_quark_files(body: &str) -> Result<Vec<QuarkFile>, QuarkError> {
    let v: Value = serde_json::from_str(body).map_err(|_| QuarkError::InvalidResponse)?;
    v.pointer("/data/list")
        .and_then(Value::as_array)
        .ok_or(QuarkError::InvalidResponse)?
        .iter()
        .map(|x| {
            Ok(QuarkFile {
                id: required(x, "fid")?,
                name: required(x, "file_name")?,
                is_folder: x["dir"]
                    .as_bool()
                    .unwrap_or(x["file_type"].as_i64() == Some(0)),
                size: x["size"].as_u64(),
            })
        })
        .collect()
}
pub fn parse_quark_download_url(body: &str) -> Result<String, QuarkError> {
    let v: Value = serde_json::from_str(body).map_err(|_| QuarkError::InvalidResponse)?;
    v.pointer("/data/0/download_url")
        .and_then(Value::as_str)
        .filter(|u| u.starts_with("https://"))
        .map(str::to_owned)
        .ok_or(QuarkError::InvalidResponse)
}
pub struct QuarkClient<'a, T: CloudHttpTransport> {
    cookie: String,
    http: &'a T,
}
impl<'a, T: CloudHttpTransport> QuarkClient<'a, T> {
    pub fn new(cookie: String, http: &'a T) -> Self {
        Self { cookie, http }
    }
    fn request_url(
        &self,
        method: &'static str,
        url: String,
        body: Option<Value>,
    ) -> Result<CloudHttpResponse, QuarkError> {
        let headers = BTreeMap::from([
            ("Cookie".into(), self.cookie.clone()),
            ("Origin".into(), "https://pan.quark.cn".into()),
            ("Referer".into(), "https://pan.quark.cn/".into()),
            ("User-Agent".into(), QUARK_DESKTOP_USER_AGENT.into()),
        ]);
        let r = self.http.send(CloudHttpRequest {
            method,
            url,
            headers,
            body: body.map(|v| v.to_string()),
        })?;
        response_json(&r)?;
        Ok(r)
    }
    fn request(
        &self,
        method: &'static str,
        path: &str,
        body: Option<Value>,
    ) -> Result<CloudHttpResponse, QuarkError> {
        self.request_url(
            method,
            format!(
                "https://drive-pc.quark.cn/1/clouddrive/{path}{}pr=ucpro&fr=pc",
                if path.contains('?') { "&" } else { "?" }
            ),
            body,
        )
    }
    pub fn account(&self) -> Result<CloudAccount, QuarkError> {
        let r = self.request_url(
            "GET",
            "https://pan.quark.cn/account/info?fr=pc&platform=pc".into(),
            None,
        )?;
        let v = response_json(&r)?;
        let d = &v["data"];
        let id = d
            .get("uid")
            .or_else(|| d.get("user_id"))
            .and_then(|x| {
                x.as_str()
                    .map(str::to_owned)
                    .or_else(|| x.as_u64().map(|n| n.to_string()))
            })
            .filter(|s| !s.is_empty())
            .or_else(|| {
                self.cookie.split(';').find_map(|part| {
                    let (name, value) = part.trim().split_once('=')?;
                    (name == "__uid" && !value.is_empty()).then(|| value.to_owned())
                })
            })
            .unwrap_or_else(|| format!("quark-{:x}", md5::compute(self.cookie.as_bytes())));
        Ok(CloudAccount {
            provider: CloudProvider::Quark,
            id,
            display_name: required(d, "nickname")?,
            auth_method: CloudAuthMethod::Web,
        })
    }
    pub fn list(&self, parent: &str) -> Result<CloudHttpResponse, QuarkError> {
        self.list_page(parent, 1)
    }
    fn list_page(&self, parent: &str, page: usize) -> Result<CloudHttpResponse, QuarkError> {
        self.request(
            "GET",
            &format!(
                "file/sort?pdir_fid={}&_page={page}&_size=200&_sort=file_name:asc",
                encoded(parent)
            ),
            None,
        )
    }
    pub fn directory(&self, parent: &str) -> Result<CloudDirectory, QuarkError> {
        let mut items = Vec::new();
        for page in 1..=10000 {
            let r = self.list_page(parent, page)?;
            let v = response_json(&r)?;
            let files = parse_quark_files(&r.body)?;
            let count = files.len();
            for f in files {
                items.push(CloudFile {
                    id: f.id,
                    parent_id: parent.into(),
                    name: f.name,
                    kind: if f.is_folder { "folder" } else { "file" }.into(),
                    size: f.size,
                    modified_at: None,
                })
            }
            let total = v.pointer("/metadata/_total").and_then(Value::as_u64);
            if count < 200 || total.is_some_and(|t| items.len() as u64 >= t) {
                return Ok(CloudDirectory {
                    id: parent.into(),
                    items,
                });
            }
        }
        Err(QuarkError::InvalidResponse)
    }
    pub fn mkdir(&self, parent: &str, name: &str) -> Result<CloudHttpResponse, QuarkError> {
        self.request(
            "POST",
            "file",
            Some(json!({"pdir_fid":parent,"file_name":name,"dir_path":"","dir_init_lock":false})),
        )
    }
    pub fn rename(&self, id: &str, name: &str) -> Result<CloudHttpResponse, QuarkError> {
        self.request(
            "POST",
            "file/rename",
            Some(json!({"fid":id,"file_name":name})),
        )
    }
    pub fn move_files(
        &self,
        ids: &[String],
        parent: &str,
    ) -> Result<CloudHttpResponse, QuarkError> {
        self.request(
            "POST",
            "file/move",
            Some(json!({"filelist":ids,"to_pdir_fid":parent,"action_type":1})),
        )
    }
    pub fn trash(&self, ids: &[String]) -> Result<CloudHttpResponse, QuarkError> {
        self.request(
            "POST",
            "file/delete",
            Some(json!({"action_type":2,"filelist":ids,"exclude_fids":[]})),
        )
    }
    pub fn download_url(&self, id: &str) -> Result<CloudHttpResponse, QuarkError> {
        self.request("POST", "file/download", Some(json!({"fids":[id]})))
    }
}
