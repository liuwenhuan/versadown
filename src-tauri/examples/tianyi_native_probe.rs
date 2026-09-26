use versadown_lib::cloud::{
    load_saved_password_login,
    tianyi::{TianyiClient, TIANYI_RECYCLE_BIN_ID},
    CloudProvider, ReqwestTransport, SystemCredentialStore,
};
use std::{collections::VecDeque, path::PathBuf};

fn main() {
    match probe() {
        Ok((root_items, download_resolved)) => println!(
            "天翼原生 App 后台探针通过：根目录 {root_items} 项，下载地址已解析={download_resolved}。"
        ),
        Err(stage) => {
            eprintln!("天翼原生 App 后台探针失败：{stage}");
            std::process::exit(1);
        }
    }
}

fn probe() -> Result<(usize, bool), &'static str> {
    let credential_root = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .map(|path| {
            path.join("com.versadown.desktop")
                .join("cloud-sessions")
        })
        .ok_or("无法定位应用凭据目录")?;
    let store = SystemCredentialStore::new(credential_root).map_err(|_| "无法读取加密凭据存储")?;
    let credentials = load_saved_password_login(&store, CloudProvider::Tianyi)
        .map_err(|_| "无法解密已保存的天翼登录信息")?
        .ok_or("没有已保存的天翼登录信息")?;
    let http = ReqwestTransport::new().map_err(|_| "无法初始化 HTTPS 客户端")?;
    let session =
        TianyiClient::login_password(credentials.username(), credentials.password(), &http)
            .map_err(|_| "原生 App 登录或根目录校验失败")?;
    drop(credentials);

    let client = TianyiClient::new(session, &http);
    let root = client
        .directory("-11")
        .map_err(|_| "原生 App 根目录读取失败")?;
    let root_items = root.items.len();
    let mut file = root.items.iter().find(|item| item.kind == "file");
    let mut pending = root
        .items
        .iter()
        .filter(|item| item.kind == "folder" && item.id != TIANYI_RECYCLE_BIN_ID)
        .map(|item| item.id.clone())
        .collect::<VecDeque<_>>();
    let mut owned_file = None;
    for _ in 0..20 {
        if file.is_some() || pending.is_empty() {
            break;
        }
        let directory_id = pending.pop_front().ok_or("目录遍历状态无效")?;
        let directory = client
            .directory(&directory_id)
            .map_err(|_| "原生 App 子目录读取失败")?;
        if let Some(found) = directory.items.iter().find(|item| item.kind == "file") {
            owned_file = Some(found.clone());
            break;
        }
        pending.extend(
            directory
                .items
                .iter()
                .filter(|item| item.kind == "folder")
                .map(|item| item.id.clone()),
        );
    }
    if file.is_none() {
        file = owned_file.as_ref();
    }
    let file = file.ok_or("前 20 个目录中没有可用于解析验证的文件")?;
    let download = client
        .download(&file.id)
        .map_err(|_| "原生 App 下载地址解析失败")?;
    let signed = download.url.starts_with("https://")
        && ["Date", "SessionKey", "Signature", "X-Request-ID"]
            .iter()
            .all(|name| download.headers.contains_key(*name));
    if !signed {
        return Err("下载地址缺少 App 签名");
    }
    Ok((root_items, true))
}
